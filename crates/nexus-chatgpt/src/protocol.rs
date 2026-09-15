//! Codex 后端（`chatgpt.com/backend-api`）的公共协议事实：身份头、额度。
//!
//! 这里只放**账号侧和网关侧都要用**的那部分——拉额度、拉模型清单要带的头，以及额度的形状。
//! 请求体怎么改、身份怎么按账号收敛、错误怎么分类，那些只有推理路径关心，在
//! `nexus-gateway` 的 `codex` 模块里。协议事实的来源逐条见 `docs/relay/CODEX-OAUTH.md` §1。

use serde::{Deserialize, Serialize};
use std::time::Duration;
use time::OffsetDateTime;

/// 推理与目录、额度接口共用的根。测试时换成假上游。
pub const DEFAULT_BACKEND_URL: &str = "https://chatgpt.com/backend-api";

/// `originator` 与 User-Agent 首段必须配套（错配是 404），两者从同一个常量派生。
pub const ORIGINATOR: &str = "codex-tui";

/// 我们自称的 Codex CLI 版本。低于 `0.144.0` 直接 404，陈旧版本高峰期会被优先降载
/// （HTTP 200 + 流内 `server_is_overloaded`），模型目录里每个模型还有 `minimal_client_version`。
/// 桌面端跟着应用发版一起升；云端网关那边是从 npm 自动同步的。
pub const CLIENT_VERSION: &str = "0.153.4";

/// 提前多久续 access token。它约十天有效；按天提前，网关拿号时几乎永远不用现刷。
pub const REFRESH_AHEAD: Duration = Duration::from_secs(24 * 60 * 60);

/// codex-rs 的 UA 形态：`{originator}/{ver} ({os}; {arch}) {terminal} ({name}; {ver})`。
/// 中段的 OS / 架构 / 终端指纹不能省，尾部括号组是新版客户端才有的，两处版本号一致。
pub fn user_agent() -> String {
    format!("{ORIGINATOR}/{CLIENT_VERSION} (Ubuntu 22.4.0; x86_64) xterm-256color ({ORIGINATOR}; {CLIENT_VERSION})")
}

/// 账号侧对 `chatgpt.com/backend-api/*`（额度、目录）说话时的一组头。`account_id` 是
/// `chatgpt_account_id`，多组织的号缺了会走错组织。
pub fn identity_headers(
    access_token: &str,
    account_id: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut h = vec![
        ("authorization", format!("Bearer {access_token}")),
        ("user-agent", user_agent()),
        ("originator", ORIGINATOR.to_string()),
        ("version", CLIENT_VERSION.to_string()),
        ("accept", "application/json".to_string()),
    ];
    if let Some(a) = account_id.map(str::trim).filter(|a| !a.is_empty()) {
        h.push(("chatgpt-account-id", a.to_string()));
    }
    h
}

// ---------------------------------------------------------------------------
// 额度
// ---------------------------------------------------------------------------

/// 一个滚动窗口。primary 通常是 5 小时，secondary 是 7 天——**以 `window_minutes` 为准**，
/// 上游改过窗口长度。
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageWindow {
    pub used_percent: Option<f64>,
    /// 窗口重置时刻（Unix 毫秒）。
    pub reset_at_ms: Option<i64>,
    pub window_minutes: Option<u32>,
}

/// `/wham/usage` 里按模型单独计的一桶（Spark 等）。主窗口之外，缺了就当没有，不画成 0%。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitBucket {
    pub name: Option<String>,
    /// `codex_bengalfox` 这类计量键；调度仍按主窗口，这里只给人看。
    pub feature: Option<String>,
    pub allowed: Option<bool>,
    pub limit_reached: Option<bool>,
    pub primary: Option<UsageWindow>,
    pub secondary: Option<UsageWindow>,
}

/// Codex 点数 / 超限额度。没有点数的号 `has_credits=false`、`balance="0"`，别写成「有 0 点」。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexCredits {
    pub has_credits: Option<bool>,
    pub unlimited: Option<bool>,
    pub overage_limit_reached: Option<bool>,
    pub balance: Option<String>,
    /// `rate_limit_reset_credits.available_count`：还能手动重置几次。
    pub reset_available: Option<i64>,
}

impl UsageWindow {
    fn is_empty(&self) -> bool {
        self.used_percent.is_none() && self.reset_at_ms.is_none() && self.window_minutes.is_none()
    }

    fn full(&self) -> bool {
        self.used_percent.is_some_and(|p| p >= 100.0)
    }
}

