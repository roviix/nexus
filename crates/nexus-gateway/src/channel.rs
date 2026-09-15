//! 通道（Channel）：网关里「一种号源」的实体。
//!
//! ```text
//! Channel = id + 前缀集合 + 一队号 (Lane) + 一个后端 (Upstream) + 门禁 (ChannelGate)
//! ChannelRegistry = 若干通道 + 用户指定的默认通道
//! ```
//!
//! 选路只有两条，对聊天 / 生图 / 生视频一致，不按模型名猜该走谁：
//! 1. 显式前缀（`chatgpt/` `grok/` `kiro/` `cursor/`）→ 强制该通道，不看它有没有号；
//! 2. 裸名或空模型 → 用户设的默认通道。空模型再填该通道此刻目录里的第一个。
//!
//! 对外目录的主键是 `{通道}/{模型}`（`cursor/claude-opus-5`）。裸名还能打进来，是因为
//! 用户把某条通道设成默认之后，客户端可以继续写短名字。

use crate::lane::Lane;
use crate::upstream::Upstream;
use serde::Serialize;
use std::sync::{Arc, RwLock};

pub type ChannelId = &'static str;

pub const CURSOR: ChannelId = "cursor";
pub const CHATGPT: ChannelId = "chatgpt";
pub const GROK: ChannelId = "grok";
pub const KIRO: ChannelId = "kiro";

/// 规范通道 id。别名（`codex/` `xai/`）只在请求前缀里认，不进这一张表。
pub fn parse_id(id: &str) -> Option<ChannelId> {
    match id.trim().to_ascii_lowercase().as_str() {
        "cursor" => Some(CURSOR),
        "chatgpt" => Some(CHATGPT),
        "grok" => Some(GROK),
        "kiro" => Some(KIRO),
        _ => None,
    }
}

/// 目录 / 接入用的主键：`{通道}/{模型}`。已经带了本通道前缀的原样返回，避免叠两层。
pub fn qualify(channel: &str, model: &str) -> String {
    let model = model.trim();
    if model.is_empty() {
        return channel.to_string();
    }
    let prefix = format!("{channel}/");
    if model.len() >= prefix.len() && model[..prefix.len()].eq_ignore_ascii_case(&prefix) {
        model.to_string()
    } else {
        format!("{channel}/{model}")
    }
}

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

/// 永远开着、认一切的门禁——给 Cursor 用：它不靠目录把门，客户端叫得出的名字都往上送。
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
            // 按**字节**比：前缀全是 ASCII，但 `model` 直接来自客户端请求体，可能是任意
            // UTF-8。`m[..p.len()]` 在多字节字符中间切会 panic（`"中文模型"` 对 `"cursor/"`
            // 就正好切在第三个字之内），而那是一条只要发个中文模型名就能踩到的路。
            if m.len() >= p.len() && m.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes()) {
                // 前缀以 `/` 收尾，匹配上就保证 `p.len()` 落在字符边界上，这一刀是安全的。
                return Some(&m[p.len()..]);
            }
        }
        None
    }
}

/// Cursor 通道：出厂默认，`cursor/` 前缀显式要它，门禁常开。
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
    /// 用户指定的默认通道。和注册表分开一把锁，改设置不用重建通道、也不用重启网关。
    default_id: Arc<RwLock<String>>,
}

impl ChannelRegistry {
    /// 第一条是 Cursor（结构上的底），出厂默认也是它；用户之后可以改。
    pub fn new(cursor: Channel) -> Self {
        Self {
            channels: vec![cursor],
            default_id: Arc::new(RwLock::new(CURSOR.to_string())),
        }
    }

