//! ChatGPT / Codex 的 OAuth：PKCE 授权、换 token、刷新、从 JWT 里读身份。
//!
//! 走的是 Codex CLI 自己那条路（`codex login`），协议事实全部来自公开的 codex-rs：
//!
//! - client 是 Codex CLI 官方的 `app_EMoamEEZ73f0CkXaXp7hrann`，回调地址登记死在
//!   `http://localhost:1455/auth/callback`——改不了，所以桌面端**自己在 1455 上听**
//!   （见 [`crate::callback`]），用户在浏览器里点完同意就自动收到授权码，不用贴地址。
//! - 授权 URL 要多带两个 OpenAI 私有参数：`id_token_add_organizations=true`（id_token 才带
//!   组织列表）和 `codex_cli_simplified_flow=true`（走 Codex 的精简同意页，而不是平台 API 的
//!   选组织页）。
//! - PKCE 用 S256；verifier 用 hex 编码（codex-rs 的写法，对端实现宽松但跟着走最稳）。
//! - 刷新时 scope 去掉 `offline_access`。**refresh_token 会轮换**：每次刷新换一把新的，旧的
//!   立刻失效，重复使用会得到 `refresh_token_reused`——所以刷完必须马上落库，且同一个账号
//!   的并发刷新要合并（在 service 层）。
//! - access_token 与 id_token 都是 JWT。身份在 `https://api.openai.com/auth` claim 下：
//!   `chatgpt_account_id` 是推理请求头 `chatgpt-account-id` 的值，缺了它这个号根本用不了
//!   Codex（纯平台 API 账号就没有）。邮箱在 id_token 顶层 `email`，或 access_token 的
//!   `https://api.openai.com/profile.email`。**只解码不验签**：这些字段只用来展示和构造
//!   请求头，真正的鉴权在上游。

use nexus_core::{AppError, ErrorCode, Secret};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use time::OffsetDateTime;

/// Codex CLI 官方客户端。
pub const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
pub const TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
/// 登记在 client 上的回调地址，改不了。
pub const REDIRECT_URI: &str = "http://localhost:1455/auth/callback";
/// 回调地址里的端口。桌面端就在这个口上收授权码。
pub const CALLBACK_PORT: u16 = 1455;
pub const SCOPE: &str = "openid profile email offline_access";
pub const REFRESH_SCOPE: &str = "openid profile email";

const AUTH_CLAIM: &str = "https://api.openai.com/auth";
const PROFILE_CLAIM: &str = "https://api.openai.com/profile";

/// 对 auth.openai.com 的超时。它偶尔慢，但不该拖住界面半分钟。
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);

/// 上游用这些 error code 表示「这把凭证到头了」——重试多少次都一样，该标失效等人重新授权。
const FATAL_CODES: &[&str] = &[
    "invalid_grant",
    "refresh_token_reused",
    "invalid_client",
    "unauthorized_client",
];

fn urlencode(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

fn urldecode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'%' && i + 2 < bytes.len() {
            let hex = &bytes[i + 1..i + 3];
            if let Some(v) = std::str::from_utf8(hex)
                .ok()
                .and_then(|h| u8::from_str_radix(h, 16).ok())
            {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b == b'+' { b' ' } else { b });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 解 `a=1&b=2`。只取第一个同名键。
pub fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (urldecode(k), urldecode(v)),
            None => (urldecode(kv), String::new()),
        })
        .collect()
}

fn query_get<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
}

// ---------------------------------------------------------------------------
// PKCE 与授权地址
// ---------------------------------------------------------------------------

/// 一对 PKCE。`verifier` 是秘密，只留在 Rust 侧。
pub struct Pkce {
    pub verifier: Secret,
    pub challenge: String,
}

impl Pkce {
    pub fn generate() -> Self {
        use base64::Engine;
        use rand::Rng;
        use sha2::{Digest, Sha256};

        let mut raw = [0u8; 64];
        rand::rng().fill_bytes(&mut raw);
        let verifier: String = raw.iter().map(|b| format!("{b:02x}")).collect();
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(verifier.as_bytes()));
        Self {
            verifier: Secret::new(verifier),
            challenge,
        }
    }
}

