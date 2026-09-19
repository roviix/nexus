//! 四个订阅平台（ChatGPT / Grok Build / Kiro / ZCode）接进通道模型的地方。
//!
//! 每个平台只回答两组问题：**号**（[`SubscriptionAccounts`]：谁能进队、给我 token）和
//! **目录**（[`ChannelGate`]：这个名字归不归你、你此刻能不能接）。lane、选路、账本、状态快照
//! 都不再认识具体平台——加第五个平台就是在这个文件里再写一对 impl 加一个 `*_channel()`。

use crate::channel::{Capability, Channel, ChannelGate, CHATGPT, GROK, KIRO, ZCODE};
use crate::codex::{CodexConfig, CodexUpstream};
use crate::grok::GrokUpstream;
use crate::kiro::KiroUpstream;
use crate::lane::{
    chatgpt_quota_hint, BoxFuture, Lane, QuotaHint, SubscriptionAccounts, SubscriptionCandidate,
    SubscriptionSource,
};
use crate::zcode::{ZcodeRoute, ZcodeRouting, ZcodeUpstream};
use nexus_chatgpt::ChatGptService;
use nexus_grok::GrokService;
use nexus_kiro::KiroService;
use nexus_zcode::ZcodeService;
use std::sync::Arc;
use std::time::SystemTime;

/// 有没有一个开着、有凭证、没被判失效的号。三条通道的 `ready()` 都是这一句。
pub fn channel_ready(accounts: &dyn SubscriptionAccounts) -> bool {
    !accounts.candidates().is_empty()
}

// ---------- ChatGPT ----------

impl SubscriptionAccounts for ChatGptService {
    fn channel(&self) -> &'static str {
        CHATGPT
    }

    fn candidates(&self) -> Vec<SubscriptionCandidate> {
        let list = match self.list() {
            Ok(l) => l,
            Err(err) => {
                tracing::warn!(%err, "读 ChatGPT 账号列表失败");
                return Vec::new();
            }
        };
        list.into_iter()
            .filter(|a| {
                a.enabled && a.status == nexus_chatgpt::ChatGptStatus::Active && a.has_refresh
            })
            .map(|a| SubscriptionCandidate {
                label: a.label(),
                quota: chatgpt_quota_hint(a.usage.as_ref()),
                id: a.id.as_str().to_string(),
            })
            .collect()
    }

    fn access_token<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, nexus_core::Result<nexus_core::Secret>> {
        Box::pin(async move {
            ChatGptService::access_token(
                self,
                &nexus_core::ChatGptAccountId::from_raw(id.to_string()),
            )
            .await
        })
    }

    fn token_expiry(&self, access_token: &str) -> Option<SystemTime> {
        nexus_chatgpt::oauth::jwt_expiry(access_token).map(SystemTime::from)
    }
}

/// ChatGPT 通道的门禁：模型名在静态目录或上游拉到的目录里就认（剥掉档位后缀比）。
pub struct ChatGptGate {
    pub accounts: Arc<ChatGptService>,
}

/// 这个号实际能用的对话模型。上游目录拉到过就以它为准（有 `gpt-6-astra` 就排第一）；
/// 还没拉到则用静态表，且不把 Astra 排在默认位——很多号对它 400。
pub fn merge_chatgpt_chat_models(live: &[String]) -> Vec<String> {
    if live.is_empty() {
        return crate::codex::protocol::CODEX_MODELS
            .iter()
            .filter(|s| **s != crate::codex::protocol::PREFERRED_CHAT_MODEL)
            .map(|s| (*s).to_string())
            .collect();
    }
    let mut out = live.to_vec();
    if let Some(i) = out
        .iter()
        .position(|m| m.eq_ignore_ascii_case(crate::codex::protocol::PREFERRED_CHAT_MODEL))
    {
        let preferred = out.remove(i);
        out.insert(0, preferred);
    }
    out
}

pub fn chatgpt_chat_models(accounts: &ChatGptService) -> Vec<String> {
    let live: Vec<String> = accounts.models().into_iter().map(|m| m.slug).collect();
    merge_chatgpt_chat_models(&live)
}

impl ChannelGate for ChatGptGate {
    fn ready(&self) -> bool {
        channel_ready(self.accounts.as_ref())
    }

    fn owns(&self, cap: Capability, base_model: &str) -> bool {
        match cap {
            Capability::Chat => {
                let base = crate::codex::protocol::split_effort_suffix(base_model)
                    .0
                    .to_ascii_lowercase();
                crate::codex::protocol::is_codex_model(&base)
                    || self
                        .accounts
                        .models()
                        .iter()
                        .any(|m| m.slug.eq_ignore_ascii_case(&base))
            }
            Capability::Image => crate::codex::protocol::is_codex_image_model(base_model),
            Capability::Video => false,
        }
    }

    fn models(&self, cap: Capability) -> Vec<String> {
        match cap {
            Capability::Chat => chatgpt_chat_models(&self.accounts),
            Capability::Image => crate::codex::protocol::CODEX_IMAGE_MODELS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            Capability::Video => Vec::new(),
        }
    }
}

