//! ChatGPT 账号的用例层：授权登录、导入、续期、额度。Tauri 命令和网关的号源都只跟它打交道。
//!
//! 三条规矩：
//! - **刷完就存**。refresh token 轮换，刷新响应到手的下一行就是落库；中间不做别的。
//! - **同一个号的刷新不并发**（按账号一把 `tokio::sync::Mutex`）。网关的两个并发请求同时发现
//!   token 快过期、同时去刷，后到的必然拿到 `refresh_token_reused`，把一个好号误判成死号。
//! - **致命与暂时分开**（`OauthError::fatal`）。refresh token 作废 → `NeedsLogin`，等人重新授权；
//!   网络抖动 → 什么都不改，还没过期的旧 access token 照用。

use crate::callback::CallbackServer;
use crate::model::{ChatGptAccount, ChatGptStatus};
use crate::oauth::{
    self, authorize_url, identity_from_tokens, jwt_expiry, looks_like_jwt, new_state,
    parse_callback, Identity, Pkce, TokenClient, TokenSet, REDIRECT_URI,
};
use crate::protocol::{self, CodexUsage, ManifestModel, DEFAULT_BACKEND_URL, REFRESH_AHEAD};
use crate::repo::{ChatGptAccounts, Upserted};
use nexus_core::{AppError, ChatGptAccountId, ErrorCode, Result, Secret};
use nexus_store::{ChatGptSecret, Db, SecretStore};
use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// 授权链接的有效期。30 分钟够走完登录含验证码；过了就重新发起。
pub const LOGIN_TTL: Duration = Duration::from_secs(30 * 60);
/// 等浏览器回调的耐心。
pub const CALLBACK_TIMEOUT: Duration = Duration::from_secs(10 * 60);

struct LoginSession {
    pkce: Pkce,
    started: Instant,
    note: Option<String>,
    cancelled: Arc<AtomicBool>,
    /// 绑成功的回调监听。`wait_login` 把它取走去等；手贴路径用不上，随会话一起丢掉。
    server: Option<CallbackServer>,
}

/// 交给前端的登录句柄。**不含 verifier**。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginHandle {
    pub session_id: String,
    pub authorize_url: String,
    pub redirect_uri: String,
    /// 本机 1455 已经在听：用户点完同意会自动完成。false = 端口被占（多半是 `codex login`
    /// 正在跑），要把地址栏的回调地址贴回来。
    pub callback_listening: bool,
}

