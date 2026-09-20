//! 一个号在 Cursor 那边的真实账况。
//!
//! 走 cursor.com 网页版 dashboard 的接口，认证方式和浏览器一样——Cookie 里放
//! `WorkosCursorSessionToken=user_xxx::<access_jwt>`。
//!
//! **字段口径以 `shop/src/lib/cursorUsage.ts` 为规格**（那份用真实响应逐条核对过），
//! 这里是它的 Rust 移植。仍然做容错（数字可能是字符串、对象可能整个缺席），因为这些
//! 接口没有公开契约；但**不做多路径猜测**：猜错了会安静地显示一个错的数，比显示「—」
//! 糟得多。

use nexus_core::{now_iso, AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
const TIMEOUT: Duration = Duration::from_secs(20);
const AGGREGATED_URL: &str = "https://cursor.com/api/dashboard/get-aggregated-usage-events";
const SET_HARD_LIMIT_URL: &str = "https://cursor.com/api/dashboard/set-hard-limit";
const GET_HARD_LIMIT_URL: &str = "https://cursor.com/api/dashboard/get-hard-limit";
/// Fable 5 的「非零数据保留」同意开关（仪表盘 Privacy 那格）。2026-09-19 抓包实测。
const SET_ZDR_CONSENT_URL: &str =
    "https://cursor.com/api/dashboard/set-user-no-zdr-model-consent";
const CREDIT_GRANTS_URL: &str = "https://cursor.com/api/dashboard/get-credit-grants-balance";
const CREDIT_GRANT_LIST_URL: &str =
    "https://cursor.com/api/dashboard/get-client-visible-credit-grants";
const DAY_MS: i64 = 86_400_000;

/// Bot（Cursor 内部代号 sand，界面上叫 "Grok Bot Plan"）通道的**周**额度。
///
/// 和 dashboard 上的 Auto / API 桶是两套独立计量。老号（pro-legacy）没有这套，
/// 整个对象缺席——宁可让上层看到「没有这套」，也不要造一个 0% 的假象。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BotQuota {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent_used: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub period_start: Option<i64>,
    /// 下次重置时刻，epoch ms。Bot 是**周**额，和月账期不是一回事，界面两个都要显示。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_available: Option<bool>,
    /// `granted` / `blocked`。没权限的话额度多少都没用。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_label: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelUsage {
    pub model: String,
    /// 1 = named/API 桶，2 = Auto 桶。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tier: Option<f64>,
    pub cents: f64,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

/// 一段时间窗内的花费：今天、近 7 天。
///
/// 数字来自 `get-aggregated-usage-events` 带日期的那一问，`cents` 是所有明细行 `totalCents`
/// 的和 —— 和本账期那个来自 `usage-summary` 的 `spend_cents` 不是同一个口径（那个是
/// Cursor 结算过的数），两者相差几美分是正常的。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    /// 窗口起止，epoch ms。
    pub start: i64,
    pub end: i64,
    pub cents: f64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub by_model: Vec<ModelUsage>,
}

/// 一笔 Cursor 赠送的积分（credit grant）。这是用户口中的「积分」——和账期里那个
/// `breakdown.bonus`（厂商补贴的免费加量）不是一回事。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditGrant {
    /// Cursor 给的名字，例如 "Power user grant"。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub total_cents: f64,
    pub remaining_cents: f64,
    /// 过期时刻，epoch ms。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsage {
    pub fetched_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_created_at: Option<String>,
    /// 原样保留 Cursor 的写法：pro / pro_plus / ultra / free / enterprise…
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subscription_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_yearly_plan: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_team_member: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_cancellation_date: Option<String>,
    /// 月账期起止，epoch ms。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle_start: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cycle_end: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bot: Option<BotQuota>,
    /// 三个已用百分比（0..100）。Auto 桶和 API 桶分别计量，任一打满那类模型就停了，
    /// 所以三个都要看。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_percent_used: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_percent_used: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_percent_used: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub included_cents: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bonus_cents: Option<f64>,
    /// Cursor 赠送的 credit grant 余额（`GetCreditGrantsBalance`），单位美分。
    ///
    /// 和上面的 `bonus_cents` 不是一回事：那个是本账期从「赠送额度」里**花掉**的钱；
    /// 这个是还剩多少赠送积分（仪表盘上常见 25 / 100 那种）。没有赠送时整组缺席。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_grant_total_cents: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_grant_used_cents: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credit_grant_remaining_cents: Option<f64>,
    /// 每一笔赠送的明细（`GetClientVisibleCreditGrants`）：叫什么、还剩多少、什么时候过期。
    /// 余额接口只给总数；仪表盘上「Power user grant · 到 10 月 15 日」那行字来自这里。
    /// 没有赠送时缺席。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit_grants: Option<Vec<CreditGrant>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spend_cents: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_limit_cents: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_demand_enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_demand_used_cents: Option<f64>,
    /// `null` 且 enabled 为真 = 不封顶。所以这里是双层 Option，不能简化。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on_demand_limit_cents: Option<Option<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub by_model: Option<Vec<ModelUsage>>,
    /// 今天（本地零点起）与近 7 天（含今天的 7 个自然日）的花费。
    ///
    /// 「今天」从几点算，Rust 这边不知道：多线程进程里拿不到可靠的本地时区，而这个
    /// 应用的刷新全部由前端触发，所以本地零点由前端递进来（`fetch` 的 `day_start_ms`）。
    /// 没递、或那两问没回来，这两项就缺席 —— 界面上退回只看本账期。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub today: Option<UsageWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub week: Option<UsageWindow>,
    /// `apiKey`：这次快照来自 `crsr_` 兑票后的逐条事件，没有额度百分比 / Bot 周额。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

/// 拉一个号的完整账况。
///
/// 六个接口并发；任意一个挂掉都不影响其余字段落地——只有 `usage-summary` 拿不到才算
/// 失败，那是唯一不可替代的一个。给了 `day_start_ms`（本地零点，epoch ms）再多两问：
/// 今天、近 7 天的按模型聚合 —— 同一个接口带日期。它们和其余六个一起并发出去。
pub async fn fetch(
    http: &reqwest::Client,
    session_token: &str,
    day_start_ms: Option<i64>,
) -> Result<AccountUsage> {
    let token = session_token.trim();
    if !token.starts_with("user_") || !token.contains("::") {
        return Err(AppError::unauthorized(
            "session token 格式不对，应为 user_xxx::<jwt>。",
        ));
    }
    let cookie = session_cookie(token);

    let now_ms = now_millis();
    // 零点在未来、或离谱地远（时钟坏了）都不问：一个空窗口比一个错窗口好。
    let today_start = day_start_ms.filter(|s| *s > 0 && *s <= now_ms && now_ms - *s <= 2 * DAY_MS);
    let week_start = today_start.map(|s| s - 6 * DAY_MS);

    let access = access_from_session(token);
    let (
        summary,
        stripe,
        me,
        agg,
        sand_usage,
        sand_access,
        today,
        week,
        grants,
        grant_list,
        hard_limit,
    ) = tokio::join!(
        call(http, "https://cursor.com/api/usage-summary", &cookie, None),
        call(http, "https://cursor.com/api/auth/stripe", &cookie, None),
        call(http, "https://cursor.com/api/auth/me", &cookie, None),
        // 空 body 默认就是本账期，实测与 usage-summary 的 breakdown.total 对得上。
        // 不传日期是为了让六个请求完全并发——传的话得先拿到账期。
        call(http, AGGREGATED_URL, &cookie, Some(serde_json::json!({}))),
        // Bot（sand）通道的周额度与权限。
        call(
            http,
            "https://cursor.com/api/dashboard/get-sand-usage-status",
            &cookie,
            Some(serde_json::json!({}))
        ),
        call(
            http,
            "https://cursor.com/api/dashboard/get-sand-access-status",
            &cookie,
            Some(serde_json::json!({}))
        ),
        ranged(http, &cookie, today_start, now_ms),
        ranged(http, &cookie, week_start, now_ms),
        connect_or_rest(
            http,
            access,
            &cookie,
            "GetCreditGrantsBalance",
            CREDIT_GRANTS_URL
        ),
        connect_or_rest(
            http,
            access,
            &cookie,
            "GetClientVisibleCreditGrants",
            CREDIT_GRANT_LIST_URL
        ),
        connect_or_rest(http, access, &cookie, "GetHardLimit", GET_HARD_LIMIT_URL),
    );

    if unauthenticated(summary.as_ref()) || unauthenticated(me.as_ref()) {
        return Err(AppError::unauthorized("Cursor 会话已失效，需要重新授权。"));
    }
    let Some(summary) = summary else {
        return Err(AppError::new(
            ErrorCode::Upstream,
            "拉取 usage-summary 失败（网络或响应异常）。",
        )
        .with_hint("稍后重试；这一项拿不到就没有可信的额度数据。"));
    };

    let mut usage = parse(
        &summary,
        stripe.as_ref(),
        me.as_ref(),
        agg.as_ref(),
        sand_usage.as_ref(),
        sand_access.as_ref(),
    );
    apply_credit_grants(&mut usage, grants.as_ref());
    apply_credit_grant_list(&mut usage, grant_list.as_ref());
    apply_hard_limit(&mut usage, hard_limit.as_ref());
    usage.today = today_start.and_then(|s| window(s, now_ms, today.as_ref()));
    usage.week = week_start.and_then(|s| window(s, now_ms, week.as_ref()));
    Ok(usage)
}

