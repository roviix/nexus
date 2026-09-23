//! Codex 后端（`chatgpt.com/backend-api/codex/responses`）推理路径的协议知识，全部是纯函数。
//!
//! 回答五个问题：请求头长什么样、请求体要改哪些地方、身份标识怎么按账号收敛、响应头里的
//! 额度怎么读、错误怎么分类（以及哪些 400 其实可以修一下再发）。不发请求、不读流——那些在
//! `codex::upstream`。拆开是为了让协议细节能被单元测试直接盯住：上游改一个字段名，这里先红。
//!
//! 与云端 gateway 的 `codex-protocol.ts` 是同一套事实的两份实现，逐条来源见
//! `docs/relay/CODEX-OAUTH.md` §1。

use crate::error::UpstreamKind;
use crate::normalized::{ChatRequest, Role};
use nexus_chatgpt::{CLIENT_VERSION, ORIGINATOR};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

type Json = Map<String, Value>;

// ---------------------------------------------------------------------------
// 常量
// ---------------------------------------------------------------------------

/// 客户端没带 instructions 时顶上的一句。Codex 后端要求这个字段非空；Codex CLI 自己一定会带，
/// 所以只在 Chat / Anthropic 方言桥接进来、且没给 system 提示时用到。
pub const DEFAULT_INSTRUCTIONS: &str = "You are a helpful, knowledgeable assistant.";

/// ChatGPT 内部端点不认的顶层 Responses 参数。带上就是 400。`service_tier` **不在**这里：
/// 它是 Codex 的 Fast 模式，模型目录里明确列为支持。
pub const REJECTED_FIELDS: &[&str] = &[
    "max_output_tokens",
    "max_completion_tokens",
    "temperature",
    "top_p",
    "frequency_penalty",
    "presence_penalty",
    "user",
    "metadata",
    "prompt_cache_retention",
    "safety_identifier",
    "stream_options",
    "truncation",
    "stop_sequences",
    "previous_response_id",
    "generate",
    "background",
    // ChatGPT 桌面端每个请求都带。Codex 订阅接口不认，原话是
    // `Unsupported parameter: personality`，整段对话 400。
    "personality",
];

/// 上游对 call_id 的长度限制。
const CALL_ID_MAX: usize = 64;

/// 默认 Codex 客户端随每个请求带的 beta 特性声明。只在客户端没自己声明时补。
const DEFAULT_BETA_FEATURES: &str = "remote_compaction_v2";

/// 上游保留的工具名 → 出站别名。`python` 是它内置代码解释器的名字，客户端声明同名工具会被拒。
const RESERVED_TOOL_ALIASES: &[(&str, &str)] = &[("python", "python__nexus")];

/// 上游认的档位，顺序就是强弱顺序，被拒时按它降一档重试。
pub const EFFORTS: &[&str] = &[
    "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
];

/// 空模型的稳妥默认。`gpt-6-astra` 很多 ChatGPT 号还没开通，不能写死成第一项。
pub const DEFAULT_CHAT_MODEL: &str = "gpt-5.4";

/// 账号目录里有它才往前排：用户要 GPT-6 时用这个上游 slug。
pub const PREFERRED_CHAT_MODEL: &str = "gpt-6-astra";

/// 短名 → 上游 slug。只做显式别名，不按家族猜。
const CODEX_ALIASES: &[(&str, &str)] = &[("gpt-6", PREFERRED_CHAT_MODEL)];

/// 本地目录里的 Codex 模型（静态兜底；上游目录接口能拉到更新的清单）。和云端控制面的
/// `CODEX_MODELS` 同一份。第一项是空模型的默认；Astra 在账号目录确认有了再往前排。
pub const CODEX_MODELS: &[&str] = &[
    DEFAULT_CHAT_MODEL,
    "gpt-5.4-mini",
    "gpt-5.6-sol",
    "gpt-5.6",
    "gpt-5.6-terra",
    "gpt-5.6-luna",
    "gpt-5.5",
    PREFERRED_CHAT_MODEL,
    "gpt-5.3-codex",
    "gpt-5.3-codex-spark",
    "gpt-5.2",
    "codex-auto-review",
];

/// 显式把请求送到 ChatGPT 通道的模型名前缀。
pub const ROUTE_PREFIXES: &[&str] = &["chatgpt/", "codex/", "openai-oauth/"];

/// 这个模型名是不是 Codex 的（剥掉路由前缀与档位后缀之后在目录或别名表里）。
pub fn is_codex_model(model: &str) -> bool {
    let (name, _) = split_route_prefix(model);
    let base = split_effort_suffix(name).0.to_ascii_lowercase();
    CODEX_MODELS.contains(&base.as_str()) || CODEX_ALIASES.iter().any(|(from, _)| *from == base)
}

/// 目录项上要挂的短名（给广场搜索 / 接入提示）。
pub fn catalog_aliases(id: &str) -> Vec<&'static str> {
    CODEX_ALIASES
        .iter()
        .filter(|(_, to)| to.eq_ignore_ascii_case(id))
        .map(|(from, _)| *from)
        .collect()
}

// ---------------------------------------------------------------------------
// 出图：`image_generation` 工具
// ---------------------------------------------------------------------------

/// ChatGPT 订阅号能出图的模型。Codex 后端没有 `/v1/images`：出图是 Responses 请求里的
/// `image_generation` 工具，由一个文本模型代为调用，图片以 `image_generation_call` 项回来。
pub const CODEX_IMAGE_MODELS: &[&str] = &["gpt-image-2", "gpt-image-1.5", "gpt-image-1"];

/// 代为调用出图工具的文本模型。挑最便宜的：它只负责把提示词原样交给工具。
pub const IMAGE_MAIN_MODEL: &str = "gpt-5.4-mini";

/// 给代调模型的指令。客户在 `/v1/images` 里写的提示词就是最终提示词，不许它润色——
/// 这条 API 的语义是「照我说的画」，改写了客户端对不上自己发的东西。
pub const IMAGE_INSTRUCTIONS: &str = "Call the image_generation tool with the user's prompt exactly as written. \
Do not rewrite, expand, shorten, translate, or add or remove any detail; keep the original language, wording and punctuation.";

pub fn is_codex_image_model(model: &str) -> bool {
    let (name, _) = split_route_prefix(model);
    let base = name.trim().to_ascii_lowercase();
    CODEX_IMAGE_MODELS.contains(&base.as_str())
}

/// 一次出图请求里我们会转给工具的参数。都是 OpenAI images API 自己的字段名，原样过。
/// `references` 非空就是编辑（`action: edit`），参考图作为 `input_image` 跟提示词一起进 input；
/// `mask` 进工具的 `input_image_mask`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageOptions<'a> {
    pub size: Option<&'a str>,
    pub quality: Option<&'a str>,
    pub background: Option<&'a str>,
    pub output_format: Option<&'a str>,
    pub references: &'a [String],
    pub mask: Option<&'a str>,
}

/// `/v1/images/{generations,edits}` → Codex 的 Responses 请求体。一次一张（`n` 由 server 串行调）。
pub fn build_image_body(
    prompt: &str,
    image_model: &str,
    opts: &ImageOptions<'_>,
    namespace: &str,
    session_key: &str,
) -> Json {
    let (model, _) = split_route_prefix(image_model);
    let editing = !opts.references.is_empty();
    let mut tool = Json::new();
    tool.insert("type".into(), Value::String("image_generation".into()));
    tool.insert(
        "action".into(),
        Value::String(if editing { "edit" } else { "generate" }.into()),
    );
    tool.insert(
        "model".into(),
        Value::String(model.trim().to_ascii_lowercase()),
    );
    if let Some(mask) = opts.mask.map(str::trim).filter(|m| !m.is_empty()) {
        tool.insert("input_image_mask".into(), json!({ "image_url": mask }));
    }
    for (k, v) in [
        ("size", opts.size),
        ("quality", opts.quality),
        ("background", opts.background),
        ("output_format", opts.output_format),
    ] {
        if let Some(v) = v.map(str::trim).filter(|v| !v.is_empty()) {
            tool.insert(k.into(), Value::String(v.to_string()));
        }
    }
    let mut content = vec![json!({ "type": "input_text", "text": prompt })];
    for r in opts.references {
        content.push(json!({ "type": "input_image", "image_url": r }));
    }
    let mut body = Json::new();
    body.insert("model".into(), Value::String(IMAGE_MAIN_MODEL.into()));
    body.insert(
        "instructions".into(),
        Value::String(IMAGE_INSTRUCTIONS.into()),
    );
    body.insert(
        "input".into(),
        json!([{ "type": "message", "role": "user", "content": content }]),
    );
    body.insert("stream".into(), Value::Bool(true));
    body.insert("store".into(), Value::Bool(false));
    body.insert(
        "reasoning".into(),
        json!({ "effort": "medium", "summary": "auto" }),
    );
    body.insert("include".into(), json!(["reasoning.encrypted_content"]));
    body.insert("parallel_tool_calls".into(), Value::Bool(true));
    body.insert("tools".into(), Value::Array(vec![Value::Object(tool)]));
    body.insert("tool_choice".into(), json!({ "type": "image_generation" }));
    body.insert(
        "prompt_cache_key".into(),
        Value::String(session_uuid(namespace, session_key)),
    );
    body
}

/// 模型没画图、只回了一段话：是被安全策略拒了，还是根本没去调工具。两种回给客户的错不同。
pub fn looks_like_content_refusal(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    [
        "content policy",
        "content_policy",
        "content filter",
        "content_filter",
        "safety system",
        "safety policy",
        "safety violation",
        "moderation",
        "安全系统",
        "安全策略",
        "安全政策",
        "内容政策",
        "内容审核",
        "违规内容",
        "不适合生成",
    ]
    .iter()
    .any(|m| t.contains(m))
}