/// 一个 ChatGPT 账号的 Codex 额度快照。随每次成功响应的头回来，也可以主动问 `/wham/usage`。
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexUsage {
    pub primary: Option<UsageWindow>,
    pub secondary: Option<UsageWindow>,
    pub plan_type: Option<String>,
    /// RFC 3339。
    pub checked_at: String,
    /// `response-headers` | `wham/usage`。
    pub source: String,
    #[serde(default)]
    pub additional: Vec<RateLimitBucket>,
    #[serde(default)]
    pub credits: Option<CodexCredits>,
    #[serde(default)]
    pub allowed: Option<bool>,
    #[serde(default)]
    pub limit_reached: Option<bool>,
    /// 额度接口顶层带回的 `user_id`，登录 JWT 里没有时用它补。
    #[serde(default)]
    pub user_id: Option<String>,
}

fn iso(t: OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

fn text(v: Option<&serde_json::Value>) -> Option<String> {
    v.and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn text_or_num(v: Option<&serde_json::Value>) -> Option<String> {
    text(v).or_else(|| {
        v.and_then(|v| v.as_f64())
            .filter(|n| n.is_finite())
            .map(|n| {
                if n.fract() == 0.0 {
                    format!("{}", n as i64)
                } else {
                    n.to_string()
                }
            })
    })
}

fn window_from_value(w: &serde_json::Value, now_ms: i64) -> Option<UsageWindow> {
    if !w.is_object() {
        return None;
    }
    let num = |k: &str| w.get(k).and_then(|v| v.as_f64()).filter(|n| n.is_finite());
    let reset_at_ms = num("reset_after_seconds")
        .filter(|s| *s > 0.0)
        .map(|s| now_ms + (s * 1000.0) as i64)
        .or_else(|| {
            num("reset_at")
                .filter(|s| *s > 0.0)
                .map(|s| (s * 1000.0) as i64)
        });
    let out = UsageWindow {
        used_percent: num("used_percent"),
        reset_at_ms,
        window_minutes: num("limit_window_seconds").map(|s| (s / 60.0).round() as u32),
    };
    (!out.is_empty()).then_some(out)
}

fn additional_buckets(raw: Option<&serde_json::Value>, now_ms: i64) -> Vec<RateLimitBucket> {
    match raw {
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| rate_limit_bucket(v, None, now_ms))
            .collect(),
        Some(serde_json::Value::Object(map)) => map
            .iter()
            .filter_map(|(k, v)| rate_limit_bucket(v, Some(k.as_str()), now_ms))
            .collect(),
        _ => Vec::new(),
    }
}

fn rate_limit_bucket(
    v: &serde_json::Value,
    fallback_name: Option<&str>,
    now_ms: i64,
) -> Option<RateLimitBucket> {
    let rl = v.get("rate_limit").unwrap_or(v);
    let name = text(v.get("limit_name"))
        .or_else(|| text(v.get("name")))
        .or_else(|| fallback_name.map(str::to_string));
    let feature = text(v.get("metered_feature")).or_else(|| text(v.get("limit_id")));
    let primary = rl
        .get("primary_window")
        .and_then(|w| window_from_value(w, now_ms));
    let secondary = rl
        .get("secondary_window")
        .and_then(|w| window_from_value(w, now_ms));
    if name.is_none() && feature.is_none() && primary.is_none() && secondary.is_none() {
        return None;
    }
    Some(RateLimitBucket {
        name,
        feature,
        allowed: rl.get("allowed").and_then(|x| x.as_bool()),
        limit_reached: rl.get("limit_reached").and_then(|x| x.as_bool()),
        primary,
        secondary,
    })
}

fn credits_from_wham(payload: &serde_json::Value) -> Option<CodexCredits> {
    let c = payload.get("credits");
    let reset = payload
        .get("rate_limit_reset_credits")
        .and_then(|v| v.get("available_count"))
        .and_then(|v| v.as_i64());
    let out = CodexCredits {
        has_credits: c
            .and_then(|v| v.get("has_credits"))
            .and_then(|v| v.as_bool()),
        unlimited: c.and_then(|v| v.get("unlimited")).and_then(|v| v.as_bool()),
        overage_limit_reached: c
            .and_then(|v| v.get("overage_limit_reached"))
            .and_then(|v| v.as_bool()),
        balance: text_or_num(c.and_then(|v| v.get("balance"))),
        reset_available: reset,
    };
    let empty = out.has_credits.is_none()
        && out.unlimited.is_none()
        && out.overage_limit_reached.is_none()
        && out.balance.is_none()
        && out.reset_available.is_none();
    (!empty).then_some(out)
}

fn header_num(headers: &reqwest::header::HeaderMap, name: &str) -> Option<f64> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|n| n.is_finite())
}

