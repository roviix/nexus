//! Kiro 推理的公共事实：端点、模型对外名、头。
//!
//! 请求体的构造在 `nexus-gateway` 的 `kiro` 模块。对外报 `kiro-claude-*`，避免和 Cursor 的
//! `claude-*` 撞车；上游要的是剥掉前缀的名字。

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

/// `(对外名, 上游名)`。对外一律 `kiro-` 前缀。
pub const KIRO_MODELS: &[(&str, &str)] = &[
    ("kiro-claude-sonnet-4.5", "claude-sonnet-4.5"),
    ("kiro-claude-sonnet-4", "claude-sonnet-4"),
    ("kiro-claude-haiku-4.5", "claude-haiku-4.5"),
    ("kiro-claude-opus-4.5", "claude-opus-4.5"),
    ("kiro-claude-opus-4.1", "claude-opus-4.1"),
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
    KIRO_MODELS
        .iter()
        .any(|(ext, up)| ext.eq_ignore_ascii_case(&base) || up.eq_ignore_ascii_case(&base))
        || base.starts_with("kiro-claude-")
}

/// 发给 Amazon Q 的模型名。`kiro-claude-sonnet-4.5` → `claude-sonnet-4.5`。
pub fn upstream_model(model: &str) -> String {
    let (name, _) = split_route_prefix(model);
    let base = name.trim();
    for (ext, up) in KIRO_MODELS {
        if ext.eq_ignore_ascii_case(base) {
            return (*up).to_string();
        }
    }
    base.strip_prefix("kiro-")
        .unwrap_or(base)
        .trim()
        .to_string()
}

pub fn chat_headers(access_token: &str) -> Vec<(String, String)> {
    vec![
        ("authorization".into(), format!("Bearer {access_token}")),
        ("content-type".into(), "application/json".into()),
        (
            "accept".into(),
            "application/vnd.amazon.eventstream, application/json".into(),
        ),
        ("x-amzn-kiro-agent-mode".into(), "vibe".into()),
        ("x-amzn-codewhisperer-optout".into(), "true".into()),
    ]
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
        assert_eq!(
            upstream_model("kiro-claude-sonnet-4.5"),
            "claude-sonnet-4.5"
        );
        assert_eq!(upstream_model("kiro/claude-haiku-4.5"), "claude-haiku-4.5");
    }
}
