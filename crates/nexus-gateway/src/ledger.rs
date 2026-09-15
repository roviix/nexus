//! 请求账本：方言口每处理完一次请求记一行，概览页的「本地用量」全从这里算。
//!
//! 只记**数字与名字**——账号、模型、状态、token 数、耗时。没有对话内容，没有凭证；这张表
//! 和活动日志一样可以随便看、随便备份。写入是尽力而为：记不进去只 warn，不影响回给客户端
//! 的响应。
//!
//! 透传口的流量原则上**不记**：它不解 body，一次对话在协议上是几十个 Connect 调用（登录轮询、
//! Dashboard、Stream…），按调用计数只会把数字灌水。唯一的例外是 IDE Agent 面板拦截
//! （`intercept`）：那一条路径解了 body，一次 `InferenceService/Stream` = 一次模型调用 = 一行，
//! `dialect` 列写 [`SOURCE_IDE_AGENT`]。它和方言口的行**分开算**：默认的 [`Ledger::summary`]
//! 不含它，界面上 IDE 那张卡用 [`Ledger::summary_source`] 单独看——两边口径不同（一个是
//! 标准 API 客户端的请求，一个是 Cursor 自己每一轮的模型调用），混在一起谁也说不清。

use crate::normalized::Usage;
use nexus_core::Result;
use nexus_store::Db;
use rusqlite::params;
use serde::Serialize;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use time::OffsetDateTime;

/// 账本里只保留这么多天。再往前的数字没人回头看，而这张表是随请求线性长的。
pub const KEEP_DAYS: i64 = 90;

/// IDE Agent 面板拦截记的行在 `dialect` 列里的值。不是方言，借这一列当来源标签，免一次迁移。
pub const SOURCE_IDE_AGENT: &str = "ide-agent";

const DAY_MS: i64 = 86_400_000;

/// 一次汇总看哪些行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// 方言口：除 IDE 拦截之外的全部。
    Dialects,
    /// 只看某个来源标签。取值是本 crate 里的常量（`SOURCE_IDE_AGENT`），不是用户输入，
    /// 所以能直接拼进 SQL。
    Source(&'static str),
}

impl Scope {
    fn clause(self) -> String {
        match self {
            Scope::Dialects => format!(" AND dialect != '{SOURCE_IDE_AGENT}'"),
            Scope::Source(s) => format!(" AND dialect = '{s}'"),
        }
    }
}

pub struct Ledger {
    db: Arc<Db>,
}

