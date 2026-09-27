//! Kiro 账号用例：Builder ID device code、导入、续期。

use crate::model::{KiroAccount, KiroStatus};
use crate::oauth::{
    identity_from_tokens, jwt_expiry, looks_like_jwt, DeviceCode, RegisteredClient, TokenClient,
    TokenSet,
};
use crate::protocol::{KIRO_MODELS, REFRESH_AHEAD};
use crate::repo::{KiroAccounts, Upserted};
use nexus_core::{AppError, ErrorCode, KiroAccountId, Result, Secret};
use nexus_store::{Db, KiroSecret, SecretStore};
use serde::Serialize;
use std::collections::HashMap;
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
}

pub struct KiroService {
    pub repo: KiroAccounts,
    tokens: TokenClient,
    logins: Mutex<HashMap<String, LoginSession>>,
    refresh_locks: Mutex<HashMap<KiroAccountId, Arc<tokio::sync::Mutex<()>>>>,
}

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
            refresh_locks: Mutex::new(HashMap::new()),
        }
    }

    pub fn models(&self) -> Vec<String> {
        KIRO_MODELS
            .iter()
            .map(|(ext, _)| (*ext).to_string())
            .collect()
    }

    /// 出站要用的机器码和社交登录的 profileArn。对不上账号时机器码按标签派生。
    pub fn outbound_for(&self, label: &str) -> KiroOutbound {
        let key = label.trim();
        let found = self.list().ok().and_then(|list| {
            list.into_iter()
                .find(|a| a.label().eq_ignore_ascii_case(key))
        });
        let Some(account) = found else {
            return KiroOutbound {
                machine_id: crate::protocol::machine_id(None, key),
                profile_arn: None,
            };
        };
        let refresh = self
            .repo
            .secret(&account.id, KiroSecret::Refresh)
            .ok()
            .flatten();
        let machine_id =
            crate::protocol::machine_id(refresh.as_ref().map(|s| s.expose()), &account.label());
        let builder = account
            .auth_method
            .as_deref()
            .is_some_and(|m| m.eq_ignore_ascii_case("builder-id"));
        let profile_arn = if builder {
            None
        } else {
            account
                .profile_arn
                .clone()
                .filter(|s| s.starts_with("arn:"))
                .or_else(|| {
                    account
                        .account_ref
                        .starts_with("arn:")
                        .then(|| account.account_ref.clone())
                })
        };
        KiroOutbound {
            machine_id,
            profile_arn,
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
                    match self.add_tokens(tokens, note.as_deref(), Some("builder-id"), None) {
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

    pub fn add_tokens(
        &self,
        tokens: TokenSet,
        note: Option<&str>,
        auth_method: Option<&str>,
        fallback_ref: Option<&str>,
    ) -> Result<Upserted> {
        let mut identity = identity_from_tokens(&tokens, fallback_ref);
        if identity.auth_method.is_none() {
            identity.auth_method = auth_method.map(str::to_string);
        }
        if identity.subject.is_empty() {
            return Err(AppError::invalid("这组 token 读不出稳定身份。"));
        }
        self.repo.upsert(&identity, &tokens, note)
    }

    pub async fn import_text(&self, text: &str, note: Option<&str>) -> Result<Upserted> {
        let imported = parse_import_text(text).ok_or_else(|| import_unrecognized())?;
        self.import_one(imported, note).await
    }

    /// 一次贴多个号。能进的先进，一条坏的不把整份退掉。
    pub async fn import_dump(&self, text: &str, note: Option<&str>) -> Result<ImportReport> {
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
        self.add_tokens(tokens, note, method, imported.profile_arn.as_deref())
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

    pub async fn access_token(&self, id: &KiroAccountId) -> Result<Secret> {
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
}
