//! Claude 订阅号的协议常量，以及不联网就能定下来的形状。
//!
//! 登录走 Claude Code 公开的 OAuth（PKCE）。出站是 Anthropic Messages：
//! 客户端本来就讲 Messages 时，网关原样转发请求体和 SSE，`cache_control`、
//! thinking 签名、`anthropic-beta` 才不会在中间表示里丢掉。
//!
//! OAuth 出站的指纹（计费头、身份句、工具名）在 [`crate::fingerprint`]。
//! 认不出是 Claude Code 的流量会被划进 extra usage，所以第三方客户端走订阅号
//! 时要补齐官方 CLI 会带的那几样。

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Claude Code 公开客户端的授权页。和 `http://localhost:54545/callback` 是一对：
/// CLIProxyAPI 用的就是这组，浏览器同意后会跳回本机这个口。
pub const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
pub const REDIRECT_URI: &str = "http://localhost:54545/callback";
pub const CALLBACK_PORT: u16 = 54545;
pub const CALLBACK_PATH: &str = "/callback";
pub const TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
pub const PROFILE_URL: &str = "https://api.anthropic.com/api/oauth/profile";
/// 换票成功后 Claude Code 紧接着查的角色。失败不影响登录，只是把这次登录补全。
pub const ROLES_URL: &str = "https://api.anthropic.com/api/oauth/claude_cli/roles";
pub const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
pub const MESSAGES_ORIGIN: &str = "https://api.anthropic.com";

/// Claude Code 公开的 OAuth client。没有配套的 client secret，PKCE 就是验证。
pub const OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";

/// OAuth token 打 Messages 时上游要求的 beta。这是「这把是 OAuth」的声明，
/// 不是 Claude Code 的身份头。
pub const OAUTH_BETA: &str = "oauth-2025-04-20";

pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// 和 Claude Code / CLIProxyAPI 要的那一组一样。多要 `org:create_api_key`
/// 会让同意页和官方客户端对不上。
pub const OAUTH_SCOPES: &[&str] = &[
    "user:profile",
    "user:inference",
    "user:sessions:claude_code",
    "user:mcp_servers",
    "user:file_upload",
];

/// 目录里报的模型。带 `claude/` 前缀之后才是对外 id；裸名不抢别的通道。
/// 短名是对外写的；OAuth 出站时部分短名要换成带日期的上游 id，见 [`normalize_model_id`]。
pub const MODELS: &[&str] = &[
    "claude-fable-5-1",
    "claude-fable-5",
    "claude-opus-5-5",
    "claude-opus-5",
    "claude-opus-4-8",
    "claude-opus-4-7",
    "claude-opus-4-6",
    "claude-opus-4-5",
    "claude-sonnet-5",
    "claude-sonnet-4-6",
    "claude-sonnet-4-5",
    "claude-haiku-4-5",
];

/// OAuth 出站必须用带日期的 id 的那几个短名。其余原样。
const MODEL_ID_OVERRIDES: &[(&str, &str)] = &[
    ("claude-sonnet-4-5", "claude-sonnet-4-5-20250929"),
    ("claude-opus-4-5", "claude-opus-4-5-20251101"),
    ("claude-haiku-4-5", "claude-haiku-4-5-20251001"),
];

/// 发给 `api.anthropic.com` 的模型名。短名按官方 OAuth 规则补日期。
pub fn normalize_model_id(id: &str) -> String {
    let id = id.trim();
    MODEL_ID_OVERRIDES
        .iter()
        .find(|(from, _)| from.eq_ignore_ascii_case(id))
        .map(|(_, to)| (*to).to_string())
        .unwrap_or_else(|| id.to_string())
}

/// 这个名字归 Claude 通道吗。认短名、带日期的上游 id、`[1M]` 后缀。
pub fn owns_model(model: &str) -> bool {
    let base = upstream_model(model);
    if MODELS.iter().any(|m| m.eq_ignore_ascii_case(&base)) {
        return true;
    }
    if MODEL_ID_OVERRIDES
        .iter()
        .any(|(_, to)| to.eq_ignore_ascii_case(&base))
    {
        return true;
    }
    let lower = base.to_ascii_lowercase();
    lower.starts_with("claude-sonnet-")
        || lower.starts_with("claude-opus-")
        || lower.starts_with("claude-haiku-")
        || lower.starts_with("claude-fable-")
}

