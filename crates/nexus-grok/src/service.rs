//! Grok 账号用例：device code 登录、导入、续期。Tauri 和网关只跟它打交道。
//!
//! 三条规矩和 ChatGPT 一样：刷完就存；同一号刷新不并发；致命失败标 `NeedsLogin`。
//! 登录是 RFC 8628 device code，**没有** localhost 回调。

use crate::model::{GrokAccount, GrokAuthKind, GrokStatus};
use crate::oauth::{
    self, identity_from_tokens, jwt_expiry, looks_like_jwt, DeviceCode, Discovery, TokenClient,
    TokenSet,
};
use crate::protocol::{self, GROK_MODELS, REFRESH_AHEAD, XAI_API};
use crate::quota;
use crate::repo::{GrokAccounts, Upserted};
use nexus_core::{AppError, ErrorCode, GrokAccountId, Result, Secret};
use nexus_store::{settings, Db, GrokSecret, SecretStore};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub const LOGIN_TTL: Duration = Duration::from_secs(30 * 60);
/// 上游拉到的模型目录落在设置表里的键。
const SETTING_MODELS: &str = "grok.models";
/// 额度快照多久算新鲜；网关取号时比它旧就顺手再拉一次。
pub const QUOTA_STALE_AFTER: Duration = Duration::from_secs(10 * 60);

/// 上游拉到的一条模型。只留网关会用的字段；其余随上游演进。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokManifestModel {
    pub id: String,
    pub display_name: Option<String>,
    /// `chat` / `image` / `video` / 其它。上游没标就按名字猜。
    pub modality: String,
}

