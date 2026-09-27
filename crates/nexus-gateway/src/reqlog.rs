//! 最近请求的明细：给「本地网关 → 最近请求」看的。
//!
//! 账本（ledger）只记数字、按天聚合；这里记的是排障要的那几样——哪个客户端、按路由换成了
//! 哪个模型、走了哪条通道哪个号、上游回了什么原话。连进网关之前就被挡下的请求（口令不对、
//! 请求体坏了、一个号都没有）也记，那恰恰是「接上了却不能用」时最想看到的一行。
//!
//! 只在内存里留最近几百条，不落库：它回答的是「刚才那一下为什么不行」，重启之后没人回头看。

use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

pub const CAPACITY: usize = 300;

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    /// 进程内单调递增，界面拿它做 key、判断有没有新行。
    pub id: u64,
    pub at: String,
    /// 从哪个客户端作用域进来的（`/client/claude/…`）。直接打 `/v1` 的是 `None`。
    pub client: Option<String>,
    /// openai / anthropic / responses。
    pub dialect: String,
    pub stream: bool,
    /// 客户端报的模型名。
    pub requested: String,
    /// 按路由换算后实际要的模型；和 `requested` 一样时不填。
    pub target: Option<String>,
    pub channel: Option<String>,
    pub account: Option<String>,
    /// 上游自称跑的模型。
    pub routed: Option<String>,
    pub ok: bool,
    pub status: u16,
    pub kind: Option<String>,
    /// 失败时的原话（截断）。
    pub error: Option<String>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub ttft_ms: Option<u64>,
    pub duration_ms: u64,
    /// 同一次请求里第几次尝试：前一个号出不了、换号重来时大于 1。
    pub attempt: u8,
}

#[derive(Default)]
pub struct RequestLog {
    entries: Mutex<VecDeque<LogEntry>>,
    seq: AtomicU64,
}

impl RequestLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, mut entry: LogEntry) {
        entry.id = self.seq.fetch_add(1, Ordering::Relaxed) + 1;
        if entry.at.is_empty() {
            entry.at = nexus_core::now_iso();
        }
        if let Some(e) = entry.error.as_mut() {
            if e.chars().count() > 600 {
                *e = e.chars().take(600).collect::<String>() + "…";
            }
        }
        let mut list = self.entries.lock().expect("request log");
        list.push_front(entry);
        list.truncate(CAPACITY);
    }

    /// 新的在前。
    pub fn recent(&self, limit: usize) -> Vec<LogEntry> {
        let list = self.entries.lock().expect("request log");
        list.iter()
            .take(limit.clamp(1, CAPACITY))
            .cloned()
            .collect()
    }

    pub fn clear(&self) {
        self.entries.lock().expect("request log").clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_capped_and_numbered() {
        let log = RequestLog::new();
        for i in 0..(CAPACITY + 5) {
            log.push(LogEntry {
                requested: format!("m{i}"),
                ..LogEntry::default()
            });
        }
        let all = log.recent(CAPACITY * 2);
        assert_eq!(all.len(), CAPACITY);
        assert_eq!(all[0].requested, format!("m{}", CAPACITY + 4));
        assert!(all[0].id > all[1].id);
        assert!(!all[0].at.is_empty());
        log.clear();
        assert!(log.recent(10).is_empty());
    }

    #[test]
    fn long_errors_are_cut() {
        let log = RequestLog::new();
        log.push(LogEntry {
            error: Some("x".repeat(2000)),
            ..LogEntry::default()
        });
        assert!(log.recent(1)[0].error.as_ref().unwrap().chars().count() <= 601);
    }
}
