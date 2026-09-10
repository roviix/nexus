//! 通道（Channel）：网关里「一种号源」的实体。
//!
//! 之前 Cursor / ChatGPT / Grok / Kiro 是 `Gateway` 上四个平行的字段，`route()` 是一条手排的
//! if 链，`/v1/models`、账本、状态快照各自再手写一遍。每加一个平台要摸十几处。这里把它收成
//! 一个东西：
//!
//! ```text
//! Channel = id + 前缀集合 + 一队号 (Lane) + 一个后端 (Upstream) + 门禁 (ChannelGate)
//! ChannelRegistry = 默认通道 (Cursor) + 若干订阅通道，按 (模型名, 能力) 查表选路
//! ```
//!
//! 选路规则只有三条，对聊天 / 生图 / 生视频一致：
//! 1. 显式前缀（`chatgpt/` `grok/` `kiro/` `cursor/`）→ 强制该通道，不看它有没有号；
//! 2. 没前缀 → 按注册顺序找**声明拥有该模型且此刻有号**的通道（生图 / 生视频还要过媒体门禁）；
//! 3. 都不认 → 默认通道。
//!
//! 「有号才接」是为了 `gpt-5.6-sol` 这种两边都有的名字：ChatGPT 这边一个号都没有时，同名请求
//! 照旧走 Cursor，而不是撞一个「没有 ChatGPT 账号」。

use crate::lane::Lane;
use crate::upstream::Upstream;
use serde::Serialize;
use std::sync::Arc;

pub type ChannelId = &'static str;

pub const CURSOR: ChannelId = "cursor";
pub const CHATGPT: ChannelId = "chatgpt";
pub const GROK: ChannelId = "grok";
pub const KIRO: ChannelId = "kiro";

/// 一条通道能做的事。选路按能力问门禁：一个模型名在聊天目录里不代表它能出图。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    Chat,
    Image,
    Video,
}

impl Capability {
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Chat => "chat",
            Capability::Image => "image",
            Capability::Video => "video",
        }
    }
}

/// 一条通道此刻的门禁与目录。每个请求都会问一次，实现要便宜（一次 SQLite 读 / 一次内存读）。
pub trait ChannelGate: Send + Sync {
    /// 此刻有没有号能接。带前缀的显式请求不看这个。
    fn ready(&self) -> bool;
    /// 这个模型名（已剥路由前缀）在这个能力下归这条通道。静态目录 + 上游拉到的目录。
    fn owns(&self, cap: Capability, base_model: &str) -> bool;
    /// 对外报的模型清单（`/v1/models` 与模型广场）。
    fn models(&self, cap: Capability) -> Vec<String>;
    /// 媒体（生图 / 生视频）是否有号能接。默认同 `ready()`；Grok 覆写：免费档服务端零额度，
    /// 有号也不等于能出图。
    fn media_ready(&self) -> bool {
        self.ready()
    }
}

/// 永远开着、认一切的门禁——给默认通道（Cursor）用：它是别的通道都不认时的兜底。
pub struct OpenGate;

impl ChannelGate for OpenGate {
    fn ready(&self) -> bool {
        true
    }

    fn owns(&self, _cap: Capability, _base_model: &str) -> bool {
        true
    }

    fn models(&self, _cap: Capability) -> Vec<String> {
        Vec::new()
    }
}

pub struct Channel {
    pub id: ChannelId,
    /// 给人看的名字（状态快照、日志）。
    pub label: &'static str,
    /// `/v1/models` 的 `owned_by`。
    pub vendor: &'static str,
    /// 显式路由前缀（小写、带斜杠）。匹配到就强制走这条通道。
    pub prefixes: &'static [&'static str],
    pub lane: Arc<dyn Lane>,
    pub upstream: Arc<dyn Upstream>,
    pub gate: Arc<dyn ChannelGate>,
    /// 上游本身讲 Responses（ChatGPT / Grok）：入站是 Responses 时原始体透传、响应头要等上游。
    pub passthrough: bool,
}

impl Channel {
    /// 这个模型名带不带本通道的前缀；带就剥掉。
    pub fn strip_prefix<'m>(&self, model: &'m str) -> Option<&'m str> {
        let m = model.trim();
        for p in self.prefixes {
            if m.len() > p.len() && m[..p.len()].eq_ignore_ascii_case(p) {
                return Some(&m[p.len()..]);
            }
        }
        None
    }
}

/// Cursor 通道：默认通道，`cursor/` 前缀显式要它，门禁常开。
pub fn cursor_channel(lane: Arc<dyn Lane>, upstream: Arc<dyn Upstream>) -> Channel {
    Channel {
        id: CURSOR,
        label: "Cursor",
        vendor: "cursor",
        prefixes: &["cursor/"],
        lane,
        upstream,
        gate: Arc::new(OpenGate),
        passthrough: false,
    }
}

/// 一次选路的结果。
pub struct Resolved<'a> {
    pub channel: &'a Channel,
    /// 剥掉路由前缀后的模型名（原样大小写）。
    pub base_model: String,
    /// 是显式前缀选中的。
    pub forced: bool,
}

pub struct ChannelRegistry {
    channels: Vec<Channel>,
}

