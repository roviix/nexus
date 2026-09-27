//! 登录、导入、刷额度、给网关发 access token。

use crate::callback::CallbackServer;
use crate::model::{
    ClaudeAccount, ClaudeAuthMode, ClaudeStatus, ClientProbe, ImportReport, LoginStart, LoginState,
};
use crate::oauth::{self, TokenSet};
use crate::protocol::{self, ImportPiece, OAuthCode};
use crate::repo::{ClaudeAccounts, SavedOauth, SavedToken};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use nexus_core::{AppError, ClaudeAccountId, Result, Secret};
use nexus_store::{Db, SecretStore};
use rand::Rng;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

struct Pending {
    state: String,
    verifier: String,
    expires_at: i64,
    note: Option<String>,
    cancelled: Arc<AtomicBool>,
    server: Option<CallbackServer>,
    /// 授权码只能换一次。自动回调和手贴可能同时走到这里。
    used: Arc<AtomicBool>,
}

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(600);

pub struct ClaudeService {
    accounts: ClaudeAccounts,
    pending: Mutex<Option<Pending>>,
    refresh: tokio::sync::Mutex<()>,
}

impl ClaudeService {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            accounts: ClaudeAccounts::new(db, secrets),
            pending: Mutex::new(None),
            refresh: tokio::sync::Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<ClaudeAccount>> {
        self.accounts.list()
    }

    pub async fn start_login(&self, note: Option<String>) -> Result<LoginStart> {
        let verifier = random_token(32);
        let state = random_token(24);
        let authorize_url = protocol::authorize_url(&state, &protocol::code_challenge(&verifier));
        let server = match CallbackServer::bind().await {
            Ok(server) => Some(server),
            Err(err) => {
                tracing::info!(%err, "54545 端口绑不上，退回手贴回调地址");
                None
            }
        };
        let callback_listening = server.is_some();
        *self.pending.lock().expect("claude login") = Some(Pending {
            state: state.clone(),
            verifier,
            expires_at: unix_now() + 600,
            note,
            cancelled: Arc::new(AtomicBool::new(false)),
            server,
            used: Arc::new(AtomicBool::new(false)),
        });
        Ok(LoginStart {
            login_id: state,
            authorize_url,
            callback_listening,
        })
    }

    pub fn cancel_login(&self) {
        let pending = self.pending.lock().expect("claude login").take();
        if let Some(pending) = pending {
            pending.cancelled.store(true, Ordering::SeqCst);
        }
    }

