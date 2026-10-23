//! Kiro 账号领域模型。表里没有秘密。

use crate::usage::KiroUsage;
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
    pub profile_arn: Option<String>,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub has_refresh: bool,
    pub access_expires_at: Option<String>,
    /// 上次问到的套餐和额度。没拉过是 `None`。
    /// 序列化成 `kiroUsage`，别和 Grok 的 `usage` 挤在同一个前端类型上。
    #[serde(rename = "kiroUsage")]
    pub usage: Option<KiroUsage>,
    /// 显示名，也是网关接力队的键。仓库按整张表算好（见 [`assign_labels`]），前端直接用，
    /// 别再自己拼。
    pub label: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroSwitchOutcome {
    pub success: bool,
    pub account_id: String,
    pub label: String,
    pub kiro_running: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroActiveInfo {
    pub account_id: Option<String>,
    pub email: Option<String>,
    pub label: Option<String>,
    pub profile_arn: Option<String>,
    pub auth_method: Option<String>,
    pub provider: Option<String>,
}

impl KiroAccount {
    pub fn label(&self) -> String {
        self.label.clone()
    }

    /// 只看这一行能起的名字：邮箱，没有邮箱就用身份的尾巴。
    fn own_label(&self) -> String {
        self.email
            .clone()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| format!("kiro…{}", tail(&self.account_ref, 6)))
    }

    /// 存下来的 profileArn。v21 以前导入的号只把它记在 `account_ref` 上。
    pub fn stored_profile_arn(&self) -> Option<String> {
        self.profile_arn
            .clone()
            .filter(|s| s.starts_with("arn:"))
            .or_else(|| {
                self.account_ref
                    .starts_with("arn:")
                    .then(|| self.account_ref.clone())
            })
    }
}

/// 给整张表起名字。`rows` 要按建号先后排好。
///
/// 通常就是邮箱。同一个邮箱可以是两个 Kiro 号（Builder ID 和用它登录的 Google / GitHub），
/// 只写邮箱的话接力队会把两个号当成一个，出站还可能带上另一行的 profileArn 和机器码。
/// 撞了的话先建的那行留着邮箱，后建的带上身份尾巴。
pub(crate) fn assign_labels(rows: &mut [KiroAccount]) {
    let mut taken = std::collections::HashSet::new();
    for a in rows.iter_mut() {
        let own = a.own_label();
        let label = [
            own.clone(),
            format!("{own} · {}", tail(&a.account_ref, 6)),
            format!("{own} · {}", a.id.as_str()),
        ]
        .into_iter()
        .find(|l| !taken.contains(&l.to_lowercase()))
        .unwrap_or_else(|| format!("{own} · {}", a.id.as_str()));
        taken.insert(label.to_lowercase());
        a.label = label;
    }
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    chars[chars.len() - n..].iter().collect()
}
