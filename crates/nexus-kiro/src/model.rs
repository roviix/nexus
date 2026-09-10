//! Kiro 账号领域模型。表里没有秘密。

use nexus_core::KiroAccountId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KiroStatus {
    Active,
    NeedsLogin,
    Dead,
}

impl KiroStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            KiroStatus::Active => "active",
            KiroStatus::NeedsLogin => "needs_login",
            KiroStatus::Dead => "dead",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "active" => KiroStatus::Active,
            "dead" => KiroStatus::Dead,
            _ => KiroStatus::NeedsLogin,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroAccount {
    pub id: KiroAccountId,
    pub account_ref: String,
    pub email: Option<String>,
    pub plan_type: Option<String>,
    pub status: KiroStatus,
    pub enabled: bool,
    pub note: Option<String>,
    pub auth_method: Option<String>,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub has_refresh: bool,
    pub access_expires_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl KiroAccount {
    pub fn label(&self) -> String {
        self.email
            .clone()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| format!("kiro…{}", tail(&self.account_ref, 6)))
    }
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    chars[chars.len() - n..].iter().collect()
}
