//! 本地使用优先抢占与并发水位控制器。
//!
//! 核心原则：用户 A 自己使用 Nexus 网关时，拥有 100% 绝对优先级。
//! 外部 Mesh 共享流量在本地活动期间自动暂停或让路。

use crate::types::{EgressMetrics, PreemptionStatus};
use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug)]
pub struct PreemptionPolicy {
    /// 最近一次本地网关请求的 Unix 时间戳 (毫秒)
    last_local_activity_ms: AtomicI64,
    /// 本地活动抢占冷却期 (在此时间内优先本地，外部流量退避)
    cooldown: Duration,
    /// 最大并发外部流限制
    max_concurrency: usize,
    /// 当前活跃连接计数
    active_connections: Arc<AtomicUsize>,
    /// 累计服务数
    total_served: AtomicU64,
    /// 累计转发字节
    total_bytes: AtomicU64,
    /// 拒绝连接数
    rejected_count: AtomicU64,
}

impl PreemptionPolicy {
    pub fn new(cooldown: Duration, max_concurrency: usize) -> Self {
        Self {
            last_local_activity_ms: AtomicI64::new(0),
            cooldown,
            max_concurrency,
            active_connections: Arc::new(AtomicUsize::new(0)),
            total_served: AtomicU64::new(0),
            total_bytes: AtomicU64::new(0),
            rejected_count: AtomicU64::new(0),
        }
    }

    /// 本地网关发起请求时调用此方法，更新活动时间戳。
    pub fn record_local_activity(&self) {
        let now = now_ms();
        self.last_local_activity_ms.store(now, Ordering::Relaxed);
    }

    /// 检查当前是否处于抢占冷却期中。
    pub fn is_preempted(&self) -> bool {
        let last = self.last_local_activity_ms.load(Ordering::Relaxed);
        if last == 0 {
            return false;
        }
        let now = now_ms();
        now.saturating_sub(last) < (self.cooldown.as_millis() as i64)
    }

    /// 尝试准入外部 Mesh 连接请求。
    pub fn try_acquire(&self) -> Result<ActiveGuard, &'static str> {
        if self.is_preempted() {
            self.rejected_count.fetch_add(1, Ordering::Relaxed);
            return Err("本机用户正在使用，优先保留本地网络与账号额度");
        }

        let curr = self.active_connections.load(Ordering::Relaxed);
        if curr >= self.max_concurrency {
            self.rejected_count.fetch_add(1, Ordering::Relaxed);
            return Err("已达到最大允许并发上限");
        }

        self.active_connections.fetch_add(1, Ordering::SeqCst);
        self.total_served.fetch_add(1, Ordering::Relaxed);

        Ok(ActiveGuard {
            active_counter: Arc::clone(&self.active_connections),
        })
    }

    /// 累计记录传输字节。
    pub fn record_bytes(&self, bytes: usize) {
        self.total_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// 导出实时统计快照。
    pub fn metrics(&self) -> EgressMetrics {
        EgressMetrics {
            total_requests_served: self.total_served.load(Ordering::Relaxed),
            total_bytes_forwarded: self.total_bytes.load(Ordering::Relaxed),
            active_connections: self.active_connections.load(Ordering::Relaxed),
            rejected_connections: self.rejected_count.load(Ordering::Relaxed),
            contribution_points: self.total_served.load(Ordering::Relaxed) * 5,
        }
    }

    /// 获取详细抢占状态快照。
    pub fn preemption_status(&self) -> PreemptionStatus {
        let last = self.last_local_activity_ms.load(Ordering::Relaxed);
        if last == 0 {
            return PreemptionStatus::default();
        }
        let now = now_ms();
        let elapsed = now.saturating_sub(last);
        let cooldown_ms = self.cooldown.as_millis() as i64;
        if elapsed < cooldown_ms {
            let remaining = ((cooldown_ms - elapsed) / 1000).max(0) as u64;
            PreemptionStatus {
                is_preempting: true,
                cooldown_remaining_secs: remaining,
                last_local_activity: Some(last / 1000),
            }
        } else {
            PreemptionStatus {
                is_preempting: false,
                cooldown_remaining_secs: 0,
                last_local_activity: Some(last / 1000),
            }
        }
    }
}

/// 活跃连接 RAII 释放守卫 (100% 安全 Rust 实现)
pub struct ActiveGuard {
    active_counter: Arc<AtomicUsize>,
}

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.active_counter.fetch_sub(1, Ordering::SeqCst);
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preemption_policy() {
        let policy = PreemptionPolicy::new(Duration::from_millis(50), 2);
        assert!(!policy.is_preempted());

        // 准入两个连接
        let g1 = policy.try_acquire().expect("acquire 1");
        let g2 = policy.try_acquire().expect("acquire 2");
        assert_eq!(policy.metrics().active_connections, 2);

        // 超出并发限制拒绝
        assert!(policy.try_acquire().is_err());

        drop(g1);
        assert_eq!(policy.metrics().active_connections, 1);

        // 模拟本地活动触发抢占
        policy.record_local_activity();
        assert!(policy.is_preempted());
        assert!(policy.try_acquire().is_err());

        drop(g2);
    }
}
