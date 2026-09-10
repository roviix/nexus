//! 号从哪来。
//!
//! server 层不认识账号、秘密存储、额度，只认一个 [`Lane`]：每次请求要一份能用的凭证，
//! 用完把结果告诉它。真正的策略都在实现里：
//!
//! - [`RelayLane`]：**额度接力**。候选号来自若干 [`Source`]（Cursor 里正登着的号钉真机码、
//!   `nexus-accounts` 里的号派生机器码），但只有用户放进名单（[`Roster`]）的号才真进队；
//!   一直用当前号，额度耗尽才接力下一个；某个模型被限流只绕行不换号；冷却与耗尽都带 TTL 自愈。
//! - [`StaticLane`]：固定一份凭证。给测试、给最简形态。
//!
//! 冷却按（号 × 模型）记，所以 `acquire` / `report` 都带 `model`。

pub mod relay;
pub mod roster;
pub mod sources;

pub use relay::{AvailableView, CandidateState, CandidateView, LaneSnapshot, RelayLane};
pub use roster::Roster;
pub use sources::{
    chatgpt_quota_hint, quota_hint, Candidate, CandidateKind, CursorLogin, CursorLoginSource,
    LoginReader, QuotaHint, ResolvedToken, Source, StoredAccountsSource, SubscriptionAccounts,
    SubscriptionCandidate, SubscriptionSource,
};

use crate::error::UpstreamError;
use crate::identity::DeviceIdentity;
use crate::normalized::Usage;
use std::future::Future;
use std::pin::Pin;

/// 一份可以直接出流量的凭证。
#[derive(Clone)]
pub struct Credential {
    /// 给人看的（邮箱），进日志、进界面，也是 Lane 内部的键。
    pub label: String,
    /// 当前有效的 access token。**是秘密**——不进日志、不进事件。
    pub access_token: String,
    pub identity: DeviceIdentity,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("label", &self.label)
            .field("access_token", &"<redacted>")
            .field("identity", &self.identity)
            .finish()
    }
}

/// 一次请求的结局，喂回给 Lane 做接力决策。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome<'a> {
    Ok(&'a Usage),
    Err(&'a UpstreamError),
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait Lane: Send + Sync {
    /// 为这个模型拿一份凭证。拿不到（没号 / 全部耗尽 / 刷不出 token）就用 [`UpstreamError`] 说清楚。
    fn acquire<'a>(&'a self, model: &'a str) -> BoxFuture<'a, Result<Credential, UpstreamError>>;
    /// 拿**指定那个号**的凭证，不走接力。异步媒体任务的状态轮询要回到创建它的号。
    /// 默认不支持（只有一份凭证的 lane 直接给那一份）。
    fn acquire_label<'a>(
        &'a self,
        label: &'a str,
    ) -> BoxFuture<'a, Result<Credential, UpstreamError>> {
        let _ = label;
        Box::pin(async {
            Err(UpstreamError::new(
                crate::error::UpstreamKind::Upstream,
                404,
                "这条通道不支持按账号取号",
            ))
        })
    }
    /// 一次请求结束后回报。实现据此决定：额度到线 → 接力下一个；这个模型限流 → 绕行。
    fn report(&self, credential: &Credential, model: &str, outcome: Outcome<'_>);
}

/// 固定一份凭证，不接力。
pub struct StaticLane {
    credential: Credential,
}

impl StaticLane {
    pub fn new(credential: Credential) -> Self {
        Self { credential }
    }
}

impl Lane for StaticLane {
    fn acquire<'a>(&'a self, _model: &'a str) -> BoxFuture<'a, Result<Credential, UpstreamError>> {
        Box::pin(async move { Ok(self.credential.clone()) })
    }

    fn acquire_label<'a>(
        &'a self,
        _label: &'a str,
    ) -> BoxFuture<'a, Result<Credential, UpstreamError>> {
        Box::pin(async move { Ok(self.credential.clone()) })
    }

    fn report(&self, credential: &Credential, model: &str, outcome: Outcome<'_>) {
        match outcome {
            Outcome::Ok(u) => tracing::debug!(
                account = %credential.label, model, input = u.input_tokens, output = u.output_tokens, "完成"
            ),
            Outcome::Err(e) => tracing::warn!(
                account = %credential.label, model, kind = e.kind.as_str(), status = e.status, "失败"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_debug_never_prints_the_token() {
        let c = Credential {
            label: "a@b".into(),
            access_token: "SECRET-TOKEN".into(),
            identity: DeviceIdentity::derived("x"),
        };
        let s = format!("{c:?}");
        assert!(s.contains("a@b"));
        assert!(!s.contains("SECRET-TOKEN"));
    }

    #[tokio::test]
    async fn static_lane_hands_out_the_same_credential() {
        let lane = StaticLane::new(Credential {
            label: "l".into(),
            access_token: "t".into(),
            identity: DeviceIdentity::derived("t"),
        });
        let a = lane.acquire("m").await.unwrap();
        let b = lane.acquire("other").await.unwrap();
        assert_eq!(a.label, b.label);
        assert_eq!(a.identity, b.identity);
    }
}