/// 防 CSRF 的 state：32 字节随机 hex。
pub fn new_state() -> String {
    use rand::Rng;
    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    raw.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn authorize_url(state: &str, challenge: &str) -> String {
    let params = [
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("scope", SCOPE),
        ("state", state),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
    ];
    let qs: Vec<String> = params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v)))
        .collect();
    format!("{AUTHORIZE_URL}?{}", qs.join("&"))
}

/// 用户从浏览器地址栏贴回来的东西（回调服务器没起来时的兜底路径）。
///
/// 三种都认：整条回调 URL（最常见）、`code#state`（个别工具这么显示）、裸 code。
/// 不认就 `None`，让界面明说「这不像一个回调地址」，而不是拿一段乱字符去换 token
/// 然后回一个语焉不详的 400。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Callback {
    pub code: String,
    pub state: Option<String>,
}

pub fn parse_callback(text: &str) -> Option<Callback> {
    let raw = text.trim();
    if raw.is_empty() {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") {
        let query = raw.split_once('?').map(|(_, q)| q)?;
        let query = query.split('#').next().unwrap_or("");
        let pairs = parse_query(query);
        let code = query_get(&pairs, "code")?.to_string();
        return Some(Callback {
            code,
            state: query_get(&pairs, "state").map(str::to_string),
        });
    }
    if let Some((code, state)) = raw.split_once('#') {
        let code = code.trim();
        if code.is_empty() {
            return None;
        }
        let state = state.trim();
        return Some(Callback {
            code: code.to_string(),
            state: (!state.is_empty()).then(|| state.to_string()),
        });
    }
    // 裸 code：OpenAI 的授权码是一段不含空白的 URL 安全字符串。
    let ok = raw.len() >= 8
        && raw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'~' | b'-'));
    ok.then(|| Callback {
        code: raw.to_string(),
        state: None,
    })
}

// ---------------------------------------------------------------------------
// token 端点
// ---------------------------------------------------------------------------

/// 一组 token。`expires_at` 是 access_token 的过期时刻，按本地时钟算（上游给的是 expires_in）。
pub struct TokenSet {
    pub access_token: Secret,
    pub refresh_token: Option<Secret>,
    pub id_token: Option<Secret>,
    pub expires_at: OffsetDateTime,
}

impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "<redacted>"),
            )
            .field("id_token", &self.id_token.as_ref().map(|_| "<redacted>"))
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// 和 auth.openai.com 打交道时的失败。
///
/// `fatal` 是调用方最需要的那一位：true 表示这把凭证已经没救（refresh token 被吊销、被重复
/// 使用、授权码用过了），该标失效等人重新授权；false 表示网络或对端暂时不可用，下次再试。
/// 分不清这两种的后果是：要么把一个只是暂时连不上的号标死，要么对着一把已吊销的 token
/// 每分钟刷一次直到有人注意日志。
#[derive(Debug, Clone)]
pub struct OauthError {
    pub message: String,
    pub fatal: bool,
    pub status: Option<u16>,
    pub code: Option<String>,
}

impl OauthError {
    fn transient(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            fatal: false,
            status: None,
            code: None,
        }
    }

    pub fn into_app_error(self) -> AppError {
        if self.fatal {
            AppError::unauthorized(self.message)
        } else if self.status.is_some() {
            AppError::upstream(self.message)
        } else {
            AppError::network(self.message)
        }
    }
}

impl std::fmt::Display for OauthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OauthError {}

/// token 端点的客户端。`token_url` 可换，测试对着假端点。
#[derive(Clone)]
pub struct TokenClient {
    http: reqwest::Client,
    token_url: String,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    id_token: Option<String>,
    expires_in: Option<f64>,
    error: Option<String>,
    error_description: Option<String>,
}

