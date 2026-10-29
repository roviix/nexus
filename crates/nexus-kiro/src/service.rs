//! Kiro 账号用例：Builder ID device code、导入、续期。

use crate::model::{KiroAccount, KiroActiveInfo, KiroStatus, KiroSwitchOutcome};
use crate::oauth::{
    identity_from_tokens, jwt_expiry, looks_like_jwt, names_one_user, DeviceCode, Identity,
    RegisteredClient, TokenClient, TokenSet,
};
use crate::protocol::{KIRO_MODELS, REFRESH_AHEAD};
use crate::repo::{KiroAccounts, Upserted};
use crate::usage::KiroUsage;
use nexus_core::{AppError, ErrorCode, KiroAccountId, Result, Secret};
use nexus_store::{Db, KiroSecret, SecretStore};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub const LOGIN_TTL: Duration = Duration::from_secs(30 * 60);

struct LoginSession {
    device: DeviceCode,
    client: RegisteredClient,
    started: Instant,
    note: Option<String>,
    cancelled: Arc<AtomicBool>,
}

struct SocialLoginSession {
    provider: String,
    state: String,
    code_verifier: String,
    started: Instant,
    note: Option<String>,
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
#[serde(rename_all = "camelCase")]
pub struct SocialLoginStart {
    pub session_id: String,
    pub authorize_url: String,
    pub provider: String,
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
        account: Box<KiroAccount>,
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
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub profile_arn: Option<String>,
    pub auth_method: Option<String>,
    pub expires_at: Option<String>,
    /// 导出里的名字或备注。用户没另写备注时用它。
    pub note: Option<String>,
}

impl Imported {
    fn is_empty(&self) -> bool {
        self.access_token.is_none() && self.refresh_token.is_none()
    }
}

/// 粘贴导入的汇总。命令层只把最后一个号回给界面，列表刷新后其余的也在。
pub struct ImportReport {
    pub account: KiroAccount,
    pub created: bool,
    pub accepted: u32,
    pub failed: u32,
    pub errors: Vec<String>,
}

fn import_unrecognized() -> AppError {
    AppError::invalid("认不出这段内容。").with_hint(
        "支持 kiro-auth-token.json、sub2api 的账号导出（accounts 里 platform 为 kiro 的 credentials）、CLIProxyAPI JSON，或 `access----refresh`。",
    )
}

fn looks_like_opaque_token(s: &str) -> bool {
    s.len() >= 16
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-' | b'/' | b'+' | b'=')
        })
}

/// 只取第一份。本机 `kiro-auth-token.json` 走这条。
pub fn parse_import_text(text: &str) -> Option<Imported> {
    parse_import_entries(text).into_iter().next()
}

/// 一份粘贴可能是一个号，也可能是 sub2api 一次导出的一打。
///
/// 认这些写法：
/// - `kiro-auth-token.json`（camelCase），或 CLIProxyAPI 摊平的 JSON；
/// - `{accounts:[{platform,credentials}]}`，`credentials` 是对象或 JSON 字符串都认，别的平台跳过；
/// - JSON 数组、`{items|contents|data.items}` 包一层、NDJSON；
/// - `access----refresh`，一行一个，`#` 当注释。
pub fn parse_import_entries(text: &str) -> Vec<Imported> {
    let raw = text.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    if looks_like_json(raw) {
        let (values, rest) = take_json_values(raw);
        let mut out = Vec::new();
        for v in values {
            out.extend(entries_from_value(v));
        }
        if !rest.is_empty() {
            out.extend(parse_import_lines(rest));
        }
        return out;
    }
    parse_import_lines(raw)
}

fn looks_like_json(s: &str) -> bool {
    matches!(s.as_bytes().first(), Some(b'{' | b'['))
}

fn take_json_values(raw: &str) -> (Vec<serde_json::Value>, &str) {
    let mut stream = serde_json::Deserializer::from_str(raw).into_iter::<serde_json::Value>();
    let mut values = Vec::new();
    loop {
        match stream.next() {
            Some(Ok(v)) => values.push(v),
            Some(Err(_)) | None => {
                let offset = stream.byte_offset().min(raw.len());
                return (values, raw[offset..].trim());
            }
        }
    }
}

fn parse_import_lines(text: &str) -> Vec<Imported> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if looks_like_json(line) {
            let (values, _) = take_json_values(line);
            for v in values {
                out.extend(entries_from_value(v));
            }
        } else if let Some(imported) = parse_dashed(line) {
            out.push(imported);
        }
    }
    out
}

fn entries_from_value(v: serde_json::Value) -> Vec<Imported> {
    match v {
        serde_json::Value::Array(items) => items.into_iter().flat_map(entries_from_value).collect(),
        serde_json::Value::String(s) => parse_import_entries(&s),
        serde_json::Value::Object(_) => {
            for key in ["accounts", "items", "contents"] {
                if let Some(serde_json::Value::Array(items)) = v.get(key).cloned() {
                    return items.into_iter().flat_map(entries_from_value).collect();
                }
            }
            if let Some(data) = v.get("data").cloned() {
                if data.get("items").is_some() || data.get("accounts").is_some() {
                    return entries_from_value(data);
                }
            }
            if rejected_platform(&v) {
                return Vec::new();
            }
            imported_from_object(&v).into_iter().collect()
        }
        _ => Vec::new(),
    }
}

/// 导出里混着别的平台时整条跳过。没有 platform / type 就当 Kiro。
fn rejected_platform(v: &serde_json::Value) -> bool {
    let Some(platform) = pick_str(v, &["platform", "type"]).map(|s| s.to_ascii_lowercase()) else {
        return false;
    };
    !matches!(
        platform.as_str(),
        "kiro" | "amazon-q" | "amazonq" | "codewhisperer" | "aws"
    )
}