impl CodexUsage {
    /// 每次响应都带的额度头：`x-codex-{primary,secondary}-{used-percent,reset-after-seconds,window-minutes}`。
    /// 一个头都没有就是 `None`——别把「没头」画成 0%。
    pub fn from_headers(headers: &reqwest::header::HeaderMap, now: OffsetDateTime) -> Option<Self> {
        let now_ms = (now.unix_timestamp_nanos() / 1_000_000) as i64;
        let read = |side: &str| -> Option<UsageWindow> {
            let w = UsageWindow {
                used_percent: header_num(headers, &format!("x-codex-{side}-used-percent")),
                reset_at_ms: header_num(headers, &format!("x-codex-{side}-reset-after-seconds"))
                    .filter(|s| *s > 0.0)
                    .map(|s| now_ms + (s * 1000.0) as i64),
                window_minutes: header_num(headers, &format!("x-codex-{side}-window-minutes"))
                    .map(|m| m.round().max(0.0) as u32),
            };
            (!w.is_empty()).then_some(w)
        };
        let primary = read("primary");
        let secondary = read("secondary");
        if primary.is_none() && secondary.is_none() {
            return None;
        }
        Some(Self {
            primary,
            secondary,
            plan_type: None,
            checked_at: iso(now),
            source: "response-headers".into(),
            ..Default::default()
        })
    }

    /// `GET /wham/usage` 的形状：`rate_limit.{primary_window,secondary_window}` 各带
    /// `used_percent` / `reset_after_seconds` / `limit_window_seconds`，顶层 `plan_type`。
    /// `additional_rate_limits` 是按模型单独计的桶（Spark 的 `codex_bengalfox`），以前丢掉了。
    pub fn from_wham(payload: &serde_json::Value, now: OffsetDateTime) -> Self {
        let now_ms = (now.unix_timestamp_nanos() / 1_000_000) as i64;
        let rl = payload.get("rate_limit");
        Self {
            primary: rl.and_then(|r| window_from_value(r.get("primary_window")?, now_ms)),
            secondary: rl.and_then(|r| window_from_value(r.get("secondary_window")?, now_ms)),
            plan_type: text(payload.get("plan_type")),
            checked_at: iso(now),
            source: "wham/usage".into(),
            additional: additional_buckets(payload.get("additional_rate_limits"), now_ms),
            credits: credits_from_wham(payload),
            allowed: rl.and_then(|r| r.get("allowed")).and_then(|v| v.as_bool()),
            limit_reached: rl
                .and_then(|r| r.get("limit_reached"))
                .and_then(|v| v.as_bool()),
            user_id: text(payload.get("user_id")),
        }
    }

    /// 响应头只有主窗口。网关每次请求用头覆盖时，别把 `/wham/usage` 问到的 Spark / 点数冲掉。
    pub fn overlay_on(&self, previous: Option<&Self>) -> Self {
        let Some(prev) = previous else {
            return self.clone();
        };
        if self.source != "response-headers" {
            return self.clone();
        }
        let mut out = self.clone();
        if out.additional.is_empty() {
            out.additional = prev.additional.clone();
        }
        if out.credits.is_none() {
            out.credits = prev.credits.clone();
        }
        if out.user_id.is_none() {
            out.user_id = prev.user_id.clone();
        }
        if out.allowed.is_none() {
            out.allowed = prev.allowed;
        }
        if out.limit_reached.is_none() {
            out.limit_reached = prev.limit_reached;
        }
        if out.plan_type.is_none() {
            out.plan_type = prev.plan_type.clone();
        }
        out
    }