pub fn chatgpt_channel(lane: Arc<dyn Lane>, accounts: Arc<ChatGptService>) -> Channel {
    Channel {
        id: CHATGPT,
        label: "ChatGPT",
        vendor: "openai",
        prefixes: crate::codex::protocol::ROUTE_PREFIXES,
        lane,
        upstream: Arc::new(CodexUpstream::new(
            CodexConfig::default(),
            Some(accounts.clone()),
        )),
        gate: Arc::new(ChatGptGate { accounts }),
        passthrough: true,
    }
}

// ---------- Grok Build ----------

impl SubscriptionAccounts for GrokService {
    fn channel(&self) -> &'static str {
        GROK
    }

    fn candidates(&self) -> Vec<SubscriptionCandidate> {
        let list = match self.list() {
            Ok(l) => l,
            Err(err) => {
                tracing::warn!(%err, "读 Grok 账号列表失败");
                return Vec::new();
            }
        };
        list.into_iter()
            .filter(|a| a.usable())
            .map(|a| SubscriptionCandidate {
                label: a.label(),
                quota: grok_quota_hint(a.usage.as_ref()),
                id: a.id.as_str().to_string(),
            })
            .collect()
    }

    fn access_token<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, nexus_core::Result<nexus_core::Secret>> {
        Box::pin(async move {
            GrokService::access_token(self, &nexus_core::GrokAccountId::from_raw(id.to_string()))
                .await
        })
    }

    fn token_expiry(&self, access_token: &str) -> Option<SystemTime> {
        nexus_grok::oauth::jwt_expiry(access_token).map(SystemTime::from)
    }
}

/// Grok 的额度线索：周额度用满（`creditUsagePercent ≥ 99.5`）且周期还没过 → 到线。
pub fn grok_quota_hint(usage: Option<&nexus_grok::GrokQuota>) -> QuotaHint {
    let Some(u) = usage else {
        return QuotaHint::default();
    };
    QuotaHint {
        percent_used: u.credit_usage_percent,
        exhausted: u.is_exhausted_now(),
    }
}

pub struct GrokGate {
    pub accounts: Arc<GrokService>,
}

impl ChannelGate for GrokGate {
    fn ready(&self) -> bool {
        channel_ready(self.accounts.as_ref())
    }

    fn owns(&self, cap: Capability, base_model: &str) -> bool {
        match cap {
            Capability::Chat => {
                nexus_grok::is_grok_model(base_model)
                    && !nexus_grok::is_grok_media_model(base_model)
            }
            Capability::Image => nexus_grok::is_grok_image_model(base_model),
            Capability::Video => nexus_grok::is_grok_video_model(base_model),
        }
    }

    fn models(&self, cap: Capability) -> Vec<String> {
        match cap {
            Capability::Chat => self.accounts.models(),
            Capability::Image => nexus_grok::GROK_IMAGE_MODELS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            Capability::Video => nexus_grok::GROK_VIDEO_MODELS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }

    /// 免费 / X Basic 档在 Imagine 服务端是零额度：有号也出不了图。只有已知付费、或还没探过
    /// （让它去撞一次，撞回 402/403 就记下来）的号才算能接媒体。
    fn media_ready(&self) -> bool {
        self.accounts
            .list()
            .map(|l| {
                l.iter()
                    .any(|a| a.usable() && a.media_eligible != Some(false))
            })
            .unwrap_or(false)
    }
}

pub fn grok_channel(lane: Arc<dyn Lane>, accounts: Arc<GrokService>) -> Channel {
    Channel {
        id: GROK,
        label: "Grok Build",
        vendor: "xai",
        prefixes: nexus_grok::protocol::ROUTE_PREFIXES,
        lane,
        upstream: Arc::new(GrokUpstream::new(Some(accounts.clone()))),
        gate: Arc::new(GrokGate { accounts }),
        passthrough: true,
    }
}

// ---------- Kiro ----------

impl SubscriptionAccounts for KiroService {
    fn channel(&self) -> &'static str {
        KIRO
    }

    fn candidates(&self) -> Vec<SubscriptionCandidate> {
        let list = match self.list() {
            Ok(l) => l,
            Err(err) => {
                tracing::warn!(%err, "读 Kiro 账号列表失败");
                return Vec::new();
            }
        };
        list.into_iter()
            .filter(|a| a.enabled && a.status == nexus_kiro::KiroStatus::Active && a.has_refresh)
            .map(|a| SubscriptionCandidate {
                label: a.label(),
                quota: QuotaHint::default(),
                id: a.id.as_str().to_string(),
            })
            .collect()
    }

    fn access_token<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, nexus_core::Result<nexus_core::Secret>> {
        Box::pin(async move {
            KiroService::access_token(self, &nexus_core::KiroAccountId::from_raw(id.to_string()))
                .await
        })
    }

    fn token_expiry(&self, access_token: &str) -> Option<SystemTime> {
        nexus_kiro::oauth::jwt_expiry(access_token).map(SystemTime::from)
    }
}

pub struct KiroGate {
    pub accounts: Arc<KiroService>,
}

impl ChannelGate for KiroGate {
    fn ready(&self) -> bool {
        channel_ready(self.accounts.as_ref())
    }

