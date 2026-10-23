//! Google 反重力 (Antigravity) 核心协议、模型映射与数据结构。
//!
//! 包含：
//! 1. Antigravity PA 内部端点与 RPC 路径定义；
//! 2. Gemini 与 Claude 模型家族映射；
//! 3. 确定性 Session ID 派生（解决开源反代破坏 Prompt Caching / KV-Cache 的痛点）；
//! 4. 思维链（Thinking / Reasoning）标签提取器；
//! 5. 消息、工具调用与流式 SSE 协议载荷建模。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// 官方 IDE 语言服务只打 Daily。Sandbox 在部分地区会回终止性 400
///（`User location is not supported`），而 400 不能换端点重试，所以不能排第一。
pub const DAILY_ENDPOINT: &str = "https://daily-cloudcode-pa.googleapis.com";
pub const SANDBOX_ENDPOINT: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";
pub const PROD_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com";
pub const DEFAULT_ENDPOINT: &str = DAILY_ENDPOINT;
pub const CLOUDCODE_ENDPOINT: &str = DAILY_ENDPOINT;
pub const FALLBACK_ENDPOINT: &str = PROD_ENDPOINT;
/// 旧常量。生成请求不走这条，官方客户端用的是上面三条 Cloud Code 源。
pub const ANTIGRAVITY_ENDPOINT: &str = "https://antigravity-pa.googleapis.com";

/// 生成 / 计数的回退顺序：Daily → Sandbox → Prod。
pub const CONTENT_ORIGINS: [&str; 3] = [DAILY_ENDPOINT, SANDBOX_ENDPOINT, PROD_ENDPOINT];

pub const RPC_LOAD_CODE_ASSIST: &str = "v1internal:loadCodeAssist";
pub const RPC_FETCH_AVAILABLE_MODELS: &str = "v1internal:fetchAvailableModels";
pub const RPC_RETRIEVE_USER_QUOTA: &str = "v1internal:retrieveUserQuotaSummary";
pub const RPC_STREAM_GENERATE_CONTENT: &str = "v1internal:streamGenerateContent?alt=sse";
pub const RPC_GENERATE_CONTENT: &str = "v1internal:generateContent";
pub const RPC_COUNT_TOKENS: &str = "v1internal:countTokens";

/// 官方 Hub 出站身份。低于 3.0 的 `antigravity/1.x` 会被分段模型（`*-high/medium/low`）直接 404。
pub const HUB_VERSION: &str = "2.17.0";
pub const HUB_CHANGELIST: &str = "986210228";
pub const CLIENT_IDE_TYPE: &str = "ANTIGRAVITY";
pub const CLIENT_IDE_NAME: &str = "antigravity";
pub const ENVELOPE_USER_AGENT: &str = "antigravity";
pub const CLAUDE_BETA: &str = "claude-code-20250219";

/// 与官方 Hub 出站 User-Agent 同形：`antigravity/hub/{ver} (aidev_client; os_type=…; arch=…; cl=…)`。
pub fn upstream_user_agent() -> String {
    let os_type = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        "linux" => "linux",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    };
    format!(
        "antigravity/hub/{HUB_VERSION} (aidev_client; os_type={os_type}; arch={arch}; cl={HUB_CHANGELIST})"
    )
}

/// 只有超时、404 和 5xx 才换下一个源。400（含地区不支持）和 429（配额）换源没有意义。
pub fn should_try_next_endpoint(status: u16) -> bool {
    status == 408 || status == 404 || (500..600).contains(&status)
}

pub fn v1_url(origin: &str, rpc: &str) -> String {
    format!("{origin}/{rpc}")
}