    /// 任一窗口用满就是不可派；恢复时刻取满了的窗口里最晚的那个。
    pub fn exhausted(&self) -> Option<Exhausted> {
        let mut labels = Vec::new();
        let mut reset: Option<i64> = None;
        for (w, label) in [(self.primary, "5 小时"), (self.secondary, "7 天")] {
            let Some(w) = w else { continue };
            if !w.full() {
                continue;
            }
            labels.push(match w.window_minutes {
                Some(m) if m > 0 && m % 1440 == 0 => format!("{} 天", m / 1440),
                Some(m) if m > 0 && m % 60 == 0 => format!("{} 小时", m / 60),
                Some(m) if m > 0 => format!("{m} 分钟"),
                _ => label.to_string(),
            });
            reset = match (reset, w.reset_at_ms) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (None, b) => b,
                (a, None) => a,
            };
        }
        if labels.is_empty() {
            return None;
        }
        Some(Exhausted {
            reason: format!("Codex 额度已用尽（{}窗口）", labels.join("与")),
            reset_at_ms: reset,
        })
    }

    /// 给界面一个总览数字：两个窗口里用得更满的那个。
    pub fn percent_used(&self) -> Option<f64> {
        [self.primary, self.secondary]
            .into_iter()
            .flatten()
            .filter_map(|w| w.used_percent)
            .fold(None, |acc, p| Some(acc.map_or(p, |a: f64| a.max(p))))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exhausted {
    pub reason: String,
    pub reset_at_ms: Option<i64>,
}

// ---------------------------------------------------------------------------
// 模型目录 `GET /backend-api/codex/models?client_version=…`
// ---------------------------------------------------------------------------

/// 目录里的一条。只留我们用得着的字段：`slug` 是模型名；`visibility` 非 `list` 的是隐藏项；
/// `available_in_plans` 非空时只有列出的套餐能用；`minimal_client_version` 高于我们自称的版本
/// 就看不见（真实客户端也看不见）；`supported_reasoning_levels` 是它认的档位。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestModel {
    pub slug: String,
    pub reasoning_levels: Vec<String>,
    pub prefer_websockets: bool,
}

fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    let parse = |s: &str| -> Vec<u64> {
        s.trim()
            .split('.')
            .map(|x| x.parse().unwrap_or(0))
            .collect()
    };
    let (pa, pb) = (parse(a), parse(b));
    for i in 0..pa.len().max(pb.len()) {
        let d = pa
            .get(i)
            .copied()
            .unwrap_or(0)
            .cmp(&pb.get(i).copied().unwrap_or(0));
        if d != std::cmp::Ordering::Equal {
            return d;
        }
    }
    std::cmp::Ordering::Equal
}

