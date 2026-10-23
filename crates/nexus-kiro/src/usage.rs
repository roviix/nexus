//! `getUsageLimits` 的快照。卡片上的套餐、基础额度、试用和重置日都从这里来。
//!
//! 合计用量把激活的试用和奖励算进去；基础额度单独留着，界面要分行写。

use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KiroUsage {
    pub subscription_title: Option<String>,
    pub email: Option<String>,
    /// `userInfo.userId`。社交登录和 Builder ID 的 token 里读不出是谁，账号身份靠它。
    #[serde(default)]
    pub user_id: Option<String>,
    pub base_used: f64,
    pub base_limit: f64,
    pub used: f64,
    pub limit: f64,
    pub percent: f64,
    pub reset_at: Option<String>,
    pub trial_used: Option<f64>,
    pub trial_limit: Option<f64>,
    pub trial_expires_at: Option<String>,
    pub bonus_used: Option<f64>,
    pub bonus_limit: Option<f64>,
    pub overage_enabled: Option<bool>,
    pub checked_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LimitsBody {
    #[serde(default)]
    next_date_reset: Option<f64>,
    #[serde(default)]
    subscription_info: Option<SubInfo>,
    #[serde(default)]
    usage_breakdown_list: Vec<Breakdown>,
    #[serde(default)]
    overage_configuration: Option<OverageCfg>,
    #[serde(default)]
    user_info: Option<UserInfo>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubInfo {
    #[serde(default)]
    subscription_title: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct OverageCfg {
    #[serde(default)]
    overage_enabled: Option<bool>,
    #[serde(default)]
    overage_status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserInfo {
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Breakdown {
    #[serde(default)]
    current_usage_with_precision: f64,
    #[serde(default)]
    usage_limit_with_precision: f64,
    #[serde(default)]
    next_date_reset: Option<f64>,
    #[serde(default)]
    bonuses: Vec<Bonus>,
    #[serde(default)]
    free_trial_info: Option<Trial>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Bonus {
    #[serde(default)]
    current_usage: f64,
    #[serde(default)]
    usage_limit: f64,
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Trial {
    #[serde(default)]
    current_usage_with_precision: f64,
    #[serde(default)]
    usage_limit_with_precision: f64,
    #[serde(default)]
    free_trial_expiry: Option<f64>,
    #[serde(default)]
    free_trial_status: Option<String>,
}

pub fn parse_usage(body: &Value) -> Result<KiroUsage, String> {
    let parsed: LimitsBody =
        serde_json::from_value(body.clone()).map_err(|e| format!("用量响应无法解析：{e}"))?;
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".into());
    let breakdown = parsed.usage_breakdown_list.first();
    let base_used = breakdown
        .map(|b| b.current_usage_with_precision)
        .unwrap_or(0.0);
    let base_limit = breakdown
        .map(|b| b.usage_limit_with_precision)
        .unwrap_or(0.0);
    let mut used = base_used;
    let mut limit = base_limit;
    let mut trial_used = None;
    let mut trial_limit = None;
    let mut trial_expires_at = None;
    if let Some(trial) = breakdown.and_then(|b| b.free_trial_info.as_ref()) {
        if trial
            .free_trial_status
            .as_deref()
            .is_some_and(|s| s.eq_ignore_ascii_case("ACTIVE"))
        {
            used += trial.current_usage_with_precision;
            limit += trial.usage_limit_with_precision;
            trial_used = Some(trial.current_usage_with_precision);
            trial_limit = Some(trial.usage_limit_with_precision);
            trial_expires_at = trial.free_trial_expiry.and_then(unix_rfc3339);
        }
    }
    let mut bonus_used_sum = 0.0;
    let mut bonus_limit_sum = 0.0;
    let mut bonus_any = false;
    if let Some(breakdown) = breakdown {
        for bonus in &breakdown.bonuses {
            if bonus
                .status
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("ACTIVE"))
            {
                bonus_any = true;
                bonus_used_sum += bonus.current_usage;
                bonus_limit_sum += bonus.usage_limit;
            }
        }
    }
    if bonus_any {
        used += bonus_used_sum;
        limit += bonus_limit_sum;
    }
    let percent = if limit > 0.0 {
        (used / limit * 100.0).clamp(0.0, 999.0)
    } else {
        0.0
    };
    let reset = breakdown
        .and_then(|b| b.next_date_reset)
        .or(parsed.next_date_reset)
        .and_then(unix_rfc3339);
    let (email, user_id) = parsed
        .user_info
        .map(|u| (non_blank(u.email), non_blank(u.user_id)))
        .unwrap_or((None, None));
    let overage_enabled = parsed.overage_configuration.and_then(|cfg| {
        cfg.overage_enabled.or_else(|| {
            cfg.overage_status
                .map(|s| s.eq_ignore_ascii_case("ENABLED"))
        })
    });
    Ok(KiroUsage {
        subscription_title: parsed
            .subscription_info
            .and_then(|s| s.subscription_title)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        email,
        user_id,
        base_used,
        base_limit,
        used,
        limit,
        percent,
        reset_at: reset,
        trial_used,
        trial_limit,
        trial_expires_at,
        bonus_used: bonus_any.then_some(bonus_used_sum),
        bonus_limit: bonus_any.then_some(bonus_limit_sum),
        overage_enabled,
        checked_at: now,
    })
}

fn non_blank(s: Option<String>) -> Option<String> {
    s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn unix_rfc3339(ts: f64) -> Option<String> {
    let secs = ts.round() as i64;
    OffsetDateTime::from_unix_timestamp(secs)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base_quota_trial_and_bonus_add_up() {
        let usage = parse_usage(&json!({
            "nextDateReset": 1_790_812_800.0,
            "subscriptionInfo": { "subscriptionTitle": "KIRO PRO+" },
            "overageConfiguration": { "overageStatus": "DISABLED" },
            "userInfo": { "email": "developer@example.invalid" },
            "usageBreakdownList": [{
                "currentUsageWithPrecision": 92.0,
                "usageLimitWithPrecision": 10000.0,
                "nextDateReset": 1_790_812_800.0,
                "freeTrialInfo": {
                    "currentUsageWithPrecision": 10.0,
                    "usageLimitWithPrecision": 500.0,
                    "freeTrialStatus": "ACTIVE",
                    "freeTrialExpiry": 1_790_812_800.0
                },
                "bonuses": [{ "currentUsage": 3.0, "usageLimit": 100.0, "status": "ACTIVE" }]
            }]
        }))
        .unwrap();
        assert_eq!(usage.subscription_title.as_deref(), Some("KIRO PRO+"));
        assert_eq!(usage.email.as_deref(), Some("developer@example.invalid"));
        assert_eq!(usage.base_used, 92.0);
        assert_eq!(usage.base_limit, 10000.0);
        assert_eq!(usage.used, 105.0);
        assert_eq!(usage.limit, 10600.0);
        assert!((usage.percent - 105.0 / 10600.0 * 100.0).abs() < 0.001);
        assert!(usage.reset_at.as_deref().unwrap().starts_with("2026-10-01"));
        assert_eq!(usage.overage_enabled, Some(false));
        assert_eq!(usage.trial_limit, Some(500.0));
        assert_eq!(usage.bonus_used, Some(3.0));
    }
}