/// `image_generation` 工具的 `output_format` → MIME。缺省 png。
pub fn image_mime(output_format: Option<&str>) -> &'static str {
    match output_format
        .map(|f| f.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("jpeg") | Some("jpg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

/// `"1024x1536"` → (1024, 1536)。
pub fn parse_size(size: Option<&str>) -> Option<(u32, u32)> {
    let (w, h) = size?.trim().split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

/// `chatgpt/gpt-5.4` → (`gpt-5.4`, true)。`cursor/x` 不在这里处理——那是 Cursor 那边的前缀。
pub fn split_route_prefix(model: &str) -> (&str, bool) {
    let m = model.trim();
    for p in ROUTE_PREFIXES {
        if m.len() > p.len() && m[..p.len()].eq_ignore_ascii_case(p) {
            return (&m[p.len()..], true);
        }
    }
    (m, false)
}

/// `gpt-5.4-high` → (`gpt-5.4`, Some("high"))。上游只认基础名，档位走 `reasoning.effort`。
pub fn split_effort_suffix(model: &str) -> (&str, Option<&'static str>) {
    let m = model.trim();
    if let Some((head, tail)) = m.rsplit_once('-') {
        let lower = tail.to_ascii_lowercase();
        if let Some(e) = EFFORTS.iter().find(|e| **e == lower) {
            if !head.is_empty() {
                return (head, Some(e));
            }
        }
    }
    (m, None)
}

pub fn to_effort(effort: Option<&str>) -> Option<&'static str> {
    let e = effort?.trim().to_ascii_lowercase();
    EFFORTS.iter().find(|x| **x == e).copied()
}

// ---------------------------------------------------------------------------
// 身份收敛
// ---------------------------------------------------------------------------

/// 账号的身份命名空间：用上游自己的 ChatGPT 账号 id，重装应用也稳定。
pub fn identity_namespace(account_ref: &str) -> String {
    format!("chatgpt:{}", account_ref.trim())
}

fn sha256_hex(seed: &str) -> String {
    let d = Sha256::digest(seed.as_bytes());
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn uuid_from_digest(seed: &str) -> String {
    let h = sha256_hex(seed);
    format!(
        "{}-{}-4{}-a{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        &h[17..20],
        &h[20..32]
    )
}

fn split_uuid(v: &str) -> Option<(String, &str)> {
    let b = v.as_bytes();
    if b.len() < 36 {
        return None;
    }
    let is_hex = |r: std::ops::Range<usize>| b[r].iter().all(|c| c.is_ascii_hexdigit());
    let dashes = b[8] == b'-' && b[13] == b'-' && b[18] == b'-' && b[23] == b'-';
    if dashes && is_hex(0..8) && is_hex(9..13) && is_hex(14..18) && is_hex(19..23) && is_hex(24..36)
    {
        Some((v[..36].to_ascii_lowercase(), &v[36..]))
    } else {
        None
    }
}

/// 把客户端的一个标识换成这个账号专属的替身。确定性：同账号 × 同原值永远同一个替身；
/// UUID 形态只换 UUID、保留尾巴（window id 形如 `<会话>:0`）。
pub fn scope_identity_value(namespace: &str, raw: &str) -> String {
    let v = raw.trim();
    if v.is_empty() {
        return String::new();
    }
    let (core, suffix) = match split_uuid(v) {
        Some((c, s)) => (c, s),
        None => (v.to_string(), ""),
    };
    format!(
        "{}{suffix}",
        uuid_from_digest(&format!("nexus:codex:identity:v1:{namespace}:{core}"))
    )
}

/// 会话键 → 账号专属的 UUID 形态缓存键。
pub fn session_uuid(namespace: &str, seed: &str) -> String {
    uuid_from_digest(&format!("nexus:codex:session:{namespace}:{seed}"))
}

/// 客户端的 prompt_cache_key 既进请求体也进请求头，形状对不上就换成稳定摘要。
pub fn safe_cache_key(value: Option<&Value>) -> Option<String> {
    let v = value?.as_str()?.trim();
    if v.is_empty() {
        return None;
    }
    let printable = v.len() <= 128 && v.bytes().all(|b| (0x21..=0x7e).contains(&b));
    Some(if printable {
        v.to_string()
    } else {
        uuid_from_digest(&format!("nexus:codex:cache-key:{v}"))
    })
}

const DEVICE_FIELDS: &[&str] = &["installation_id", "x-codex-installation-id"];
const IDENTITY_FIELDS: &[&str] = &[
    "installation_id",
    "x-codex-installation-id",
    "session_id",
    "session-id",
    "thread_id",
    "thread-id",
    "x-codex-parent-thread-id",
    "turn_id",
    "turn-id",
    "window_id",
    "x-codex-window-id",
    "x-client-request-id",
    "prompt_cache_key",
];

/// 身份改写的三档。本地网关一台机器一个人，默认 `scope` 就是最像真实用法的：上游看到的设备数
/// 等于真实客户端设备数，只是换了名字。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IdentityMode {
    #[default]
    Scope,
    Device,
    Off,
}

pub fn device_id_for(namespace: &str) -> String {
    uuid_from_digest(&format!("nexus:codex:device:v1:{namespace}"))
}

fn scope_one(namespace: &str, field: &str, raw: &str, mode: IdentityMode) -> String {
    match mode {
        IdentityMode::Off => raw.to_string(),
        IdentityMode::Device if DEVICE_FIELDS.contains(&field) => device_id_for(namespace),
        _ => scope_identity_value(namespace, raw),
    }
}

fn scope_fields_in_place(values: &mut Json, namespace: &str, mode: IdentityMode) {
    for field in IDENTITY_FIELDS {
        let Some(Value::String(raw)) = values.get(*field) else {
            continue;
        };
        if raw.trim().is_empty() {
            continue;
        }
        let scoped = scope_one(namespace, field, raw, mode);
        values.insert((*field).to_string(), Value::String(scoped));
    }
}

fn scope_turn_metadata(raw: &str, namespace: &str, mode: IdentityMode) -> String {
    match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(mut o)) => {
            scope_fields_in_place(&mut o, namespace, mode);
            Value::Object(o).to_string()
        }
        _ => raw.to_string(),
    }
}

/// 把请求体里的客户端标识全部换成账号专属替身，并把换完的值按请求头的名字交出来。
pub fn apply_identity_scope(
    body: &mut Json,
    namespace: &str,
    mode: IdentityMode,
) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    if let Some(Value::Object(meta)) = body.get_mut("client_metadata") {
        scope_fields_in_place(meta, namespace, mode);
        if let Some(Value::String(tm)) = meta.get("x-codex-turn-metadata").cloned() {
            let scoped = scope_turn_metadata(&tm, namespace, mode);
            meta.insert(
                "x-codex-turn-metadata".into(),
                Value::String(scoped.clone()),
            );
            headers.insert("x-codex-turn-metadata".to_string(), scoped);
        }
        for h in [
            "x-codex-installation-id",
            "x-codex-window-id",
            "x-client-request-id",
        ] {
            if let Some(Value::String(v)) = meta.get(h) {
                if !v.is_empty() {
                    headers.insert(h.to_string(), v.clone());
                }
            }
        }
    }
    if let Some(Value::String(k)) = body.get("prompt_cache_key").cloned() {
        if !k.trim().is_empty() {
            body.insert(
                "prompt_cache_key".into(),
                Value::String(scope_one(namespace, "prompt_cache_key", &k, mode)),
            );
        }
    }
    headers
}

// ---------------------------------------------------------------------------
// 会话状态印：上游铸、客户端回带，只能回到铸它的那个号
// ---------------------------------------------------------------------------

pub const TURN_STATE_HEADER: &str = "x-codex-turn-state";
const TURN_STATE_TAG_PREFIX: &str = "nx1";

fn account_tag(namespace: &str) -> String {
    sha256_hex(&format!("nexus:codex:turn-state-tag:{namespace}"))[..12].to_string()
}

/// 上游在响应头里铸的 `x-codex-turn-state` 转给客户端之前打上账号标记。回带时只有同一个号
/// 铸的才剥掉标记转给上游——把 A 号铸的印交给 B 号是真实客户端永远不会产生的矛盾信号。
pub fn tag_turn_state(namespace: &str, blob: &str) -> String {
    format!("{TURN_STATE_TAG_PREFIX}.{}.{blob}", account_tag(namespace))
}

pub fn untag_turn_state(namespace: &str, value: Option<&str>) -> Option<String> {
    let v = value?.trim();
    let prefix = format!("{TURN_STATE_TAG_PREFIX}.{}.", account_tag(namespace));
    v.strip_prefix(prefix.as_str())
        .filter(|rest| !rest.is_empty())
        .map(str::to_string)
}

/// 上游响应头里值得转给客户端的：会话状态印（打标）、额度头（真实客户端拿它显示用量）。
pub fn relay_response_headers(
    upstream: &reqwest::header::HeaderMap,
    namespace: &str,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    if let Some(turn) = upstream
        .get(TURN_STATE_HEADER)
        .and_then(|v| v.to_str().ok())
    {
        if !turn.trim().is_empty() {
            out.push((
                TURN_STATE_HEADER.to_string(),
                tag_turn_state(namespace, turn.trim()),
            ));
        }
    }
    for side in ["primary", "secondary"] {
        for suffix in ["used-percent", "reset-after-seconds", "window-minutes"] {
            let name = format!("x-codex-{side}-{suffix}");
            if let Some(v) = upstream.get(name.as_str()).and_then(|v| v.to_str().ok()) {
                if !v.trim().is_empty() {
                    out.push((name, v.trim().to_string()));
                }
            }
        }
    }
    out
}

