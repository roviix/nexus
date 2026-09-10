//! ChatGPT 账号的领域模型。
//!
//! 和 Cursor 的 `Account` 是两个类型、两张表（ARCHITECTURE §5.3）：持久化身份是 `(platform, external_id)`，
//! 这里的 external_id 是 `chatgpt_account_id`（`account_ref`）——同一个邮箱在 Cursor 和 ChatGPT 是两个
//! 账号，邮箱只是展示属性，甚至可能为空（纯 access_token 导入时读不到）。
//!
//! 结构体里**没有秘密**，只有 `has_refresh` 这类投影；凭证在 `SecretStore`。

use crate::protocol::CodexUsage;
use nexus_core::ChatGptAccountId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatGptStatus {
    /// 有 refresh token，上次刷新 / 使用没报凭证问题。
    Active,
    /// refresh token 被吊销 / 用重了 / 没有：要重新授权。
    NeedsLogin,
    /// 账号或工作区被停用。授权也救不回来，留着是让用户看见为什么。
    Dead,
}

impl ChatGptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ChatGptStatus::Active => "active",
            ChatGptStatus::NeedsLogin => "needs_login",
            ChatGptStatus::Dead => "dead",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "active" => ChatGptStatus::Active,
            "dead" => ChatGptStatus::Dead,
            _ => ChatGptStatus::NeedsLogin,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptAccount {
    pub id: ChatGptAccountId,
    /// `chatgpt_account_id`：上游侧的账号身份，推理请求头 `chatgpt-account-id` 就是它。
    pub account_ref: String,
    pub email: Option<String>,
    /// plus / pro / team / free …
    pub plan_type: Option<String>,
    pub status: ChatGptStatus,
    /// 进不进网关接力队。ChatGPT 账号在这个应用里只有一个用途——给本地网关出流量——
    /// 所以加进来默认就开着；用户想临时摘掉某个号时关它，比删了重授权轻得多。
    pub enabled: bool,
    pub note: Option<String>,
    pub usage: Option<CodexUsage>,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub has_refresh: bool,
    /// access token 的过期时刻（RFC 3339）。界面据它显示「几天后续期」。
    pub access_expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl ChatGptAccount {
    /// 给人看的名字：邮箱，没有就用账号 id 的尾巴。
    pub fn label(&self) -> String {
        self.email
            .clone()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| format!("chatgpt…{}", tail(&self.account_ref, 6)))
    }
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    chars[chars.len() - n..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_round_trips_and_unknown_means_needs_login() {
        for s in [
            ChatGptStatus::Active,
            ChatGptStatus::NeedsLogin,
            ChatGptStatus::Dead,
        ] {
            assert_eq!(ChatGptStatus::parse(s.as_str()), s);
        }
        assert_eq!(ChatGptStatus::parse("garbage"), ChatGptStatus::NeedsLogin);
    }

    #[test]
    fn label_prefers_email_and_falls_back_to_the_account_ref_tail() {
        let mut a = ChatGptAccount {
            id: ChatGptAccountId::new(),
            account_ref: "acct_0123456789abcdef".into(),
            email: Some("alice@example.com".into()),
            plan_type: None,
            status: ChatGptStatus::Active,
            enabled: true,
            note: None,
            usage: None,
            last_checked_at: None,
            last_error: None,
            has_refresh: true,
            access_expires_at: None,
            created_at: "t".into(),
            updated_at: "t".into(),
        };
        assert_eq!(a.label(), "alice@example.com");
        a.email = Some("  ".into());
        assert_eq!(a.label(), "chatgpt…abcdef");
    }
}