const DASHBOARD_SERVICE: &str = "https://api2.cursor.sh/aiserver.v1.DashboardService";

/// 用 `crsr_` 兑一把短期 access，再拉逐条用量。
///
/// 没有额度百分比、Bot 周额、账期：那些只在 cookie dashboard 上。这里能给的是
/// 花费合计、按模型、以及（给了本地零点时）今天 / 近 7 天窗口。
pub async fn fetch_via_api_key(
    http: &reqwest::Client,
    api_key: &str,
    day_start_ms: Option<i64>,
) -> Result<AccountUsage> {
    let access = crate::token::exchange_api_key(http, api_key).await?;
    let now_ms = now_millis();
    let today_start = day_start_ms.filter(|s| *s > 0 && *s <= now_ms && now_ms - *s <= 2 * DAY_MS);
    let week_start = today_start.map(|s| s - 6 * DAY_MS);
    let start_ms = week_start.unwrap_or(now_ms - 30 * DAY_MS).max(0);
    let events = usage_events(http, &access, start_ms, now_ms).await?;
    Ok(events_to_usage(&events, now_ms, today_start, week_start))
}

#[derive(Debug, Clone, Default)]
struct UsageEvent {
    ts: i64,
    model: String,
    charged_cents: f64,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    plan: Option<String>,
}

async fn usage_events(
    http: &reqwest::Client,
    access: &str,
    start_ms: i64,
    end_ms: i64,
) -> Result<Vec<UsageEvent>> {
    let mut events = Vec::new();
    for page in 1..=10 {
        let body = serde_json::json!({
            "page": page,
            "pageSize": 100,
            "startDate": start_ms.to_string(),
            "endDate": end_ms.to_string(),
        });
        let json = dashboard_call(http, access, "GetFilteredUsageEvents", &body).await?;
        let rows = json
            .get("usageEventsDisplay")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let n = rows.len();
        for row in rows {
            if let Some(ev) = parse_usage_event(&row) {
                events.push(ev);
            }
        }
        if n < 100 {
            break;
        }
    }
    Ok(events)
}

async fn dashboard_call(
    http: &reqwest::Client,
    access: &str,
    method: &str,
    body: &Value,
) -> Result<Value> {
    let url = format!("{DASHBOARD_SERVICE}/{method}");
    let res = http
        .post(&url)
        .timeout(TIMEOUT)
        .header("authorization", format!("Bearer {access}"))
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .header("connect-protocol-version", "1")
        .json(body)
        .send()
        .await
        .map_err(|err| AppError::network(format!("{method} 请求失败：{err}")))?;

    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(AppError::unauthorized(format!(
            "{method} 被拒绝（{status}）。API Key 可能已失效。"
        )));
    }
    if !status.is_success() {
        let head: String = text.chars().take(160).collect();
        return Err(AppError::upstream(format!(
            "{method} 返回 {status}：{head}"
        )));
    }
    if text.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(&text).map_err(|_| AppError::upstream(format!("{method} 响应不是 JSON。")))
}

fn parse_usage_event(row: &Value) -> Option<UsageEvent> {
    let tu = row.get("tokenUsage");
    let model = text(row.get("model")).unwrap_or_default();
    Some(UsageEvent {
        ts: int(row.get("timestamp")),
        model,
        charged_cents: num(row.get("chargedCents"))
            .or_else(|| num(tu.and_then(|v| v.get("totalCents"))))
            .unwrap_or(0.0),
        input: int(tu.and_then(|v| v.get("inputTokens"))),
        output: int(tu.and_then(|v| v.get("outputTokens"))),
        cache_read: int(tu.and_then(|v| v.get("cacheReadTokens"))),
        cache_write: int(tu.and_then(|v| v.get("cacheWriteTokens"))),
        plan: text(row.get("subscriptionProductId")),
    })
}

fn events_to_usage(
    events: &[UsageEvent],
    now_ms: i64,
    today_start: Option<i64>,
    week_start: Option<i64>,
) -> AccountUsage {
    let mut usage = AccountUsage {
        fetched_at: now_iso(),
        via: Some("apiKey".into()),
        spend_cents: Some(events.iter().map(|e| e.charged_cents).sum()),
        input_tokens: Some(events.iter().map(|e| e.input).sum()),
        output_tokens: Some(events.iter().map(|e| e.output).sum()),
        cache_read_tokens: Some(events.iter().map(|e| e.cache_read).sum()),
        cache_write_tokens: Some(events.iter().map(|e| e.cache_write).sum()),
        plan: events.iter().find_map(|e| e.plan.clone()),
        ..Default::default()
    };
    let by = aggregate_models(events);
    if !by.is_empty() {
        usage.by_model = Some(by);
    }
    usage.today = today_start.and_then(|s| event_window(events, s, now_ms));
    usage.week = week_start.and_then(|s| event_window(events, s, now_ms));
    usage
}

