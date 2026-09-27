//! Claude 账号。表里没有 token。

use nexus_core::ClaudeAccountId;
use serde::Serialize;

use crate::protocol::ClaudeQuota;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeAuthMode {
    /// Pro / Max / Team 的 OAuth，有 refresh。
    Oauth,
    /// `claude setup-token` 出来的长期推理票，没有 refresh。
    SetupToken,
    /// Console API Key。额度不在订阅接口里。
    ApiKey,
}

impl ClaudeAuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaudeAuthMode::Oauth => "oauth",
            ClaudeAuthMode::SetupToken => "setup_token",
            ClaudeAuthMode::ApiKey => "api_key",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "setup_token" => ClaudeAuthMode::SetupToken,
            "api_key" => ClaudeAuthMode::ApiKey,
            _ => ClaudeAuthMode::Oauth,
        }
    }

    /// 网关出站怎么带这把票。setup-token 和 OAuth 都是 Bearer。
    pub fn uses_bearer(self) -> bool {
        !matches!(self, ClaudeAuthMode::ApiKey)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeStatus {
    Active,
    NeedsLogin,
    Dead,
}

impl ClaudeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ClaudeStatus::Active => "active",
            ClaudeStatus::NeedsLogin => "needs_login",
            ClaudeStatus::Dead => "dead",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "active" => ClaudeStatus::Active,
            "dead" => ClaudeStatus::Dead,
            _ => ClaudeStatus::NeedsLogin,
        }
    }
}

/// 给账号卡片的额度。形状跟 Grok 那张卡读的字段对齐，这样不用再做一套卡片。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCard {
    pub credit_usage_percent: Option<f64>,
    pub period_type: Option<String>,
    pub period_start: Option<String>,
    pub period_end: Option<String>,
    pub products: Vec<serde_json::Value>,
    pub subscription_tier: Option<String>,
    pub prepaid_balance_cents: Option<i64>,
    pub remaining_requests: Option<i64>,
    pub remaining_tokens: Option<i64>,
    pub retry_after_ms: Option<i64>,
    pub checked_at: String,
    pub source: String,
}

impl UsageCard {
    pub fn from_quota(quota: &ClaudeQuota, plan: Option<&str>, checked_at: &str) -> Self {
        let (pct, period, reset) = quota.tighter();
        Self {
            credit_usage_percent: Some(pct as f64),
            period_type: Some(period.to_string()),
            period_start: None,
            period_end: reset.and_then(rfc3339_secs),
            products: Vec::new(),
            subscription_tier: plan.map(str::to_string),
            prepaid_balance_cents: None,
            remaining_requests: None,
            remaining_tokens: None,
            retry_after_ms: None,
            checked_at: checked_at.to_string(),
            source: "oauth_usage".into(),
        }
    }
}

fn rfc3339_secs(secs: i64) -> Option<String> {
    let dt = time::OffsetDateTime::from_unix_timestamp(secs).ok()?;
    dt.format(&time::format_description::well_known::Rfc3339)
        .ok()
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAccount {
    pub id: ClaudeAccountId,
    pub account_ref: String,
    pub label: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub auth_mode: ClaudeAuthMode,
    /// 卡片上的「API Key」标只认这一个取值。setup-token 仍算订阅票。
    pub auth_kind: &'static str,
    pub status: ClaudeStatus,
    pub enabled: bool,
    pub note: Option<String>,
    pub key_hint: Option<String>,
    pub has_refresh: bool,
    pub has_token: bool,
    pub has_api_key: bool,
    pub usage: Option<UsageCard>,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStart {
    pub login_id: String,
    pub authorize_url: String,
    /// 本机 54545 已经在听：同意后会自动完成。false = 端口被占，要把地址栏贴回来。
    pub callback_listening: bool,
}

/// 等本机回调时推给界面的状态。
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
        account: ClaudeAccount,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub accounts: Vec<ClaudeAccount>,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientProbe {
    pub present: bool,
    pub path: String,
    /// 文件不在时，导入还会再试的位置。macOS 上是登录钥匙串里的 Claude Code 凭证。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub also: Option<String>,
}
