//! Google 反重力 (Antigravity) 核心业务服务。
//!
//! 负责：
//! 1. 启动本地 OAuth 监听与会话生命周期管理；
//! 2. Token 刷新锁（Single-flight Mutex，杜绝开源反代并发踩踏导致的 Token 作废问题）；
//! 3. 访问 Google `loadCodeAssist` 自动探测绑定的 GCP Project ID；
//! 4. 文本粘贴与本机 CLI 配置导入解析；
//! 5. 为网关提供带有到期自愈能力的有效 Access Token。

use crate::callback::CallbackServer;
use crate::model::{
    normalize_subscription_tier, tier_display_name, AntigravityAccount, AntigravityStatus,
    AntigravityUsage, ClientProbe, LoginStart, LoginState, ModelQuotaBucket, QuotaGroup,
    QuotaWindow,
};
use crate::oauth::{self, TokenSet};
use crate::protocol::{
    should_try_next_endpoint, upstream_user_agent, v1_url, CONTENT_ORIGINS,
    RPC_FETCH_AVAILABLE_MODELS, RPC_LOAD_CODE_ASSIST, RPC_RETRIEVE_USER_QUOTA,
};
use crate::repo::AntigravityAccounts;
use base64::Engine;
use nexus_core::{AntigravityAccountId, AppError, ErrorCode, Result, Secret};
use nexus_store::{Db, SecretStore};
use rand::RngExt;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// 提前 5 分钟换票，避免请求在中途遇到 401。
pub const REFRESH_AHEAD: Duration = Duration::from_secs(300);

struct Pending {
    state: String,
    verifier: String,
    #[allow(dead_code)]
    expires_at: i64,
    note: Option<String>,
    cancelled: Arc<AtomicBool>,
    server: Option<CallbackServer>,
    used: Arc<AtomicBool>,
}

const CALLBACK_TIMEOUT: Duration = Duration::from_secs(600);

pub struct AntigravityService {
    accounts: AntigravityAccounts,
    pending: Mutex<Option<Pending>>,
    refresh_lock: tokio::sync::Mutex<()>,
}

impl AntigravityService {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            accounts: AntigravityAccounts::new(db, secrets),
            pending: Mutex::new(None),
            refresh_lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn list(&self) -> Result<Vec<AntigravityAccount>> {
        self.accounts.list()
    }

    pub fn get(&self, id: &AntigravityAccountId) -> Result<Option<AntigravityAccount>> {
        self.accounts.get(id)
    }