/// 一次请求的结果。字段都是借用：记账在响应发完之后，不值得为它再 clone 一遍。
pub struct RequestRecord<'a> {
    /// 走的哪条通道（`cursor` / `chatgpt` / `grok` / `kiro`）。
    pub channel: &'a str,
    pub account: &'a str,
    pub model: &'a str,
    pub routed: Option<&'a str>,
    pub dialect: &'a str,
    pub ok: bool,
    pub status: u16,
    /// 失败时的分类（`UpstreamKind::as_str`）。
    pub kind: Option<&'a str>,
    pub usage: Usage,
    pub usage_measured: bool,
    pub ttft_ms: Option<u64>,
    pub duration_ms: u64,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Ledger {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    /// 某条通道按账号合计，不截断。账号卡要每个号自己的数字，概览那份 `by_account` 只留前 8。
    pub fn channel_account_totals(&self, channel: &str, days: u32) -> Result<Vec<NamedUsage>> {
        let days = days.clamp(1, KEEP_DAYS as u32) as i64;
        let since_ms = now_ms() - days * DAY_MS;
        let channel = channel.trim();
        self.db.with(|c| {
            let mut stmt = c.prepare(
                "SELECT account, COUNT(*), SUM(ok = 0), SUM(input_tokens + output_tokens)
                 FROM gateway_requests
                 WHERE at_ms >= ?1 AND channel = ?2 AND dialect != ?3
                 GROUP BY account ORDER BY COUNT(*) DESC",
            )?;
            let rows = stmt.query_map(params![since_ms, channel, SOURCE_IDE_AGENT], |r| {
                Ok(NamedUsage {
                    name: r.get(0)?,
                    calls: r.get(1)?,
                    errors: r.get(2)?,
                    tokens: r.get(3)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
        })
    }

    /// 记一行。失败只 warn：账本是给人看的，不该反过来影响请求。
    pub fn record(&self, r: RequestRecord<'_>) {
        self.record_at(now_ms(), r);
    }

    fn record_at(&self, at_ms: i64, r: RequestRecord<'_>) {
        let result = self.db.with(|c| {
            c.execute(
                "INSERT INTO gateway_requests
                   (at_ms, account, model, routed, dialect, ok, status, kind,
                    input_tokens, output_tokens, cache_read, cache_write, measured, ttft_ms, duration_ms, channel)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    at_ms,
                    r.account.trim().to_lowercase(),
                    r.model,
                    r.routed,
                    r.dialect,
                    r.ok as i64,
                    i64::from(r.status),
                    r.kind,
                    i64::from(r.usage.input_tokens),
                    i64::from(r.usage.output_tokens),
                    i64::from(r.usage.cache_read_tokens),
                    i64::from(r.usage.cache_write_tokens),
                    r.usage_measured as i64,
                    r.ttft_ms.map(|v| v as i64),
                    r.duration_ms as i64,
                    r.channel,
                ],
            )
        });
        if let Err(err) = result {
            tracing::warn!(%err, "网关请求账本写入失败");
        }
    }

    /// 裁掉 `KEEP_DAYS` 之前的行。启动时跑一次。
    pub fn prune(&self) -> Result<usize> {
        let cutoff = now_ms() - KEEP_DAYS * DAY_MS;
        self.db
            .with(|c| c.execute("DELETE FROM gateway_requests WHERE at_ms < ?1", [cutoff]))
    }

    /// 最近 `days` 天（含今天）的账。
    ///
    /// `tz_offset_min` 是前端给的本地时区偏移（`-new Date().getTimezoneOffset()`，东八区是
    /// 480）：「今天」按用户墙上的钟算，不按 UTC——否则晚上八点之后的请求会被记到「明天」。
    pub fn summary(&self, days: u32, tz_offset_min: i32) -> Result<UsageSummary> {
        self.summary_at(now_ms(), days, tz_offset_min, Scope::Dialects)
    }

    /// 只看某个来源标签（今天只有 [`SOURCE_IDE_AGENT`]）的账，形状与 [`Ledger::summary`] 相同。
    pub fn summary_source(
        &self,
        days: u32,
        tz_offset_min: i32,
        source: &'static str,
    ) -> Result<UsageSummary> {
        self.summary_at(now_ms(), days, tz_offset_min, Scope::Source(source))
    }

    fn summary_at(
        &self,
        now_ms: i64,
        days: u32,
        tz_offset_min: i32,
        scope: Scope,
    ) -> Result<UsageSummary> {
        let days = days.clamp(1, KEEP_DAYS as u32) as i64;
        let tz_ms = i64::from(tz_offset_min) * 60_000;
        let today_idx = (now_ms + tz_ms).div_euclid(DAY_MS);
        let first_idx = today_idx - (days - 1);
        let since_ms = first_idx * DAY_MS - tz_ms;
        let today_since_ms = today_idx * DAY_MS - tz_ms;
        let scope_sql = scope.clause();

        self.db.with(|c| {
            // 逐天。SQLite 里按 (at_ms + tz) / 86400000 整除分桶，空的天在下面补零。
            let mut by_day: Vec<DayUsage> = (0..days)
                .map(|i| DayUsage {
                    day: day_label(first_idx + i),
                    calls: 0,
                    errors: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                })
                .collect();
            {
                let mut stmt = c.prepare(&format!(
                    "SELECT (at_ms + ?1) / 86400000 AS d,
                            COUNT(*), SUM(ok = 0), SUM(input_tokens), SUM(output_tokens)
                     FROM gateway_requests WHERE at_ms >= ?2{scope_sql}
                     GROUP BY d"
                ))?;
                let rows = stmt.query_map(params![tz_ms, since_ms], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })?;
                for row in rows {
                    let (d, calls, errors, input, output) = row?;
                    let i = d - first_idx;
                    if (0..days).contains(&i) {
                        let slot = &mut by_day[i as usize];
                        slot.calls = calls;
                        slot.errors = errors;
                        slot.input_tokens = input;
                        slot.output_tokens = output;
                    }
                }
            }

            // 今天逐小时。给概览的「今天」视图用：一天只有一根柱子什么走势也看不出，
            // 拆成小时才回答得了「上午还是下午在跑」。分桶同样按用户的钟。
            let mut by_hour: Vec<HourUsage> = (0..24)
                .map(|h| HourUsage {
                    hour: h,
                    calls: 0,
                    errors: 0,
                    input_tokens: 0,
                    output_tokens: 0,
                })
                .collect();
            {
                let mut stmt = c.prepare(&format!(
                    "SELECT ((at_ms + ?1) % 86400000) / 3600000 AS h,
                            COUNT(*), SUM(ok = 0), SUM(input_tokens), SUM(output_tokens)
                     FROM gateway_requests WHERE at_ms >= ?2{scope_sql}
                     GROUP BY h"
                ))?;
                let rows = stmt.query_map(params![tz_ms, today_since_ms], |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                    ))
                })?;
                for row in rows {
                    let (h, calls, errors, input, output) = row?;
                    if let Some(slot) = usize::try_from(h).ok().and_then(|i| by_hour.get_mut(i)) {
                        slot.calls = calls;
                        slot.errors = errors;
                        slot.input_tokens = input;
                        slot.output_tokens = output;
                    }
                }
            }

            let today = totals(c, today_since_ms, &scope_sql)?;
            let window = totals(c, since_ms, &scope_sql)?;

            let by_model = {
                let mut stmt = c.prepare(&format!(
                    "SELECT model, COUNT(*), SUM(ok = 0), SUM(input_tokens + output_tokens)
                     FROM gateway_requests WHERE at_ms >= ?1{scope_sql}
                     GROUP BY model ORDER BY COUNT(*) DESC LIMIT 8"
                ))?;
                let rows = stmt.query_map([since_ms], |r| {
                    Ok(NamedUsage {
                        name: r.get(0)?,
                        calls: r.get(1)?,
                        errors: r.get(2)?,
                        tokens: r.get(3)?,
                    })
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            let by_account = {
                let mut stmt = c.prepare(&format!(
                    "SELECT account, COUNT(*), SUM(ok = 0), SUM(input_tokens + output_tokens)
                     FROM gateway_requests WHERE at_ms >= ?1{scope_sql}
                     GROUP BY account ORDER BY COUNT(*) DESC LIMIT 8"
                ))?;
                let rows = stmt.query_map([since_ms], |r| {
                    Ok(NamedUsage {
                        name: r.get(0)?,
                        calls: r.get(1)?,
                        errors: r.get(2)?,
                        tokens: r.get(3)?,
                    })
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            let by_channel = {
                let mut stmt = c.prepare(&format!(
                    "SELECT channel, COUNT(*), SUM(ok = 0), SUM(input_tokens + output_tokens)
                     FROM gateway_requests WHERE at_ms >= ?1{scope_sql}
                     GROUP BY channel ORDER BY COUNT(*) DESC"
                ))?;
                let rows = stmt.query_map([since_ms], |r| {
                    Ok(NamedUsage {
                        name: r.get(0)?,
                        calls: r.get(1)?,
                        errors: r.get(2)?,
                        tokens: r.get(3)?,
                    })
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            let recent = {
                let mut stmt = c.prepare(&format!(
                    "SELECT at_ms, account, model, routed, ok, status, kind,
                            input_tokens, output_tokens, ttft_ms, duration_ms, channel
                     FROM gateway_requests WHERE 1 = 1{scope_sql} ORDER BY id DESC LIMIT 8"
                ))?;
                let rows = stmt.query_map([], |r| {
                    Ok(RecentRequest {
                        at: nexus_core::clock::iso_from_millis(r.get::<_, i64>(0)?)
                            .unwrap_or_default(),
                        account: r.get(1)?,
                        model: r.get(2)?,
                        routed: r.get(3)?,
                        ok: r.get::<_, i64>(4)? != 0,
                        status: r.get::<_, i64>(5)? as u16,
                        kind: r.get(6)?,
                        input_tokens: r.get(7)?,
                        output_tokens: r.get(8)?,
                        ttft_ms: r.get(9)?,
                        duration_ms: r.get(10)?,
                        channel: r.get(11)?,
                    })
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            let first_at: Option<i64> = c
                .query_row(
                    &format!("SELECT MIN(at_ms) FROM gateway_requests WHERE 1 = 1{scope_sql}"),
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(None);

            Ok(UsageSummary {
                days: by_day,
                hours: by_hour,
                today,
                window,
                by_model,
                by_account,
                by_channel,
                recent,
                since: first_at.and_then(nexus_core::clock::iso_from_millis),
            })
        })
    }
}

/// 一段时间的合计。首字 / 总耗时取**中位数**而不是平均：一次卡了 40 秒的请求会把平均值
/// 拖到没人信，中位数说的才是「平时什么感觉」。
fn totals(c: &rusqlite::Connection, since_ms: i64, scope_sql: &str) -> rusqlite::Result<Totals> {
    let (calls, errors, input, output, cache_read): (i64, i64, i64, i64, i64) = c.query_row(
        &format!(
            "SELECT COUNT(*), COALESCE(SUM(ok = 0), 0), COALESCE(SUM(input_tokens), 0),
                    COALESCE(SUM(output_tokens), 0), COALESCE(SUM(cache_read), 0)
             FROM gateway_requests WHERE at_ms >= ?1{scope_sql}"
        ),
        [since_ms],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let median = |col: &str| -> rusqlite::Result<Option<i64>> {
        // 只看成功的：失败的请求 ttft 多半是空的，耗时也说不明什么。
        let n: i64 = c.query_row(
            &format!(
                "SELECT COUNT(*) FROM gateway_requests WHERE at_ms >= ?1{scope_sql} AND ok = 1 AND {col} IS NOT NULL"
            ),
            [since_ms],
            |r| r.get(0),
        )?;
        if n == 0 {
            return Ok(None);
        }
        c.query_row(
            &format!(
                "SELECT {col} FROM gateway_requests WHERE at_ms >= ?1{scope_sql} AND ok = 1 AND {col} IS NOT NULL
                 ORDER BY {col} LIMIT 1 OFFSET ?2"
            ),
            params![since_ms, n / 2],
            |r| r.get::<_, Option<i64>>(0),
        )
    };
    Ok(Totals {
        calls,
        errors,
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: cache_read,
        ttft_p50_ms: median("ttft_ms")?,
        duration_p50_ms: median("duration_ms")?,
    })
}

/// 第 `idx` 个「本地日」的日期串（`2026-09-03`）。
///
/// `idx` 是 `(at_ms + tz) / 86400000`：分桶时已经把时区折进去了，所以这里的日期就是
/// 「自 1970-01-01 起第 idx 天」，不必再带偏移。
fn day_label(idx: i64) -> String {
    OffsetDateTime::from_unix_timestamp(idx * 86_400)
        .map(|t| {
            let d = t.date();
            format!("{:04}-{:02}-{:02}", d.year(), u8::from(d.month()), d.day())
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DayUsage {
    /// `2026-09-03`，按用户的本地时区。
    pub day: String,
    pub calls: i64,
    pub errors: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

/// 今天的某一个小时。`hour` 是用户本地时区的 0–23。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HourUsage {
    pub hour: u8,
    pub calls: i64,
    pub errors: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub calls: i64,
    pub errors: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub ttft_p50_ms: Option<i64>,
    pub duration_p50_ms: Option<i64>,
}

/// 按模型 / 按账号的一行。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NamedUsage {
    pub name: String,
    pub calls: i64,
    pub errors: i64,
    pub tokens: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RecentRequest {
    pub at: String,
    pub account: String,
    pub model: String,
    pub routed: Option<String>,
    pub ok: bool,
    pub status: u16,
    pub kind: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub ttft_ms: Option<i64>,
    pub duration_ms: i64,
    pub channel: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    /// 从旧到新，恰好 `days` 条；没请求的天也在，数字为零。
    pub days: Vec<DayUsage>,
    /// 今天的 24 个小时，按用户本地时区，0 点在前；还没到的小时也在，数字为零。
    pub hours: Vec<HourUsage>,
    pub today: Totals,
    pub window: Totals,
    pub by_model: Vec<NamedUsage>,
    pub by_account: Vec<NamedUsage>,
    /// 按通道（cursor / chatgpt / grok / kiro）。
    pub by_channel: Vec<NamedUsage>,
    pub recent: Vec<RecentRequest>,
    /// 账本里最早一条的时刻；一条都没有是 `None`。
    pub since: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ledger() -> Ledger {
        Ledger::new(Arc::new(Db::open_in_memory().unwrap()))
    }

    fn ok_record<'a>(
        account: &'a str,
        model: &'a str,
        input: u32,
        output: u32,
    ) -> RequestRecord<'a> {
        RequestRecord {
            channel: "cursor",
            account,
            model,
            routed: Some("routed"),
            dialect: "openai",
            ok: true,
            status: 200,
            kind: None,
            usage: Usage {
                input_tokens: input,
                output_tokens: output,
                ..Default::default()
            },
            usage_measured: true,
            ttft_ms: Some(900),
            duration_ms: 4000,
        }
    }

    fn err_record<'a>(account: &'a str, model: &'a str) -> RequestRecord<'a> {
        RequestRecord {
            channel: "cursor",
            account,
            model,
            routed: None,
            dialect: "anthropic",
            ok: false,
            status: 402,
            kind: Some("quota"),
            usage: Usage::default(),
            usage_measured: false,
            ttft_ms: None,
            duration_ms: 120,
        }
    }

    // 2026-09-03T12:00:00Z
    const NOW: i64 = 1_788_436_800_000;
    const UTC8: i32 = 480;

    #[test]
    fn days_are_bucketed_in_the_users_timezone() {
        let l = ledger();
        // 东八区 2026-09-03 03:00 = UTC 2026-09-02 19:00：按 UTC 是「昨天」，按用户是「今天」。
        l.record_at(
            NOW - 17 * 3_600_000,
            ok_record("A@x.com", "claude-sonnet-5", 10, 5),
        );
        l.record_at(NOW - 2 * 3_600_000, err_record("a@x.com", "gpt-5.6-sol"));

        let s = l.summary_at(NOW, 7, UTC8, Scope::Dialects).unwrap();
        assert_eq!(s.days.len(), 7);
        assert_eq!(s.days.last().unwrap().day, "2026-09-03");
        assert_eq!(s.days[0].day, "2026-08-28");
        assert_eq!(s.days.last().unwrap().calls, 2, "两条都算今天");
        assert_eq!(s.today.calls, 2);
        assert_eq!(s.today.errors, 1);
        assert_eq!(s.today.input_tokens, 10);

        // 同一份数据按 UTC 看，早的那条落在昨天。
        let utc = l.summary_at(NOW, 7, 0, Scope::Dialects).unwrap();
        assert_eq!(utc.today.calls, 1);
        assert_eq!(utc.days[5].calls, 1);
        assert_eq!(utc.days[5].day, "2026-09-02");
    }

    #[test]
    fn hours_cover_today_in_the_users_timezone() {
        let l = ledger();
        // NOW 是 UTC 12:00 = 东八区 20:00。
        l.record_at(NOW, ok_record("a@x.com", "m", 10, 5)); // 20 点
        l.record_at(NOW - 30 * 60_000, err_record("a@x.com", "m")); // 19 点半
        l.record_at(NOW - 17 * 3_600_000, ok_record("a@x.com", "m", 1, 1)); // 东八区 03:00
        l.record_at(NOW - 25 * 3_600_000, ok_record("a@x.com", "m", 1, 1)); // 昨天，不算

        let s = l.summary_at(NOW, 7, UTC8, Scope::Dialects).unwrap();
        assert_eq!(s.hours.len(), 24);
        assert!(
            s.hours
                .iter()
                .enumerate()
                .all(|(i, h)| usize::from(h.hour) == i),
            "0 点在前，逐小时"
        );
        assert_eq!(s.hours[20].calls, 1);
        assert_eq!(s.hours[20].input_tokens, 10);
        assert_eq!(s.hours[19].calls, 1);
        assert_eq!(s.hours[19].errors, 1);
        assert_eq!(s.hours[3].calls, 1);
        assert_eq!(
            s.hours.iter().map(|h| h.calls).sum::<i64>(),
            3,
            "昨天那条不在今天的小时里"
        );

        // 同一份数据按 UTC 看：20 点那条落在 12 点，03:00 那条是 UTC 前一天 19:00，不算今天。
        let utc = l.summary_at(NOW, 7, 0, Scope::Dialects).unwrap();
        assert_eq!(utc.hours[12].calls, 1);
        assert_eq!(utc.hours[11].errors, 1);
        assert_eq!(utc.hours.iter().map(|h| h.calls).sum::<i64>(), 2);
    }

    #[test]
    fn totals_group_by_model_and_lowercased_account() {
        let l = ledger();
        for _ in 0..3 {
            l.record_at(NOW - 1000, ok_record("A@x.com", "claude-sonnet-5", 100, 50));
        }
        l.record_at(NOW - 1000, ok_record("b@x.com", "gpt-5.6-sol", 10, 10));
        l.record_at(NOW - 1000, err_record("b@x.com", "gpt-5.6-sol"));

        let s = l.summary_at(NOW, 7, 0, Scope::Dialects).unwrap();
        assert_eq!(s.window.calls, 5);
        assert_eq!(s.window.errors, 1);
        assert_eq!(s.window.input_tokens, 310);
        assert_eq!(s.window.output_tokens, 160);
        assert_eq!(s.by_model[0].name, "claude-sonnet-5");
        assert_eq!(s.by_model[0].calls, 3);
        assert_eq!(s.by_model[0].tokens, 450);
        assert_eq!(s.by_model[1].errors, 1);
        assert_eq!(s.by_account[0].name, "a@x.com", "邮箱统一小写");
        assert_eq!(s.by_account.len(), 2);
        assert_eq!(s.recent.len(), 5);
        assert!(!s.recent[0].ok, "最新的在前");
        assert!(s.since.is_some());
    }

    #[test]
    fn medians_only_count_successful_requests() {
        let l = ledger();
        let mut fast = ok_record("a@x.com", "m", 1, 1);
        fast.ttft_ms = Some(300);
        fast.duration_ms = 1000;
        let mut slow = ok_record("a@x.com", "m", 1, 1);
        slow.ttft_ms = Some(40_000);
        slow.duration_ms = 90_000;
        l.record_at(NOW - 10, fast);
        l.record_at(NOW - 9, ok_record("a@x.com", "m", 1, 1)); // 900 / 4000
        l.record_at(NOW - 8, slow);
        l.record_at(NOW - 7, err_record("a@x.com", "m"));

        let s = l.summary_at(NOW, 1, 0, Scope::Dialects).unwrap();
        assert_eq!(s.window.ttft_p50_ms, Some(900));
        assert_eq!(s.window.duration_p50_ms, Some(4000));

        let empty = ledger().summary_at(NOW, 1, 0, Scope::Dialects).unwrap();
        assert_eq!(empty.window.ttft_p50_ms, None);
        assert_eq!(empty.window.calls, 0);
        assert!(empty.since.is_none());
        assert_eq!(empty.days.len(), 1);
    }

    #[test]
    fn window_excludes_older_rows_and_prune_drops_them() {
        let l = ledger();
        l.record_at(NOW - 10 * DAY_MS, ok_record("a@x.com", "m", 1, 1));
        l.record_at(
            NOW - (KEEP_DAYS + 1) * DAY_MS,
            ok_record("a@x.com", "m", 1, 1),
        );
        l.record_at(NOW, ok_record("a@x.com", "m", 1, 1));

        let s = l.summary_at(NOW, 7, 0, Scope::Dialects).unwrap();
        assert_eq!(s.window.calls, 1, "十天前的不在七天窗口里");
        assert_eq!(s.recent.len(), 3, "最近几条不受窗口限制");

        // prune 用的是真实的现在；这条 91 天前（相对 NOW）的行相对真实现在也早就过期。
        let removed = l.prune().unwrap();
        assert!(removed >= 1);
    }

    /// IDE 拦截的行和方言口的行两边分开算：默认汇总看不见 IDE 的，IDE 的汇总也看不见方言口的。
    #[test]
    fn ide_agent_rows_are_scoped_apart_from_dialect_rows() {
        let l = ledger();
        l.record_at(NOW - 1000, ok_record("a@x.com", "gpt-5.6-sol", 10, 5));
        l.record_at(
            NOW - 500,
            RequestRecord {
                dialect: SOURCE_IDE_AGENT,
                ..ok_record("a@x.com", "claude-opus-5", 400_000, 800)
            },
        );
        l.record_at(
            NOW - 400,
            RequestRecord {
                dialect: SOURCE_IDE_AGENT,
                ..err_record("a@x.com", "claude-opus-5")
            },
        );

        let dialects = l.summary_at(NOW, 7, 0, Scope::Dialects).unwrap();
        assert_eq!(dialects.window.calls, 1);
        assert_eq!(dialects.window.input_tokens, 10);
        assert_eq!(dialects.recent.len(), 1);
        assert_eq!(dialects.by_model[0].name, "gpt-5.6-sol");
        assert_eq!(dialects.days.last().unwrap().calls, 1);

        let ide = l
            .summary_at(NOW, 7, 0, Scope::Source(SOURCE_IDE_AGENT))
            .unwrap();
        assert_eq!(ide.window.calls, 2);
        assert_eq!(ide.window.errors, 1);
        assert_eq!(ide.window.input_tokens, 400_000);
        assert_eq!(ide.recent.len(), 2);
        assert_eq!(ide.by_model.len(), 1);
        assert_eq!(ide.by_model[0].name, "claude-opus-5");
        assert_eq!(ide.today.calls, 2);
        assert!(ide.since.is_some());
        // 中位数只算成功且有值的那条。
        assert_eq!(ide.window.ttft_p50_ms, Some(900));

        // 公开入口走同一条路 —— 但它们读的是墙上时间，所以这一段必须另起一份按「此刻」
        // 记的账。拿上面那份固定 NOW 的数据去断言，过一周就会滑出 7 天窗口，
        // 测试会在某个和改动毫无关系的日子突然变红。
        let live = ledger();
        live.record(ok_record("a@x.com", "gpt-5.6-sol", 10, 5));
        live.record(RequestRecord {
            dialect: SOURCE_IDE_AGENT,
            ..ok_record("a@x.com", "claude-opus-5", 400_000, 800)
        });
        assert_eq!(live.summary(7, 0).unwrap().window.calls, 1);
        assert_eq!(
            live.summary_source(7, 0, SOURCE_IDE_AGENT)
                .unwrap()
                .window
                .calls,
            1
        );
    }

    #[test]
    fn day_label_is_the_civil_date_of_that_day_index() {
        let idx = NOW.div_euclid(DAY_MS);
        assert_eq!(day_label(idx), "2026-09-03");
        assert_eq!(day_label(idx - 3), "2026-08-31");
        assert_eq!(day_label(0), "1970-01-01");
    }

    #[test]
    fn channel_totals_keep_every_account_and_ignore_other_channels() {
        let l = ledger();
        let now = now_ms();
        l.record_at(
            now - 1000,
            RequestRecord {
                channel: "chatgpt",
                ..ok_record("plus@example.com", "gpt-5.4", 100, 20)
            },
        );
        l.record_at(
            now - 1000,
            ok_record("cursor@example.com", "gpt-5.6-sol", 9, 1),
        );
        let totals = l.channel_account_totals("chatgpt", 90).unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].name, "plus@example.com");
        assert_eq!(totals[0].tokens, 120);
        assert!(l.channel_account_totals("grok", 90).unwrap().is_empty());
    }
}