impl TokenClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            token_url: TOKEN_URL.to_string(),
        }
    }

    pub fn with_token_url(mut self, url: impl Into<String>) -> Self {
        self.token_url = url.into();
        self
    }

    pub fn token_url(&self) -> &str {
        &self.token_url
    }

    /// 授权码换 token。`redirect_uri` 必须和发起授权时的一致。
    pub async fn exchange_code(
        &self,
        code: &str,
        verifier: &Secret,
    ) -> std::result::Result<TokenSet, OauthError> {
        self.post(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("redirect_uri", REDIRECT_URI),
            ("code_verifier", verifier.expose()),
        ])
        .await
    }

    /// 刷新。调用方负责同一把 refresh token 不并发刷（见 service 层的按账号互斥）。
    pub async fn refresh(
        &self,
        refresh_token: &Secret,
    ) -> std::result::Result<TokenSet, OauthError> {
        self.post(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token.expose()),
            ("scope", REFRESH_SCOPE),
        ])
        .await
    }

    async fn post(&self, form: &[(&str, &str)]) -> std::result::Result<TokenSet, OauthError> {
        let res = self
            .http
            .post(&self.token_url)
            .timeout(HTTP_TIMEOUT)
            .header("accept", "application/json")
            .form(form)
            .send()
            .await
            .map_err(|e| OauthError::transient(format!("无法连接 auth.openai.com：{e}")))?;
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        let parsed: Option<TokenResponse> = serde_json::from_str(&text).ok();

        if !(200..300).contains(&status) {
            let code = parsed
                .as_ref()
                .and_then(|p| p.error.clone())
                .filter(|c| !c.is_empty());
            let desc = parsed
                .as_ref()
                .and_then(|p| p.error_description.clone())
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| {
                    let head: String = text.chars().take(200).collect();
                    if head.is_empty() {
                        format!("HTTP {status}")
                    } else {
                        head
                    }
                });
            // 4xx 且带着上游明确的错误码才算致命；5xx 和网关错误页是对端的问题，换个时间再试。
            let fatal = status < 500
                && match &code {
                    Some(c) => FATAL_CODES.contains(&c.as_str()),
                    None => status == 400 || status == 401,
                };
            return Err(OauthError {
                message: format!(
                    "auth.openai.com {status}{}: {desc}",
                    code.as_deref().map(|c| format!(" {c}")).unwrap_or_default()
                ),
                fatal,
                status: Some(status),
                code,
            });
        }

        let Some(p) = parsed else {
            return Err(OauthError {
                message: "auth.openai.com 返回了 200 但不是 JSON".into(),
                fatal: false,
                status: Some(status),
                code: None,
            });
        };
        let access = p
            .access_token
            .filter(|t| !t.is_empty())
            .ok_or_else(|| OauthError {
                message: "auth.openai.com 返回了 200 但没有 access_token".into(),
                fatal: false,
                status: Some(status),
                code: None,
            })?;
        let expires_in = p
            .expires_in
            .filter(|s| s.is_finite() && *s > 0.0)
            .unwrap_or(3600.0);
        Ok(TokenSet {
            access_token: Secret::new(access),
            refresh_token: p.refresh_token.filter(|t| !t.is_empty()).map(Secret::new),
            id_token: p.id_token.filter(|t| !t.is_empty()).map(Secret::new),
            expires_at: OffsetDateTime::now_utc() + Duration::from_secs_f64(expires_in),
        })
    }
}

// ---------------------------------------------------------------------------
// JWT 与身份
// ---------------------------------------------------------------------------

/// 只解码 payload，不验签。
pub fn decode_jwt_claims(jwt: &str) -> Option<serde_json::Value> {
    use base64::Engine;
    let mut parts = jwt.trim().split('.');
    let (_h, payload, _s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.is_object().then_some(v)
}

/// access_token 是不是 JWT 形态。refresh token 是不透明串，两者靠这个区分。
pub fn looks_like_jwt(value: &str) -> bool {
    let v = value.trim();
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        && decode_jwt_claims(v).is_some()
}

/// JWT `exp`（秒）→ 时刻。
pub fn jwt_expiry(jwt: &str) -> Option<OffsetDateTime> {
    let claims = decode_jwt_claims(jwt)?;
    let exp = claims.get("exp")?.as_f64()?;
    (exp > 0.0)
        .then(|| OffsetDateTime::from_unix_timestamp(exp as i64).ok())
        .flatten()
}

/// 从一对 token 里拼出的身份。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub email: Option<String>,
    /// ChatGPT 账号 id：推理请求头 `chatgpt-account-id` 就是它。
    pub account_id: Option<String>,
    pub user_id: Option<String>,
    /// plus / pro / team / free …
    pub plan_type: Option<String>,
    pub organization_id: Option<String>,
    /// JWT `organizations[].title` / `name`。个人工作区常常是 `Personal`。
    pub organization_title: Option<String>,
}