pub const ROUTE_PREFIXES: &[&str] = &["claude/"];

/// 过期前这么久就去刷。OAuth access 通常一小时，提前五分钟换。
pub const REFRESH_AHEAD_SECS: i64 = 5 * 60;

pub fn authorize_url(state: &str, code_challenge: &str) -> String {
    let scope = OAUTH_SCOPES.join(" ");
    format!(
        "{AUTHORIZE_URL}?code=true&client_id={client}&response_type=code&redirect_uri={redirect}&scope={scope}&code_challenge={challenge}&code_challenge_method=S256&state={state}",
        client = form_escape(OAUTH_CLIENT_ID),
        redirect = form_escape(REDIRECT_URI),
        scope = form_escape(&scope),
        challenge = form_escape(code_challenge),
        state = form_escape(state),
    )
}

pub fn code_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// 发给 `api.anthropic.com` 的模型名。通道前缀和 Claude Code 的 `[1m]` 预算后缀都剥掉。
pub fn upstream_model(model: &str) -> String {
    let mut m = model.trim();
    for prefix in ROUTE_PREFIXES {
        if m.len() >= prefix.len()
            && m.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        {
            m = &m[prefix.len()..];
            break;
        }
    }
    let lower = m.to_ascii_lowercase();
    if let Some(stripped) = lower.strip_suffix("[1m]") {
        m = &m[..stripped.len()];
    }
    normalize_model_id(m.trim())
}

pub fn wants_one_million(model: &str) -> bool {
    model.to_ascii_lowercase().contains("[1m]")
}