/// 构造带有 macOS 系统代理自适应的 HTTP 客户端。
pub fn http_client() -> reqwest::Client {
    let mut builder = reqwest::Client::builder().timeout(std::time::Duration::from_secs(25));
    if let Some(proxy_url) = detect_system_proxy() {
        if let Ok(proxy) = reqwest::Proxy::all(&proxy_url) {
            builder = builder.proxy(proxy);
        }
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

/// 自动探测系统代理（优先环境变量，其次 macOS scutil）。
pub fn detect_system_proxy() -> Option<String> {
    for var in &[
        "https_proxy",
        "HTTPS_PROXY",
        "all_proxy",
        "ALL_PROXY",
        "http_proxy",
        "HTTP_PROXY",
    ] {
        if let Ok(val) = std::env::var(var) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Ok(out) = std::process::Command::new("/usr/sbin/scutil")
            .arg("--proxy")
            .output()
        {
            if out.status.success() {
                let s = String::from_utf8_lossy(&out.stdout);
                let is_https = s.contains("HTTPSEnable : 1") || s.contains("HTTPEnable : 1");
                let is_socks = s.contains("SOCKSEnable : 1");
                let extract = |k: &str| -> Option<String> {
                    for line in s.lines() {
                        let t = line.trim();
                        if t.starts_with(k) {
                            if let Some((_, v)) = t.split_once(':') {
                                let val = v.trim();
                                if !val.is_empty() {
                                    return Some(val.to_string());
                                }
                            }
                        }
                    }
                    None
                };
                if is_https {
                    let host = extract("HTTPSProxy").or_else(|| extract("HTTPProxy"));
                    let port = extract("HTTPSPort").or_else(|| extract("HTTPPort"));
                    if let (Some(h), Some(p)) = (host, port) {
                        return Some(format!("http://{h}:{p}"));
                    }
                } else if is_socks {
                    let host = extract("SOCKSProxy");
                    let port = extract("SOCKSPort");
                    if let (Some(h), Some(p)) = (host, port) {
                        return Some(format!("socks5://{h}:{p}"));
                    }
                }
            }
        }
    }

    None
}

/// 对外目录。精选活跃与前沿旗舰模型：
/// - 次世代前沿旗舰: gemini-4-argon, gemini-4-argon-thinking
/// - 深度思考与推理: claude-opus-4-6-thinking, claude-sonnet-4-6-thinking
/// - 高频编程主力: gemini-3.8-flash-high, claude-sonnet-4-6
/// - 经济高吞吐: gemini-3.8-flash-medium, gemini-3.8-flash-low, gemini-3.1-flash-lite
/// - 生图与开源: gemini-3.1-flash-image, gpt-oss-120b-medium
pub const ANTIGRAVITY_MODELS: &[&str] = &[
    "gemini-4-argon",
    "gemini-4-argon-thinking",
    "claude-opus-4-6-thinking",
    "claude-sonnet-4-6-thinking",
    "claude-sonnet-4-6",
    "gemini-3.8-flash-high",
    "gemini-3.8-flash-medium",
    "gemini-3.8-flash-low",
    "gemini-3.8-flash-tiered",
    "gemini-3.1-flash-image",
    "gemini-3.1-flash-lite",
    "gpt-oss-120b-medium",
];

pub const ROUTE_PREFIXES: &[&str] = &["antigravity/", "agy/", "google/"];

/// 这个模型名归 Antigravity 通道吗。
pub fn owns_model(model: &str) -> bool {
    let stripped = strip_route_prefix(model).to_ascii_lowercase();
    ANTIGRAVITY_MODELS
        .iter()
        .any(|m| m.eq_ignore_ascii_case(&stripped))
        || stripped.starts_with("gemini-")
        || stripped.starts_with("claude-")
        || stripped.starts_with("gpt-oss")
        || stripped.starts_with("imagen")
        || stripped.starts_with("image-")
}

/// 客户端模型名 → Cloud Code 上游 id。
///
/// 公开名和上游真 id 不是同一套：`gemini-3.1-pro-high` 实际要发 `gemini-pro-agent`，
/// 旧的 2.5 / 3-flash 要收到当前健康档。认不出的 id 原样送上去，让上游自己拒绝。
pub fn upstream_model(model: &str) -> String {
    let stripped = strip_route_prefix(model).to_string();
    let canonical = canonicalize_claude_id(&stripped);
    let id = strip_date_suffix(&canonical);
    if let Some(mapped) = static_upstream(id) {
        return mapped.to_string();
    }
    if let Some(target) = legacy_claude_family(id) {
        return target.to_string();
    }
    if let Some(target) = bare_flash_default(id) {
        return target.to_string();
    }
    id.to_string()
}

fn strip_route_prefix(model: &str) -> &str {
    route_body(model)
}

fn route_body(model: &str) -> &str {
    let trimmed = model.trim();
    let lower = trimmed.to_ascii_lowercase();
    for prefix in ["antigravity/", "agy/", "google/", "models/", "anthropic/"] {
        if lower.starts_with(prefix) {
            return &trimmed[prefix.len()..];
        }
    }
    trimmed
}

/// `claude-opus-4.6` / `claude-open-4.6` → `claude-opus-4-6`。只改 Claude id 里的短版本号。
fn canonicalize_claude_id(input: &str) -> String {
    let mut id = input.trim().to_ascii_lowercase();
    if id.contains("claude-open-") {
        id = id.replace("claude-open-", "claude-opus-");
    }
    if !id.contains("claude") {
        return input.trim().to_ascii_lowercase();
    }
    let bytes = id.as_bytes();
    let mut out = String::with_capacity(id.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            if i < bytes.len() && bytes[i] == b'.' {
                let dot = i;
                let mut j = i + 1;
                while j < bytes.len() && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j > dot + 1 && j - (dot + 1) <= 2 {
                    out.push_str(&id[start..dot]);
                    out.push('-');
                    out.push_str(&id[dot + 1..j]);
                    i = j;
                    continue;
                }
            }
            out.push_str(&id[start..i]);
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

fn strip_date_suffix(id: &str) -> &str {
    if let Some(idx) = id.rfind('-') {
        let tail = &id[idx + 1..];
        if tail.len() == 8 && tail.chars().all(|c| c.is_ascii_digit()) {
            return &id[..idx];
        }
    }
    id
}

fn static_upstream(id: &str) -> Option<&'static str> {
    Some(match id {
        "claude-sonnet-4-6"
        | "claude-sonnet-4-6-thinking"
        | "claude-sonnet-4-5"
        | "claude-sonnet-4-5-thinking"
        | "claude-haiku-4-5"
        | "claude-haiku-4"
        | "claude-3-5-sonnet"
        | "claude-3-5-sonnet-latest"
        | "claude-3-7-sonnet"
        | "claude-3-7-sonnet-latest"
        | "claude-3-5-haiku" => "claude-sonnet-4-6",
        "claude-opus-4-6"
        | "claude-opus-4-6-thinking"
        | "claude-opus-4-5-thinking"
        | "claude-opus-4"
        | "claude-opus-4-5" => "claude-opus-4-6-thinking",

        "gpt-oss-120b-medium" => "gpt-oss-120b-medium",
        "gpt-4o" | "gpt-4o-mini" | "gpt-4-turbo" | "gpt-4" | "gpt-3.5-turbo" | "gpt-3.5" => {
            "gemini-3.8-flash-high"
        }

        "gemini-4-argon"
        | "gemini-4-argon-thinking"
        | "gemini-4-argon-high"
        | "gemini-4-argon-preview" => "gemini-4-argon",

        "gemini-3.8-flash-tiered" => "gemini-3.8-flash-tiered",
        "gemini-3.8-flash-high" => "gemini-3.8-flash-high",
        "gemini-3.8-flash-medium" => "gemini-3.8-flash-medium",
        "gemini-3.8-flash-low" => "gemini-3.8-flash-low",

        // 3.8 以下所有历史 Flash/Pro 档位已由 Google 官方下线，自动平滑收敛到 3.8-flash-high，防止 404/503
        "gemini-3.7-flash-tiered"
        | "gemini-3.7-flash-high"
        | "gemini-3.7-flash-medium"
        | "gemini-3.7-flash-low"
        | "gemini-3.6-flash-tiered"
        | "gemini-3.6-flash-high"
        | "gemini-3.6-flash-medium"
        | "gemini-3.6-flash-low"
        | "gemini-3.5-flash"
        | "gemini-3.5-flash-low"
        | "gemini-3.5-flash-extra-low"
        | "gemini-3-flash"
        | "gemini-3-flash-agent"
        | "gemini-2.5-flash"
        | "gemini-2.5-flash-thinking"
        | "gemini-2.0-flash"
        | "gemini-1.5-flash" => "gemini-3.8-flash-high",

        "gemini-3.1-pro-high"
        | "gemini-3.1-pro"
        | "gemini-3.1-pro-low"
        | "gemini-3.1-pro-preview"
        | "gemini-3-pro-high"
        | "gemini-3-pro"
        | "gemini-3-pro-low"
        | "gemini-3-pro-preview"
        | "gemini-2.5-pro"
        | "gemini-2.0-pro"
        | "gemini-1.5-pro"
        | "gemini-pro-agent" => "gemini-3.8-flash-high",

        "gemini-3.1-flash-lite" | "gemini-2.5-flash-lite" | "gemini-3.5-flash-lite" => {
            "gemini-3.1-flash-lite"
        }
        "gemini-3.1-flash-image" | "gemini-3-pro-image" => "gemini-3.1-flash-image",
        _ => return None,
    })
}

/// Claude 主版本低于 4.6 时，按家族收到 4.6。
fn legacy_claude_family(id: &str) -> Option<&'static str> {
    if !id.contains("claude") {
        return None;
    }
    let mut version: Option<f32> = None;
    let tokens: Vec<&str> = id.split('-').collect();
    for window in tokens.windows(2) {
        if let (Ok(major), Ok(minor)) = (window[0].parse::<u32>(), window[1].parse::<u32>()) {
            if minor < 100 {
                version = Some(major as f32 + (minor as f32) / 10.0);
            }
        }
    }
    if !version.is_some_and(|ver| ver < 4.6) {
        return None;
    }
    let _thinking = id.contains("thinking");
    if id.contains("opus") {
        return Some("claude-opus-4-6-thinking");
    }
    if id.contains("sonnet") || id.contains("haiku") {
        return Some("claude-sonnet-4-6");
    }
    None
}

/// 没写档位的 Flash 裸名默认走 high。已经带 `-high/-low/-tiered` 的不会进这里。
fn bare_flash_default(id: &str) -> Option<&'static str> {
    match id {
        "gemini-3.8-flash" => Some("gemini-3.8-flash-high"),
        "gemini-3.7-flash" => Some("gemini-3.7-flash-high"),
        "gemini-3.6-flash" => Some("gemini-3.6-flash-high"),
        _ => None,
    }
}

/// 分段 Flash 只声明 `includeThoughts`，档位已经在模型 id 里。
/// Claude 思考模型给官方默认预算 1024，并保证 `maxOutputTokens` 严格大于它。
/// Claude 的规则是 `max_tokens > thinking.budget_tokens`，相等也会 400。
pub fn thinking_config_for(model: &str) -> Option<ThinkingConfig> {
    let lower = model.to_ascii_lowercase();
    let flash_tier = lower.contains("flash")
        && (lower.contains("-high")
            || lower.contains("-medium")
            || lower.contains("-low")
            || lower.contains("tiered"));
    let claude_thinking = lower.contains("claude")
        && (lower.contains("thinking")
            || lower.contains("sonnet")
            || lower.contains("opus"));
    if flash_tier || claude_thinking || lower.contains("-thinking") {
        return Some(ThinkingConfig {
            thinking_budget: if claude_thinking { Some(1024) } else { None },
            include_thoughts: Some(true),
        });
    }
    None
}

pub fn output_ceiling(model: &str) -> i32 {
    let lower = model.to_ascii_lowercase();
    if lower.contains("claude") {
        64_000
    } else if lower.contains("pro") {
        65_535
    } else {
        65_536
    }
}

/// 输出上限和思考预算一起定。Claude 思考模型没给上限时用官方天花板；
/// 给了但不够盖住预算时，抬到预算加 1024，而不是把思考关掉。
pub fn align_generation(
    model: &str,
    requested_max: Option<i32>,
) -> (Option<i32>, Option<ThinkingConfig>) {
    let thinking = thinking_config_for(model);
    let ceiling = output_ceiling(model);
    let budget = thinking.as_ref().and_then(|cfg| cfg.thinking_budget);
    let max = match (requested_max, budget) {
        (None, Some(_)) => Some(ceiling),
        (Some(max), Some(budget)) if max > budget => Some(max.min(ceiling)),
        (_, Some(budget)) => Some((budget + 1024).min(ceiling)),
        (Some(max), None) => Some(max.min(ceiling)),
        (None, None) => None,
    };
    (max, thinking)
}

pub fn cap_max_output(model: &str, requested: Option<i32>) -> Option<i32> {
    align_generation(model, requested).0
}

/// `agent/{会话}/{毫秒}/{轨迹}/{步}`。会话段稳定，轨迹段每次请求唯一。
pub fn official_request_id(session_id: &str, step: u64) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let mut hasher = Sha256::new();
    hasher.update(session_id.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    let conversation = &hex[..16];
    let trajectory = &uuid::Uuid::new_v4().simple().to_string()[..8];
    format!("agent/{conversation}/{ts}/{trajectory}/{step}")
}

/// 派生一致性 Session ID，确保相同对话命中有力缓存（KV Cache），而非随机破坏。
pub fn derive_session_id(first_turn_text: &str, system_prompt: Option<&str>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"nexus-antigravity-session-v1:");
    if let Some(sys) = system_prompt {
        hasher.update(sys.as_bytes());
        hasher.update(b"|");
    }
    hasher.update(first_turn_text.as_bytes());
    let hex_hash = format!("{:x}", hasher.finalize());
    format!("session-{}", &hex_hash[..16])
}

