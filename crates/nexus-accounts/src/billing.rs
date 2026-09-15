//! Cursor 个人订阅的 Stripe 账单快照。
//!
//! dashboard 的 `/api/auth/stripe` 没有优惠券。真实标价、券、发票只出现在
//! Customer Portal：用已托管的 session cookie 向 `stripeSession` 换一扇临时门，
//! 再用门户自己的 ephemeral key 读门户范围内的订阅 / 发票。
//!
//! **不变量**：门户 URL、`secret=`、`ek_live_` / `ek_test_` 只活在这次请求的栈上，
//! 不写库、不进日志、不进序列化结构。页面改了、字段缺席时标「未知」，
//! 不要把「没读到」写成「没有折扣」。

use crate::usage::{self, session_cookie};
use nexus_core::{now_iso, AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
const STRIPE_SESSION_URL: &str = "https://cursor.com/api/stripeSession";
const STRIPE_API: &str = "https://api.stripe.com";
/// 门户 ephemeral key 认的版本。换了会 400，测试里用夹具，不依赖线上。
const STRIPE_VERSION: &str = "2026-08-26.dahlia";
const PORTAL_HOST: &str = "billing.stripe.com";
const INVOICE_LIMIT: usize = 12;
const LINE_LIMIT: usize = 8;

/// 当前订阅上的折扣结论。
///
/// `unknown` 是「没读到」，不是「没有」。只有门户订阅对象完整回来，
/// 才能把空的 `discount` 写成 `none`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DiscountState {
    #[default]
    Unknown,
    None,
    Active,
    Expired,
}

