//! Kiro 推理的公共事实：端点、模型对外名、头。
//!
//! 请求体的构造在 `nexus-gateway` 的 `kiro` 模块。对外报 `kiro-claude-*`，避免和 Cursor 的
//! `claude-*` 撞车；上游要的是剥掉前缀的名字。

use sha2::{Digest, Sha256};
use std::time::Duration;

pub const Q_ENDPOINT: &str = "https://q.us-east-1.amazonaws.com/generateAssistantResponse";
pub const OIDC_BASE: &str = "https://oidc.us-east-1.amazonaws.com";
pub const START_URL: &str = "https://view.awsapps.com/start";
pub const SOCIAL_REFRESH: &str = "https://prod.us-east-1.auth.desktop.kiro.dev/refreshToken";
pub const SOCIAL_AUTH_BASE: &str = "https://prod.us-east-1.auth.desktop.kiro.dev";
pub const SOCIAL_REDIRECT_URI: &str = "kiro://kiro.kiroAgent/authenticate-success";
pub const CLIENT_NAME: &str = "Kiro IDE";
pub const REGION: &str = "us-east-1";

pub fn social_authorize_url(provider: &str, state: &str, code_challenge: &str) -> String {
    format!(
        "{SOCIAL_AUTH_BASE}/login?idp={provider}&redirect_uri={redirect}&code_challenge={challenge}&code_challenge_method=S256&state={state}",
        provider = query_escape(provider),
        redirect = query_escape(SOCIAL_REDIRECT_URI),
        challenge = query_escape(code_challenge),
        state = query_escape(state),
    )
}

pub fn code_challenge(verifier: &str) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn query_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub fn percent_decode(s: &str) -> String {
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

pub fn parse_social_callback(raw: &str) -> (Option<String>, Option<String>) {
    let trimmed = raw.trim();
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
        if code.is_some() {
            return (code, state);
        }
    }
    if !trimmed.is_empty()
        && !trimmed.contains('/')
        && !trimmed.contains('?')
        && !trimmed.contains('&')
        && !trimmed.contains(' ')
    {
        return (Some(trimmed.to_string()), None);
    }
    (None, None)
}

pub const SCOPES: &[&str] = &[
    "codewhisperer:completions",
    "codewhisperer:analysis",
    "codewhisperer:conversations",
    "codewhisperer:transformations",
    "codewhisperer:taskassist",
];

pub const REFRESH_AHEAD: Duration = Duration::from_secs(5 * 60);

/// 流式 `generateAssistantResponse` 对 Builder ID 强制要的占位 profile。
/// 没有真实 profile 的号不带它会 400：`profileArn is required`。
pub const BUILDER_ID_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX";

/// GitHub / Google 社交登录共用的 profile。用量类接口不能带 Builder ID 占位符。
pub const SOCIAL_PROFILE_ARN: &str =
    "arn:aws:codewhisperer:us-east-1:699475941385:profile/EHGA3GRVQMUK";