    pub async fn start_login(&self, note: Option<String>) -> Result<LoginStart> {
        let verifier = random_token(32);
        let state = random_token(24);
        let challenge = oauth::code_challenge(&verifier);
        let authorize_url = oauth::authorize_url(&state, &challenge);

        let server = match CallbackServer::bind().await {
            Ok(server) => Some(server),
            Err(err) => {
                tracing::info!(%err, "51121 端口绑定失败，降级为手动粘贴授权回调地址");
                None
            }
        };
        let callback_listening = server.is_some();
        *self.pending.lock().expect("antigravity login lock") = Some(Pending {
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
        let pending = self.pending.lock().expect("antigravity login lock").take();
        if let Some(p) = pending {
            p.cancelled.store(true, Ordering::SeqCst);
        }
    }

    pub async fn wait_login(
        &self,
        session_id: &str,
        on_state: &(dyn Fn(LoginState) + Sync),
    ) -> Result<AntigravityAccount> {
        let (server, cancelled) = {
            let mut pending = self.pending.lock().expect("antigravity login lock");
            let Some(slot) = pending.as_mut() else {
                return Err(AppError::new(
                    ErrorCode::AccountNotFound,
                    "没有进行中的登录流程",
                ));
            };
            if slot.state != session_id {
                return Err(AppError::new(ErrorCode::InvalidInput, "会话 ID 不匹配"));
            }
            (slot.server.take(), Arc::clone(&slot.cancelled))
        };

        let Some(server) = server else {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "本地回调端口未在监听，请在输入框中手动粘贴跳转后的地址栏内容",
            ));
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

        let is_cancelled = move || cancelled.load(Ordering::SeqCst);
        let received = tokio::select! {
            r = server.wait(session_id, CALLBACK_TIMEOUT, &is_cancelled) => r,
            _ = progress => unreachable!("进度循环不会自己结束"),
        };

        let received = match received {
            Ok(r) => r,
            Err(err) => {
                self.pending.lock().expect("antigravity login lock").take();
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

        match self.finish_code_exchange(session_id, &received.code).await {
            Ok(account) => {
                on_state(LoginState::Succeeded {
                    session_id: sid,
                    account: Box::new(account.clone()),
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

    /// 完成授权码到 Token 的兑换，并探测 Google Cloud Project ID。
    pub async fn finish_code_exchange(
        &self,
        session_id: &str,
        code: &str,
    ) -> Result<AntigravityAccount> {
        let (verifier, note) = {
            let mut pending = self.pending.lock().expect("antigravity login lock");
            let Some(slot) = pending.as_mut() else {
                return Err(AppError::new(
                    ErrorCode::AccountNotFound,
                    "没有进行中的登录流程",
                ));
            };
            if slot.state != session_id {
                return Err(AppError::new(ErrorCode::InvalidInput, "会话 ID 不匹配"));
            }
            if slot.used.swap(true, Ordering::SeqCst) {
                return Err(AppError::new(
                    ErrorCode::Busy,
                    "此授权码已被换取，请重新点击登录",
                ));
            }
            (slot.verifier.clone(), slot.note.clone())
        };

        let tokens = oauth::exchange_code(code, &verifier, oauth::DEFAULT_REDIRECT_URI).await?;
        let email = tokens
            .email
            .clone()
            .unwrap_or_else(|| "antigravity_user".to_string());

        let account = self.accounts.upsert_oauth(&email, &tokens, note)?;

        // 尝试探测 Project ID 与账号配额及 Tier
        if let Ok((project_id, tier, usage)) = self.probe_metadata(&tokens.access_token).await {
            if let Some(u) = usage.as_ref() {
                let _ = self.accounts.set_usage(&account.id, u, tier.as_deref());
            } else {
                let _ = self.accounts.update_metadata(
                    &account.id,
                    tier.as_deref(),
                    project_id.as_deref(),
                );
            }
            if let Ok(Some(updated)) = self.accounts.get(&account.id) {
                return Ok(updated);
            }
        }

        Ok(account)
    }

    /// 解析用户手动粘贴的内容（支持整段回调 URL、Token JSON、或者 Refresh Token 字符串）。
    pub async fn import_text(
        &self,
        text: &str,
        note: Option<String>,
    ) -> Result<AntigravityAccount> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(AppError::new(ErrorCode::InvalidInput, "粘贴内容为空"));
        }

        // 0. 支持 go-keyring-base64 编码的凭证字符串（macOS Keychain / 官方 Antigravity 格式）
        let decoded_storage;
        let candidate_text = if let Some(b64) = trimmed.strip_prefix("go-keyring-base64:") {
            if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) {
                if let Ok(s) = String::from_utf8(bytes) {
                    decoded_storage = s;
                    decoded_storage.trim()
                } else {
                    trimmed
                }
            } else {
                trimmed
            }
        } else {
            trimmed
        };

        // 1. 如果贴的是回调 URL（例如 http://localhost:51121/oauth-callback?code=...）
        if candidate_text.contains("code=") {
            let query = candidate_text
                .split_once('?')
                .map(|(_, q)| q)
                .unwrap_or(candidate_text);
            let mut code = None;
            for pair in query.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    if k == "code" {
                        code = Some(crate::callback::percent_decode(v));
                        break;
                    }
                }
            }
            if let Some(code) = code {
                // 如果当前正好有 pending 会话，则走带 verifier 的兑换流程
                let pending_info = {
                    let pending = self.pending.lock().expect("antigravity login lock");
                    pending
                        .as_ref()
                        .map(|p| (p.state.clone(), p.verifier.clone(), p.note.clone()))
                };
                if let Some((session_id, _verifier, _p_note)) = pending_info {
                    return self.finish_code_exchange(&session_id, &code).await;
                } else {
                    return Err(AppError::new(
                        ErrorCode::InvalidInput,
                        "请先点击「登录授权」打开浏览器，再粘贴跳转后的地址。",
                    ));
                }
            }
        }

        // 2. 如果贴的是 JSON（例如 CLIProxyAPI、opencode、或 Antigravity 本机 Keychain 保存的凭证）
        if candidate_text.starts_with('{') && candidate_text.ends_with('}') {
            if let Ok(val) = serde_json::from_str::<Value>(candidate_text) {
                let token_node = val.get("token");
                let refresh_token = val
                    .get("refresh_token")
                    .or_else(|| val.get("refreshToken"))
                    .or_else(|| token_node.and_then(|t| t.get("refresh_token")))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let access_token = val
                    .get("access_token")
                    .or_else(|| val.get("accessToken"))
                    .or_else(|| token_node.and_then(|t| t.get("access_token")))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let id_token = val
                    .get("id_token")
                    .or_else(|| token_node.and_then(|t| t.get("id_token")))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                let email = val
                    .get("email")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| {
                        val.get("account")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                    })
                    .or_else(|| id_token.as_deref().and_then(oauth::email_from_jwt));

                if let Some(rt) = refresh_token {
                    let tokens = oauth::refresh(&rt).await?;
                    let em = email
                        .or(tokens.email.clone())
                        .unwrap_or_else(|| "antigravity_user".to_string());
                    let account = self.accounts.upsert_oauth(&em, &tokens, note)?;
                    if let Ok((pid, tier, usage)) = self.probe_metadata(&tokens.access_token).await
                    {
                        if let Some(u) = usage.as_ref() {
                            let _ = self.accounts.set_usage(&account.id, u, tier.as_deref());
                        } else {
                            let _ = self.accounts.update_metadata(
                                &account.id,
                                tier.as_deref(),
                                pid.as_deref(),
                            );
                        }
                    }
                    return self
                        .accounts
                        .get(&account.id)?
                        .ok_or_else(|| AppError::new(ErrorCode::Database, "读取刚写入的账号失败"));
                } else if let Some(at) = access_token {
                    let em = email
                        .or_else(|| oauth::email_from_jwt(&at))
                        .unwrap_or_else(|| "antigravity_token_user".to_string());
                    let tokens = TokenSet {
                        access_token: at.clone(),
                        refresh_token: None,
                        expires_in: 3600,
                        id_token,
                        email: Some(em.clone()),
                    };
                    let account = self.accounts.upsert_oauth(&em, &tokens, note)?;
                    return Ok(account);
                }
            }
        }

        // 3. 如果包含以 "1//" 开头的 Google Refresh Token（支持直接粘贴、pbtxt、或环境变量）
        if let Some(idx) = candidate_text.find("1//") {
            let rest = &candidate_text[idx..];
            let token_end = rest
                .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == '}')
                .unwrap_or(rest.len());
            let rt = &rest[..token_end];
            if rt.len() > 10 {
                let tokens = oauth::refresh(rt).await?;
                let em = tokens
                    .email
                    .clone()
                    .unwrap_or_else(|| "antigravity_user".to_string());
                let account = self.accounts.upsert_oauth(&em, &tokens, note)?;
                if let Ok((pid, tier, usage)) = self.probe_metadata(&tokens.access_token).await {
                    if let Some(u) = usage.as_ref() {
                        let _ = self.accounts.set_usage(&account.id, u, tier.as_deref());
                    } else {
                        let _ = self.accounts.update_metadata(
                            &account.id,
                            tier.as_deref(),
                            pid.as_deref(),
                        );
                    }
                }
                return self
                    .accounts
                    .get(&account.id)?
                    .ok_or_else(|| AppError::new(ErrorCode::Database, "读取刚写入的账号失败"));
            }
        }

        Err(AppError::new(
            ErrorCode::InvalidInput,
            "无法识别粘贴的凭证格式，支持 Google 授权跳转 URL、包含 1// 的 Refresh Token、或包含 refresh_token 的 JSON。",
        ))
    }

    /// 从本机 Antigravity 配置或 macOS 钥匙串中直接导入账号。
    pub async fn import_local(&self, note: Option<String>) -> Result<AntigravityAccount> {
        #[cfg(target_os = "macos")]
        {
            let kc = tokio::task::spawn_blocking(read_keychain)
                .await
                .ok()
                .flatten();
            if let Some(text) = kc {
                if let Ok(account) = self.import_text(&text, note.clone()).await {
                    return Ok(account);
                }
            }
        }

        let home = std::env::var("HOME").unwrap_or_default();
        let path1 = PathBuf::from(&home).join(".gemini/antigravity/antigravity_state.pbtxt");
        let path2 = PathBuf::from(&home).join(".gemini/antigravity-cli/settings.json");

        let content = if path1.exists() {
            std::fs::read_to_string(&path1).ok()
        } else if path2.exists() {
            std::fs::read_to_string(&path2).ok()
        } else {
            None
        };

        if let Some(text) = content {
            if let Ok(account) = self.import_text(&text, note).await {
                return Ok(account);
            }
        }

        Err(AppError::new(
            ErrorCode::InvalidInput,
            "未在本机找到已登录的 Google Antigravity 凭证（检查 macOS 钥匙串或 ~/.gemini 配置）",
        ))
    }

    /// 探测本机是否存在 Google Antigravity 配置。
    pub async fn probe_local(&self) -> Result<ClientProbe> {
        #[cfg(target_os = "macos")]
        {
            let kc = tokio::task::spawn_blocking(read_keychain)
                .await
                .ok()
                .flatten();
            if let Some(text) = kc {
                if text.contains("1//") || text.contains("token") || text.contains("go-keyring") {
                    return Ok(ClientProbe {
                        present: true,
                        path: "macOS Keychain (gemini / antigravity)".to_string(),
                        also: None,
                    });
                }
            }
        }

        let home = std::env::var("HOME").unwrap_or_default();
        let path1 = PathBuf::from(&home).join(".gemini/antigravity/antigravity_state.pbtxt");
        let path2 = PathBuf::from(&home).join(".gemini/antigravity-cli/settings.json");

        if path1.exists() {
            Ok(ClientProbe {
                present: true,
                path: path1.display().to_string(),
                also: if path2.exists() {
                    Some(path2.display().to_string())
                } else {
                    None
                },
            })
        } else if path2.exists() {
            Ok(ClientProbe {
                present: true,
                path: path2.display().to_string(),
                also: None,
            })
        } else {
            Ok(ClientProbe {
                present: false,
                path: path1.display().to_string(),
                also: None,
            })
        }
    }

    /// 获取供网关调用的有效 Access Token。
    /// 包含单飞并发互斥锁与提前续期逻辑，确保生产环境高并发调用不被上游拒绝。
    pub async fn access_token(&self, id: &str) -> Result<Secret> {
        let account_id = AntigravityAccountId::from_raw(id);
        let account = self.accounts.get(&account_id)?.ok_or_else(|| {
            AppError::new(ErrorCode::AccountNotFound, "未找到该 Antigravity 账号")
        })?;

        if account.status == AntigravityStatus::Dead {
            return Err(AppError::new(
                ErrorCode::Unauthorized,
                "账号凭证已被 Google 判定失效，请重新登录",
            ));
        }

        // 检查 access_token 是否在有效期内（留出 5 分钟缓冲）
        let need_refresh = match account.access_expires_at.as_deref() {
            Some(exp) => {
                if let Ok(exp_time) = OffsetDateTime::parse(exp, &Rfc3339) {
                    let now = OffsetDateTime::now_utc();
                    exp_time - now <= time::Duration::seconds(REFRESH_AHEAD.as_secs() as i64)
                } else {
                    true
                }
            }
            None => true,
        };

        if !need_refresh {
            if let Some(access) = self.accounts.get_access_token(&account_id)? {
                return Ok(access);
            }
        }

        // 需要刷新：加锁以防并发踩踏 (Single-flight token refresh)
        let _guard = self.refresh_lock.lock().await;

        // 再次检查刷新后的状态
        let account = self
            .accounts
            .get(&account_id)?
            .ok_or_else(|| AppError::new(ErrorCode::AccountNotFound, "账号已被移除"))?;

        if let Some(exp) = account.access_expires_at.as_deref() {
            if let Ok(exp_time) = OffsetDateTime::parse(exp, &Rfc3339) {
                let now = OffsetDateTime::now_utc();
                if exp_time - now > time::Duration::seconds(REFRESH_AHEAD.as_secs() as i64) {
                    if let Some(access) = self.accounts.get_access_token(&account_id)? {
                        return Ok(access);
                    }
                }
            }
        }

        let Some(refresh_secret) = self.accounts.get_refresh_token(&account_id)? else {
            // 没有 refresh token
            if let Some(access) = self.accounts.get_access_token(&account_id)? {
                return Ok(access);
            }
            return Err(AppError::new(
                ErrorCode::SecretMissing,
                "该账号缺少 Refresh Token，无法自动续期，请重新登录",
            ));
        };

        let rt = refresh_secret.expose();
        match oauth::refresh(rt).await {
            Ok(tokens) => {
                let expires_at = OffsetDateTime::now_utc()
                    .checked_add(time::Duration::seconds(tokens.expires_in))
                    .and_then(|t| t.format(&Rfc3339).ok());

                self.accounts.save_tokens(
                    &account_id,
                    &tokens.access_token,
                    tokens.refresh_token.as_deref(),
                    expires_at.as_deref(),
                )?;
                self.accounts
                    .set_status(&account_id, AntigravityStatus::Active, None)?;

                Ok(Secret::new(&tokens.access_token))
            }
            Err(err) => {
                let is_invalid_grant =
                    err.message.contains("invalid_grant") || err.code == ErrorCode::Unauthorized;
                if is_invalid_grant {
                    let _ = self.accounts.set_status(
                        &account_id,
                        AntigravityStatus::NeedsLogin,
                        Some(err.message.clone()),
                    );
                    return Err(err);
                }
                // 暂时续不上（网络 / Google 抖动）：提前 5 分钟换票，旧 token 多半还没真过期，先顶着用。
                let still_valid = account
                    .access_expires_at
                    .as_deref()
                    .and_then(|exp| OffsetDateTime::parse(exp, &Rfc3339).ok())
                    .is_some_and(|exp| {
                        exp > OffsetDateTime::now_utc() + time::Duration::seconds(60)
                    });
                if still_valid {
                    if let Some(access) = self.accounts.get_access_token(&account_id)? {
                        tracing::warn!(%err, "Antigravity 续期暂时失败，先用还没过期的旧 access token");
                        return Ok(access);
                    }
                }
                Err(err)
            }
        }
    }

    /// 探测 GCP 项目、订阅档位，以及 5 小时 / 周配额窗。
    ///
    /// `loadCodeAssist` 是档位的唯一权威来源。`fetchAvailableModels` 给出模型目录，
    /// 但它的 remainingFraction 对免费号和付费号经常都是 1，不能拿来判断到线。
    /// 真正的 5h / weekly 在 `retrieveUserQuotaSummary`。
    pub async fn probe_metadata(
        &self,
        access_token: &str,
    ) -> Result<(Option<String>, Option<String>, Option<AntigravityUsage>)> {
        let client = crate::protocol::http_client();
        let load = post_cloud(
            &client,
            access_token,
            RPC_LOAD_CODE_ASSIST,
            &json!({ "metadata": { "ideType": "ANTIGRAVITY" } }),
            false,
        )
        .await;
        if matches!(load, CloudReply::Unauthorized) {
            return Err(AppError::new(
                ErrorCode::Unauthorized,
                "Google Antigravity 登录已失效，请重新授权",
            ));
        }

        let load_body = match &load {
            CloudReply::Ok(value) => value.clone(),
            _ => Value::Null,
        };
        let project_id = project_id_from_load(&load_body);
        let (tier, tier_name, tier_desc) = tier_from_load(&load_body);

        let quota_body = match project_id.as_deref() {
            Some(project) => json!({ "project": project }),
            None => json!({}),
        };
        let models = post_cloud(
            &client,
            access_token,
            RPC_FETCH_AVAILABLE_MODELS,
            &quota_body,
            true,
        )
        .await;
        if matches!(models, CloudReply::Unauthorized) {
            return Err(AppError::new(
                ErrorCode::Unauthorized,
                "Google Antigravity 登录已失效，请重新授权",
            ));
        }
        let summary = post_cloud(
            &client,
            access_token,
            RPC_RETRIEVE_USER_QUOTA,
            &quota_body,
            true,
        )
        .await;

        let forbidden =
            matches!(load, CloudReply::Forbidden) || matches!(models, CloudReply::Forbidden);
        let model_buckets = match &models {
            CloudReply::Ok(value) => model_buckets_from_catalog(value),
            _ => Vec::new(),
        };
        let quota_groups = match &summary {
            CloudReply::Ok(value) => quota_groups_from_summary(value),
            _ => Vec::new(),
        };

        let five_hour = quota_groups
            .iter()
            .flat_map(|group| group.buckets.iter())
            .filter(|bucket| {
                let window = bucket.window.to_ascii_lowercase();
                window.contains("5h") || window.contains("5-h")
            })
            .map(|bucket| bucket.percent_remaining)
            .min();
        let resets_at = quota_groups
            .iter()
            .flat_map(|group| group.buckets.iter())
            .find_map(|bucket| bucket.reset_time.clone());
        let percent_remaining = five_hour.map(|p| p as f64);
        let percent_used = five_hour.map(|p| (100 - p) as f64);
        let now_iso = OffsetDateTime::now_utc().format(&Rfc3339).ok();
        let last_status = if forbidden {
            "forbidden"
        } else if matches!(load, CloudReply::Ok(_))
            || !model_buckets.is_empty()
            || !quota_groups.is_empty()
        {
            "OK"
        } else {
            "degraded"
        };

        let usage = AntigravityUsage {
            project_id: project_id.clone(),
            tier: tier.clone(),
            tier_name,
            tier_description: tier_desc,
            percent_used,
            percent_remaining,
            remaining_requests: None,
            resets_at,
            model_buckets,
            quota_groups,
            last_status: Some(last_status.to_string()),
            checked_at: now_iso,
        };
        Ok((project_id, tier, Some(usage)))
    }

    pub async fn probe_project_id(&self, access_token: &str) -> Result<Option<String>> {
        let (project_id, _, _) = self.probe_metadata(access_token).await?;
        Ok(project_id)
    }

    /// 刷新账号健康状态与配额元数据。
    pub async fn refresh_quota(&self, id: &str) -> Result<AntigravityAccount> {
        let aid = AntigravityAccountId::from_raw(id);
        let token = self.access_token(id).await?;
        match self.probe_metadata(token.expose()).await {
            Ok((project_id, tier, usage)) => {
                if let Some(u) = usage.as_ref() {
                    let _ = self.accounts.set_usage(&aid, u, tier.as_deref());
                }
                let _ = self
                    .accounts
                    .update_metadata(&aid, tier.as_deref(), project_id.as_deref());
                let _ = self
                    .accounts
                    .set_status(&aid, AntigravityStatus::Active, None);
            }
            Err(err) => {
                let is_auth_error = err.code == ErrorCode::Unauthorized
                    || err.message.contains("invalid_grant")
                    || err.message.contains("401");
                if is_auth_error {
                    let _ = self.accounts.set_status(
                        &aid,
                        AntigravityStatus::NeedsLogin,
                        Some(err.message.clone()),
                    );
                    return Err(err);
                } else {
                    tracing::warn!(%err, "刷新 Antigravity 配额遇到暂态异常，保持活跃状态");
                    let _ = self
                        .accounts
                        .set_status(&aid, AntigravityStatus::Active, None);
                }
            }
        }

        // 尝试自动修复 has_refresh 缺失的问题（针对早期版本导入但缺少 has_refresh 标记的账号）
        #[cfg(target_os = "macos")]
        {
            if let Ok(Some(acc)) = self.accounts.get(&aid) {
                if !acc.has_refresh {
                    if let Some(text) = tokio::task::spawn_blocking(read_keychain)
                        .await
                        .ok()
                        .flatten()
                    {
                        let trimmed = text.trim();
                        let decoded_str;
                        let cand = if let Some(b64) = trimmed.strip_prefix("go-keyring-base64:") {
                            if let Ok(bytes) =
                                base64::engine::general_purpose::STANDARD.decode(b64.trim())
                            {
                                if let Ok(s) = String::from_utf8(bytes) {
                                    decoded_str = s;
                                    decoded_str.trim()
                                } else {
                                    trimmed
                                }
                            } else {
                                trimmed
                            }
                        } else {
                            trimmed
                        };
                        if let Ok(val) = serde_json::from_str::<Value>(cand) {
                            let field = |names: &[&str]| -> Option<String> {
                                names.iter().find_map(|n| {
                                    val.get(*n)
                                        .or_else(|| val.get("token").and_then(|t| t.get(*n)))
                                        .and_then(|v| v.as_str())
                                        .map(str::to_string)
                                })
                            };
                            let rt = field(&["refresh_token", "refreshToken"]);
                            // 钥匙串里是本机官方客户端此刻登着的那个人。只有确认是同一个号（同一把
                            // access token，或同一个邮箱）才补，否则会把别人的 refresh token 挂到
                            // 这个号上，之后它的请求全以另一个人的身份发出去。
                            let same_session = field(&["access_token", "accessToken"])
                                .is_some_and(|at| at == token.expose());
                            let same_email = field(&["email"]).is_some_and(|e| {
                                acc.email
                                    .as_deref()
                                    .is_some_and(|mine| mine.eq_ignore_ascii_case(e.trim()))
                            });
                            if !(same_session || same_email) {
                                tracing::info!(
                                    "本机钥匙串里的 Antigravity 登录不是这个号，不补 refresh token"
                                );
                            }
                            if let Some(rt_str) =
                                rt.as_deref().filter(|_| same_session || same_email)
                            {
                                let _ = self.accounts.save_tokens(
                                    &aid,
                                    token.expose(),
                                    Some(rt_str),
                                    acc.access_expires_at.as_deref(),
                                );
                            }
                        }
                    }
                }
            }
        }

        self.accounts
            .get(&aid)?
            .ok_or_else(|| AppError::new(ErrorCode::AccountNotFound, "未找到该 Antigravity 账号"))
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<AntigravityAccount> {
        let aid = AntigravityAccountId::from_raw(id);
        self.accounts.set_enabled(&aid, enabled)
    }

    pub fn set_note(&self, id: &str, note: Option<String>) -> Result<AntigravityAccount> {
        let aid = AntigravityAccountId::from_raw(id);
        self.accounts.set_note(&aid, note)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let aid = AntigravityAccountId::from_raw(id);
        self.accounts.remove(&aid)
    }
}

fn random_token(len: usize) -> String {
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..len).map(|_| rng.random()).collect();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(target_os = "macos")]
fn read_keychain() -> Option<String> {
    let out = std::process::Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-s",
            "gemini",
            "-a",
            "antigravity",
            "-w",
        ])
        .output()
        .ok()?;
    if out.status.success() {
        let pass = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !pass.is_empty() {
            return Some(pass);
        }
    }
    let out2 = std::process::Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-s",
            "Antigravity IDE Safe Storage",
            "-a",
            "antigravity",
            "-w",
        ])
        .output()
        .ok()?;
    if out2.status.success() {
        let pass = String::from_utf8_lossy(&out2.stdout).trim().to_string();
        if !pass.is_empty() {
            return Some(pass);
        }
    }
    None
}