impl DiscountState {
    pub fn as_str(self) -> &'static str {
        match self {
            DiscountState::Unknown => "unknown",
            DiscountState::None => "none",
            DiscountState::Active => "active",
            DiscountState::Expired => "expired",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingDiscount {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<DiscountState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent_off: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount_off: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// `once` / `repeating` / `forever`，原样保留 Stripe 的写法。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_in_months: Option<i64>,
    /// 绑到订阅上的起止，epoch ms。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub starts_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingItem {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_amount: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingInvoiceLine {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quantity: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingInvoice {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subtotal: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount_due: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount_paid: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amount_remaining: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_end: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discounts: Vec<BillingDiscount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<BillingInvoiceLine>,
}

/// 一次门户读取的结果。里面没有任何 URL / 密钥。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountBilling {
    pub fetched_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_period_start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_period_end: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cancel_at_period_end: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canceled_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<BillingItem>,
    /// Stripe 最小货币单位（美元是美分，日元是日元）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub list_price: Option<i64>,
    /// 把当前订阅上的折扣算进去之后的应付；没有折扣就等于标价。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_amount: Option<i64>,
    pub discount_state: DiscountState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discount: Option<BillingDiscount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invoices: Vec<BillingInvoice>,
}

/// 拉一个号的订阅账单。门户密钥出了这个函数就不存在。
pub async fn fetch(http: &reqwest::Client, session_token: &str) -> Result<AccountBilling> {
    let token = session_token.trim();
    if !token.starts_with("user_") || !token.contains("::") {
        return Err(AppError::unauthorized(
            "session token 格式不对，应为 user_xxx::<jwt>。",
        ));
    }
    let cookie = session_cookie(token);
    let portal_url = open_portal(http, &cookie).await?;
    let html = fetch_portal_html(http, &portal_url).await?;
    drop(portal_url);

    let creds = extract_portal_creds(&html).ok_or_else(|| {
        AppError::upstream("账单门户页面里没有会话信息。")
            .with_hint("Cursor 或 Stripe 可能改了门户页面；过一会儿再试。")
    })?;

    let (subs, invoices) = tokio::join!(
        portal_get(http, &creds, "subscriptions", &[]),
        portal_get(
            http,
            &creds,
            "invoices",
            &[("limit", "12"), ("expand[]", "data.discounts")],
        ),
    );
    drop(creds);

    if subs.is_none() && invoices.is_none() {
        return Err(AppError::upstream("账单门户没有返回订阅或发票。")
            .with_hint("这个号可能没有个人账单，或门户接口暂时不可用。"));
    }

    Ok(assemble(subs.as_ref(), invoices.as_ref(), now_millis()))
}

async fn open_portal(http: &reqwest::Client, cookie: &str) -> Result<String> {
    let res = http
        .post(STRIPE_SESSION_URL)
        .header("Cookie", cookie)
        .header("User-Agent", UA)
        .header("Accept", "application/json, text/plain, */*")
        .header("Content-Type", "application/json")
        .header("Origin", "https://cursor.com")
        .header("Referer", "https://cursor.com/dashboard")
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(|err| {
            AppError::network(format!("打不开账单门户：{err}")).with_hint("检查网络后重试。")
        })?;

    let status = res.status();
    let body = res
        .text()
        .await
        .map_err(|err| AppError::network(format!("账单门户响应读不出来：{err}")))?;

    if status.as_u16() == 401 {
        return Err(AppError::unauthorized("Cursor 会话已失效，需要重新授权。"));
    }
    if let Ok(json) = serde_json::from_str::<Value>(&body) {
        if usage::unauthenticated(Some(&json)) {
            return Err(AppError::unauthorized("Cursor 会话已失效，需要重新授权。"));
        }
        if let Some(msg) = json_error_message(&json) {
            if status.as_u16() == 403 || status.as_u16() == 404 {
                return Err(
                    AppError::new(ErrorCode::Forbidden, "这个号没有个人账单门户。").with_hint(msg),
                );
            }
        }
    }
    if !status.is_success() {
        return Err(AppError::upstream(format!("账单门户入口返回了 {status}。"))
            .with_hint("免费号或团队成员常常没有个人账单；有订阅的号过一会儿再试。"));
    }

    parse_portal_url(&body).ok_or_else(|| {
        AppError::upstream("未返回个人账单门户链接。").with_hint(
            "Cursor 有时回一段 URL 文本、有时回 JSON；两种都认了还没有，多半是这个号没有门户。",
        )
    })
}

/// 认门户地址：裸 URL、JSON 字符串、或 `portalUrl` / `url` 一类字段。
///
/// 只收 `https://billing.stripe.com/…`。别的主机即便看起来像账单页也不跟。
pub fn parse_portal_url(body: &str) -> Option<String> {
    let trimmed = body.trim().trim_matches('"').trim();
    if is_portal_url(trimmed) {
        return Some(trimmed.to_string());
    }
    let json: Value = serde_json::from_str(body).ok()?;
    pick_portal_url(&json)
}

fn pick_portal_url(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if is_portal_url(s) => Some(s.trim().to_string()),
        Value::Object(map) => {
            for key in [
                "portalUrl",
                "portal_url",
                "url",
                "sessionUrl",
                "session_url",
                "stripeUrl",
                "stripe_url",
            ] {
                if let Some(found) = map.get(key).and_then(pick_portal_url) {
                    return Some(found);
                }
            }
            map.get("data").and_then(pick_portal_url)
        }
        _ => None,
    }
}

fn is_portal_url(raw: &str) -> bool {
    let s = raw.trim();
    let Some(rest) = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    host.eq_ignore_ascii_case(PORTAL_HOST)
}

async fn fetch_portal_html(http: &reqwest::Client, url: &str) -> Result<String> {
    let res = http
        .get(url)
        .header("User-Agent", UA)
        .header("Accept", "text/html,application/xhtml+xml")
        .timeout(Duration::from_secs(25))
        .send()
        .await
        .map_err(|_| AppError::network("账单门户打不开。").with_hint("检查网络后重试。"))?;

    if res.url().host_str() != Some(PORTAL_HOST) {
        return Err(AppError::upstream("账单门户跳到了意外的地址。")
            .with_hint("没有跟着走，以免离开 Stripe。"));
    }
    if !res.status().is_success() {
        return Err(AppError::upstream(format!(
            "账单门户页面返回了 {}。",
            res.status()
        )));
    }
    res.text()
        .await
        .map_err(|_| AppError::network("账单门户页面读不出来。").with_hint("检查网络后重试。"))
}