/// `(对外名, 上游名)`。上游认带点的短名（`claude-sonnet-4.6`），不认日期后缀。
/// `-thinking` 只决定要不要打开思考，上游模型名不含它。
///
/// 这张表是**对真上游逐个打过的**（`cargo run -p nexus-gateway --example kiro_matrix -- models`，
/// 2026-10-01，KIRO POWER 账号）。加新条目前先用那个探针问一遍：上游不认的名字会回
/// `INVALID_MODEL_ID`，而前端把这张表当模型清单，写错就是给用户一个点下去必报错的选项。
/// 已经确认上游**不认**、所以不在表里的：`claude-sonnet-4.8`、`claude-fable-5`、`claude-opus-4.1`。
pub const KIRO_MODELS: &[(&str, &str)] = &[
    // ── 旗舰推理 (Opus 系列，深度思考优先) ──
    ("kiro-claude-opus-5.5", "claude-opus-5.5"),
    ("kiro-claude-opus-5.5-thinking", "claude-opus-5.5"),
    ("kiro-claude-opus-5", "claude-opus-5"),
    ("kiro-claude-opus-5-thinking", "claude-opus-5"),
    ("kiro-claude-opus-4.8", "claude-opus-4.8"),
    ("kiro-claude-opus-4.8-thinking", "claude-opus-4.8"),
    ("kiro-claude-opus-4.7", "claude-opus-4.7"),
    ("kiro-claude-opus-4.7-thinking", "claude-opus-4.7"),
    ("kiro-claude-opus-4.6", "claude-opus-4.6"),
    ("kiro-claude-opus-4.6-thinking", "claude-opus-4.6"),
    ("kiro-claude-opus-4.5", "claude-opus-4.5"),
    ("kiro-claude-opus-4.5-thinking", "claude-opus-4.5"),
    // ── 主力编程 (Sonnet 系列) ──
    ("kiro-claude-sonnet-5", "claude-sonnet-5"),
    ("kiro-claude-sonnet-5-thinking", "claude-sonnet-5"),
    ("kiro-claude-sonnet-4.6", "claude-sonnet-4.6"),
    ("kiro-claude-sonnet-4.6-thinking", "claude-sonnet-4.6"),
    ("kiro-claude-sonnet-4.5", "claude-sonnet-4.5"),
    ("kiro-claude-sonnet-4.5-thinking", "claude-sonnet-4.5"),
    ("kiro-claude-sonnet-4", "claude-sonnet-4"),
    // ── 极速轻量 (Haiku 系列) ──
    ("kiro-claude-haiku-4.5", "claude-haiku-4.5"),
    // ── 通用基座 (GPT 系列) ──
    ("kiro-gpt-5.6-sol", "gpt-5.6-sol"),
    ("kiro-gpt-5.6-terra", "gpt-5.6-terra"),
    ("kiro-gpt-5.6-luna", "gpt-5.6-luna"),
];

/// 客户端可能直接写 Anthropic 的日期名或别名。先收成上游的短名。
const MODEL_ALIASES: &[(&str, &str)] = &[
    ("claude-opus-5-5", "claude-opus-5.5"),
    ("claude-opus-5.5", "claude-opus-5.5"),
    ("claude-opus-5-5-20260201", "claude-opus-5.5"),
    ("claude-opus-5", "claude-opus-5"),
    ("claude-opus-4-8", "claude-opus-4.8"),
    ("claude-opus-4.8", "claude-opus-4.8"),
    ("claude-opus-4-7", "claude-opus-4.7"),
    ("claude-opus-4.7", "claude-opus-4.7"),
    ("claude-opus-4-6", "claude-opus-4.6"),
    ("claude-opus-4.6", "claude-opus-4.6"),
    ("claude-opus-4-5-20251101", "claude-opus-4.5"),
    ("claude-opus-4.5", "claude-opus-4.5"),
    ("claude-3-opus", "claude-opus-4.5"),
    ("claude-3.5-opus", "claude-opus-4.5"),
    ("claude-sonnet-5", "claude-sonnet-5"),
    ("claude-sonnet-4-6", "claude-sonnet-4.6"),
    ("claude-sonnet-4.6", "claude-sonnet-4.6"),
    ("claude-sonnet-4-5-20250929", "claude-sonnet-4.5"),
    ("claude-sonnet-4-5", "claude-sonnet-4.5"),
    ("claude-sonnet-4.5", "claude-sonnet-4.5"),
    ("claude-sonnet-4", "claude-sonnet-4"),
    ("claude-3-7-sonnet", "claude-sonnet-4.5"),
    ("claude-3.7-sonnet", "claude-sonnet-4.5"),
    ("claude-3-5-sonnet", "claude-sonnet-4.5"),
    ("claude-3.5-sonnet", "claude-sonnet-4.5"),
    ("claude-3-sonnet", "claude-sonnet-4"),
    ("claude-haiku-4-5-20251001", "claude-haiku-4.5"),
    ("claude-haiku-4-5", "claude-haiku-4.5"),
    ("claude-haiku-4.5", "claude-haiku-4.5"),
    ("claude-3-5-haiku", "claude-haiku-4.5"),
    ("claude-3.5-haiku", "claude-haiku-4.5"),
    ("claude-3-haiku", "claude-haiku-4.5"),
];

pub const ROUTE_PREFIXES: &[&str] = &["kiro/"];