/// 客户端请求头里值得带给上游的那几个，按账号收敛之后的值。请求体里 client_metadata 已经给出
/// 的值优先（两处必须一致）；请求头独有的原样转。
pub fn forward_client_headers(
    client: &HashMap<String, String>,
    namespace: &str,
    from_body: HashMap<String, String>,
    mode: IdentityMode,
) -> HashMap<String, String> {
    let mut out = from_body;
    let pass = |out: &mut HashMap<String, String>, name: &str| {
        if let Some(v) = client.get(name).filter(|v| !v.is_empty()) {
            out.entry(name.to_string()).or_insert_with(|| v.clone());
        }
    };
    pass(&mut out, "accept-language");
    pass(&mut out, "x-openai-internal-codex-responses-lite");
    pass(&mut out, "x-openai-subagent");
    match client
        .get("x-codex-beta-features")
        .filter(|v| !v.is_empty())
    {
        Some(v) => {
            out.insert("x-codex-beta-features".into(), v.clone());
        }
        None => {
            out.insert("x-codex-beta-features".into(), DEFAULT_BETA_FEATURES.into());
        }
    }
    for name in [
        "x-codex-installation-id",
        "x-codex-window-id",
        "x-client-request-id",
        "thread-id",
        "x-codex-parent-thread-id",
    ] {
        if out.contains_key(name) {
            continue;
        }
        if let Some(v) = client.get(name).filter(|v| !v.is_empty()) {
            let scoped = if IDENTITY_FIELDS.contains(&name) {
                scope_one(namespace, name, v, mode)
            } else {
                v.clone()
            };
            out.insert(name.to_string(), scoped);
        }
    }
    if !out.contains_key("x-codex-turn-metadata") {
        if let Some(v) = client
            .get("x-codex-turn-metadata")
            .filter(|v| !v.is_empty())
        {
            out.insert(
                "x-codex-turn-metadata".into(),
                scope_turn_metadata(v, namespace, mode),
            );
        }
    }
    if let Some(t) = untag_turn_state(namespace, client.get(TURN_STATE_HEADER).map(String::as_str))
    {
        out.insert(TURN_STATE_HEADER.into(), t);
    }
    out
}

// ---------------------------------------------------------------------------
// 请求头
// ---------------------------------------------------------------------------

pub fn user_agent() -> String {
    nexus_chatgpt::protocol::user_agent()
}

/// 推理请求的全部头。`client` 是从客户端转来、已按账号收敛的头；固定头排最后，说了算。
pub fn request_headers(
    access_token: &str,
    account_ref: &str,
    session_id: &str,
    client: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut h: Vec<(String, String)> = Vec::new();
    for (k, v) in client {
        h.push((k.to_ascii_lowercase(), v.clone()));
    }
    let fixed = [
        ("content-type", "application/json".to_string()),
        ("accept", "text/event-stream".to_string()),
        ("authorization", format!("Bearer {access_token}")),
        ("user-agent", user_agent()),
        ("originator", ORIGINATOR.to_string()),
        ("version", CLIENT_VERSION.to_string()),
        ("openai-beta", "responses=experimental".to_string()),
        ("session_id", session_id.to_string()),
        ("conversation_id", session_id.to_string()),
        ("chatgpt-account-id", account_ref.to_string()),
    ];
    for (k, v) in fixed {
        h.retain(|(name, _)| name != k);
        h.push((k.to_string(), v));
    }
    h
}

// ---------------------------------------------------------------------------
// 请求体
// ---------------------------------------------------------------------------

fn fit_call_id(id: &str, item_type: &str) -> String {
    if id.len() <= CALL_ID_MAX {
        return id.to_string();
    }
    let prefix = if item_type.starts_with("custom_tool_call") {
        "ctc_"
    } else {
        "fc_"
    };
    let digest = sha256_hex(id);
    format!("{prefix}{}", &digest[..CALL_ID_MAX - prefix.len()])
}

const TOOL_CALL_TYPES: &[&str] = &["function_call", "custom_tool_call"];
const TOOL_OUTPUT_TYPES: &[&str] = &["function_call_output", "custom_tool_call_output"];

/// reasoning 的回放凭据是一枚 Fernet 令牌：`gAAAA` 开头、base64url。形状不对的发上去是 400。
pub fn looks_like_reasoning_blob(value: &Value) -> bool {
    let Some(v) = value.as_str() else {
        return false;
    };
    v == v.trim()
        && v.starts_with("gAAAA")
        && v.len() <= 200_000
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'=' | b'-'))
}

fn type_of(item: &Json) -> String {
    item.get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string()
}

/// 逐条整理 input 项。规则每一条都对应上游一种拒收形态（见 CODEX-OAUTH.md 1.3）。
pub fn sanitize_input_items(input: Option<&Value>) -> Vec<Value> {
    let items: Vec<Value> = match input {
        Some(Value::String(s)) => {
            return if s.trim().is_empty() {
                vec![]
            } else {
                vec![json!({ "type": "message", "role": "user", "content": s })]
            };
        }
        Some(Value::Array(a)) => a.clone(),
        Some(Value::Object(o)) => vec![Value::Object(o.clone())],
        _ => return vec![],
    };
    let mut out = Vec::with_capacity(items.len());
    for raw in items {
        match raw {
            Value::String(s) => {
                if !s.is_empty() {
                    out.push(json!({ "type": "message", "role": "user", "content": s }));
                }
            }
            Value::Object(mut item) => {
                let t = type_of(&item);
                if t == "item_reference" {
                    continue;
                }
                let raw_id = item.get("id").and_then(|v| v.as_str()).map(str::to_string);
                item.remove("id");
                if t == "reasoning" {
                    item.remove("call_id");
                    if !item.get("summary").is_some_and(Value::is_array) {
                        item.insert("summary".into(), Value::Array(vec![]));
                    }
                    if item
                        .get("encrypted_content")
                        .is_some_and(|v| !looks_like_reasoning_blob(v))
                    {
                        item.remove("encrypted_content");
                    }
                    out.push(Value::Object(item));
                    continue;
                }
                if TOOL_CALL_TYPES.contains(&t.as_str()) || TOOL_OUTPUT_TYPES.contains(&t.as_str())
                {
                    let call_id = item
                        .get("call_id")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .or(raw_id
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty()));
                    if let Some(id) = call_id {
                        item.insert("call_id".into(), Value::String(fit_call_id(&id, &t)));
                    }
                    if TOOL_CALL_TYPES.contains(&t.as_str())
                        && item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .is_none_or(|n| n.trim().is_empty())
                    {
                        item.insert("name".into(), Value::String("tool".into()));
                    }
                    out.push(Value::Object(item));
                    continue;
                }
                item.remove("call_id");
                if item.get("role").and_then(|r| r.as_str()) == Some("system") {
                    item.insert("role".into(), Value::String("developer".into()));
                }
                out.push(Value::Object(item));
            }
            _ => {}
        }
    }
    out
}

fn alias_for(name: &Value) -> Option<(String, String)> {
    let n = name.as_str()?.trim();
    let lower = n.to_ascii_lowercase();
    RESERVED_TOOL_ALIASES
        .iter()
        .find(|(from, _)| *from == lower)
        .filter(|(_, to)| *to != n)
        .map(|(_, to)| (n.to_string(), (*to).to_string()))
}

fn rename_key(obj: &mut Json, key: &str, reverse: &mut HashMap<String, String>) {
    let Some((original, alias)) = obj.get(key).and_then(alias_for) else {
        return;
    };
    // 先走到的是工具声明：回程要还原成客户端**声明**时的写法（它按那个名字找 handler），
    // tool_choice / 历史里大小写不同的写法不覆盖它。
    reverse.entry(alias.clone()).or_insert(original);
    obj.insert(key.to_string(), Value::String(alias));
}

fn walk_tools(tools: Option<&mut Value>, reverse: &mut HashMap<String, String>) {
    let Some(Value::Array(list)) = tools else {
        return;
    };
    for t in list.iter_mut() {
        let Value::Object(o) = t else { continue };
        let ty = type_of(o).to_ascii_lowercase();
        if ty == "function" {
            rename_key(o, "name", reverse);
            if let Some(Value::Object(f)) = o.get_mut("function") {
                rename_key(f, "name", reverse);
            }
        }
        if ty == "namespace" {
            walk_tools(o.get_mut("tools"), reverse);
        }
    }
}

/// 把保留的工具名改成别名，返回「别名 → 原名」供回程还原。声明、tool_choice、历史三处一起改。
pub fn alias_reserved_tool_names(body: &mut Json) -> HashMap<String, String> {
    let mut reverse = HashMap::new();
    walk_tools(body.get_mut("tools"), &mut reverse);
    if let Some(Value::Object(tc)) = body.get_mut("tool_choice") {
        if type_of(tc).eq_ignore_ascii_case("function") {
            rename_key(tc, "name", &mut reverse);
            if let Some(Value::Object(f)) = tc.get_mut("function") {
                rename_key(f, "name", &mut reverse);
            }
        }
    }
    if let Some(Value::Array(input)) = body.get_mut("input") {
        for it in input.iter_mut() {
            let Value::Object(o) = it else { continue };
            let ty = type_of(o).to_ascii_lowercase();
            if ty == "additional_tools" {
                walk_tools(o.get_mut("tools"), &mut reverse);
            }
            if ty == "function_call" {
                rename_key(o, "name", &mut reverse);
            }
        }
    }
    reverse
}

pub fn restore_tool_name(name: &str, reverse: &HashMap<String, String>) -> String {
    reverse
        .get(name)
        .cloned()
        .unwrap_or_else(|| name.to_string())
}

pub struct PrepareOptions<'a> {
    /// 客户端要的模型名（可能带路由前缀与档位后缀，这里负责剥）。
    pub model: &'a str,
    /// 会话键，用来生成 prompt_cache_key。
    pub session_key: &'a str,
    pub namespace: &'a str,
    /// 客户端的协议相关请求头（小写键）。
    pub client_headers: &'a HashMap<String, String>,
    pub identity_mode: IdentityMode,
}