/// 等回调过程中的状态，经事件推给界面。
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum LoginState {
    Waiting {
        session_id: String,
        elapsed_secs: u64,
    },
    Succeeded {
        session_id: String,
        /// 装在 Box 里：这个变体比别的大一个数量级，clippy 的 large_enum_variant 说得对。
        account: Box<ChatGptAccount>,
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

/// 一行导入文本解析出来的东西。三种写法（见 [`parse_import_text`]）最后都落到这里。
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

/// 不透明的 refresh token：URL 安全字符、够长。短串会被拒掉，那是有意的——
/// 一段乱字符拿去刷只会换来一个语焉不详的 400。
fn looks_like_opaque_token(s: &str) -> bool {
    s.len() >= 20
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-'))
}

/// 认三种写法：
/// - `~/.codex/auth.json` 原文（`{"tokens":{"id_token","access_token","refresh_token"}}`，
///   也接受把这三个键直接放顶层的形态）；
/// - `xxx----yyy`（`----` 分隔），JWT 形态的是 access / id token，不透明串是 refresh token，
///   邮箱段忽略（邮箱从 token 里读）；
/// - 单独一个 JWT 或一个不透明 refresh token。
pub fn parse_import_text(text: &str) -> Option<Imported> {
    let raw = text.trim();
    if raw.is_empty() || raw.starts_with('#') {
        return None;
    }
    if raw.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(raw).ok()?;
        let tokens = v.get("tokens").filter(|t| t.is_object()).unwrap_or(&v);
        let pick = |k: &str| {
            tokens
                .get(k)
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let out = Imported {
            access_token: pick("access_token").filter(|t| looks_like_jwt(t)),
            refresh_token: pick("refresh_token").filter(|t| looks_like_opaque_token(t)),
            id_token: pick("id_token").filter(|t| looks_like_jwt(t)),
        };
        return (!out.is_empty()).then_some(out);
    }
    let mut out = Imported::default();
    for part in raw.split("----").map(str::trim).filter(|p| !p.is_empty()) {
        if part.contains('@') || part.contains(char::is_whitespace) {
            continue;
        }
        if looks_like_jwt(part) {
            // id_token 带 email 顶层 claim 且没有 profile claim；access_token 反过来。
            // 两个 JWT 都给时按 claim 区分，只给一个就当 access token。
            let claims = oauth::decode_jwt_claims(part).unwrap_or_default();
            let is_id = claims.get("email").is_some()
                && claims.get("https://api.openai.com/profile").is_none();
            if is_id && out.id_token.is_none() {
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

pub struct ChatGptService {
    pub repo: ChatGptAccounts,
    db: Arc<Db>,
    http: reqwest::Client,
    tokens: TokenClient,
    backend_url: String,
    logins: Mutex<HashMap<String, LoginSession>>,
    refresh_locks: Mutex<HashMap<ChatGptAccountId, Arc<tokio::sync::Mutex<()>>>>,
    /// 上游目录的缓存（落库的那份读进内存），网关每个请求都要问「这个名字认不认」。
    models: std::sync::RwLock<Vec<ManifestModel>>,
}

/// 上游目录落库的键。
pub const SETTING_MODELS: &str = "chatgpt.models";

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest 客户端初始化（只在 TLS 后端缺失时失败）")
}

impl ChatGptService {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        let http = http_client();
        let models: Vec<ManifestModel> =
            nexus_store::settings::get_or(&db, SETTING_MODELS, Vec::new());
        Self {
            repo: ChatGptAccounts::new(db.clone(), secrets),
            db,
            tokens: TokenClient::new(http.clone()),
            http,
            backend_url: DEFAULT_BACKEND_URL.to_string(),
            logins: Mutex::new(HashMap::new()),
            refresh_locks: Mutex::new(HashMap::new()),
            models: std::sync::RwLock::new(models),
        }
    }

    // ── 模型目录 ─────────────────────────────────────────────────────────────

    /// 上次从上游拉到的目录（按当时那个号的套餐与我们的版本筛过）。空 = 还没拉过。
    pub fn models(&self) -> Vec<ManifestModel> {
        self.models.read().expect("models").clone()
    }

    /// 用一个号的凭证拉一次目录 `GET /codex/models?client_version=…`，筛完落库。
    /// 它和推理端点同一路径族，不在被挑战的 web 面上。拿不到就保留上一次的。
    pub async fn refresh_models(&self, id: &ChatGptAccountId) -> Result<Vec<ManifestModel>> {
        let access = self.access_token(id).await?;
        let account = self.repo.get(id)?;
        let mut req = self.http.get(format!(
            "{}/codex/models?client_version={}",
            self.backend_url,
            protocol::CLIENT_VERSION
        ));
        for (k, v) in protocol::identity_headers(access.expose(), Some(&account.account_ref)) {
            req = req.header(k, v);
        }
        let res = req
            .send()
            .await
            .map_err(|e| AppError::network(format!("连不上 chatgpt.com：{e}")))?;
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        if status == 401 {
            self.repo
                .record_failure(id, "模型目录 401", Some(ChatGptStatus::NeedsLogin))?;
            return Err(AppError::unauthorized("chatgpt.com 拒绝了这个号的凭证。"));
        }
        if !(200..300).contains(&status) {
            let head: String = text.chars().take(160).collect();
            return Err(AppError::upstream(format!("模型目录 {status}：{head}")));
        }
        let manifest: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| AppError::upstream("模型目录返回的不是 JSON"))?;
        let selected = protocol::select_models(
            &manifest,
            account.plan_type.as_deref(),
            protocol::CLIENT_VERSION,
        );
        if selected.is_empty() {
            // 空清单是「一个都不能派」，比「承诺多了」更糟——保留上一次的，只记一笔。
            return Err(AppError::upstream(
                "模型目录里没有一个能用的模型，保留上一次的清单",
            ));
        }
        nexus_store::settings::set(&self.db, SETTING_MODELS, &selected)?;
        *self.models.write().expect("models") = selected.clone();
        Ok(selected)
    }

    /// 挑一个能用的号刷目录。给「加了号 / 起网关」这种不知道该用谁刷的时刻。
    pub async fn refresh_models_any(&self) -> Result<Vec<ManifestModel>> {
        let list = self.repo.list()?;
        let Some(a) = list
            .into_iter()
            .find(|a| a.enabled && a.status == ChatGptStatus::Active && a.has_refresh)
        else {
            return Err(AppError::invalid("没有可用的 ChatGPT 账号。"));
        };
        self.refresh_models(&a.id).await
    }

    /// 底下那个库（网关侧要用同一份设置时拿它）。
    pub fn repo_db(&self) -> Arc<Db> {
        self.db.clone()
    }

    /// 测试：把 token 端点和 chatgpt.com 都指到假上游。
    pub fn with_endpoints(mut self, token_url: &str, backend_url: &str) -> Self {
        self.tokens = TokenClient::new(self.http.clone()).with_token_url(token_url);
        self.backend_url = backend_url.trim_end_matches('/').to_string();
        self
    }

    pub fn list(&self) -> Result<Vec<ChatGptAccount>> {
        self.repo.list()
    }

    pub fn get(&self, id: &ChatGptAccountId) -> Result<ChatGptAccount> {
        self.repo.get(id)
    }

    pub fn remove(&self, id: &ChatGptAccountId) -> Result<()> {
        self.repo.remove(id)
    }

    pub fn set_enabled(&self, id: &ChatGptAccountId, enabled: bool) -> Result<ChatGptAccount> {
        self.repo.set_enabled(id, enabled)
    }

    pub fn set_note(&self, id: &ChatGptAccountId, note: Option<&str>) -> Result<ChatGptAccount> {
        self.repo.set_note(id, note)
    }

    // ── 进池：授权登录 ─────────────────────────────────────────────────────────

    /// 发起一次授权：生成 PKCE 与 state，尽力在 1455 上把回调监听绑起来。
    pub async fn start_login(&self, note: Option<String>) -> Result<LoginHandle> {
        self.sweep_logins();
        let pkce = Pkce::generate();
        let state = new_state();
        let url = authorize_url(&state, &pkce.challenge);
        let server = match CallbackServer::bind().await {
            Ok(s) => Some(s),
            Err(err) => {
                tracing::info!(%err, "1455 端口绑不上，退回手贴回调地址");
                None
            }
        };
        let listening = server.is_some();
        self.logins.lock().expect("logins").insert(
            state.clone(),
            LoginSession {
                pkce,
                started: Instant::now(),
                note,
                cancelled: Arc::new(AtomicBool::new(false)),
                server,
            },
        );
        Ok(LoginHandle {
            session_id: state,
            authorize_url: url,
            redirect_uri: REDIRECT_URI.to_string(),
            callback_listening: listening,
        })
    }

    /// 等浏览器回调并完成登录。只在 `start_login` 报 `callback_listening = true` 时有意义。
    /// 命令层把它 spawn 出去，状态经 `on_state` 推给界面。
    pub async fn wait_login(
        &self,
        session_id: &str,
        on_state: &(dyn Fn(LoginState) + Sync),
    ) -> Result<Upserted> {
        let (server, cancelled) = {
            let mut logins = self.logins.lock().expect("logins");
            let Some(s) = logins.get_mut(session_id) else {
                return Err(AppError::invalid("这次授权已过期或不存在，重新发起一次。"));
            };
            (s.server.take(), s.cancelled.clone())
        };
        let Some(server) = server else {
            return Err(AppError::invalid("这次授权没有在本机监听回调。")
                .with_hint("把浏览器地址栏的回调地址贴进来完成。"));
        };
        let sid = session_id.to_string();
        let started = Instant::now();
        on_state(LoginState::Waiting {
            session_id: sid.clone(),
            elapsed_secs: 0,
        });
        // 每秒报一次进度，取消也从这里看。
        let progress = async {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                on_state(LoginState::Waiting {
                    session_id: sid.clone(),
                    elapsed_secs: started.elapsed().as_secs(),
                });
            }
        };
        let is_cancelled = || cancelled.load(Ordering::SeqCst);
        let received = tokio::select! {
            r = server.wait(session_id, CALLBACK_TIMEOUT, &is_cancelled) => r,
            _ = progress => unreachable!("进度循环不会自己结束"),
        };
        let received = match received {
            Ok(r) => r,
            Err(err) => {
                self.logins.lock().expect("logins").remove(session_id);
                if err.code == ErrorCode::Cancelled {
                    on_state(LoginState::Cancelled { session_id: sid });
                } else {
                    on_state(LoginState::Failed {
                        session_id: sid,
                        message: err.message.clone(),
                        hint: err.hint.clone(),
                    });
                }
                return Err(err);
            }
        };
        match self.finish_login(session_id, &received.code).await {
            Ok(up) => {
                on_state(LoginState::Succeeded {
                    session_id: sid,
                    account: Box::new(up.account.clone()),
                    created: up.created,
                });
                Ok(up)
            }
            Err(err) => {
                on_state(LoginState::Failed {
                    session_id: sid,
                    message: err.message.clone(),
                    hint: err.hint.clone(),
                });
                Err(err)
            }
        }
    }

    /// 手贴路径：用户把回调地址（或 `code#state`、裸 code）贴回来。
    pub async fn complete_login(&self, session_id: &str, callback_text: &str) -> Result<Upserted> {
        let cb = parse_callback(callback_text).ok_or_else(|| {
            AppError::invalid("这不像一个回调地址。").with_hint(
                "把浏览器地址栏里 http://localhost:1455/auth/callback?code=… 那整条地址复制过来。",
            )
        })?;
        if let Some(st) = &cb.state {
            if st != session_id {
                return Err(AppError::invalid("回调里的 state 和这次授权对不上。")
                    .with_hint("可能贴的是上一次的地址。重新发起授权，再贴新的那条。"));
            }
        }
        self.finish_login(session_id, &cb.code).await
    }

    pub fn cancel_login(&self, session_id: &str) {
        if let Some(s) = self.logins.lock().expect("logins").get(session_id) {
            s.cancelled.store(true, Ordering::SeqCst);
        }
    }

    async fn finish_login(&self, session_id: &str, code: &str) -> Result<Upserted> {
        // 会话一次性：授权码只能换一次，先摘下来再去换，换失败也不留一个能重放的会话。
        let session = self
            .logins
            .lock()
            .expect("logins")
            .remove(session_id)
            .ok_or_else(|| AppError::invalid("这次授权已过期或不存在，重新发起一次。"))?;
        if session.started.elapsed() > LOGIN_TTL {
            return Err(
                AppError::invalid("这条授权链接已过期（30 分钟）。").with_hint("重新发起一次。")
            );
        }
        let tokens = self
            .tokens
            .exchange_code(code, &session.pkce.verifier)
            .await
            .map_err(|e| e.into_app_error())?;
        self.add_tokens(tokens, session.note.as_deref())
    }

    fn sweep_logins(&self) {
        self.logins
            .lock()
            .expect("logins")
            .retain(|_, s| s.started.elapsed() <= LOGIN_TTL);
    }

    // ── 进池：导入 ────────────────────────────────────────────────────────────

    /// 一组已经到手的 token 入池：从 JWT 里读身份，按 `chatgpt_account_id` 建号或合并。
    pub fn add_tokens(&self, tokens: TokenSet, note: Option<&str>) -> Result<Upserted> {
        let identity = identity_from_tokens(
            tokens.id_token.as_ref().map(|t| t.expose()),
            Some(tokens.access_token.expose()),
        );
        self.repo.upsert(&identity, &tokens, note)
    }

    /// 导入一段文本（`auth.json` 原文、`xxx----yyy`、单个 token）。只有 refresh token 的话
    /// 先刷一次拿到 access token 与身份——所以这一步可能联网。
    pub async fn import_text(&self, text: &str, note: Option<&str>) -> Result<Upserted> {
        let imported = parse_import_text(text).ok_or_else(|| {
            AppError::invalid("认不出这段内容。")
                .with_hint("支持 ~/.codex/auth.json 的原文、`access_token----refresh_token`，或单独一个 refresh token。")
        })?;
        let tokens = match (&imported.access_token, &imported.refresh_token) {
            (Some(access), refresh) => TokenSet {
                expires_at: jwt_expiry(access)
                    .unwrap_or_else(|| OffsetDateTime::now_utc() + Duration::from_secs(3600)),
                access_token: Secret::new(access.clone()),
                refresh_token: refresh.clone().map(Secret::new),
                id_token: imported.id_token.clone().map(Secret::new),
            },
            (None, Some(refresh)) => {
                let mut fresh = self
                    .tokens
                    .refresh(&Secret::new(refresh.clone()))
                    .await
                    .map_err(|e| e.into_app_error())?;
                // 上游没回新 refresh 就沿用导进来的那把。
                if fresh.refresh_token.is_none() {
                    fresh.refresh_token = Some(Secret::new(refresh.clone()));
                }
                fresh
            }
            (None, None) => unreachable!("parse_import_text 不会给出两个都空的结果"),
        };
        self.add_tokens(tokens, note)
    }

    /// 从本机 Codex CLI 的登录态导入（`<codex_home>/auth.json`，默认 `~/.codex`）。
    pub async fn import_codex_cli(&self, codex_home: &Path) -> Result<Upserted> {
        let path = codex_home.join("auth.json");
        let raw = std::fs::read_to_string(&path).map_err(|e| {
            AppError::new(ErrorCode::Io, format!("读不到 {}：{e}", path.display()))
                .with_hint("先在终端里跑一次 `codex login`，或者用「授权登录」。")
        })?;
        self.import_text(&raw, Some("来自本机 Codex CLI")).await
    }

    // ── 凭证 ─────────────────────────────────────────────────────────────────

    /// 拿一份能用的 access token：离过期还远就直接给；快过期或没有就刷一把（同一个号串行）。
    pub async fn access_token(&self, id: &ChatGptAccountId) -> Result<Secret> {
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
        // 等锁期间别人可能已经刷好了。
        if let Some(t) = self.fresh_access(id)? {
            return Ok(t);
        }
        self.refresh_now(id).await
    }

    /// 还能用一天以上的 access token；否则 `None`。
    fn fresh_access(&self, id: &ChatGptAccountId) -> Result<Option<Secret>> {
        let account = self.repo.get(id)?;
        let Some(access) = self.repo.secret(id, ChatGptSecret::Access)? else {
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

    /// 刷一把并落库。致命失败标 `NeedsLogin`；暂时失败时旧 token 若还没真过期就先顶着用。
    async fn refresh_now(&self, id: &ChatGptAccountId) -> Result<Secret> {
        let refresh = self
            .repo
            .secret(id, ChatGptSecret::Refresh)?
            .ok_or_else(|| {
                let _ = self.repo.record_failure(
                    id,
                    "没有 refresh token",
                    Some(ChatGptStatus::NeedsLogin),
                );
                AppError::new(
                    ErrorCode::SecretMissing,
                    "这个 ChatGPT 账号没有 refresh token。",
                )
                .with_hint("重新授权一次。")
            })?;
        match self.tokens.refresh(&refresh).await {
            Ok(mut fresh) => {
                if fresh.refresh_token.is_none() {
                    fresh.refresh_token = Some(refresh);
                }
                self.repo.store_tokens(id, &fresh)?;
                // 刷新响应里带身份（套餐可能变），顺手更新。
                let identity = identity_from_tokens(
                    fresh.id_token.as_ref().map(|t| t.expose()),
                    Some(fresh.access_token.expose()),
                );
                let _ = self.touch_identity(id, &identity);
                Ok(fresh.access_token)
            }
            Err(err) if err.fatal => {
                self.repo
                    .record_failure(id, &err.message, Some(ChatGptStatus::NeedsLogin))?;
                Err(err.into_app_error())
            }
            Err(err) => {
                self.repo.record_failure(id, &err.message, None)?;
                // 网络抖动：旧 token 没真过期就先用。
                if let Some(access) = self.repo.secret(id, ChatGptSecret::Access)? {
                    if jwt_expiry(access.expose()).is_some_and(|exp| {
                        exp > OffsetDateTime::now_utc() + Duration::from_secs(60)
                    }) {
                        tracing::warn!(account = %id, %err, "刷新失败，先用还没过期的旧 access token");
                        return Ok(access);
                    }
                }
                Err(err.into_app_error())
            }
        }
    }

    fn touch_identity(&self, id: &ChatGptAccountId, identity: &Identity) -> Result<()> {
        if identity.plan_type.is_none() && identity.email.is_none() {
            return Ok(());
        }
        let account = self.repo.get(id)?;
        if identity
            .account_id
            .as_deref()
            .is_some_and(|a| a != account.account_ref)
        {
            // 刷出来的是另一个账号的 token？不该发生；不动，留日志。
            tracing::warn!(account = %id, "刷新响应里的 chatgpt_account_id 和账号不一致，忽略身份更新");
            return Ok(());
        }
        self.repo.update_identity(id, identity)
    }

    /// 给网关：这个号的 `chatgpt_account_id`。
    pub fn account_ref(&self, id: &ChatGptAccountId) -> Result<String> {
        Ok(self.repo.get(id)?.account_ref)
    }

    // ── 额度 ─────────────────────────────────────────────────────────────────

    /// 主动问一次 `/wham/usage`。它在 `chatgpt.com` 的 web 面上，从机房 IP 可能被 Cloudflare 挑战；
    /// 本机（家庭宽带）一般能过。拿不到就报错、保留上一次的快照。
    pub async fn refresh_usage(&self, id: &ChatGptAccountId) -> Result<CodexUsage> {
        let access = self.access_token(id).await?;
        let account_ref = self.account_ref(id)?;
        let mut req = self.http.get(format!("{}/wham/usage", self.backend_url));
        for (k, v) in protocol::identity_headers(access.expose(), Some(&account_ref)) {
            req = req.header(k, v);
        }
        let res = req.send().await.map_err(|e| {
            let _ = self
                .repo
                .record_failure(id, &format!("额度接口连不上：{e}"), None);
            AppError::network(format!("连不上 chatgpt.com：{e}"))
        })?;
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        if status == 401 {
            self.repo
                .record_failure(id, "额度接口 401", Some(ChatGptStatus::NeedsLogin))?;
            return Err(AppError::unauthorized("chatgpt.com 拒绝了这个号的凭证。"));
        }
        if !(200..300).contains(&status) {
            let head: String = text.chars().take(160).collect();
            let looks_like_page = head.to_ascii_lowercase().contains("<html")
                || head.to_ascii_lowercase().contains("<!doctype");
            let msg = if looks_like_page {
                format!("额度接口被前置网关拦下（{status}），多半是出口 IP 被挑战")
            } else {
                format!("额度接口 {status}：{head}")
            };
            self.repo.record_failure(id, &msg, None)?;
            return Err(AppError::upstream(msg));
        }
        let payload: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| AppError::upstream("额度接口返回的不是 JSON"))?;
        let usage = CodexUsage::from_wham(&payload, OffsetDateTime::now_utc());
        self.repo.record_usage(id, &usage)?;
        Ok(usage)
    }

    /// 网关从响应头里读到的额度，落成快照。
    pub fn record_usage(&self, id: &ChatGptAccountId, usage: &CodexUsage) -> Result<()> {
        self.repo.record_usage(id, usage)
    }

    /// 网关报「这个号的凭证被上游拒了」。
    pub fn mark_unauthorized(&self, id: &ChatGptAccountId, message: &str) -> Result<()> {
        self.repo
            .record_failure(id, message, Some(ChatGptStatus::NeedsLogin))
    }

    /// 网关报「账号或工作区被停用」。
    pub fn mark_dead(&self, id: &ChatGptAccountId, message: &str) -> Result<()> {
        self.repo
            .record_failure(id, message, Some(ChatGptStatus::Dead))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Form, Query, State};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use nexus_store::MemorySecrets;
    use std::sync::atomic::AtomicUsize;

    fn jwt(payload: serde_json::Value) -> String {
        use base64::Engine;
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes());
        format!(
            "{}.{}.sig",
            b64(r#"{"alg":"RS256"}"#),
            b64(&payload.to_string())
        )
    }

    fn id_token(email: &str, account: &str) -> String {
        jwt(serde_json::json!({
            "email": email,
            "https://api.openai.com/auth": { "chatgpt_account_id": account, "chatgpt_plan_type": "plus" }
        }))
    }

    fn access_token(account: &str, exp: i64) -> String {
        jwt(serde_json::json!({
            "exp": exp,
            "https://api.openai.com/profile": { "email": "fallback@example.com" },
            "https://api.openai.com/auth": { "chatgpt_account_id": account, "chatgpt_plan_type": "plus" }
        }))
    }

    const RT: &str = "rt-opaque-refresh-token-0123456789";

    /// 假的 auth.openai.com + chatgpt.com。按队列给 token 响应；额度接口按脚本。
    #[derive(Default)]
    struct Fake {
        token_calls: Mutex<Vec<HashMap<String, String>>>,
        token_queue: Mutex<Vec<(u16, serde_json::Value)>>,
        usage_calls: AtomicUsize,
        usage_status: Mutex<u16>,
        usage_headers: Mutex<Vec<HashMap<String, String>>>,
    }

    async fn token(
        State(f): State<Arc<Fake>>,
        Form(form): Form<HashMap<String, String>>,
    ) -> (axum::http::StatusCode, Json<serde_json::Value>) {
        f.token_calls.lock().unwrap().push(form.clone());
        let mut q = f.token_queue.lock().unwrap();
        let (status, body) = if q.is_empty() {
            (
                200,
                serde_json::json!({
                    "access_token": access_token("acct_1", 4_000_000_000),
                    "refresh_token": format!("{RT}-rotated-{}", f.token_calls.lock().unwrap().len()),
                    "id_token": id_token("alice@example.com", "acct_1"),
                    "expires_in": 864000,
                }),
            )
        } else {
            q.remove(0)
        };
        (
            axum::http::StatusCode::from_u16(status).unwrap(),
            Json(body),
        )
    }

    async fn usage(
        State(f): State<Arc<Fake>>,
        headers: axum::http::HeaderMap,
        Query(_q): Query<HashMap<String, String>>,
    ) -> (axum::http::StatusCode, String) {
        f.usage_calls.fetch_add(1, Ordering::SeqCst);
        f.usage_headers.lock().unwrap().push(
            headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .collect(),
        );
        let status = *f.usage_status.lock().unwrap();
        if status != 200 {
            return (
                axum::http::StatusCode::from_u16(status).unwrap(),
                if status == 403 {
                    "<!DOCTYPE html><html>challenge</html>".into()
                } else {
                    "{}".into()
                },
            );
        }
        (
            axum::http::StatusCode::OK,
            serde_json::json!({
                "plan_type": "pro",
                "rate_limit": {
                    "primary_window": { "used_percent": 37.5, "reset_after_seconds": 600, "limit_window_seconds": 18000 },
                    "secondary_window": { "used_percent": 12, "reset_after_seconds": 86400, "limit_window_seconds": 604800 }
                }
            })
            .to_string(),
        )
    }

    async fn models(
        State(f): State<Arc<Fake>>,
        headers: axum::http::HeaderMap,
        Query(q): Query<HashMap<String, String>>,
    ) -> (axum::http::StatusCode, String) {
        f.usage_headers.lock().unwrap().push(
            headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
                .chain(std::iter::once((
                    "client_version".to_string(),
                    q.get("client_version").cloned().unwrap_or_default(),
                )))
                .collect(),
        );
        (
            axum::http::StatusCode::OK,
            serde_json::json!({ "models": [
                { "slug": "gpt-5.4", "visibility": "list", "available_in_plans": ["plus", "pro"], "minimal_client_version": "0.140.0",
                  "supported_reasoning_levels": [{ "effort": "low" }, { "effort": "high" }] },
                { "slug": "hidden", "visibility": "hide" },
                { "slug": "pro-only", "visibility": "list", "available_in_plans": ["pro"] },
            ] })
            .to_string(),
        )
    }

    async fn spawn_fake() -> (Arc<Fake>, String) {
        let fake = Arc::new(Fake::default());
        *fake.usage_status.lock().unwrap() = 200;
        let app = Router::new()
            .route("/oauth/token", post(token))
            .route("/backend-api/wham/usage", get(usage))
            .route("/backend-api/codex/models", get(models))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (fake, format!("http://{addr}"))
    }

    fn service(base: &str) -> ChatGptService {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::new());
        ChatGptService::new(db, secrets).with_endpoints(
            &format!("{base}/oauth/token"),
            &format!("{base}/backend-api"),
        )
    }

    #[test]
    fn import_text_understands_auth_json_and_dashed_lines() {
        let auth_json = serde_json::json!({
            "OPENAI_API_KEY": null,
            "tokens": { "id_token": id_token("a@x.com", "acct_1"), "access_token": access_token("acct_1", 4_000_000_000), "refresh_token": RT, "account_id": "acct_1" },
            "last_refresh": "2026-09-01T00:00:00Z"
        })
        .to_string();
        let i = parse_import_text(&auth_json).unwrap();
        assert!(i.access_token.is_some() && i.id_token.is_some());
        assert_eq!(i.refresh_token.as_deref(), Some(RT));

        let dashed = parse_import_text(&format!("Bob@Example.com----{RT}")).unwrap();
        assert_eq!(dashed.refresh_token.as_deref(), Some(RT));
        assert!(dashed.access_token.is_none());

        let both = parse_import_text(&format!(
            "{RT}----{}",
            access_token("acct_c", 4_000_000_000)
        ))
        .unwrap();
        assert!(both.access_token.is_some());
        assert_eq!(both.refresh_token.as_deref(), Some(RT));

        let three = parse_import_text(&format!(
            "{}----{}----{RT}",
            id_token("dan@x.com", "acct_d"),
            access_token("acct_d", 4_000_000_000)
        ))
        .unwrap();
        assert!(three.id_token.is_some() && three.access_token.is_some());
        let claims = oauth::decode_jwt_claims(three.access_token.as_deref().unwrap()).unwrap();
        assert!(
            claims.get("https://api.openai.com/profile").is_some(),
            "带 profile 的那把是 access token"
        );

        assert!(parse_import_text("").is_none());
        assert!(parse_import_text("# 注释").is_none());
        assert!(parse_import_text("{not json").is_none());
        assert!(parse_import_text("only@email.com").is_none());
        assert!(
            parse_import_text("short").is_none(),
            "太短不像 refresh token"
        );
    }

    #[tokio::test]
    async fn importing_only_a_refresh_token_refreshes_first_and_lands_an_active_account() {
        let (fake, base) = spawn_fake().await;
        let svc = service(&base);
        let up = svc
            .import_text(&format!("someone@x.com----{RT}"), None)
            .await
            .unwrap();
        assert!(up.created);
        assert_eq!(
            up.account.email.as_deref(),
            Some("alice@example.com"),
            "邮箱来自 token，不是导入行"
        );
        assert_eq!(up.account.account_ref, "acct_1");
        assert_eq!(up.account.plan_type.as_deref(), Some("plus"));
        assert_eq!(up.account.status, ChatGptStatus::Active);
        assert!(up.account.enabled);
        let calls = fake.token_calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["grant_type"], "refresh_token");
        assert_eq!(calls[0]["refresh_token"], RT);
        assert_eq!(
            calls[0]["scope"], "openid profile email",
            "刷新不带 offline_access"
        );
        drop(calls);
        // 轮换后的 refresh token 落了库。
        assert!(svc
            .repo
            .secret(&up.account.id, ChatGptSecret::Refresh)
            .unwrap()
            .unwrap()
            .expose()
            .contains("rotated"));
    }

    #[tokio::test]
    async fn access_token_is_served_from_cache_until_it_nears_expiry_then_refreshed_once() {
        let (fake, base) = spawn_fake().await;
        let svc = service(&base);
        // 导入一组还有十天的 token：不该刷。
        let far = OffsetDateTime::now_utc().unix_timestamp() + 10 * 86_400;
        let up = svc
            .import_text(&format!("{}----{RT}", access_token("acct_1", far)), None)
            .await
            .unwrap();
        let t = svc.access_token(&up.account.id).await.unwrap();
        assert_eq!(t.expose(), access_token("acct_1", far));
        assert!(
            fake.token_calls.lock().unwrap().is_empty(),
            "离过期还远，不刷"
        );

        // 换成只剩 1 小时的：要刷；十个并发只刷一次。
        let soon = OffsetDateTime::now_utc().unix_timestamp() + 3600;
        svc.repo
            .store_tokens(
                &up.account.id,
                &TokenSet {
                    access_token: Secret::new(access_token("acct_1", soon)),
                    refresh_token: None,
                    id_token: None,
                    expires_at: OffsetDateTime::from_unix_timestamp(soon).unwrap(),
                },
            )
            .unwrap();
        let svc = Arc::new(svc);
        let mut handles = Vec::new();
        for _ in 0..10 {
            let s = svc.clone();
            let id = up.account.id.clone();
            handles.push(tokio::spawn(
                async move { s.access_token(&id).await.unwrap() },
            ));
        }
        for h in handles {
            let t = h.await.unwrap();
            assert_eq!(
                t.expose(),
                access_token("acct_1", 4_000_000_000),
                "拿到的是刷出来的新 token"
            );
        }
        assert_eq!(
            fake.token_calls.lock().unwrap().len(),
            1,
            "并发合并成一次刷新"
        );
    }

    #[tokio::test]
    async fn a_revoked_refresh_token_marks_needs_login_but_a_blip_keeps_the_old_token() {
        let (fake, base) = spawn_fake().await;
        let svc = service(&base);
        let soon = OffsetDateTime::now_utc().unix_timestamp() + 3600;
        let up = svc
            .import_text(&format!("{}----{RT}", access_token("acct_1", soon)), None)
            .await
            .unwrap();

        // 5xx：暂时的。旧 token 还有一小时，先顶着用；状态不动。
        fake.token_queue.lock().unwrap().push((
            503,
            serde_json::json!({ "error": "temporarily_unavailable" }),
        ));
        let t = svc.access_token(&up.account.id).await.unwrap();
        assert_eq!(t.expose(), access_token("acct_1", soon));
        let a = svc.get(&up.account.id).unwrap();
        assert_eq!(a.status, ChatGptStatus::Active);
        assert!(a.last_error.as_deref().unwrap().contains("503"));

        // invalid_grant：致命。标 needs_login，报 unauthorized。
        fake.token_queue.lock().unwrap().push((
            400,
            serde_json::json!({ "error": "invalid_grant", "error_description": "refresh token revoked" }),
        ));
        let err = svc.access_token(&up.account.id).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Unauthorized);
        assert_eq!(
            svc.get(&up.account.id).unwrap().status,
            ChatGptStatus::NeedsLogin
        );

        // 重新授权（新 token 落库）→ 复活。
        svc.repo
            .store_tokens(
                &up.account.id,
                &TokenSet {
                    access_token: Secret::new(access_token("acct_1", 4_000_000_000)),
                    refresh_token: Some(Secret::new(format!("{RT}-new"))),
                    id_token: None,
                    expires_at: OffsetDateTime::from_unix_timestamp(4_000_000_000).unwrap(),
                },
            )
            .unwrap();
        assert_eq!(
            svc.get(&up.account.id).unwrap().status,
            ChatGptStatus::Active
        );
    }

    #[tokio::test]
    async fn usage_is_fetched_with_codex_identity_headers_and_a_challenge_page_is_named() {
        let (fake, base) = spawn_fake().await;
        let svc = service(&base);
        let up = svc
            .import_text(
                &format!("{}----{RT}", access_token("acct_1", 4_000_000_000)),
                None,
            )
            .await
            .unwrap();
        let u = svc.refresh_usage(&up.account.id).await.unwrap();
        assert_eq!(u.plan_type.as_deref(), Some("pro"));
        assert_eq!(u.primary.unwrap().used_percent, Some(37.5));
        assert_eq!(u.source, "wham/usage");
        let a = svc.get(&up.account.id).unwrap();
        assert_eq!(a.plan_type.as_deref(), Some("pro"), "套餐跟着快照更新");
        assert_eq!(
            a.usage.unwrap().secondary.unwrap().window_minutes,
            Some(10080)
        );
        {
            let seen = fake.usage_headers.lock().unwrap();
            let h = &seen[0];
            assert_eq!(h["chatgpt-account-id"], "acct_1");
            assert_eq!(h["originator"], "codex-tui");
            assert!(h["user-agent"].starts_with("codex-tui/"));
            assert!(h["authorization"].starts_with("Bearer "));
        }

        *fake.usage_status.lock().unwrap() = 403;
        let err = svc.refresh_usage(&up.account.id).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Upstream);
        assert!(err.message.contains("挑战"), "{}", err.message);
        assert_eq!(
            svc.get(&up.account.id).unwrap().status,
            ChatGptStatus::Active,
            "被挑战不是号的错"
        );
        assert!(
            svc.get(&up.account.id).unwrap().usage.is_some(),
            "上一次的快照保留"
        );
    }

    #[tokio::test]
    async fn the_model_manifest_is_fetched_with_the_accounts_plan_and_persisted() {
        let (fake, base) = spawn_fake().await;
        let svc = service(&base);
        assert!(svc.models().is_empty(), "还没拉过");
        assert!(svc.refresh_models_any().await.is_err(), "没号刷不了");

        let up = svc
            .import_text(
                &format!("{}----{RT}", access_token("acct_1", 4_000_000_000)),
                None,
            )
            .await
            .unwrap();
        // 导入的号是 plus：pro-only 不进清单，hidden 不进清单。
        let got = svc.refresh_models_any().await.unwrap();
        assert_eq!(
            got.iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(),
            vec!["gpt-5.4"]
        );
        assert_eq!(got[0].reasoning_levels, vec!["low", "high"]);
        assert_eq!(svc.models(), got, "内存里那份跟着更新");
        {
            let seen = fake.usage_headers.lock().unwrap();
            let h = seen.last().unwrap();
            assert_eq!(
                h["client_version"],
                protocol::CLIENT_VERSION,
                "带上我们自称的版本"
            );
            assert_eq!(h["chatgpt-account-id"], "acct_1");
            assert_eq!(h["originator"], "codex-tui");
        }
        // 落库了：另起一个 service 也读得到。
        let again = ChatGptService::new(svc.repo_db(), Arc::new(MemorySecrets::new()));
        assert_eq!(
            again
                .models()
                .iter()
                .map(|m| m.slug.as_str())
                .collect::<Vec<_>>(),
            vec!["gpt-5.4"]
        );
        let _ = up;
    }

    #[tokio::test]
    async fn login_round_trip_through_the_local_callback_server() {
        let (fake, base) = spawn_fake().await;
        let svc = Arc::new(service(&base));
        // 真实端口 1455 可能被占（本机正跑着 codex login）；那样就只测手贴路径。
        let handle = svc.start_login(Some("测试".into())).await.unwrap();
        assert!(handle
            .authorize_url
            .contains(&format!("state={}", handle.session_id)));
        assert_eq!(handle.redirect_uri, REDIRECT_URI);

        if handle.callback_listening {
            let states = Arc::new(Mutex::new(Vec::new()));
            let waiter = {
                let svc = svc.clone();
                let sid = handle.session_id.clone();
                let states = states.clone();
                tokio::spawn(async move {
                    svc.wait_login(&sid, &|st| states.lock().unwrap().push(st))
                        .await
                })
            };
            // 模拟浏览器：先要 favicon，再回调。
            tokio::time::sleep(Duration::from_millis(50)).await;
            let _ = reqwest::get("http://127.0.0.1:1455/favicon.ico").await;
            let res = reqwest::get(format!(
                "http://127.0.0.1:1455/auth/callback?code=the-code&state={}",
                handle.session_id
            ))
            .await
            .unwrap();
            assert_eq!(res.status(), 200);
            let up = waiter.await.unwrap().unwrap();
            assert!(up.created);
            assert_eq!(up.account.note.as_deref(), Some("测试"));
            assert!(matches!(
                states.lock().unwrap().last(),
                Some(LoginState::Succeeded { .. })
            ));
            let calls = fake.token_calls.lock().unwrap();
            assert_eq!(calls[0]["grant_type"], "authorization_code");
            assert_eq!(calls[0]["code"], "the-code");
            assert_eq!(calls[0]["redirect_uri"], REDIRECT_URI);
            assert_eq!(calls[0]["code_verifier"].len(), 128);
        } else {
            let up = svc
                .complete_login(
                    &handle.session_id,
                    &format!(
                        "http://localhost:1455/auth/callback?code=the-code&state={}",
                        handle.session_id
                    ),
                )
                .await
                .unwrap();
            assert!(up.created);
        }

        // 会话一次性：同一个 session 再来一次要被拒。
        let again = svc
            .complete_login(&handle.session_id, "another-code-12345")
            .await;
        assert!(again.is_err());
    }

    #[tokio::test]
    async fn manual_completion_checks_state_and_rejects_junk() {
        let (fake, base) = spawn_fake().await;
        let svc = service(&base);
        let handle = svc.start_login(None).await.unwrap();
        let junk = svc
            .complete_login(&handle.session_id, "这不是地址")
            .await
            .unwrap_err();
        assert_eq!(junk.code, ErrorCode::InvalidInput);
        let wrong = svc
            .complete_login(
                &handle.session_id,
                "http://localhost:1455/auth/callback?code=c&state=someone-elses",
            )
            .await
            .unwrap_err();
        assert!(wrong.message.contains("state"));
        assert!(
            fake.token_calls.lock().unwrap().is_empty(),
            "state 对不上就不该去换 token"
        );
        let ok = svc
            .complete_login(
                &handle.session_id,
                &format!("the-code#{}", handle.session_id),
            )
            .await
            .unwrap();
        assert!(ok.created);
        assert_eq!(svc.list().unwrap().len(), 1);

        // 没有 chatgpt_account_id 的 token（无订阅的纯平台账号）被拒，不进池子。
        let h2 = svc.start_login(None).await.unwrap();
        fake.token_queue.lock().unwrap().push((
            200,
            serde_json::json!({
                "access_token": jwt(serde_json::json!({ "exp": 4_000_000_000u64, "https://api.openai.com/profile": { "email": "noplan@example.com" } })),
                "refresh_token": "rt-noplan-0123456789abcdef",
                "expires_in": 3600
            }),
        ));
        let err = svc
            .complete_login(&h2.session_id, &format!("code#{}", h2.session_id))
            .await
            .unwrap_err();
        assert!(err.message.contains("chatgpt_account_id"));
        assert_eq!(svc.list().unwrap().len(), 1);
    }
}
