//! 按客户端的路由。
//!
//! 一键接入写进客户端的地址带着客户端名：`http://127.0.0.1:8787/client/claude`。从这个口进来的
//! 请求按**这个客户端自己的路由**选通道和模型，不再看全局默认通道——Claude Code 走 Cursor、
//! Codex 走 ChatGPT 可以同时成立，谁也不用为了另一个去改默认。
//!
//! 路由存在网关设置里，改了下一发就照新的走：客户端配置只写一次，换通道、换模型在应用里点，
//! 不用重写配置、不用重启客户端。Claude Code 靠它的四档别名做到这一点：配置里写的是稳定的
//! 官方档名（`claude-sonnet-4-6` 这种），网关按档名里的 sonnet / opus / haiku / fable 认出是
//! 哪一档，再换成这里配的真实模型。
//!
//! 选模型的规矩，从上往下第一条命中就停：
//!
//! 1. 名字带通道前缀（`chatgpt/gpt-5.4`）——客户端里手动指名的，原样尊重；
//! 2. Claude Code 的档名——换成这一档配的模型，没单独配的档跟主模型；
//! 3. 空名字、或者是接入时写进这个客户端配置的名字（[`ClientRoute::aliases`]）——主模型。
//!    路由改了、客户端还没重开，它发来的仍是旧配置里的名字，照样跟着新路由走；
//! 4. 路由所在的那条通道认识这个裸名（Codex 里 `/model` 换了一个）——就在这条通道上跑它；
//! 5. 其余一律走主模型。

use crate::channel::{qualify, Capability, ChannelRegistry};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 会写配置的几个客户端。地址里的名字就是这几个。
pub const CLIENTS: &[&str] = &["claude", "codex", "opencode", "grok"];

/// 地址里认不认这个客户端名（大小写不敏感）。
pub fn parse_client(raw: &str) -> Option<&'static str> {
    let raw = raw.trim().to_ascii_lowercase();
    CLIENTS.iter().copied().find(|c| *c == raw)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClientRoute {
    /// 主模型，`{通道}/{模型}`。Claude Code 的 Sonnet 档和没单独配的档都走它。
    pub model: String,
    /// Claude Code 另外三档，空 = 跟主模型。别的客户端不看。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opus: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub haiku: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fable: Option<String>,
    /// Claude Code 按 1M 上下文算预算（别名后面带 `[1M]`）。只影响写进配置的档名。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub context1m: bool,
    /// 接入时写进这个客户端配置的模型名，新的在前。从这个口进来叫这些名字的，就是在要
    /// 「Nexus 给它配的那个模型」——一律按主模型走。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
}

/// 记住的旧名字最多这么多个：够覆盖「改了几次路由、客户端一直没重开」。
const MAX_ALIASES: usize = 8;

pub type ClientRoutes = BTreeMap<String, ClientRoute>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeRole {
    Opus,
    Sonnet,
    Haiku,
    Fable,
}

impl ClaudeRole {
    pub const ALL: [ClaudeRole; 4] = [
        ClaudeRole::Sonnet,
        ClaudeRole::Opus,
        ClaudeRole::Haiku,
        ClaudeRole::Fable,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ClaudeRole::Opus => "opus",
            ClaudeRole::Sonnet => "sonnet",
            ClaudeRole::Haiku => "haiku",
            ClaudeRole::Fable => "fable",
        }
    }
}

/// Claude Code 发来的是哪一档。顺序照官方分类器：fable 先认（没配时它降到 opus 那边由调用方兜），
/// 然后 haiku、opus、sonnet。
pub fn detect_role(model: &str) -> Option<ClaudeRole> {
    let m = model.to_ascii_lowercase();
    [
        ("fable", ClaudeRole::Fable),
        ("haiku", ClaudeRole::Haiku),
        ("opus", ClaudeRole::Opus),
        ("sonnet", ClaudeRole::Sonnet),
    ]
    .into_iter()
    .find(|(word, _)| m.contains(word))
    .map(|(_, role)| role)
}

impl ClientRoute {
    /// 这一档实际走的模型。Fable 没配时先跟 Opus（官方也是这么降），再跟主模型。
    pub fn role_model(&self, role: ClaudeRole) -> &str {
        fn pick(v: &Option<String>) -> Option<&str> {
            v.as_deref().map(str::trim).filter(|s| !s.is_empty())
        }
        match role {
            ClaudeRole::Sonnet => self.model.trim(),
            ClaudeRole::Opus => pick(&self.opus).unwrap_or(self.model.trim()),
            ClaudeRole::Haiku => pick(&self.haiku).unwrap_or(self.model.trim()),
            ClaudeRole::Fable => pick(&self.fable)
                .or_else(|| pick(&self.opus))
                .unwrap_or(self.model.trim()),
        }
    }

