//! ZCode 推理的公共事实：端点、模型目录、发给上游的头。
//!
//! 请求体的构造在 `nexus-gateway` 的 `zcode` 模块。这里只放「两边都要知道」的常量，
//! 以及那套必须和官方客户端逐字节一致的身份头 —— 少一个、顺序错一个都是可识别的差异。
//!
//! 事实来源：官方 ZCode 桌面客户端 3.12.3（`dev.zcode.app`）。

use crate::model::{ZcodePlan, ZcodeProvider};
use std::sync::OnceLock;

/// 官方客户端版本。头里报什么、系统提示词里写什么，都从这一个值来。
pub const APP_VERSION: &str = "3.12.3";
/// LLM 请求的 User-Agent 尾巴。官方客户端在 LLM 调用上（且只在 LLM 调用上）追加它。
pub const AI_SDK_SUFFIX: &str = " ai-sdk/anthropic/3.0.81";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";
pub const ZCODE_ORIGIN: &str = "https://zcode.z.ai";
/// 体验套餐的网关前缀。付费套餐直连服务商域名，不走这里。
pub const START_PLAN_BASE: &str = "https://zcode.z.ai/api/v1/zcode-plan";

pub const ROUTE_PREFIXES: &[&str] = &["zcode/", "glm/"];

/// 服务商的 Anthropic 端点根。
pub fn anthropic_base(provider: ZcodeProvider) -> &'static str {
    match provider {
        ZcodeProvider::Zai => "https://api.z.ai/api/anthropic",
        ZcodeProvider::Bigmodel => "https://open.bigmodel.cn/api/anthropic",
    }
}

/// biz API 根（换 API key、拉配额用）。
pub fn biz_host(provider: ZcodeProvider) -> &'static str {
    match provider {
        ZcodeProvider::Zai => "https://api.z.ai",
        ZcodeProvider::Bigmodel => "https://open.bigmodel.cn",
    }
}

/// 一次聊天请求发到哪。
///
/// 两档都讲 Anthropic Messages —— 官方客户端把 OpenAI 那条路废了（2026-08-28 起
/// `.../zcode-plan/chat/completions` 回 404），所以这里不给 OpenAI 分支。
pub fn chat_url(provider: ZcodeProvider, plan: ZcodePlan) -> String {
    match plan {
        ZcodePlan::StartPlan => format!("{START_PLAN_BASE}/anthropic/v1/messages"),
        ZcodePlan::CodingPlan => format!("{}/v1/messages", anthropic_base(provider)),
    }
}

/// 一个模型的静态事实。
///
/// `max_output` 是硬需求而不是参考值：客户端没给 `max_tokens` 时要拿它兜底。
/// 官方客户端就是这么做的，填一个通用小值（4096 之类）本身就是可识别的差异。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: &'static str,
    pub context: u32,
    pub max_output: u32,
    /// 会不会出 thinking 块。
    pub reasoning: bool,
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "glm-4.5-air",
        context: 131_072,
        max_output: 98_304,
        reasoning: false,
    },
    ModelSpec {
        id: "glm-4.6",
        context: 204_800,
        max_output: 131_072,
        reasoning: true,
    },
    ModelSpec {
        id: "glm-4.6v",
        context: 131_072,
        max_output: 32_768,
        reasoning: true,
    },
    ModelSpec {
        id: "glm-4.7",
        context: 204_800,
        max_output: 131_072,
        reasoning: true,
    },
    ModelSpec {
        id: "glm-5",
        context: 204_800,
        max_output: 65_536,
        reasoning: true,
    },
    ModelSpec {
        id: "glm-5-turbo",
        context: 204_800,
        max_output: 65_536,
        reasoning: true,
    },
    ModelSpec {
        id: "glm-5v-turbo",
        context: 204_800,
        max_output: 131_072,
        reasoning: true,
    },
    ModelSpec {
        id: "glm-5.1",
        context: 204_800,
        max_output: 65_536,
        reasoning: true,
    },
    ModelSpec {
        id: "glm-5.2",
        context: 1_048_576,
        max_output: 131_072,
        reasoning: true,
    },
    ModelSpec {
        id: "glm-5.3",
        context: 1_048_576,
        max_output: 131_072,
        reasoning: true,
    },
    ModelSpec {
        id: "glm-5.3-flash",
        context: 1_048_576,
        max_output: 131_072,
        reasoning: true,
    },
];

