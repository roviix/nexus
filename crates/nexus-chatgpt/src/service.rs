//! ChatGPT 账号的用例层：授权登录、导入、续期、额度。Tauri 命令和网关的号源都只跟它打交道。
//!
//! 三条规矩：
//! - **刷完就存**。refresh token 轮换，刷新响应到手的下一行就是落库；中间不做别的。
//! - **同一个号的刷新不并发**（按账号一把 `tokio::sync::Mutex`）。网关的两个并发请求同时发现
//!   token 快过期、同时去刷，后到的必然拿到 `refresh_token_reused`，把一个好号误判成死号。
//! - **致命与暂时分开**（`OauthError::fatal`）。refresh token 作废 → `NeedsLogin`，等人重新授权；
//!   网络抖动 → 什么都不改，还没过期的旧 access token 照用。

use crate::billing::ChatGptBilling;
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

/// 一行（或一个 JSON 对象）解析出来的凭证。见 [`parse_import_entries`]。
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

/// 粘贴导入的汇总。凭证不回传——前端只需要新建 / 更新 / 失败各几个。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub created: u32,
    pub updated: u32,
    pub failed: u32,
    pub errors: Vec<String>,
}

impl ImportOutcome {
    pub fn accepted(&self) -> u32 {
        self.created + self.updated
    }
}

/// 不透明的 refresh token：URL 安全字符、够长。短串会被拒掉，那是有意的——
/// 一段乱字符拿去刷只会换来一个语焉不详的 400。
fn looks_like_opaque_token(s: &str) -> bool {
    s.len() >= 20
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'~' | b'-'))
}

fn looks_like_json(s: &str) -> bool {
    matches!(s.as_bytes().first(), Some(b'{' | b'['))
}

/// 从一段可能夹着 NDJSON / 剩余文本的输入里尽量抠 JSON 值。
/// 抠出的偏移之后交给行模式，这样「一个 pretty-printed 对象 + 几行 `----`」也能一次贴进来。
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