fn pick_str(obj: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        obj.get(*k)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn object_field(v: &serde_json::Value, key: &str) -> Option<serde_json::Value> {
    let field = v.get(key)?;
    if field.is_object() {
        return Some(field.clone());
    }
    let raw = field.as_str()?.trim();
    if !raw.starts_with('{') {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .filter(|p| p.is_object())
}

fn fill_imported(into: &mut Imported, from: Imported) {
    if into.access_token.is_none() {
        into.access_token = from.access_token;
    }
    if into.refresh_token.is_none() {
        into.refresh_token = from.refresh_token;
    }
    if into.client_id.is_none() {
        into.client_id = from.client_id;
    }
    if into.client_secret.is_none() {
        into.client_secret = from.client_secret;
    }
    if into.profile_arn.is_none() {
        into.profile_arn = from.profile_arn;
    }
    if into.auth_method.is_none() {
        into.auth_method = from.auth_method;
    }
    if into.expires_at.is_none() {
        into.expires_at = from.expires_at;
    }
    if into.note.is_none() {
        into.note = from.note;
    }
}

fn tokens_from_obj(obj: &serde_json::Value) -> Imported {
    let auth_method = pick_str(obj, &["authMethod", "auth_method", "provider"]).filter(|s| {
        !matches!(
            s.to_ascii_lowercase().as_str(),
            "kiro" | "amazon-q" | "amazonq" | "codewhisperer" | "aws"
        )
    });
    Imported {
        access_token: pick_str(obj, &["accessToken", "access_token"]),
        refresh_token: pick_str(obj, &["refreshToken", "refresh_token"]),
        client_id: pick_str(obj, &["clientId", "client_id"]),
        client_secret: pick_str(obj, &["clientSecret", "client_secret"]),
        profile_arn: pick_str(obj, &["profileArn", "profile_arn"]),
        auth_method,
        expires_at: pick_str(obj, &["expiresAt", "expires_at"]),
        note: pick_str(obj, &["notes", "note", "name"]),
    }
}

fn imported_from_object(v: &serde_json::Value) -> Option<Imported> {
    let mut out = Imported::default();
    for key in ["credentials", "tokens", "attributes"] {
        if let Some(obj) = object_field(v, key) {
            fill_imported(&mut out, tokens_from_obj(&obj));
        }
    }
    fill_imported(&mut out, tokens_from_obj(v));
    (!out.is_empty()).then_some(out)
}

fn parse_dashed(raw: &str) -> Option<Imported> {
    let mut out = Imported::default();
    for part in raw.split("----").map(str::trim).filter(|p| !p.is_empty()) {
        if part.contains('@') || part.contains(char::is_whitespace) {
            continue;
        }
        if looks_like_jwt(part) && out.access_token.is_none() {
            out.access_token = Some(part.to_string());
        } else if looks_like_opaque_token(part) && out.refresh_token.is_none() {
            out.refresh_token = Some(part.to_string());
        }
    }
    (!out.is_empty()).then_some(out)
}

pub struct KiroOutbound {
    pub machine_id: String,
    pub profile_arn: Option<String>,
    pub token_type: Option<String>,
}

pub struct KiroService {
    pub repo: KiroAccounts,
    tokens: TokenClient,
    logins: Mutex<HashMap<String, LoginSession>>,
    social_logins: Mutex<HashMap<String, SocialLoginSession>>,
    refresh_locks: Mutex<HashMap<KiroAccountId, Arc<tokio::sync::Mutex<()>>>>,
    /// 已经问过 `ListAvailableProfiles`、上游确认没有企业 profile 的账号。
    profile_attempts: Mutex<HashSet<String>>,
    /// 问 `ListAvailableProfiles` 出错的账号和时刻。歇一阵再问：每个请求都问的话，
    /// 两个区域各 30 秒的超时会全加到推理请求的首字延迟上。
    profile_failures: Mutex<HashMap<String, Instant>>,
}

/// 问 profile 出错之后，多久之内不再问。
const PROFILE_RETRY_AFTER: Duration = Duration::from_secs(10 * 60);

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(30))
        .build()
        .expect("reqwest 客户端初始化（只在 TLS 后端缺失时失败）")
}