/// 三道门筛目录：对外可见、套餐包含、客户端版本够。和云端控制面的 `selectCodexModels` 同一套。
pub fn select_models(
    manifest: &serde_json::Value,
    plan_type: Option<&str>,
    client_version: &str,
) -> Vec<ManifestModel> {
    let Some(list) = manifest.get("models").and_then(|m| m.as_array()) else {
        return Vec::new();
    };
    let mut out: Vec<ManifestModel> = Vec::new();
    for raw in list {
        let Some(slug) = raw
            .get("slug")
            .and_then(|s| s.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if let Some(v) = raw.get("visibility").and_then(|v| v.as_str()) {
            if v != "list" {
                continue;
            }
        }
        if let (Some(plan), Some(plans)) = (
            plan_type,
            raw.get("available_in_plans").and_then(|p| p.as_array()),
        ) {
            if !plans.is_empty() && !plans.iter().any(|p| p.as_str() == Some(plan)) {
                continue;
            }
        }
        if let Some(min) = raw.get("minimal_client_version").and_then(|v| v.as_str()) {
            if compare_versions(client_version, min) == std::cmp::Ordering::Less {
                continue;
            }
        }
        if out.iter().any(|m| m.slug == slug) {
            continue;
        }
        out.push(ManifestModel {
            slug: slug.to_string(),
            reasoning_levels: raw
                .get("supported_reasoning_levels")
                .and_then(|l| l.as_array())
                .map(|l| {
                    l.iter()
                        .filter_map(|e| e.get("effort").or(Some(e)).and_then(|x| x.as_str()))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
            prefer_websockets: raw
                .get("prefer_websockets")
                .and_then(|b| b.as_bool())
                .unwrap_or(false),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    fn now() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap()
    }

    #[test]
    fn user_agent_and_originator_come_from_the_same_constant() {
        let ua = user_agent();
        assert!(ua.starts_with(
            "codex-tui/0.153.4 (Ubuntu 22.4.0; x86_64) xterm-256color (codex-tui; 0.153.4)"
        ));
        let h = identity_headers("tok", Some(" acct_1 "));
        assert!(h.iter().any(|(k, v)| *k == "originator" && v == ORIGINATOR));
        assert!(h
            .iter()
            .any(|(k, v)| *k == "chatgpt-account-id" && v == "acct_1"));
        assert!(h
            .iter()
            .any(|(k, v)| *k == "authorization" && v == "Bearer tok"));
        assert!(!identity_headers("tok", None)
            .iter()
            .any(|(k, _)| *k == "chatgpt-account-id"));
    }

    #[test]
    fn quota_headers_become_windows_and_reset_times_are_absolute() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-codex-primary-used-percent",
            HeaderValue::from_static("41.5"),
        );
        h.insert(
            "x-codex-primary-reset-after-seconds",
            HeaderValue::from_static("600"),
        );
        h.insert(
            "x-codex-primary-window-minutes",
            HeaderValue::from_static("300"),
        );
        h.insert(
            "x-codex-secondary-used-percent",
            HeaderValue::from_static("100"),
        );
        h.insert(
            "x-codex-secondary-window-minutes",
            HeaderValue::from_static("10080"),
        );
        let u = CodexUsage::from_headers(&h, now()).unwrap();
        let p = u.primary.unwrap();
        assert_eq!(p.used_percent, Some(41.5));
        assert_eq!(p.reset_at_ms, Some(1_700_000_000_000 + 600_000));
        assert_eq!(p.window_minutes, Some(300));
        assert_eq!(u.secondary.unwrap().window_minutes, Some(10080));
        assert_eq!(u.source, "response-headers");
        assert_eq!(u.percent_used(), Some(100.0));
        let ex = u.exhausted().unwrap();
        assert!(ex.reason.contains("7 天"), "{}", ex.reason);
        assert_eq!(ex.reset_at_ms, None, "7 天窗口没给重置时刻");

        assert!(
            CodexUsage::from_headers(&HeaderMap::new(), now()).is_none(),
            "没头不是 0%"
        );
    }

    #[test]
    fn manifest_is_filtered_by_visibility_plan_and_client_version() {
        let manifest = serde_json::json!({ "models": [
            { "slug": "gpt-5.4", "visibility": "list", "available_in_plans": ["plus", "pro"], "minimal_client_version": "0.140.0",
              "supported_reasoning_levels": [{ "effort": "low" }, { "effort": "high" }], "prefer_websockets": true },
            { "slug": "gpt-6-astra", "visibility": "list", "minimal_client_version": "9.0.0" },
            { "slug": "secret", "visibility": "hide" },
            { "slug": "pro-only", "visibility": "list", "available_in_plans": ["pro"] },
            { "slug": "gpt-5.4" },
            { "slug": "  " },
        ] });
        let plus = select_models(&manifest, Some("plus"), CLIENT_VERSION);
        assert_eq!(
            plus.iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(),
            vec!["gpt-5.4"]
        );
        assert_eq!(plus[0].reasoning_levels, vec!["low", "high"]);
        assert!(plus[0].prefer_websockets);
        let pro = select_models(&manifest, Some("pro"), CLIENT_VERSION);
        assert_eq!(
            pro.iter().map(|m| m.slug.as_str()).collect::<Vec<_>>(),
            vec!["gpt-5.4", "pro-only"]
        );
        let unknown_plan = select_models(&manifest, None, CLIENT_VERSION);
        assert_eq!(unknown_plan.len(), 2, "不知道套餐就不按套餐筛");
        let future = select_models(&manifest, Some("plus"), "9.0.0");
        assert!(
            future.iter().any(|m| m.slug == "gpt-6-astra"),
            "版本够了就看得见"
        );
        assert!(select_models(&serde_json::json!({}), None, CLIENT_VERSION).is_empty());
        assert_eq!(
            compare_versions("0.153.4", "0.153.10"),
            std::cmp::Ordering::Less
        );
        assert_eq!(compare_versions("1.0", "1.0.0"), std::cmp::Ordering::Equal);
    }

    #[test]
    fn wham_usage_is_parsed_with_the_plan_and_the_latest_reset_wins() {
        let v = serde_json::json!({
            "plan_type": "plus",
            "rate_limit": {
                "primary_window": { "used_percent": 100, "reset_after_seconds": 600, "limit_window_seconds": 18000 },
                "secondary_window": { "used_percent": 100, "reset_after_seconds": 86400, "limit_window_seconds": 604800 }
            }
        });
        let u = CodexUsage::from_wham(&v, now());
        assert_eq!(u.plan_type.as_deref(), Some("plus"));
        assert_eq!(u.primary.unwrap().window_minutes, Some(300));
        assert_eq!(u.secondary.unwrap().window_minutes, Some(10080));
        let ex = u.exhausted().unwrap();
        assert_eq!(ex.reset_at_ms, Some(1_700_000_000_000 + 86_400_000));
        assert!(
            ex.reason.contains("5 小时") && ex.reason.contains("7 天"),
            "{}",
            ex.reason
        );

        let fine = CodexUsage::from_wham(
            &serde_json::json!({ "rate_limit": { "primary_window": { "used_percent": 12 } } }),
            now(),
        );
        assert!(fine.exhausted().is_none());
        assert_eq!(fine.percent_used(), Some(12.0));
        assert!(fine.secondary.is_none());
    }

    #[test]
    fn wham_keeps_spark_buckets_credits_and_user_id() {
        let v = serde_json::json!({
            "user_id": "user_abc",
            "plan_type": "pro",
            "rate_limit": {
                "allowed": true,
                "limit_reached": false,
                "primary_window": { "used_percent": 34, "reset_after_seconds": 600, "limit_window_seconds": 18000 },
                "secondary_window": { "used_percent": 37, "reset_after_seconds": 86400, "limit_window_seconds": 604800 }
            },
            "additional_rate_limits": [
                {
                    "limit_name": "GPT-5.3-Codex-Spark",
                    "metered_feature": "codex_bengalfox",
                    "rate_limit": {
                        "allowed": true,
                        "limit_reached": false,
                        "primary_window": { "used_percent": 100, "reset_after_seconds": 18000, "limit_window_seconds": 18000 },
                        "secondary_window": { "used_percent": 12, "reset_after_seconds": 519837, "limit_window_seconds": 604800 }
                    }
                }
            ],
            "credits": { "has_credits": false, "unlimited": false, "overage_limit_reached": false, "balance": "0" },
            "rate_limit_reset_credits": { "available_count": 0 }
        });
        let u = CodexUsage::from_wham(&v, now());
        assert_eq!(u.user_id.as_deref(), Some("user_abc"));
        assert_eq!(u.allowed, Some(true));
        assert_eq!(u.limit_reached, Some(false));
        assert_eq!(u.additional.len(), 1);
        assert_eq!(u.additional[0].name.as_deref(), Some("GPT-5.3-Codex-Spark"));
        assert_eq!(u.additional[0].feature.as_deref(), Some("codex_bengalfox"));
        assert_eq!(u.additional[0].primary.unwrap().used_percent, Some(100.0));
        assert_eq!(u.additional[0].primary.unwrap().window_minutes, Some(300));
        assert_eq!(u.credits.as_ref().unwrap().balance.as_deref(), Some("0"));
        assert_eq!(u.credits.as_ref().unwrap().reset_available, Some(0));
        assert!(
            u.exhausted().is_none(),
            "Spark 满了不该把主 Codex 窗口判成耗尽"
        );
        assert_eq!(u.percent_used(), Some(37.0));

        let map_form = CodexUsage::from_wham(
            &serde_json::json!({
                "additional_rate_limits": {
                    "GPT-Reserve": { "secondary_window": { "used_percent": 8, "limit_window_seconds": 604800 } }
                }
            }),
            now(),
        );
        assert_eq!(map_form.additional[0].name.as_deref(), Some("GPT-Reserve"));
        assert_eq!(
            map_form.additional[0].secondary.unwrap().used_percent,
            Some(8.0)
        );

        let headers = CodexUsage {
            primary: Some(UsageWindow {
                used_percent: Some(90.0),
                reset_at_ms: Some(1),
                window_minutes: Some(300),
            }),
            source: "response-headers".into(),
            checked_at: "t".into(),
            ..Default::default()
        };
        let merged = headers.overlay_on(Some(&u));
        assert_eq!(merged.primary.unwrap().used_percent, Some(90.0));
        assert_eq!(merged.additional.len(), 1, "头覆盖不能冲掉 Spark");
        assert_eq!(merged.user_id.as_deref(), Some("user_abc"));
        assert_eq!(
            u.overlay_on(Some(&headers)).additional.len(),
            1,
            "整份 wham 快照覆盖旧头"
        );
    }

    #[test]
    fn old_usage_json_without_additional_still_deserializes() {
        let u: CodexUsage = serde_json::from_value(serde_json::json!({
            "primary": { "usedPercent": 10, "resetAtMs": 1, "windowMinutes": 300 },
            "secondary": null,
            "planType": "plus",
            "checkedAt": "t",
            "source": "wham/usage"
        }))
        .unwrap();
        assert!(u.additional.is_empty());
        assert!(u.credits.is_none());
        assert_eq!(u.primary.unwrap().used_percent, Some(10.0));
    }
}