pub fn model_spec(model: &str) -> Option<&'static ModelSpec> {
    let base = upstream_model(model);
    MODELS.iter().find(|m| m.id.eq_ignore_ascii_case(&base))
}

/// 客户端没给 `max_tokens` 时用哪个数。认不出的模型给一个保守但不寒酸的值。
pub fn default_max_tokens(model: &str) -> u32 {
    model_spec(model).map(|m| m.max_output).unwrap_or(65_536)
}

pub fn split_route_prefix(model: &str) -> (&str, bool) {
    let t = model.trim();
    let lower = t.to_ascii_lowercase();
    for p in ROUTE_PREFIXES {
        if let Some(rest) = lower.strip_prefix(p) {
            let start = t.len() - rest.len();
            return (&t[start..], true);
        }
    }
    (t, false)
}

/// 这个名字归不归这条通道。
///
/// `glm-*` 在本地网关里不和谁撞车（Cursor 不出 GLM，Kiro 那边是 `kiro-claude-*`），
/// 所以裸名也认 —— 用户把 zcode 设成默认通道之后可以直接写 `glm-4.7`。
pub fn is_zcode_model(model: &str) -> bool {
    let (name, forced) = split_route_prefix(model);
    if forced {
        return true;
    }
    let base = name.trim().to_ascii_lowercase();
    MODELS.iter().any(|m| m.id.eq_ignore_ascii_case(&base)) || base.starts_with("glm-")
}

/// 发给上游的模型名。`zcode/glm-4.7` → `glm-4.7`。
pub fn upstream_model(model: &str) -> String {
    let (name, _) = split_route_prefix(model);
    name.trim().to_string()
}

/// 对外报的模型清单。
pub fn catalog() -> Vec<String> {
    MODELS.iter().map(|m| m.id.to_string()).collect()
}

// ---------------------------------------------------------------------------
// GLM-5.3 的思考档位
// ---------------------------------------------------------------------------

/// GLM-5.3 系列的思考预算。
///
/// 这一族**完全无视** OpenAI 的 `reasoning_effort`：唯一能改思考深度的通道是
/// `output_config.effort`，而且必须配一个匹配的 `thinking.budget_tokens`。
/// 低于 1024 上游会塌缩成几乎不思考。
pub const GLM53_BUDGET_LOW: u32 = 8_000;
pub const GLM53_BUDGET_HIGH: u32 = 16_000;
pub const GLM53_BUDGET_MAX: u32 = 32_000;
pub const GLM53_MIN_BUDGET: u32 = 1_024;

/// 只有 5.3 这一族认 effort —— 5 / 5.1 / 5.2 都不认，别顺手放进来。
pub fn is_glm53_family(model: &str) -> bool {
    let base = upstream_model(model).to_ascii_lowercase();
    base == "glm-5.3" || base == "glm-5.3-flash"
}

/// `(effort, budget_tokens)`。默认档是 `max` 而不是 OpenAI 的 `medium`，
/// 认不出的档位往**高**里靠（`medium` → `high`）。
pub fn glm53_effort(requested: Option<&str>) -> (&'static str, u32) {
    match requested.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("low") | Some("minimal") => ("low", GLM53_BUDGET_LOW),
        Some("medium") | Some("high") => ("high", GLM53_BUDGET_HIGH),
        _ => ("max", GLM53_BUDGET_MAX),
    }
}

// ---------------------------------------------------------------------------
// 身份
// ---------------------------------------------------------------------------

/// 一台机器的身份。进程内只算一次。
#[derive(Debug, Clone)]
pub struct Identity {
    pub app_version: String,
    pub source_title: String,
    pub referer_origin: String,
    /// node 口径的平台名（`darwin` / `win32` / `linux`）。
    pub platform: String,
    /// node 口径的架构名（`arm64` / `x64`）。
    pub arch: String,
    /// 内核版本（`os.release()`）。取不到就不发这个头。
    pub os_release: Option<String>,
    pub language: String,
    pub timezone: String,
    pub release_channel: String,
}