fn aggregate_models(events: &[UsageEvent]) -> Vec<ModelUsage> {
    use std::collections::BTreeMap;
    let mut by: BTreeMap<String, ModelUsage> = BTreeMap::new();
    for e in events {
        let name = e.model.trim();
        if name.is_empty() {
            continue;
        }
        let row = by.entry(name.to_string()).or_insert_with(|| ModelUsage {
            model: name.to_string(),
            ..Default::default()
        });
        row.cents += e.charged_cents;
        row.input += e.input;
        row.output += e.output;
        row.cache_read += e.cache_read;
        row.cache_write += e.cache_write;
    }
    let mut rows: Vec<_> = by.into_values().collect();
    rows.sort_by(|a, b| {
        b.cents
            .partial_cmp(&a.cents)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    rows
}

fn event_window(events: &[UsageEvent], start: i64, end: i64) -> Option<UsageWindow> {
    let slice: Vec<_> = events
        .iter()
        .filter(|e| e.ts >= start && e.ts <= end)
        .cloned()
        .collect();
    if slice.is_empty() {
        return Some(UsageWindow {
            start,
            end,
            ..Default::default()
        });
    }
    Some(UsageWindow {
        start,
        end,
        cents: slice.iter().map(|e| e.charged_cents).sum(),
        input_tokens: slice.iter().map(|e| e.input).sum(),
        output_tokens: slice.iter().map(|e| e.output).sum(),
        cache_read_tokens: slice.iter().map(|e| e.cache_read).sum(),
        cache_write_tokens: slice.iter().map(|e| e.cache_write).sum(),
        by_model: aggregate_models(&slice),
    })
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 带日期的那一问。`start` 为 None 就不问 —— 让 `join!` 的形状保持固定。
async fn ranged(
    http: &reqwest::Client,
    cookie: &str,
    start: Option<i64>,
    end: i64,
) -> Option<Value> {
    let start = start?;
    // DashboardService 的日期是 int64 毫秒，JSON 里按字符串传（与 get-filtered-usage-events 同）。
    let body = serde_json::json!({ "startDate": start.to_string(), "endDate": end.to_string() });
    call(http, AGGREGATED_URL, cookie, Some(body)).await
}

/// 仪表盘把「No Limit」编码成 `hardLimit = 2^31-1`（i32 最大值）。2026-09-20
/// Spending 页勾不封顶抓到的。**省略 `hardLimit` 不等于不封顶**：Connect proto3 会把缺席
/// 的 int32 默认成 0，Spending 页显示 Fixed $0，按需等于没开。
pub const UNLIMITED_HARD_LIMIT_DOLLARS: i64 = i32::MAX as i64;

/// 改这个号的按需计费：开/关，以及每月上限（美分；`None` = 不封顶）。
///
/// 写 Spending 页的开关，权威接口是 DashboardService 的 `SetHardLimit`（Bearer JWT）。
/// 网页那条 `set-hard-limit` 当退路。`hardLimit` 的单位是**美元整数**。关掉时必须带
/// `hardLimit: 0`，只传 `noUsageBasedAllowed` 上游会当没改过。开启且不封顶时传
/// [`UNLIMITED_HARD_LIMIT_DOLLARS`]（仪表盘「No Limit」）。写完再读一遍 `GetHardLimit`：
/// usage-summary 的 `onDemand.enabled` 跟这个开关不是同一份状态，拿它当回执会以为没生效。
pub async fn set_on_demand(
    http: &reqwest::Client,
    session_token: &str,
    enabled: bool,
    limit_cents: Option<f64>,
) -> Result<()> {
    let cookie = session_cookie(session_token);
    let body = hard_limit_body(enabled, limit_cents);
    let access = access_from_session(session_token);

    let mut wrote = false;
    if let Some(access) = access {
        match dashboard_call(http, access, "SetHardLimit", &body).await {
            Ok(json) if unauthenticated(Some(&json)) => {
                return Err(AppError::unauthorized("session token 已被上游拒绝。")
                    .with_hint("到凭证页更新 session token，或授权一次重新登录。"));
            }
            Ok(json) => {
                if let Some(msg) = error_message(&json) {
                    return Err(AppError::upstream(msg)
                        .with_hint("Apple 内购的号开不了按需；团队号可能只有管理员能改。"));
                }
                wrote = true;
            }
            Err(err) if err.code == ErrorCode::Unauthorized => return Err(err),
            Err(_) => {}
        }
    }
    if !wrote {
        let json = post_required(http, SET_HARD_LIMIT_URL, &cookie, body, "改按需计费").await?;
        if unauthenticated(Some(&json)) {
            return Err(AppError::unauthorized("session token 已被上游拒绝。")
                .with_hint("到凭证页更新 session token，或授权一次重新登录。"));
        }
        if let Some(msg) = error_message(&json) {
            return Err(AppError::upstream(msg)
                .with_hint("Apple 内购的号开不了按需；团队号可能只有管理员能改。"));
        }
    }

    let got = connect_or_rest(http, access, &cookie, "GetHardLimit", GET_HARD_LIMIT_URL).await;
    if let Some(got) = got.as_ref() {
        if unauthenticated(Some(got)) {
            return Err(AppError::unauthorized("session token 已被上游拒绝。")
                .with_hint("到凭证页更新 session token，或授权一次重新登录。"));
        }
        if !hard_limit_stuck(got, enabled, limit_cents) {
            return Err(AppError::upstream("Cursor 没有接受这次按需改动。").with_hint(
                "Apple 内购的号开不了按需；团队号可能只有管理员能改。开启不封顶必须带 hardLimit=2147483647，省略会被写成 Fixed $0。",
            ));
        }
    }
    Ok(())
}

/// 写完再读的那一问：开关对不对，不封顶有没有被写成 $0。
fn hard_limit_stuck(got: &Value, enabled: bool, limit_cents: Option<f64>) -> bool {
    if let Some(now_enabled) = as_bool(got.get("noUsageBasedAllowed")).map(|no| !no) {
        if now_enabled != enabled {
            return false;
        }
    }
    if !enabled {
        return true;
    }
    let asked_unlimited = limit_cents
        .filter(|c| c.is_finite() && *c > 0.0)
        .is_none();
    if !asked_unlimited {
        return true;
    }
    match num(got.get("hardLimit")) {
        // 省略字段时 Connect 回 0，Spending 页就是 Fixed $0。
        Some(d) if d <= 0.0 => false,
        Some(d) if is_unlimited_dollars(d) => true,
        // 回了一个普通上限：不是我们要的「不封顶」。
        Some(_) => false,
        None => true,
    }
}

fn is_unlimited_dollars(dollars: f64) -> bool {
    dollars >= UNLIMITED_HARD_LIMIT_DOLLARS as f64
}

/// Fable 5 数据保留策略里那个模型 id 与条款版本（2026-09-19 抓包）。上游会校验版本，
/// 版本号变了要跟着改；单独拎成常量，接口挪动时改一处。
pub const FABLE5_MODEL_ID: &str = "claude-fable-5";
pub const FABLE5_CONSENT_VERSION: &str = "fable-data-retention-v1";

/// 给某个模型打开「非零数据保留」同意——仪表盘 Privacy 页那个开关。
///
/// 走 cursor.com 的 cookie 面 `set-user-no-zdr-model-consent`（2026-09-19 抓包实测）：
/// `enabled:true` 开、`acknowledged:true` 表示看过条款、`consentVersion` 是当前条款版本；
/// 上游回 `{"consented":true}`。
///
/// **幂等，所以不查旧状态直接写。** 重复对同一个 consent POST 上游照样回 `consented:true`，
/// 没有副作用。查一遍现状要多打一个 `get-no-zdr-model-consent-status`，为省这一个请求，
/// 代价只是重跑一批号时各多发一次这条 POST——划算。
pub async fn set_data_retention_consent(
    http: &reqwest::Client,
    session_token: &str,
    model_id: &str,
    consent_version: &str,
) -> Result<()> {
    let cookie = session_cookie(session_token);
    let body = serde_json::json!({
        "modelId": model_id,
        "enabled": true,
        "acknowledged": true,
        "consentVersion": consent_version,
    });
    let json = post_required(http, SET_ZDR_CONSENT_URL, &cookie, body, "开数据保留策略").await?;
    if unauthenticated(Some(&json)) {
        return Err(AppError::unauthorized("session token 已被上游拒绝。")
            .with_hint("到凭证页更新 session token，或授权一次重新登录。"));
    }
    if let Some(msg) = error_message(&json) {
        return Err(AppError::upstream(msg)
            .with_hint("条款版本可能变了；抓一条新的 set-user-no-zdr-model-consent 更新常量。"));
    }
    Ok(())
}

/// `set-hard-limit` / `SetHardLimit` 的请求体。单测对着形状，不打真接口。
///
/// 团队策略那几项（`preserveHardLimitPerUser` 等）个人号会忽略；仪表盘每次都带，
/// 跟它对齐，免得上游把缺字段当成一次残缺的团队策略写入。
pub(crate) fn hard_limit_body(enabled: bool, limit_cents: Option<f64>) -> Value {
    let mut body = serde_json::Map::new();
    if !enabled {
        // 关掉必须带 hardLimit: 0。只传 noUsageBasedAllowed 上游会当没改过。
        body.insert("hardLimit".into(), Value::from(0));
        body.insert("noUsageBasedAllowed".into(), Value::Bool(true));
    } else {
        body.insert("noUsageBasedAllowed".into(), Value::Bool(false));
        let dollars = match limit_cents.filter(|c| c.is_finite() && *c > 0.0) {
            Some(cents) => {
                let n = (cents / 100.0).round() as i64;
                n.clamp(1, UNLIMITED_HARD_LIMIT_DOLLARS - 1)
            }
            // 不封顶 = i32::MAX。省略会被写成 0（Fixed $0）。
            None => UNLIMITED_HARD_LIMIT_DOLLARS,
        };
        body.insert("hardLimit".into(), Value::from(dollars));
    }
    body.insert("preserveHardLimitPerUser".into(), Value::Bool(false));
    body.insert("perUserMonthlyLimitDollars".into(), Value::from(0));
    body.insert("clearPerUserMonthlyLimitDollars".into(), Value::Bool(false));
    body.insert("isDynamicTeamLimit".into(), Value::Bool(false));
    body.insert("clearConflictingPolicy".into(), Value::Bool(false));
    body.insert(
        "clearPerUserFirstPartyModelsAdditionalBudgetDollars".into(),
        Value::Bool(false),
    );
    body.insert(
        "clearPerUserFirstPartyModelsAdditionalBudgetUnlimited".into(),
        Value::Bool(false),
    );
    Value::Object(body)
}

fn error_message(json: &Value) -> Option<String> {
    let msg = json
        .get("error")
        .and_then(|e| {
            e.as_str()
                .map(str::to_string)
                .or_else(|| e.get("message").and_then(Value::as_str).map(str::to_string))
        })
        .or_else(|| {
            json.get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
        })?;
    let trimmed = msg.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("ok") {
        return None;
    }
    Some(trimmed.to_string())
}

/// 改配置的那一记：失败必须报出去，不能像刷用量那样把单项静默降级成「没有」。
/// 往 cursor.com 的 dashboard cookie 面发一个写请求，429 / 5xx 重试三次。
///
/// `action` 是给人看的动作名（「改按需计费」「开数据保留策略」），只用来拼错误话——
/// 一个通用的 POST 助手被两三个写操作共用，报错里得说清是哪一件事没成。
async fn post_required(
    http: &reqwest::Client,
    url: &str,
    cookie: &str,
    body: Value,
    action: &str,
) -> Result<Value> {
    const ATTEMPTS: u32 = 3;
    let mut last_err: Option<AppError> = None;
    for attempt in 0..ATTEMPTS {
        let last = attempt + 1 == ATTEMPTS;
        let req = http
            .post(url)
            .json(&body)
            .header("Cookie", cookie)
            .header("User-Agent", UA)
            .header("Accept", "application/json")
            .header("Origin", "https://cursor.com")
            .header("Referer", "https://cursor.com/dashboard");
        match req.send().await {
            Ok(res) => {
                let status = res.status();
                let retryable =
                    status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
                if retryable && !last {
                    let after = res
                        .headers()
                        .get(reqwest::header::RETRY_AFTER)
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_owned);
                    tokio::time::sleep(backoff(attempt, after.as_deref())).await;
                    continue;
                }
                if status == reqwest::StatusCode::UNAUTHORIZED
                    || status == reqwest::StatusCode::FORBIDDEN
                {
                    return Err(AppError::unauthorized(format!("Cursor 拒绝了这次{action}。"))
                        .with_hint("会话可能过期了；到凭证页更新 session token，或授权一次。"));
                }
                let text = res.text().await.unwrap_or_default();
                if text.trim().is_empty() {
                    if status.is_success() {
                        return Ok(Value::Object(serde_json::Map::new()));
                    }
                    return Err(AppError::upstream(format!("{action}失败（HTTP {status}）。")));
                }
                let json: Value =
                    serde_json::from_str(&text).unwrap_or(Value::String(text.clone()));
                if status.is_success() {
                    return Ok(json);
                }
                return Err(AppError::upstream(
                    error_message(&json)
                        .unwrap_or_else(|| format!("{action}失败（HTTP {status}）。")),
                ));
            }
            Err(err) if !last => {
                last_err = Some(AppError::network(format!("{action}请求失败：{err}")));
                tokio::time::sleep(backoff(attempt, None)).await;
            }
            Err(err) => {
                return Err(last_err
                    .unwrap_or_else(|| AppError::network(format!("{action}请求失败：{err}"))));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| AppError::upstream(format!("{action}失败。"))))
}

pub fn session_cookie(session_token: &str) -> String {
    // 有些来源里 `::` 是被 URL 编码过的，先还原再拼 Cookie。
    let raw = if session_token.contains("%3A%3A") {
        session_token.replace("%3A%3A", "::")
    } else {
        session_token.to_string()
    };
    format!("WorkosCursorSessionToken={raw}")
}

/// `user_xxx::<jwt>` 里的 JWT，给 DashboardService 当 Bearer。
fn access_from_session(session_token: &str) -> Option<&str> {
    session_token
        .split_once("::")
        .map(|(_, jwt)| jwt.trim())
        .filter(|s| s.len() > 20)
}

/// 优先走 api2 的 Connect RPC（Bearer JWT），网页 cookie 那条当退路。
async fn connect_or_rest(
    http: &reqwest::Client,
    access: Option<&str>,
    cookie: &str,
    method: &str,
    rest_url: &str,
) -> Option<Value> {
    if let Some(access) = access {
        if let Ok(v) = dashboard_call(http, access, method, &serde_json::json!({})).await {
            return Some(v);
        }
    }
    call(http, rest_url, cookie, Some(serde_json::json!({}))).await
}

/// 一次重试要等多久。
///
/// 上游说了等多久就等多久（`Retry-After`），它比我们瞎猜准。没说才退回指数退避，
/// 并且**加一点抖动** —— 六个请求是同时发的，一起被限流又一起原地重试，就是把同一记
/// 突发再打一遍。上限卡住，免得一个离谱的 `Retry-After` 把整批刷号拖到天荒地老。
pub(crate) fn backoff(attempt: u32, retry_after: Option<&str>) -> Duration {
    const MAX: Duration = Duration::from_secs(5);
    if let Some(secs) = retry_after.and_then(|v| v.trim().parse::<u64>().ok()) {
        return Duration::from_secs(secs).min(MAX);
    }
    let base = Duration::from_millis(400 * 3u64.pow(attempt.min(3)));
    // 抖动取地址低位，够散且不用引随机数依赖。
    let jitter = Duration::from_millis((std::ptr::addr_of!(base) as u64) % 120);
    (base + jitter).min(MAX)
}

/// 单个接口，最多试三次。
///
/// 这不是可有可无的加固：冷启动那一次，六个并发请求要一起做 TLS 握手，实测会掉一两个。
/// 而调用方对缺字段是静默降级的——比如 sand 那个掉了，Bot 周额就成了 `None`，界面显示
/// 「无」而不是真实的「已用尽」。宁可多花几百毫秒重来一次。
async fn call(
    http: &reqwest::Client,
    url: &str,
    cookie: &str,
    body: Option<Value>,
) -> Option<Value> {
    const ATTEMPTS: u32 = 3;
    for attempt in 0..ATTEMPTS {
        let last = attempt + 1 == ATTEMPTS;
        let mut req = match &body {
            Some(b) => http.post(url).json(b),
            None => http.get(url),
        };
        req = req
            .header("Cookie", cookie)
            .header("User-Agent", UA)
            .header("Accept", "application/json")
            .header("Origin", "https://cursor.com")
            .header("Referer", "https://cursor.com/dashboard");

        match req.send().await {
            Ok(res) => {
                let status = res.status();
                // 429 和 5xx 都是「现在不行，等下再来」，不是这个号的稳定答案。
                //
                // 429 以前混在 4xx 里当稳定答案处理，直接返回 None —— 那意味着一次限流会
                // 被读成「这个号没有这项数据」，然后一份空快照覆盖掉好数据。其余 4xx 才是
                // 真正稳定的答案（这个号就是没这项），重试没意义。
                let retryable =
                    status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
                if retryable {
                    if !last {
                        let after = res
                            .headers()
                            .get(reqwest::header::RETRY_AFTER)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_owned);
                        tokio::time::sleep(backoff(attempt, after.as_deref())).await;
                        continue;
                    }
                    // 试到底还是不行：**当作拿不到**，不要把错误体交出去。
                    // 交出去的话，`parse` 会从里面读不到任何字段，于是一份「计划未知、
                    // 用量全空」的快照会覆盖掉上一份好数据，而且不留错误——用户看到的是
                    // 「刚刚查过，没问题」。更糟的是错误体里若含 unauthorized 字样，
                    // 这个号会被判死。
                    tracing::warn!(url, %status, "上游持续不可用，放弃这一项");
                    return None;
                }
                let text = res.text().await.ok()?;
                return match serde_json::from_str(&text) {
                    Ok(v) => Some(v),
                    // 走到这里多半不是「接口改了」，而是压根没走到接口：被挡在防护层
                    // 会回一整页 HTML。上层只会说「网络或响应异常」，不留下这一笔就
                    // 查不出到底是被挡了、还是真的网络不通。
                    Err(_) => {
                        tracing::warn!(
                            url,
                            %status,
                            head = %text.chars().take(120).collect::<String>(),
                            "响应不是 JSON"
                        );
                        None
                    }
                };
            }
            Err(_) if !last => {
                tokio::time::sleep(backoff(attempt, None)).await;
            }
            Err(err) => {
                tracing::warn!(url, %err, "请求失败");
                return None;
            }
        }
    }
    None
}

/// Cursor 对失效会话返回 **200 + `{"error":"not_authenticated"}`**，状态码看不出来。
pub(crate) fn unauthenticated(json: Option<&Value>) -> bool {
    let Some(j) = json else { return false };
    let matches = |s: &str| {
        let s = s.to_ascii_lowercase();
        s.contains("not_authenticated")
            || s.contains("unauthorized")
            || s.contains("not authenticated")
    };
    match j.get("error") {
        Some(Value::String(s)) => matches(s),
        Some(obj) => obj
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(matches),
        None => false,
    }
}

// ── 解析。字段路径全部按真实响应核对过（规格见 cursorUsage.ts）。 ──────────────

fn num(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite()),
        // 非有限值也要挡掉：`"NaN"` 能被 parse 出来，但之后 serde_json 序列化不了它，
        // 整份用量都会写不进库。
        Value::String(s) if !s.trim().is_empty() => {
            s.trim().parse::<f64>().ok().filter(|f| f.is_finite())
        }
        _ => None,
    }
}