fn str_of(v: Option<&serde_json::Value>) -> Option<String> {
    v.and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// id_token 和 access_token 各带一部分身份，两个都看：`codex login` 的 auth.json 两个都有，
/// 刷新响应里也都有；只有 access_token 时邮箱要从 profile claim 取。
pub fn identity_from_tokens(id_token: Option<&str>, access_token: Option<&str>) -> Identity {
    let id = id_token.and_then(decode_jwt_claims).unwrap_or_default();
    let access = access_token.and_then(decode_jwt_claims).unwrap_or_default();
    let empty = serde_json::Map::new();
    let access_auth = access
        .get(AUTH_CLAIM)
        .and_then(|v| v.as_object())
        .unwrap_or(&empty);
    let id_auth = id
        .get(AUTH_CLAIM)
        .and_then(|v| v.as_object())
        .unwrap_or(&empty);
    let profile = access
        .get(PROFILE_CLAIM)
        .and_then(|v| v.as_object())
        .unwrap_or(&empty);
    // id_token 的 auth claim 优先（它才带组织列表），缺的字段从 access_token 补。
    let auth = |key: &str| str_of(id_auth.get(key)).or_else(|| str_of(access_auth.get(key)));

    let orgs = id_auth
        .get("organizations")
        .or_else(|| access_auth.get("organizations"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let default_org = orgs
        .iter()
        .find(|o| o.get("is_default").and_then(|v| v.as_bool()) == Some(true))
        .or_else(|| orgs.first());

    Identity {
        email: str_of(id.get("email")).or_else(|| str_of(profile.get("email"))),
        account_id: auth("chatgpt_account_id"),
        user_id: auth("chatgpt_user_id").or_else(|| auth("user_id")),
        plan_type: auth("chatgpt_plan_type"),
        organization_id: default_org.and_then(|o| str_of(o.get("id"))),
        organization_title: default_org.and_then(|o| {
            str_of(o.get("title"))
                .or_else(|| str_of(o.get("name")))
                .or_else(|| str_of(o.get("description")))
        }),
    }
}

/// `ErrorCode` 与 `fatal` 的对应关系，给 service 层判「要不要标失效」用。
pub fn is_fatal(err: &AppError) -> bool {
    err.code == ErrorCode::Unauthorized
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn jwt(payload: serde_json::Value) -> String {
        use base64::Engine;
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes());
        format!(
            "{}.{}.sig",
            b64(r#"{"alg":"RS256","typ":"JWT"}"#),
            b64(&payload.to_string())
        )
    }

    #[test]
    fn pkce_challenge_is_the_sha256_of_a_hex_verifier() {
        use base64::Engine;
        use sha2::{Digest, Sha256};
        let p = Pkce::generate();
        assert_eq!(p.verifier.expose().len(), 128, "64 字节的 hex");
        assert!(p.verifier.expose().chars().all(|c| c.is_ascii_hexdigit()));
        let want = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(p.verifier.expose().as_bytes()));
        assert_eq!(p.challenge, want);
        assert_ne!(Pkce::generate().verifier.expose(), p.verifier.expose());
    }

    #[test]
    fn authorize_url_carries_openais_private_parameters() {
        let url = authorize_url("st", "ch");
        assert!(url.starts_with("https://auth.openai.com/oauth/authorize?"));
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("redirect_uri=http%3A%2F%2Flocalhost%3A1455%2Fauth%2Fcallback"));
        assert!(url.contains("scope=openid%20profile%20email%20offline_access"));
        assert!(url.contains("id_token_add_organizations=true"));
        assert!(url.contains("codex_cli_simplified_flow=true"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("state=st"));
    }

    #[test]
    fn callback_input_accepts_three_shapes_and_rejects_junk() {
        assert_eq!(
            parse_callback("http://localhost:1455/auth/callback?code=abc123&state=xyz"),
            Some(Callback {
                code: "abc123".into(),
                state: Some("xyz".into())
            })
        );
        assert_eq!(
            parse_callback("  the-code#the-state "),
            Some(Callback {
                code: "the-code".into(),
                state: Some("the-state".into())
            })
        );
        assert_eq!(
            parse_callback("AbCdEfGh1234_.~-"),
            Some(Callback {
                code: "AbCdEfGh1234_.~-".into(),
                state: None
            })
        );
        assert_eq!(parse_callback(""), None);
        assert_eq!(parse_callback("这不是地址"), None);
        assert_eq!(
            parse_callback("http://localhost:1455/auth/callback?state=only"),
            None
        );
        assert_eq!(parse_callback("short"), None, "太短不像授权码");
    }

    #[test]
    fn identity_is_merged_from_both_tokens_with_the_default_org_winning() {
        let id = jwt(serde_json::json!({
            "email": "alice@example.com",
            "https://api.openai.com/auth": {
                "chatgpt_account_id": "acct_123",
                "chatgpt_plan_type": "plus",
                "organizations": [
                    { "id": "org-a", "title": "Other", "is_default": false },
                    { "id": "org-b", "title": "Personal", "is_default": true }
                ],
            }
        }));
        let access = jwt(serde_json::json!({
            "exp": 4_000_000_000u64,
            "https://api.openai.com/profile": { "email": "ignored@example.com" },
            "https://api.openai.com/auth": { "chatgpt_account_id": "acct_123", "user_id": "user-1" },
        }));
        let who = identity_from_tokens(Some(&id), Some(&access));
        assert_eq!(
            who.email.as_deref(),
            Some("alice@example.com"),
            "id_token 的邮箱优先"
        );
        assert_eq!(who.account_id.as_deref(), Some("acct_123"));
        assert_eq!(
            who.user_id.as_deref(),
            Some("user-1"),
            "缺的字段从 access_token 补"
        );
        assert_eq!(who.plan_type.as_deref(), Some("plus"));
        assert_eq!(who.organization_id.as_deref(), Some("org-b"));
        assert_eq!(who.organization_title.as_deref(), Some("Personal"));

        let only_access = identity_from_tokens(None, Some(&access));
        assert_eq!(only_access.email.as_deref(), Some("ignored@example.com"));
        assert!(jwt_expiry(&access).is_some());
        assert!(looks_like_jwt(&access));
        assert!(!looks_like_jwt("rt-opaque-refresh-token-xxxxxxxx"));
        assert!(!looks_like_jwt("a.b"));
    }

    #[test]
    fn query_parsing_decodes_percent_and_plus() {
        let q = parse_query("code=a%2Fb&state=x+y&flag");
        assert_eq!(q[0], ("code".into(), "a/b".into()));
        assert_eq!(q[1], ("state".into(), "x y".into()));
        assert_eq!(q[2], ("flag".into(), String::new()));
    }

    #[test]
    fn oauth_errors_map_fatal_to_unauthorized_and_the_rest_to_retryable_codes() {
        let fatal = OauthError {
            message: "gone".into(),
            fatal: true,
            status: Some(400),
            code: Some("invalid_grant".into()),
        };
        assert_eq!(fatal.into_app_error().code, ErrorCode::Unauthorized);
        let upstream = OauthError {
            message: "503".into(),
            fatal: false,
            status: Some(503),
            code: None,
        };
        assert_eq!(upstream.into_app_error().code, ErrorCode::Upstream);
        assert_eq!(
            OauthError::transient("net").into_app_error().code,
            ErrorCode::Network
        );
    }
}