pub fn split_route_prefix(model: &str) -> (&str, bool) {
    let t = model.trim();
    let lower = t.to_ascii_lowercase();
    for p in ROUTE_PREFIXES {
        if let Some(rest) = lower.strip_prefix(p) {
            let start = t.len() - rest.len();
            return (&t[start..], true);
        }
    }
    (t, false)
}

pub fn is_kiro_model(model: &str) -> bool {
    let (name, forced) = split_route_prefix(model);
    if forced {
        return true;
    }
    let base = name.trim().to_ascii_lowercase();
    // 上游短名（claude-opus-4.6）不在这里认。裸的 claude-* 是 Cursor 的号；
    // 要走 Kiro 得写 kiro-claude-* 或 kiro/。
    KIRO_MODELS
        .iter()
        .any(|(ext, _)| ext.eq_ignore_ascii_case(&base))
        || base.starts_with("kiro-claude-")
        || base.starts_with("kiro-gpt-")
}

/// 发给 Amazon Q 的模型名。`kiro-claude-sonnet-4.6-thinking` → `claude-sonnet-4.6`。
pub fn upstream_model(model: &str) -> String {
    let (name, _) = split_route_prefix(model);
    let mut base = name.trim().to_string();
    if let Some(rest) = base.strip_prefix("kiro-") {
        base = rest.to_string();
    }
    let bare = strip_thinking(&base);
    for (ext, up) in KIRO_MODELS {
        let ext_body = ext.strip_prefix("kiro-").unwrap_or(ext);
        if ext.eq_ignore_ascii_case(&base)
            || ext_body.eq_ignore_ascii_case(&base)
            || ext_body.eq_ignore_ascii_case(&bare)
            || up.eq_ignore_ascii_case(&bare)
        {
            return (*up).to_string();
        }
    }
    // 优先检查精确别名表（避免日期后缀或者老版本号落入通用正则映射成不识别的模型）
    for (alias, up) in MODEL_ALIASES {
        if alias.eq_ignore_ascii_case(&bare) {
            return (*up).to_string();
        }
    }
    // 去除 8 位纯数字日期后缀后再次匹配别名（如 claude-3-7-sonnet-20250219）
    if let Some((prefix, suffix)) = bare.rsplit_once('-') {
        if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
            for (alias, up) in MODEL_ALIASES {
                if alias.eq_ignore_ascii_case(prefix) {
                    return (*up).to_string();
                }
            }
        }
    }
    if let Some(norm) = normalize_claude_model(&bare) {
        return norm;
    }
    bare
}

/// 1M 上下文的型号。`contextUsageEvent` 只给百分比，漏配会把进度条缩小五倍。
pub fn context_window(model: &str) -> u32 {
    let mapped = upstream_model(model).to_ascii_lowercase();
    if mapped.starts_with("gpt") {
        return 272_000;
    }
    if matches!(
        mapped.as_str(),
        "claude-opus-5.5"
            | "claude-opus-5"
            | "claude-opus-4.8"
            | "claude-opus-4.7"
            | "claude-opus-4.6"
            | "claude-sonnet-5"
            | "claude-sonnet-4.6"
    ) || mapped.contains("claude-5")
    {
        return 1_000_000;
    }
    200_000
}

pub fn is_placeholder_profile_arn(arn: &str) -> bool {
    arn == BUILDER_ID_PROFILE_ARN
}

fn is_api_key(auth_method: Option<&str>) -> bool {
    auth_method.is_some_and(|m| {
        let m = m.trim();
        m.eq_ignore_ascii_case("api_key") || m.eq_ignore_ascii_case("apikey")
    })
}

fn is_social(auth_method: Option<&str>) -> bool {
    auth_method.is_some_and(|m| {
        let m = m.trim();
        m.eq_ignore_ascii_case("social")
            || m.eq_ignore_ascii_case("github")
            || m.eq_ignore_ascii_case("google")
    })
}

/// 流式聊天要带的 profileArn。API Key 没有这个概念。
/// 已存的真实 ARN（含社交共享 ARN）原样用；没有就按登录方式补默认值。
pub fn streaming_profile_arn(auth_method: Option<&str>, stored: Option<&str>) -> Option<String> {
    if is_api_key(auth_method) {
        return None;
    }
    if let Some(arn) = stored.map(str::trim).filter(|s| s.starts_with("arn:")) {
        return Some(arn.to_string());
    }
    Some(
        if is_social(auth_method) {
            SOCIAL_PROFILE_ARN
        } else {
            BUILDER_ID_PROFILE_ARN
        }
        .to_string(),
    )
}

