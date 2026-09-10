//! Grok Build 的额度：`GET {cli-chat-proxy}/v1/billing?format=credits`。
//!
//! 这是官方 Grok Build 客户端 `/usage` 面板背后的那个接口（`xai-grok-shell/src/extensions/billing.rs`，
//! Apache-2.0）。响应形状：
//!
//! ```json
//! { "config": {
//!     "creditUsagePercent": 75.0,
//!     "currentPeriod": { "type": "USAGE_PERIOD_TYPE_WEEKLY", "start": "…", "end": "…" },
//!     "productUsage": [ { "product": "GrokBuild", "usagePercent": 75.0 } ],
//!     "isUnifiedBillingUser": true,
//!     "prepaidBalance": { "val": 0 }, "onDemandCap": { "val": 0 }, "onDemandUsed": { "val": 0 },
//!     "monthlyLimit": { "val": 2000 }, "used": { "val": 1800 }      // 老形状，兜底
//!   },
//!   "subscriptionTier": "SuperGrok" }
//! ```
//!
//! 三件事这里**不做**：不编造额度（拉不到就是未知）；不把「周额度用光」当成账号失效（它会重置）；
//! 不把「付费与否」从额度百分比里猜——档位从 `subscriptionTier` / `GET /v1/user` 来，猜不出就是未知。
//! 被动信号（`x-ratelimit-*` / `retry-after` 响应头）由网关那边在每次响应后写进同一份快照。