    /// 等浏览器跳回 `localhost:54545/callback`，换票并落库。
    pub async fn wait_login(
        &self,
        session_id: &str,
        on_state: &(dyn Fn(LoginState) + Sync),
    ) -> Result<ClaudeAccount> {
        let (server, cancelled) = {
            let mut pending = self.pending.lock().expect("claude login");
            let Some(slot) = pending.as_mut() else {
                return Err(AppError::new(
                    nexus_core::ErrorCode::Cancelled,
                    "已取消这次授权。",
                ));
            };
            if slot.state != session_id {
                return Err(AppError::invalid("这次授权已过期或不存在，重新发起一次。"));
            }
            (slot.server.take(), slot.cancelled.clone())
        };
        let Some(server) = server else {
            return Err(AppError::invalid("这次授权没有在本机监听回调。")
                .with_hint("把浏览器地址栏的回调地址贴进来完成。"));
        };
        let sid = session_id.to_string();
        let started = std::time::Instant::now();
        on_state(LoginState::Waiting {
            session_id: sid.clone(),
            elapsed_secs: 0,
        });
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
                self.pending.lock().expect("claude login").take();
                if err.code == nexus_core::ErrorCode::Cancelled {
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
        match self
            .finish_code(
                protocol::OAuthCode {
                    code: received.code,
                    state: received.state,
                },
                None,
            )
            .await
        {
            Ok(account) => {
                on_state(LoginState::Succeeded {
                    session_id: sid,
                    account: account.clone(),
                    created: true,
                });
                Ok(account)
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

    pub async fn import_text(&self, text: &str, note: Option<&str>) -> Result<ImportReport> {
        let Some(piece) = protocol::classify_import(text) else {
            return Err(AppError::invalid("认不出这份 Claude 凭证。").with_hint(
                "贴授权页回调地址、~/.claude/.credentials.json，或 sk-ant- 开头的 setup-token / API Key。",
            ));
        };
        let account = match piece {
            ImportPiece::Code(code) => self.finish_code(code, note).await?,
            ImportPiece::Credentials(creds) => {
                let expires_at = creds.expires_at_ms.and_then(|ms| rfc3339_ms(ms));
                self.accounts.save_oauth(SavedOauth {
                    account_ref: creds
                        .account_uuid
                        .clone()
                        .unwrap_or_else(|| stable_ref("oauth", &creds.access_token)),
                    email: creds.email,
                    plan: creds.plan,
                    access_token: creds.access_token,
                    refresh_token: creds.refresh_token,
                    expires_at,
                    note: note.map(str::to_string),
                })?
            }
            ImportPiece::SetupToken(token) => self.accounts.save_static(SavedToken {
                account_ref: stable_ref("setup", &token),
                mode: ClaudeAuthMode::SetupToken,
                token,
                note: note.map(str::to_string),
            })?,
            ImportPiece::ApiKey(token) => self.accounts.save_static(SavedToken {
                account_ref: stable_ref("apikey", &token),
                mode: ClaudeAuthMode::ApiKey,
                token,
                note: note.map(str::to_string),
            })?,
        };
        Ok(ImportReport {
            accounts: vec![account],
            skipped: Vec::new(),
        })
    }

    pub fn probe_local(&self) -> ClientProbe {
        let path = credentials_path();
        let present = path.as_ref().is_some_and(|p| p.is_file());
        ClientProbe {
            present,
            path: path
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "~/.claude/.credentials.json".into()),
            also: (!present && cfg!(target_os = "macos")).then(|| KEYCHAIN_LABEL.into()),
        }
    }

    pub async fn import_local(&self, note: Option<&str>) -> Result<ImportReport> {
        let file = read_credentials_file();
        let keychain = if file.as_deref().is_some_and(has_oauth) {
            None
        } else {
            tokio::task::spawn_blocking(read_keychain)
                .await
                .ok()
                .flatten()
        };
        let Some(text) = prefer_local_blob(file, keychain) else {
            return Err(AppError::invalid("这台电脑上没找到 Claude Code 的登录态。").with_hint(
                "先在这台电脑上用 claude 登录一次，或改用「粘贴」。凭证在 ~/.claude/.credentials.json，macOS 上也可能在钥匙串「Claude Code-credentials」。",
            ));
        };
        self.import_text(&text, note).await
    }

    pub async fn refresh_quota(&self, id: &ClaudeAccountId) -> Result<ClaudeAccount> {
        let account = self.accounts.get(id)?;
        if account.auth_mode == ClaudeAuthMode::ApiKey {
            let _ = self.accounts.set_error(
                id,
                account.status,
                "API Key 不走订阅额度。用量在 Anthropic Console。",
            );
            return self.accounts.get(id);
        }
        let token = self.access_token(id).await?;
        match oauth::usage(token.expose()).await {
            Ok(quota) => self.accounts.set_usage(id, &quota),
            Err(err) => {
                let _ = self.accounts.set_error(id, account.status, &err.message);
                Err(err)
            }
        }
    }

    pub fn set_enabled(&self, id: &ClaudeAccountId, enabled: bool) -> Result<ClaudeAccount> {
        self.accounts.set_enabled(id, enabled)
    }

    pub fn set_note(&self, id: &ClaudeAccountId, note: Option<&str>) -> Result<ClaudeAccount> {
        self.accounts.set_note(id, note)
    }

    pub fn remove(&self, id: &ClaudeAccountId) -> Result<()> {
        self.accounts.remove(id)
    }

    pub fn auth_mode_of_label(&self, label: &str) -> Option<ClaudeAuthMode> {
        self.route_of_label(label).map(|(mode, _)| mode)
    }

    /// 网关出站要知道这是哪一种票、账号锚点是谁（写进 `metadata.user_id`）。
    pub fn route_of_label(&self, label: &str) -> Option<(ClaudeAuthMode, String)> {
        self.accounts
            .by_label(label)
            .ok()
            .flatten()
            .map(|a| (a.auth_mode, a.account_ref))
    }

    /// 网关每次拿号都走这里。快过期的 OAuth 会先刷新，刷新失败把号标成需要重新授权。
    pub async fn access_token(&self, id: &ClaudeAccountId) -> Result<Secret> {
        let account = self.accounts.get(id)?;
        if account.status == ClaudeStatus::Dead || !account.enabled {
            return Err(AppError::unauthorized("这个 Claude 账号停用了。"));
        }
        if account.auth_mode == ClaudeAuthMode::ApiKey {
            return self
                .accounts
                .api_key(id)?
                .ok_or_else(|| AppError::unauthorized("这把 API Key 不在了。"));
        }
        let expires = self.expiry_of(id)?;
        let stale = ClaudeAccounts::expires_soon(expires.as_deref(), unix_now());
        if account.auth_mode == ClaudeAuthMode::Oauth
            && (stale || self.accounts.access(id)?.is_none())
        {
            self.refresh_oauth(id).await?;
        }
        self.accounts.access(id)?.ok_or_else(|| {
            AppError::unauthorized("没有可用的 Claude 登录态。").with_hint("重新授权一次。")
        })
    }

    async fn finish_code(&self, code: OAuthCode, note: Option<&str>) -> Result<ClaudeAccount> {
        let (verifier, state, saved_note, used, expired) = {
            let pending = self.pending.lock().expect("claude login");
            let Some(pending) = pending.as_ref() else {
                return Err(AppError::invalid("还没有开始授权。")
                    .with_hint("先点「打开授权页」，登录后再把回调地址贴回来。"));
            };
            if let Some(got) = code.state.as_deref() {
                if got != pending.state {
                    return Err(AppError::invalid("授权回调的 state 对不上。")
                        .with_hint("重新打开授权页，不要混用上一次的链接。"));
                }
            }
            (
                pending.verifier.clone(),
                pending.state.clone(),
                pending.note.clone(),
                pending.used.clone(),
                pending.expires_at < unix_now(),
            )
        };
        if expired {
            self.pending.lock().expect("claude login").take();
            return Err(AppError::invalid("这次授权过期了。").with_hint("重新打开授权页。"));
        }
        if used.swap(true, Ordering::SeqCst) {
            return Err(AppError::invalid("这次授权已经用过了。"));
        }
        let tokens = oauth::exchange_code(&code.code, &verifier, &state).await?;
        self.pending.lock().expect("claude login").take();
        let note = note.map(str::to_string).or(saved_note);
        self.store_tokens(tokens, None, note.as_deref()).await
    }

    async fn refresh_oauth(&self, id: &ClaudeAccountId) -> Result<()> {
        let _guard = self.refresh.lock().await;
        let account = self.accounts.get(id)?;
        let expires = self.expiry_of(id)?;
        if !ClaudeAccounts::expires_soon(expires.as_deref(), unix_now()) {
            if self.accounts.access(id)?.is_some() {
                return Ok(());
            }
        }
        let refresh = self.accounts.refresh_token(id)?.ok_or_else(|| {
            AppError::unauthorized("这个号没有 refresh token，续不上。").with_hint("重新授权一次。")
        })?;
        match oauth::refresh(refresh.expose()).await {
            Ok(tokens) => {
                let _ = self.store_tokens(tokens, Some(id), None).await?;
                Ok(())
            }
            Err(err) => {
                let _ = self
                    .accounts
                    .set_error(id, ClaudeStatus::NeedsLogin, &err.message);
                let _ = account;
                Err(err)
            }
        }
    }

    async fn store_tokens(
        &self,
        tokens: TokenSet,
        existing: Option<&ClaudeAccountId>,
        note: Option<&str>,
    ) -> Result<ClaudeAccount> {
        let profile = oauth::profile(&tokens.access_token).await.ok();
        // 官方客户端换票后马上查角色。查不到不影响这次登录。
        if let Err(err) = oauth::roles(&tokens.access_token).await {
            tracing::warn!("读取 Claude CLI 角色失败：{err}");
        }
        let email = profile
            .as_ref()
            .and_then(protocol::email_from_profile)
            .or(tokens.email);
        let plan = profile.as_ref().and_then(protocol::plan_from_profile);
        let uuid = profile
            .as_ref()
            .and_then(protocol::uuid_from_profile)
            .or(tokens.account_uuid);
        let expires_at = rfc3339_in(tokens.expires_in);
        if let Some(id) = existing {
            let refresh = tokens.refresh_token.clone();
            self.accounts.store_access(
                id,
                &tokens.access_token,
                refresh.as_deref(),
                expires_at.as_deref(),
            )?;
            self.accounts
                .touch_profile(id, email.as_deref(), plan.as_deref())?;
            return self.accounts.get(id);
        }
        let account_ref = uuid.unwrap_or_else(|| stable_ref("oauth", &tokens.access_token));
        self.accounts.save_oauth(SavedOauth {
            account_ref,
            email,
            plan,
            access_token: tokens.access_token,
            refresh_token: tokens.refresh_token,
            expires_at,
            note: note.map(str::to_string),
        })
    }

    fn expiry_of(&self, id: &ClaudeAccountId) -> Result<Option<String>> {
        self.accounts.access_expires_at(id)
    }
}

fn random_token(n: usize) -> String {
    let mut raw = vec![0u8; n];
    rand::rng().fill_bytes(&mut raw);
    URL_SAFE_NO_PAD.encode(raw)
}

fn stable_ref(kind: &str, token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    format!("{kind}:{}", hex_prefix(&digest, 8))
}

fn hex_prefix(bytes: &[u8], n: usize) -> String {
    bytes.iter().take(n).map(|b| format!("{b:02x}")).collect()
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn rfc3339_in(secs: i64) -> Option<String> {
    let dt = OffsetDateTime::from_unix_timestamp(unix_now() + secs).ok()?;
    dt.format(&Rfc3339).ok()
}

fn rfc3339_ms(ms: i64) -> Option<String> {
    let dt = OffsetDateTime::from_unix_timestamp(ms / 1000).ok()?;
    dt.format(&Rfc3339).ok()
}

fn credentials_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|h| !h.is_empty()))?;
    Some(
        PathBuf::from(home)
            .join(".claude")
            .join(".credentials.json"),
    )
}

const KEYCHAIN_LABEL: &str = "钥匙串 Claude Code-credentials";
#[cfg(target_os = "macos")]
const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

fn read_credentials_file() -> Option<String> {
    let path = credentials_path()?;
    std::fs::read_to_string(path).ok()
}

fn has_oauth(text: &str) -> bool {
    matches!(
        protocol::classify_import(text),
        Some(protocol::ImportPiece::Credentials(_))
    )
}

/// 文件里已经是一份 OAuth 就用文件，免得再去碰钥匙串。
/// 两边都有时用过期更晚的那份。都认不出就把原文交回去，让导入去说该贴什么。
fn prefer_local_blob(file: Option<String>, keychain: Option<String>) -> Option<String> {
    match (file, keychain) {
        (Some(file), Some(keychain)) if has_oauth(&file) && has_oauth(&keychain) => {
            let file_exp = oauth_expiry(&file);
            let key_exp = oauth_expiry(&keychain);
            if key_exp > file_exp {
                Some(keychain)
            } else {
                Some(file)
            }
        }
        (Some(file), _) if has_oauth(&file) => Some(file),
        (_, Some(keychain)) if has_oauth(&keychain) => Some(keychain),
        (Some(file), keychain) => Some(file).or(keychain),
        (None, keychain) => keychain,
    }
}

fn oauth_expiry(text: &str) -> i64 {
    match protocol::classify_import(text) {
        Some(protocol::ImportPiece::Credentials(creds)) => creds.expires_at_ms.unwrap_or(0),
        _ => 0,
    }
}

/// macOS 上 Claude Code 把 OAuth 放在登录钥匙串，服务名 `Claude Code-credentials`。
/// 只在用户点「从本机导入」时读。文件里已经有 token 就不会走到这里。
fn read_keychain() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let user = std::env::var("USER").ok();
        if let Some(user) = user.as_deref() {
            if let Some(text) = security_password(&["-a", user]) {
                return Some(text);
            }
        }
        return security_password(&[]);
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn security_password(extra: &[&str]) -> Option<String> {
    let mut args = vec!["find-generic-password", "-s", KEYCHAIN_SERVICE, "-w"];
    args.extend_from_slice(extra);
    let out = std::process::Command::new("/usr/bin/security")
        .args(&args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

#[cfg(test)]
mod local_import_tests {
    use super::*;

    fn blob(token: &str, expires: i64) -> String {
        format!(
            r#"{{"claudeAiOauth":{{"accessToken":"{token}","refreshToken":"rt","expiresAt":{expires}}}}}"#
        )
    }

    #[test]
    fn a_usable_file_wins_over_an_older_keychain_entry() {
        let file = blob("file-token", 200);
        let keychain = blob("key-token", 100);
        let chosen = prefer_local_blob(Some(file.clone()), Some(keychain)).unwrap();
        assert!(chosen.contains("file-token"));
    }

    #[test]
    fn a_newer_keychain_entry_wins_when_both_are_oauth() {
        let file = blob("file-token", 100);
        let keychain = blob("key-token", 900);
        let chosen = prefer_local_blob(Some(file), Some(keychain)).unwrap();
        assert!(chosen.contains("key-token"));
    }

    #[test]
    fn keychain_fills_in_when_the_file_has_no_token() {
        let chosen = prefer_local_blob(Some("{}".into()), Some(blob("key-token", 1))).unwrap();
        assert!(chosen.contains("key-token"));
    }
}