fn int(v: Option<&Value>) -> i64 {
    num(v).map(|f| f.round() as i64).unwrap_or(0)
}

fn text(v: Option<&Value>) -> Option<String> {
    let s = v?.as_str()?.trim();
    (!s.is_empty()).then(|| s.to_string())
}

fn flag(v: Option<&Value>) -> Option<bool> {
    (v? == &Value::Bool(true)).then_some(true)
}

/// 真假都要认。`flag` 把 `false` 收成 `None`，读 `noUsageBasedAllowed: false` 会丢。
fn as_bool(v: Option<&Value>) -> Option<bool> {
    match v? {
        Value::Bool(b) => Some(*b),
        _ => None,
    }
}

/// 赠送积分余额。空对象 = 这个号没有；`hasCreditGrants: true` 才落数字。
fn apply_credit_grants(usage: &mut AccountUsage, json: Option<&Value>) {
    let Some(json) = json else { return };
    if unauthenticated(Some(json)) {
        return;
    }
    if json.as_object().is_some_and(|o| o.is_empty()) {
        return;
    }
    let has = as_bool(json.get("hasCreditGrants"));
    let total = num(json.get("totalCents"));
    let used = num(json.get("usedCents"));
    // 真机抓包里余额那格叫 `creditBalanceCents`；`remainingCents` 是明细接口的写法，两个都认。
    let remaining = num(json.get("creditBalanceCents"))
        .or_else(|| num(json.get("remainingCents")))
        .or_else(|| match (total, used) {
            (Some(t), Some(u)) => Some((t - u).max(0.0)),
            (Some(t), None) => Some(t),
            _ => None,
        });
    if has == Some(false) && total.unwrap_or(0.0) <= 0.0 {
        return;
    }
    if has != Some(true) && total.is_none() && remaining.is_none() {
        return;
    }
    usage.credit_grant_total_cents = total;
    usage.credit_grant_used_cents = used;
    usage.credit_grant_remaining_cents = remaining;
}