/// 用户贴进来的东西是哪一种。认不出来就 `None`，调用方去说该贴什么。
pub fn classify_import(raw: &str) -> Option<ImportPiece> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    if text.starts_with('{') {
        let value: Value = serde_json::from_str(text).ok()?;
        return credentials_from_json(&value);
    }
    if let Some(code) = code_from_text(text) {
        return Some(ImportPiece::Code(code));
    }
    let compact = text.split_whitespace().next().unwrap_or(text);
    if compact.starts_with("sk-ant-api") {
        return Some(ImportPiece::ApiKey(compact.to_string()));
    }
    if compact.starts_with("sk-ant-") {
        return Some(ImportPiece::SetupToken(compact.to_string()));
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportPiece {
    /// 授权页回调里的 `code`，可能带 `#state`。
    Code(OAuthCode),
    Credentials(ImportedOauth),
    SetupToken(String),
    ApiKey(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthCode {
    pub code: String,
    pub state: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedOauth {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// 毫秒时间戳。文件里是 `expiresAt`。
    pub expires_at_ms: Option<i64>,
    pub email: Option<String>,
    pub account_uuid: Option<String>,
    pub plan: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClaudeQuota {
    pub five_hour_percentage: i32,
    pub five_hour_resets_at: Option<i64>,
    pub seven_day_percentage: i32,
    pub seven_day_resets_at: Option<i64>,
    pub seven_day_sonnet_percentage: Option<i32>,
    pub extra_usage_percentage: Option<i32>,
}

impl ClaudeQuota {
    /// 卡片上画更紧的那一桶。两边都是 0 时仍报 5 小时，让人知道这是滚动窗口而不是「没有额度」。
    pub fn tighter(&self) -> (i32, &'static str, Option<i64>) {
        if self.five_hour_percentage >= self.seven_day_percentage {
            (
                self.five_hour_percentage,
                "five_hour",
                self.five_hour_resets_at,
            )
        } else {
            (
                self.seven_day_percentage,
                "weekly",
                self.seven_day_resets_at,
            )
        }
    }

    pub fn exhausted(&self) -> bool {
        self.five_hour_percentage >= 100 || self.seven_day_percentage >= 100
    }
}

pub fn parse_quota(raw: &Value) -> ClaudeQuota {
    let five = raw.get("five_hour");
    let week = raw.get("seven_day");
    let sonnet = raw
        .get("seven_day_sonnet")
        .or_else(|| raw.get("seven_day_sonnet_4"))
        .or_else(|| raw.get("seven_day_model"));
    let extra = raw.get("extra_usage");
    let extra_on = extra
        .and_then(|v| v.get("is_enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    ClaudeQuota {
        five_hour_percentage: percent_of(five),
        five_hour_resets_at: resets_at(five),
        seven_day_percentage: percent_of(week),
        seven_day_resets_at: resets_at(week),
        seven_day_sonnet_percentage: sonnet.map(percent_of_value),
        extra_usage_percentage: extra_on.then(|| percent_of(extra)),
    }
}

pub fn plan_from_profile(profile: &Value) -> Option<String> {
    match profile
        .pointer("/organization/organization_type")
        .and_then(Value::as_str)
    {
        Some("claude_max") => Some("Max".into()),
        Some("claude_pro") => Some("Pro".into()),
        Some("claude_enterprise") => Some("Enterprise".into()),
        Some("claude_team") => Some("Team".into()),
        _ => None,
    }
}

pub fn email_from_profile(profile: &Value) -> Option<String> {
    profile
        .pointer("/account/email")
        .or_else(|| profile.pointer("/account/email_address"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

pub fn uuid_from_profile(profile: &Value) -> Option<String> {
    profile
        .pointer("/account/uuid")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

fn credentials_from_json(value: &Value) -> Option<ImportPiece> {
    let oauth = value.get("claudeAiOauth").unwrap_or(value);
    let access = string_at(oauth, &["accessToken", "access_token"])?;
    Some(ImportPiece::Credentials(ImportedOauth {
        access_token: access,
        refresh_token: string_at(oauth, &["refreshToken", "refresh_token"]),
        expires_at_ms: int_at(oauth, &["expiresAt", "expires_at"]),
        email: string_at(value, &["email"]).or_else(|| string_at(oauth, &["email"])),
        account_uuid: string_at(oauth, &["accountUuid", "account_uuid"]).or_else(|| {
            value
                .pointer("/account/uuid")
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
        plan: string_at(oauth, &["subscriptionType", "subscription_type"]),
    }))
}

fn code_from_text(text: &str) -> Option<OAuthCode> {
    let trimmed = text.trim();
    if let Some(query) = trimmed
        .split_once('?')
        .map(|(_, q)| q)
        .or_else(|| trimmed.contains("code=").then_some(trimmed))
    {
        let query = query.split('#').next().unwrap_or(query);
        let mut code = None;
        let mut state = None;
        for pair in query.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            let v = percent_decode(v);
            match k {
                "code" if !v.is_empty() => code = Some(v),
                "state" if !v.is_empty() => state = Some(v),
                _ => {}
            }
        }
        if let Some(code) = code {
            return Some(split_code_state(code, state));
        }
    }
    None
}

fn split_code_state(code: String, state: Option<String>) -> OAuthCode {
    if let Some((code, extra)) = code.split_once('#') {
        let state = state.or_else(|| {
            let s = extra.trim();
            (!s.is_empty()).then(|| s.to_string())
        });
        return OAuthCode {
            code: code.trim().to_string(),
            state,
        };
    }
    OAuthCode { code, state }
}

fn string_at(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        value
            .get(*k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
    })
}

fn int_at(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|k| match value.get(*k) {
        Some(Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

fn percent_of(bucket: Option<&Value>) -> i32 {
    bucket.map(percent_of_value).unwrap_or(0)
}

fn percent_of_value(bucket: &Value) -> i32 {
    let raw = bucket
        .get("utilization")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    if !raw.is_finite() {
        return 0;
    }
    raw.round().clamp(0.0, 100.0) as i32
}

fn resets_at(bucket: Option<&Value>) -> Option<i64> {
    let value = bucket?.get("resets_at")?;
    match value {
        Value::Number(n) => {
            let n = n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))?;
            Some(if n > 10_000_000_000 { n / 1000 } else { n })
        }
        Value::String(s) => {
            let s = s.trim();
            if let Ok(n) = s.parse::<i64>() {
                return Some(if n > 10_000_000_000 { n / 1000 } else { n });
            }
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
                .ok()
                .map(|t| t.unix_timestamp())
        }
        _ => None,
    }
}

fn form_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn authorize_url_carries_pkce_and_the_public_client() {
        let url = authorize_url("st ate", "chal");
        assert!(url.starts_with(AUTHORIZE_URL));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(OAUTH_CLIENT_ID));
        assert!(url.contains("user%3Ainference") || url.contains("user:inference"));
        assert!(url.contains("st+ate") || url.contains("st%20ate"));
    }

    #[test]
    fn challenge_is_base64url_sha256_without_padding() {
        assert_eq!(code_challenge("abc"), code_challenge("abc"));
        assert!(!code_challenge("abc").contains('='));
        assert_ne!(code_challenge("abc"), code_challenge("abd"));
    }

    #[test]
    fn upstream_model_strips_prefix_and_context_suffix() {
        assert_eq!(
            upstream_model("claude/claude-sonnet-4-6[1M]"),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            upstream_model("claude-sonnet-4-5"),
            "claude-sonnet-4-5-20250929"
        );
        assert!(wants_one_million("claude-opus-4-8[1m]"));
        assert!(!wants_one_million("claude-opus-4-8"));
        assert!(owns_model("claude-sonnet-4-5-20250929"));
        assert!(owns_model("claude/claude-fable-5[1M]"));
        assert!(!owns_model("gpt-5.4"));
    }

    #[test]
    fn callback_and_credentials_and_keys_classify() {
        let code = classify_import(
            "https://platform.claude.com/oauth/code/callback?code=abc%2F1&state=s1",
        )
        .unwrap();
        assert_eq!(
            code,
            ImportPiece::Code(OAuthCode {
                code: "abc/1".into(),
                state: Some("s1".into()),
            })
        );
        let creds = classify_import(
            r#"{"claudeAiOauth":{"accessToken":"at","refreshToken":"rt","expiresAt":10}}"#,
        )
        .unwrap();
        match creds {
            ImportPiece::Credentials(c) => {
                assert_eq!(c.access_token, "at");
                assert_eq!(c.refresh_token.as_deref(), Some("rt"));
                assert_eq!(c.expires_at_ms, Some(10));
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            classify_import("sk-ant-api03-xyz"),
            Some(ImportPiece::ApiKey(_))
        ));
        assert!(matches!(
            classify_import("sk-ant-oat01-xyz"),
            Some(ImportPiece::SetupToken(_))
        ));
    }

    #[test]
    fn quota_reads_the_tighter_window() {
        let q = parse_quota(&json!({
            "five_hour": { "utilization": 80, "resets_at": 1_700_000_000 },
            "seven_day": { "utilization": 12.4, "resets_at": "2026-10-01T00:00:00Z" },
            "extra_usage": { "is_enabled": true, "utilization": 3 }
        }));
        assert_eq!(q.five_hour_percentage, 80);
        assert_eq!(q.seven_day_percentage, 12);
        assert_eq!(q.five_hour_resets_at, Some(1_700_000_000));
        assert!(q.seven_day_resets_at.is_some());
        assert_eq!(q.extra_usage_percentage, Some(3));
        assert_eq!(q.tighter().0, 80);
        assert!(!q.exhausted());
    }

    #[test]
    fn profile_plan_is_the_four_official_names() {
        let p = json!({"organization": {"organization_type": "claude_max"}, "account": {"email": "a@b.co", "uuid": "u1"}});
        assert_eq!(plan_from_profile(&p).as_deref(), Some("Max"));
        assert_eq!(email_from_profile(&p).as_deref(), Some("a@b.co"));
        assert_eq!(uuid_from_profile(&p).as_deref(), Some("u1"));
        assert!(
            plan_from_profile(&json!({"organization": {"organization_type": "nope"}})).is_none()
        );
    }
}