pub struct Prepared {
    pub body: Json,
    /// 从客户端转来的、已按账号收敛的头。
    pub headers: HashMap<String, String>,
    /// 工具别名 → 原名。
    pub tool_aliases: HashMap<String, String>,
    /// 出站的模型基础名。
    pub model: String,
}

fn upstream_model(model: &str) -> (String, Option<&'static str>) {
    let (name, _) = split_route_prefix(model);
    let (base, effort) = split_effort_suffix(name);
    let slug = canonicalize_model(base);
    (slug, effort)
}

fn canonicalize_model(base: &str) -> String {
    let lower = base.trim().to_ascii_lowercase();
    CODEX_ALIASES
        .iter()
        .find(|(from, _)| *from == lower)
        .map(|(_, to)| (*to).to_string())
        .unwrap_or_else(|| base.to_string())
}

/// Responses 入站 → Codex 请求体。原样保留客户端的一切，只改上游会拒收的地方，以及把身份标识
/// 换成这个账号的面孔。这是给 Codex CLI 走的路。
pub fn build_passthrough_body(raw: &Json, opts: &PrepareOptions<'_>) -> Prepared {
    let mut body = raw.clone();
    let (model, suffix_effort) = upstream_model(opts.model);
    body.insert("model".into(), Value::String(model.clone()));
    body.insert("stream".into(), Value::Bool(true));
    body.insert("store".into(), Value::Bool(false));
    for k in REJECTED_FIELDS {
        body.remove(*k);
    }
    if let Some(prompt) = body.remove("prompt") {
        if body.get("input").is_none_or(Value::is_null) {
            body.insert("input".into(), prompt);
        }
    }
    let input = sanitize_input_items(body.get("input"));
    body.insert("input".into(), Value::Array(input));
    if body
        .get("instructions")
        .and_then(|v| v.as_str())
        .is_none_or(|s| s.trim().is_empty())
    {
        body.insert(
            "instructions".into(),
            Value::String(DEFAULT_INSTRUCTIONS.into()),
        );
    }
    if let Some(effort) = to_effort(suffix_effort) {
        let mut reasoning = match body.get("reasoning") {
            Some(Value::Object(o)) => o.clone(),
            _ => Json::new(),
        };
        if reasoning
            .get("effort")
            .and_then(|v| v.as_str())
            .is_none_or(|s| s.is_empty())
        {
            reasoning.insert("effort".into(), Value::String(effort.into()));
        }
        body.insert("reasoning".into(), Value::Object(reasoning));
    }
    let has_tools = body
        .get("tools")
        .and_then(|t| t.as_array())
        .is_some_and(|a| !a.is_empty());
    // 显式的 false 要留着：Codex 的 Responses-Lite 头要求它为 false，删掉上游就按默认 true 拒收。
    if !has_tools && body.get("parallel_tool_calls") != Some(&Value::Bool(false)) {
        body.remove("parallel_tool_calls");
    }
    let tool_aliases = alias_reserved_tool_names(&mut body);
    let cache_key = safe_cache_key(body.get("prompt_cache_key"))
        .unwrap_or_else(|| session_uuid(opts.namespace, opts.session_key));
    body.insert("prompt_cache_key".into(), Value::String(cache_key));
    let from_body = apply_identity_scope(&mut body, opts.namespace, opts.identity_mode);
    let headers = forward_client_headers(
        opts.client_headers,
        opts.namespace,
        from_body,
        opts.identity_mode,
    );
    clamp_unsupported_effort(&mut body);
    Prepared {
        body,
        headers,
        tool_aliases,
        model,
    }
}

/// 目录里写着 `ultra`，推理接口却不认，原话是只支持到 `max`。
/// ChatGPT 桌面端把档位存成 ultra 时，每个请求都会 400。
fn clamp_unsupported_effort(body: &mut Json) {
    let Some(Value::Object(reasoning)) = body.get_mut("reasoning") else {
        return;
    };
    let Some(effort) = reasoning.get("effort").and_then(|v| v.as_str()) else {
        return;
    };
    if effort.eq_ignore_ascii_case("ultra") {
        reasoning.insert("effort".into(), Value::String("max".into()));
    }
}

/// 中间表示 → Codex 请求体。给 OpenAI SDK、Claude Code 这类不讲 Responses 的客户端。
/// system 全部并进 instructions；采样参数一律不带（上游不收）；图片按 input_image 内联。
pub fn build_bridge_body(request: &ChatRequest, opts: &PrepareOptions<'_>) -> Prepared {
    let (model, suffix_effort) = upstream_model(opts.model);
    let mut system: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for m in &request.messages {
        match m.role {
            Role::System => {
                if !m.text.trim().is_empty() {
                    system.push(m.text.clone());
                }
            }
            Role::Tool => {
                for r in &m.tool_results {
                    let id = if r.tool_call_id.is_empty() {
                        "call_0"
                    } else {
                        &r.tool_call_id
                    };
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": fit_call_id(id, "function_call_output"),
                        "output": if r.is_error { format!("[error] {}", r.text) } else { r.text.clone() },
                    }));
                }
            }
            Role::Assistant => {
                if !m.text.trim().is_empty() {
                    input.push(json!({
                        "type": "message", "role": "assistant",
                        "content": [{ "type": "output_text", "text": m.text }],
                    }));
                }
                for c in &m.tool_calls {
                    let id = if c.id.is_empty() { "call_0" } else { &c.id };
                    input.push(json!({
                        "type": "function_call",
                        "call_id": fit_call_id(id, "function_call"),
                        "name": if c.name.is_empty() { "tool" } else { &c.name },
                        "arguments": if c.arguments.is_empty() { "{}" } else { &c.arguments },
                    }));
                }
            }
            Role::User => {
                let mut content: Vec<Value> = Vec::new();
                if !m.text.trim().is_empty() || m.images.is_empty() {
                    content.push(json!({ "type": "input_text", "text": m.text }));
                }
                for img in &m.images {
                    content.push(json!({
                        "type": "input_image",
                        "image_url": format!("data:{};base64,{}", img.mime_type, img.data),
                        "detail": "auto",
                    }));
                }
                input.push(json!({ "type": "message", "role": "user", "content": content }));
            }
        }
    }
    let instructions = {
        let joined = system.join("\n\n").trim().to_string();
        if joined.is_empty() {
            DEFAULT_INSTRUCTIONS.to_string()
        } else {
            joined
        }
    };
    let mut body = Json::new();
    body.insert("model".into(), Value::String(model.clone()));
    body.insert("instructions".into(), Value::String(instructions));
    body.insert("input".into(), Value::Array(input));
    body.insert("stream".into(), Value::Bool(true));
    body.insert("store".into(), Value::Bool(false));
    body.insert(
        "prompt_cache_key".into(),
        Value::String(session_uuid(opts.namespace, opts.session_key)),
    );
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                    "strict": false,
                })
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        use crate::normalized::ToolChoice;
        let choice = match &request.tool_choice {
            ToolChoice::Auto => Value::String("auto".into()),
            ToolChoice::None => Value::String("none".into()),
            ToolChoice::Required => Value::String("required".into()),
            ToolChoice::Tool(name) => json!({ "type": "function", "name": name }),
        };
        body.insert("tool_choice".into(), choice);
    }
    // summary 有两个用处：客户端能看到思考，而且深档位思考几分钟不出正文时摘要增量让连接一直
    // 有字节流动，不被中间设备掐掉。
    if let Some(effort) = to_effort(suffix_effort) {
        body.insert(
            "reasoning".into(),
            json!({ "effort": effort, "summary": "auto" }),
        );
    }
    let tool_aliases = alias_reserved_tool_names(&mut body);
    let headers = forward_client_headers(
        &HashMap::new(),
        opts.namespace,
        HashMap::new(),
        opts.identity_mode,
    );
    clamp_unsupported_effort(&mut body);
    Prepared {
        body,
        headers,
        tool_aliases,
        model,
    }
}

// ---------------------------------------------------------------------------
// 被拒之后修一下再发
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathKey {
    Key(String),
    Index(usize),
}

/// `input[3].namespace` → [input, 3, namespace]。中间夹着不认识的字符就别猜。
fn parse_path(param: &str) -> Option<Vec<PathKey>> {
    let cleaned = param.trim().trim_start_matches("body.");
    let mut tokens = Vec::new();
    let mut chars = cleaned.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c == '.' {
            chars.next();
        } else if c == '[' {
            chars.next();
            let mut digits = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_digit() {
                    digits.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            if chars.next() != Some(']') || digits.is_empty() {
                return None;
            }
            tokens.push(PathKey::Index(digits.parse().ok()?));
        } else if c.is_ascii_alphabetic() || c == '_' {
            let mut ident = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_ascii_alphanumeric() || d == '_' || d == '-' {
                    ident.push(d);
                    chars.next();
                } else {
                    break;
                }
            }
            tokens.push(PathKey::Key(ident));
        } else {
            return None;
        }
    }
    (!tokens.is_empty()).then_some(tokens)
}