/// sub2api 后台导出常常是 `{data:{items:[…]}}` / `{accounts:[…]}` / `{contents:[…]}`，
/// 不是单个 session。摊平之后每个元素再各自解析，省得用户先手工拆。
fn unwrap_collection(v: serde_json::Value) -> Vec<serde_json::Value> {
    match v {
        serde_json::Value::Array(arr) => arr.into_iter().flat_map(unwrap_collection).collect(),
        serde_json::Value::Object(map) => {
            for key in ["items", "accounts", "contents"] {
                if let Some(serde_json::Value::Array(arr)) = map.get(key) {
                    return arr.iter().cloned().flat_map(unwrap_collection).collect();
                }
            }
            if let Some(data) = map.get("data") {
                let wrapped = data.is_array()
                    || data.as_object().is_some_and(|o| {
                        o.contains_key("items")
                            || o.contains_key("accounts")
                            || o.contains_key("contents")
                    });
                if wrapped {
                    return unwrap_collection(data.clone());
                }
            }
            vec![serde_json::Value::Object(map)]
        }
        other => vec![other],
    }
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
    // 有的导出把 credentials 序列化成字符串。
    let raw = field.as_str()?.trim();
    if !raw.starts_with('{') {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(raw)
        .ok()
        .filter(|p| p.is_object())
}

fn is_agent_identity(v: &serde_json::Value) -> bool {
    if v.get("agent_identity").is_some() || v.get("agentIdentity").is_some() {
        return true;
    }
    matches!(
        pick_str(v, &["auth_mode", "authMode"]).as_deref(),
        Some(m) if m.eq_ignore_ascii_case("agentidentity")
            || m.eq_ignore_ascii_case("agent_identity")
    )
}

fn looks_like_agent_identity_blob(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("agentidentity")
        || lower.contains("agent_identity")
        || lower.contains("\"agent_runtime_id\"")
}

fn tokens_from_obj(obj: &serde_json::Value) -> Imported {
    Imported {
        // `token` 是 sub2api 对 accessToken 的短键；sessionToken 故意不看——
        // 那是 chatgpt.com 的 cookie，当 refresh 存进去刷一次就会把好号刷废。
        access_token: pick_str(obj, &["access_token", "accessToken", "token"])
            .filter(|t| looks_like_jwt(t)),
        refresh_token: pick_str(obj, &["refresh_token", "refreshToken"])
            .filter(|t| looks_like_opaque_token(t)),
        id_token: pick_str(obj, &["id_token", "idToken"]).filter(|t| looks_like_jwt(t)),
    }
}

fn merge_imported(mut into: Imported, from: Imported) -> Imported {
    if into.access_token.is_none() {
        into.access_token = from.access_token;
    }
    if into.refresh_token.is_none() {
        into.refresh_token = from.refresh_token;
    }
    if into.id_token.is_none() {
        into.id_token = from.id_token;
    }
    into
}

fn imported_from_value(v: &serde_json::Value) -> Option<Imported> {
    match v {
        serde_json::Value::String(s) => parse_parts(s),
        serde_json::Value::Object(_) => {
            if is_agent_identity(v) {
                return None;
            }
            let mut out = Imported::default();
            // Codex CLI 的 tokens 段最干净，先它；再 sub2api 账号的 credentials；
            // 最后顶层（chatgpt.com session JSON 的 accessToken / camelCase）。
            if let Some(tokens) = object_field(v, "tokens") {
                out = merge_imported(out, tokens_from_obj(&tokens));
            }
            if let Some(creds) = object_field(v, "credentials") {
                out = merge_imported(out, tokens_from_obj(&creds));
            }
            out = merge_imported(out, tokens_from_obj(v));
            (!out.is_empty()).then_some(out)
        }
        _ => None,
    }
}

fn parse_parts(raw: &str) -> Option<Imported> {
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
                for item in unwrap_collection(v) {
                    if let Some(imported) = imported_from_value(&item) {
                        out.push(imported);
                    }
                }
            }
        } else if let Some(imported) = parse_parts(line) {
            out.push(imported);
        }
    }
    out
}

/// 一份粘贴可能是一个号，也可能是 sub2api 那种一次一打。
///
/// 认这些写法（和 sub2api 后台「Codex session」对得上，没有抄它的代码）：
/// - `~/.codex/auth.json` 原文，或把 `tokens` 三个键放顶层 / 放进 `credentials`；
/// - camelCase（`accessToken` / `refreshToken` / `idToken` / `token`）；
/// - JSON 数组、NDJSON、`{items|accounts|contents|data.items:[…]}` 包一层的导出；
/// - `xxx----yyy`（`----` 分隔），JWT 是 access / id，不透明串是 refresh，邮箱段忽略；
/// - 单独一个 JWT 或一个不透明 refresh token；
/// - 以上混贴，一行一个，`#` 当注释。
///
/// `sessionToken` 故意丢掉：那是 chatgpt.com 的 cookie，不是 OAuth refresh。
/// Agent Identity（`auth_mode=agentIdentity`）桌面端还没接，整段跳过。
pub fn parse_import_entries(text: &str) -> Vec<Imported> {
    let raw = text.trim();
    if raw.is_empty() {
        return Vec::new();
    }
    if looks_like_json(raw) {
        let (values, rest) = take_json_values(raw);
        let mut out = Vec::new();
        for v in values {
            for item in unwrap_collection(v) {
                if let Some(imported) = imported_from_value(&item) {
                    out.push(imported);
                }
            }
        }
        if !rest.is_empty() {
            out.extend(parse_import_lines(rest));
        }
        return out;
    }
    parse_import_lines(raw)
}

/// 只取第一份。给「肯定只有一个号」的路径（本机 `auth.json`、旧测试）用。
pub fn parse_import_text(text: &str) -> Option<Imported> {
    parse_import_entries(text).into_iter().next()
}