impl Identity {
    /// `X-Platform` 的值。平台和架构缺一个就不发这个头（官方客户端的行为）。
    pub fn platform_tag(&self) -> Option<String> {
        (!self.platform.is_empty() && !self.arch.is_empty())
            .then(|| format!("{}-{}", self.platform, self.arch))
    }

    pub fn os_category(&self) -> &'static str {
        match self.platform.as_str() {
            "darwin" => "macos",
            "win32" => "windows",
            _ => "linux",
        }
    }
}

fn env_override(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && printable_ascii(v))
}

/// 每个头的值都要过这一关。不可打印的字符会让上游直接拒掉整个请求，
/// 官方客户端的做法是「这个值不合格就不发这个头」而不是发一个坏的。
fn printable_ascii(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| (0x20..=0x7e).contains(&b))
}

fn node_platform() -> String {
    match std::env::consts::OS {
        "macos" => "darwin".into(),
        "windows" => "win32".into(),
        other => other.into(),
    }
}

fn node_arch() -> String {
    match std::env::consts::ARCH {
        "aarch64" => "arm64".into(),
        "x86_64" => "x64".into(),
        other => other.into(),
    }
}

#[cfg(unix)]
fn os_release() -> Option<String> {
    // `os.release()` 在 macOS 上是 Darwin 内核版本（`25.6.0`），不是 `15.x` 那个产品版本。
    let mut buf: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut buf) } != 0 {
        return None;
    }
    let bytes: Vec<u8> = buf
        .release
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8(bytes).ok().filter(|s| printable_ascii(s))
}

#[cfg(not(unix))]
fn os_release() -> Option<String> {
    None
}

fn detect_identity() -> Identity {
    Identity {
        app_version: env_override("ZCODE_IDENTITY_APP_VERSION")
            .unwrap_or_else(|| APP_VERSION.to_string()),
        source_title: env_override("ZCODE_IDENTITY_SOURCE_TITLE").unwrap_or_else(|| "cli".into()),
        referer_origin: env_override("ZCODE_IDENTITY_REFERER_ORIGIN")
            .unwrap_or_else(|| ZCODE_ORIGIN.to_string()),
        platform: env_override("ZCODE_IDENTITY_PLATFORM").unwrap_or_else(node_platform),
        arch: env_override("ZCODE_IDENTITY_ARCH").unwrap_or_else(node_arch),
        os_release: env_override("ZCODE_IDENTITY_RELEASE").or_else(os_release),
        // 取不到就报 `unknown`——官方客户端在 Intl 不可用时就是这么发的。
        language: env_override("ZCODE_IDENTITY_CLIENT_LANGUAGE").unwrap_or_else(|| "en-US".into()),
        timezone: env_override("ZCODE_IDENTITY_CLIENT_TIMEZONE").unwrap_or_else(|| {
            iana_time_zone::get_timezone()
                .ok()
                .filter(|t| printable_ascii(t))
                .unwrap_or_else(|| "unknown".into())
        }),
        release_channel: env_override("ZCODE_IDENTITY_RELEASE_CHANNEL")
            .unwrap_or_else(|| "production".into()),
    }
}

pub fn identity() -> &'static Identity {
    static ID: OnceLock<Identity> = OnceLock::new();
    ID.get_or_init(detect_identity)
}

