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
pub const CLIENT_NAME: &str = "Kiro IDE";
pub const REGION: &str = "us-east-1";

pub const SCOPES: &[&str] = &[
    "codewhisperer:completions",
    "codewhisperer:analysis",
    "codewhisperer:conversations",
    "codewhisperer:transformations",
    "codewhisperer:taskassist",
];

pub const REFRESH_AHEAD: Duration = Duration::from_secs(5 * 60);

/// `(对外名, 上游名)`。上游认带点的短名（`claude-sonnet-4.6`），不认日期后缀。
/// `-thinking` 只决定要不要在 system 里打开思考，上游模型名不含它。
pub const KIRO_MODELS: &[(&str, &str)] = &[
    ("kiro-claude-opus-4.6", "claude-opus-4.6"),
    ("kiro-claude-opus-4.6-thinking", "claude-opus-4.6"),
    ("kiro-claude-sonnet-4.6", "claude-sonnet-4.6"),
    ("kiro-claude-sonnet-4.6-thinking", "claude-sonnet-4.6"),
    ("kiro-claude-opus-4.5", "claude-opus-4.5"),
    ("kiro-claude-opus-4.5-thinking", "claude-opus-4.5"),
    ("kiro-claude-sonnet-4.5", "claude-sonnet-4.5"),
    ("kiro-claude-sonnet-4.5-thinking", "claude-sonnet-4.5"),
    ("kiro-claude-haiku-4.5", "claude-haiku-4.5"),
    ("kiro-claude-sonnet-4", "claude-sonnet-4"),
    ("kiro-claude-opus-4.1", "claude-opus-4.1"),
];

/// 客户端可能直接写 Anthropic 的日期名。先收成上游的短名。
const MODEL_ALIASES: &[(&str, &str)] = &[
    ("claude-opus-4-6", "claude-opus-4.6"),
    ("claude-opus-4.6", "claude-opus-4.6"),
    ("claude-sonnet-4-6", "claude-sonnet-4.6"),
    ("claude-sonnet-4.6", "claude-sonnet-4.6"),
    ("claude-opus-4-5-20251101", "claude-opus-4.5"),
    ("claude-opus-4.5", "claude-opus-4.5"),
    ("claude-sonnet-4-5-20250929", "claude-sonnet-4.5"),
    ("claude-sonnet-4.5", "claude-sonnet-4.5"),
    ("claude-haiku-4-5-20251001", "claude-haiku-4.5"),
    ("claude-haiku-4.5", "claude-haiku-4.5"),
    ("claude-sonnet-4", "claude-sonnet-4"),
    ("claude-opus-4.1", "claude-opus-4.1"),
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
}

/// 发给 Amazon Q 的模型名。`kiro-claude-sonnet-4.6-thinking` → `claude-sonnet-4.6`。
pub fn upstream_model(model: &str) -> String {
    let (name, _) = split_route_prefix(model);
    let mut base = name.trim().to_string();
    if let Some(rest) = base.strip_prefix("kiro-") {
        base = rest.to_string();
    }
    let thinking = base.to_ascii_lowercase().contains("thinking");
    let bare = strip_thinking(&base);
    for (ext, up) in KIRO_MODELS {
        if ext.eq_ignore_ascii_case(&base) || ext.eq_ignore_ascii_case(&bare) {
            return (*up).to_string();
        }
    }
    for (alias, up) in MODEL_ALIASES {
        if alias.eq_ignore_ascii_case(&bare) {
            return (*up).to_string();
        }
    }
    let _ = thinking;
    bare
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
/// Builder ID 不带 `profileArn`；社交登录带。
pub fn chat_headers(
    access_token: &str,
    machine_id: &str,
    profile_arn: Option<&str>,
    invocation_id: &str,
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
        (
            "accept".into(),
            "application/vnd.amazon.eventstream".into(),
        ),
        ("user-agent".into(), user_agent),
        ("x-amz-user-agent".into(), amz_ua),
        ("x-amzn-kiro-agent-mode".into(), "vibe".into()),
        ("x-amzn-codewhisperer-optout".into(), "true".into()),
        ("amz-sdk-request".into(), "attempt=1; max=3".into()),
        ("amz-sdk-invocation-id".into(), invocation_id.to_string()),
    ];
    if let Some(arn) = profile_arn.map(str::trim).filter(|s| !s.is_empty()) {
        headers.push(("x-amzn-kiro-profile-arn".into(), arn.to_string()));
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
        assert_eq!(upstream_model("kiro/claude-haiku-4.5"), "claude-haiku-4.5");
        assert_eq!(
            upstream_model("kiro/claude-sonnet-4-6-thinking"),
            "claude-sonnet-4.6"
        );
        assert_eq!(
            upstream_model("kiro-claude-opus-4.6-thinking"),
            "claude-opus-4.6"
        );
        assert!(wants_thinking("kiro-claude-sonnet-4.6-thinking"));
        assert!(!wants_thinking("kiro-claude-sonnet-4.6"));
    }

    #[test]
    fn headers_look_like_kiro_ide_and_social_carries_the_profile() {
        let id = machine_id(Some("refresh-token"), "ignored");
        assert_eq!(id.len(), 64);
        assert_eq!(machine_id(Some("refresh-token"), "other"), id);
        let social = chat_headers("tok", &id, Some("arn:aws:codewhisperer:us-east-1:1:profile/p"), "inv-1");
        let joined = social
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("KiroIDE-0.11.132-"));
        assert!(joined.contains("x-amzn-kiro-agent-mode: vibe"));
        assert!(joined.contains("amz-sdk-invocation-id: inv-1"));
        assert!(joined.contains("x-amzn-kiro-profile-arn: arn:aws:codewhisperer"));
        let builder = chat_headers("tok", &id, None, "inv-1");
        assert!(builder
            .iter()
            .all(|(k, _)| k != "x-amzn-kiro-profile-arn"));
    }
}