/// `GET /v1/models-v2`（cli-chat-proxy）的响应形状不承诺稳定：`models[]` / `data[]` / 顶层数组
/// 三种都认，每条取 `id` / `name` / `displayName`。
pub fn parse_models_v2(body: &serde_json::Value) -> Vec<GrokManifestModel> {
    let list = body
        .get("models")
        .or_else(|| body.get("data"))
        .and_then(|v| v.as_array())
        .or_else(|| body.as_array());
    let Some(list) = list else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|m| {
            let id = m
                .get("id")
                .or_else(|| m.get("model"))
                .or_else(|| m.get("name"))
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())?
                .to_string();
            if m.get("hidden").and_then(|v| v.as_bool()) == Some(true)
                || m.get("deprecated").and_then(|v| v.as_bool()) == Some(true)
            {
                return None;
            }
            let display_name = m
                .get("displayName")
                .or_else(|| m.get("display_name"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let modality = m
                .get("modality")
                .or_else(|| m.get("type"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_ascii_lowercase())
                .unwrap_or_else(|| {
                    if protocol::is_grok_video_model(&id) {
                        "video".into()
                    } else if protocol::is_grok_image_model(&id) || id.contains("image") {
                        "image".into()
                    } else {
                        "chat".into()
                    }
                });
            Some(GrokManifestModel {
                id,
                display_name,
                modality,
            })
        })
        .collect()
}

struct LoginSession {
    device: DeviceCode,
    discovery: Discovery,
    started: Instant,
    note: Option<String>,
    cancelled: Arc<AtomicBool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginHandle {
    pub session_id: String,
    pub authorize_url: String,
    pub user_code: String,
    pub expires_in_secs: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum LoginState {
    Waiting {
        session_id: String,
        user_code: String,
        elapsed_secs: u64,
    },
    Succeeded {
        session_id: String,
        account: Box<GrokAccount>,
        created: bool,
    },
    Failed {
        session_id: String,
        message: String,
        hint: Option<String>,
    },
    Cancelled {
        session_id: String,
    },
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Imported {
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
}

impl Imported {
    fn is_empty(&self) -> bool {
        self.access_token.is_none() && self.refresh_token.is_none()
    }
}

fn looks_like_opaque_token(s: &str) -> bool {
    s.len() >= 20
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-'))
}

/// xAI 开发者 key：`xai-` 开头的一串。
pub fn looks_like_api_key(s: &str) -> bool {
    let s = s.trim();
    s.starts_with("xai-") && s.len() >= 20 && !s.contains(char::is_whitespace)
}

fn fingerprint(key: &str) -> String {
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(key.trim().as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    hex[..12].to_string()
}

/// 认 `~/.grok/auth.json`、CLIProxyAPI `type=xai` JSON、`access----refresh`、单独 JWT / refresh。
pub fn parse_import_text(text: &str) -> Option<Imported> {
    let raw = text.trim();
    if raw.is_empty() || raw.starts_with('#') {
        return None;
    }
    if raw.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(raw).ok()?;
        let tokens = v
            .get("tokens")
            .filter(|t| t.is_object())
            .or_else(|| v.get("token").filter(|t| t.is_object()))
            .unwrap_or(&v);
        let pick = |obj: &serde_json::Value, keys: &[&str]| {
            keys.iter().find_map(|k| {
                obj.get(*k)
                    .and_then(|x| x.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
        };
        let mut out = Imported {
            access_token: pick(tokens, &["access_token", "accessToken"])
                .filter(|t| looks_like_jwt(t)),
            refresh_token: pick(tokens, &["refresh_token", "refreshToken"])
                .filter(|t| looks_like_opaque_token(t)),
            id_token: pick(tokens, &["id_token", "idToken"]).filter(|t| looks_like_jwt(t)),
        };
        if out.is_empty() {
            // CLIProxyAPI 把 token 放在 attributes 里。
            if let Some(attrs) = v.get("attributes").or_else(|| v.get("auth")) {
                out = Imported {
                    access_token: pick(attrs, &["access_token", "accessToken"])
                        .filter(|t| looks_like_jwt(t)),
                    refresh_token: pick(attrs, &["refresh_token", "refreshToken"])
                        .filter(|t| looks_like_opaque_token(t)),
                    id_token: pick(attrs, &["id_token", "idToken"]).filter(|t| looks_like_jwt(t)),
                };
            }
        }
        return (!out.is_empty()).then_some(out);
    }
    let mut out = Imported::default();
    for part in raw.split("----").map(str::trim).filter(|p| !p.is_empty()) {
        if part.contains('@') || part.contains(char::is_whitespace) {
            continue;
        }
        if looks_like_jwt(part) {
            let claims = oauth::decode_jwt_claims(part).unwrap_or_default();
            let is_id = claims.get("email").is_some() && claims.get("sub").is_some();
            if is_id && out.id_token.is_none() && out.access_token.is_some() {
                out.id_token = Some(part.to_string());
            } else if out.access_token.is_none() {
                out.access_token = Some(part.to_string());
            } else if out.id_token.is_none() {
                out.id_token = Some(part.to_string());
            }
        } else if looks_like_opaque_token(part) && out.refresh_token.is_none() {
            out.refresh_token = Some(part.to_string());
        }
    }
    (!out.is_empty()).then_some(out)
}

pub struct GrokService {
    pub repo: GrokAccounts,
    db: Arc<Db>,
    http: reqwest::Client,
    tokens: TokenClient,
    discovery: tokio::sync::Mutex<Option<Discovery>>,
    logins: Mutex<HashMap<String, LoginSession>>,
    refresh_locks: Mutex<HashMap<GrokAccountId, Arc<tokio::sync::Mutex<()>>>>,
    /// 额度探测同号不并发：网关每次取号都可能触发一次，别把上游打成筛子。
    quota_locks: Mutex<HashMap<GrokAccountId, Arc<tokio::sync::Mutex<()>>>>,
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest 客户端初始化（只在 TLS 后端缺失时失败）")
}

impl GrokService {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        let http = http_client();
        Self {
            repo: GrokAccounts::new(db.clone(), secrets),
            db,
            http: http.clone(),
            tokens: TokenClient::new(http),
            discovery: tokio::sync::Mutex::new(None),
            logins: Mutex::new(HashMap::new()),
            refresh_locks: Mutex::new(HashMap::new()),
            quota_locks: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(test)]
    pub fn with_discovery_url(mut self, url: &str) -> Self {
        self.tokens = TokenClient::with_discovery(http_client(), url.to_string());
        self
    }

    /// 对话模型：静态清单 ∪ 上游目录里的对话模型，静态在前、去重。
    pub fn models(&self) -> Vec<String> {
        let mut out: Vec<String> = GROK_MODELS.iter().map(|m| (*m).to_string()).collect();
        for m in self.manifest() {
            if m.modality == "chat" && !out.iter().any(|x| x.eq_ignore_ascii_case(&m.id)) {
                out.push(m.id);
            }
        }
        out
    }

    /// 上次从上游拉到的目录；空 = 还没拉过。
    pub fn manifest(&self) -> Vec<GrokManifestModel> {
        settings::get_or(&self.db, SETTING_MODELS, Vec::new())
    }

    /// 拉一次 `GET /v1/models-v2`。要一个有凭证的订阅号（API Key 号没有这个接口）；一个都没有就
    /// 原样返回现有目录。拉失败不清空旧目录。
    pub async fn refresh_models_any(&self) -> Result<Vec<GrokManifestModel>> {
        let Some(account) = self
            .list()?
            .into_iter()
            .find(|a| a.usable() && a.auth_kind == GrokAuthKind::Oauth)
        else {
            return Ok(self.manifest());
        };
        let token = self.access_token(&account.id).await?;
        let url = format!(
            "{}/models-v2",
            protocol::CLI_CHAT_PROXY.trim_end_matches('/')
        );
        let mut req = self.http.get(&url).timeout(Duration::from_secs(20));
        for (k, v) in protocol::cli_headers(token.expose()) {
            req = req.header(k, v);
        }
        let res = req
            .send()
            .await
            .map_err(|e| AppError::upstream(format!("拉 Grok 模型目录失败：{e}")))?;
        if !res.status().is_success() {
            return Err(AppError::upstream(format!(
                "Grok 模型目录 HTTP {}",
                res.status().as_u16()
            )));
        }
        let body: serde_json::Value = res
            .json()
            .await
            .map_err(|e| AppError::upstream(format!("Grok 模型目录不是 JSON：{e}")))?;
        let list = parse_models_v2(&body);
        if !list.is_empty() {
            settings::set(&self.db, SETTING_MODELS, &list)?;
        }
        Ok(list)
    }

    // ---------- 额度 / 媒体资格 ----------

    /// 现在拉一次额度并落库。API Key 号没有订阅额度，直接回现状。
    pub async fn refresh_quota(&self, id: &GrokAccountId) -> Result<GrokAccount> {
        let account = self.repo.get(id)?;
        if account.auth_kind == GrokAuthKind::ApiKey {
            return Ok(account);
        }
        let lock = self
            .quota_locks
            .lock()
            .expect("quota locks")
            .entry(id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        let token = self.access_token(id).await?;
        let mut quota = match quota::fetch_billing(
            &self.http,
            token.expose(),
            Some(&account.account_ref),
        )
        .await
        {
            Ok(q) => q,
            Err(err) if err.code == ErrorCode::Forbidden => {
                // 没有 Grok Build 权益：聊天可能也不行，但先只把媒体关掉、把原因记下来。
                self.repo.record_media_probe(id, false)?;
                self.repo.record_failure(id, &err.message, None)?;
                return self.repo.get(id);
            }
            Err(err) if err.code == ErrorCode::Unauthorized => {
                self.repo
                    .record_failure(id, &err.message, Some(GrokStatus::NeedsLogin))?;
                return Err(err);
            }
            Err(err) => {
                self.repo.record_failure(id, &err.message, None)?;
                return Err(err);
            }
        };
        if quota.subscription_tier.is_none() {
            if let Some(user) = quota::fetch_user(&self.http, token.expose()).await {
                quota.subscription_tier = quota::tier_from_user(&user);
                if account.email.is_none() {
                    if let Some(email) = user.get("email").and_then(|v| v.as_str()) {
                        let _ = self.repo.update_identity(
                            id,
                            &crate::oauth::Identity {
                                subject: account.account_ref.clone(),
                                email: Some(email.to_string()),
                            },
                        );
                    }
                }
            }
        }
        self.repo.store_quota(id, &quota)
    }

    /// 快照过期就拉一次，否则什么都不做。网关取号前调；失败吞掉——额度只是线索。
    pub async fn ensure_quota_fresh(&self, id: &GrokAccountId) {
        let Ok(account) = self.repo.get(id) else {
            return;
        };
        if account.auth_kind == GrokAuthKind::ApiKey {
            return;
        }
        let fresh = account
            .usage
            .as_ref()
            .and_then(|u| OffsetDateTime::parse(&u.checked_at, &Rfc3339).ok())
            .is_some_and(|t| t + QUOTA_STALE_AFTER > OffsetDateTime::now_utc());
        if fresh {
            return;
        }
        if let Err(err) = self.refresh_quota(id).await {
            tracing::info!(account = %id, %err, "拉 Grok 额度失败，沿用旧快照");
        }
    }

    /// 网关每次响应后把被动信号（`x-ratelimit-*` / `retry-after`）并进快照。按账号标签找号。
    pub fn absorb_headers(&self, label: &str, headers: &[(String, String)]) {
        let Ok(list) = self.repo.list() else { return };
        let Some(account) = list
            .into_iter()
            .find(|a| a.label().eq_ignore_ascii_case(label))
        else {
            return;
        };
        let mut quota = account.usage.unwrap_or_default();
        if quota.absorb_headers(headers) {
            let _ = self.repo.store_quota(&account.id, &quota);
        }
    }

    /// 媒体请求撞回来的判决。402 / 403 → 这个号出不了媒体（聊天不受影响）；成功 → 能。
    pub fn record_media_outcome(&self, label: &str, eligible: bool) {
        let Ok(list) = self.repo.list() else { return };
        if let Some(account) = list
            .into_iter()
            .find(|a| a.label().eq_ignore_ascii_case(label))
        {
            let _ = self.repo.record_media_probe(&account.id, eligible);
        }
    }

    pub fn set_media_override(
        &self,
        id: &GrokAccountId,
        value: Option<bool>,
    ) -> Result<GrokAccount> {
        self.repo.set_media_override(id, value)
    }

    /// 有没有一个号此刻能接媒体请求。
    pub fn media_ready(&self) -> bool {
        self.list()
            .map(|l| {
                l.iter()
                    .any(|a| a.usable() && a.media_eligible != Some(false))
            })
            .unwrap_or(false)
    }

    // ---------- API Key 号 ----------

    /// 加一个 xAI API Key 号。先打 `GET /v1/api-key` 拿 key id 与名字（顺带验证 key 是活的）；
    /// 接口不通就用 key 的指纹当身份——不阻止用户加号。
    pub async fn add_api_key(&self, api_key: &str, note: Option<&str>) -> Result<Upserted> {
        let key = api_key.trim();
        if !looks_like_api_key(key) {
            return Err(AppError::invalid(
                "这不像 xAI 的 API Key（应以 `xai-` 开头）。",
            ));
        }
        let (account_ref, display) = match self.probe_api_key(key).await {
            Ok((id, name)) => (id, name),
            Err(err) if err.code == ErrorCode::Unauthorized => return Err(err),
            Err(err) => {
                tracing::info!(%err, "xAI api-key 接口不可用，用指纹当身份");
                (format!("key:{}", fingerprint(key)), None)
            }
        };
        self.repo
            .upsert_api_key(&account_ref, display.as_deref(), &Secret::new(key), note)
    }

    async fn probe_api_key(&self, key: &str) -> Result<(String, Option<String>)> {
        let url = format!("{}/api-key", XAI_API.trim_end_matches('/'));
        let res = self
            .http
            .get(&url)
            .bearer_auth(key)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(|e| AppError::upstream(format!("连不上 api.x.ai：{e}")))?;
        let status = res.status().as_u16();
        let body: serde_json::Value = res.json().await.unwrap_or_default();
        match status {
            200..=299 => {
                let id = body
                    .get("api_key_id")
                    .or_else(|| body.get("apiKeyId"))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("key:{}", fingerprint(key)));
                let name = body
                    .get("name")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.trim().is_empty())
                    .map(str::to_string)
                    .or_else(|| {
                        body.get("team_id")
                            .and_then(|v| v.as_str())
                            .map(|t| format!("team {t}"))
                    });
                let blocked = body.get("api_key_blocked").and_then(|v| v.as_bool()) == Some(true)
                    || body.get("api_key_disabled").and_then(|v| v.as_bool()) == Some(true)
                    || body.get("team_blocked").and_then(|v| v.as_bool()) == Some(true);
                if blocked {
                    return Err(AppError::new(
                        ErrorCode::Unauthorized,
                        "这把 API Key 已被停用。",
                    ));
                }
                Ok((format!("apikey:{id}"), name))
            }
            401 | 403 => Err(AppError::new(
                ErrorCode::Unauthorized,
                "xAI 不认这把 API Key。",
            )),
            _ => Err(AppError::upstream(format!(
                "xAI api-key 接口 HTTP {status}"
            ))),
        }
    }

    pub fn list(&self) -> Result<Vec<GrokAccount>> {
        self.repo.list()
    }

    pub fn get(&self, id: &GrokAccountId) -> Result<GrokAccount> {
        self.repo.get(id)
    }

    pub fn remove(&self, id: &GrokAccountId) -> Result<()> {
        self.repo.remove(id)
    }

    pub fn set_enabled(&self, id: &GrokAccountId, enabled: bool) -> Result<GrokAccount> {
        self.repo.set_enabled(id, enabled)
    }

    pub fn set_note(&self, id: &GrokAccountId, note: Option<&str>) -> Result<GrokAccount> {
        self.repo.set_note(id, note)
    }

    async fn discovery(&self) -> Result<Discovery> {
        if let Some(d) = self.discovery.lock().await.clone() {
            return Ok(d);
        }
        let d = self.tokens.discover().await?;
        *self.discovery.lock().await = Some(d.clone());
        Ok(d)
    }

    pub async fn start_login(&self, note: Option<String>) -> Result<LoginHandle> {
        self.sweep_logins();
        let discovery = self.discovery().await?;
        let device = self
            .tokens
            .start_device(&discovery.device_authorization_endpoint)
            .await?;
        let session_id = uuid::Uuid::new_v4().to_string();
        let handle = LoginHandle {
            session_id: session_id.clone(),
            authorize_url: device.verification_uri_complete.clone(),
            user_code: device.user_code.clone(),
            expires_in_secs: device.expires_in_secs,
        };
        self.logins.lock().expect("logins").insert(
            session_id,
            LoginSession {
                device,
                discovery,
                started: Instant::now(),
                note,
                cancelled: Arc::new(AtomicBool::new(false)),
            },
        );
        Ok(handle)
    }

    pub async fn wait_login(
        &self,
        session_id: &str,
        on_state: &(dyn Fn(LoginState) + Sync),
    ) -> Result<Upserted> {
        let (device, discovery, cancelled, note, started) = {
            let logins = self.logins.lock().expect("logins");
            let Some(s) = logins.get(session_id) else {
                return Err(AppError::invalid("这次授权已过期或不存在，重新发起一次。"));
            };
            (
                s.device.clone(),
                s.discovery.clone(),
                s.cancelled.clone(),
                s.note.clone(),
                s.started,
            )
        };
        let sid = session_id.to_string();
        on_state(LoginState::Waiting {
            session_id: sid.clone(),
            user_code: device.user_code.clone(),
            elapsed_secs: 0,
        });
        let deadline = started + Duration::from_secs(device.expires_in_secs.max(30));
        let mut interval = Duration::from_secs(device.interval_secs.max(1));
        loop {
            if cancelled.load(Ordering::SeqCst) {
                self.logins.lock().expect("logins").remove(session_id);
                on_state(LoginState::Cancelled {
                    session_id: sid.clone(),
                });
                return Err(AppError::new(ErrorCode::Cancelled, "已取消授权。"));
            }
            if Instant::now() > deadline || started.elapsed() > LOGIN_TTL {
                self.logins.lock().expect("logins").remove(session_id);
                let err = AppError::invalid("这条授权码已过期。").with_hint("重新发起一次。");
                on_state(LoginState::Failed {
                    session_id: sid,
                    message: err.message.clone(),
                    hint: err.hint.clone(),
                });
                return Err(err);
            }
            match self
                .tokens
                .poll_once(&discovery.token_endpoint, &device.device_code)
                .await
            {
                Ok(Some(tokens)) => {
                    self.logins.lock().expect("logins").remove(session_id);
                    match self.add_tokens(tokens, note.as_deref()) {
                        Ok(up) => {
                            on_state(LoginState::Succeeded {
                                session_id: sid,
                                account: Box::new(up.account.clone()),
                                created: up.created,
                            });
                            return Ok(up);
                        }
                        Err(err) => {
                            on_state(LoginState::Failed {
                                session_id: sid,
                                message: err.message.clone(),
                                hint: err.hint.clone(),
                            });
                            return Err(err);
                        }
                    }
                }
                Ok(None) => {
                    on_state(LoginState::Waiting {
                        session_id: sid.clone(),
                        user_code: device.user_code.clone(),
                        elapsed_secs: started.elapsed().as_secs(),
                    });
                    tokio::time::sleep(interval).await;
                }
                Err(err) if err.code == ErrorCode::Unauthorized => {
                    self.logins.lock().expect("logins").remove(session_id);
                    on_state(LoginState::Failed {
                        session_id: sid,
                        message: err.message.clone(),
                        hint: err.hint.clone(),
                    });
                    return Err(err);
                }
                Err(err) => {
                    // slow_down 以外的暂时失败：拉长间隔再试。
                    if err.message.contains("slow_down") {
                        interval = interval.saturating_mul(2).min(Duration::from_secs(15));
                    }
                    on_state(LoginState::Waiting {
                        session_id: sid.clone(),
                        user_code: device.user_code.clone(),
                        elapsed_secs: started.elapsed().as_secs(),
                    });
                    tokio::time::sleep(interval).await;
                }
            }
        }
    }

    pub fn cancel_login(&self, session_id: &str) {
        if let Some(s) = self.logins.lock().expect("logins").get(session_id) {
            s.cancelled.store(true, Ordering::SeqCst);
        }
    }

    fn sweep_logins(&self) {
        self.logins
            .lock()
            .expect("logins")
            .retain(|_, s| s.started.elapsed() <= LOGIN_TTL);
    }

    pub fn add_tokens(&self, tokens: TokenSet, note: Option<&str>) -> Result<Upserted> {
        let identity = identity_from_tokens(&tokens);
        if identity.subject.is_empty() {
            return Err(AppError::invalid(
                "这组 token 里读不出 sub，不是 xAI 订阅账号。",
            ));
        }
        self.repo.upsert(&identity, &tokens, note)
    }

    pub async fn import_text(&self, text: &str, note: Option<&str>) -> Result<Upserted> {
        if looks_like_api_key(text.trim()) {
            return self.add_api_key(text, note).await;
        }
        let imported = parse_import_text(text).ok_or_else(|| {
            AppError::invalid("认不出这段内容。").with_hint(
                "支持 ~/.grok/auth.json 原文、`access_token----refresh_token`，或单独一个 refresh token。",
            )
        })?;
        let tokens = match (&imported.access_token, &imported.refresh_token) {
            (Some(access), refresh) => TokenSet {
                expires_at: jwt_expiry(access),
                access_token: Secret::new(access.clone()),
                refresh_token: refresh.clone().map(Secret::new),
                id_token: imported.id_token.clone().map(Secret::new),
            },
            (None, Some(refresh)) => {
                let disc = self.discovery().await?;
                let mut fresh = self.tokens.refresh(&disc.token_endpoint, refresh).await?;
                if fresh.refresh_token.is_none() {
                    fresh.refresh_token = Some(Secret::new(refresh.clone()));
                }
                fresh
            }
            (None, None) => unreachable!("parse_import_text 不会给出两个都空的结果"),
        };
        self.add_tokens(tokens, note)
    }

    /// 从本机 Grok CLI 登录态导入（`<grok_home>/auth.json`，默认 `~/.grok`）。
    pub async fn import_grok_cli(&self, grok_home: &Path) -> Result<Upserted> {
        let path = grok_home.join("auth.json");
        let raw = std::fs::read_to_string(&path).map_err(|e| {
            AppError::new(ErrorCode::Io, format!("读不到 {}：{e}", path.display()))
                .with_hint("先在终端里跑一次 `grok login`，或者用「授权登录」。")
        })?;
        self.import_text(&raw, Some("来自本机 Grok CLI")).await
    }

    /// 一份能直接出流量的凭证。API Key 号给 key；订阅号给（必要时续过的）access token。
    pub async fn access_token(&self, id: &GrokAccountId) -> Result<Secret> {
        let account = self.repo.get(id)?;
        if account.auth_kind == GrokAuthKind::ApiKey {
            return self.repo.secret(id, GrokSecret::ApiKey)?.ok_or_else(|| {
                let _ = self
                    .repo
                    .record_failure(id, "API Key 丢失", Some(GrokStatus::NeedsLogin));
                AppError::new(ErrorCode::SecretMissing, "这个号的 API Key 不在了。")
                    .with_hint("重新粘贴一次 API Key。")
            });
        }
        if let Some(t) = self.fresh_access(id)? {
            return Ok(t);
        }
        let lock = self
            .refresh_locks
            .lock()
            .expect("refresh locks")
            .entry(id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        let _guard = lock.lock().await;
        if let Some(t) = self.fresh_access(id)? {
            return Ok(t);
        }
        self.refresh_now(id).await
    }

    fn fresh_access(&self, id: &GrokAccountId) -> Result<Option<Secret>> {
        let account = self.repo.get(id)?;
        let Some(access) = self.repo.secret(id, GrokSecret::Access)? else {
            return Ok(None);
        };
        let expires = account
            .access_expires_at
            .as_deref()
            .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
            .or_else(|| jwt_expiry(access.expose()));
        let fresh = match expires {
            Some(exp) => exp > OffsetDateTime::now_utc() + REFRESH_AHEAD,
            None => false,
        };
        Ok(fresh.then_some(access))
    }

    async fn refresh_now(&self, id: &GrokAccountId) -> Result<Secret> {
        let refresh = self.repo.secret(id, GrokSecret::Refresh)?.ok_or_else(|| {
            let _ =
                self.repo
                    .record_failure(id, "没有 refresh token", Some(GrokStatus::NeedsLogin));
            AppError::new(
                ErrorCode::SecretMissing,
                "这个 Grok 账号没有 refresh token。",
            )
            .with_hint("重新授权一次。")
        })?;
        let disc = self.discovery().await?;
        match self
            .tokens
            .refresh(&disc.token_endpoint, refresh.expose())
            .await
        {
            Ok(mut fresh) => {
                if fresh.refresh_token.is_none() {
                    fresh.refresh_token = Some(refresh);
                }
                self.repo.store_tokens(id, &fresh)?;
                let identity = identity_from_tokens(&fresh);
                let _ = self.repo.update_identity(id, &identity);
                Ok(fresh.access_token)
            }
            Err(err) if err.code == ErrorCode::Unauthorized => {
                self.repo
                    .record_failure(id, &err.message, Some(GrokStatus::NeedsLogin))?;
                Err(err)
            }
            Err(err) => {
                self.repo.record_failure(id, &err.message, None)?;
                if let Some(access) = self.repo.secret(id, GrokSecret::Access)? {
                    if jwt_expiry(access.expose()).is_some_and(|exp| {
                        exp > OffsetDateTime::now_utc() + Duration::from_secs(60)
                    }) {
                        tracing::warn!(account = %id, %err, "刷新失败，先用还没过期的旧 access token");
                        return Ok(access);
                    }
                }
                Err(err)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: &str) -> String {
        use base64::Engine;
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes());
        format!("{}.{}.sig", b64("{}"), b64(payload))
    }

    #[test]
    fn parse_grok_auth_json_and_dash_form() {
        let access = jwt(r#"{"sub":"u1"}"#);
        let idt = jwt(r#"{"sub":"u1","email":"a@x.ai"}"#);
        let json = format!(
            r#"{{"tokens":{{"access_token":"{access}","refresh_token":"rt_abcdefghijklmnopqrst","id_token":"{idt}"}}}}"#
        );
        let parsed = parse_import_text(&json).unwrap();
        assert_eq!(parsed.access_token.as_deref(), Some(access.as_str()));
        assert_eq!(
            parsed.refresh_token.as_deref(),
            Some("rt_abcdefghijklmnopqrst")
        );
        let dashed = format!("{access}----rt_abcdefghijklmnopqrst");
        assert!(parse_import_text(&dashed).unwrap().refresh_token.is_some());
    }
}