impl KiroService {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            repo: KiroAccounts::new(db, secrets),
            tokens: TokenClient::new(http_client()),
            logins: Mutex::new(HashMap::new()),
            social_logins: Mutex::new(HashMap::new()),
            refresh_locks: Mutex::new(HashMap::new()),
            profile_attempts: Mutex::new(HashSet::new()),
            profile_failures: Mutex::new(HashMap::new()),
        }
    }

    pub fn models(&self) -> Vec<String> {
        KIRO_MODELS
            .iter()
            .map(|(ext, _)| (*ext).to_string())
            .collect()
    }

    /// 出站要用的机器码、流式 profileArn 和 `tokentype`。对不上账号时按 Builder ID 占位。
    pub fn outbound_for(&self, label: &str) -> KiroOutbound {
        let key = label.trim();
        let found = self.list().ok().and_then(|list| {
            list.into_iter()
                .find(|a| a.label().eq_ignore_ascii_case(key))
        });
        let Some(account) = found else {
            return KiroOutbound {
                machine_id: crate::protocol::machine_id(None, key),
                profile_arn: Some(crate::protocol::BUILDER_ID_PROFILE_ARN.to_string()),
                token_type: None,
            };
        };
        let refresh = self
            .repo
            .secret(&account.id, KiroSecret::Refresh)
            .ok()
            .flatten();
        let machine_id =
            crate::protocol::machine_id(refresh.as_ref().map(|s| s.expose()), &account.label());
        let stored = account.stored_profile_arn();
        KiroOutbound {
            machine_id,
            profile_arn: crate::protocol::streaming_profile_arn(
                account.auth_method.as_deref(),
                stored.as_deref(),
            ),
            token_type: crate::protocol::token_type_header(account.auth_method.as_deref())
                .map(str::to_string),
        }
    }

    /// 占位 profile 先问一次上游。问到真实 ARN 就写回账号；确认没有就记住，别每个请求都问。
    pub async fn ensure_streaming_profile(
        &self,
        label: &str,
        access_token: &str,
    ) -> Option<String> {
        let outbound = self.outbound_for(label);
        if outbound.token_type.as_deref() == Some("API_KEY") {
            return None;
        }
        let current = outbound.profile_arn.clone();
        let needs = current
            .as_deref()
            .is_none_or(crate::protocol::is_placeholder_profile_arn);
        if !needs {
            return current;
        }
        let Some(account) = self.list().ok().and_then(|list| {
            list.into_iter()
                .find(|a| a.label().eq_ignore_ascii_case(label.trim()))
        }) else {
            return current;
        };
        let id = account.id.as_str().to_string();
        if self
            .profile_attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&id)
        {
            return current;
        }
        if self
            .profile_failures
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .is_some_and(|at| at.elapsed() < PROFILE_RETRY_AFTER)
        {
            return current;
        }
        match self
            .tokens
            .list_profiles(access_token, outbound.token_type.as_deref())
            .await
        {
            Ok(Some(arn)) => {
                if let Err(err) = self.repo.set_profile_arn(&account.id, &arn) {
                    tracing::warn!(%err, "写回 Kiro profileArn 失败");
                }
                self.profile_attempts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id);
                Some(arn)
            }
            Ok(None) => {
                self.profile_attempts
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id);
                current
            }
            Err(err) => {
                tracing::warn!(%err, "解析 Kiro profileArn 失败，先用占位符，10 分钟后再问");
                self.profile_failures
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id, Instant::now());
                current
            }
        }
    }

    pub fn list(&self) -> Result<Vec<KiroAccount>> {
        self.repo.list()
    }

    pub fn get(&self, id: &KiroAccountId) -> Result<KiroAccount> {
        self.repo.get(id)
    }

    pub fn remove(&self, id: &KiroAccountId) -> Result<()> {
        self.repo.remove(id)
    }

    pub fn set_enabled(&self, id: &KiroAccountId, enabled: bool) -> Result<KiroAccount> {
        self.repo.set_enabled(id, enabled)
    }

    pub fn set_note(&self, id: &KiroAccountId, note: Option<&str>) -> Result<KiroAccount> {
        self.repo.set_note(id, note)
    }

    pub async fn start_login(&self, note: Option<String>) -> Result<LoginHandle> {
        self.sweep_logins();
        let client = self.tokens.register().await?;
        let device = self.tokens.start_device(&client).await?;
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
                client,
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
        let (device, client, cancelled, note, started) = {
            let logins = self.logins.lock().expect("logins");
            let Some(s) = logins.get(session_id) else {
                return Err(AppError::invalid("这次授权已过期或不存在，重新发起一次。"));
            };
            (
                s.device.clone(),
                s.client.clone(),
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
            match self.tokens.poll_once(&client, &device.device_code).await {
                Ok(Some(tokens)) => {
                    self.logins.lock().expect("logins").remove(session_id);
                    match self
                        .add_tokens(tokens, note.as_deref(), Some("builder-id"), None)
                        .await
                    {
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
                Err(_) => {
                    interval = interval.saturating_mul(2).min(Duration::from_secs(15));
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

    pub fn start_social_login(
        &self,
        provider: &str,
        note: Option<String>,
    ) -> Result<SocialLoginStart> {
        self.sweep_social_logins();
        let provider_name = if provider.eq_ignore_ascii_case("github") {
            "Github"
        } else {
            "Google"
        };
        let session_id = uuid::Uuid::new_v4().to_string();
        let state = uuid::Uuid::new_v4().to_string();
        use rand::Rng;
        let mut verifier_bytes = [0u8; 32];
        rand::rng().fill_bytes(&mut verifier_bytes);
        use base64::Engine;
        let code_verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(verifier_bytes);
        let code_challenge = crate::protocol::code_challenge(&code_verifier);
        let authorize_url =
            crate::protocol::social_authorize_url(provider_name, &state, &code_challenge);

        self.social_logins.lock().expect("social_logins").insert(
            session_id.clone(),
            SocialLoginSession {
                provider: provider_name.to_string(),
                state,
                code_verifier,
                started: Instant::now(),
                note,
            },
        );

        Ok(SocialLoginStart {
            session_id,
            authorize_url,
            provider: provider_name.to_string(),
        })
    }

    pub async fn complete_social_login(
        &self,
        session_id: &str,
        callback: &str,
    ) -> Result<Upserted> {
        let (code, state) = crate::protocol::parse_social_callback(callback);
        let code = code.ok_or_else(|| {
            AppError::invalid("未在输入中找到有效的授权码（code）。").with_hint(
                "请确认已在浏览器中完成登录，并将以 kiro://kiro.kiroAgent/authenticate-success?code= 开头的重定向链接完整粘贴进来。",
            )
        })?;

        let session = {
            let mut logins = self.social_logins.lock().expect("social_logins");
            if !session_id.is_empty() {
                if let Some(s) = logins.remove(session_id) {
                    s
                } else if let Some(st) = state.as_deref() {
                    let found_key = logins
                        .iter()
                        .find(|(_, s)| s.state == st)
                        .map(|(k, _)| k.clone());
                    if let Some(k) = found_key {
                        logins.remove(&k).unwrap()
                    } else if logins.len() == 1 {
                        let key = logins.keys().next().cloned().unwrap();
                        logins.remove(&key).unwrap()
                    } else {
                        return Err(AppError::invalid(
                            "社交登录会话已过期或不存在，请重新发起登录。",
                        ));
                    }
                } else if logins.len() == 1 {
                    let key = logins.keys().next().cloned().unwrap();
                    logins.remove(&key).unwrap()
                } else {
                    return Err(AppError::invalid(
                        "社交登录会话已过期或不存在，请重新发起登录。",
                    ));
                }
            } else if let Some(st) = state.as_deref() {
                let found_key = logins
                    .iter()
                    .find(|(_, s)| s.state == st)
                    .map(|(k, _)| k.clone());
                if let Some(k) = found_key {
                    logins.remove(&k).unwrap()
                } else if logins.len() == 1 {
                    let key = logins.keys().next().cloned().unwrap();
                    logins.remove(&key).unwrap()
                } else {
                    return Err(AppError::invalid(
                        "社交登录会话已过期或不存在，请重新发起登录。",
                    ));
                }
            } else if logins.len() == 1 {
                let key = logins.keys().next().cloned().unwrap();
                logins.remove(&key).unwrap()
            } else {
                return Err(AppError::invalid(
                    "社交登录会话已过期或不存在，请重新发起登录。",
                ));
            }
        };

        if session.started.elapsed() > LOGIN_TTL {
            return Err(AppError::invalid("本次登录会话已超时，请重新发起登录。"));
        }

        let (tokens, profile_arn) = self
            .tokens
            .exchange_social_code(&code, &session.code_verifier)
            .await?;

        let fallback_profile =
            profile_arn.or_else(|| Some(crate::protocol::SOCIAL_PROFILE_ARN.to_string()));

        let auth_method = if session.provider.eq_ignore_ascii_case("github") {
            "github"
        } else {
            "google"
        };

        self.add_tokens(
            tokens,
            session.note.as_deref(),
            Some(auth_method),
            fallback_profile.as_deref(),
        )
        .await
    }

    pub fn cancel_social_login(&self, session_id: &str) {
        self.social_logins
            .lock()
            .expect("social_logins")
            .remove(session_id);
    }

    fn sweep_social_logins(&self) {
        self.social_logins
            .lock()
            .expect("social_logins")
            .retain(|_, s| s.started.elapsed() <= LOGIN_TTL);
    }

    pub async fn add_tokens(
        &self,
        mut tokens: TokenSet,
        note: Option<&str>,
        auth_method: Option<&str>,
        fallback_ref: Option<&str>,
    ) -> Result<Upserted> {
        let mut identity = identity_from_tokens(&tokens, fallback_ref);
        if identity.auth_method.is_none() {
            identity.auth_method = auth_method.map(str::to_string);
        }
        // 旧行存的是导进来的这把 refresh token；whoami 换 access token 时上游可能顺带换掉它。
        let imported_refresh = tokens.refresh_token.clone();
        // 社交登录和 Builder ID 的 access token 不是 JWT，读不出是谁，身份只剩共用的 profileArn
        // 或一次性的 client。先问一次用量接口，拿 userInfo.userId 当身份，邮箱和额度顺带记下。
        let mut usage = None;
        let mut lookup_err = None;
        if !names_one_user(&identity.subject) {
            match self.whoami(&mut tokens, &identity).await {
                Ok(u) => usage = Some(u),
                Err(err) => {
                    tracing::warn!(%err, "导入时问不到 Kiro 用户身份，先按 profileArn / client 记");
                    lookup_err = Some(err);
                }
            }
        }
        if let Some(u) = &usage {
            if let Some(uid) = &u.user_id {
                identity.subject = uid.clone();
            }
            if identity.email.is_none() {
                identity.email = u.email.clone();
            }
        }
        if identity.subject.is_empty() {
            return Err(AppError::invalid("这组 token 读不出稳定身份。"));
        }
        if names_one_user(&identity.subject) {
            // 先把共用 ARN 上的旧行问出真名，再决定认回哪一行。
            self.heal_shared_rows(identity.profile_arn.as_deref()).await;
            self.repo.adopt_legacy(
                &identity.subject,
                imported_refresh.as_ref().map(|s| s.expose()),
                identity.profile_arn.as_deref(),
            )?;
        } else if identity.subject.starts_with("arn:") {
            self.refuse_shared_overwrite(
                &identity.subject,
                imported_refresh.as_ref(),
                lookup_err.as_ref(),
            )?;
        }
        let mut up = self.repo.upsert(&identity, &tokens, note)?;
        if let Some(u) = &usage {
            self.repo.save_usage(&up.account.id, u)?;
            up.account = self.repo.get(&up.account.id)?;
        }
        Ok(up)
    }

    /// 问 `getUsageLimits` 这组 token 是谁，顺带拿到套餐和额度。access token 快过期、或者被
    /// 上游 401 了（导出文件里的 `expiresAt` 缺了 / 认不出时只能这样发现），用 refresh token
    /// 换一把再问。问不到时调用方先按 profileArn / client 记，刷新额度时再改记。
    async fn whoami(&self, tokens: &mut TokenSet, identity: &Identity) -> Result<KiroUsage> {
        let token_type = crate::protocol::token_type_header(identity.auth_method.as_deref());
        let profile_arn = identity.profile_arn.as_deref();
        let stale = tokens
            .expires_at
            .is_some_and(|exp| exp <= OffsetDateTime::now_utc() + Duration::from_secs(60));
        let refreshed = stale && self.refresh_for_import(tokens).await;
        let first = self
            .tokens
            .usage_limits(tokens.access_token.expose(), token_type, profile_arn)
            .await;
        match first {
            Err(err) if err.code == ErrorCode::Unauthorized && !refreshed => {
                if self.refresh_for_import(tokens).await {
                    self.tokens
                        .usage_limits(tokens.access_token.expose(), token_type, profile_arn)
                        .await
                } else {
                    Err(err)
                }
            }
            other => other,
        }
    }

    /// 导入时用 refresh token 换一把 access token，换到了回 `true`。上游没回的字段沿用原来的。
    async fn refresh_for_import(&self, tokens: &mut TokenSet) -> bool {
        let Some(refresh) = tokens.refresh_token.clone() else {
            return false;
        };
        let refreshed = self
            .refresh_imported(
                refresh.expose(),
                tokens.client_id.as_ref().map(|s| s.expose()),
                tokens.client_secret.as_ref().map(|s| s.expose()),
            )
            .await;
        match refreshed {
            Ok(mut fresh) => {
                fresh.refresh_token = fresh.refresh_token.or(Some(refresh));
                fresh.client_id = fresh.client_id.or_else(|| tokens.client_id.take());
                fresh.client_secret = fresh.client_secret.or_else(|| tokens.client_secret.take());
                *tokens = fresh;
                true
            }
            Err(err) => {
                tracing::warn!(%err, "导入时换不到新的 Kiro access token");
                false
            }
        }
    }

    /// 认不出是谁时只能按共用的 profileArn 记。那一行要是存着另一把 refresh token，可能是别人的号，
    /// 也可能是同一个人重新登录过——分不清就不导，免得把别人的凭证盖掉。
    /// 那一行已经用不了（需要重新授权 / 停用）时照旧原地复活它：它的凭证本来就失效了。
    fn refuse_shared_overwrite(
        &self,
        account_ref: &str,
        refresh: Option<&Secret>,
        why: Option<&AppError>,
    ) -> Result<()> {
        let Some(existing) = self.repo.by_ref(account_ref)? else {
            return Ok(());
        };
        if existing.status != KiroStatus::Active {
            return Ok(());
        }
        let stored = self.repo.secret(&existing.id, KiroSecret::Refresh)?;
        if let (Some(new), Some(old)) = (refresh, stored.as_ref()) {
            if new.expose() == old.expose() {
                return Ok(());
            }
        }
        let label = existing.label();
        let (why, hint) = match why {
            Some(e) if e.code == ErrorCode::Unauthorized => (
                e.message.as_str(),
                "这组凭证已经失效（换 token 也不行），换一份新的再导。".to_string(),
            ),
            Some(e) => (
                e.message.as_str(),
                format!("等网络恢复再导一次。「{label}」刷新过一次额度、改记到自己名下以后，就不会再撞上。"),
            ),
            None => (
                "上游没给 userId",
                format!("「{label}」刷新过一次额度、改记到自己名下以后再导，就不会撞上。"),
            ),
        };
        Err(AppError::upstream(format!(
            "认不出这组 Kiro 凭证属于谁（{why}）。按共用的 profileArn 记会盖掉「{label}」。"
        ))
        .with_hint(hint))
    }

    /// 把还记在 `shared` 这个共用 profileArn 上的旧行问出真实身份，改记到人名下。
    ///
    /// 为什么非问不可：同一个共用 ARN 下，「同一个人重新登录」和「另一个 Google 号」长得
    /// 一模一样——`adopt_legacy` 只能靠 refresh token 对得上才敢认回，而账号管理器切号时
    /// 会轮换它。对不上又不敢认，`upsert` 就另起一行，同一个号在号池里占两格，旧那格还带
    /// 着一把过期凭证。所以先拿旧行自己的凭证去问一次 `getUsageLimits`：问到就改记到
    /// userId 上，之后 `upsert` 自然对得上——同一个人更新那一行，不同的人各走一行。
    ///
    /// 每行最多问一次（问到就不再是旧行了），问不到就按原样留着，由 `adopt_legacy` /
    /// `refuse_shared_overwrite` 兜住。
    async fn heal_shared_rows(&self, shared: Option<&str>) {
        let Some(shared) = shared.map(str::trim).filter(|s| s.starts_with("arn:")) else {
            return;
        };
        let Ok(rows) = self.repo.list() else {
            return;
        };
        for account in rows {
            if account.account_ref != shared || names_one_user(&account.account_ref) {
                continue;
            }
            if !account.has_refresh {
                continue;
            }
            let label = account.label();
            match self.refresh_usage(&account.id).await {
                Ok(_) => tracing::info!(%label, "共用 profileArn 上的旧 Kiro 行已改记到本人名下"),
                Err(err) => {
                    tracing::warn!(%err, %label, "问不到旧 Kiro 行的身份，先按原样留着")
                }
            }
        }
    }

    /// 旧版把社交登录记在共用的 profileArn 上，Builder ID 记在 client 上。问到 userId 以后
    /// 改记到人身上。若那个人的 canonical 行已经存在，保留 canonical 凭证，把刚验证过的
    /// 用量/邮箱和缺失的 profile 搬过去，再删掉旧行；不能只告警，否则两行都会进入号池。
    fn rekey_to_user(&self, account: &KiroAccount, usage: &KiroUsage) -> Result<KiroAccountId> {
        let Some(uid) = usage
            .user_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Ok(account.id.clone());
        };
        if names_one_user(&account.account_ref) {
            return Ok(account.id.clone());
        }
        if self.repo.rekey(&account.id, uid)? {
            return Ok(account.id.clone());
        }
        let Some(canonical) = self.repo.by_ref(uid)? else {
            return Err(AppError::internal(format!(
                "Kiro 账号改记到 {uid} 失败，但目标行也不存在。"
            )));
        };
        if canonical.stored_profile_arn().is_none() {
            if let Some(profile) = account.stored_profile_arn() {
                self.repo.set_profile_arn(&canonical.id, &profile)?;
            }
        }
        self.repo.save_usage(&canonical.id, usage)?;
        self.repo.remove(&account.id)?;
        tracing::info!(
            duplicate = %account.id,
            canonical = %canonical.id,
            "合并重复的 Kiro 共用 profileArn 行"
        );
        Ok(canonical.id)
    }

    pub async fn import_text(&self, text: &str, note: Option<&str>) -> Result<Upserted> {
        let imported = parse_import_text(text).ok_or_else(import_unrecognized)?;
        self.import_one(imported, note).await
    }

    /// 一次贴多个号。能进的先进，一条坏的不把整份退掉。
    pub async fn import_dump(&self, text: &str, note: Option<&str>) -> Result<ImportReport> {
        let trimmed = text.trim();
        if trimmed.starts_with("kiro://") || trimmed.contains("code=") {
            if let Ok(up) = self.complete_social_login("", trimmed).await {
                return Ok(ImportReport {
                    account: up.account,
                    created: up.created,
                    accepted: 1,
                    failed: 0,
                    errors: Vec::new(),
                });
            }
        }
        let entries = parse_import_entries(text);
        if entries.is_empty() {
            return Err(import_unrecognized());
        }
        let mut last = None;
        let mut accepted = 0u32;
        let mut failed = 0u32;
        let mut errors = Vec::new();
        for imported in entries {
            let item_note = note.map(str::to_string).or_else(|| imported.note.clone());
            match self.import_one(imported, item_note.as_deref()).await {
                Ok(up) => {
                    accepted += 1;
                    last = Some(up);
                }
                Err(err) => {
                    failed += 1;
                    if errors.len() < 5 {
                        errors.push(err.message);
                    }
                }
            }
        }
        let Some(up) = last else {
            let detail = errors
                .first()
                .cloned()
                .unwrap_or_else(|| "没有可用的凭证。".into());
            return Err(AppError::invalid(format!("一个都没导进去。{detail}"))
                .with_hint("检查 refresh token 和 profileArn。别的平台的账号会被跳过。"));
        };
        Ok(ImportReport {
            account: up.account,
            created: up.created,
            accepted,
            failed,
            errors,
        })
    }

    async fn import_one(&self, imported: Imported, note: Option<&str>) -> Result<Upserted> {
        let tokens = match (&imported.access_token, &imported.refresh_token) {
            (Some(access), refresh) => TokenSet {
                expires_at: imported
                    .expires_at
                    .as_deref()
                    .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
                    .or_else(|| jwt_expiry(access)),
                access_token: Secret::new(access.clone()),
                refresh_token: refresh.clone().map(Secret::new),
                client_id: imported.client_id.clone().map(Secret::new),
                client_secret: imported.client_secret.clone().map(Secret::new),
            },
            (None, Some(refresh)) => {
                let mut fresh = self
                    .refresh_imported(
                        refresh,
                        imported.client_id.as_deref(),
                        imported.client_secret.as_deref(),
                    )
                    .await?;
                if fresh.refresh_token.is_none() {
                    fresh.refresh_token = Some(Secret::new(refresh.clone()));
                }
                if fresh.client_id.is_none() {
                    fresh.client_id = imported.client_id.clone().map(Secret::new);
                }
                if fresh.client_secret.is_none() {
                    fresh.client_secret = imported.client_secret.clone().map(Secret::new);
                }
                fresh
            }
            (None, None) => unreachable!("parse_import_text 不会给出两个都空的结果"),
        };
        let method = imported
            .auth_method
            .as_deref()
            .or(if imported.client_id.is_some() {
                Some("builder-id")
            } else {
                Some("social")
            });
        let fallback_arn = imported
            .profile_arn
            .as_deref()
            .or_else(|| (method == Some("social")).then_some(crate::protocol::SOCIAL_PROFILE_ARN));
        self.add_tokens(tokens, note, method, fallback_arn).await
    }

    async fn refresh_imported(
        &self,
        refresh: &str,
        client_id: Option<&str>,
        client_secret: Option<&str>,
    ) -> Result<TokenSet> {
        match (client_id, client_secret) {
            (Some(cid), Some(csec)) if !cid.is_empty() && !csec.is_empty() => {
                self.tokens.refresh_oidc(cid, csec, refresh).await
            }
            _ => self.tokens.refresh_social(refresh).await,
        }
    }

    /// 从本机 Kiro IDE 登录态导入（默认 `~/.aws/sso/cache/kiro-auth-token.json`）。
    pub async fn import_kiro_cli(&self, path: &Path) -> Result<Upserted> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            AppError::new(ErrorCode::Io, format!("读不到 {}：{e}", path.display()))
                .with_hint("先在 Kiro IDE 登录一次，或用「授权登录」。")
        })?;
        self.import_text(&raw, Some("来自本机 Kiro")).await
    }

    /// 将指定账号的最新有效凭证写入本机 Kiro IDE 客户端配置文件（`~/.aws/sso/cache/kiro-auth-token.json`）。
    pub async fn switch_to_client(
        &self,
        id: &KiroAccountId,
        home_dir: &Path,
    ) -> Result<KiroSwitchOutcome> {
        let account = self.repo.get(id)?;

        // 确保拿到最新且未过期的 Access Token 和有效 Refresh Token
        let access_token = self.access_token(id).await?;
        let refresh_token = self.repo.secret(id, KiroSecret::Refresh)?.ok_or_else(|| {
            AppError::new(
                ErrorCode::SecretMissing,
                "该 Kiro 账号没有 Refresh Token，无法切号到客户端。",
            )
            .with_hint("请在抽屉中点击「重新授权」一次。")
        })?;

        let client_id = self.repo.secret(id, KiroSecret::ClientId)?;
        let client_secret = self.repo.secret(id, KiroSecret::ClientSecret)?;

        let auth_method = account.auth_method.as_deref().unwrap_or("social");
        let is_social = auth_method == "social"
            || auth_method.eq_ignore_ascii_case("github")
            || auth_method.eq_ignore_ascii_case("google");

        let provider = if is_social {
            if auth_method.eq_ignore_ascii_case("google") {
                "Google"
            } else {
                "Github"
            }
        } else {
            "BuilderId"
        };

        let profile_arn = account
            .profile_arn
            .as_deref()
            .filter(|arn| !arn.is_empty())
            .unwrap_or(if is_social {
                crate::protocol::SOCIAL_PROFILE_ARN
            } else {
                "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX"
            });

        let sso_cache_dir = home_dir.join(".aws").join("sso").join("cache");
        std::fs::create_dir_all(&sso_cache_dir).map_err(|e| {
            AppError::new(
                ErrorCode::Io,
                format!("创建 SSO 缓存目录失败 ({}): {e}", sso_cache_dir.display()),
            )
        })?;

        let expires_at_iso = (time::OffsetDateTime::now_utc() + time::Duration::hours(1))
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|e| AppError::internal(format!("格式化时间失败: {e}")))?;

        let token_data = if is_social {
            serde_json::json!({
                "accessToken": access_token.expose(),
                "refreshToken": refresh_token.expose(),
                "profileArn": profile_arn,
                "expiresAt": expires_at_iso,
                "authMethod": "social",
                "provider": provider
            })
        } else {
            let client_id_hash = "e909a0580879b06ece1202964fbe9dda95ea4ce3";
            serde_json::json!({
                "accessToken": access_token.expose(),
                "refreshToken": refresh_token.expose(),
                "expiresAt": expires_at_iso,
                "clientIdHash": client_id_hash,
                "authMethod": "IdC",
                "provider": "BuilderId",
                "region": "us-east-1",
                "profileArn": profile_arn
            })
        };

        let token_path = sso_cache_dir.join("kiro-auth-token.json");
        let raw_json = serde_json::to_string_pretty(&token_data)
            .map_err(|e| AppError::internal(format!("序列化 token 失败: {e}")))?;

        // 原子写入 kiro-auth-token.json
        let tmp_path = sso_cache_dir.join(".kiro-auth-token.json.tmp");
        std::fs::write(&tmp_path, &raw_json).map_err(|e| {
            AppError::new(
                ErrorCode::Io,
                format!("写入临时文件失败 ({}): {e}", tmp_path.display()),
            )
        })?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&tmp_path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o600);
                let _ = std::fs::set_permissions(&tmp_path, perms);
            }
        }

        std::fs::rename(&tmp_path, &token_path).map_err(|e| {
            AppError::new(
                ErrorCode::Io,
                format!("替换 {} 失败: {e}", token_path.display()),
            )
        })?;

        // 若存在 client_id & client_secret 且为 IdC / BuilderId，一并写入客户端注册缓存
        if !is_social {
            if let (Some(cid), Some(csec)) = (client_id, client_secret) {
                let client_id_hash = "e909a0580879b06ece1202964fbe9dda95ea4ce3";
                let client_reg_path = sso_cache_dir.join(format!("{client_id_hash}.json"));
                let client_expires_at = (time::OffsetDateTime::now_utc()
                    + time::Duration::days(90))
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_default();
                let client_reg_data = serde_json::json!({
                    "clientId": cid.expose(),
                    "clientSecret": csec.expose(),
                    "expiresAt": client_expires_at.trim_end_matches('Z'),
                    "scopes": [
                        "codewhisperer:completions",
                        "codewhisperer:analysis",
                        "codewhisperer:conversations",
                        "codewhisperer:taskassist"
                    ]
                });
                if let Ok(reg_raw) = serde_json::to_string_pretty(&client_reg_data) {
                    let _ = std::fs::write(&client_reg_path, reg_raw);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if let Ok(meta) = std::fs::metadata(&client_reg_path) {
                            let mut perms = meta.permissions();
                            perms.set_mode(0o600);
                            let _ = std::fs::set_permissions(&client_reg_path, perms);
                        }
                    }
                }
            }
        }

        let kiro_running = is_kiro_running();

        Ok(KiroSwitchOutcome {
            success: true,
            account_id: id.to_string(),
            label: account.label(),
            kiro_running,
            message: if kiro_running {
                "已切换！检测到 Kiro 客户端正在运行，重载窗口或重启 Kiro 即可生效。".to_string()
            } else {
                "已成功切号！打开 Kiro 客户端将直接以此账号登录。".to_string()
            },
        })
    }

    /// 读取本地 SSO 缓存中当前生效的 Kiro 客户端登录态，并与本库账号比对匹配
    pub fn active_client(&self, home_dir: &Path) -> Result<Option<KiroActiveInfo>> {
        let token_path = home_dir
            .join(".aws")
            .join("sso")
            .join("cache")
            .join("kiro-auth-token.json");
        let Ok(raw) = std::fs::read_to_string(&token_path) else {
            return Ok(None);
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Ok(None);
        };

        let disk_rt = v.get("refreshToken").and_then(|x| x.as_str());
        let disk_at = v.get("accessToken").and_then(|x| x.as_str());
        let disk_provider = v
            .get("provider")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        let disk_auth_method = v
            .get("authMethod")
            .and_then(|x| x.as_str())
            .map(str::to_string);
        let disk_profile_arn = v
            .get("profileArn")
            .and_then(|x| x.as_str())
            .map(str::to_string);

        // 尝试从本地账号库中比对 refresh token 或 access token
        let accounts = self.repo.list()?;
        for acc in &accounts {
            if let Ok(Some(sec)) = self.repo.secret(&acc.id, KiroSecret::Refresh) {
                if Some(sec.expose()) == disk_rt {
                    return Ok(Some(KiroActiveInfo {
                        account_id: Some(acc.id.to_string()),
                        email: acc.email.clone(),
                        label: Some(acc.label()),
                        profile_arn: disk_profile_arn,
                        auth_method: disk_auth_method,
                        provider: disk_provider,
                    }));
                }
            }
            if let Ok(Some(sec)) = self.repo.secret(&acc.id, KiroSecret::Access) {
                if Some(sec.expose()) == disk_at {
                    return Ok(Some(KiroActiveInfo {
                        account_id: Some(acc.id.to_string()),
                        email: acc.email.clone(),
                        label: Some(acc.label()),
                        profile_arn: disk_profile_arn,
                        auth_method: disk_auth_method,
                        provider: disk_provider,
                    }));
                }
            }
        }

        Ok(Some(KiroActiveInfo {
            account_id: None,
            email: None,
            label: None,
            profile_arn: disk_profile_arn,
            auth_method: disk_auth_method,
            provider: disk_provider,
        }))
    }

    pub async fn access_token(&self, id: &KiroAccountId) -> Result<Secret> {
        if let Some(t) = self.fresh_access(id)? {
            return Ok(t);
        }
        let lock = self.refresh_lock(id);
        let _guard = lock.lock().await;
        if let Some(t) = self.fresh_access(id)? {
            return Ok(t);
        }
        self.refresh_now(id).await
    }

    fn refresh_lock(&self, id: &KiroAccountId) -> Arc<tokio::sync::Mutex<()>> {
        self.refresh_locks
            .lock()
            .expect("refresh locks")
            .entry(id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// 上游没到期就拒了 `rejected` 这把 access token：换一把。锁里发现别人已经换过，就用新的。
    /// 换不到时 `refresh_now` 自己会判要不要标成需要重新授权。
    async fn refresh_rejected(&self, id: &KiroAccountId, rejected: &Secret) -> Result<Secret> {
        let lock = self.refresh_lock(id);
        let _guard = lock.lock().await;
        if let Some(current) = self.repo.secret(id, KiroSecret::Access)? {
            if current.expose() != rejected.expose() {
                return Ok(current);
            }
        }
        self.refresh_now(id).await
    }

    fn fresh_access(&self, id: &KiroAccountId) -> Result<Option<Secret>> {
        let account = self.repo.get(id)?;
        let Some(access) = self.repo.secret(id, KiroSecret::Access)? else {
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

    async fn refresh_now(&self, id: &KiroAccountId) -> Result<Secret> {
        let refresh = self.repo.secret(id, KiroSecret::Refresh)?.ok_or_else(|| {
            let _ =
                self.repo
                    .record_failure(id, "没有 refresh token", Some(KiroStatus::NeedsLogin));
            AppError::new(
                ErrorCode::SecretMissing,
                "这个 Kiro 账号没有 refresh token。",
            )
            .with_hint("重新授权一次。")
        })?;
        let client_id = self.repo.secret(id, KiroSecret::ClientId)?;
        let client_secret = self.repo.secret(id, KiroSecret::ClientSecret)?;
        let result = match (client_id.as_ref(), client_secret.as_ref()) {
            (Some(cid), Some(csec)) => {
                self.tokens
                    .refresh_oidc(cid.expose(), csec.expose(), refresh.expose())
                    .await
            }
            _ => self.tokens.refresh_social(refresh.expose()).await,
        };
        match result {
            Ok(mut fresh) => {
                if fresh.refresh_token.is_none() {
                    fresh.refresh_token = Some(refresh);
                }
                if fresh.client_id.is_none() {
                    fresh.client_id = client_id;
                }
                if fresh.client_secret.is_none() {
                    fresh.client_secret = client_secret;
                }
                self.repo.store_tokens(id, &fresh)?;
                let identity = identity_from_tokens(&fresh, None);
                let _ = self.repo.update_identity(id, &identity);
                Ok(fresh.access_token)
            }
            Err(err) if err.code == ErrorCode::Unauthorized => {
                self.repo
                    .record_failure(id, &err.message, Some(KiroStatus::NeedsLogin))?;
                Err(err)
            }
            Err(err) => {
                self.repo.record_failure(id, &err.message, None)?;
                if let Some(access) = self.repo.secret(id, KiroSecret::Access)? {
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

    /// 问一次套餐和额度，写进账号快照。401 会把号标成需要重新授权。
    pub async fn refresh_usage(&self, id: &KiroAccountId) -> Result<KiroAccount> {
        let account = self.repo.get(id)?;
        let access = self.access_token(id).await?;
        let token_type = crate::protocol::token_type_header(account.auth_method.as_deref());
        let profile_arn = account.stored_profile_arn();
        let mut result = self
            .tokens
            .usage_limits(access.expose(), token_type, profile_arn.as_deref())
            .await;
        if account.has_refresh && matches!(&result, Err(err) if err.code == ErrorCode::Unauthorized)
        {
            // 存着的 access token 没到期就被拒了：换一把再问一次，别直接判成要重新授权。
            let fresh = self.refresh_rejected(id, &access).await?;
            result = self
                .tokens
                .usage_limits(fresh.expose(), token_type, profile_arn.as_deref())
                .await;
        }
        match result {
            Ok(usage) => {
                self.repo.save_usage(id, &usage)?;
                let resulting_id = self.rekey_to_user(&account, &usage)?;
                self.repo.get(&resulting_id)
            }
            Err(err) if err.code == ErrorCode::Unauthorized => {
                self.repo
                    .record_failure(id, &err.message, Some(KiroStatus::NeedsLogin))?;
                Err(err)
            }
            Err(err) => {
                self.repo.record_failure(id, &err.message, None)?;
                Err(err)
            }
        }
    }
}

pub fn is_kiro_running() -> bool {
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        std::process::Command::new("pgrep")
            .args(["-x", "Kiro"])
            .output()
            .map(|o| o.status.success() && !o.stdout.is_empty())
            .unwrap_or(false)
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq Kiro.exe", "/FO", "CSV", "/NH"])
            .output()
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .to_lowercase()
                    .contains("kiro.exe")
            })
            .unwrap_or(false)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_kiro_auth_token_json() {
        let json = r#"{
            "accessToken": "at_abcdefghijklmnopqrst",
            "refreshToken": "rt_abcdefghijklmnopqrst",
            "clientId": "cid-1",
            "clientSecret": "csec-1",
            "profileArn": "arn:aws:codewhisperer:us-east-1:1:profile/default",
            "authMethod": "IdC"
        }"#;
        let p = parse_import_text(json).unwrap();
        assert_eq!(p.client_id.as_deref(), Some("cid-1"));
        assert_eq!(
            p.profile_arn.as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:1:profile/default")
        );
    }

    #[test]
    fn parse_sub2api_account_export_and_skips_other_platforms() {
        let json = r#"{
            "accounts": [
                {
                    "platform": "openai",
                    "credentials": { "refresh_token": "rt_not_kiro_xxxxxxxxxxxx" }
                },
                {
                    "name": "desk",
                    "platform": "kiro",
                    "credentials": {
                        "access_token": "at_abcdefghijklmnopqrst",
                        "refresh_token": "rt_abcdefghijklmnopqrst",
                        "profile_arn": "arn:aws:codewhisperer:us-east-1:1:profile/desk",
                        "auth_method": "social"
                    }
                },
                {
                    "platform": "kiro",
                    "notes": "stringified",
                    "credentials": "{\"refresh_token\":\"rt_stringified_token_xx\",\"access_token\":\"at_stringified_token_xx\"}"
                }
            ]
        }"#;
        let all = parse_import_entries(json);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].note.as_deref(), Some("desk"));
        assert_eq!(all[0].auth_method.as_deref(), Some("social"));
        assert_eq!(
            all[0].profile_arn.as_deref(),
            Some("arn:aws:codewhisperer:us-east-1:1:profile/desk")
        );
        assert_eq!(all[1].note.as_deref(), Some("stringified"));
        assert_eq!(
            all[1].refresh_token.as_deref(),
            Some("rt_stringified_token_xx")
        );

        let flat = parse_import_text(
            r#"{"type":"kiro","access_token":"at_abcdefghijklmnopqrst","refresh_token":"rt_abcdefghijklmnopqrst"}"#,
        )
        .unwrap();
        assert!(flat.auth_method.is_none());
        assert_eq!(
            flat.refresh_token.as_deref(),
            Some("rt_abcdefghijklmnopqrst")
        );
    }

    #[test]
    fn canonical_account_absorbs_a_shared_arn_duplicate() {
        use nexus_store::MemorySecrets;

        let db = Arc::new(Db::open_in_memory().unwrap());
        let service = KiroService::new(db, Arc::new(MemorySecrets::new()));
        let token_set = |access: &str, refresh: &str| TokenSet {
            access_token: Secret::new(access),
            refresh_token: Some(Secret::new(refresh)),
            client_id: None,
            client_secret: None,
            expires_at: Some(OffsetDateTime::from_unix_timestamp(4_000_000_000).unwrap()),
        };
        let canonical = service
            .repo
            .upsert(
                &Identity {
                    subject: "user-123".into(),
                    email: Some("old@example.invalid".into()),
                    auth_method: Some("social".into()),
                    profile_arn: None,
                },
                &token_set("canonical-access", "canonical-refresh"),
                None,
            )
            .unwrap();
        let legacy = service
            .repo
            .upsert(
                &Identity {
                    subject: crate::protocol::SOCIAL_PROFILE_ARN.into(),
                    email: None,
                    auth_method: Some("social".into()),
                    profile_arn: Some(crate::protocol::SOCIAL_PROFILE_ARN.into()),
                },
                &token_set("legacy-access", "rotated-legacy-refresh"),
                None,
            )
            .unwrap();
        let usage = KiroUsage {
            subscription_title: Some("KIRO POWER".into()),
            email: Some("owner@example.invalid".into()),
            user_id: Some("user-123".into()),
            base_used: 25.0,
            base_limit: 100.0,
            used: 25.0,
            limit: 100.0,
            percent: 25.0,
            reset_at: None,
            trial_used: None,
            trial_limit: None,
            trial_expires_at: None,
            bonus_used: None,
            bonus_limit: None,
            overage_enabled: Some(false),
            checked_at: "2026-10-01T00:00:00Z".into(),
        };

        let resulting = service.rekey_to_user(&legacy.account, &usage).unwrap();
        assert_eq!(resulting, canonical.account.id);
        let rows = service.list().unwrap();
        assert_eq!(rows.len(), 1, "旧共享 ARN 行不能继续占号池一格");
        let account = &rows[0];
        assert_eq!(account.account_ref, "user-123");
        assert_eq!(account.email.as_deref(), Some("owner@example.invalid"));
        assert_eq!(account.plan_type.as_deref(), Some("KIRO POWER"));
        assert_eq!(
            account.stored_profile_arn().as_deref(),
            Some(crate::protocol::SOCIAL_PROFILE_ARN)
        );
        assert_eq!(
            service
                .repo
                .secret(&account.id, KiroSecret::Refresh)
                .unwrap()
                .unwrap()
                .expose(),
            "canonical-refresh",
            "合并不能拿旧行 refresh token 覆盖 canonical 凭证"
        );
        assert!(service
            .repo
            .secret(&legacy.account.id, KiroSecret::Access)
            .unwrap()
            .is_none());
    }
}