enum CloudReply {
    Ok(Value),
    Unauthorized,
    Forbidden,
    Failed,
}

async fn post_cloud(
    client: &reqwest::Client,
    access_token: &str,
    rpc: &str,
    body: &Value,
    fallback_on_429: bool,
) -> CloudReply {
    for (idx, origin) in CONTENT_ORIGINS.iter().enumerate() {
        let url = v1_url(origin, rpc);
        let has_next = idx + 1 < CONTENT_ORIGINS.len();
        let sent = client
            .post(&url)
            .bearer_auth(access_token)
            .header("Content-Type", "application/json")
            .header("User-Agent", upstream_user_agent())
            .json(body)
            .send()
            .await;
        let resp = match sent {
            Ok(resp) => resp,
            Err(err) => {
                tracing::warn!(%err, %url, "Antigravity 元数据请求失败");
                if has_next {
                    continue;
                }
                return CloudReply::Failed;
            }
        };
        let status = resp.status();
        if status.is_success() {
            return CloudReply::Ok(resp.json().await.unwrap_or(Value::Null));
        }
        let code = status.as_u16();
        let text = resp.text().await.unwrap_or_default();
        tracing::warn!(%status, %url, %text, "Antigravity 元数据接口非成功");
        if code == 401 || text.contains("invalid_grant") || text.contains("ACCESS_TOKEN_EXPIRED") {
            return CloudReply::Unauthorized;
        }
        if code == 403 {
            return CloudReply::Forbidden;
        }
        let retry =
            has_next && (should_try_next_endpoint(code) || (fallback_on_429 && code == 429));
        if retry {
            continue;
        }
        return CloudReply::Failed;
    }
    CloudReply::Failed
}