/// LLM 请求的身份头，**按顺序**。
///
/// 顺序是指纹的一部分，所以给的是有序数组而不是 map。和控制面那一组
/// （配额、端点路由）有两处刻意的不同：这里有 `X-ZCode-Agent` 且它排在第 8 位，
/// 而且这里**不发** `X-Device-Mid`。把两组混用是很显眼的破绽。
pub fn llm_identity_headers() -> Vec<(String, String)> {
    let id = identity();
    let mut out: Vec<(String, String)> = Vec::with_capacity(11);
    out.push(("HTTP-Referer".into(), id.referer_origin.clone()));
    out.push((
        "User-Agent".into(),
        format!("ZCode/{}{AI_SDK_SUFFIX}", id.app_version),
    ));
    if printable_ascii(&id.app_version) {
        out.push(("X-ZCode-App-Version".into(), id.app_version.clone()));
    }
    out.push(("X-Title".into(), format!("Z Code@{}", id.source_title)));
    out.push(("X-Release-Channel".into(), id.release_channel.clone()));
    out.push(("X-Client-Language".into(), id.language.clone()));
    out.push(("X-Client-Timezone".into(), id.timezone.clone()));
    out.push(("X-ZCode-Agent".into(), "glm".into()));
    if let Some(tag) = id.platform_tag() {
        out.push(("X-Platform".into(), tag));
    }
    out.push(("X-Os-Category".into(), id.os_category().into()));
    if let Some(rel) = &id.os_release {
        out.push(("X-Os-Version".into(), rel.clone()));
    }
    out
}

/// 控制面（配额 / 端点路由 / 领取套餐）的身份头。
///
/// 和 LLM 那一组的差别是刻意的：没有 `X-ZCode-Agent`，`X-Release-Channel` 挪到
/// `X-Platform` 之后，而且**要发** `X-Device-Mid` —— 计费网关认这个值。
pub fn control_identity_headers(device_mid: &str) -> Vec<(String, String)> {
    let id = identity();
    let mut out: Vec<(String, String)> = Vec::with_capacity(11);
    out.push(("HTTP-Referer".into(), id.referer_origin.clone()));
    out.push(("User-Agent".into(), format!("ZCode/{}", id.app_version)));
    if printable_ascii(&id.app_version) {
        out.push(("X-ZCode-App-Version".into(), id.app_version.clone()));
    }
    out.push(("X-Title".into(), format!("Z Code@{}", id.source_title)));
    out.push(("X-Client-Language".into(), id.language.clone()));
    out.push(("X-Client-Timezone".into(), id.timezone.clone()));
    if let Some(tag) = id.platform_tag() {
        out.push(("X-Platform".into(), tag));
    }
    out.push(("X-Release-Channel".into(), id.release_channel.clone()));
    out.push(("X-Os-Category".into(), id.os_category().into()));
    if let Some(rel) = &id.os_release {
        out.push(("X-Os-Version".into(), rel.clone()));
    }
    if printable_ascii(device_mid) {
        out.push(("X-Device-Mid".into(), device_mid.to_string()));
    }
    out
}

/// 一次请求的 trace 头，**按顺序**。
///
/// `x-zcode-session-type` 永远有；`subagent_agent_` 开头的会话报 `subagent`。
/// 内部前缀在发出时要剥掉 —— 头里带的是 `worker_1` 而不是 `subagent_agent_worker_1`。
pub fn trace_headers(request_id: &str, trace_id: &str, session_id: &str) -> Vec<(String, String)> {
    let session_type = if session_id.starts_with("subagent_agent_") {
        "subagent"
    } else {
        "main"
    };
    vec![
        ("x-request-id".into(), request_id.to_string()),
        ("x-zcode-session-type".into(), session_type.into()),
        ("x-zcode-trace-id".into(), trace_id.to_string()),
        ("x-session-id".into(), strip_session_prefix(session_id)),
    ]
}

fn strip_session_prefix(session_id: &str) -> String {
    for p in ["subagent_agent_", "sess_", "query_"] {
        if let Some(rest) = session_id.strip_prefix(p) {
            return rest.to_string();
        }
    }
    session_id.to_string()
}