fn get_parent<'a>(root: &'a mut Value, path: &[PathKey]) -> Option<&'a mut Value> {
    let mut cur = root;
    for k in &path[..path.len() - 1] {
        cur = match (k, cur) {
            (PathKey::Index(i), Value::Array(a)) => a.get_mut(*i)?,
            (PathKey::Key(k), Value::Object(o)) => o.get_mut(k)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn leaf_exists(parent: &Value, leaf: &PathKey) -> bool {
    match (leaf, parent) {
        (PathKey::Index(i), Value::Array(a)) => *i < a.len(),
        (PathKey::Key(k), Value::Object(o)) => o.contains_key(k),
        _ => false,
    }
}

fn delete_leaf(parent: &mut Value, leaf: &PathKey) -> bool {
    match (leaf, parent) {
        (PathKey::Index(i), Value::Array(a)) if *i < a.len() => {
            a.remove(*i);
            true
        }
        (PathKey::Key(k), Value::Object(o)) => o.remove(k).is_some(),
        _ => false,
    }
}

fn param_from_message(message: &str) -> Option<String> {
    // "Unknown parameter: 'x'" / "unsupported parameter x" / "parameter 'input[3].foo'"
    let lower = message.to_ascii_lowercase();
    let idx = ["parameter", "field", "property", "key"]
        .iter()
        .filter_map(|w| lower.find(w).map(|i| i + w.len()))
        .min()?;
    let rest = &message[idx..];
    let rest = rest.trim_start_matches(|c: char| {
        c == 's' || c == ':' || c == '=' || c.is_whitespace() || c == '-'
    });
    let rest = rest
        .trim_start_matches("is ")
        .trim_start_matches("for ")
        .trim_start();
    let rest = rest.trim_start_matches(['\'', '"', '`']);
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '[' | ']' | '-'))
        .collect();
    (name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_'))
    .then_some(name)
}

pub struct Repair {
    pub body: Json,
    pub reason: String,
}

/// 上游 400 指着某个字段不认，就把它拿掉再发一次。认不出、或字段本来就不在请求里，返回 `None`。
pub fn repair_rejected_request(body: &Json, status: u16, response_text: &str) -> Option<Repair> {
    if status != 400 || response_text.is_empty() {
        return None;
    }
    let parsed: Value = serde_json::from_str(response_text).ok()?;
    let err = match parsed.get("error") {
        Some(Value::Object(e)) => e.clone(),
        _ => parsed.as_object()?.clone(),
    };
    let code = err
        .get("code")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let message = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("")
        .to_string();
    let param = err
        .get("param")
        .and_then(|p| p.as_str())
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .or_else(|| param_from_message(&message))?;
    let path = parse_path(&param)?;
    let mut next = Value::Object(body.clone());
    let leaf = path.last()?.clone();

    // reasoning.effort：降一档而不是删。
    if path.len() == 2
        && path[0] == PathKey::Key("reasoning".into())
        && leaf == PathKey::Key("effort".into())
    {
        let parent = get_parent(&mut next, &path)?;
        let Value::Object(reasoning) = parent else {
            return None;
        };
        let cur = reasoning
            .get("effort")
            .and_then(|e| e.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let ladder: Vec<&str> = EFFORTS
            .iter()
            .copied()
            .filter(|e| *e != "none" && *e != "minimal")
            .collect();
        if let Some(at) = ladder.iter().position(|e| *e == cur) {
            if at > 0 {
                reasoning.insert("effort".into(), Value::String(ladder[at - 1].into()));
                let reason = format!("reasoning.effort {cur} → {}", ladder[at - 1]);
                return Some(Repair {
                    body: next.as_object()?.clone(),
                    reason,
                });
            }
        }
        reasoning.remove("effort");
        return Some(Repair {
            body: next.as_object()?.clone(),
            reason: format!(
                "删除 reasoning.effort（{}）",
                if cur.is_empty() { "?" } else { &cur }
            ),
        });
    }

    // input[N].status：同类型全删。
    if path.len() == 3
        && path[0] == PathKey::Key("input".into())
        && leaf == PathKey::Key("status".into())
    {
        if let PathKey::Index(n) = path[1] {
            let items = next.get_mut("input").and_then(|v| v.as_array_mut())?;
            let ty = items.get(n).and_then(|it| it.get("type")).cloned();
            let mut cleared = 0;
            for it in items.iter_mut() {
                let Value::Object(o) = it else { continue };
                if (ty.is_none() || o.get("type") == ty.as_ref()) && o.remove("status").is_some() {
                    cleared += 1;
                }
            }
            if cleared == 0 {
                return None;
            }
            let label = ty
                .as_ref()
                .and_then(|t| t.as_str())
                .unwrap_or("input")
                .to_string();
            return Some(Repair {
                body: next.as_object()?.clone(),
                reason: format!("删除 {cleared} 个 {label} 项的 status"),
            });
        }
    }

    let parent = get_parent(&mut next, &path)?;
    if !leaf_exists(parent, &leaf) {
        return None;
    }

    // input[N].content 为 null。
    if path.len() == 3
        && path[0] == PathKey::Key("input".into())
        && leaf == PathKey::Key("content".into())
    {
        if let Value::Object(o) = parent {
            if o.get("content").is_some_and(Value::is_null) {
                if o.get("type").and_then(|t| t.as_str()) == Some("reasoning") {
                    o.remove("content");
                } else {
                    o.insert("content".into(), Value::String(String::new()));
                }
                return Some(Repair {
                    body: next.as_object()?.clone(),
                    reason: format!("修正 {param} 的 null content"),
                });
            }
        }
    }

    // tools[N].parameters 缺 type。
    if leaf == PathKey::Key("parameters".into()) && message.to_ascii_lowercase().contains("type") {
        if let Value::Object(o) = parent {
            if let Some(Value::Object(p)) = o.get_mut("parameters") {
                if !p.contains_key("type") {
                    p.insert("type".into(), Value::String("object".into()));
                    return Some(Repair {
                        body: next.as_object()?.clone(),
                        reason: format!("补齐 {param}.type = object"),
                    });
                }
            }
        }
    }

    if !delete_leaf(parent, &leaf) {
        return None;
    }
    Some(Repair {
        body: next.as_object()?.clone(),
        reason: format!(
            "删除上游不认的 {param}{}",
            if code.is_empty() {
                String::new()
            } else {
                format!("（{code}）")
            }
        ),
    })
}

/// 回带了别的账号签的 reasoning 凭据：上游只会说签名无效。
pub fn is_invalid_reasoning_signature(status: u16, response_text: &str) -> bool {
    if status != 400 {
        return false;
    }
    let t = response_text.to_ascii_lowercase();
    t.contains("invalid signature in thinking block")
        || t.contains("invalid_encrypted_content")
        || t.contains("encrypted_content")
}

/// 剔掉所有 reasoning 项的回放凭据（只留摘要），换号后重发用。返回是否改了什么。
pub fn strip_encrypted_reasoning(body: &mut Json) -> bool {
    let mut changed = false;
    if let Some(Value::Array(input)) = body.get_mut("input") {
        for it in input.iter_mut() {
            let Value::Object(o) = it else { continue };
            if o.get("type").and_then(|t| t.as_str()) == Some("reasoning")
                && o.remove("encrypted_content").is_some()
            {
                changed = true;
            }
        }
    }
    changed
}

// ---------------------------------------------------------------------------
// 错误分类
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub kind: UpstreamKind,
    pub message: String,
    /// 上游明确告知的恢复时刻（Unix 毫秒）。
    pub reset_at_ms: Option<i64>,
}

struct ErrorBody {
    code: String,
    kind: String,
    message: String,
    resets_in: Option<f64>,
    resets_at: Option<f64>,
}

fn read_error(body: &str) -> ErrorBody {
    let head: String = body.chars().take(300).collect();
    let Ok(parsed) = serde_json::from_str::<Value>(body) else {
        return ErrorBody {
            code: String::new(),
            kind: String::new(),
            message: head,
            resets_in: None,
            resets_at: None,
        };
    };
    let e = match parsed.get("error") {
        Some(Value::Object(o)) => Value::Object(o.clone()),
        _ => match parsed.get("detail") {
            Some(Value::Object(o)) => Value::Object(o.clone()),
            _ => parsed.clone(),
        },
    };
    let s = |k: &str| e.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let n = |k: &str| {
        e.get(k)
            .and_then(|v| v.as_f64())
            .filter(|n| n.is_finite() && *n > 0.0)
    };
    let mut message = s("message");
    if message.is_empty() {
        if let Some(d) = parsed.get("detail").and_then(|d| d.as_str()) {
            message = d.to_string();
        }
    }
    if message.is_empty() {
        message = head;
    }
    ErrorBody {
        code: s("code"),
        kind: s("type"),
        message,
        resets_in: n("resets_in_seconds").or_else(|| n("reset_after_seconds")),
        resets_at: n("resets_at").or_else(|| n("reset_at")),
    }
}

fn looks_like_web_page(body: &str) -> bool {
    let head: String = body
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_lowercase();
    [
        "<!doctype html",
        "<html",
        "<head>",
        "<title>",
        "cloudflare",
        "just a moment",
    ]
    .iter()
    .any(|m| head.contains(m))
}

fn is_access_state_code(code: &str) -> bool {
    let c = code.to_ascii_lowercase();
    if c == "deactivated_workspace" {
        return true;
    }
    let subjects = ["workspace", "account", "organization", "org"];
    let states = ["deactivated", "disabled", "suspended"];
    subjects.iter().any(|s| {
        states
            .iter()
            .any(|st| c == format!("{s}_{st}") || c == format!("{st}_{s}"))
    })
}

fn is_transient_400(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("an error occurred while processing your request")
        || m.contains("selected model is at capacity")
        || m.contains("model is at capacity")
        || m.contains("you can retry your request")
}

fn usage_limited(signals: &str) -> bool {
    let s = signals.to_ascii_lowercase();
    [
        "usage_limit",
        "usage limit",
        "usage_not_included",
        "usage not included",
        "insufficient_quota",
        "insufficient quota",
        "quota",
        "plan does not include",
        "upgrade your plan",
    ]
    .iter()
    .any(|m| s.contains(m))
}

fn header_num(headers: &reqwest::header::HeaderMap, name: &str) -> Option<f64> {
    headers
        .get(name)?
        .to_str()
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite())
}

