//! ChatGPT 订阅账单快照。
//!
//! Codex OAuth 打不开 ChatGPT 网页的 Stripe 门户，所以这里**没有**标价、券、发票。
//! 能拿到的是 `accounts/check` / `subscriptions` 里的档位、是否在订、到期时刻、会不会续。
//! 字段缺席是 `None`（未知），不要写成「没有订阅 / 不会续费」。

use serde::{Deserialize, Serialize};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// 一份读回来的订阅快照。没有秘密。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptBilling {
    pub plan_type: Option<String>,
    /// 上游原始套餐名，例如 `chatgptplusplan`。界面优先用 [`Self::plan_type`]。
    pub subscription_plan: Option<String>,
    /// `None` = 没读到，不是「没订阅」。
    pub has_active_subscription: Option<bool>,
    /// RFC 3339。没有到期字段就是 `None`，不要编一个。
    pub expires_at: Option<String>,
    /// `None` = 没读到，不是「不续费」。
    pub will_renew: Option<bool>,
    pub billing_period: Option<String>,
    pub checked_at: String,
    /// `accounts/check` | `wham/accounts/check` | `subscriptions`。
    pub source: String,
}

impl ChatGptBilling {
    pub fn empty(now: OffsetDateTime, source: impl Into<String>) -> Self {
        Self {
            plan_type: None,
            subscription_plan: None,
            has_active_subscription: None,
            expires_at: None,
            will_renew: None,
            billing_period: None,
            checked_at: iso(now),
            source: source.into(),
        }
    }

    /// `GET /accounts/check/v4-2023-04-27`（以及同形状的 `/wham/accounts/check`、旧版
    /// `/accounts/check`）。按 `account_ref` 选组织；对不上再退 `default` / `is_default`。
    pub fn from_accounts_check(
        payload: &serde_json::Value,
        account_ref: &str,
        now: OffsetDateTime,
        source: impl Into<String>,
    ) -> Self {
        let mut out = Self::empty(now, source);
        if let Some(acc) = pick_account(payload, account_ref) {
            fill_from_account_node(&mut out, acc);
        }
        if let Some(plan) = payload.get("account_plan") {
            fill_from_account_plan(&mut out, plan);
        }
        fill_from_entitlement(&mut out, payload.get("entitlement").unwrap_or(payload));
        if out.plan_type.is_none() {
            out.plan_type = text(payload.get("plan_type"))
                .or_else(|| plan_from_subscription_name(out.subscription_plan.as_deref()));
        }
        out
    }

    /// `GET /subscriptions?account_id=`。只补缺的字段，不覆盖 check 已经读到的到期日。
    pub fn overlay_subscriptions(&mut self, payload: &serde_json::Value, now: OffsetDateTime) {
        let node = pick_subscription(payload).unwrap_or(payload);
        if self.expires_at.is_none() {
            self.expires_at = timestamp(
                node,
                &[
                    "active_until",
                    "expires_at",
                    "current_period_end",
                    "current_period_end_timestamp",
                ],
            );
        }
        if self.will_renew.is_none() {
            self.will_renew = flag(
                node,
                &["will_renew", "auto_renew", "renews", "is_auto_renew"],
            );
        }
        if self.has_active_subscription.is_none() {
            self.has_active_subscription = flag(
                node,
                &[
                    "has_active_subscription",
                    "is_active",
                    "active",
                    "is_paid_subscription_active",
                ],
            );
        }
        if self.plan_type.is_none() {
            self.plan_type = text(node.get("plan_type")).or_else(|| {
                plan_from_subscription_name(text(node.get("subscription_plan")).as_deref())
            });
        }
        if self.subscription_plan.is_none() {
            self.subscription_plan =
                text(node.get("subscription_plan")).or_else(|| text(node.get("plan_name")));
        }
        if self.billing_period.is_none() {
            self.billing_period = text(node.get("billing_period"))
                .or_else(|| text(node.get("interval")))
                .or_else(|| text(node.get("billing_cycle")));
        }
        if self.source.is_empty() {
            self.source = "subscriptions".into();
        }
        self.checked_at = iso(now);
    }
}