    /// 跟服务层共用同一把默认通道锁：设置一改，正在听的网关下一发就照新的走。
    pub fn share_default(mut self, slot: Arc<RwLock<String>>) -> Self {
        self.default_id = slot;
        self
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

    pub fn default_id(&self) -> String {
        self.default_id.read().expect("default channel").clone()
    }

    pub fn set_default(&self, id: &str) -> Result<(), String> {
        let Some(parsed) = parse_id(id) else {
            return Err(format!(
                "默认通道只能是 cursor / chatgpt / grok / kiro，给的是 {id}"
            ));
        };
        if self.get(parsed).is_none() {
            return Err(format!("没有这条通道：{parsed}"));
        }
        *self.default_id.write().expect("default channel") = parsed.to_string();
        Ok(())
    }

    pub fn default_channel(&self) -> &Channel {
        let id = self.default_id();
        self.get(&id).unwrap_or(&self.channels[0])
    }

    pub fn get(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Channel> {
        self.channels.iter()
    }

    /// 订阅通道（Cursor 之外的）。
    pub fn extras(&self) -> impl Iterator<Item = &Channel> {
        self.channels.iter().skip(1)
    }

    pub fn resolve(&self, model: &str, cap: Capability) -> Resolved<'_> {
        let model = model.trim();
        for ch in &self.channels {
            if let Some(rest) = ch.strip_prefix(model) {
                let base = if rest.trim().is_empty() {
                    fallback_model(ch, cap)
                } else {
                    rest.to_string()
                };
                return Resolved {
                    channel: ch,
                    base_model: base,
                    forced: true,
                };
            }
        }
        let ch = self.default_channel();
        let base = if model.is_empty() {
            fallback_model(ch, cap)
        } else {
            model.to_string()
        };
        Resolved {
            channel: ch,
            base_model: base,
            forced: false,
        }
    }
}

fn fallback_model(ch: &Channel, cap: Capability) -> String {
    if let Some(id) = ch.gate.models(cap).into_iter().next() {
        return id;
    }
    match cap {
        Capability::Chat | Capability::Image => "auto".into(),
        Capability::Video => String::new(),
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

    /// `model` 整个来自客户端请求体，可能是任意 UTF-8。按字节切前缀时若切在多字节字符中间
    /// 会 panic，而这条路发一个中文模型名就能踩到。
    #[test]
    fn a_multibyte_model_name_does_not_panic() {
        let r = registry(true, true);
        for name in ["中文模型", "модель", "🙂", "グロック", "xai/中文模型"] {
            let got = r.resolve(name, Capability::Chat);
            // 只有最后那个带前缀的才该被强制到 Grok；其余是裸名，走默认通道。
            let expect_forced = name.starts_with("xai/");
            assert_eq!(got.forced, expect_forced, "{name}");
            assert_eq!(
                got.channel.id,
                if expect_forced { GROK } else { CURSOR },
                "{name}"
            );
        }
    }

    #[test]
    fn bare_names_go_to_the_user_default_not_whoever_owns_them() {
        let r = registry(true, true);
        assert_eq!(r.resolve("grok-4.5", Capability::Chat).channel.id, CURSOR);
        assert_eq!(
            r.resolve("grok-imagine-image", Capability::Image)
                .channel
                .id,
            CURSOR
        );
        r.set_default(GROK).unwrap();
        assert_eq!(r.resolve("grok-4.5", Capability::Chat).channel.id, GROK);
        assert_eq!(
            r.resolve("claude-sonnet-5", Capability::Chat).channel.id,
            GROK
        );
        assert_eq!(r.resolve("", Capability::Chat).base_model, "grok-4.5");
        assert_eq!(
            r.resolve("cursor/claude-sonnet-5", Capability::Chat)
                .channel
                .id,
            CURSOR
        );
    }

    #[test]
    fn qualify_does_not_double_the_channel_prefix() {
        assert_eq!(qualify(CURSOR, "claude-opus-5"), "cursor/claude-opus-5");
        assert_eq!(
            qualify(CURSOR, "cursor/claude-opus-5"),
            "cursor/claude-opus-5"
        );
        assert_eq!(qualify("chatgpt", ""), "chatgpt");
    }
}