struct PortalCreds {
    api_key: String,
    session_id: String,
}

/// 从门户 HTML 里抠 ephemeral key 和 `bps_…`。只认页面字段，不把 URL 里的 `secret=` 留下。
fn extract_portal_creds(html: &str) -> Option<PortalCreds> {
    let api_key = extract_ek(html)?;
    let session_id = extract_bps(html)?;
    Some(PortalCreds {
        api_key,
        session_id,
    })
}

fn extract_ek(html: &str) -> Option<String> {
    if let Some(v) = quoted_field(html, "session_api_key") {
        if v.starts_with("ek_live_") || v.starts_with("ek_test_") {
            return Some(v);
        }
    }
    take_prefixed_token(html, "ek_live_", is_token_char)
        .or_else(|| take_prefixed_token(html, "ek_test_", is_token_char))
}

fn extract_bps(html: &str) -> Option<String> {
    if let Some(v) = quoted_field(html, "portal_session_id") {
        if v.starts_with("bps_") {
            return Some(strip_secret_suffix(&v));
        }
    }
    take_prefixed_token(html, "bps_", is_token_char).map(|s| strip_secret_suffix(&s))
}

fn strip_secret_suffix(raw: &str) -> String {
    match raw.find("_secret") {
        Some(i) => raw[..i].to_string(),
        None => raw.to_string(),
    }
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn take_prefixed_token(hay: &str, prefix: &str, ok: impl Fn(char) -> bool) -> Option<String> {
    let i = hay.find(prefix)?;
    let rest = &hay[i + prefix.len()..];
    let n = rest.find(|c: char| !ok(c)).unwrap_or(rest.len());
    if n == 0 {
        return None;
    }
    Some(format!("{}{}", prefix, &rest[..n]))
}

/// `key":"value"` / `key": "value"` / `key: "value"`。
fn quoted_field(hay: &str, key: &str) -> Option<String> {
    let mut from = 0;
    while let Some(rel) = hay[from..].find(key) {
        let i = from + rel + key.len();
        let tail = hay[i..].trim_start();
        let Some(tail) = tail.strip_prefix(':').or_else(|| tail.strip_prefix('=')) else {
            from = i;
            continue;
        };
        let tail = tail.trim_start_matches([' ', '\t', '\n', '\r']);
        let Some(&quote) = tail.as_bytes().first() else {
            from = i;
            continue;
        };
        if quote != b'"' && quote != b'\'' {
            from = i;
            continue;
        }
        let inner = &tail[1..];
        let Some(end) = inner.find(quote as char) else {
            from = i;
            continue;
        };
        let value = inner[..end].to_string();
        if !value.is_empty() {
            return Some(value);
        }
        from = i;
    }
    None
}

async fn portal_get(
    http: &reqwest::Client,
    creds: &PortalCreds,
    resource: &str,
    query: &[(&str, &str)],
) -> Option<Value> {
    let url = format!(
        "{STRIPE_API}/v1/billing_portal/sessions/{}/{}",
        creds.session_id, resource
    );
    const ATTEMPTS: u32 = 3;
    for attempt in 0..ATTEMPTS {
        let last = attempt + 1 == ATTEMPTS;
        let req = http
            .get(&url)
            .query(query)
            .header("Authorization", format!("Bearer {}", creds.api_key))
            .header("Stripe-Version", STRIPE_VERSION)
            .header("Accept", "application/json")
            .header("User-Agent", UA);
        match req.send().await {
            Ok(res) => {
                let status = res.status();
                let retryable =
                    status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
                if retryable && !last {
                    tokio::time::sleep(usage::backoff(attempt, None)).await;
                    continue;
                }
                if !status.is_success() {
                    tracing::warn!(resource, %status, "门户接口没有给出数据");
                    return None;
                }
                return res.json().await.ok();
            }
            Err(_) if !last => {
                tokio::time::sleep(usage::backoff(attempt, None)).await;
            }
            Err(err) => {
                tracing::warn!(resource, %err, "门户接口请求失败");
                return None;
            }
        }
    }
    None
}

/// 把门户 JSON 收成快照。订阅对象完整时才能下「没有折扣」的结论。
pub fn assemble(subs: Option<&Value>, invoices: Option<&Value>, now_ms: i64) -> AccountBilling {
    let sub = pick_subscription(subs);
    let items = sub.map(parse_items).unwrap_or_default();
    let list_price = sum_list(&items);
    let currency = items
        .first()
        .and_then(|i| i.currency.clone())
        .or_else(|| text(sub.and_then(|s| s.get("currency"))));
    let (discount_state, discount) = match sub {
        Some(s) => current_discount(s, now_ms),
        None => (DiscountState::Unknown, None),
    };
    let current_amount = payable(list_price, discount.as_ref(), discount_state);

    AccountBilling {
        fetched_at: now_iso(),
        currency,
        collection_method: text(sub.and_then(|s| s.get("collection_method"))),
        subscription_status: text(sub.and_then(|s| s.get("status"))),
        interval: items.iter().find_map(|i| i.interval.clone()),
        current_period_start: stripe_ts(sub.and_then(|s| s.get("current_period_start"))),
        current_period_end: stripe_ts(sub.and_then(|s| s.get("current_period_end"))),
        cancel_at_period_end: sub
            .and_then(|s| s.get("cancel_at_period_end"))
            .and_then(Value::as_bool),
        canceled_at: stripe_ts(sub.and_then(|s| s.get("canceled_at"))),
        items,
        list_price,
        current_amount,
        discount_state,
        discount,
        invoices: invoices.map(parse_invoices).unwrap_or_default(),
    }
}

fn pick_subscription(list: Option<&Value>) -> Option<&Value> {
    let rows = list?.get("data").and_then(Value::as_array)?;
    let rank = |s: &Value| match text(s.get("status")).as_deref() {
        Some("active") => 0,
        Some("trialing") => 1,
        Some("past_due") => 2,
        Some("unpaid") => 3,
        _ => 8,
    };
    rows.iter().min_by_key(|s| rank(s))
}

fn parse_items(sub: &Value) -> Vec<BillingItem> {
    let rows = sub
        .get("items")
        .and_then(|i| i.get("data"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    rows.iter()
        .map(|item| {
            let price = item.get("price");
            let plan = item.get("plan");
            let recurring = price.and_then(|p| p.get("recurring"));
            BillingItem {
                name: product_name(item),
                interval: text(recurring.and_then(|r| r.get("interval")))
                    .or_else(|| text(plan.and_then(|p| p.get("interval")))),
                unit_amount: int_opt(price.and_then(|p| p.get("unit_amount")))
                    .or_else(|| int_opt(plan.and_then(|p| p.get("amount")))),
                quantity: int_opt(item.get("quantity")).or(Some(1)),
                currency: text(price.and_then(|p| p.get("currency")))
                    .or_else(|| text(plan.and_then(|p| p.get("currency"))))
                    .or_else(|| text(sub.get("currency"))),
            }
        })
        .collect()
}

fn product_name(item: &Value) -> Option<String> {
    let price = item.get("price");
    match price.and_then(|p| p.get("product")) {
        Some(Value::Object(o)) => text(o.get("name")).or_else(|| text(o.get("description"))),
        Some(Value::String(s)) if !s.starts_with("prod_") && !s.is_empty() => Some(s.clone()),
        _ => None,
    }
    .or_else(|| text(price.and_then(|p| p.get("nickname"))))
    .or_else(|| text(item.get("plan").and_then(|p| p.get("nickname"))))
    .or_else(|| text(item.get("plan").and_then(|p| p.get("name"))))
}

fn sum_list(items: &[BillingItem]) -> Option<i64> {
    if items.is_empty() || items.iter().all(|i| i.unit_amount.is_none()) {
        return None;
    }
    Some(
        items
            .iter()
            .map(|i| i.unit_amount.unwrap_or(0) * i.quantity.unwrap_or(1))
            .sum(),
    )
}

fn current_discount(sub: &Value, now_ms: i64) -> (DiscountState, Option<BillingDiscount>) {
    let Some(raw) = first_discount_value(sub) else {
        return (DiscountState::None, None);
    };
    let mut d = parse_discount(&raw);
    let ended = d.ends_at.is_some_and(|end| end <= now_ms);
    let state = if ended {
        DiscountState::Expired
    } else {
        DiscountState::Active
    };
    d.state = Some(state);
    (state, Some(d))
}

fn first_discount_value(obj: &Value) -> Option<Value> {
    if let Some(v) = obj.get("discount") {
        if v.is_object() {
            return Some(v.clone());
        }
    }
    let list = obj.get("discounts").and_then(Value::as_array)?;
    for v in list {
        if v.is_object() {
            return Some(v.clone());
        }
    }
    None
}

fn parse_discount(raw: &Value) -> BillingDiscount {
    let coupon = raw.get("coupon").filter(|c| c.is_object()).or_else(|| {
        raw.get("source")
            .and_then(|s| s.get("coupon"))
            .filter(|c| c.is_object())
    });
    BillingDiscount {
        state: None,
        name: text(coupon.and_then(|c| c.get("name")))
            .or_else(|| text(raw.get("name")))
            .or_else(|| text(coupon.and_then(|c| c.get("id"))).filter(|s| !s.starts_with("j_"))),
        percent_off: num(coupon.and_then(|c| c.get("percent_off"))),
        amount_off: int_opt(coupon.and_then(|c| c.get("amount_off"))),
        currency: text(coupon.and_then(|c| c.get("currency")))
            .or_else(|| text(raw.get("currency"))),
        duration: text(coupon.and_then(|c| c.get("duration"))),
        duration_in_months: int_opt(coupon.and_then(|c| c.get("duration_in_months"))),
        starts_at: stripe_ts(raw.get("start")).or_else(|| stripe_ts(raw.get("starts_at"))),
        ends_at: stripe_ts(raw.get("end")).or_else(|| stripe_ts(raw.get("ends_at"))),
    }
}

fn payable(
    list: Option<i64>,
    discount: Option<&BillingDiscount>,
    state: DiscountState,
) -> Option<i64> {
    let list = list?;
    if state != DiscountState::Active {
        return Some(list);
    }
    let d = discount?;
    if let Some(pct) = d.percent_off {
        let cut = ((list as f64) * pct / 100.0).round() as i64;
        return Some((list - cut).max(0));
    }
    if let Some(off) = d.amount_off {
        return Some((list - off).max(0));
    }
    Some(list)
}

fn parse_invoices(list: &Value) -> Vec<BillingInvoice> {
    let rows = list
        .get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    rows.iter()
        .take(INVOICE_LIMIT)
        .map(|inv| BillingInvoice {
            number: text(inv.get("number")),
            created: stripe_ts(inv.get("created")),
            status: text(inv.get("status")),
            description: text(inv.get("description")),
            subtotal: int_opt(inv.get("subtotal")),
            total: int_opt(inv.get("total")),
            amount_due: int_opt(inv.get("amount_due")),
            amount_paid: int_opt(inv.get("amount_paid")),
            amount_remaining: int_opt(inv.get("amount_remaining")),
            currency: text(inv.get("currency")),
            period_start: stripe_ts(inv.get("period_start")),
            period_end: stripe_ts(inv.get("period_end")),
            discounts: invoice_discounts(inv),
            lines: invoice_lines(inv),
        })
        .collect()
}

fn invoice_discounts(inv: &Value) -> Vec<BillingDiscount> {
    let mut out = Vec::new();
    for key in ["discounts", "discount_objects"] {
        if let Some(arr) = inv.get(key).and_then(Value::as_array) {
            for v in arr {
                if v.is_object() {
                    let d = parse_discount(v);
                    if d.name.is_some() || d.amount_off.is_some() || d.percent_off.is_some() {
                        out.push(d);
                    }
                }
            }
        }
    }
    if out.is_empty() {
        if let Some(v) = inv.get("discount").filter(|v| v.is_object()) {
            let d = parse_discount(v);
            if d.name.is_some() || d.amount_off.is_some() || d.percent_off.is_some() {
                out.push(d);
            }
        }
    }
    out
}

fn invoice_lines(inv: &Value) -> Vec<BillingInvoiceLine> {
    let rows = inv
        .get("lines")
        .and_then(|l| l.get("data"))
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    rows.iter()
        .filter_map(|line| {
            let description = text(line.get("description"))?;
            Some(BillingInvoiceLine {
                description: Some(description),
                amount: int_opt(line.get("amount")),
                quantity: int_opt(line.get("quantity")),
            })
        })
        .take(LINE_LIMIT)
        .collect()
}

/// 序列化结果里不该出现的东西。测试盯着；运行时再扫一次当最后防线。
pub fn snapshot_is_clean(billing: &AccountBilling) -> bool {
    let Ok(json) = serde_json::to_string(billing) else {
        return false;
    };
    !json.contains("ek_live_")
        && !json.contains("ek_test_")
        && !json.contains("secret=")
        && !json.contains("billing.stripe.com")
        && !json.contains("hosted_invoice_url")
}

fn json_error_message(v: &Value) -> Option<String> {
    text(v.get("error"))
        .or_else(|| text(v.get("message")))
        .or_else(|| text(v.get("error").and_then(|e| e.get("message"))))
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        Value::String(s) if !s.trim().is_empty() => {
            s.trim().parse::<f64>().ok().filter(|f| f.is_finite())
        }
        _ => None,
    }
}

fn int_opt(v: Option<&Value>) -> Option<i64> {
    num(v).map(|f| f.round() as i64)
}

fn text(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// Stripe 时间戳一般是秒；已经是毫秒的不乘。
fn stripe_ts(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => {
            let n = n.as_i64().or_else(|| n.as_f64().map(|f| f as i64))?;
            (n > 0).then_some(if n < 1_000_000_000_000 { n * 1000 } else { n })
        }
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            if let Ok(n) = s.parse::<i64>() {
                return (n > 0).then_some(if n < 1_000_000_000_000 { n * 1000 } else { n });
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_raw_url_string_is_a_portal_link() {
        let url = "https://billing.stripe.com/p/session/live_abc";
        assert_eq!(parse_portal_url(url).as_deref(), Some(url));
        assert_eq!(
            parse_portal_url(&format!("\"{url}\"")).as_deref(),
            Some(url)
        );
        assert_eq!(
            parse_portal_url(&format!("  \"{url}\"  \n")).as_deref(),
            Some(url)
        );
    }

    #[test]
    fn json_portal_url_fields_are_accepted() {
        let url = "https://billing.stripe.com/p/session?secret=live_xxx";
        for body in [
            json!({ "portalUrl": url }),
            json!({ "url": url }),
            json!({ "sessionUrl": url }),
            json!({ "stripeUrl": url }),
            json!({ "data": { "url": url } }),
        ] {
            assert_eq!(
                parse_portal_url(&body.to_string()).as_deref(),
                Some(url),
                "{body}"
            );
        }
        // 一段被 JSON 编码的纯字符串。
        assert_eq!(
            parse_portal_url(&serde_json::to_string(url).unwrap()).as_deref(),
            Some(url)
        );
    }

    #[test]
    fn foreign_hosts_are_not_portal_links() {
        assert!(parse_portal_url("https://evil.example/billing.stripe.com").is_none());
        assert!(parse_portal_url("https://cursor.com/api/stripeSession").is_none());
        assert!(parse_portal_url("not a url").is_none());
        assert!(parse_portal_url("{\"url\":\"https://example.com\"}").is_none());
    }

    #[test]
    fn portal_html_yields_ephemeral_creds_and_strips_the_url_secret() {
        let html = r#"
            <script>
            window.__STRIPE = {
              "session_api_key": "ek_test_abcDEF123",
              "portal_session_id": "bps_1TestSession"
            };
            // 页面里偶尔会回显带 secret 的地址，不能把后半段当 id。
            const href = "https://billing.stripe.com/p/session?secret=bps_1TestSession_secret_leak";
            </script>
        "#;
        let creds = extract_portal_creds(html).unwrap();
        assert_eq!(creds.api_key, "ek_test_abcDEF123");
        assert_eq!(creds.session_id, "bps_1TestSession");
        assert!(!creds.session_id.contains("secret"));
    }

    #[test]
    fn a_live_subscription_without_coupon_is_none_not_unknown() {
        let subs = json!({
            "data": [{
                "status": "active",
                "collection_method": "charge_automatically",
                "currency": "usd",
                "current_period_start": 1755475200i64,
                "current_period_end": 1758067200i64,
                "cancel_at_period_end": false,
                "discount": null,
                "discounts": [],
                "items": {
                    "data": [{
                        "quantity": 1,
                        "price": {
                            "unit_amount": 2000,
                            "currency": "usd",
                            "recurring": { "interval": "month" },
                            "product": { "name": "Cursor Pro" }
                        }
                    }]
                }
            }]
        });
        let snap = assemble(Some(&subs), None, 1_758_000_000_000);
        assert_eq!(snap.discount_state, DiscountState::None);
        assert!(snap.discount.is_none());
        assert_eq!(snap.list_price, Some(2000));
        assert_eq!(snap.current_amount, Some(2000));
        assert_eq!(snap.items[0].name.as_deref(), Some("Cursor Pro"));
        assert_eq!(snap.interval.as_deref(), Some("month"));
        assert!(snapshot_is_clean(&snap));
    }

    #[test]
    fn an_active_repeating_coupon_cuts_the_payable_to_zero() {
        let subs = json!({
            "data": [{
                "status": "active",
                "currency": "usd",
                "discount": {
                    "start": 1755475200i64,
                    "end": 1770000000i64,
                    "coupon": {
                        "name": "SuperGrok Heavy",
                        "amount_off": 20000,
                        "percent_off": null,
                        "duration": "repeating",
                        "duration_in_months": 6,
                        "currency": "usd"
                    }
                },
                "items": {
                    "data": [{
                        "quantity": 1,
                        "price": {
                            "unit_amount": 20000,
                            "currency": "usd",
                            "recurring": { "interval": "month" },
                            "product": { "name": "Cursor Ultra" }
                        }
                    }]
                }
            }]
        });
        let snap = assemble(Some(&subs), None, 1_758_000_000_000);
        assert_eq!(snap.discount_state, DiscountState::Active);
        let d = snap.discount.as_ref().unwrap();
        assert_eq!(d.name.as_deref(), Some("SuperGrok Heavy"));
        assert_eq!(d.amount_off, Some(20000));
        assert_eq!(d.duration.as_deref(), Some("repeating"));
        assert_eq!(d.duration_in_months, Some(6));
        assert_eq!(snap.list_price, Some(20000));
        assert_eq!(snap.current_amount, Some(0));
        assert!(snapshot_is_clean(&snap));
    }

    #[test]
    fn a_percent_off_coupon_rounds_to_the_nearest_unit() {
        let subs = json!({
            "data": [{
                "status": "active",
                "discounts": [{
                    "coupon": { "name": "Loyalty discount 50", "percent_off": 50, "duration": "once" }
                }],
                "items": {
                    "data": [{
                        "quantity": 1,
                        "price": { "unit_amount": 19000, "currency": "usd", "recurring": { "interval": "month" }, "product": { "name": "Cursor Ultra" } }
                    }]
                }
            }]
        });
        let snap = assemble(Some(&subs), None, 1_758_000_000_000);
        assert_eq!(snap.discount_state, DiscountState::Active);
        assert_eq!(snap.current_amount, Some(9500));
    }

    #[test]
    fn an_ended_discount_is_expired_and_next_charge_is_list_price() {
        let subs = json!({
            "data": [{
                "status": "active",
                "discount": {
                    "start": 1700000000i64,
                    "end": 1701000000i64,
                    "coupon": { "name": "Referred by a friend", "amount_off": 1000, "duration": "once", "currency": "usd" }
                },
                "items": {
                    "data": [{
                        "quantity": 1,
                        "price": { "unit_amount": 2000, "currency": "usd", "recurring": { "interval": "month" }, "product": { "name": "Cursor Pro" } }
                    }]
                }
            }]
        });
        let snap = assemble(Some(&subs), None, 1_758_000_000_000);
        assert_eq!(snap.discount_state, DiscountState::Expired);
        assert_eq!(snap.current_amount, Some(2000));
        assert_eq!(
            snap.discount.as_ref().and_then(|d| d.name.as_deref()),
            Some("Referred by a friend")
        );
    }

    #[test]
    fn missing_subscription_object_stays_unknown() {
        let snap = assemble(None, Some(&json!({ "data": [] })), 1);
        assert_eq!(snap.discount_state, DiscountState::Unknown);
        assert!(snap.discount.is_none());
        assert!(snap.list_price.is_none());
    }

    #[test]
    fn invoices_keep_coupon_history_and_drop_hosted_urls() {
        let invoices = json!({
            "data": [{
                "number": "INV-1",
                "created": 1755475200i64,
                "status": "paid",
                "description": "Cursor Pro",
                "subtotal": 2000,
                "total": 1000,
                "amount_due": 0,
                "amount_paid": 1000,
                "amount_remaining": 0,
                "currency": "usd",
                "period_start": 1755475200i64,
                "period_end": 1758067200i64,
                "hosted_invoice_url": "https://invoice.stripe.com/i/acct_xxx/live_secret",
                "invoice_pdf": "https://pay.stripe.com/invoice/xxx/pdf",
                "discounts": [{
                    "coupon": {
                        "name": "Referred by a friend",
                        "amount_off": 1000,
                        "duration": "once",
                        "currency": "usd"
                    }
                }],
                "lines": {
                    "data": [
                        { "description": "1 × Cursor Pro (at $20.00 / month)", "amount": 2000, "quantity": 1 },
                        { "description": "Referred by a friend", "amount": -1000, "quantity": 1 }
                    ]
                }
            }]
        });
        let snap = assemble(None, Some(&invoices), 1);
        assert_eq!(snap.invoices.len(), 1);
        let inv = &snap.invoices[0];
        assert_eq!(inv.number.as_deref(), Some("INV-1"));
        assert_eq!(inv.amount_paid, Some(1000));
        assert_eq!(
            inv.discounts[0].name.as_deref(),
            Some("Referred by a friend")
        );
        assert_eq!(inv.lines.len(), 2);
        let json = serde_json::to_string(&snap).unwrap();
        assert!(!json.contains("hosted_invoice_url"));
        assert!(!json.contains("invoice.stripe.com"));
        assert!(!json.contains("live_secret"));
        assert!(snapshot_is_clean(&snap));
    }

    #[test]
    fn stripe_seconds_become_epoch_millis() {
        assert_eq!(
            stripe_ts(Some(&json!(1755475200i64))),
            Some(1_755_475_200_000)
        );
        assert_eq!(
            stripe_ts(Some(&json!(1_755_475_200_000i64))),
            Some(1_755_475_200_000)
        );
    }

    #[test]
    fn serialized_billing_uses_camel_case() {
        let snap = AccountBilling {
            fetched_at: "2026-09-13T00:00:00Z".into(),
            discount_state: DiscountState::Active,
            list_price: Some(20000),
            current_amount: Some(0),
            ..Default::default()
        };
        let v = serde_json::to_value(&snap).unwrap();
        assert_eq!(v["discountState"], "active");
        assert_eq!(v["listPrice"], 20000);
        assert_eq!(v["currentAmount"], 0);
        assert_eq!(v["fetchedAt"], "2026-09-13T00:00:00Z");
    }
}