/// 赠送积分的逐笔明细。数字字段在 JSON 里是**字符串**（protobuf int64 的 JSON 写法），
/// `num()` 两种都认。没有 `grants` 或为空 = 没有赠送，不落数组。
fn apply_credit_grant_list(usage: &mut AccountUsage, json: Option<&Value>) {
    let Some(json) = json else { return };
    if unauthenticated(Some(json)) {
        return;
    }
    let Some(list) = json.get("grants").and_then(Value::as_array) else {
        return;
    };
    let grants: Vec<CreditGrant> = list
        .iter()
        .filter_map(|g| {
            let total = num(g.get("totalCents"))?;
            Some(CreditGrant {
                display_name: text(g.get("displayName")),
                total_cents: total,
                remaining_cents: num(g.get("remainingCents")).unwrap_or(total),
                expires_at: ts(g.get("expiresAtMs")),
            })
        })
        .collect();
    if grants.is_empty() {
        return;
    }
    // 只有明细、没有余额接口时，用明细把总数补齐——两边说的是同一笔钱。
    if usage.credit_grant_total_cents.is_none() {
        let total: f64 = grants.iter().map(|g| g.total_cents).sum();
        let remaining: f64 = grants.iter().map(|g| g.remaining_cents).sum();
        usage.credit_grant_total_cents = Some(total);
        usage.credit_grant_remaining_cents = Some(remaining);
        usage.credit_grant_used_cents = Some((total - remaining).max(0.0));
    }
    usage.credit_grants = Some(grants);
}

/// Spending 页的开关以 `GetHardLimit` 为准。usage-summary 的 `onDemand.enabled`
/// 是「有没有按需消费过」，改开关之后经常还是旧的。
fn apply_hard_limit(usage: &mut AccountUsage, json: Option<&Value>) {
    let Some(json) = json else { return };
    if unauthenticated(Some(json)) {
        return;
    }
    let Some(no) = as_bool(json.get("noUsageBasedAllowed")) else {
        return;
    };
    usage.on_demand_enabled = Some(!no);
    if no {
        return;
    }
    usage.on_demand_limit_cents = match num(json.get("hardLimit")) {
        Some(dollars) if is_unlimited_dollars(dollars) => Some(None),
        Some(dollars) if dollars > 0.0 => Some(Some(dollars * 100.0)),
        // 开着但上限 $0 = Spending 页的 Fixed $0，不是「不封顶」。旧实现把 0 读成
        // 不封顶，库里那份快照会让自动配置跳过重写。
        Some(_) => Some(Some(0.0)),
        None => Some(None),
    };
}

/// 账期时间戳有两种写法：ISO 串（usage-summary）和毫秒数字串（sand 那两个）。
fn ts(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => n.as_i64().filter(|x| *x > 0),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            if s.chars().all(|c| c.is_ascii_digit()) {
                let n: i64 = s.parse().ok()?;
                // 秒级时间戳（10 位）要补成毫秒。
                return (n > 0).then_some(if n < 1_000_000_000_000 { n * 1000 } else { n });
            }
            parse_iso_millis(s)
        }
        _ => None,
    }
}

fn parse_iso_millis(raw: &str) -> Option<i64> {
    let t = nexus_core::clock::parse_iso(raw)?;
    Some((t.unix_timestamp_nanos() / 1_000_000) as i64)
}

/// 百分比按 Cursor 原样保留小数，只夹到 0..100 —— 超过 100 的桶展示成 100 就够了。
fn pct(v: Option<&Value>) -> Option<f64> {
    num(v).map(|n| n.clamp(0.0, 100.0))
}

fn obj<'a>(v: Option<&'a Value>, key: &str) -> Option<&'a Value> {
    v?.get(key).filter(|x| x.is_object())
}

fn parse(
    summary: &Value,
    stripe: Option<&Value>,
    me: Option<&Value>,
    agg: Option<&Value>,
    sand_usage: Option<&Value>,
    sand_access: Option<&Value>,
) -> AccountUsage {
    let individual = obj(Some(summary), "individualUsage");
    let plan = obj(individual, "plan");
    let breakdown = obj(plan, "breakdown");
    let on_demand = obj(individual, "onDemand");

    // membershipType 三处都有，优先级：usage-summary > stripe 的个人档 > stripe 的总档。
    // 个人档比总档准：团队成员的总档会显示 team，但实际吃的是他个人的额度。
    let plan_name = text(summary.get("membershipType"))
        .or_else(|| text(stripe.and_then(|s| s.get("individualMembershipType"))))
        .or_else(|| text(stripe.and_then(|s| s.get("membershipType"))));

    let mut usage = AccountUsage {
        fetched_at: now_iso(),
        email: text(me.and_then(|m| m.get("email"))),
        account_created_at: text(me.and_then(|m| m.get("created_at"))),
        plan: plan_name,
        subscription_status: text(stripe.and_then(|s| s.get("subscriptionStatus"))),
        is_yearly_plan: flag(stripe.and_then(|s| s.get("isYearlyPlan"))),
        is_team_member: flag(stripe.and_then(|s| s.get("isTeamMember"))),
        pending_cancellation_date: text(stripe.and_then(|s| s.get("pendingCancellationDate"))),
        cycle_start: ts(summary.get("billingCycleStart")),
        cycle_end: ts(summary.get("billingCycleEnd")),
        bot: parse_bot(sand_usage, sand_access),
        total_percent_used: pct(plan.and_then(|p| p.get("totalPercentUsed"))),
        auto_percent_used: pct(plan.and_then(|p| p.get("autoPercentUsed"))),
        api_percent_used: pct(plan.and_then(|p| p.get("apiPercentUsed"))),
        included_cents: num(breakdown.and_then(|b| b.get("included"))),
        bonus_cents: num(breakdown.and_then(|b| b.get("bonus"))),
        // breakdown.total 是 included + bonus 的真实消费；plan.used 会被 limit 截顶
        // （用超了也只显示 2000/2000），只能当兜底。
        spend_cents: num(breakdown.and_then(|b| b.get("total")))
            .or_else(|| num(plan.and_then(|p| p.get("used")))),
        plan_limit_cents: num(plan.and_then(|p| p.get("limit"))),
        on_demand_enabled: flag(on_demand.and_then(|o| o.get("enabled"))),
        on_demand_used_cents: num(on_demand.and_then(|o| o.get("used"))),
        on_demand_limit_cents: on_demand.map(|o| match o.get("limit") {
            None | Some(Value::Null) => None,
            other => num(other),
        }),
        ..Default::default()
    };

    if let Some(agg) = agg {
        let a = aggregate(agg);
        usage.input_tokens = Some(a.input);
        usage.output_tokens = Some(a.output);
        usage.cache_read_tokens = Some(a.cache_read);
        usage.cache_write_tokens = Some(a.cache_write);
        if !a.by_model.is_empty() {
            usage.by_model = Some(a.by_model);
        }
    }
    usage
}