// ─────────────────────────────────────────────────────────────────────────────
// 内部请求结构建模
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thought: Option<bool>,
    /// 上一轮思考的上游签名。占位哈希不能填这里，上游验不过会整轮 400。
    #[serde(skip_serializing_if = "Option::is_none", alias = "thoughtSignature")]
    pub thought_signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_call: Option<FunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function_response: Option<FunctionResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_data: Option<InlineData>,
}

impl ContentPart {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            thought: None,
            thought_signature: None,
            function_call: None,
            function_response: None,
            inline_data: None,
        }
    }

    pub fn thought(text: impl Into<String>) -> Self {
        Self {
            text: Some(text.into()),
            thought: Some(true),
            thought_signature: None,
            function_call: None,
            function_response: None,
            inline_data: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InlineData {
    pub mime_type: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCall {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub name: String,
    pub response: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>, // "user" | "model"
    pub parts: Vec<ContentPart>,
}

impl ContentMessage {
    pub fn user(parts: Vec<ContentPart>) -> Self {
        Self {
            role: Some("user".to_string()),
            parts,
        }
    }

    pub fn model(parts: Vec<ContentPart>) -> Self {
        Self {
            role: Some("model".to_string()),
            parts,
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Some("user".to_string()),
            parts: vec![ContentPart::text(text)],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_thoughts: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_sequences: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_config: Option<ThinkingConfig>,
}

/// 客户端明确要求工具策略时才带。默认 Auto 不写这个字段，和官方报文一致。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionCallingConfig {
    pub mode: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_function_names: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolConfig {
    pub function_calling_config: FunctionCallingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FunctionDeclaration {
    pub name: String,
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolDeclaration {
    pub function_declarations: Vec<FunctionDeclaration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentRequest {
    pub contents: Vec<ContentMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<ContentMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_config: Option<GenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDeclaration>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_config: Option<ToolConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

/// 请求以 model 轮结尾时，上游会 400。补一回合中性用户文本，不改前面的前缀。
pub const USER_TURN_FALLBACK: &str = "ok go on";

/// 对话中途插入的系统消息不能改写顶层 systemInstruction，否则服务端提示缓存整段失效。
pub fn wrap_system_reminder(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with("<system-reminder>") && trimmed.ends_with("</system-reminder>") {
        return trimmed.to_string();
    }
    format!(
        "<system-reminder>\nBefore the user's request for this turn, the system provides the following reminder for your awareness. Please note that this is from prior system messages, not spoken by the user:\n{trimmed}\n</system-reminder>"
    )
}

impl GenerateContentRequest {
    /// 带工具声明或历史里已经有调用 / 回执时，信封标成 `agent`。
    pub fn request_type(&self) -> Option<&'static str> {
        let has_tools = self.tools.as_ref().is_some_and(|t| !t.is_empty());
        let has_interactions = self.contents.iter().any(|message| {
            message
                .parts
                .iter()
                .any(|part| part.function_call.is_some() || part.function_response.is_some())
        });
        if has_tools || has_interactions {
            Some("agent")
        } else {
            None
        }
    }

    /// 相邻同角色合并、空 parts 补齐、保证最后一轮是 user。
    pub fn finalize(&mut self) {
        merge_same_role(&mut self.contents);
        for message in &mut self.contents {
            if message.parts.is_empty() {
                message.parts.push(ContentPart::text(
                    if message.role.as_deref() == Some("model") {
                        "..."
                    } else {
                        USER_TURN_FALLBACK
                    },
                ));
            }
        }
        if self.contents.is_empty() {
            self.contents
                .push(ContentMessage::user(vec![ContentPart::text(
                    USER_TURN_FALLBACK,
                )]));
            return;
        }
        let needs_user = self.contents.last().is_some_and(|message| {
            message.role.as_deref() == Some("model") || !has_substantive_parts(&message.parts)
        });
        if needs_user {
            if self
                .contents
                .last()
                .is_some_and(|message| message.role.as_deref() != Some("model"))
                && self
                    .contents
                    .last()
                    .is_some_and(|message| !has_substantive_parts(&message.parts))
            {
                if let Some(last) = self.contents.last_mut() {
                    last.parts = vec![ContentPart::text(USER_TURN_FALLBACK)];
                }
            } else if self
                .contents
                .last()
                .is_some_and(|message| message.role.as_deref() == Some("model"))
            {
                self.contents
                    .push(ContentMessage::user(vec![ContentPart::text(
                        USER_TURN_FALLBACK,
                    )]));
            }
        }
    }
}

fn merge_same_role(contents: &mut Vec<ContentMessage>) {
    if contents.len() < 2 {
        return;
    }
    let mut merged: Vec<ContentMessage> = Vec::with_capacity(contents.len());
    for message in std::mem::take(contents) {
        if let Some(last) = merged.last_mut() {
            if last.role == message.role && same_turn_family(last, &message) {
                last.parts.extend(message.parts);
                continue;
            }
        }
        merged.push(message);
    }
    *contents = merged;
}

/// 调用轮和回执轮即使角色相同也必须分成两段。Gemini 的回执角色是 model，
/// 粘进前面的 functionCall 之后，上游会把整段当成又一轮缺少签名的模型回复。
fn same_turn_family(left: &ContentMessage, right: &ContentMessage) -> bool {
    let left_kind = turn_family(left);
    let right_kind = turn_family(right);
    !matches!(
        (left_kind, right_kind),
        (TurnFamily::Response, TurnFamily::Call)
            | (TurnFamily::Call, TurnFamily::Response)
            | (TurnFamily::Response, TurnFamily::Other)
            | (TurnFamily::Other, TurnFamily::Response)
    )
}

enum TurnFamily {
    Call,
    Response,
    Other,
}

fn turn_family(message: &ContentMessage) -> TurnFamily {
    let has_call = message
        .parts
        .iter()
        .any(|part| part.function_call.is_some());
    let has_response = message
        .parts
        .iter()
        .any(|part| part.function_response.is_some());
    if has_response && !has_call {
        TurnFamily::Response
    } else if has_call && !has_response {
        TurnFamily::Call
    } else {
        TurnFamily::Other
    }
}

fn has_substantive_parts(parts: &[ContentPart]) -> bool {
    parts.iter().any(|part| {
        part.function_call.is_some()
            || part.function_response.is_some()
            || part.inline_data.is_some()
            || part.text.as_deref().is_some_and(|text| {
                let trimmed = text.trim();
                !trimmed.is_empty() && trimmed != "(no content)" && trimmed != "·"
            })
    })
}

// ─────────────────────────────────────────────────────────────────────────────
// 内部响应结构建模
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidatePart {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub thought: Option<bool>,
    #[serde(default)]
    pub thought_signature: Option<String>,
    #[serde(default)]
    pub function_call: Option<FunctionCall>,
    #[serde(default)]
    pub inline_data: Option<InlineData>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateContent {
    pub parts: Option<Vec<CandidatePart>>,
    pub role: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseCandidate {
    pub content: Option<CandidateContent>,
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageMetadata {
    pub prompt_token_count: Option<i64>,
    pub candidates_token_count: Option<i64>,
    pub total_token_count: Option<i64>,
    /// 命中提示缓存的输入。不单独记的话，按 Anthropic 口径回给客户端会把缓存算两遍。
    pub cached_content_token_count: Option<i64>,
    pub thoughts_token_count: Option<i64>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerateContentResponse {
    pub response: Option<Box<GenerateContentResponse>>,
    pub candidates: Option<Vec<ResponseCandidate>>,
    pub usage_metadata: Option<UsageMetadata>,
    pub error: Option<Value>,
}

impl GenerateContentResponse {
    /// 展开外层包裹的 response（Google CloudCode SSE 格式：`{"response": {"candidates": [...]}}`）。
    pub fn unwrap_envelope(mut self) -> Self {
        if let Some(inner) = self.response.take() {
            let mut inner = *inner;
            if inner.usage_metadata.is_none() {
                inner.usage_metadata = self.usage_metadata;
            }
            inner
        } else {
            self
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 思维链 (Thinking Tags) 流式剥离器
// ─────────────────────────────────────────────────────────────────────────────

/// 状态机式思维标签提取器。
/// 在模型吐出流式字符时，正确把 `<thinking>...</thinking>` 拆分进 thinking 缓冲区，
/// 剩余正文进文本缓冲区。
pub struct ThinkingTagParser {
    in_thinking: bool,
    buffer: String,
}

impl ThinkingTagParser {
    pub fn new() -> Self {
        Self {
            in_thinking: false,
            buffer: String::new(),
        }
    }

    /// 接收流式文本片断，产出 `(思维增量, 最终正文增量)`。
    pub fn push(&mut self, text: &str) -> (Option<String>, Option<String>) {
        self.buffer.push_str(text);
        let mut thinking_out = String::new();
        let mut text_out = String::new();

        loop {
            if !self.in_thinking {
                if let Some(pos) = self.buffer.find("<thinking>") {
                    // 标签前面的算正文
                    if pos > 0 {
                        text_out.push_str(&self.buffer[..pos]);
                    }
                    self.buffer.drain(..pos + "<thinking>".len());
                    self.in_thinking = true;
                } else if let Some(pos) = self.buffer.find("<thought>") {
                    if pos > 0 {
                        text_out.push_str(&self.buffer[..pos]);
                    }
                    self.buffer.drain(..pos + "<thought>".len());
                    self.in_thinking = true;
                } else {
                    // 没有命中开始标签，但若末尾可能有半截标签比如 "<th"，则保留末尾
                    let keep = if self.buffer.ends_with('<') {
                        1
                    } else if let Some(idx) = self.buffer.rfind('<') {
                        if self.buffer.len() - idx < 10 {
                            self.buffer.len() - idx
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let emit_len = self.buffer.len() - keep;
                    if emit_len > 0 {
                        text_out.push_str(&self.buffer[..emit_len]);
                        self.buffer.drain(..emit_len);
                    }
                    break;
                }
            } else {
                if let Some(pos) = self.buffer.find("</thinking>") {
                    if pos > 0 {
                        thinking_out.push_str(&self.buffer[..pos]);
                    }
                    self.buffer.drain(..pos + "</thinking>".len());
                    self.in_thinking = false;
                } else if let Some(pos) = self.buffer.find("</thought>") {
                    if pos > 0 {
                        thinking_out.push_str(&self.buffer[..pos]);
                    }
                    self.buffer.drain(..pos + "</thought>".len());
                    self.in_thinking = false;
                } else {
                    let keep = if self.buffer.ends_with('<') {
                        1
                    } else if let Some(idx) = self.buffer.rfind('<') {
                        if self.buffer.len() - idx < 11 {
                            self.buffer.len() - idx
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    let emit_len = self.buffer.len() - keep;
                    if emit_len > 0 {
                        thinking_out.push_str(&self.buffer[..emit_len]);
                        self.buffer.drain(..emit_len);
                    }
                    break;
                }
            }
        }

        let th = if thinking_out.is_empty() {
            None
        } else {
            Some(thinking_out)
        };
        let tx = if text_out.is_empty() {
            None
        } else {
            Some(text_out)
        };
        (th, tx)
    }

    /// 流终结时冲刷剩余缓冲。
    pub fn flush(self) -> (Option<String>, Option<String>) {
        if self.buffer.is_empty() {
            return (None, None);
        }
        if self.in_thinking {
            (Some(self.buffer), None)
        } else {
            (None, Some(self.buffer))
        }
    }
}

const GEMINI_SCHEMA_FIELDS: &[&str] = &[
    "type",
    "description",
    "properties",
    "required",
    "items",
    "enum",
    "title",
];

/// Gemini 的 Schema 只认很少几个字段。Claude Code 的工具声明带着 `$schema`、
/// `const`、`propertyNames`、`exclusiveMinimum`，原样送上去就是 400。
pub fn clean_tool_schema(value: &mut Value) {
    clean_schema_node(value, 0);
}

fn clean_schema_node(value: &mut Value, depth: usize) {
    if depth > 12 {
        return;
    }
    let Value::Object(map) = value else {
        return;
    };

    absorb_const(map);
    absorb_all_of(map);

    if map.get("type").and_then(Value::as_str) == Some("object") || map.contains_key("properties") {
        if let Some(items) = map.remove("items") {
            if let Some(source) = items.as_object() {
                let target = map
                    .entry("properties".to_string())
                    .or_insert_with(|| json!({}));
                if let Some(target_map) = target.as_object_mut() {
                    for (key, child) in source {
                        target_map
                            .entry(key.clone())
                            .or_insert_with(|| child.clone());
                    }
                }
            }
        }
    }

    if let Some(props) = map.get_mut("properties") {
        if !props.is_object() {
            *props = json!({});
        }
    }
    if let Some(Value::Object(props)) = map.get_mut("properties") {
        props.retain(|_, child| child.is_object());
        for child in props.values_mut() {
            clean_schema_node(child, depth + 1);
        }
    }
    let property_keys: Option<Vec<String>> = map
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| props.keys().cloned().collect());
    if let Some(keys) = property_keys {
        if let Some(Value::Array(required)) = map.get_mut("required") {
            required.retain(|item| {
                item.as_str()
                    .is_some_and(|key| keys.iter().any(|name| name == key))
            });
        }
        if map
            .get("required")
            .and_then(Value::as_array)
            .is_some_and(|arr| arr.is_empty())
        {
            map.remove("required");
        }
        if !map.contains_key("type") {
            map.insert("type".into(), Value::String("object".into()));
        }
    }

    if map.get("items").is_some_and(|items| !items.is_object()) {
        map.remove("items");
    }
    if let Some(items) = map.get_mut("items") {
        clean_schema_node(items, depth + 1);
        if !map.contains_key("type") {
            map.insert("type".into(), Value::String("array".into()));
        }
    }
    let is_array = map
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|text| text.eq_ignore_ascii_case("array"));
    if is_array && !map.contains_key("items") {
        map.insert("items".into(), json!({ "type": "string" }));
    }

    for key in ["anyOf", "oneOf"] {
        if let Some(Value::Array(branches)) = map.get_mut(key) {
            for branch in branches.iter_mut() {
                clean_schema_node(branch, depth + 1);
            }
        }
    }
    let union = map
        .remove("anyOf")
        .or_else(|| map.remove("oneOf"))
        .and_then(|value| match value {
            Value::Array(branches) => Some(branches),
            _ => None,
        });
    if let Some(branches) = union {
        if let Some(Value::Object(branch)) = best_schema_branch(&branches) {
            for (key, branch_value) in branch {
                match key.as_str() {
                    "properties" => merge_object_field(map, "properties", &branch_value),
                    "required" => merge_array_field(map, "required", &branch_value),
                    _ => {
                        map.entry(key).or_insert(branch_value);
                    }
                }
            }
        }
    }

    map.retain(|key, _| GEMINI_SCHEMA_FIELDS.contains(&key.as_str()));

    let is_object = map
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|text| text.eq_ignore_ascii_case("object"));
    if is_object && !map.contains_key("properties") {
        map.insert("properties".into(), json!({}));
    }
    let property_keys: Option<Vec<String>> = map
        .get("properties")
        .and_then(Value::as_object)
        .map(|props| props.keys().cloned().collect());
    if let Some(Value::Array(required)) = map.get_mut("required") {
        match &property_keys {
            Some(keys) => {
                required.retain(|item| {
                    item.as_str()
                        .is_some_and(|key| keys.iter().any(|name| name == key))
                });
            }
            None => required.clear(),
        }
    }
    if map
        .get("required")
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.is_empty())
    {
        map.remove("required");
    }

    if !map.contains_key("type") {
        let inferred = if map.contains_key("enum") {
            Some("string")
        } else if map.contains_key("properties") {
            Some("object")
        } else if map.contains_key("items") {
            Some("array")
        } else {
            None
        };
        if let Some(inferred) = inferred {
            map.insert("type".into(), Value::String(inferred.into()));
        }
    }

    let fallback = if map.contains_key("properties") {
        "object"
    } else if map.contains_key("items") {
        "array"
    } else {
        "string"
    };
    if let Some(type_val) = map.get_mut("type") {
        let picked = match type_val {
            Value::String(text) => {
                let lower = text.to_ascii_lowercase();
                if lower == "null" {
                    None
                } else {
                    Some(lower)
                }
            }
            Value::Array(options) => options.iter().filter_map(Value::as_str).find_map(|text| {
                let lower = text.to_ascii_lowercase();
                (lower != "null").then_some(lower)
            }),
            _ => None,
        };
        *type_val = Value::String(picked.unwrap_or_else(|| fallback.into()));
    }

    let mut has_enum = false;
    if let Some(Value::Array(items)) = map.get_mut("enum") {
        for item in items.iter_mut() {
            if !item.is_string() {
                let text = if item.is_null() {
                    "null".to_string()
                } else {
                    item.to_string()
                };
                *item = Value::String(text);
            }
        }
        items.retain(|item| item.as_str().is_some_and(|text| !text.is_empty()));
        has_enum = !items.is_empty();
    }
    if map
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|arr| arr.is_empty())
    {
        map.remove("enum");
    }
    if has_enum {
        map.insert("type".into(), Value::String("string".into()));
    }
    if let Some(Value::String(text)) = map.get_mut("type") {
        *text = text.to_ascii_uppercase();
    }
}

fn absorb_const(map: &mut serde_json::Map<String, Value>) {
    let Some(const_val) = map.remove("const") else {
        return;
    };
    if !map.contains_key("type") {
        let inferred = match &const_val {
            Value::String(_) => Some("string"),
            Value::Bool(_) => Some("boolean"),
            Value::Number(number) if number.is_i64() || number.is_u64() => Some("integer"),
            Value::Number(_) => Some("number"),
            Value::Array(_) => Some("array"),
            Value::Object(_) => Some("object"),
            Value::Null => None,
        };
        if let Some(inferred) = inferred {
            map.insert("type".into(), Value::String(inferred.into()));
        }
    }
    let entry = map
        .entry("enum".to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Value::Array(items) = entry {
        if !items.contains(&const_val) {
            items.push(const_val);
        }
    }
}

fn absorb_all_of(map: &mut serde_json::Map<String, Value>) {
    let Some(Value::Array(branches)) = map.remove("allOf") else {
        return;
    };
    for branch in branches {
        let Value::Object(branch) = branch else {
            continue;
        };
        for (key, value) in branch {
            match key.as_str() {
                "properties" => merge_object_field(map, "properties", &value),
                "required" => merge_array_field(map, "required", &value),
                _ => {
                    map.entry(key).or_insert(value);
                }
            }
        }
    }
}

fn merge_object_field(map: &mut serde_json::Map<String, Value>, key: &str, incoming: &Value) {
    let Some(source) = incoming.as_object() else {
        return;
    };
    let target = map.entry(key.to_string()).or_insert_with(|| json!({}));
    if let Some(target_map) = target.as_object_mut() {
        for (name, child) in source {
            target_map
                .entry(name.clone())
                .or_insert_with(|| child.clone());
        }
    }
}

fn merge_array_field(map: &mut serde_json::Map<String, Value>, key: &str, incoming: &Value) {
    let Some(source) = incoming.as_array() else {
        return;
    };
    let target = map
        .entry(key.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if let Some(target_items) = target.as_array_mut() {
        for item in source {
            if !target_items.contains(item) {
                target_items.push(item.clone());
            }
        }
    }
}

fn best_schema_branch(branches: &[Value]) -> Option<Value> {
    let mut best: Option<&Value> = None;
    let mut best_score = 0;
    for branch in branches {
        let score = score_schema_branch(branch);
        if score > best_score {
            best_score = score;
            best = Some(branch);
        }
    }
    best.cloned()
}

fn score_schema_branch(value: &Value) -> i32 {
    let Value::Object(map) = value else {
        return 0;
    };
    if map.contains_key("properties") || map.get("type").and_then(Value::as_str) == Some("object") {
        return 3;
    }
    if map.contains_key("items") || map.get("type").and_then(Value::as_str) == Some("array") {
        return 2;
    }
    if map.contains_key("enum") {
        return 2;
    }
    match map.get("type").and_then(Value::as_str) {
        Some("null") | None => 0,
        Some(_) => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_models_accurately() {
        assert_eq!(upstream_model("gemini-3.1-pro"), "gemini-3.8-flash-high");
        assert_eq!(upstream_model("gemini-3.1-pro-high"), "gemini-3.8-flash-high");
        assert_eq!(upstream_model("gemini-3.1-pro-low"), "gemini-3.8-flash-high");
        assert_eq!(
            upstream_model("antigravity/gemini-3.8-flash"),
            "gemini-3.8-flash-high"
        );
        assert_eq!(
            upstream_model("gemini-3.8-flash-low"),
            "gemini-3.8-flash-low"
        );
        assert_eq!(
            upstream_model("gemini-2.5-flash"),
            "gemini-3.8-flash-high"
        );
        assert_eq!(
            upstream_model("claude-3-7-sonnet"),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            upstream_model("claude-opus-4.6"),
            "claude-opus-4-6-thinking"
        );
        assert_eq!(
            upstream_model("claude-opus-4-6-thinking"),
            "claude-opus-4-6-thinking"
        );
        assert_eq!(upstream_model("gpt-4o"), "gemini-3.8-flash-high");
        assert_eq!(
            upstream_model("some-unreleased-model"),
            "some-unreleased-model"
        );
        assert_eq!(CONTENT_ORIGINS[0], DAILY_ENDPOINT);
        assert!(should_try_next_endpoint(503));
        assert!(!should_try_next_endpoint(400));
        assert!(!should_try_next_endpoint(429));
    }

    #[test]
    fn claude_thinking_max_tokens_stays_above_budget() {
        let (max, thinking) = align_generation("claude-opus-4-6-thinking", Some(1024));
        assert_eq!(thinking.unwrap().thinking_budget, Some(1024));
        assert!(max.unwrap() > 1024);
        assert_eq!(
            align_generation("claude-opus-4-6-thinking", Some(16_000)).0,
            Some(16_000)
        );
        assert_eq!(
            align_generation("claude-opus-4-6-thinking", None).0,
            Some(64_000)
        );
        assert!(align_generation("gemini-3.8-flash-high", Some(1000))
            .1
            .unwrap()
            .thinking_budget
            .is_none());
    }

    #[test]
    fn tool_schema_drops_fields_gemini_rejects() {
        let mut schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "properties": {
                "mode": { "const": "agent" },
                "names": { "type": "object", "propertyNames": { "type": "string" } },
                "count": { "type": "integer", "exclusiveMinimum": 0 },
                "kind": {
                    "anyOf": [
                        { "const": "read" },
                        { "type": "null" }
                    ]
                },
                "rows": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "width": { "type": "number", "exclusiveMinimum": 0 },
                            "height": { "type": "number", "exclusiveMinimum": 0 }
                        }
                    }
                }
            },
            "required": ["mode"]
        });
        clean_tool_schema(&mut schema);
        let text = schema.to_string();
        for banned in [
            "$schema",
            "propertyNames",
            "const",
            "exclusiveMinimum",
            "anyOf",
        ] {
            assert!(!text.contains(banned), "{text}");
        }
        assert_eq!(schema["properties"]["mode"]["enum"][0], "agent");
        assert_eq!(schema["properties"]["mode"]["type"], "STRING");
        assert_eq!(schema["properties"]["kind"]["enum"][0], "read");
        assert_eq!(schema["properties"]["count"]["type"], "INTEGER");
        assert_eq!(schema["type"], "OBJECT");
        assert!(schema["properties"]["rows"]["items"]["properties"]["width"]
            .get("exclusiveMinimum")
            .is_none());
    }

    #[test]
    fn thinking_parser_extracts_tags_cleanly() {
        let mut parser = ThinkingTagParser::new();
        let (th1, tx1) = parser.push("Hello <thinking>Thinking about this");
        assert_eq!(tx1.as_deref(), Some("Hello "));
        assert_eq!(th1.as_deref(), Some("Thinking about this"));

        let (th2, tx2) = parser.push(" deep problem</thinking> Here is the answer.");
        assert_eq!(th2.as_deref(), Some(" deep problem"));
        assert_eq!(tx2.as_deref(), Some(" Here is the answer."));
    }

    #[test]
    fn deterministic_session_id_is_stable() {
        let s1 = derive_session_id("hi", Some("you are a bot"));
        let s2 = derive_session_id("hi", Some("you are a bot"));
        assert_eq!(s1, s2);
        assert!(s1.starts_with("session-"));
        assert_ne!(s1, derive_session_id("hi", Some("you are another bot")));
    }

    #[test]
    fn reminder_does_not_rewrite_itself() {
        let once = wrap_system_reminder("keep the file");
        assert!(once.contains("keep the file"));
        assert_eq!(wrap_system_reminder(&once), once);
        assert!(wrap_system_reminder("  ").is_empty());
    }

    #[test]
    fn finalize_merges_roles_and_closes_on_user() {
        let mut request = GenerateContentRequest {
            contents: vec![
                ContentMessage::user(vec![ContentPart::text("a")]),
                ContentMessage::user(vec![ContentPart::text("b")]),
                ContentMessage::model(vec![ContentPart::text("c")]),
            ],
            system_instruction: None,
            generation_config: None,
            tools: None,
            tool_config: None,
            session_id: None,
        };
        request.finalize();
        assert_eq!(request.contents.len(), 3);
        assert_eq!(request.contents[0].parts.len(), 2);
        assert_eq!(
            request.contents.last().unwrap().role.as_deref(),
            Some("user")
        );
        assert_eq!(
            request.contents.last().unwrap().parts[0].text.as_deref(),
            Some(USER_TURN_FALLBACK)
        );
    }
}
