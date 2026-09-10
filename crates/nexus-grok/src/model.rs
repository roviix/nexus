//! Grok 账号领域模型。表里没有秘密。

use crate::quota::GrokQuota;
use nexus_core::GrokAccountId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokStatus {
    Active,
    NeedsLogin,
    Dead,
}

impl GrokStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            GrokStatus::Active => "active",
            GrokStatus::NeedsLogin => "needs_login",
            GrokStatus::Dead => "dead",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "active" => GrokStatus::Active,
            "dead" => GrokStatus::Dead,
            _ => GrokStatus::NeedsLogin,
        }
    }
}

/// 号是哪一种凭证。两种走的上游不一样：订阅号走 `cli-chat-proxy.grok.com`（订阅额度），
/// API Key 走 `api.x.ai`（按 token 计费）；媒体两种都走 `api.x.ai`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrokAuthKind {
    Oauth,
    ApiKey,
}

impl GrokAuthKind {
    pub fn as_str(self) -> &'static str {
        match self {
            GrokAuthKind::Oauth => "oauth",
            GrokAuthKind::ApiKey => "api_key",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "api_key" => GrokAuthKind::ApiKey,
            _ => GrokAuthKind::Oauth,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokAccount {
    pub id: GrokAccountId,
    /// OAuth：JWT `sub`；API Key：`GET /v1/api-key` 回的 `api_key_id`（没有就用 key 的指纹）。
    pub account_ref: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub status: GrokStatus,
    pub enabled: bool,
    pub note: Option<String>,
    pub auth_kind: GrokAuthKind,
    /// 订阅档位（`SuperGrok` / `SuperGrok Heavy` / `Free` …），来自额度探测。
    pub subscription_tier: Option<String>,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    /// OAuth 号：有 refresh token；API Key 号：有 key。都是「还能自己拿到凭证」的意思。
    pub has_refresh: bool,
    pub access_expires_at: Option<String>,
    /// 额度快照（周额度百分比、周期、档位）。
    pub usage: Option<GrokQuota>,
    /// 自动探测出的媒体资格：`None` 还没探过 / 探不出来。
    pub media_probe: Option<bool>,
    /// 用户手动覆盖。`Some(_)` 时以它为准。
    pub media_override: Option<bool>,
    /// 生效的媒体资格：覆盖 > 探测。`None` = 未知，让它去撞一次。
    pub media_eligible: Option<bool>,
    pub created_at: String,
    pub updated_at: String,
}

impl GrokAccount {
    pub fn label(&self) -> String {
        self.email
            .clone()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| match self.auth_kind {
                GrokAuthKind::ApiKey => format!("xai-key…{}", tail(&self.account_ref, 6)),
                GrokAuthKind::Oauth => format!("grok…{}", tail(&self.account_ref, 6)),
            })
    }

    /// 能进接力队：开着、没被判失效、还能自己拿到凭证。
    pub fn usable(&self) -> bool {
        self.enabled && self.status == GrokStatus::Active && self.has_refresh
    }
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    chars[chars.len() - n..].iter().collect()
}