fn project_id_from_load(val: &Value) -> Option<String> {
    let raw = val
        .get("cloudaicompanionProject")
        .or_else(|| val.get("cloudAiCompanionProject"))
        .and_then(|v| {
            v.as_str()
                .map(|s| s.to_string())
                .or_else(|| v.get("id").and_then(|i| i.as_str()).map(|s| s.to_string()))
        })
        .or_else(|| {
            val.get("projectId")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(
            trimmed
                .strip_prefix("projects/")
                .unwrap_or(trimmed)
                .to_string(),
        )
    }
}

fn tier_from_load(val: &Value) -> (Option<String>, Option<String>, Option<String>) {
    let raw = val
        .get("paidTier")
        .and_then(tier_token)
        .or_else(|| val.get("currentTier").and_then(tier_token));
    let Some(raw) = raw else {
        return (None, None, None);
    };
    let normalized = normalize_subscription_tier(&raw);
    let known = matches!(normalized.as_str(), "FREE" | "PRO" | "ULTRA");
    let tier = if known {
        Some(normalized.clone())
    } else if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    };
    let display = tier.as_deref().map(tier_display_name);
    let desc = val
        .get("paidTier")
        .and_then(|tier| tier.get("description"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    (tier, display, desc)
}

fn tier_token(tier: &Value) -> Option<String> {
    tier.get("id")
        .or_else(|| tier.get("name"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.trim().is_empty())
}

fn model_buckets_from_catalog(val: &Value) -> Vec<ModelQuotaBucket> {
    let Some(models) = val.get("models").and_then(|m| m.as_object()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, info) in models {
        let keep = name.starts_with("gemini")
            || name.starts_with("claude")
            || name.starts_with("gpt")
            || name.starts_with("image")
            || name.starts_with("imagen");
        if !keep {
            continue;
        }
        let quota = info.get("quotaInfo");
        let remaining_fraction = quota
            .and_then(|q| q.get("remainingFraction"))
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0);
        let percent_remaining = (remaining_fraction.clamp(0.0, 1.0) * 100.0).round() as u32;
        out.push(ModelQuotaBucket {
            model_id: name.clone(),
            display_name: info
                .get("displayName")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format_quota_model_name(name)),
            remaining_fraction,
            percent_remaining,
            percent_used: 100u32.saturating_sub(percent_remaining),
            reset_time: quota
                .and_then(|q| q.get("resetTime"))
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        });
    }
    out.sort_by(|a, b| a.model_id.cmp(&b.model_id));
    out
}

fn quota_groups_from_summary(val: &Value) -> Vec<QuotaGroup> {
    let Some(groups) = val.get("groups").and_then(|g| g.as_array()) else {
        return Vec::new();
    };
    groups
        .iter()
        .filter_map(|group| {
            let buckets = group
                .get("buckets")
                .and_then(|b| b.as_array())?
                .iter()
                .filter_map(|bucket| {
                    let remaining = bucket.get("remainingFraction").and_then(|v| v.as_f64())?;
                    let window = bucket
                        .get("window")
                        .and_then(|v| v.as_str())
                        .or_else(|| bucket.get("bucketId").and_then(|v| v.as_str()))
                        .unwrap_or("5h");
                    Some(QuotaWindow {
                        window: window.to_string(),
                        remaining_fraction: remaining,
                        percent_remaining: (remaining.clamp(0.0, 1.0) * 100.0).round() as u32,
                        reset_time: bucket
                            .get("resetTime")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                    })
                })
                .collect::<Vec<_>>();
            if buckets.is_empty() {
                return None;
            }
            Some(QuotaGroup {
                display_name: group
                    .get("displayName")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Quota")
                    .to_string(),
                buckets,
            })
        })
        .collect()
}

fn format_quota_model_name(model_id: &str) -> String {
    let lower = model_id.to_ascii_lowercase();
    if lower.contains("claude-3-7-sonnet") || lower.contains("claude-3.7-sonnet") {
        "Claude 3.7 Sonnet".to_string()
    } else if lower.contains("claude-opus") {
        "Claude Opus".to_string()
    } else if lower.contains("claude-3-5-sonnet") || lower.contains("claude-3.5-sonnet") {
        "Claude 3.5 Sonnet".to_string()
    } else if lower.contains("gemini-3.1-pro") || lower.contains("gemini-3-pro") {
        "Gemini 3.1 Pro".to_string()
    } else if lower.contains("gemini-3.8-flash") || lower.contains("gemini-3-flash") {
        "Gemini 3.8 Flash".to_string()
    } else if lower.contains("gemini-2.5-pro") {
        "Gemini 2.5 Pro".to_string()
    } else if lower.contains("gemini-2.5-flash") {
        "Gemini 2.5 Flash".to_string()
    } else if lower.contains("gemini-pro") {
        "Gemini Pro".to_string()
    } else {
        model_id
            .trim_end_matches("-tiered")
            .split('-')
            .map(|word| {
                let mut c = word.chars();
                match c.next() {
                    None => String::new(),
                    Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}
