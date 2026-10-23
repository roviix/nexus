//! Qoder 的套餐和额度快照。
//!
//! 两路都问，合在一张快照里：
//! - `GET /api/v2/quota/usage`：套餐 Credits、加购、组织资源包；
//! - `GET /api/v3/user/status`：给人看的套餐名（`userTag` / `plan`）和重置日。
//!
//! 缺一路就用另一路。两路都失败才算这次没问到。凭证不进这张快照。

use crate::model::QoderBackend;
use crate::protocol::{self, CLIENT_TYPE, OPENAPI_COSY_VERSION};
use nexus_core::now_iso;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

const UA: &str = "QoderCLI/1.1.38";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QoderUsage {
    pub plan: String,
    pub user_type: Option<String>,
    /// 主额度已用百分比（0–100）。算不出来是 null，界面不要编一个 0。
    pub percent: Option<f64>,
    pub exceeded: bool,
    /// 团队 / 企业席位，没有个人计数。
    pub unlimited: bool,
    pub reset_at: Option<String>,
    pub unit: Option<String>,
    pub buckets: Vec<QoderBucket>,
    pub checked_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QoderBucket {
    /// `plan` / `addon` / `org`。
    pub id: String,
    pub label: String,
    pub used: Option<f64>,
    pub total: Option<f64>,
    pub remaining: Option<f64>,
    pub percent: Option<f64>,
    pub unit: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UsageError {
    pub auth: bool,
    pub message: String,
}

pub async fn fetch(
    http: &reqwest::Client,
    backend: QoderBackend,
    job_token: &str,
) -> Result<QoderUsage, UsageError> {
    let quota = get_json(http, &protocol::quota_url(backend), job_token).await;
    let status = get_json(http, &protocol::user_status_url(backend), job_token).await;
    let quota_v = quota.as_ref().ok().cloned();
    let status_v = status.as_ref().ok().cloned();
    if quota_v.is_none() && status_v.is_none() {
        return Err(quota.err().or(status.err()).unwrap_or(UsageError {
            auth: false,
            message: "没有问到 Qoder 用量。".into(),
        }));
    }
    Ok(parse_snapshot(
        quota_v.as_ref(),
        status_v.as_ref(),
        &now_iso(),
    ))
}

pub fn parse_snapshot(
    quota: Option<&Value>,
    status: Option<&Value>,
    checked_at: &str,
) -> QoderUsage {
    let quota = quota.map(body);
    let status = status.map(body);
    let user_type = text(status, &["userType", "user_type"])
        .or_else(|| text(quota, &["userType", "user_type"]));
    let mut buckets = Vec::new();
    if let Some(q) = quota {
        push_bucket(
            &mut buckets,
            "plan",
            "套餐额度",
            q.get("userQuota").or_else(|| q.get("user_quota")),
            &["total"],
        );
        push_bucket(
            &mut buckets,
            "addon",
            "加购额度",
            q.get("addOnQuota").or_else(|| q.get("add_on_quota")),
            &["total"],
        );
        push_bucket(
            &mut buckets,
            "org",
            "组织额度",
            q.get("orgResourcePackage")
                .or_else(|| q.get("org_resource_package")),
            &["cap", "total"],
        );
    }
    let exceeded = flag(status, &["isQuotaExceeded", "is_quota_exceeded"])
        || flag(quota, &["isQuotaExceeded", "is_quota_exceeded"]);
    let unlimited = is_pooled(user_type.as_deref())
        && !exceeded
        && !buckets.iter().any(|b| b.total.unwrap_or(0.0) > 0.0);
    let percent = buckets
        .iter()
        .find(|b| b.id == "plan")
        .and_then(|b| b.percent)
        .or_else(|| buckets.iter().find_map(|b| b.percent))
        .or_else(|| {
            overall_percent(
                quota.and_then(|q| num(q, &["totalUsagePercentage", "total_usage_percentage"])),
            )
        });
    let unit = buckets.iter().find_map(|b| b.unit.clone());
    let reset_at = quota
        .and_then(|q| time_of(q, &["expiresAt", "expires_at", "resetAt", "reset_at"]))
        .or_else(|| {
            status
                .and_then(|s| time_of(s, &["nextResetAt", "next_reset_at", "expiresAt", "resetAt"]))
        });
    QoderUsage {
        plan: plan_name(status, quota),
        user_type,
        percent,
        exceeded,
        unlimited,
        reset_at,
        unit,
        buckets,
        checked_at: checked_at.to_string(),
    }
}

fn push_bucket(
    out: &mut Vec<QoderBucket>,
    id: &str,
    label: &str,
    raw: Option<&Value>,
    total_keys: &[&str],
) {
    let Some(raw) = raw.filter(|v| v.is_object()) else {
        return;
    };
    let total = num(raw, total_keys);
    let used = num(raw, &["used"]);
    let remaining = num(raw, &["remaining"]);
    if total.is_none() && used.is_none() && remaining.is_none() {
        return;
    }
    let percent = match (used, total) {
        (Some(u), Some(t)) if t > 0.0 => Some((u / t * 100.0).clamp(0.0, 100.0)),
        _ => overall_percent(num(raw, &["percentage"])),
    };
    let unit = text(Some(raw), &["unit"]).filter(|s| !s.is_empty());
    out.push(QoderBucket {
        id: id.to_string(),
        label: label.to_string(),
        used,
        total,
        remaining,
        percent,
        unit,
    });
}

fn plan_name(status: Option<&Value>, quota: Option<&Value>) -> String {
    if let Some(tag) = text(status, &["userTag", "user_tag"]).filter(|s| !s.is_empty()) {
        return tag;
    }
    if let Some(plan) = text(
        status,
        &["plan", "planName", "plan_name", "subscriptionTitle"],
    )
    .filter(|s| !s.is_empty())
    {
        let pretty = prettify_plan(&plan);
        if !pretty.eq_ignore_ascii_case("qoder") {
            return pretty;
        }
    }
    match text(status, &["userType", "user_type"])
        .or_else(|| text(quota, &["userType", "user_type"]))
        .as_deref()
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("personal") | Some("individual") => "个人版".into(),
        Some("pro") | Some("professional") => "Pro".into(),
        Some("team") | Some("teams") => "团队版".into(),
        Some("enterprise") | Some("org") | Some("organization") => "企业版".into(),
        Some("free") => "免费版".into(),
        Some(other) => prettify_plan(other),
        None => "Qoder".into(),
    }
}

fn prettify_plan(raw: &str) -> String {
    let raw = raw.trim();
    let body = raw
        .strip_prefix("PLAN_TIER_")
        .or_else(|| raw.strip_prefix("plan_tier_"))
        .unwrap_or(raw);
    let mut out = String::new();
    let mut cap = true;
    for ch in body.chars() {
        if ch == '_' || ch == '-' {
            if !out.ends_with(' ') && !out.is_empty() {
                out.push(' ');
            }
            cap = true;
            continue;
        }
        if cap {
            out.extend(ch.to_uppercase());
            cap = false;
        } else {
            out.extend(ch.to_lowercase());
        }
    }
    let pretty = out.trim().to_string();
    if pretty.is_empty() {
        "Qoder".into()
    } else {
        pretty
    }
}

fn is_pooled(user_type: Option<&str>) -> bool {
    matches!(
        user_type.map(|s| s.to_ascii_lowercase()).as_deref(),
        Some("team") | Some("teams") | Some("enterprise") | Some("org") | Some("organization")
    )
}

/// 大于 1 的按 0–100 的百分比。`(0, 1]` 按比例（0.42 → 42）。恰好用尽时上游有时给 1。
fn overall_percent(raw: Option<f64>) -> Option<f64> {
    let p = raw.filter(|n| n.is_finite() && *n >= 0.0)?;
    Some(if p > 1.0 { p.min(100.0) } else { p * 100.0 })
}

fn body(v: &Value) -> &Value {
    v.get("data").filter(|d| d.is_object()).unwrap_or(v)
}

fn text(v: Option<&Value>, keys: &[&str]) -> Option<String> {
    let v = v?;
    for key in keys {
        if let Some(s) = v.get(*key).and_then(Value::as_str) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn flag(v: Option<&Value>, keys: &[&str]) -> bool {
    let Some(v) = v else {
        return false;
    };
    keys.iter()
        .any(|key| v.get(*key).and_then(Value::as_bool) == Some(true))
}

fn num(v: &Value, keys: &[&str]) -> Option<f64> {
    for key in keys {
        if let Some(n) = v.get(*key).and_then(as_num) {
            if n.is_finite() && n >= 0.0 {
                return Some(n);
            }
        }
    }
    None
}

fn as_num(v: &Value) -> Option<f64> {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn time_of(v: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(raw) = v.get(*key) {
            if let Some(formatted) = format_time(raw) {
                return Some(formatted);
            }
        }
    }
    None
}

fn format_time(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if OffsetDateTime::parse(s, &Rfc3339).is_ok() {
            return Some(s.to_string());
        }
        return s.parse::<f64>().ok().and_then(unix_to_rfc3339);
    }
    v.as_f64().and_then(unix_to_rfc3339)
}

fn unix_to_rfc3339(n: f64) -> Option<String> {
    if !n.is_finite() || n <= 0.0 {
        return None;
    }
    let secs = if n > 1e12 { n / 1000.0 } else { n };
    let dt = OffsetDateTime::from_unix_timestamp(secs as i64).ok()?;
    dt.format(&Rfc3339).ok()
}

async fn get_json(http: &reqwest::Client, url: &str, job_token: &str) -> Result<Value, UsageError> {
    let res = http
        .get(url)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {job_token}"),
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, UA)
        .header("Cosy-Version", OPENAPI_COSY_VERSION)
        .header("Cosy-ClientType", CLIENT_TYPE)
        .send()
        .await
        .map_err(|e| UsageError {
            auth: false,
            message: format!("连不上 Qoder：{e}"),
        })?;
    let status = res.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err(UsageError {
            auth: true,
            message: "Qoder 拒绝了这把登录态。".into(),
        });
    }
    if !status.is_success() {
        return Err(UsageError {
            auth: false,
            message: format!("用量接口返回 {}", status.as_u16()),
        });
    }
    let text = res.text().await.map_err(|e| UsageError {
        auth: false,
        message: format!("读用量响应失败：{e}"),
    })?;
    serde_json::from_str(&text).map_err(|_| UsageError {
        auth: false,
        message: "用量响应不是 JSON。".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn quota_and_status_become_one_snapshot() {
        let quota = json!({
            "userType": "personal",
            "userQuota": { "total": 3000, "used": 1260, "remaining": 1740, "percentage": 42, "unit": "Credits" },
            "addOnQuota": { "total": 500, "used": 10, "remaining": 490, "unit": "Credits" },
            "totalUsagePercentage": 0.42,
            "isQuotaExceeded": false,
            "expiresAt": 1_760_000_000_000_i64
        });
        let status = json!({
            "userType": "personal",
            "plan": "PLAN_TIER_PRO",
            "userTag": "Pro",
            "nextResetAt": "2026-11-01T00:00:00Z"
        });
        let snap = parse_snapshot(Some(&quota), Some(&status), "2026-10-11T00:00:00Z");
        assert_eq!(snap.plan, "Pro");
        assert_eq!(snap.user_type.as_deref(), Some("personal"));
        assert!((snap.percent.unwrap() - 42.0).abs() < 0.01);
        assert!(!snap.exceeded);
        assert!(!snap.unlimited);
        // 额度接口上的周期结束优先于 status 的重置日。
        assert_eq!(snap.reset_at.as_deref(), Some("2025-10-09T08:53:20Z"));
        assert_eq!(snap.buckets.len(), 2);
        assert_eq!(snap.buckets[0].id, "plan");
        assert_eq!(snap.buckets[0].remaining, Some(1740.0));
        assert_eq!(snap.buckets[1].id, "addon");
    }

    #[test]
    fn plan_tier_is_prettified_when_there_is_no_tag() {
        let status = json!({ "plan": "PLAN_TIER_PRO", "userType": "personal" });
        let snap = parse_snapshot(None, Some(&status), "2026-10-11T00:00:00Z");
        assert_eq!(snap.plan, "Pro");
    }

    #[test]
    fn org_package_uses_cap_and_a_wrapped_body() {
        let quota = json!({
            "data": {
                "orgResourcePackage": { "cap": 50000, "used": 8200, "remaining": 41800, "unit": "Credits" },
                "userType": "teams"
            }
        });
        let snap = parse_snapshot(Some(&quota), None, "2026-10-11T00:00:00Z");
        assert_eq!(snap.plan, "团队版");
        assert_eq!(snap.buckets.len(), 1);
        assert_eq!(snap.buckets[0].id, "org");
        assert_eq!(snap.buckets[0].total, Some(50000.0));
        assert!(!snap.unlimited);
        assert!((snap.percent.unwrap() - 16.4).abs() < 0.01);
    }

    #[test]
    fn a_pooled_seat_without_a_personal_counter_is_unlimited() {
        let status =
            json!({ "userType": "enterprise", "userTag": "企业版", "isQuotaExceeded": false });
        let snap = parse_snapshot(None, Some(&status), "2026-10-11T00:00:00Z");
        assert!(snap.unlimited);
        assert!(snap.buckets.is_empty());
        assert_eq!(snap.plan, "企业版");
    }

    #[test]
    fn a_fractional_overall_percentage_is_scaled() {
        let quota = json!({ "totalUsagePercentage": 0.42, "userType": "free" });
        let snap = parse_snapshot(Some(&quota), None, "2026-10-11T00:00:00Z");
        assert_eq!(snap.plan, "免费版");
        assert!((snap.percent.unwrap() - 42.0).abs() < 0.01);
    }
}