fn pick_account<'a>(
    payload: &'a serde_json::Value,
    account_ref: &str,
) -> Option<&'a serde_json::Value> {
    let accounts = payload.get("accounts")?;
    if let Some(obj) = accounts.as_object() {
        let wanted = account_ref.trim();
        if !wanted.is_empty() {
            if let Some(exact) = obj.get(wanted) {
                return Some(exact);
            }
            for v in obj.values() {
                let id = text(v.pointer("/account/account_id"))
                    .or_else(|| text(v.get("account_id")))
                    .or_else(|| text(v.pointer("/account/id")));
                if id.as_deref() == Some(wanted) {
                    return Some(v);
                }
            }
        }
        if let Some(default) = obj.get("default") {
            return Some(default);
        }
        for v in obj.values() {
            if v.pointer("/account/is_default").and_then(|x| x.as_bool()) == Some(true)
                || v.get("is_default").and_then(|x| x.as_bool()) == Some(true)
            {
                return Some(v);
            }
        }
        return obj.values().next();
    }
    if accounts.is_object() || accounts.is_array() {
        return None;
    }
    Some(accounts)
}

fn pick_subscription(payload: &serde_json::Value) -> Option<&serde_json::Value> {
    if let Some(arr) = payload.get("subscriptions").and_then(|v| v.as_array()) {
        return arr.first();
    }
    if let Some(arr) = payload.as_array() {
        return arr.first();
    }
    None
}

fn fill_from_account_node(out: &mut ChatGptBilling, acc: &serde_json::Value) {
    let account = acc.get("account").unwrap_or(acc);
    if out.plan_type.is_none() {
        out.plan_type = text(account.get("plan_type")).or_else(|| text(acc.get("plan_type")));
    }
    fill_from_entitlement(out, acc.get("entitlement").unwrap_or(acc));
    if out.plan_type.is_none() {
        out.plan_type = plan_from_subscription_name(out.subscription_plan.as_deref());
    }
}

fn fill_from_account_plan(out: &mut ChatGptBilling, plan: &serde_json::Value) {
    if out.has_active_subscription.is_none() {
        out.has_active_subscription = flag(
            plan,
            &[
                "is_paid_subscription_active",
                "has_active_subscription",
                "is_active",
            ],
        );
    }
    if out.subscription_plan.is_none() {
        out.subscription_plan = text(plan.get("subscription_plan"));
    }
    if out.expires_at.is_none() {
        out.expires_at = timestamp(
            plan,
            &[
                "subscription_expires_at_timestamp",
                "expires_at",
                "subscription_expires_at",
            ],
        );
    }
    if out.will_renew.is_none() {
        out.will_renew = flag(plan, &["will_renew", "auto_renew"]);
    }
    if out.plan_type.is_none() {
        out.plan_type = plan_from_subscription_name(out.subscription_plan.as_deref());
    }
}

fn fill_from_entitlement(out: &mut ChatGptBilling, ent: &serde_json::Value) {
    if out.has_active_subscription.is_none() {
        out.has_active_subscription = flag(
            ent,
            &[
                "has_active_subscription",
                "is_paid_subscription_active",
                "is_active",
            ],
        );
    }
    if out.subscription_plan.is_none() {
        out.subscription_plan =
            text(ent.get("subscription_plan")).or_else(|| text(ent.get("plan_name")));
    }
    if out.expires_at.is_none() {
        out.expires_at = timestamp(
            ent,
            &[
                "expires_at",
                "active_until",
                "subscription_expires_at",
                "subscription_expires_at_timestamp",
                "current_period_end",
            ],
        );
    }
    if out.will_renew.is_none() {
        out.will_renew = flag(
            ent,
            &["will_renew", "auto_renew", "renews", "is_auto_renew"],
        );
    }
    if out.billing_period.is_none() {
        out.billing_period = text(ent.get("billing_period")).or_else(|| text(ent.get("interval")));
    }
}

fn plan_from_subscription_name(raw: Option<&str>) -> Option<String> {
    let p = raw?.to_ascii_lowercase();
    if p.is_empty() {
        return None;
    }
    if p.contains("prolite") || p.contains("pro_lite") || p.contains("pro-lite") {
        return Some("prolite".into());
    }
    if p.contains("pro") {
        return Some("pro".into());
    }
    if p.contains("plus") {
        return Some("plus".into());
    }
    if p.contains("team") || p.contains("business") || p.contains("enterprise") {
        return Some("team".into());
    }
    if p.contains("free") {
        return Some("free".into());
    }
    None
}

fn text(v: Option<&serde_json::Value>) -> Option<String> {
    v.and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn flag(obj: &serde_json::Value, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|k| obj.get(*k).and_then(|v| v.as_bool()))
}

fn timestamp(obj: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| obj.get(*k).and_then(value_to_rfc3339))
}

fn value_to_rfc3339(v: &serde_json::Value) -> Option<String> {
    if let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) {
        if OffsetDateTime::parse(s, &Rfc3339).is_ok() {
            return Some(s.to_string());
        }
        if let Ok(n) = s.parse::<f64>() {
            return unix_to_rfc3339(n);
        }
        return None;
    }
    v.as_f64().and_then(unix_to_rfc3339)
}

