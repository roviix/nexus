//! Qoder 账号领域模型。表里没有秘密。

use nexus_core::QoderAccountId;
use serde::{Deserialize, Serialize};

/// 哪一边的 Qoder。决定 openapi 和聊天网关的域名，也决定模型 key。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QoderBackend {
    /// `qoder.com` / `api3.qoder.sh`。
    Global,
    /// `qoder.com.cn` / `gateway.qoder.com.cn`。
    Cn,
}

impl QoderBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            QoderBackend::Global => "global",
            QoderBackend::Cn => "cn",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "cn" | "qoder-cn" | "china" | "国内" => QoderBackend::Cn,
            _ => QoderBackend::Global,
        }
    }

    pub fn product(self) -> &'static str {
        match self {
            QoderBackend::Global => "Qoder",
            QoderBackend::Cn => "Qoder CN",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QoderStatus {
    Active,
    NeedsLogin,
    Dead,
}

impl QoderStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            QoderStatus::Active => "active",
            QoderStatus::NeedsLogin => "needs_login",
            QoderStatus::Dead => "dead",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "active" => QoderStatus::Active,
            "dead" => QoderStatus::Dead,
            _ => QoderStatus::NeedsLogin,
        }
    }
}

/// 签名一个聊天请求时要的身份。token 本身在 `Credential` 里，不在这里。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QoderIdentity {
    pub backend: QoderBackend,
    pub user_id: String,
    pub name: String,
    pub email: String,
    pub machine_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QoderAccount {
    pub id: QoderAccountId,
    pub account_ref: String,
    pub label: String,
    pub backend: QoderBackend,
    /// 界面上的档位胶囊。国际 / 国内，不是上游的套餐名。
    pub plan_type: String,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub status: QoderStatus,
    pub enabled: bool,
    pub note: Option<String>,
    pub key_hint: Option<String>,
    pub has_token: bool,
    pub access_expires_at: Option<String>,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl QoderAccount {
    pub fn label(&self) -> String {
        self.label.clone()
    }

    pub fn has_credential(&self) -> bool {
        self.has_token && self.status == QoderStatus::Active
    }
}

pub fn compute_label(
    backend: QoderBackend,
    email: Option<&str>,
    hint: Option<&str>,
    user_id: Option<&str>,
) -> String {
    let product = backend.product();
    if let Some(email) = email.map(str::trim).filter(|e| !e.is_empty()) {
        return format!("{email} · {product}");
    }
    if let Some(hint) = hint.map(str::trim).filter(|h| !h.is_empty()) {
        return format!("{product} · {hint}");
    }
    let tail = user_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("?");
    let tail = tail
        .chars()
        .rev()
        .take(6)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{product} · {tail}")
}

/// PAT 的前 4 位和后 4 位。给人认哪张是哪张，不够复原这把 token。
pub fn hint_of(token: &str) -> Option<String> {
    let chars: Vec<char> = token.chars().collect();
    if chars.len() < 12 {
        return None;
    }
    let head: String = chars[..4].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    Some(format!("{head}…{tail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cn_and_global_are_different_products_under_one_email() {
        let a = compute_label(QoderBackend::Global, Some("a@b.c"), None, None);
        let b = compute_label(QoderBackend::Cn, Some("a@b.c"), None, None);
        assert_ne!(a, b);
        assert!(a.contains("Qoder"));
        assert!(b.contains("Qoder CN"));
    }

    #[test]
    fn hint_hides_the_middle_of_a_pat() {
        assert_eq!(hint_of("pt-abcdefghij").as_deref(), Some("pt-a…ghij"));
        assert!(hint_of("short").is_none());
    }
}