impl ChannelRegistry {
    /// 第一条就是默认通道。
    pub fn new(default: Channel) -> Self {
        Self {
            channels: vec![default],
        }
    }

    pub fn with(mut self, channel: Channel) -> Self {
        debug_assert!(
            !self.channels.iter().any(|c| c.id == channel.id),
            "通道 id 重复：{}",
            channel.id
        );
        self.channels.push(channel);
        self
    }

    pub fn default_channel(&self) -> &Channel {
        &self.channels[0]
    }

    pub fn get(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Channel> {
        self.channels.iter()
    }

    /// 订阅通道（默认通道之外的）。
    pub fn extras(&self) -> impl Iterator<Item = &Channel> {
        self.channels.iter().skip(1)
    }

    pub fn resolve(&self, model: &str, cap: Capability) -> Resolved<'_> {
        for ch in &self.channels {
            if let Some(rest) = ch.strip_prefix(model) {
                return Resolved {
                    channel: ch,
                    base_model: rest.to_string(),
                    forced: true,
                };
            }
        }
        let base = model.trim();
        for ch in self.extras() {
            let ok = match cap {
                Capability::Chat => ch.gate.ready(),
                Capability::Image | Capability::Video => ch.gate.media_ready(),
            };
            if ok && ch.gate.owns(cap, base) {
                return Resolved {
                    channel: ch,
                    base_model: base.to_string(),
                    forced: false,
                };
            }
        }
        Resolved {
            channel: self.default_channel(),
            base_model: base.to_string(),
            forced: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceIdentity;
    use crate::lane::{Credential, StaticLane};
    use crate::upstream::CursorUpstream;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct FixedGate {
        ready: AtomicBool,
        media: AtomicBool,
        chat: Vec<&'static str>,
        image: Vec<&'static str>,
    }

    impl ChannelGate for FixedGate {
        fn ready(&self) -> bool {
            self.ready.load(Ordering::Relaxed)
        }
        fn owns(&self, cap: Capability, m: &str) -> bool {
            let list = match cap {
                Capability::Chat => &self.chat,
                Capability::Image => &self.image,
                Capability::Video => return false,
            };
            list.iter().any(|x| x.eq_ignore_ascii_case(m))
        }
        fn models(&self, cap: Capability) -> Vec<String> {
            match cap {
                Capability::Chat => self.chat.iter().map(|s| s.to_string()).collect(),
                Capability::Image => self.image.iter().map(|s| s.to_string()).collect(),
                Capability::Video => vec![],
            }
        }
        fn media_ready(&self) -> bool {
            self.media.load(Ordering::Relaxed)
        }
    }

    fn lane() -> Arc<dyn Lane> {
        Arc::new(StaticLane::new(Credential {
            label: "l".into(),
            access_token: "t".into(),
            identity: DeviceIdentity::derived("t"),
        }))
    }

    fn channel(
        id: ChannelId,
        prefixes: &'static [&'static str],
        gate: Arc<dyn ChannelGate>,
    ) -> Channel {
        Channel {
            id,
            label: id,
            vendor: id,
            prefixes,
            lane: lane(),
            upstream: Arc::new(CursorUpstream::new(Default::default())),
            gate,
            passthrough: false,
        }
    }

    fn registry(ready: bool, media: bool) -> ChannelRegistry {
        let gate = Arc::new(FixedGate {
            ready: AtomicBool::new(ready),
            media: AtomicBool::new(media),
            chat: vec!["grok-4.5"],
            image: vec!["grok-imagine-image"],
        });
        ChannelRegistry::new(channel(CURSOR, &["cursor/"], Arc::new(OpenGate))).with(channel(
            GROK,
            &["grok/", "xai/"],
            gate,
        ))
    }

    #[test]
    fn prefix_forces_the_channel_even_without_accounts() {
        let r = registry(false, false);
        let got = r.resolve("xai/grok-4.5", Capability::Chat);
        assert_eq!(got.channel.id, GROK);
        assert!(got.forced);
        assert_eq!(got.base_model, "grok-4.5");
        assert_eq!(
            r.resolve("cursor/grok-4.5", Capability::Chat).channel.id,
            CURSOR
        );
    }

    #[test]
    fn unprefixed_owned_model_needs_a_ready_channel_else_falls_back() {
        assert_eq!(
            registry(false, false)
                .resolve("grok-4.5", Capability::Chat)
                .channel
                .id,
            CURSOR
        );
        assert_eq!(
            registry(true, false)
                .resolve("grok-4.5", Capability::Chat)
                .channel
                .id,
            GROK
        );
        assert_eq!(
            registry(true, false)
                .resolve("claude-sonnet-5", Capability::Chat)
                .channel
                .id,
            CURSOR
        );
    }

    #[test]
    fn media_routing_uses_the_media_gate_not_the_chat_gate() {
        let r = registry(true, false);
        assert_eq!(
            r.resolve("grok-imagine-image", Capability::Image)
                .channel
                .id,
            CURSOR
        );
        let r = registry(true, true);
        assert_eq!(
            r.resolve("grok-imagine-image", Capability::Image)
                .channel
                .id,
            GROK
        );
        // 聊天目录里的名字不算能出图。
        assert_eq!(r.resolve("grok-4.5", Capability::Image).channel.id, CURSOR);
    }
}