    fn owns(&self, cap: Capability, base_model: &str) -> bool {
        // 裸的 `claude-*` 是 Cursor 的；这里只认 `kiro-claude-*`。
        cap == Capability::Chat && nexus_kiro::is_kiro_model(base_model)
    }

    fn models(&self, cap: Capability) -> Vec<String> {
        match cap {
            Capability::Chat => self.accounts.models(),
            _ => Vec::new(),
        }
    }
}

pub fn kiro_channel(lane: Arc<dyn Lane>, accounts: Arc<KiroService>) -> Channel {
    Channel {
        id: KIRO,
        label: "Kiro",
        vendor: "aws",
        prefixes: nexus_kiro::protocol::ROUTE_PREFIXES,
        lane,
        upstream: Arc::new(KiroUpstream::new()),
        gate: Arc::new(KiroGate { accounts }),
        passthrough: false,
    }
}

// ---------- ZCode ----------

impl SubscriptionAccounts for ZcodeService {
    fn channel(&self) -> &'static str {
        ZCODE
    }

    fn candidates(&self) -> Vec<SubscriptionCandidate> {
        let list = match self.list() {
            Ok(l) => l,
            Err(err) => {
                tracing::warn!(%err, "读 ZCode 账号列表失败");
                return Vec::new();
            }
        };
        list.into_iter()
            .filter(|a| {
                a.enabled
                    && a.status == nexus_zcode::ZcodeStatus::Active
                    && a.has_credential()
                    // 体验套餐进不了队：它的网关每个请求都要验证码票据，本地网关还没有求解器。
                    // 号照样留在列表里（用户能看见、能删），只是不当候选。
                    && a.plan == nexus_zcode::ZcodePlan::CodingPlan
            })
            .map(|a| SubscriptionCandidate {
                label: a.label(),
                quota: QuotaHint::default(),
                id: a.id.as_str().to_string(),
            })
            .collect()
    }

    fn access_token<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, nexus_core::Result<nexus_core::Secret>> {
        Box::pin(async move {
            ZcodeService::credential(self, &nexus_core::ZcodeAccountId::from_raw(id.to_string()))
                .await
        })
    }

    /// 编码套餐的凭证是永久 API key，根本不是 JWT，没有过期时刻可读。
    fn token_expiry(&self, _access_token: &str) -> Option<SystemTime> {
        None
    }
}

/// 从 lane 的 label 回头问「这个号走哪条上游」。
///
/// `Credential` 只带 label 和 token，装不下服务商与套餐档，而这两者决定 URL 和认证头。
impl ZcodeRouting for ZcodeService {
    fn route(&self, label: &str) -> Option<ZcodeRoute> {
        let list = self.list().ok()?;
        let hit = list.into_iter().find(|a| a.label() == label)?;
        Some(ZcodeRoute {
            provider: hit.provider,
            plan: hit.plan,
        })
    }
}

pub struct ZcodeGate {
    pub accounts: Arc<ZcodeService>,
}

impl ChannelGate for ZcodeGate {
    fn ready(&self) -> bool {
        channel_ready(self.accounts.as_ref())
    }

    fn owns(&self, cap: Capability, base_model: &str) -> bool {
        cap == Capability::Chat && nexus_zcode::is_zcode_model(base_model)
    }

    fn models(&self, cap: Capability) -> Vec<String> {
        match cap {
            Capability::Chat => self.accounts.models(),
            _ => Vec::new(),
        }
    }
}

pub fn zcode_channel(lane: Arc<dyn Lane>, accounts: Arc<ZcodeService>) -> Channel {
    Channel {
        id: ZCODE,
        label: "ZCode",
        vendor: "zhipu",
        prefixes: nexus_zcode::protocol::ROUTE_PREFIXES,
        lane,
        upstream: Arc::new(ZcodeUpstream::new(accounts.clone())),
        gate: Arc::new(ZcodeGate { accounts }),
        passthrough: false,
    }
}

/// 一条订阅通道的接力队：只有这个平台自己的号，全部自动进队（它们在这个应用里只有这一个用途）。
pub fn subscription_lane(accounts: Arc<dyn SubscriptionAccounts>) -> crate::lane::RelayLane {
    crate::lane::RelayLane::new(
        vec![Arc::new(SubscriptionSource::new(accounts)) as Arc<dyn crate::lane::Source>],
        Arc::new(crate::lane::Roster::open()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_catalog_puts_astra_first_when_the_account_has_it() {
        let live = vec!["gpt-5.4".into(), "gpt-6-astra".into(), "gpt-5.5".into()];
        assert_eq!(
            merge_chatgpt_chat_models(&live)[0],
            crate::codex::protocol::PREFERRED_CHAT_MODEL
        );
    }

    #[test]
    fn empty_live_catalog_does_not_default_to_astra() {
        let models = merge_chatgpt_chat_models(&[]);
        assert_eq!(models[0], crate::codex::protocol::DEFAULT_CHAT_MODEL);
        assert!(!models
            .iter()
            .any(|m| m == crate::codex::protocol::PREFERRED_CHAT_MODEL));
    }
}