/// HTTP 错误 → 分类。429 两种含义处置相反：`usage_limit_reached` 是窗口用满（带重置时刻），
/// `rate_limit_exceeded` 是瞬时限速。两个都不判 quota——这条通道上 quota 留给「套餐不含」。
pub fn classify_http_error(
    status: u16,
    body: &str,
    headers: &reqwest::header::HeaderMap,
    now_ms: i64,
) -> Verdict {
    let e = read_error(body);
    let detail = if e.message.is_empty() {
        format!("HTTP {status}")
    } else {
        e.message.clone()
    };
    let signals = format!("{} {} {}", e.code, e.kind, e.message);
    let reset_from_body = e
        .resets_in
        .map(|s| now_ms + (s * 1000.0) as i64)
        .or_else(|| e.resets_at.map(|s| (s * 1000.0) as i64));
    let reset_from_headers = {
        let full = |side: &str| {
            header_num(headers, &format!("x-codex-{side}-used-percent")).is_some_and(|p| p >= 100.0)
        };
        let reset = |side: &str| {
            header_num(headers, &format!("x-codex-{side}-reset-after-seconds"))
                .filter(|s| *s > 0.0)
                .map(|s| now_ms + (s * 1000.0) as i64)
        };
        if full("primary") {
            reset("primary")
        } else if full("secondary") {
            reset("secondary")
        } else {
            None
        }
    };
    let retry_after = header_num(headers, "retry-after").map(|s| now_ms + (s * 1000.0) as i64);
    let verdict = |kind: UpstreamKind, message: String| Verdict {
        kind,
        message,
        reset_at_ms: None,
    };

    if is_access_state_code(&e.code) {
        return verdict(
            UpstreamKind::Auth,
            format!("账号或工作区已被停用：{detail}"),
        );
    }
    match status {
        401 => verdict(UpstreamKind::Auth, format!("上游拒绝凭证：{detail}")),
        403 => {
            if looks_like_web_page(body) {
                verdict(
                    UpstreamKind::Upstream,
                    "出口 IP 被上游前置的 WAF 挑战拦下（403 HTML）".into(),
                )
            } else if usage_limited(&signals) {
                verdict(UpstreamKind::Quota, format!("套餐不含此能力：{detail}"))
            } else {
                verdict(UpstreamKind::Forbidden, format!("上游 403：{detail}"))
            }
        }
        429 => {
            let s = signals.to_ascii_lowercase();
            if s.contains("usage_not_included")
                || s.contains("plan does not include")
                || s.contains("upgrade your plan")
            {
                return verdict(UpstreamKind::Quota, format!("套餐不含此能力：{detail}"));
            }
            Verdict {
                kind: UpstreamKind::RateLimit,
                message: format!("上游限流：{detail}"),
                reset_at_ms: reset_from_body.or(reset_from_headers).or(retry_after),
            }
        }
        402 => verdict(UpstreamKind::Quota, detail),
        404 => {
            if signals.to_ascii_lowercase().contains("model") {
                verdict(UpstreamKind::ModelUnsupported, detail)
            } else {
                verdict(UpstreamKind::BadRequest, format!("上游 404：{detail}"))
            }
        }
        413 => verdict(UpstreamKind::BadRequest, format!("请求体过大：{detail}")),
        400 | 422 => {
            let c = e.code.to_ascii_lowercase();
            if c.contains("server_is_overloaded")
                || c.contains("slow_down")
                || is_transient_400(&e.message)
            {
                return verdict(UpstreamKind::RateLimit, format!("上游瞬时故障：{detail}"));
            }
            let ck = format!("{} {}", e.code, e.kind).to_ascii_lowercase();
            let m = e.message.to_ascii_lowercase();
            if ck.contains("model")
                && (m.contains("not found")
                    || m.contains("not_found")
                    || m.contains("unsupported")
                    || m.contains("does not exist")
                    || m.contains("invalid"))
            {
                return verdict(UpstreamKind::ModelUnsupported, detail);
            }
            verdict(UpstreamKind::BadRequest, detail)
        }
        s if s >= 500 => {
            if looks_like_web_page(body) {
                verdict(UpstreamKind::Upstream, format!("上游前置网关 {status}"))
            } else if e.code.to_ascii_lowercase().contains("server_is_overloaded")
                || e.code.to_ascii_lowercase().contains("slow_down")
                || is_transient_400(&e.message)
            {
                verdict(UpstreamKind::RateLimit, format!("上游过载：{detail}"))
            } else {
                verdict(UpstreamKind::Provider, format!("上游 {status}：{detail}"))
            }
        }
        _ => verdict(UpstreamKind::Upstream, format!("上游 {status}：{detail}")),
    }
}