    /// 去掉空白，空的档收成 `None`，旧名字去重、限量。
    pub fn cleaned(mut self) -> Self {
        let clean = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
        self.model = self.model.trim().to_string();
        self.opus = clean(self.opus);
        self.haiku = clean(self.haiku);
        self.fable = clean(self.fable);
        let mut aliases: Vec<String> = Vec::new();
        for a in self.aliases {
            let a = a.trim().to_string();
            if !a.is_empty() && !aliases.iter().any(|x| x.eq_ignore_ascii_case(&a)) {
                aliases.push(a);
            }
        }
        aliases.truncate(MAX_ALIASES);
        self.aliases = aliases;
        self
    }

    /// 记下又写进配置的一个名字（排到最前）。
    pub fn remember(&mut self, written: &str) {
        let written = written.trim();
        if written.is_empty() {
            return;
        }
        self.aliases.retain(|a| !a.eq_ignore_ascii_case(written));
        self.aliases.insert(0, written.to_string());
        self.aliases.truncate(MAX_ALIASES);
    }
}

/// 这个客户端的这次请求实际要哪个模型（`{通道}/{模型}` 或原样）。规矩见模块注释。
pub fn resolve(
    route: &ClientRoute,
    client: &str,
    requested: &str,
    reg: &ChannelRegistry,
) -> String {
    let requested = requested.trim();
    if reg.iter().any(|ch| ch.strip_prefix(requested).is_some()) {
        return requested.to_string();
    }
    let main = route.model.trim();
    if main.is_empty() {
        return requested.to_string();
    }
    if client == "claude" {
        if let Some(role) = detect_role(requested) {
            return route.role_model(role).to_string();
        }
    }
    let bare = nexus_store::key_providers::bare_model(requested);
    if bare.is_empty() || route.aliases.iter().any(|a| a.eq_ignore_ascii_case(bare)) {
        return main.to_string();
    }
    let home = reg.resolve(main, Capability::Chat).channel;
    if home.gate.owns(Capability::Chat, bare) {
        return qualify(home.id, bare);
    }
    main.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{Channel, ChannelGate, OpenGate, CHATGPT, CURSOR};
    use crate::identity::DeviceIdentity;
    use crate::lane::{Credential, StaticLane};
    use crate::upstream::CursorUpstream;
    use std::sync::Arc;

    struct ListGate(Vec<&'static str>);

    impl ChannelGate for ListGate {
        fn ready(&self) -> bool {
            true
        }
        fn owns(&self, _cap: Capability, m: &str) -> bool {
            self.0.iter().any(|x| x.eq_ignore_ascii_case(m))
        }
        fn models(&self, _cap: Capability) -> Vec<String> {
            self.0.iter().map(|s| s.to_string()).collect()
        }
    }

    fn channel(
        id: &'static str,
        prefixes: &'static [&'static str],
        gate: Arc<dyn ChannelGate>,
    ) -> Channel {
        Channel {
            id,
            label: id,
            vendor: id,
            prefixes,
            lane: Arc::new(StaticLane::new(Credential {
                label: "l".into(),
                access_token: "t".into(),
                identity: DeviceIdentity::derived("t"),
            })),
            upstream: Arc::new(CursorUpstream::new(Default::default())),
            gate,
            passthrough: false,
        }
    }

    fn registry() -> ChannelRegistry {
        ChannelRegistry::new(channel(CURSOR, &["cursor/"], Arc::new(OpenGate))).with(channel(
            CHATGPT,
            &["chatgpt/", "codex/"],
            Arc::new(ListGate(vec!["gpt-5.4", "gpt-5.5"])),
        ))
    }

    fn claude_route() -> ClientRoute {
        ClientRoute {
            model: "cursor/claude-sonnet-5".into(),
            opus: Some("cursor/claude-opus-5".into()),
            haiku: Some("chatgpt/gpt-5.4".into()),
            ..ClientRoute::default()
        }
    }

    #[test]
    fn claude_aliases_map_to_their_roles() {
        let r = registry();
        let route = claude_route();
        assert_eq!(
            resolve(&route, "claude", "claude-sonnet-4-6", &r),
            "cursor/claude-sonnet-5"
        );
        assert_eq!(
            resolve(&route, "claude", "claude-opus-4-8[1m]", &r),
            "cursor/claude-opus-5"
        );
        assert_eq!(
            resolve(&route, "claude", "claude-haiku-4-5-20251001", &r),
            "chatgpt/gpt-5.4"
        );
        // Fable 没配：跟 Opus。
        assert_eq!(
            resolve(&route, "claude", "claude-fable-5", &r),
            "cursor/claude-opus-5"
        );
    }

    #[test]
    fn an_explicit_channel_prefix_is_respected() {
        let r = registry();
        assert_eq!(
            resolve(&claude_route(), "claude", "chatgpt/gpt-5.5", &r),
            "chatgpt/gpt-5.5"
        );
        assert_eq!(
            resolve(&claude_route(), "codex", "codex/gpt-5.5", &r),
            "codex/gpt-5.5"
        );
    }

    #[test]
    fn a_bare_name_stays_on_the_routes_channel_when_it_knows_it() {
        let r = registry();
        let route = ClientRoute {
            model: "chatgpt/gpt-5.4".into(),
            ..ClientRoute::default()
        };
        // Codex 里 /model 换了一个 ChatGPT 认识的：就跑它。
        assert_eq!(resolve(&route, "codex", "gpt-5.5", &r), "chatgpt/gpt-5.5");
        // 不认识的名字（配置里残留的旧名）：走主模型。
        assert_eq!(
            resolve(&route, "codex", "claude-opus-5", &r),
            "chatgpt/gpt-5.4"
        );
        assert_eq!(resolve(&route, "codex", "", &r), "chatgpt/gpt-5.4");
        // 路由切到 Cursor：同一个名字立刻换到 Cursor 上跑。
        let moved = ClientRoute {
            model: "cursor/gpt-5.4".into(),
            ..ClientRoute::default()
        };
        assert_eq!(resolve(&moved, "codex", "gpt-5.4", &r), "cursor/gpt-5.4");
    }

    #[test]
    fn names_written_into_the_config_follow_the_route_immediately() {
        let r = registry();
        let mut route = ClientRoute {
            model: "chatgpt/gpt-5.4".into(),
            ..ClientRoute::default()
        };
        route.remember("gpt-5.4");
        // 在应用里改成 Cursor 的 Opus；Codex 还没重开，发来的还是旧配置里的 gpt-5.4。
        route.model = "cursor/claude-opus-5".into();
        route.remember("claude-opus-5");
        assert_eq!(route.aliases, vec!["claude-opus-5", "gpt-5.4"]);
        assert_eq!(
            resolve(&route, "codex", "gpt-5.4", &r),
            "cursor/claude-opus-5"
        );
        assert_eq!(
            resolve(&route, "codex", "claude-opus-5", &r),
            "cursor/claude-opus-5"
        );
        // 客户端里另选了一个没写过的名字：在路由的通道上跑它。
        assert_eq!(resolve(&route, "codex", "gpt-5.5", &r), "cursor/gpt-5.5");
    }

    #[test]
    fn no_route_means_unscoped_behaviour() {
        let r = registry();
        assert_eq!(
            resolve(&ClientRoute::default(), "claude", "claude-sonnet-4-6", &r),
            "claude-sonnet-4-6"
        );
    }

    #[test]
    fn roles_are_detected_by_word() {
        assert_eq!(
            detect_role("claude-3-5-haiku-latest"),
            Some(ClaudeRole::Haiku)
        );
        assert_eq!(detect_role("Claude-Opus-4-8[1M]"), Some(ClaudeRole::Opus));
        assert_eq!(detect_role("gpt-5.4"), None);
        assert_eq!(parse_client("Claude"), Some("claude"));
        assert_eq!(parse_client("cursor"), None);
    }

    #[test]
    fn empty_role_overrides_are_dropped() {
        let route = ClientRoute {
            model: " cursor/a ".into(),
            opus: Some("  ".into()),
            haiku: Some(" chatgpt/b ".into()),
            ..ClientRoute::default()
        }
        .cleaned();
        assert_eq!(route.model, "cursor/a");
        assert_eq!(route.opus, None);
        assert_eq!(route.haiku.as_deref(), Some("chatgpt/b"));
        assert_eq!(route.role_model(ClaudeRole::Opus), "cursor/a");
    }
}