/// CodeWhisperer 的 `tokentype` 头。社交 / IdC 不带；缺了企业 SSO 会被静默拒绝。
pub fn token_type_header(auth_method: Option<&str>) -> Option<&'static str> {
    let m = auth_method?.trim();
    if is_api_key(Some(m)) {
        return Some("API_KEY");
    }
    const EXTERNAL: &[&str] = &[
        "external_idp",
        "azuread",
        "azure",
        "entra",
        "entra-id",
        "microsoft",
        "m365",
        "office365",
        "external",
    ];
    if EXTERNAL.iter().any(|a| m.eq_ignore_ascii_case(a)) {
        Some("EXTERNAL_IDP")
    } else {
        None
    }
}

fn canonical_version(parts: &[&str]) -> Option<String> {
    let first = *parts.first()?;
    if parts.len() == 1
        && first.contains('.')
        && first
            .split('.')
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    {
        return Some(first.to_string());
    }
    if !first.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    match parts {
        [_, second] if second.chars().all(|c| c.is_ascii_digit()) => {
            Some(format!("{first}.{second}"))
        }
        [_] => Some(first.to_string()),
        _ => None,
    }
}

/// `claude-opus-4-7`、`claude-sonnet-4-5-20250929` 收成上游短名。非 Claude 不动。
fn normalize_claude_model(model: &str) -> Option<String> {
    let mut normalized = model.trim().to_ascii_lowercase();
    loop {
        let mut stripped = false;
        for suffix in ["-thinking", "-latest"] {
            if let Some(rest) = normalized.strip_suffix(suffix) {
                normalized = rest.to_string();
                stripped = true;
            }
        }
        if !stripped {
            break;
        }
    }
    if let Some((base, suffix)) = normalized.rsplit_once('-') {
        if suffix.len() == 8 && suffix.chars().all(|c| c.is_ascii_digit()) {
            normalized = base.to_string();
        }
    }
    let body = normalized.strip_prefix("claude-")?;
    const FAMILIES: [&str; 5] = ["sonnet", "opus", "haiku", "fable", "mythos"];
    for family in FAMILIES {
        if let Some(rest) = body.strip_prefix(family) {
            let rest = rest
                .strip_prefix('-')
                .or_else(|| rest.strip_prefix('.'))
                .unwrap_or(rest);
            let parts: Vec<&str> = rest.split('-').collect();
            let version = canonical_version(&parts)?;
            return Some(format!("claude-{family}-{version}"));
        }
    }
    let parts: Vec<&str> = body.split('-').collect();
    let family_index = parts.iter().position(|part| FAMILIES.contains(part))?;
    if family_index == 0 || family_index + 1 != parts.len() {
        return None;
    }
    let version = canonical_version(&parts[..family_index])?;
    Some(format!("claude-{}-{version}", parts[family_index]))
}

pub fn wants_thinking(model: &str) -> bool {
    let (name, _) = split_route_prefix(model);
    name.to_ascii_lowercase().contains("thinking")
}

fn strip_thinking(model: &str) -> String {
    let mut base = model.trim().to_string();
    loop {
        let lower = base.to_ascii_lowercase();
        let Some(stripped) = lower.strip_suffix("-thinking") else {
            return base;
        };
        base.truncate(stripped.len());
    }
}