/// 流内错误（HTTP 200 之后 SSE 里的 `error` / `response.failed`）→ 分类。
pub fn classify_stream_error(code: &str, message: &str) -> Verdict {
    let c = code.to_ascii_lowercase();
    let text = format!("{c} {}", message.to_ascii_lowercase());
    let msg = if message.is_empty() {
        code.to_string()
    } else {
        message.to_string()
    };
    let v = |kind: UpstreamKind, message: String| Verdict {
        kind,
        message,
        reset_at_ms: None,
    };
    if is_access_state_code(&c) {
        return v(UpstreamKind::Auth, format!("账号或工作区已被停用：{msg}"));
    }
    if c == "server_is_overloaded"
        || c == "slow_down"
        || text.contains("overloaded")
        || text.contains("at capacity")
    {
        return v(UpstreamKind::RateLimit, format!("上游降载：{msg}"));
    }
    if text.contains("usage_limit_reached") || text.contains("rate_limit") {
        return v(UpstreamKind::RateLimit, msg);
    }
    if text.contains("usage_not_included")
        || text.contains("insufficient_quota")
        || text.contains("quota")
    {
        return v(UpstreamKind::Quota, msg);
    }
    if text.contains("unauthorized")
        || text.contains("invalid_token")
        || text.contains("invalid token")
        || text.contains("authentication")
        || text.contains("token has been invalidated")
    {
        return v(UpstreamKind::Auth, msg);
    }
    if text.contains("invalid_signature")
        || text.contains("invalid signature")
        || text.contains("invalid_request")
        || text.contains("invalid request")
        || text.contains("invalid_prompt")
        || text.contains("context_length")
        || text.contains("context length")
        || text.contains("context_window")
        || text.contains("context window")
        || text.contains("too long")
        || text.contains("too many")
    {
        return v(UpstreamKind::BadRequest, msg);
    }
    if is_transient_400(message) {
        return v(UpstreamKind::RateLimit, format!("上游瞬时故障：{message}"));
    }
    v(
        UpstreamKind::Provider,
        if msg.is_empty() {
            "上游在流中报错".into()
        } else {
            msg
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts<'a>(model: &'a str, headers: &'a HashMap<String, String>) -> PrepareOptions<'a> {
        PrepareOptions {
            model,
            session_key: "sess-1",
            namespace: "chatgpt:acct_A",
            client_headers: headers,
            identity_mode: IdentityMode::Scope,
        }
    }

    #[test]
    fn model_names_split_route_prefix_and_effort_suffix() {
        assert_eq!(
            split_effort_suffix("gpt-5.4-high"),
            ("gpt-5.4", Some("high"))
        );
        assert_eq!(
            split_effort_suffix("gpt-5.3-codex-xhigh"),
            ("gpt-5.3-codex", Some("xhigh"))
        );
        assert_eq!(
            split_effort_suffix("gpt-5.6-sol-max"),
            ("gpt-5.6-sol", Some("max"))
        );
        assert_eq!(split_effort_suffix("gpt-5.6-sol"), ("gpt-5.6-sol", None));
        assert_eq!(split_route_prefix("chatgpt/gpt-5.4"), ("gpt-5.4", true));
        assert_eq!(
            split_route_prefix("CODEX/gpt-5.4-high"),
            ("gpt-5.4-high", true)
        );
        assert_eq!(split_route_prefix("gpt-5.4"), ("gpt-5.4", false));
        assert!(is_codex_model("gpt-5.4-high"));
        assert!(is_codex_model("chatgpt/GPT-5.6-sol"));
        assert!(is_codex_model("gpt-6"));
        assert!(is_codex_model("chatgpt/gpt-6-high"));
        assert!(!is_codex_model("claude-sonnet-5"));
        assert!(!is_codex_model("auto"));
        assert_eq!(CODEX_MODELS[0], DEFAULT_CHAT_MODEL);
        assert_eq!(DEFAULT_CHAT_MODEL, "gpt-5.4");
        assert_eq!(PREFERRED_CHAT_MODEL, "gpt-6-astra");
        let aliased = build_passthrough_body(&Map::new(), &opts("gpt-6", &HashMap::new()));
        assert_eq!(aliased.model, "gpt-6-astra");
        assert_eq!(aliased.body["model"], "gpt-6-astra");
        assert_eq!(to_effort(Some("ULTRA")), Some("ultra"));
        assert_eq!(to_effort(Some("bogus")), None);
    }

    #[test]
    fn image_requests_become_a_forced_image_generation_tool_call() {
        assert!(is_codex_image_model("gpt-image-2"));
        assert!(is_codex_image_model("chatgpt/GPT-Image-1.5"));
        assert!(!is_codex_image_model("nano-banana-2"));
        assert!(!is_codex_image_model("gpt-5.4"));

        let b = build_image_body(
            "a red panda, watercolor",
            "chatgpt/gpt-image-2",
            &ImageOptions {
                size: Some("1024x1536"),
                quality: Some("high"),
                background: None,
                output_format: Some(" webp "),
                ..ImageOptions::default()
            },
            "chatgpt:acct_A",
            "image:1",
        );
        assert_eq!(b["model"], IMAGE_MAIN_MODEL, "代调的是便宜的文本模型");
        assert_eq!(
            b["tool_choice"]["type"], "image_generation",
            "强制调工具，不给模型闲聊的机会"
        );
        assert_eq!(b["tools"][0]["type"], "image_generation");
        assert_eq!(b["tools"][0]["model"], "gpt-image-2");
        assert_eq!(b["tools"][0]["action"], "generate");
        assert_eq!(b["tools"][0]["size"], "1024x1536");
        assert_eq!(b["tools"][0]["quality"], "high");
        assert_eq!(b["tools"][0]["output_format"], "webp");
        assert!(b["tools"][0].get("background").is_none(), "没给的不带");
        assert_eq!(
            b["input"][0]["content"][0]["text"],
            "a red panda, watercolor"
        );
        assert_eq!(b["store"], false);
        assert_eq!(b["stream"], true);
        assert!(b["instructions"]
            .as_str()
            .unwrap()
            .contains("exactly as written"));

        // 编辑：参考图跟提示词一起进 input，action 换成 edit，遮罩进工具。
        let refs = vec![
            "data:image/png;base64,AAAA".to_string(),
            "data:image/jpeg;base64,BBBB".to_string(),
        ];
        let e = build_image_body(
            "make it night",
            "gpt-image-2",
            &ImageOptions {
                references: &refs,
                mask: Some("data:image/png;base64,MMMM"),
                ..ImageOptions::default()
            },
            "chatgpt:acct_A",
            "image:2",
        );
        assert_eq!(e["tools"][0]["action"], "edit");
        assert_eq!(
            e["tools"][0]["input_image_mask"]["image_url"],
            "data:image/png;base64,MMMM"
        );
        let content = e["input"][0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 3);
        assert_eq!(content[0]["type"], "input_text");
        assert_eq!(content[1]["type"], "input_image");
        assert_eq!(content[1]["image_url"], "data:image/png;base64,AAAA");
        assert_eq!(content[2]["image_url"], "data:image/jpeg;base64,BBBB");

        assert_eq!(image_mime(Some("webp")), "image/webp");
        assert_eq!(image_mime(Some("JPEG")), "image/jpeg");
        assert_eq!(image_mime(None), "image/png");
        assert_eq!(parse_size(Some("1024x1536")), Some((1024, 1536)));
        assert_eq!(parse_size(Some("auto")), None);
        assert!(looks_like_content_refusal(
            "I can't help with that due to our content policy."
        ));
        assert!(looks_like_content_refusal("这个请求不适合生成图片"));
        assert!(!looks_like_content_refusal(
            "Here is a description of a red panda instead."
        ));
    }

    #[test]
    fn identity_values_are_scoped_per_account_and_keep_uuid_tails() {
        let conv = "7d1e9c1a-1111-4222-8333-444455556666";
        let a = scope_identity_value("chatgpt:A", conv);
        assert_eq!(a, scope_identity_value("chatgpt:A", conv));
        assert_ne!(a, scope_identity_value("chatgpt:B", conv));
        assert_ne!(a, conv);
        assert_eq!(a.len(), 36);
        assert_eq!(
            scope_identity_value("chatgpt:A", &format!("{conv}:0")),
            format!("{a}:0")
        );
        assert_eq!(scope_identity_value("chatgpt:A", &conv.to_uppercase()), a);

        let mut body: Json = json!({
            "prompt_cache_key": conv,
            "client_metadata": {
                "session_id": conv,
                "x-codex-installation-id": "inst-1",
                "x-codex-window-id": format!("{conv}:0"),
                "x-codex-turn-metadata": json!({ "turn_id": "turn-1", "window_id": format!("{conv}:0") }).to_string(),
            }
        })
        .as_object()
        .unwrap()
        .clone();
        let headers = apply_identity_scope(&mut body, "chatgpt:A", IdentityMode::Scope);
        assert_eq!(body["prompt_cache_key"], a);
        assert_eq!(body["client_metadata"]["session_id"], a);
        assert_eq!(
            body["client_metadata"]["x-codex-window-id"],
            format!("{a}:0")
        );
        assert_eq!(headers["x-codex-window-id"], format!("{a}:0"));
        let tm: Value = serde_json::from_str(headers["x-codex-turn-metadata"].as_str()).unwrap();
        assert_eq!(tm["window_id"], format!("{a}:0"));
        assert_ne!(tm["turn_id"], "turn-1");

        // device 模式：设备 id 是账号常量；off 原样。
        let mut dev: Json = json!({ "client_metadata": { "x-codex-installation-id": "dev-1", "session_id": conv } })
            .as_object()
            .unwrap()
            .clone();
        apply_identity_scope(&mut dev, "chatgpt:A", IdentityMode::Device);
        assert_eq!(
            dev["client_metadata"]["x-codex-installation-id"],
            device_id_for("chatgpt:A")
        );
        assert_eq!(dev["client_metadata"]["session_id"], a);
        let mut off: Json = json!({ "client_metadata": { "x-codex-installation-id": "dev-1" } })
            .as_object()
            .unwrap()
            .clone();
        apply_identity_scope(&mut off, "chatgpt:A", IdentityMode::Off);
        assert_eq!(off["client_metadata"]["x-codex-installation-id"], "dev-1");
    }

    #[test]
    fn turn_state_is_tagged_per_account_and_only_the_minting_account_gets_it_back() {
        let tagged = tag_turn_state("chatgpt:A", "blob.with.dots==");
        assert!(tagged.starts_with("nx1."));
        assert_eq!(
            untag_turn_state("chatgpt:A", Some(&tagged)).as_deref(),
            Some("blob.with.dots==")
        );
        assert_eq!(untag_turn_state("chatgpt:B", Some(&tagged)), None);
        assert_eq!(untag_turn_state("chatgpt:A", Some("raw-blob")), None);
        assert_eq!(untag_turn_state("chatgpt:A", None), None);

        let mut h = reqwest::header::HeaderMap::new();
        h.insert("x-codex-turn-state", "st-1".parse().unwrap());
        h.insert("x-codex-primary-used-percent", "12".parse().unwrap());
        h.insert("set-cookie", "no".parse().unwrap());
        let relay = relay_response_headers(&h, "chatgpt:A");
        assert!(relay
            .iter()
            .any(|(k, v)| k == "x-codex-turn-state" && *v == tag_turn_state("chatgpt:A", "st-1")));
        assert!(relay
            .iter()
            .any(|(k, v)| k == "x-codex-primary-used-percent" && v == "12"));
        assert!(!relay.iter().any(|(k, _)| k == "set-cookie"));

        let mut client = HashMap::new();
        client.insert("x-codex-turn-state".to_string(), relay[0].1.clone());
        let fwd = forward_client_headers(&client, "chatgpt:A", HashMap::new(), IdentityMode::Scope);
        assert_eq!(fwd["x-codex-turn-state"], "st-1");
        assert_eq!(
            fwd["x-codex-beta-features"], "remote_compaction_v2",
            "没声明就补默认"
        );
    }

    #[test]
    fn passthrough_body_keeps_the_client_shape_and_fixes_only_what_upstream_rejects() {
        let raw: Json = json!({
            "model": "gpt-5.4-high",
            "stream": false,
            "store": true,
            "temperature": 0.2,
            "max_output_tokens": 100,
            "instructions": "",
            "input": [
                { "id": "msg_1", "type": "message", "role": "system", "content": "sys" },
                { "id": "rs_1", "type": "reasoning", "encrypted_content": "gAAAAABq", "summary": null },
                { "id": "rs_2", "type": "reasoning", "encrypted_content": "not-a-blob" },
                { "type": "item_reference", "id": "ref" },
                { "type": "function_call", "id": "fc_1", "name": "python", "arguments": "{}" },
                { "type": "function_call_output", "id": "fco_1", "output": "ok" },
            ],
            "tools": [{ "type": "function", "name": "Python", "parameters": {} }],
            "tool_choice": { "type": "function", "name": "python" },
            "include": ["reasoning.encrypted_content"],
            "service_tier": "priority",
            "personality": "friendly",
        })
        .as_object()
        .unwrap()
        .clone();
        let p = build_passthrough_body(&raw, &opts("chatgpt/gpt-5.4-high", &HashMap::new()));
        let b = &p.body;
        assert_eq!(b["model"], "gpt-5.4");
        assert_eq!(b["stream"], true);
        assert_eq!(b["store"], false);
        assert!(b.get("temperature").is_none() && b.get("max_output_tokens").is_none());
        assert!(
            b.get("personality").is_none(),
            "桌面端的 personality 上游不认"
        );
        let ultra: Json = json!({
            "model": "gpt-6-astra",
            "instructions": "x",
            "input": [],
            "reasoning": { "effort": "ultra" },
            "personality": "friendly",
        })
        .as_object()
        .unwrap()
        .clone();
        let clamped = build_passthrough_body(&ultra, &opts("gpt-6-astra", &HashMap::new()));
        assert_eq!(clamped.body["reasoning"]["effort"], "max");
        assert!(clamped.body.get("personality").is_none());
        assert_eq!(b["service_tier"], "priority", "Fast 模式保留");
        assert_eq!(b["include"][0], "reasoning.encrypted_content");
        assert_eq!(b["instructions"], DEFAULT_INSTRUCTIONS);
        assert_eq!(b["reasoning"]["effort"], "high");
        let input = b["input"].as_array().unwrap();
        assert_eq!(input.len(), 5, "item_reference 丢掉");
        assert!(input.iter().all(|i| i.get("id").is_none()), "所有 id 去掉");
        assert_eq!(input[0]["role"], "developer");
        assert_eq!(input[1]["summary"], json!([]));
        assert_eq!(input[1]["encrypted_content"], "gAAAAABq");
        assert!(
            input[2].get("encrypted_content").is_none(),
            "不像 Fernet 的凭据剔掉"
        );
        assert_eq!(input[3]["name"], "python__nexus");
        assert_eq!(input[3]["call_id"], "fc_1", "缺 call_id 拿 id 顶上");
        assert_eq!(input[4]["call_id"], "fco_1");
        assert_eq!(b["tools"][0]["name"], "python__nexus");
        assert_eq!(b["tool_choice"]["name"], "python__nexus");
        assert_eq!(p.tool_aliases["python__nexus"], "Python");
        assert_eq!(
            restore_tool_name("python__nexus", &p.tool_aliases),
            "Python"
        );
        // 没带就按会话键生成，再和其他标识一样按账号换一次脸（同账号同会话恒等）。
        let expected_key =
            scope_identity_value("chatgpt:acct_A", &session_uuid("chatgpt:acct_A", "sess-1"));
        assert_eq!(b["prompt_cache_key"], expected_key);
        let again = build_passthrough_body(&raw, &opts("chatgpt/gpt-5.4-high", &HashMap::new()));
        assert_eq!(again.body["prompt_cache_key"], expected_key, "确定性");
        assert_eq!(p.headers["x-codex-beta-features"], "remote_compaction_v2");
        assert!(p.body.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn passthrough_keeps_explicit_false_parallel_tool_calls_without_tools() {
        let raw = json!({ "input": "hi", "parallel_tool_calls": false })
            .as_object()
            .unwrap()
            .clone();
        let p = build_passthrough_body(&raw, &opts("gpt-6-astra", &HashMap::new()));
        assert_eq!(p.body["parallel_tool_calls"], false);

        let raw = json!({ "input": "hi", "parallel_tool_calls": true })
            .as_object()
            .unwrap()
            .clone();
        let p = build_passthrough_body(&raw, &opts("gpt-6-astra", &HashMap::new()));
        assert!(p.body.get("parallel_tool_calls").is_none());
    }

    #[test]
    fn bridge_body_folds_system_into_instructions_and_maps_tools() {
        use crate::normalized::{Message, ToolCall, ToolChoice, ToolDef, ToolResult};
        let req = ChatRequest {
            model: "gpt-5.4".into(),
            messages: vec![
                Message::text(Role::System, "be terse"),
                Message::text(Role::User, "read a"),
                Message {
                    role: Role::Assistant,
                    text: String::new(),
                    images: vec![],
                    tool_calls: vec![ToolCall {
                        id: "call_1".into(),
                        name: "read".into(),
                        arguments: r#"{"p":"a"}"#.into(),
                    }],
                    tool_results: vec![],
                },
                Message {
                    role: Role::Tool,
                    text: String::new(),
                    images: vec![],
                    tool_calls: vec![],
                    tool_results: vec![ToolResult {
                        tool_call_id: "call_1".into(),
                        tool_name: "read".into(),
                        text: "body".into(),
                        is_error: true,
                    }],
                },
            ],
            tools: vec![ToolDef {
                name: "read".into(),
                description: "d".into(),
                parameters: json!({ "type": "object" }),
                grammar: false,
                namespace: None,
            }],
            tool_choice: ToolChoice::Tool("read".into()),
            sampling: Default::default(),
            conversation_id: Some("c".into()),
            ..Default::default()
        };
        let p = build_bridge_body(&req, &opts("gpt-5.4-xhigh", &HashMap::new()));
        let b = &p.body;
        assert_eq!(b["model"], "gpt-5.4");
        assert_eq!(b["instructions"], "be terse");
        let input = b["input"].as_array().unwrap();
        assert_eq!(input[0]["role"], "user");
        assert_eq!(input[0]["content"][0]["text"], "read a");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["output"], "[error] body");
        assert_eq!(b["tools"][0]["name"], "read");
        assert_eq!(b["tool_choice"]["name"], "read");
        assert_eq!(b["reasoning"]["effort"], "xhigh");
        assert_eq!(b["reasoning"]["summary"], "auto");
        assert!(b.get("temperature").is_none());
        assert_eq!(b["store"], false);
    }

    #[test]
    fn rejected_requests_are_repaired_by_what_the_upstream_named() {
        let body: Json = json!({
            "reasoning": { "effort": "max" },
            "input": [
                { "type": "message", "role": "user", "content": "x", "status": "completed" },
                { "type": "message", "role": "assistant", "content": null, "status": "completed" },
                { "type": "reasoning", "summary": [], "status": "completed" },
            ],
            "tools": [{ "type": "function", "name": "t", "parameters": { "properties": {} } }],
            "safety_identifier": "x",
        })
        .as_object()
        .unwrap()
        .clone();
        let err = |param: &str, msg: &str| {
            json!({ "error": { "code": "unknown_parameter", "param": param, "message": msg } })
                .to_string()
        };

        let r =
            repair_rejected_request(&body, 400, &err("reasoning.effort", "unsupported")).unwrap();
        assert_eq!(r.body["reasoning"]["effort"], "xhigh");
        assert!(r.reason.contains("max → xhigh"));

        let r = repair_rejected_request(&body, 400, &err("input[0].status", "unknown")).unwrap();
        assert!(r.body["input"][0].get("status").is_none());
        assert!(r.body["input"][1].get("status").is_none(), "同类型全删");
        assert!(r.body["input"][2].get("status").is_some(), "别的类型不动");

        let r = repair_rejected_request(&body, 400, &err("input[1].content", "null")).unwrap();
        assert_eq!(r.body["input"][1]["content"], "");

        let r = repair_rejected_request(
            &body,
            400,
            &err("tools[0].parameters", "Missing required parameter type"),
        )
        .unwrap();
        assert_eq!(r.body["tools"][0]["parameters"]["type"], "object");

        let r = repair_rejected_request(&body, 400, &err("safety_identifier", "Unknown parameter"))
            .unwrap();
        assert!(r.body.get("safety_identifier").is_none());

        // message 里点名、没有 param 字段。
        let r = repair_rejected_request(
            &body,
            400,
            &json!({ "error": { "message": "Unknown parameter: 'safety_identifier'" } })
                .to_string(),
        )
        .unwrap();
        assert!(r.body.get("safety_identifier").is_none());

        assert!(
            repair_rejected_request(&body, 400, &err("nope", "x")).is_none(),
            "字段不在请求里"
        );
        assert!(repair_rejected_request(&body, 401, &err("safety_identifier", "x")).is_none());
        assert!(repair_rejected_request(&body, 400, "not json").is_none());

        assert!(is_invalid_reasoning_signature(
            400,
            r#"{"error":{"message":"Invalid signature in thinking block"}}"#
        ));
        let mut stripped = body.clone();
        stripped["input"][2]["encrypted_content"] = Value::String("gAAAA".into());
        assert!(strip_encrypted_reasoning(&mut stripped));
        assert!(stripped["input"][2].get("encrypted_content").is_none());
        assert!(!strip_encrypted_reasoning(&mut stripped));
    }

    #[test]
    fn http_errors_are_classified_by_what_they_mean_for_the_account() {
        let h = reqwest::header::HeaderMap::new();
        let now = 1_700_000_000_000;
        let c = |status: u16, body: &str| classify_http_error(status, body, &h, now);
        assert_eq!(c(401, r#"{"detail":"nope"}"#).kind, UpstreamKind::Auth);
        assert_eq!(
            c(403, "<!DOCTYPE html><title>Just a moment</title>").kind,
            UpstreamKind::Upstream
        );
        assert_eq!(
            c(
                403,
                r#"{"error":{"message":"your plan does not include codex"}}"#
            )
            .kind,
            UpstreamKind::Quota
        );
        assert_eq!(
            c(403, r#"{"error":{"message":"forbidden"}}"#).kind,
            UpstreamKind::Forbidden
        );
        let limited = c(
            429,
            r#"{"error":{"type":"usage_limit_reached","message":"limit","resets_in_seconds":600}}"#,
        );
        assert_eq!(limited.kind, UpstreamKind::RateLimit);
        assert_eq!(limited.reset_at_ms, Some(now + 600_000));
        assert_eq!(
            c(
                429,
                r#"{"error":{"code":"usage_not_included","message":"x"}}"#
            )
            .kind,
            UpstreamKind::Quota
        );
        assert_eq!(c(402, "{}").kind, UpstreamKind::Quota);
        assert_eq!(
            c(404, r#"{"error":{"message":"model not found"}}"#).kind,
            UpstreamKind::ModelUnsupported
        );
        assert_eq!(
            c(404, r#"{"error":{"message":"not found"}}"#).kind,
            UpstreamKind::BadRequest
        );
        assert_eq!(
            c(
                400,
                r#"{"error":{"message":"An error occurred while processing your request"}}"#
            )
            .kind,
            UpstreamKind::RateLimit
        );
        assert_eq!(
            c(400, r#"{"error":{"param":"x","message":"bad"}}"#).kind,
            UpstreamKind::BadRequest
        );
        assert!(c(
            401,
            r#"{"error":{"code":"account_deactivated","message":"x"}}"#
        )
        .message
        .contains("停用"));
        assert_eq!(
            c(503, r#"{"error":{"message":"boom"}}"#).kind,
            UpstreamKind::Provider
        );
        assert_eq!(
            c(502, "<html>bad gateway</html>").kind,
            UpstreamKind::Upstream
        );

        let mut hq = reqwest::header::HeaderMap::new();
        hq.insert("x-codex-primary-used-percent", "100".parse().unwrap());
        hq.insert(
            "x-codex-primary-reset-after-seconds",
            "300".parse().unwrap(),
        );
        let v = classify_http_error(429, "{}", &hq, now);
        assert_eq!(v.reset_at_ms, Some(now + 300_000), "body 没说就看额度头");

        assert_eq!(
            classify_stream_error("server_is_overloaded", "").kind,
            UpstreamKind::RateLimit
        );
        assert_eq!(
            classify_stream_error("usage_limit_reached", "x").kind,
            UpstreamKind::RateLimit
        );
        assert_eq!(
            classify_stream_error("invalid_signature", "x").kind,
            UpstreamKind::BadRequest
        );
        assert_eq!(
            classify_stream_error("", "token has been invalidated").kind,
            UpstreamKind::Auth
        );
        assert_eq!(
            classify_stream_error("weird", "boom").kind,
            UpstreamKind::Provider
        );
    }
}