use crate::protocol::{self, CLI_CHAT_PROXY};
use nexus_core::{AppError, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// 到线的判据。留 0.5% 余量：到 100% 再换，最后那一两个请求会先撞一次错。
pub const QUOTA_LINE: f64 = 99.5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GrokQuota {
    /// 当前周期已用百分比（0–100）。
    pub credit_usage_percent: Option<f64>,
    /// `weekly` / `monthly` / 原文。
    pub period_type: Option<String>,
    pub period_start: Option<String>,
    pub period_end: Option<String>,
    /// 各产品拆分（`GrokBuild` / `API` …）。
    pub products: Vec<ProductUsage>,
    pub subscription_tier: Option<String>,
    /// 预付余额（分）。
    pub prepaid_balance_cents: Option<i64>,
    /// 最近一次成功响应头里的限流剩余（请求数 / token 数）。
    pub remaining_requests: Option<i64>,
    pub remaining_tokens: Option<i64>,
    /// 上游让我们等到什么时候（Unix 毫秒）。
    pub retry_after_ms: Option<i64>,
    pub checked_at: String,
    /// `billing` / `headers` / `probe`。
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProductUsage {
    pub product: String,
    pub usage_percent: Option<f64>,
}

impl GrokQuota {
    /// 周额度到线且周期还没过 → 现在别派它。周期过了快照就是旧的，不算。
    pub fn is_exhausted_now(&self) -> bool {
        let over = self.credit_usage_percent.is_some_and(|p| p >= QUOTA_LINE);
        if !over {
            return self.retry_after_ms.is_some_and(|at| at > now_ms());
        }
        match self
            .period_end
            .as_deref()
            .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok())
        {
            Some(end) => end > OffsetDateTime::now_utc(),
            None => true,
        }
    }

    /// 从 `/billing?format=credits` 的响应体建快照。老形状（`monthlyLimit` / `used`）兜底。
    pub fn from_billing(body: &Value) -> Self {
        let cfg = body.get("config").unwrap_or(body);
        let mut percent = cfg.get("creditUsagePercent").and_then(Value::as_f64);
        if percent.is_none() {
            let limit = cfg.pointer("/monthlyLimit/val").and_then(Value::as_f64);
            let used = cfg.pointer("/used/val").and_then(Value::as_f64);
            if let (Some(l), Some(u)) = (limit, used) {
                if l > 0.0 {
                    percent = Some((u / l * 100.0).clamp(0.0, 100.0));
                }
            }
        }
        let period = cfg.get("currentPeriod");
        let period_type = period
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .map(|t| {
                let t = t.to_ascii_lowercase();
                if t.contains("week") {
                    "weekly".to_string()
                } else if t.contains("month") {
                    "monthly".to_string()
                } else {
                    t
                }
            });
        let period_start = period
            .and_then(|p| p.get("start"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                cfg.get("billingPeriodStart")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        let period_end = period
            .and_then(|p| p.get("end"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                cfg.get("billingPeriodEnd")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        let products = cfg
            .get("productUsage")
            .and_then(Value::as_array)
            .map(|arr| {
                arr.iter()
                    .filter_map(|p| {
                        Some(ProductUsage {
                            product: p.get("product")?.as_str()?.to_string(),
                            usage_percent: p.get("usagePercent").and_then(Value::as_f64),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let tier = body
            .get("subscriptionTier")
            .or_else(|| body.get("subscription_tier"))
            .or_else(|| cfg.get("subscriptionTier"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        Self {
            credit_usage_percent: percent,
            period_type,
            period_start,
            period_end,
            products,
            subscription_tier: tier,
            prepaid_balance_cents: cfg.pointer("/prepaidBalance/val").and_then(Value::as_i64),
            remaining_requests: None,
            remaining_tokens: None,
            retry_after_ms: None,
            checked_at: nexus_core::now_iso(),
            source: "billing".into(),
        }
    }

    /// 把一次响应头里的被动信号并进来。没有头就什么都不动。
    pub fn absorb_headers(&mut self, headers: &[(String, String)]) -> bool {
        let mut touched = false;
        for (k, v) in headers {
            let k = k.to_ascii_lowercase();
            let n = v.trim().parse::<i64>().ok();
            match k.as_str() {
                "x-ratelimit-remaining-requests" => {
                    self.remaining_requests = n;
                    touched = true;
                }
                "x-ratelimit-remaining-tokens" => {
                    self.remaining_tokens = n;
                    touched = true;
                }
                "retry-after" => {
                    if let Some(secs) = n {
                        self.retry_after_ms = Some(now_ms() + secs.max(0) * 1000);
                        touched = true;
                    }
                }
                _ => {}
            }
        }
        if touched {
            self.checked_at = nexus_core::now_iso();
            if self.source != "billing" {
                self.source = "headers".into();
            }
        }
        touched
    }
}

/// 从档位名判断媒体资格。免费 / X Basic 在 Imagine 服务端是零额度（官方客户端也是据此直接不发请求）；
/// 认得出是付费档的给 `Some(true)`；认不出的给 `None`——让它去撞，撞回 402/403 再记。
pub fn media_eligibility_from_tier(tier: Option<&str>) -> Option<bool> {
    let t = tier?.trim().to_ascii_lowercase();
    if t.is_empty() {
        return None;
    }
    if t.contains("free") || t.contains("basic") || t == "none" {
        return Some(false);
    }
    if t.contains("supergrok")
        || t.contains("premium")
        || t.contains("heavy")
        || t.contains("pro")
        || t.contains("team")
    {
        return Some(true);
    }
    None
}

fn now_ms() -> i64 {
    OffsetDateTime::now_utc().unix_timestamp() * 1000
}

/// 拉一次额度。`user_id` 是 JWT `sub`（官方客户端带 `x-userid`）。
pub async fn fetch_billing(
    http: &reqwest::Client,
    access_token: &str,
    user_id: Option<&str>,
) -> Result<GrokQuota, AppError> {
    let url = format!(
        "{}/billing?format=credits",
        CLI_CHAT_PROXY.trim_end_matches('/')
    );
    let mut req = http.get(&url).timeout(Duration::from_secs(20));
    for (k, v) in protocol::cli_headers(access_token) {
        req = req.header(k, v);
    }
    if let Some(uid) = user_id.filter(|s| !s.trim().is_empty()) {
        req = req.header("x-userid", uid.trim());
    }
    let res = req
        .send()
        .await
        .map_err(|e| AppError::upstream(format!("拉 Grok 额度失败：{e}")))?;
    let status = res.status().as_u16();
    let body: Value = res
        .json()
        .await
        .map_err(|e| AppError::upstream(format!("Grok 额度响应不是 JSON：{e}")))?;
    match status {
        200..=299 => Ok(GrokQuota::from_billing(&body)),
        401 => Err(AppError::new(
            ErrorCode::Unauthorized,
            "Grok 额度接口 401：凭证失效",
        )),
        403 => Err(AppError::new(
            ErrorCode::Forbidden,
            "Grok 额度接口 403：这个号没有 Grok Build 权益",
        )),
        _ => Err(AppError::upstream(format!(
            "Grok 额度接口 HTTP {status}：{}",
            body.to_string().chars().take(200).collect::<String>()
        ))),
    }
}

/// `GET {cli-chat-proxy}/v1/user`：档位、邮箱。拉不到不算错——只是少一点信息。
pub async fn fetch_user(http: &reqwest::Client, access_token: &str) -> Option<Value> {
    let url = format!("{}/user", CLI_CHAT_PROXY.trim_end_matches('/'));
    let mut req = http.get(&url).timeout(Duration::from_secs(15));
    for (k, v) in protocol::cli_headers(access_token) {
        req = req.header(k, v);
    }
    let res = req.send().await.ok()?;
    if !res.status().is_success() {
        return None;
    }
    res.json().await.ok()
}

/// 从 `/v1/user` 的响应里挑档位。字段名不稳定，几种都认。
pub fn tier_from_user(user: &Value) -> Option<String> {
    for key in [
        "subscriptionTierDisplay",
        "subscription_tier_display",
        "subscriptionTier",
        "subscription_tier",
        "tier",
        "plan",
    ] {
        if let Some(s) = user
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return Some(s.to_string());
        }
    }
    user.pointer("/subscription/tier")
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_credits_shape_and_legacy_shape() {
        let q = GrokQuota::from_billing(&json!({
            "config": {
                "creditUsagePercent": 75.0,
                "currentPeriod": { "type": "USAGE_PERIOD_TYPE_WEEKLY", "start": "2026-09-07T00:00:00Z", "end": "2099-09-14T00:00:00Z" },
                "productUsage": [ { "product": "GrokBuild", "usagePercent": 75.0 } ],
                "prepaidBalance": { "val": 120 }
            },
            "subscriptionTier": "SuperGrok"
        }));
        assert_eq!(q.credit_usage_percent, Some(75.0));
        assert_eq!(q.period_type.as_deref(), Some("weekly"));
        assert_eq!(q.products.len(), 1);
        assert_eq!(q.subscription_tier.as_deref(), Some("SuperGrok"));
        assert_eq!(q.prepaid_balance_cents, Some(120));
        assert!(!q.is_exhausted_now());

        let legacy = GrokQuota::from_billing(&json!({
            "config": { "monthlyLimit": { "val": 2000 }, "used": { "val": 2000 },
                        "billingPeriodEnd": "2099-01-01T00:00:00Z" }
        }));
        assert_eq!(legacy.credit_usage_percent, Some(100.0));
        assert!(legacy.is_exhausted_now());
    }

    #[test]
    fn exhausted_only_while_the_period_is_still_running() {
        let q = GrokQuota {
            credit_usage_percent: Some(100.0),
            period_end: Some("2000-01-01T00:00:00Z".into()),
            ..Default::default()
        };
        assert!(!q.is_exhausted_now(), "周期早过了，快照是旧的");
    }

    #[test]
    fn headers_feed_the_passive_signals() {
        let mut q = GrokQuota::default();
        assert!(q.absorb_headers(&[
            ("X-RateLimit-Remaining-Requests".into(), "12".into()),
            ("retry-after".into(), "120".into()),
        ]));
        assert_eq!(q.remaining_requests, Some(12));
        assert!(q.is_exhausted_now(), "retry-after 还没到");
        assert!(!GrokQuota::default().absorb_headers(&[("content-type".into(), "x".into())]));
    }

    #[test]
    fn tier_decides_media_eligibility_only_when_it_is_clear() {
        assert_eq!(media_eligibility_from_tier(Some("Free")), Some(false));
        assert_eq!(media_eligibility_from_tier(Some("X Basic")), Some(false));
        assert_eq!(
            media_eligibility_from_tier(Some("SuperGrok Heavy")),
            Some(true)
        );
        assert_eq!(media_eligibility_from_tier(Some("weird")), None);
        assert_eq!(media_eligibility_from_tier(None), None);
    }
}
