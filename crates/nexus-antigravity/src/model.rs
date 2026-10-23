//! Google 反重力 (Antigravity) 账号领域模型。
//!
//! 遵循 Nexus 架构（ARCHITECTURE §5.3）：
//! 业务表只存元信息与投影，不存明文凭证；OAuth 凭证只在 `secrets` 表。

use nexus_core::AntigravityAccountId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AntigravityStatus {
    Active,
    NeedsLogin,
    Dead,
}

impl AntigravityStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            AntigravityStatus::Active => "active",
            AntigravityStatus::NeedsLogin => "needs_login",
            AntigravityStatus::Dead => "dead",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "active" => AntigravityStatus::Active,
            "dead" => AntigravityStatus::Dead,
            _ => AntigravityStatus::NeedsLogin,
        }
    }
}

/// `retrieveUserQuotaSummary` 里的一个时间窗（5h 或 weekly）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaWindow {
    pub window: String,
    pub remaining_fraction: f64,
    pub percent_remaining: u32,
    pub reset_time: Option<String>,
}

/// 一组模型共享的配额窗。Gemini 一组，Claude / GPT 一组。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuotaGroup {
    pub display_name: String,
    pub buckets: Vec<QuotaWindow>,
}

/// 各核心模型的配额分桶快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelQuotaBucket {
    pub model_id: String,
    pub display_name: String,
    pub remaining_fraction: f64,
    pub percent_remaining: u32,
    pub percent_used: u32,
    pub reset_time: Option<String>,
}

/// 额度与套餐用量快照。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityUsage {
    /// 绑定的 GCP 项目 ID。
    pub project_id: Option<String>,
    /// 套餐级别（Free / Google AI Pro / Google AI Ultra / Enterprise）。
    pub tier: Option<String>,
    /// 套餐名称显示（例如 "Google AI Pro"、"Google AI Ultra"）。
    pub tier_name: Option<String>,
    /// 套餐描述或升级提示文案。
    pub tier_description: Option<String>,
    /// 主模型使用百分比 (0.0 - 100.0)。
    pub percent_used: Option<f64>,
    /// 主模型剩余百分比 (0.0 - 100.0)。
    pub percent_remaining: Option<f64>,
    /// 剩余可用请求数（如有明确配额返回）。
    pub remaining_requests: Option<i64>,
    /// 重置时间戳（RFC 3339）。
    pub resets_at: Option<String>,
    /// 各核心模型的分桶配额明细。`fetchAvailableModels` 的 remainingFraction 经常恒为 1，
    /// 账号是否到线以 [`Self::quota_groups`] 的 5h / weekly 窗为准。
    #[serde(default)]
    pub model_buckets: Vec<ModelQuotaBucket>,
    /// 按模型组的 5 小时窗和周窗。
    #[serde(default)]
    pub quota_groups: Vec<QuotaGroup>,
    /// 最近一次调用状态。`forbidden` 表示账号被上游拒绝，不再参与调度。
    pub last_status: Option<String>,
    /// 上次刷新时间（RFC 3339）。
    pub checked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityAccount {
    pub id: AntigravityAccountId,
    /// 平台身份标识，通常为 Google 邮箱。
    pub account_ref: String,
    pub email: Option<String>,
    pub project_id: Option<String>,
    pub status: AntigravityStatus,
    pub enabled: bool,
    pub note: Option<String>,
    pub tier: Option<String>,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub has_refresh: bool,
    pub access_expires_at: Option<String>,
    #[serde(rename = "antigravityUsage")]
    pub usage: Option<AntigravityUsage>,
    /// 展示标签，也是 Lane 接力队的主键。
    pub label: String,
    pub created_at: String,
    pub updated_at: String,
}

impl AntigravityAccount {
    pub fn label(&self) -> String {
        self.label.clone()
    }

    fn own_label(&self) -> String {
        self.email
            .clone()
            .filter(|e| !e.trim().is_empty())
            .unwrap_or_else(|| {
                if self.account_ref.contains('@') {
                    self.account_ref.clone()
                } else {
                    format!("antigravity…{}", tail(&self.account_ref, 6))
                }
            })
    }
}

/// `loadCodeAssist` 的档位 id / 名字收成 FREE / PRO / ULTRA。认不出就保持原文，调用方再决定。
pub fn normalize_subscription_tier(tier: &str) -> String {
    let lower = tier.trim().to_lowercase();
    if lower.is_empty() {
        return String::new();
    }
    if lower.contains("ultra") || lower.contains("helium") {
        return "ULTRA".to_string();
    }
    if lower.contains("free") || lower.contains("starter") {
        return "FREE".to_string();
    }
    if lower.contains("pro") || lower.contains("premium") || lower.contains("advanced") {
        return "PRO".to_string();
    }
    tier.trim().to_string()
}

pub fn tier_display_name(tier: &str) -> String {
    match normalize_subscription_tier(tier).as_str() {
        "ULTRA" => "Google AI Ultra".to_string(),
        "PRO" => "Google AI Pro".to_string(),
        "FREE" => "免费体验版".to_string(),
        other => other.to_string(),
    }
}

fn tail(s: &str, n: usize) -> &str {
    if s.len() <= n {
        s
    } else {
        &s[s.len() - n..]
    }
}

/// 为账号列表分配无冲突的唯一显示标签。
pub(crate) fn assign_labels(rows: &mut [AntigravityAccount]) {
    let mut taken = std::collections::HashSet::new();
    for a in rows.iter_mut() {
        let own = a.own_label();
        let label = [
            own.clone(),
            format!("{own} · {}", tail(&a.account_ref, 6)),
            format!("{own} · {}", a.id.as_str()),
        ]
        .into_iter()
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or_else(|| format!("{own} · {}", a.id.as_str()));
        taken.insert(label.clone());
        a.label = label;
    }
}

/// 发起 OAuth 授权的返回结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginStart {
    pub login_id: String,
    pub authorize_url: String,
    pub callback_listening: bool,
}

/// 登录进行中的状态事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        account: Box<AntigravityAccount>,
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

/// 本机客户端探测结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientProbe {
    pub present: bool,
    pub path: String,
    pub also: Option<String>,
}

/// 批量导入结果。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub imported: usize,
    pub updated: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
}