/// `get-aggregated-usage-events` 的一份响应拆开：四个 token 总数 + 按模型的行 + 所有行的花费和。
struct Aggregated {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_write: i64,
    /// 花费从多到少。没有模型名的行不进来 —— 它们在界面上没地方摆。
    by_model: Vec<ModelUsage>,
    /// **所有**行的 `totalCents` 之和，包括没有模型名的那些：钱不该因为一行少了个名字就漏掉。
    cents: f64,
}

fn aggregate(agg: &Value) -> Aggregated {
    let rows = agg
        .get("aggregations")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut by_model: Vec<ModelUsage> = rows
        .iter()
        .filter_map(|r| {
            let model = text(r.get("modelIntent")).or_else(|| text(r.get("model")))?;
            Some(ModelUsage {
                model,
                tier: num(r.get("tier")),
                cents: num(r.get("totalCents")).unwrap_or(0.0),
                input: int(r.get("inputTokens")),
                output: int(r.get("outputTokens")),
                cache_read: int(r.get("cacheReadTokens")),
                cache_write: int(r.get("cacheWriteTokens")),
            })
        })
        .collect();
    by_model.sort_by(|a, b| b.cents.total_cmp(&a.cents));
    Aggregated {
        input: int(agg.get("totalInputTokens")),
        output: int(agg.get("totalOutputTokens")),
        cache_read: int(agg.get("totalCacheReadTokens")),
        cache_write: int(agg.get("totalCacheWriteTokens")),
        by_model,
        cents: rows.iter().filter_map(|r| num(r.get("totalCents"))).sum(),
    }
}

/// 把带日期那一问的响应装成一个时间窗。
///
/// 响应若自报了 `period` 而它不是我们要的那段（比如接口无视日期、退回了本账期），
/// 宁可不给 —— 一个错的「今天花了 $21」比一个「—」糟得多。差一天以内算对得上：
/// 上游可能把起点归整到它自己的日界。
fn window(start: i64, end: i64, agg: Option<&Value>) -> Option<UsageWindow> {
    let agg = agg?;
    if let Some(got) = ts(agg.get("period").and_then(|p| p.get("startDate"))) {
        if (got - start).abs() > DAY_MS {
            tracing::warn!(
                want = start,
                got,
                "聚合接口回的不是要的那段，丢弃这个时间窗"
            );
            return None;
        }
    }
    let a = aggregate(agg);
    Some(UsageWindow {
        start,
        end,
        cents: a.cents,
        input_tokens: a.input,
        output_tokens: a.output,
        cache_read_tokens: a.cache_read,
        cache_write_tokens: a.cache_write,
        by_model: a.by_model,
    })
}

fn parse_bot(usage: Option<&Value>, access: Option<&Value>) -> Option<BotQuota> {
    let percent_used = pct(usage.and_then(|u| u.get("usagePercent")));
    let reset_at = ts(usage.and_then(|u| u.get("nextResetTimestampUtc")));
    let raw_state = text(access.and_then(|a| a.get("state")));
    // 三样都没有 = 这个号没有 Bot 通道（老 pro-legacy 号返回空对象）。
    if percent_used.is_none() && reset_at.is_none() && raw_state.is_none() {
        return None;
    }

    let mut bot = BotQuota {
        percent_used,
        period_start: ts(usage.and_then(|u| u.get("currentPeriodStart"))),
        reset_at,
        has_available: usage
            .and_then(|u| u.get("hasAvailableUsage"))
            .and_then(Value::as_bool),
        plan_label: text(usage.and_then(|u| u.get("grokPlanLabel"))),
        ..Default::default()
    };
    if let Some(state) = raw_state {
        let granted = state == "SAND_ACCESS_STATE_GRANTED";
        bot.access = Some(if granted { "granted" } else { "blocked" }.to_string());
        if !granted {
            // NONE 是「没有阻断原因」的占位，带上去只会让界面显示一句废话。
            bot.block_reason = text(access.and_then(|a| a.get("blockReason")))
                .filter(|r| r != "SAND_ACCESS_BLOCK_REASON_NONE");
        }
    }
    Some(bot)
}

pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .user_agent(UA)
        .build()
        .expect("构造 HTTP 客户端失败")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retry_after_from_upstream_wins_over_our_guess() {
        // 上游说了等多久就等多久，它比我们瞎猜准。
        assert_eq!(backoff(0, Some("2")), Duration::from_secs(2));
        assert_eq!(backoff(2, Some("1")), Duration::from_secs(1));
        // 但不能被一个离谱的值把整批刷号拖住。
        assert_eq!(backoff(0, Some("3600")), Duration::from_secs(5));
        // 看不懂的头就当没有，回到退避。
        assert!(backoff(0, Some("Wed, 21 Oct 2026 07:28:00 GMT")) < Duration::from_secs(1));
    }

    #[test]
    fn backoff_grows_and_stays_bounded() {
        let d0 = backoff(0, None);
        let d1 = backoff(1, None);
        let d2 = backoff(2, None);
        assert!(
            d0 < d1 && d1 < d2,
            "退避要一次比一次久：{d0:?} {d1:?} {d2:?}"
        );
        assert!(d0 >= Duration::from_millis(400));
        for attempt in 0..8 {
            assert!(backoff(attempt, None) <= Duration::from_secs(5));
        }
    }
    use serde_json::json;

    /// 一份贴着真实响应形状的 usage-summary。
    fn summary() -> Value {
        json!({
            "membershipType": "ultra",
            "billingCycleStart": "2026-08-15T00:00:00.000Z",
            "billingCycleEnd": "2026-09-15T00:00:00.000Z",
            "individualUsage": {
                "plan": {
                    "totalPercentUsed": 42.5,
                    "autoPercentUsed": 61.25,
                    "apiPercentUsed": 12,
                    "used": 2000,
                    "limit": 2000,
                    "breakdown": { "included": 1800, "bonus": 450, "total": 2250 }
                },
                "onDemand": { "enabled": true, "used": 315, "limit": null }
            }
        })
    }

    #[test]
    fn parses_the_headline_numbers() {
        let u = parse(&summary(), None, None, None, None, None);
        assert_eq!(u.plan.as_deref(), Some("ultra"));
        assert_eq!(u.total_percent_used, Some(42.5));
        assert_eq!(u.auto_percent_used, Some(61.25));
        assert_eq!(u.api_percent_used, Some(12.0));
        assert_eq!(u.plan_limit_cents, Some(2000.0));
        assert!(u.cycle_end.unwrap() > u.cycle_start.unwrap());
    }

    #[test]
    fn spend_prefers_breakdown_total_over_the_capped_used() {
        // plan.used 被 limit 截顶成 2000，真实消费是 2250。显示 2000 会让人以为
        // 「刚好用完」，而实际已经超支 250。
        let u = parse(&summary(), None, None, None, None, None);
        assert_eq!(u.spend_cents, Some(2250.0));
        assert_eq!(u.included_cents, Some(1800.0));
        assert_eq!(u.bonus_cents, Some(450.0));
    }

    #[test]
    fn spend_falls_back_to_used_when_there_is_no_breakdown() {
        let s = json!({ "individualUsage": { "plan": { "used": 1234 } } });
        assert_eq!(
            parse(&s, None, None, None, None, None).spend_cents,
            Some(1234.0)
        );
    }

    #[test]
    fn uncapped_on_demand_is_distinguishable_from_absent() {
        // limit: null + enabled → 不封顶；整个 onDemand 缺席 → 字段是 None。
        let u = parse(&summary(), None, None, None, None, None);
        assert_eq!(u.on_demand_enabled, Some(true));
        assert_eq!(u.on_demand_used_cents, Some(315.0));
        assert_eq!(u.on_demand_limit_cents, Some(None), "null = 不封顶");

        let bare = json!({ "individualUsage": { "plan": {} } });
        assert_eq!(
            parse(&bare, None, None, None, None, None).on_demand_limit_cents,
            None
        );
    }

    #[test]
    fn hard_limit_body_enables_with_a_dollar_cap() {
        let v = hard_limit_body(true, Some(5_000.0));
        assert_eq!(v["noUsageBasedAllowed"], false);
        assert_eq!(v["hardLimit"], 50);
    }

    #[test]
    fn hard_limit_body_enables_unlimited_as_i32_max() {
        let v = hard_limit_body(true, None);
        assert_eq!(v["noUsageBasedAllowed"], false);
        assert_eq!(v["hardLimit"], UNLIMITED_HARD_LIMIT_DOLLARS);
        assert_eq!(v["preserveHardLimitPerUser"], false);
        assert_eq!(v["isDynamicTeamLimit"], false);
    }

    #[test]
    fn hard_limit_body_disables_usage_based() {
        let v = hard_limit_body(false, Some(2_000.0));
        assert_eq!(v["noUsageBasedAllowed"], true);
        assert_eq!(v["hardLimit"], 0, "关掉必须带 0，不能把旧上限捎回去");
        let bare = hard_limit_body(false, None);
        assert_eq!(bare["hardLimit"], 0);
        assert_eq!(bare["noUsageBasedAllowed"], true);
    }

    #[test]
    fn credit_grants_are_the_gifted_balance_not_cycle_spend() {
        let mut u = AccountUsage::default();
        apply_credit_grants(
            &mut u,
            Some(&json!({
                "hasCreditGrants": true,
                "totalCents": 2500,
                "usedCents": 400
            })),
        );
        assert_eq!(u.credit_grant_total_cents, Some(2500.0));
        assert_eq!(u.credit_grant_used_cents, Some(400.0));
        assert_eq!(u.credit_grant_remaining_cents, Some(2100.0));

        let mut empty = AccountUsage::default();
        apply_credit_grants(&mut empty, Some(&json!({})));
        assert!(empty.credit_grant_remaining_cents.is_none());

        // 真机抓包（cursor.com.har，2026-09-15）的形状：余额叫 creditBalanceCents，数字是字符串。
        let mut real = AccountUsage::default();
        apply_credit_grants(
            &mut real,
            Some(&json!({
                "hasCreditGrants": true,
                "creditBalanceCents": "2252",
                "totalCents": "2500",
                "usedCents": "248"
            })),
        );
        assert_eq!(real.credit_grant_remaining_cents, Some(2252.0));
    }

    /// 明细接口给的是「哪一笔、叫什么、什么时候过期」——仪表盘上那行 "Power user grant" 就来自它。
    #[test]
    fn credit_grant_list_carries_names_and_expiry() {
        let mut u = AccountUsage::default();
        apply_credit_grant_list(
            &mut u,
            Some(&json!({
                "grants": [{
                    "remainingCents": "2252",
                    "totalCents": "2500",
                    "expiresAtMs": "1792073218986",
                    "displayName": "Power user grant"
                }]
            })),
        );
        let g = &u.credit_grants.as_ref().unwrap()[0];
        assert_eq!(g.display_name.as_deref(), Some("Power user grant"));
        assert_eq!(g.total_cents, 2500.0);
        assert_eq!(g.remaining_cents, 2252.0);
        assert_eq!(g.expires_at, Some(1_792_073_218_986));
        // 余额接口没回来时，用明细补总数。
        assert_eq!(u.credit_grant_total_cents, Some(2500.0));
        assert_eq!(u.credit_grant_used_cents, Some(248.0));

        let mut none = AccountUsage::default();
        apply_credit_grant_list(&mut none, Some(&json!({ "grants": [] })));
        assert!(none.credit_grants.is_none());
    }

    #[test]
    fn hard_limit_overlay_is_the_spending_toggle() {
        let mut u = parse(&summary(), None, None, None, None, None);
        // usage-summary 说开着且不封顶；GetHardLimit 才是 Spending 页那一档。
        apply_hard_limit(
            &mut u,
            Some(&json!({ "hardLimit": 0, "noUsageBasedAllowed": true })),
        );
        assert_eq!(u.on_demand_enabled, Some(false));

        apply_hard_limit(
            &mut u,
            Some(&json!({ "hardLimit": 50, "noUsageBasedAllowed": false })),
        );
        assert_eq!(u.on_demand_enabled, Some(true));
        assert_eq!(u.on_demand_limit_cents, Some(Some(5_000.0)));

        // i32::MAX = 仪表盘 No Limit，不是二十亿刀的上限。
        apply_hard_limit(
            &mut u,
            Some(&json!({ "hardLimit": 2_147_483_647i64, "noUsageBasedAllowed": false })),
        );
        assert_eq!(u.on_demand_enabled, Some(true));
        assert_eq!(u.on_demand_limit_cents, Some(None));

        // 开着但上限 $0 = Fixed $0，不是不封顶。
        apply_hard_limit(
            &mut u,
            Some(&json!({ "hardLimit": 0, "noUsageBasedAllowed": false })),
        );
        assert_eq!(u.on_demand_enabled, Some(true));
        assert_eq!(u.on_demand_limit_cents, Some(Some(0.0)));
    }

    #[test]
    fn hard_limit_stuck_rejects_a_zero_cap_when_we_asked_for_unlimited() {
        assert!(hard_limit_stuck(
            &json!({ "hardLimit": 2_147_483_647i64, "noUsageBasedAllowed": false }),
            true,
            None,
        ));
        assert!(
            !hard_limit_stuck(
                &json!({ "hardLimit": 0, "noUsageBasedAllowed": false }),
                true,
                None,
            ),
            "Fixed $0 不能当不封顶的回执"
        );
        assert!(!hard_limit_stuck(
            &json!({ "hardLimit": 50, "noUsageBasedAllowed": true }),
            true,
            None,
        ));
    }

    #[test]
    fn individual_membership_beats_the_team_wide_one() {
        // 团队成员的总档显示 team，但派单吃的是他个人的额度。
        let s = json!({ "individualUsage": { "plan": {} } });
        let stripe = json!({ "membershipType": "team", "individualMembershipType": "pro" });
        assert_eq!(
            parse(&s, Some(&stripe), None, None, None, None)
                .plan
                .as_deref(),
            Some("pro")
        );
    }

    #[test]
    fn percentages_are_clamped_but_keep_their_decimals() {
        let s = json!({ "individualUsage": { "plan": {
            "totalPercentUsed": 137.9, "autoPercentUsed": -3, "apiPercentUsed": "55.5"
        }}});
        let u = parse(&s, None, None, None, None, None);
        assert_eq!(u.total_percent_used, Some(100.0));
        assert_eq!(u.auto_percent_used, Some(0.0));
        assert_eq!(u.api_percent_used, Some(55.5), "字符串数字也要认");
    }

    #[test]
    fn bot_quota_is_absent_for_legacy_accounts() {
        // 老号这两个接口返回空对象。造一个 0% 的假象会让界面显示「额度充足」。
        let empty = json!({});
        assert!(parse_bot(Some(&empty), Some(&empty)).is_none());
        assert!(parse_bot(None, None).is_none());
    }

    #[test]
    fn bot_quota_parses_weekly_reset_and_access() {
        let usage = json!({
            "usagePercent": 87.5,
            "currentPeriodStart": "1756800000000",
            "nextResetTimestampUtc": "1757404800000",
            "hasAvailableUsage": true,
            "grokPlanLabel": "Grok Bot Plan"
        });
        let access = json!({ "state": "SAND_ACCESS_STATE_GRANTED", "blockReason": "SAND_ACCESS_BLOCK_REASON_NONE" });
        let bot = parse_bot(Some(&usage), Some(&access)).unwrap();
        assert_eq!(bot.percent_used, Some(87.5));
        assert_eq!(bot.reset_at, Some(1_757_404_800_000));
        assert_eq!(bot.access.as_deref(), Some("granted"));
        assert_eq!(bot.has_available, Some(true));
        assert_eq!(bot.plan_label.as_deref(), Some("Grok Bot Plan"));
        assert!(bot.block_reason.is_none());
    }

    #[test]
    fn a_blocked_bot_channel_reports_a_real_reason_only() {
        let usage = json!({ "usagePercent": 0 });
        let blocked = json!({ "state": "SAND_ACCESS_STATE_BLOCKED", "blockReason": "SAND_ACCESS_BLOCK_REASON_ABUSE" });
        let bot = parse_bot(Some(&usage), Some(&blocked)).unwrap();
        assert_eq!(bot.access.as_deref(), Some("blocked"));
        assert_eq!(
            bot.block_reason.as_deref(),
            Some("SAND_ACCESS_BLOCK_REASON_ABUSE")
        );

        // NONE 是占位，不该显示出来。
        let placeholder = json!({ "state": "SAND_ACCESS_STATE_BLOCKED", "blockReason": "SAND_ACCESS_BLOCK_REASON_NONE" });
        assert!(parse_bot(Some(&usage), Some(&placeholder))
            .unwrap()
            .block_reason
            .is_none());
    }

    #[test]
    fn model_breakdown_is_sorted_by_spend() {
        let agg = json!({
            "totalInputTokens": 1000, "totalOutputTokens": "2000",
            "totalCacheReadTokens": 3000, "totalCacheWriteTokens": 4000,
            "aggregations": [
                { "modelIntent": "auto", "tier": 2, "totalCents": 10, "inputTokens": 1 },
                { "model": "claude-4-opus", "tier": 1, "totalCents": 250, "inputTokens": 2 },
                { "totalCents": 999 },
                { "modelIntent": "gpt-5", "tier": 1, "totalCents": 30 }
            ]
        });
        let u = parse(&summary(), None, None, Some(&agg), None, None);
        let models = u.by_model.unwrap();
        assert_eq!(models.len(), 3, "没有模型名的那行要丢掉");
        assert_eq!(models[0].model, "claude-4-opus");
        assert_eq!(models[0].cents, 250.0);
        assert_eq!(models[2].model, "auto");
        assert_eq!(u.output_tokens, Some(2000), "字符串数字也要认");
    }

    #[test]
    fn a_missing_optional_endpoint_degrades_silently() {
        // stripe / me / agg / sand 全挂，usage-summary 还在：核心数据照样落地。
        let u = parse(&summary(), None, None, None, None, None);
        assert_eq!(u.total_percent_used, Some(42.5));
        assert!(u.email.is_none() && u.bot.is_none() && u.by_model.is_none());
    }

    #[test]
    fn detects_the_200_ok_not_authenticated_response() {
        assert!(unauthenticated(Some(
            &json!({ "error": "not_authenticated" })
        )));
        assert!(unauthenticated(Some(
            &json!({ "error": { "message": "Unauthorized" } })
        )));
        assert!(!unauthenticated(Some(&json!({ "error": "rate_limited" }))));
        assert!(!unauthenticated(Some(&summary())));
        assert!(!unauthenticated(None));
    }

    #[test]
    fn timestamps_accept_iso_seconds_and_millis() {
        assert_eq!(
            ts(Some(&json!("2026-09-02T00:00:00Z"))),
            Some(1_788_307_200_000)
        );
        assert_eq!(ts(Some(&json!("1757404800000"))), Some(1_757_404_800_000));
        assert_eq!(
            ts(Some(&json!("1757404800"))),
            Some(1_757_404_800_000),
            "秒要补成毫秒"
        );
        assert_eq!(
            ts(Some(&json!(1_757_404_800_000i64))),
            Some(1_757_404_800_000)
        );
        assert_eq!(ts(Some(&json!(""))), None);
        assert_eq!(ts(Some(&json!("昨天"))), None);
        assert_eq!(ts(None), None);
    }

    #[test]
    fn session_cookie_decodes_an_encoded_separator() {
        assert_eq!(
            session_cookie("user_1%3A%3Ajwt"),
            "WorkosCursorSessionToken=user_1::jwt"
        );
        assert_eq!(
            session_cookie("user_1::jwt"),
            "WorkosCursorSessionToken=user_1::jwt"
        );
    }

    #[tokio::test]
    async fn a_malformed_session_token_fails_fast_without_a_request() {
        let http = http_client();
        for bad in ["", "just-a-jwt", "user_1", "abc::def"] {
            let err = fetch(&http, bad, None).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::Unauthorized, "应当拒绝 {bad:?}");
        }
    }

    #[test]
    fn a_window_sums_every_row_but_ranks_only_named_models() {
        let agg = json!({
            "totalInputTokens": 500, "totalOutputTokens": 60,
            "aggregations": [
                { "modelIntent": "claude-sonnet-5", "tier": 1, "totalCents": 120 },
                { "modelIntent": "auto", "tier": 2, "totalCents": 30 },
                // 没有模型名的行：钱要算，排行里没它。
                { "totalCents": 5 }
            ]
        });
        let w = window(1_000, 2_000, Some(&agg)).unwrap();
        assert_eq!((w.start, w.end), (1_000, 2_000));
        assert_eq!(w.cents, 155.0);
        assert_eq!(w.input_tokens, 500);
        assert_eq!(w.by_model.len(), 2);
        assert_eq!(w.by_model[0].model, "claude-sonnet-5");
    }

    #[test]
    fn api_key_events_become_a_partial_usage_snapshot() {
        let events = vec![
            UsageEvent {
                ts: 2_000,
                model: "cursor-grok".into(),
                charged_cents: 80.0,
                input: 10,
                output: 2,
                cache_read: 4,
                cache_write: 0,
                plan: Some("pro-legacy".into()),
            },
            UsageEvent {
                ts: 500,
                model: "cursor-grok".into(),
                charged_cents: 20.0,
                input: 5,
                output: 1,
                cache_read: 0,
                cache_write: 0,
                plan: Some("pro-legacy".into()),
            },
        ];
        let u = events_to_usage(&events, 2_500, Some(1_000), Some(0));
        assert_eq!(u.via.as_deref(), Some("apiKey"));
        assert_eq!(u.spend_cents, Some(100.0));
        assert_eq!(u.plan.as_deref(), Some("pro-legacy"));
        assert_eq!(u.today.as_ref().map(|w| w.cents), Some(80.0));
        assert_eq!(u.week.as_ref().map(|w| w.cents), Some(100.0));
        assert!(u.total_percent_used.is_none());
        assert!(u.bot.is_none());
    }

    #[test]
    fn a_window_is_dropped_when_upstream_answers_a_different_period() {
        // 接口无视日期、退回本账期：宁可没有，也不能把整月的钱标成「今天」。
        let start = 1_757_116_800_000i64;
        let agg = json!({
            "period": { "startDate": (start - 20 * DAY_MS).to_string(), "endDate": "1757462399999" },
            "aggregations": [{ "modelIntent": "auto", "totalCents": 999 }]
        });
        assert!(window(start, start + 3_600_000, Some(&agg)).is_none());

        // 起点只差几小时（上游按自己的日界归整）算对得上。
        let close = json!({
            "period": { "startDate": (start - 3 * 3_600_000).to_string() },
            "aggregations": [{ "modelIntent": "auto", "totalCents": 7 }]
        });
        assert_eq!(window(start, start + 1, Some(&close)).unwrap().cents, 7.0);

        // 没自报 period 的照单全收；没响应的就是没有。
        assert!(window(start, start + 1, Some(&json!({ "aggregations": [] }))).is_some());
        assert!(window(start, start + 1, None).is_none());
    }

    #[test]
    fn serialized_windows_ride_along_and_stay_optional() {
        let mut u = parse(&summary(), None, None, None, None, None);
        assert!(serde_json::to_value(&u).unwrap().get("today").is_none());
        u.today = Some(UsageWindow {
            start: 1,
            end: 2,
            cents: 3.0,
            ..Default::default()
        });
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["today"]["cents"], 3.0);
        // 旧快照里没有这两个字段，读回来也得成。
        let back: AccountUsage = serde_json::from_value(json!({ "fetchedAt": "x" })).unwrap();
        assert!(back.today.is_none() && back.week.is_none());
    }

    #[test]
    fn serialized_usage_omits_absent_fields() {
        let u = parse(
            &json!({ "individualUsage": { "plan": {} } }),
            None,
            None,
            None,
            None,
            None,
        );
        let v = serde_json::to_value(&u).unwrap();
        assert!(v.get("bot").is_none(), "缺席的字段不该以 null 出现在前端");
        assert!(v.get("fetchedAt").is_some());
    }
}