fn import_unrecognized(text: &str) -> AppError {
    let hint = if looks_like_agent_identity_blob(text) {
        "桌面端还不支持 Codex Agent Identity。请贴 OAuth 的 auth.json，或用授权登录。"
    } else {
        "支持 ~/.codex/auth.json、sub2api 的 Codex session JSON（数组 / 多行）、`access----refresh`，或单独一个 refresh token。"
    };
    AppError::invalid("认不出这段内容。").with_hint(hint)
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
        let imported = parse_import_text(text).ok_or_else(|| import_unrecognized(text))?;
        self.import_parsed(imported, note).await
    }

    /// 一次贴多个号：sub2api 的 Codex session 导出、JSON 数组、多行 `----`。
    /// 能进的先进，认不出或刷失败的记进 `errors`，不因为一条坏的把整份退掉。
    pub async fn import_dump(&self, text: &str, note: Option<&str>) -> Result<ImportOutcome> {
        let entries = parse_import_entries(text);
        if entries.is_empty() {
            return Err(import_unrecognized(text));
        }
        let mut out = ImportOutcome::default();
        for imported in entries {
            match self.import_parsed(imported, note).await {
                Ok(up) => {
                    if up.created {
                        out.created += 1;
                    } else {
                        out.updated += 1;
                    }
                }
                Err(err) => {
                    out.failed += 1;
                    out.errors.push(err.message);
                }
            }
        }
        if out.accepted() == 0 {
            let detail = out
                .errors
                .first()
                .cloned()
                .unwrap_or_else(|| "没有可用的凭证。".into());
            return Err(AppError::invalid(format!("一个都没导进去。{detail}"))
                .with_hint("检查是不是 OAuth 的 auth.json / refresh token；Agent Identity 和 session cookie 进不来。"));
        }
        Ok(out)
    }

    async fn import_parsed(&self, imported: Imported, note: Option<&str>) -> Result<Upserted> {
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
        if identity.plan_type.is_none()
            && identity.email.is_none()
            && identity.user_id.is_none()
            && identity.organization_id.is_none()
            && identity.organization_title.is_none()
        {
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
    ///
    /// 用量到手之后顺带读一次订阅账单。账单失败（被挑战、字段缺）不影响额度快照，也不改账号状态。
    pub async fn refresh_usage(&self, id: &ChatGptAccountId) -> Result<CodexUsage> {
        let (status, text) = match self.backend_get(id, "/wham/usage").await {
            Ok(v) => v,
            Err(e) => {
                let _ = self
                    .repo
                    .record_failure(id, &format!("额度接口连不上：{e}"), None);
                return Err(e);
            }
        };
        if status == 401 {
            self.repo
                .record_failure(id, "额度接口 401", Some(ChatGptStatus::NeedsLogin))?;
            return Err(AppError::unauthorized("chatgpt.com 拒绝了这个号的凭证。"));
        }
        if !(200..300).contains(&status) {
            let err = backend_error("额度接口", status, &text);
            self.repo.record_failure(id, &err.message, None)?;
            return Err(err);
        }
        let payload: serde_json::Value = serde_json::from_str(&text)
            .map_err(|_| AppError::upstream("额度接口返回的不是 JSON"))?;
        let usage = CodexUsage::from_wham(&payload, OffsetDateTime::now_utc());
        self.repo.record_usage(id, &usage)?;
        self.try_refresh_billing(id).await;
        Ok(usage)
    }

    /// 主动问一次订阅：`/accounts/check/v4-2023-04-27`，必要时再问 `/subscriptions`。
    /// 被挑战时保留上一份快照；字段缺席写成 `null`，不要写成「没有订阅」。
    pub async fn refresh_billing(&self, id: &ChatGptAccountId) -> Result<ChatGptBilling> {
        self.refresh_billing_inner(id, true).await
    }

    async fn try_refresh_billing(&self, id: &ChatGptAccountId) {
        if let Err(err) = self.refresh_billing_inner(id, false).await {
            tracing::info!(%err, account = %id, "刷完用量后读订阅失败，不影响额度快照");
        }
    }

    async fn refresh_billing_inner(
        &self,
        id: &ChatGptAccountId,
        auth_is_fatal: bool,
    ) -> Result<ChatGptBilling> {
        let account_ref = self.account_ref(id)?;
        let now = OffsetDateTime::now_utc();
        let mut last_err: Option<AppError> = None;
        let mut parsed: Option<ChatGptBilling> = None;

        for (path, source) in [
            ("/accounts/check/v4-2023-04-27", "accounts/check"),
            ("/wham/accounts/check", "wham/accounts/check"),
        ] {
            match self.backend_class(id, path).await? {
                BackendClass::Json(payload) => {
                    parsed = Some(ChatGptBilling::from_accounts_check(
                        &payload,
                        &account_ref,
                        now,
                        source,
                    ));
                    break;
                }
                BackendClass::Auth => {
                    if auth_is_fatal {
                        self.repo.record_failure(
                            id,
                            "订阅接口 401",
                            Some(ChatGptStatus::NeedsLogin),
                        )?;
                    }
                    return Err(AppError::unauthorized("chatgpt.com 拒绝了这个号的凭证。"));
                }
                BackendClass::Challenge(status) => {
                    last_err = Some(backend_error("订阅接口", status, "<html>"));
                }
                BackendClass::Http(status, head) => {
                    last_err = Some(backend_error("订阅接口", status, &head));
                }
            }
        }

        let had_check = parsed.is_some();
        let mut billing = parsed.unwrap_or_else(|| ChatGptBilling::empty(now, "accounts/check"));
        if billing.expires_at.is_none() || billing.will_renew.is_none() {
            let path = format!("/subscriptions?account_id={account_ref}");
            match self.backend_class(id, &path).await {
                Ok(BackendClass::Json(payload)) => billing.overlay_subscriptions(&payload, now),
                Ok(BackendClass::Auth) if !had_check => {
                    if auth_is_fatal {
                        self.repo.record_failure(
                            id,
                            "订阅接口 401",
                            Some(ChatGptStatus::NeedsLogin),
                        )?;
                    }
                    return Err(AppError::unauthorized("chatgpt.com 拒绝了这个号的凭证。"));
                }
                Ok(BackendClass::Challenge(status)) if !had_check => {
                    last_err = Some(backend_error("订阅接口", status, "<html>"));
                }
                Ok(_) | Err(_) => {}
            }
        }

        let looked = billing.expires_at.is_some()
            || billing.has_active_subscription.is_some()
            || billing.will_renew.is_some()
            || billing.plan_type.is_some();
        if !had_check && !looked {
            return Err(last_err.unwrap_or_else(|| AppError::upstream("订阅接口没有返回账单")));
        }

        self.repo.record_billing(id, &billing)?;
        Ok(billing)
    }

    async fn backend_get(&self, id: &ChatGptAccountId, path: &str) -> Result<(u16, String)> {
        let access = self.access_token(id).await?;
        let account_ref = self.account_ref(id)?;
        let mut req = self.http.get(format!("{}{path}", self.backend_url));
        for (k, v) in protocol::identity_headers(access.expose(), Some(&account_ref)) {
            req = req.header(k, v);
        }
        let res = req
            .send()
            .await
            .map_err(|e| AppError::network(format!("连不上 chatgpt.com：{e}")))?;
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        Ok((status, text))
    }

    async fn backend_class(&self, id: &ChatGptAccountId, path: &str) -> Result<BackendClass> {
        let (status, text) = self.backend_get(id, path).await?;
        Ok(classify_backend(status, &text))
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

enum BackendClass {
    Json(serde_json::Value),
    Auth,
    Challenge(u16),
    Http(u16, String),
}

fn looks_like_challenge(text: &str) -> bool {
    let head: String = text.chars().take(160).collect();
    let low = head.to_ascii_lowercase();
    low.contains("<html") || low.contains("<!doctype")
}

fn classify_backend(status: u16, text: &str) -> BackendClass {
    if status == 401 {
        return BackendClass::Auth;
    }
    if looks_like_challenge(text) {
        return BackendClass::Challenge(status);
    }
    if (200..300).contains(&status) {
        match serde_json::from_str(text) {
            Ok(v) => BackendClass::Json(v),
            Err(_) => BackendClass::Http(status, "返回的不是 JSON".into()),
        }
    } else {
        BackendClass::Http(status, text.chars().take(160).collect())
    }
}

fn backend_error(label: &str, status: u16, text: &str) -> AppError {
    if looks_like_challenge(text) || text.contains("<html") {
        AppError::upstream(format!(
            "{label}被前置网关拦下（{status}），多半是出口 IP 被挑战"
        ))
    } else {
        let head: String = text.chars().take(160).collect();
        AppError::upstream(format!("{label} {status}：{head}"))
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
        check_status: Mutex<u16>,
        check_body: Mutex<Option<serde_json::Value>>,
        sub_status: Mutex<u16>,
        sub_body: Mutex<Option<serde_json::Value>>,
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
                "user_id": "user_wham",
                "plan_type": "pro",
                "rate_limit": {
                    "allowed": true,
                    "limit_reached": false,
                    "primary_window": { "used_percent": 37.5, "reset_after_seconds": 600, "limit_window_seconds": 18000 },
                    "secondary_window": { "used_percent": 12, "reset_after_seconds": 86400, "limit_window_seconds": 604800 }
                },
                "additional_rate_limits": [{
                    "limit_name": "GPT-5.3-Codex-Spark",
                    "metered_feature": "codex_bengalfox",
                    "rate_limit": {
                        "primary_window": { "used_percent": 100, "reset_after_seconds": 18000, "limit_window_seconds": 18000 },
                        "secondary_window": { "used_percent": 8, "reset_after_seconds": 86400, "limit_window_seconds": 604800 }
                    }
                }],
                "credits": { "has_credits": false, "balance": "0" }
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

    async fn accounts_check(State(f): State<Arc<Fake>>) -> (axum::http::StatusCode, String) {
        let status = *f.check_status.lock().unwrap();
        if status != 200 {
            return (
                axum::http::StatusCode::from_u16(status)
                    .unwrap_or(axum::http::StatusCode::NOT_FOUND),
                if status == 403 {
                    "<!DOCTYPE html><html>challenge</html>".into()
                } else {
                    "{}".into()
                },
            );
        }
        let body = f
            .check_body
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| serde_json::json!({ "accounts": {} }));
        (axum::http::StatusCode::OK, body.to_string())
    }

    async fn subscriptions(State(f): State<Arc<Fake>>) -> (axum::http::StatusCode, String) {
        let status = *f.sub_status.lock().unwrap();
        if status != 200 {
            return (
                axum::http::StatusCode::from_u16(status)
                    .unwrap_or(axum::http::StatusCode::NOT_FOUND),
                if status == 403 {
                    "<!DOCTYPE html><html>challenge</html>".into()
                } else {
                    "{}".into()
                },
            );
        }
        let body = f
            .sub_body
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| serde_json::json!({}));
        (axum::http::StatusCode::OK, body.to_string())
    }

    async fn spawn_fake() -> (Arc<Fake>, String) {
        let fake = Arc::new(Fake::default());
        *fake.usage_status.lock().unwrap() = 200;
        *fake.check_status.lock().unwrap() = 404;
        *fake.sub_status.lock().unwrap() = 404;
        let app = Router::new()
            .route("/oauth/token", post(token))
            .route("/backend-api/wham/usage", get(usage))
            .route(
                "/backend-api/accounts/check/v4-2023-04-27",
                get(accounts_check),
            )
            .route("/backend-api/wham/accounts/check", get(accounts_check))
            .route("/backend-api/subscriptions", get(subscriptions))
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

    #[test]
    fn import_text_understands_sub2api_session_shapes_and_batches() {
        let access = access_token("acct_s", 4_000_000_000);
        let idt = id_token("cam@x.com", "acct_s");

        let camel = parse_import_text(
            &serde_json::json!({
                "accessToken": access,
                "refreshToken": RT,
                "idToken": idt,
                "sessionToken": "eyJhbGciOiJub25lIn0.eyJlbWFpbCI6InNlc3Npb25AZXhhbXBsZS5jb20ifQ.sig"
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(camel.access_token.as_deref(), Some(access.as_str()));
        assert_eq!(camel.refresh_token.as_deref(), Some(RT));
        assert_eq!(camel.id_token.as_deref(), Some(idt.as_str()));

        let creds = parse_import_text(
            &serde_json::json!({
                "name": "cam@x.com",
                "platform": "openai",
                "credentials": {
                    "access_token": access,
                    "refresh_token": RT
                }
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(creds.refresh_token.as_deref(), Some(RT));

        let stringified = parse_import_text(
            &serde_json::json!({
                "credentials": serde_json::json!({"refresh_token": RT, "access_token": access}).to_string()
            })
            .to_string(),
        )
        .unwrap();
        assert_eq!(stringified.refresh_token.as_deref(), Some(RT));

        let wrapped = parse_import_entries(
            &serde_json::json!({
                "data": { "items": [
                    { "tokens": { "access_token": access, "refresh_token": RT } },
                    { "accessToken": access_token("acct_t", 4_000_000_000), "refreshToken": "rt-opaque-refresh-token-other-000" }
                ] }
            })
            .to_string(),
        );
        assert_eq!(wrapped.len(), 2);

        let mixed = parse_import_entries(&format!(
            "{}\n# skip\nbob@x.com----{RT}\n",
            serde_json::json!({ "refreshToken": "rt-opaque-refresh-token-ndjson-00" })
        ));
        assert_eq!(mixed.len(), 2);
        assert_eq!(
            mixed[0].refresh_token.as_deref(),
            Some("rt-opaque-refresh-token-ndjson-00")
        );
        assert_eq!(mixed[1].refresh_token.as_deref(), Some(RT));

        let pretty_then_line = format!(
            "{{\n  \"accessToken\": \"{access}\",\n  \"refreshToken\": \"{RT}\"\n}}\nsecond@x.com----{RT}\n"
        );
        assert_eq!(parse_import_entries(&pretty_then_line).len(), 2);

        assert!(
            parse_import_text(
                &serde_json::json!({
                    "sessionToken": "cookie-only",
                    "auth_mode": "agentIdentity",
                    "agent_runtime_id": "rt_1"
                })
                .to_string()
            )
            .is_none(),
            "session cookie / Agent Identity 都不能当 OAuth 凭证"
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
    async fn import_dump_takes_a_sub2api_batch_and_keeps_going_after_a_bad_row() {
        let (_fake, base) = spawn_fake().await;
        let svc = service(&base);
        let a = access_token("acct_batch_1", 4_000_000_000);
        let b = access_token("acct_batch_2", 4_000_000_000);
        let no_sub = jwt(serde_json::json!({
            "exp": 4_000_000_000u64,
            "https://api.openai.com/profile": { "email": "nosub@example.com" }
        }));
        let dump = format!(
            "{}\n{}\n{}",
            serde_json::json!({ "accessToken": a, "refreshToken": RT }),
            serde_json::json!({ "accessToken": no_sub }),
            serde_json::json!({
                "tokens": {
                    "access_token": b,
                    "refresh_token": "rt-opaque-refresh-token-batch-2"
                }
            })
        );
        let out = svc.import_dump(&dump, Some("from sub2api")).await.unwrap();
        assert_eq!(out.created, 2);
        assert_eq!(out.failed, 1);
        assert!(out.errors.iter().any(|e| e.contains("chatgpt_account_id")));
        assert_eq!(svc.list().unwrap().len(), 2);

        let again = svc
            .import_dump(
                &serde_json::json!({ "accessToken": a, "refreshToken": RT }).to_string(),
                None,
            )
            .await
            .unwrap();
        assert_eq!(again.updated, 1);
        assert_eq!(again.created, 0);
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
        assert_eq!(u.user_id.as_deref(), Some("user_wham"));
        assert_eq!(u.additional.len(), 1);
        assert_eq!(u.additional[0].name.as_deref(), Some("GPT-5.3-Codex-Spark"));
        let a = svc.get(&up.account.id).unwrap();
        assert_eq!(a.plan_type.as_deref(), Some("pro"), "套餐跟着快照更新");
        assert_eq!(a.user_id.as_deref(), Some("user_wham"), "额度接口补用户 id");
        assert!(
            a.billing.is_none(),
            "订阅接口 404 不该写成一份空账单，免得界面把「没问到」当成「没有到期日」"
        );
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

    #[tokio::test]
    async fn billing_reads_accounts_check_and_keeps_the_snapshot_when_challenged() {
        let (fake, base) = spawn_fake().await;
        let svc = service(&base);
        let up = svc
            .import_text(
                &format!("{}----{RT}", access_token("acct_1", 4_000_000_000)),
                None,
            )
            .await
            .unwrap();
        *fake.check_status.lock().unwrap() = 200;
        *fake.check_body.lock().unwrap() = Some(serde_json::json!({
            "accounts": {
                "acct_1": {
                    "account": { "account_id": "acct_1", "plan_type": "plus" },
                    "entitlement": {
                        "has_active_subscription": true,
                        "subscription_plan": "chatgptplusplan",
                        "expires_at": "2026-10-01T00:00:00Z"
                    }
                }
            }
        }));
        let b = svc.refresh_billing(&up.account.id).await.unwrap();
        assert_eq!(b.expires_at.as_deref(), Some("2026-10-01T00:00:00Z"));
        assert_eq!(b.has_active_subscription, Some(true));
        assert_eq!(b.will_renew, None, "没写会不会续，不是不会续");
        assert_eq!(
            svc.get(&up.account.id)
                .unwrap()
                .billing
                .unwrap()
                .expires_at
                .as_deref(),
            Some("2026-10-01T00:00:00Z")
        );

        *fake.check_status.lock().unwrap() = 403;
        let err = svc.refresh_billing(&up.account.id).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Upstream);
        assert!(err.message.contains("挑战"), "{}", err.message);
        assert_eq!(
            svc.get(&up.account.id)
                .unwrap()
                .billing
                .unwrap()
                .expires_at
                .as_deref(),
            Some("2026-10-01T00:00:00Z"),
            "被挑战不能把上一份快照冲掉"
        );
        assert_eq!(
            svc.get(&up.account.id).unwrap().status,
            ChatGptStatus::Active
        );
    }

    #[tokio::test]
    async fn billing_fills_renewal_from_subscriptions_when_check_omits_it() {
        let (fake, base) = spawn_fake().await;
        let svc = service(&base);
        let up = svc
            .import_text(
                &format!("{}----{RT}", access_token("acct_1", 4_000_000_000)),
                None,
            )
            .await
            .unwrap();
        *fake.check_status.lock().unwrap() = 200;
        *fake.check_body.lock().unwrap() = Some(serde_json::json!({
            "accounts": {
                "acct_1": {
                    "account": { "account_id": "acct_1", "plan_type": "pro" },
                    "entitlement": { "has_active_subscription": true }
                }
            }
        }));
        *fake.sub_status.lock().unwrap() = 200;
        *fake.sub_body.lock().unwrap() = Some(serde_json::json!({
            "active_until": "2026-11-01T00:00:00Z",
            "will_renew": true,
            "billing_period": "monthly"
        }));
        let b = svc.refresh_billing(&up.account.id).await.unwrap();
        assert_eq!(b.expires_at.as_deref(), Some("2026-11-01T00:00:00Z"));
        assert_eq!(b.will_renew, Some(true));
        assert_eq!(b.billing_period.as_deref(), Some("monthly"));
        assert_eq!(b.plan_type.as_deref(), Some("pro"));
    }
}