/// 同一把 refresh token 对应同一台「Kiro IDE」。没有 refresh 时用账号标签顶上。
pub fn machine_id(refresh_token: Option<&str>, seed: &str) -> String {
    let material = match refresh_token.map(str::trim).filter(|s| !s.is_empty()) {
        Some(token) => format!("KotlinNativeAPI/{token}"),
        None => format!("KiroFallback/{seed}"),
    };
    let digest = Sha256::digest(material.as_bytes());
    hex_encode(&digest)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

/// 出站头对齐 Kiro IDE 打 `generateAssistantResponse` 的那一套。
/// 头里只放真实 profileArn；Builder ID 占位符只进请求体，不进这个头。
pub fn chat_headers(
    access_token: &str,
    machine_id: &str,
    profile_arn: Option<&str>,
    invocation_id: &str,
    token_type: Option<&str>,
) -> Vec<(String, String)> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let os_ver = match os {
        "darwin" => "24.6.0",
        "win32" => "10.0.22631",
        _ => "6.8.0",
    };
    const SDK: &str = "1.0.34";
    const KIRO: &str = "0.11.132";
    const NODE: &str = "22.22.0";
    let user_agent = format!(
        "aws-sdk-js/{SDK} ua/2.1 os/{os}#{os_ver} lang/js md/nodejs#{NODE} api/codewhispererstreaming#{SDK} m/E KiroIDE-{KIRO}-{machine_id}"
    );
    let amz_ua = format!("aws-sdk-js/{SDK} KiroIDE-{KIRO}-{machine_id}");
    let mut headers = vec![
        ("authorization".into(), format!("Bearer {access_token}")),
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "application/vnd.amazon.eventstream".into()),
        ("user-agent".into(), user_agent),
        ("x-amz-user-agent".into(), amz_ua),
        ("x-amzn-kiro-agent-mode".into(), "vibe".into()),
        ("x-amzn-codewhisperer-optout".into(), "true".into()),
        ("amz-sdk-request".into(), "attempt=1; max=3".into()),
        ("amz-sdk-invocation-id".into(), invocation_id.to_string()),
    ];
    if let Some(arn) = profile_arn
        .map(str::trim)
        .filter(|s| !s.is_empty() && !is_placeholder_profile_arn(s))
    {
        headers.push(("x-amzn-kiro-profile-arn".into(), arn.to_string()));
    }
    if let Some(token_type) = token_type.map(str::trim).filter(|s| !s.is_empty()) {
        headers.push(("tokentype".into(), token_type.to_string()));
    }
    headers
}

pub const MAX_TOOL_DESC: usize = 10237;
pub const MAX_TOOL_NAME: usize = 63;

pub fn truncate_tool_description(description: &str) -> String {
    if description.len() <= MAX_TOOL_DESC {
        return description.to_string();
    }
    let marker = "... (description truncated)";
    let limit = MAX_TOOL_DESC.saturating_sub(marker.len());
    format!("{}{marker}", utf8_prefix(description, limit))
}

pub fn shorten_tool_name(name: &str) -> String {
    let name = name.trim();
    if name.len() <= MAX_TOOL_NAME {
        return name.to_string();
    }
    let digest = Sha256::digest(name.as_bytes());
    let suffix = hex_encode(&digest)[..8].to_string();
    let prefix_len = MAX_TOOL_NAME - 1 - suffix.len();
    format!("{}_{suffix}", utf8_prefix(name, prefix_len))
}