/// 认证头。
///
/// coding-plan 上 `x-api-key` 和 `Authorization: Bearer` **同值双发**，这不是冗余：
/// 真客户端一个来自 SDK 配置、一个来自自定义头合并，只发一个就露馅了。
pub fn auth_headers(plan: ZcodePlan, credential: &str) -> Vec<(String, String)> {
    match plan {
        ZcodePlan::StartPlan => vec![
            ("authorization".into(), format!("Bearer {credential}")),
            ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
        ],
        ZcodePlan::CodingPlan => vec![
            ("x-api-key".into(), credential.to_string()),
            ("authorization".into(), format!("Bearer {credential}")),
            ("anthropic-version".into(), ANTHROPIC_VERSION.into()),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glm_names_route_here_but_claude_does_not() {
        assert!(is_zcode_model("glm-4.7"));
        assert!(is_zcode_model("zcode/glm-5.3"));
        assert!(is_zcode_model("GLM-4.6"));
        assert!(!is_zcode_model("claude-opus-5"));
        assert!(!is_zcode_model("kiro-claude-sonnet-4.5"));
        assert_eq!(upstream_model("zcode/glm-4.7"), "glm-4.7");
        assert_eq!(upstream_model("glm-4.7"), "glm-4.7");
    }

    #[test]
    fn a_multibyte_model_name_does_not_panic() {
        // 前缀比对必须按字节走：`"中文模型"[..6]` 会切在字符中间。
        assert!(!is_zcode_model("中文模型"));
        assert_eq!(upstream_model("中文模型"), "中文模型");
    }

    #[test]
    fn missing_max_tokens_falls_back_to_the_catalog_not_a_round_number() {
        assert_eq!(default_max_tokens("glm-4.6"), 131_072);
        assert_eq!(default_max_tokens("zcode/glm-5.2"), 131_072);
        assert_ne!(default_max_tokens("glm-4.6"), 4_096);
    }

    #[test]
    fn only_the_53_family_takes_an_effort() {
        assert!(is_glm53_family("glm-5.3"));
        assert!(is_glm53_family("zcode/glm-5.3-flash"));
        assert!(!is_glm53_family("glm-5.2"), "5.2 不认 effort");
        assert!(!is_glm53_family("glm-5"));
        // 默认往 max，medium 往上取到 high。
        assert_eq!(glm53_effort(None), ("max", GLM53_BUDGET_MAX));
        assert_eq!(glm53_effort(Some("medium")), ("high", GLM53_BUDGET_HIGH));
        assert_eq!(glm53_effort(Some("low")), ("low", GLM53_BUDGET_LOW));
    }

    #[test]
    fn coding_plan_sends_the_credential_twice() {
        let h = auth_headers(ZcodePlan::CodingPlan, "id.secret");
        let names: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["x-api-key", "authorization", "anthropic-version"]);
        assert_eq!(h[0].1, "id.secret");
        assert_eq!(h[1].1, "Bearer id.secret");

        // 体验套餐只发 Bearer，而且发的是 JWT。
        let s = auth_headers(ZcodePlan::StartPlan, "jwt");
        let names: Vec<&str> = s.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(names, ["authorization", "anthropic-version"]);
    }

    #[test]
    fn llm_headers_keep_the_documented_order() {
        let h = llm_identity_headers();
        let names: Vec<&str> = h.iter().map(|(k, _)| k.as_str()).collect();
        let want = [
            "HTTP-Referer",
            "User-Agent",
            "X-ZCode-App-Version",
            "X-Title",
            "X-Release-Channel",
            "X-Client-Language",
            "X-Client-Timezone",
            "X-ZCode-Agent",
        ];
        assert_eq!(&names[..want.len()], &want);
        // 控制面那组没有 X-ZCode-Agent，但有 X-Device-Mid。
        let c = control_identity_headers("mid-1");
        let cnames: Vec<&str> = c.iter().map(|(k, _)| k.as_str()).collect();
        assert!(!cnames.contains(&"X-ZCode-Agent"));
        assert!(cnames.contains(&"X-Device-Mid"));
        assert!(!names.contains(&"X-Device-Mid"));
    }

    #[test]
    fn subagent_sessions_are_labelled_and_the_prefix_is_stripped() {
        let h = trace_headers("r", "t", "subagent_agent_worker_1");
        assert_eq!(h[1], ("x-zcode-session-type".into(), "subagent".into()));
        assert_eq!(h[3], ("x-session-id".into(), "worker_1".into()));
        let m = trace_headers("r", "t", "plain");
        assert_eq!(m[1].1, "main");
    }
}