fn unix_to_rfc3339(n: f64) -> Option<String> {
    if !n.is_finite() || n <= 0.0 {
        return None;
    }
    let secs = if n >= 1_000_000_000_000.0 {
        (n / 1000.0).round() as i64
    } else {
        n.round() as i64
    };
    OffsetDateTime::from_unix_timestamp(secs)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

fn iso(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_778_371_200).unwrap() // 2026-05-10
    }

    #[test]
    fn v4_check_picks_the_matching_account_and_leaves_missing_renew_unknown() {
        let payload = serde_json::json!({
            "accounts": {
                "org-expired-workspace": {
                    "account": { "account_id": "org-expired-workspace", "plan_type": "team", "is_default": true },
                    "entitlement": { "expires_at": "2020-01-01T00:00:00Z", "has_active_subscription": false }
                },
                "acct_plus": {
                    "account": { "account_id": "acct_plus", "plan_type": "plus" },
                    "entitlement": {
                        "has_active_subscription": true,
                        "subscription_plan": "chatgptplusplan",
                        "expires_at": "2026-10-01T00:00:00Z"
                    }
                }
            }
        });
        let b = ChatGptBilling::from_accounts_check(&payload, "acct_plus", now(), "accounts/check");
        assert_eq!(b.plan_type.as_deref(), Some("plus"));
        assert_eq!(b.subscription_plan.as_deref(), Some("chatgptplusplan"));
        assert_eq!(b.has_active_subscription, Some(true));
        assert_eq!(b.expires_at.as_deref(), Some("2026-10-01T00:00:00Z"));
        assert_eq!(b.will_renew, None, "没写会不会续，不是不会续");
        assert_eq!(b.source, "accounts/check");
    }

    #[test]
    fn old_account_plan_shape_and_unix_timestamp_still_parse() {
        let payload = serde_json::json!({
            "account_plan": {
                "is_paid_subscription_active": true,
                "subscription_plan": "chatgptproplan",
                "subscription_expires_at_timestamp": 1_790_812_800
            }
        });
        let b = ChatGptBilling::from_accounts_check(&payload, "acct_x", now(), "accounts/check");
        assert_eq!(b.plan_type.as_deref(), Some("pro"));
        assert_eq!(b.has_active_subscription, Some(true));
        assert_eq!(b.expires_at.as_deref(), Some("2026-10-01T00:00:00Z"));
    }

    #[test]
    fn absent_entitlement_stays_unknown_not_unsubscribed() {
        let b = ChatGptBilling::from_accounts_check(
            &serde_json::json!({ "accounts": { "default": { "account": { "plan_type": "plus" } } } }),
            "acct_x",
            now(),
            "accounts/check",
        );
        assert_eq!(b.plan_type.as_deref(), Some("plus"));
        assert_eq!(b.has_active_subscription, None);
        assert_eq!(b.expires_at, None);
        assert_eq!(b.will_renew, None);
    }

    #[test]
    fn subscriptions_only_fill_gaps() {
        let mut b = ChatGptBilling::from_accounts_check(
            &serde_json::json!({
                "accounts": {
                    "acct_1": {
                        "account": { "account_id": "acct_1", "plan_type": "pro" },
                        "entitlement": { "expires_at": "2026-10-01T00:00:00Z", "has_active_subscription": true }
                    }
                }
            }),
            "acct_1",
            now(),
            "accounts/check",
        );
        b.overlay_subscriptions(
            &serde_json::json!({
                "plan_type": "plus",
                "active_until": "2027-03-01T00:00:00Z",
                "will_renew": true,
                "billing_period": "monthly"
            }),
            now(),
        );
        assert_eq!(
            b.expires_at.as_deref(),
            Some("2026-10-01T00:00:00Z"),
            "check 已经有到期日，subscriptions 不能盖掉"
        );
        assert_eq!(b.will_renew, Some(true));
        assert_eq!(b.billing_period.as_deref(), Some("monthly"));
        assert_eq!(b.plan_type.as_deref(), Some("pro"));
        assert_eq!(b.source, "accounts/check");
    }

    #[test]
    fn milliseconds_and_prolite_names() {
        let b = ChatGptBilling::from_accounts_check(
            &serde_json::json!({
                "accounts": {
                    "default": {
                        "entitlement": {
                            "subscription_plan": "chatgptproliteplan",
                            "expires_at": 1_790_812_800_000i64
                        }
                    }
                }
            }),
            "",
            now(),
            "wham/accounts/check",
        );
        assert_eq!(b.plan_type.as_deref(), Some("prolite"));
        assert_eq!(b.expires_at.as_deref(), Some("2026-10-01T00:00:00Z"));
    }
}