fn utf8_prefix(s: &str, limit: usize) -> &str {
    if s.len() <= limit {
        return s;
    }
    let mut end = limit;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kiro_names_do_not_steal_cursor_claude() {
        assert!(is_kiro_model("kiro-claude-sonnet-4.5"));
        assert!(is_kiro_model("kiro/claude-sonnet-4.5"));
        assert!(!is_kiro_model("claude-sonnet-5"));
        assert!(!is_kiro_model("claude-opus-4.6"));
        assert!(!is_kiro_model("claude-sonnet-4-5-20250929"));
        assert_eq!(
            upstream_model("kiro-claude-sonnet-4.5"),
            "claude-sonnet-4.5"
        );
        assert_eq!(upstream_model("kiro-claude-opus-5.5"), "claude-opus-5.5");
        assert_eq!(
            upstream_model("kiro-claude-opus-5.5-thinking"),
            "claude-opus-5.5"
        );
        assert_eq!(upstream_model("kiro/claude-opus-5-5"), "claude-opus-5.5");
        assert_eq!(
            upstream_model("kiro/claude-3-7-sonnet-20250219"),
            "claude-sonnet-4.5"
        );
        assert_eq!(
            upstream_model("kiro/claude-3-5-sonnet"),
            "claude-sonnet-4.5"
        );
        assert_eq!(upstream_model("kiro/claude-3-5-haiku"), "claude-haiku-4.5");
        assert_eq!(upstream_model("kiro/claude-haiku-4.5"), "claude-haiku-4.5");
        assert_eq!(
            upstream_model("kiro/claude-sonnet-4-6-thinking"),
            "claude-sonnet-4.6"
        );
        assert_eq!(
            upstream_model("kiro-claude-opus-4.6-thinking"),
            "claude-opus-4.6"
        );
        assert_eq!(upstream_model("kiro/claude-opus-4-7"), "claude-opus-4.7");
        assert_eq!(
            upstream_model("kiro/claude-sonnet-4-5-20250929"),
            "claude-sonnet-4.5"
        );
        assert_eq!(upstream_model("kiro-gpt-5.6-sol"), "gpt-5.6-sol");
        assert_eq!(context_window("kiro-claude-opus-5.5"), 1_000_000);
        assert_eq!(context_window("kiro-claude-opus-4.7"), 1_000_000);
        assert_eq!(context_window("kiro-gpt-5.6-luna"), 272_000);
        assert_eq!(context_window("kiro-claude-haiku-4.5"), 200_000);
        assert!(wants_thinking("kiro-claude-opus-5.5-thinking"));
        assert!(!wants_thinking("kiro-claude-opus-5.5"));
        assert!(wants_thinking("kiro-claude-sonnet-4.6-thinking"));
        assert!(!wants_thinking("kiro-claude-sonnet-4.6"));
        assert_eq!(
            streaming_profile_arn(Some("builder-id"), None).as_deref(),
            Some(BUILDER_ID_PROFILE_ARN)
        );
        assert_eq!(
            streaming_profile_arn(Some("github"), None).as_deref(),
            Some(SOCIAL_PROFILE_ARN)
        );
        assert!(streaming_profile_arn(Some("api_key"), None).is_none());
        assert_eq!(token_type_header(Some("entra")), Some("EXTERNAL_IDP"));
        assert!(token_type_header(Some("IdC")).is_none());
    }

    #[test]
    fn headers_look_like_kiro_ide_and_social_carries_the_profile() {
        let id = machine_id(Some("refresh-token"), "ignored");
        assert_eq!(id.len(), 64);
        assert_eq!(machine_id(Some("refresh-token"), "other"), id);
        let social = chat_headers(
            "tok",
            &id,
            Some("arn:aws:codewhisperer:us-east-1:1:profile/p"),
            "inv-1",
            None,
        );
        let joined = social
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("KiroIDE-0.11.132-"));
        assert!(joined.contains("x-amzn-kiro-agent-mode: vibe"));
        assert!(joined.contains("amz-sdk-invocation-id: inv-1"));
        assert!(joined.contains("x-amzn-kiro-profile-arn: arn:aws:codewhisperer"));
        let builder = chat_headers("tok", &id, None, "inv-1", None);
        assert!(builder.iter().all(|(k, _)| k != "x-amzn-kiro-profile-arn"));
        let placeholder = chat_headers(
            "tok",
            &id,
            Some(BUILDER_ID_PROFILE_ARN),
            "inv-1",
            Some("API_KEY"),
        );
        assert!(placeholder
            .iter()
            .all(|(k, _)| k != "x-amzn-kiro-profile-arn"));
        assert!(placeholder
            .iter()
            .any(|(k, v)| k == "tokentype" && v == "API_KEY"));
    }

    #[test]
    fn test_social_authorize_and_parse_callback() {
        let auth_url = social_authorize_url("Google", "state-123", "challenge-abc");
        assert!(auth_url.contains("idp=Google"));
        assert!(auth_url.contains("state=state-123"));
        assert!(auth_url.contains("code_challenge=challenge-abc"));
        assert!(
            auth_url.contains("redirect_uri=kiro%3A%2F%2Fkiro.kiroAgent%2Fauthenticate-success")
        );

        let cb = "kiro://kiro.kiroAgent/authenticate-success?code=my-code-123&state=state-123";
        let (code, state) = parse_social_callback(cb);
        assert_eq!(code.as_deref(), Some("my-code-123"));
        assert_eq!(state.as_deref(), Some("state-123"));

        let cb_query = "code=code_only&state=state_only";
        let (code2, state2) = parse_social_callback(cb_query);
        assert_eq!(code2.as_deref(), Some("code_only"));
        assert_eq!(state2.as_deref(), Some("state_only"));
    }
}
