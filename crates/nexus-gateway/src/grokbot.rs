//! 透传口的 Grok Bot 额度开关：开着时 `InferenceService/Stream` 不用接力队里的号，改用
//! `nexus-grokbot` 维护的 grokBotToken（快过期自动凭 `sbi_*` 续期）。
//!
//! 这不是第三种号源——它只管这一条路径、不进 [`crate::lane::Lane`]、不参与接力 / 冷却：
//! Grok Bot 的额度是一个独立池子，混进接力队会让「额度耗尽换下一个」的语义变得说不清。
//! 别的路径（`agent.v1.*`、`/auth/*`、其它 aiserver 方法）照旧走 Lane。
//!
//! 与 Sand 补丁的关系：Sand 页「推理经本机网关」把 IDE 的 Stream 改道到这个口；补丁侧 Grok 鉴权
//! 选「关」或「直连」都行（直连时补丁先换一次 Bearer、这里再换一次，无害）；Box Relay 会把 URL
//! 改到 Box 去，根本到不了这里，所以 Sand 侧禁止两者同开。

use crate::error::{UpstreamError, UpstreamKind};
use crate::identity::DeviceIdentity;
use crate::lane::Credential;
use nexus_core::ErrorCode;
use nexus_grokbot::GrokBotService;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const SETTING_GROKBOT_STREAM: &str = "gateway.grokbot_stream";
/// 走 Grok Bot 额度的请求在账本里的账号标签前缀。
pub const GROKBOT_LABEL_PREFIX: &str = "grokbot:";

pub struct GrokBotStreamAuth {
    svc: Arc<GrokBotService>,
    enabled: AtomicBool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GrokBotStreamSnapshot {
    pub enabled: bool,
    /// 本地直连凭证的状态（`None` = 还没生成）。开着而这里是 `None` / 过期不可续，Stream 会 502。
    pub credential: Option<nexus_grokbot::DirectCredentialInfo>,
}

impl GrokBotStreamAuth {
    pub fn new(svc: Arc<GrokBotService>, enabled: bool) -> Self {
        Self {
            svc,
            enabled: AtomicBool::new(enabled),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    pub fn service(&self) -> &Arc<GrokBotService> {
        &self.svc
    }

    pub fn snapshot(&self) -> GrokBotStreamSnapshot {
        GrokBotStreamSnapshot {
            enabled: self.enabled(),
            credential: self.svc.status().direct,
        }
    }

    /// 一份可以直接出流量的 Grok Bot 凭证。`machine_id` 钉 Grok Bot 自己的 machineId——它就是
    /// 那台「电脑」，别再派生一台。
    pub async fn credential(&self) -> Result<Credential, UpstreamError> {
        let cred = self.svc.fresh_stream_credential().await.map_err(|e| {
            let kind = match e.code {
                ErrorCode::Unauthorized | ErrorCode::SecretMissing => UpstreamKind::Auth,
                ErrorCode::Network => UpstreamKind::Upstream,
                _ => UpstreamKind::Upstream,
            };
            UpstreamError::new(kind, 502, format!("Grok Bot 凭证不可用：{e}"))
        })?;
        let label = format!(
            "{GROKBOT_LABEL_PREFIX}{}",
            cred.account_email.as_deref().unwrap_or("grok-bot")
        );
        Ok(Credential {
            label,
            identity: DeviceIdentity::pinned(&cred.grok_bot_token, cred.machine_id.clone()),
            access_token: cred.grok_bot_token,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_is_hot() {
        let dir = tempfile::tempdir().unwrap();
        let g = GrokBotStreamAuth::new(Arc::new(GrokBotService::offline(dir.path())), false);
        assert!(!g.enabled());
        g.set_enabled(true);
        assert!(g.snapshot().enabled);
        assert!(g.snapshot().credential.is_none());
    }

    #[tokio::test]
    async fn missing_credential_is_an_auth_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let g = GrokBotStreamAuth::new(Arc::new(GrokBotService::offline(dir.path())), true);
        let err = g.credential().await.unwrap_err();
        assert_eq!(err.kind, UpstreamKind::Auth);
    }
}
