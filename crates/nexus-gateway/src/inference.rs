//! `aiserver.v1.InferenceService/Stream` 客户端：请求构造、流驱动、错误映射。
//!
//! 逐条移植 `protocol.js` 的 `streamCursorChat` / `toInferenceMessages` / `planToolChoice` /
//! `planTextChunk` / `mapUpstreamError` / `mapInferenceError`。这条路是**无状态纯推理**：
//! 每次带完整 messages，模型要调工具就在流里给 `tool_call_part`，我们攒完整交回调用方执行，
//! 下一轮调用方把结果放进 messages 再来——和 OpenAI / Anthropic 的形状一致，所以不需要任何
//! 会话状态，换号也不会失忆。
//!
//! 三处不是 Stream 的缺陷、而是 Cursor 后端行为的妥协，照搬不改：① system 只能折进首条 user
//! （实测发 role=SYSTEM 被 `ERROR_PROVIDER_ERROR` 拒）；② tool_choice 没有字段，`none` 硬保证
//! （不声明工具），`required` / 指定只能软引导；③ stop 序列与输出上限本地兜底执行。

use crate::connect::{self, ConnectError, ConnectFailure, StreamItem};
use crate::error::{UpstreamError, UpstreamKind};
use crate::headers::{ai_headers, RequestNonce};
use crate::identity::DeviceIdentity;
use crate::normalized::{
    estimate_tokens, ChatRequest, Completion, Delta, FinishReason, ImageInput, Message, Role,
    Sampling, ToolCall, ToolChoice, ToolDef, Usage,
};
use crate::proto::inference_content_part::Part;
use crate::proto::inference_core_message::Content;
use crate::proto::inference_stream_response::Response;
use crate::proto::{
    InferenceAgentTool, InferenceContentPart, InferenceContentParts, InferenceCoreMessage,
    InferenceImagePart, InferenceMessageRole, InferenceModelConfig, InferenceRequestedModel,
    InferenceResponseInfo, InferenceStreamError, InferenceStreamErrorType, InferenceStreamRequest,
    InferenceStreamResponse, InferenceTextPart, InferenceToolCall, InferenceToolResultContent,
    InferenceToolResultPart,
};
use prost::Message as _;
use prost_types::value::Kind;
use regex::Regex;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

pub const DEFAULT_BASE_URL: &str = "https://api2.cursor.sh";
pub const STREAM_PATH: &str = "/aiserver.v1.InferenceService/Stream";
/// 空闲超时：上游多久没动静才算死。不是总时长——按总时长掐会把正在稳定出字的长回答一并杀掉。
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(180);
/// 单轮总时长的纯兜底。
pub const DEFAULT_MAX_TURN: Duration = Duration::from_secs(1800);
/// 缺省的额度通道标签。`cli` 是一条普通用户通道；`sand`（bot 额度）是另一个显式的高风险开关，
/// 不在这里预设。
pub const DEFAULT_CLIENT_TYPE: &str = "cli";
/// Cursor 通道的静态清单来自 [`crate::models::CURSOR_MODELS`]；对外报的时候加成 `cursor/…`。
pub use crate::models::CURSOR_MODELS as STATIC_MODELS;

#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub base_url: String,
    pub client_type: String,
    pub idle_timeout: Duration,
    pub max_turn: Duration,
    /// 强制所有请求都发这个上游模型，忽略客户端要什么。
    ///
    /// 给「账号只开得了某一档」的情形：便宜号常常只能跑 `auto`，而 Claude Code 这类客户端
    /// 会**客户端校验模型名**、不认识 `auto`，于是用户没法在客户端里填它。让客户端继续报
    /// 它认识的名字、由网关换成账号真能跑的那个，是唯一能同时满足两边的做法。
    /// 默认关：改写用户要的模型是件该由他自己点头的事。
    pub force_model: Option<String>,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            client_type: DEFAULT_CLIENT_TYPE.into(),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
            force_model: None,
        }
    }
}

/// 网关用的 HTTP 客户端。不设总超时——流可以很长，空闲超时在 [`stream`] 里按分片管。
///
/// 协议跟 Cursor 默认一样走 HTTP/2（`cursor.general.disableHttp2` 未开）。Connect
/// 本就是 protobuf over h2。要避开的不是 Clash，是 **系统 HTTP 代理那条 CONNECT**：
/// reqwest 若走 `127.0.0.1:7897` 再在隧道里谈 h2，长流会占着连接不吐帧。`no_proxy`
/// 之后和 Cursor 一样出网（本机 Clash TUN 仍会拦，那是透明 TLS，h2 正常）。
/// 空闲 PING 给 4.7 那种先默想几十秒的流保活，免得中间设备当死连接切掉。
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .no_proxy()
        .http2_keep_alive_interval(Duration::from_secs(10))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .build()
        .expect("reqwest 客户端初始化（只在 TLS 后端缺失时失败）")
}

// ---------- 请求构造 ----------

/// 客户端要的模型名 → 发给上游的名字。
///
/// 两步：先把别家的名字映射成 Cursor 认识的（`crate::models`，不做的话 Claude Code 直连
/// 必然 `ERROR_BAD_MODEL_NAME`），再把 `auto` / 空翻成上游的 `default`（交给它自选）。
/// 改写过就记一行日志——用户在客户端里看到的仍是自己填的名字，出了偏差要能查到是这里换的。
pub fn requested_model(model: &str) -> String {
    let resolved = crate::models::resolve(model);
    if let Some(note) = &resolved.note {
        tracing::info!(%note, "模型名映射");
    }
    if resolved.upstream == "auto" {
        "default".to_string()
    } else {
        resolved.upstream
    }
}

fn role_of(role: Role) -> i32 {
    match role {
        Role::System => InferenceMessageRole::System as i32,
        Role::User => InferenceMessageRole::User as i32,
        Role::Assistant => InferenceMessageRole::Assistant as i32,
        Role::Tool => InferenceMessageRole::Tool as i32,
    }
}

/// JSON → `google.protobuf.Value`。
pub fn value_from_json(v: &serde_json::Value) -> prost_types::Value {
    let kind = match v {
        serde_json::Value::Null => Kind::NullValue(0),
        serde_json::Value::Bool(b) => Kind::BoolValue(*b),
        serde_json::Value::Number(n) => Kind::NumberValue(n.as_f64().unwrap_or(0.0)),
        serde_json::Value::String(s) => Kind::StringValue(s.clone()),
        serde_json::Value::Array(a) => Kind::ListValue(prost_types::ListValue {
            values: a.iter().map(value_from_json).collect(),
        }),
        serde_json::Value::Object(_) => Kind::StructValue(struct_from_json(v)),
    };
    prost_types::Value { kind: Some(kind) }
}

/// JSON 对象 → `google.protobuf.Struct`。不是对象就退成空 `{}`——让一个坏 schema 退化成空，
/// 而不是打挂整轮。
pub fn struct_from_json(v: &serde_json::Value) -> prost_types::Struct {
    match v {
        serde_json::Value::Object(map) => prost_types::Struct {
            fields: map
                .iter()
                .map(|(k, v)| (k.clone(), value_from_json(v)))
                .collect(),
        },
        _ => prost_types::Struct::default(),
    }
}

fn struct_from_args(args: &str) -> prost_types::Struct {
    let src = if args.trim().is_empty() { "{}" } else { args };
    serde_json::from_str::<serde_json::Value>(src)
        .ok()
        .filter(|v| v.is_object())
        .map(|v| struct_from_json(&v))
        .unwrap_or_default()
}

fn string_value(s: &str) -> prost_types::Value {
    prost_types::Value {
        kind: Some(Kind::StringValue(s.to_string())),
    }
}

/// 统一消息表 → `InferenceCoreMessage[]`。
///
/// user / assistant / tool 一一映射；图片走 `parts`；assistant 的 tool_calls 带结构化 args 和
/// 原文；tool 结果走 `tool_content` 按 id 配对。**system 折进首条 user**（没有 user 时单独补一条
/// user 承载）；`extra_system` 是我们自己追加的指令（tool_choice 软引导），跟在调用方的 system
/// 之后。既无正文 / 图片也无工具调用的空消息不发——上游会拒空 turn。
pub fn to_inference_messages(
    messages: &[Message],
    extra_system: Option<&str>,
) -> Vec<InferenceCoreMessage> {
    let mut system_parts: Vec<&str> = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.text.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if let Some(extra) = extra_system.map(str::trim).filter(|s| !s.is_empty()) {
        system_parts.push(extra);
    }
    let system_text = system_parts.join("\n\n");

    let mut out = Vec::new();
    let mut system_injected = false;
    for msg in messages {
        match msg.role {
            Role::System => continue,
            Role::Tool => {
                let parts: Vec<InferenceToolResultPart> = msg
                    .tool_results
                    .iter()
                    .map(|r| InferenceToolResultPart {
                        tool_call_id: r.tool_call_id.clone(),
                        tool_name: r.tool_name.clone(),
                        result: Some(string_value(&r.text)),
                        is_error: r.is_error,
                        ..Default::default()
                    })
                    .collect();
                if !parts.is_empty() {
                    out.push(InferenceCoreMessage {
                        role: role_of(Role::Tool),
                        content: Some(Content::ToolContent(InferenceToolResultContent { parts })),
                        ..Default::default()
                    });
                }
            }
            Role::User | Role::Assistant => {
                let mut text = msg.text.trim().to_string();
                if !system_injected && msg.role == Role::User && !system_text.is_empty() {
                    text = if text.is_empty() {
                        system_text.clone()
                    } else {
                        format!("{system_text}\n\n{text}")
                    };
                    system_injected = true;
                }
                let images: Vec<&ImageInput> =
                    msg.images.iter().filter(|i| !i.data.is_empty()).collect();
                let tool_calls: Vec<InferenceToolCall> = if msg.role == Role::Assistant {
                    msg.tool_calls
                        .iter()
                        .map(|t| InferenceToolCall {
                            tool_call_id: t.id.clone(),
                            tool_name: t.name.clone(),
                            args: Some(struct_from_args(&t.arguments)),
                            raw_tool_call_args: Some(if t.arguments.is_empty() {
                                "{}".into()
                            } else {
                                t.arguments.clone()
                            }),
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                // content 是 oneof（text | parts | tool_content）：带图走 parts，否则纯 text。
                let content = if !images.is_empty() {
                    let mut parts = Vec::new();
                    if !text.is_empty() {
                        parts.push(InferenceContentPart {
                            part: Some(Part::Text(InferenceTextPart {
                                text: text.clone(),
                                provider_options: None,
                            })),
                        });
                    }
                    for im in images {
                        parts.push(InferenceContentPart {
                            part: Some(Part::Image(InferenceImagePart {
                                data: im.data.clone(),
                                mime_type: Some(if im.mime_type.is_empty() {
                                    "image/png".into()
                                } else {
                                    im.mime_type.clone()
                                }),
                                provider_options: None,
                            })),
                        });
                    }
                    Some(Content::Parts(InferenceContentParts { parts }))
                } else if !text.is_empty() {
                    Some(Content::Text(text))
                } else {
                    None
                };
                if content.is_some() || !tool_calls.is_empty() {
                    out.push(InferenceCoreMessage {
                        role: role_of(msg.role),
                        content,
                        tool_calls,
                        ..Default::default()
                    });
                }
            }
        }
    }
    if !system_text.is_empty() && !system_injected {
        out.insert(
            0,
            InferenceCoreMessage {
                role: role_of(Role::User),
                content: Some(Content::Text(system_text)),
                ..Default::default()
            },
        );
    }
    out
}

const TOOL_CHOICE_REQUIRED_GUIDANCE: &str = "For this turn you MUST call one of the tools provided with this request instead of replying in plain text. Choose the most appropriate tool and supply its arguments.";

fn named_tool_guidance(name: &str) -> String {
    format!("For this turn you MUST call the tool `{name}` instead of replying in plain text. Supply its arguments based on the conversation.")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolPlan {
    /// 不向上游声明任何工具（`none` 的硬保证）。
    pub suppress_tools: bool,
    /// 追加到 system 的软引导（`required` / 指定工具）。
    pub guidance: Option<String>,
}

/// tool_choice：`InferenceStreamRequest` 没有对应字段，能落地的只有两档。没有工具可调时
/// `required` / 指定都无意义——硬塞引导只会让模型去找不存在的工具。
pub fn plan_tool_choice(choice: &ToolChoice, tools: &[ToolDef]) -> ToolPlan {
    let plain = ToolPlan {
        suppress_tools: false,
        guidance: None,
    };
    match choice {
        ToolChoice::None => ToolPlan {
            suppress_tools: true,
            guidance: None,
        },
        _ if tools.is_empty() => plain,
        ToolChoice::Auto => plain,
        ToolChoice::Required => ToolPlan {
            suppress_tools: false,
            guidance: Some(TOOL_CHOICE_REQUIRED_GUIDANCE.into()),
        },
        ToolChoice::Tool(name) => {
            let want = name.trim();
            let guidance = match tools.iter().find(|t| t.name == want) {
                Some(hit) => named_tool_guidance(&hit.name),
                None => TOOL_CHOICE_REQUIRED_GUIDANCE.into(),
            };
            ToolPlan {
                suppress_tools: false,
                guidance: Some(guidance),
            }
        }
    }
}

fn effective_stops(s: &Sampling) -> Vec<String> {
    s.stop_sequences
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect()
}

/// 把统一请求变成上游要的 `InferenceStreamRequest`。
pub fn build_request(req: &ChatRequest, conversation_id: &str) -> InferenceStreamRequest {
    build_request_with(req, conversation_id, None)
}

/// 同上，但允许强制指定上游模型（[`StreamConfig::force_model`]）。
pub fn build_request_with(
    req: &ChatRequest,
    conversation_id: &str,
    force_model: Option<&str>,
) -> InferenceStreamRequest {
    let model = match force_model {
        Some(m) if !m.trim().is_empty() => {
            let forced = requested_model(m);
            if !req.model.is_empty() && req.model != m {
                tracing::debug!(asked = %req.model, forced = %forced, "按设置强制改写上游模型");
            }
            forced
        }
        _ => requested_model(&req.model),
    };
    let plan = plan_tool_choice(&req.tool_choice, &req.tools);
    let messages = to_inference_messages(&req.messages, plan.guidance.as_deref());
    let tools = if plan.suppress_tools {
        Vec::new()
    } else {
        req.tools
            .iter()
            .map(|t| InferenceAgentTool {
                name: t.name.clone(),
                description: t.description.clone(),
                // schema 要套一层 `jsonSchema` —— 抓官方 Cursor 的真实请求比出来的
                // （`gateway/scripts/decode-inference-dump.mjs`）。发裸 schema 时上游对
                // OpenAI / Gemini 宽容照跑，Anthropic 那一腿直接回 400，表现成
                // `ERROR_PROVIDER_ERROR: Provider Error`，和工具形状毫无关系的措辞。
                parameters: Some(struct_from_json(&serde_json::json!({
                    "jsonSchema": match &t.parameters {
                        serde_json::Value::Null => {
                            serde_json::json!({ "type": "object", "properties": {} })
                        }
                        v => v.clone(),
                    }
                }))),
                custom_tool_format: None,
            })
            .collect()
    };

    let s = &req.sampling;
    let mut mc = InferenceModelConfig::default();
    let mut any = false;
    if let Some(m) = s.max_output_tokens.filter(|&m| m > 0) {
        mc.max_tokens = Some(m as i32);
        any = true;
    }
    if let Some(t) = s.temperature {
        mc.temperature = Some(t);
        any = true;
    }
    if let Some(p) = s.top_p {
        mc.top_p = Some(p);
        any = true;
    }
    let stops = effective_stops(s);
    if !stops.is_empty() {
        mc.stop_sequences = stops;
        any = true;
    }

    InferenceStreamRequest {
        messages,
        tools,
        model_config: any.then_some(mc),
        model_id: Some(model.clone()),
        requested_model: Some(InferenceRequestedModel {
            model_id: model,
            max_mode: false,
            ..Default::default()
        }),
        conversation_id: Some(conversation_id.to_string()),
        ..Default::default()
    }
}

// ---------- 本地执行 stop 序列与输出上限 ----------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextPlan {
    /// 这次要发给客户端的增量。
    pub emit: String,
    /// 累积后的全文。
    pub next_text: String,
    /// 该停了（及为什么）。
    pub stop: Option<FinishReason>,
}

/// 给累积文本和一个新增量，决定发什么、要不要停。stop 序列可能跨越增量边界，所以从
/// `prev.len() - stop.len() + 1` 往后找，而不是只看 delta。
pub fn plan_text_chunk(prev: &str, delta: &str, stops: &[String], max_out: u32) -> TextPlan {
    let combined = format!("{prev}{delta}");
    if !stops.is_empty() {
        let mut cut_at: Option<usize> = None;
        for s in stops.iter().filter(|s| !s.is_empty()) {
            let mut from = (prev.len() + 1).saturating_sub(s.len());
            while from > 0 && !combined.is_char_boundary(from) {
                from -= 1;
            }
            if let Some(i) = combined[from..].find(s.as_str()) {
                let idx = from + i;
                if cut_at.is_none_or(|c| idx < c) {
                    cut_at = Some(idx);
                }
            }
        }
        if let Some(cut) = cut_at {
            let emit = if cut >= prev.len() {
                combined[prev.len()..cut].to_string()
            } else {
                String::new()
            };
            return TextPlan {
                emit,
                next_text: combined[..cut].to_string(),
                stop: Some(FinishReason::Stop),
            };
        }
    }
    if max_out > 0 && estimate_tokens(&combined) >= max_out {
        return TextPlan {
            emit: delta.to_string(),
            next_text: combined,
            stop: Some(FinishReason::Length),
        };
    }
    TextPlan {
        emit: delta.to_string(),
        next_text: combined,
        stop: None,
    }
}

// ---------- 错误归类 ----------

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("静态正则")
}
static RE_AUTH: LazyLock<Regex> =
    LazyLock::new(|| re(r"unauthenticated|not.?logged|unauthorized|invalid.?api|error_not_logged"));
/// 「这个号出不了这个模型」。后两句是掉成 Free 的号：`ERROR_RATE_LIMITED_CHANGEABLE: Named models
/// unavailable: Free plans can only use Auto`——套着限流码，实际是套餐差异，`auto` 仍能出，
/// 指名模型永远出不了；按限流 5 分钟一冷却会让号池每 5 分钟在它身上白撞一次。
static RE_MODEL_UNSUPPORTED: LazyLock<Regex> = LazyLock::new(|| {
    re(r"model_blocked|model_not_found|named models unavailable|free plans can only use auto")
});
static RE_BAD_REQUEST: LazyLock<Regex> =
    LazyLock::new(|| re(r"bad_model_name|unknown option|unknown argument|error_bad_request"));
static RE_LOOP: LazyLock<Regex> =
    LazyLock::new(|| re(r"looping detected|unrecoverable agent|model .{0,24}loop"));
static RE_CONTEXT: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"context length|context window|maximum context|prompt is too long|input is too long|too many tokens|reduce the length of",
    )
});
static RE_PROVIDER: LazyLock<Regex> = LazyLock::new(|| re(r"error_provider_error|provider_error"));
/// 欠费：`ERROR_RATE_LIMITED: You have an unpaid invoice…`。Cursor 把它套在限流码里发，但它是整个号的
/// 事——付款之前哪个模型都出不了。按限流处置只冷却（号 × 模型），号池里每个模型都要在它身上白撞一次。
static RE_BILLING: LazyLock<Regex> = LazyLock::new(|| re(r"unpaid|invoice"));
static RE_RATE_LIMIT: LazyLock<Regex> =
    LazyLock::new(|| re(r"rate.?limited|rate_limit|too many|429"));
static RE_QUOTA: LazyLock<Regex> =
    LazyLock::new(|| re(r"quota|usage|billing|payment|insufficient|exhaust|free.?trial"));
static RE_TOKEN_LIMIT: LazyLock<Regex> =
    LazyLock::new(|| re(r"token limit|context (length|window)"));

/// Connect 层错误（code 是 gRPC 风格字符串）+ 原文 → (HTTP 状态, 分类)。
///
/// 分支顺序是有意的，动之前读一遍：
/// - 「这个号出不了这个模型」（`model_blocked`）和「压根没这个模型」（`bad_model_name`）分开：
///   前者换号能好，后者换号也没用。
/// - `ERROR_CUSTOM_MESSAGE` 里「循环检测」「上下文超长」是内容驱动的，换哪个号都必然重现，
///   归 bad_request 不重试不罚号。
/// - `ERROR_PROVIDER_ERROR` 必须**先于**限流分支识别：Cursor 用 ResourceExhausted 带回它。
///   供应商故障是上游整体在抖，每个号都会撞上，换号毫无意义；当成限流重罚，一次上游抖动
///   就能打空整个号池。
/// - 欠费（`unpaid invoice`）也套在 `ERROR_RATE_LIMITED` 里发，同样要先于限流分支：它是整个号
///   的事，归 quota 让 lane 整号接力，而不是一个模型一个模型地撞。
pub fn map_upstream_error(code: &str, detail: &str) -> (u16, UpstreamKind) {
    let e = detail.to_lowercase();
    if code == "canceled" {
        return (499, UpstreamKind::Canceled);
    }
    if code == "unauthenticated" || RE_AUTH.is_match(&e) {
        return (401, UpstreamKind::Auth);
    }
    if RE_MODEL_UNSUPPORTED.is_match(&e) {
        return (404, UpstreamKind::ModelUnsupported);
    }
    if code == "invalid_argument" || RE_BAD_REQUEST.is_match(&e) {
        return (400, UpstreamKind::BadRequest);
    }
    if RE_LOOP.is_match(&e) || RE_CONTEXT.is_match(&e) {
        return (400, UpstreamKind::BadRequest);
    }
    if RE_PROVIDER.is_match(&e) {
        return (429, UpstreamKind::Provider);
    }
    if RE_BILLING.is_match(&e) {
        return (402, UpstreamKind::Quota);
    }
    if code == "resource_exhausted" || RE_RATE_LIMIT.is_match(&e) {
        return (429, UpstreamKind::RateLimit);
    }
    if RE_QUOTA.is_match(&e) {
        return (402, UpstreamKind::Quota);
    }
    if code == "permission_denied" {
        return (403, UpstreamKind::Forbidden);
    }
    (502, UpstreamKind::Upstream)
}

/// 流内 `InferenceStreamError` → 分类。`error_type` 枚举是权威信号，认不出再退回文本匹配。
pub fn map_inference_error(err: &InferenceStreamError) -> (u16, UpstreamKind) {
    use InferenceStreamErrorType as T;
    let t = err.error_type;
    let msg = err.message.to_lowercase();
    if t == T::InputTokenLimit as i32
        || t == T::OutputTokenLimit as i32
        || err.is_input_token_limit_error
        || err.is_output_token_limit_error
        || RE_TOKEN_LIMIT.is_match(&msg)
    {
        return (400, UpstreamKind::BadRequest);
    }
    if RE_BILLING.is_match(&msg) {
        return (402, UpstreamKind::Quota);
    }
    if t == T::RateLimit as i32 {
        return (429, UpstreamKind::RateLimit);
    }
    if t == T::Authentication as i32 {
        return (401, UpstreamKind::Auth);
    }
    if t == T::Permission as i32 {
        return (403, UpstreamKind::Forbidden);
    }
    if t == T::Overloaded as i32 {
        return (429, UpstreamKind::Provider);
    }
    if t == T::ContentFilter as i32 {
        return (400, UpstreamKind::BadRequest);
    }
    let detail = if err.message.is_empty() {
        &err.code
    } else {
        &err.message
    };
    map_upstream_error("", detail)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimeoutReason {
    Idle,
    Cap,
}

/// 我们自己掐掉这一轮时的错。单独一个分类而不是复用 canceled：canceled 是「没人要这个
/// 结果了」，上层据此闭嘴；超时正相反——客户端还在等，必须把原因告诉它。
fn timeout_error(reason: TimeoutReason, after: Duration) -> UpstreamError {
    let secs = after.as_secs();
    let detail = match reason {
        TimeoutReason::Idle => format!("上游 {secs} 秒没有任何响应"),
        TimeoutReason::Cap => format!("单轮超过 {secs} 秒仍未结束"),
    };
    UpstreamError::new(UpstreamKind::Timeout, 504, detail)
}

/// Connect 层的失败 → 我们的错误。生图那条（一元调用）也走它，两条路的传输失败长得一样。
pub fn failure_to_error(f: ConnectFailure) -> UpstreamError {
    match f {
        ConnectFailure::Transport(e) if e.is_timeout() => {
            UpstreamError::new(UpstreamKind::Timeout, 504, format!("上游超时：{e}"))
        }
        ConnectFailure::Transport(e) if e.is_connect() => {
            UpstreamError::new(UpstreamKind::Upstream, 502, format!("连不上上游：{e}"))
        }
        ConnectFailure::Transport(e) => {
            UpstreamError::new(UpstreamKind::Upstream, 502, format!("上游连接中断：{e}"))
        }
        ConnectFailure::Http { status, body } => {
            let (status, kind) = match status {
                401 => (401, UpstreamKind::Auth),
                402 => (402, UpstreamKind::Quota),
                403 => (403, UpstreamKind::Forbidden),
                429 => (429, UpstreamKind::RateLimit),
                other => {
                    let (_, kind) = map_upstream_error("", &body);
                    (other, kind)
                }
            };
            let preview: String = body.chars().take(300).collect();
            UpstreamError::new(kind, status, format!("HTTP {status}：{preview}"))
        }
        // 一元调用把应用层错误放在这里，和流尾那个 JSON 是同一个东西，归类也走同一条。
        ConnectFailure::Rpc { error, .. } => connect_error_to_upstream(error),
        other => UpstreamError::new(UpstreamKind::Upstream, 502, other.to_string()),
    }
}

/// Connect 错误（流尾信封或一元的 JSON 体）→ 我们的错误。`canceled` 归 upstream 而不是
/// canceled：到这里的 canceled 只可能是上游把流断了，不是客户端不要了。
pub fn connect_error_to_upstream(err: ConnectError) -> UpstreamError {
    let info = err.cursor_info();
    let detail = info.format();
    tracing::warn!(
        connect = %err.code,
        cursor = %info.code,
        title = %info.title,
        expected = ?info.expected,
        "上游错误"
    );
    if err.code == "canceled" {
        return UpstreamError::new(UpstreamKind::Upstream, 502, format!("上游断流：{detail}"))
            .with_cursor_code(info.code);
    }
    let (status, kind) = map_upstream_error(&err.code, &detail);
    UpstreamError::new(kind, status, detail).with_cursor_code(info.code)
}

// ---------- 流驱动 ----------

#[derive(Default)]
struct ToolAcc {
    id: String,
    name: String,
    args: String,
}

/// 一个 `tool_call_part` 归到哪个槽。上游给 `tool_index` 就按它；不给（api2 现在的
/// InferenceService/Stream 一律不给）就按 `tool_call_id`；两者都没有就接着最后一个槽——
/// 那是同一调用的参数增量。
#[derive(Debug, Clone, PartialEq, Eq)]
enum ToolSlot {
    Index(i32),
    Id(String),
    Last,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Stop,
}

/// 流内消息的累积状态。从网络循环里拆出来，每个分支的行为都能不联网测到。
struct Collector {
    started: Instant,
    stops: Vec<String>,
    max_out: u32,
    text: String,
    thinking: String,
    usage: Usage,
    measured: bool,
    routed_model: Option<String>,
    finish: Option<FinishReason>,
    ttft_ms: Option<u64>,
    stream_err: Option<InferenceStreamError>,
    /// 保持到达顺序而不是按 index 排序，与 protocol.js 的 Map 语义一致。
    tool_acc: Vec<(ToolSlot, ToolAcc)>,
}

impl Collector {
    fn new(request: &ChatRequest, started: Instant) -> Self {
        Self {
            started,
            stops: effective_stops(&request.sampling),
            max_out: request.sampling.max_output_tokens.unwrap_or(0),
            text: String::new(),
            thinking: String::new(),
            usage: Usage::default(),
            measured: false,
            routed_model: None,
            finish: None,
            ttft_ms: None,
            stream_err: None,
            tool_acc: Vec::new(),
        }
    }

    fn has_output(&self) -> bool {
        !self.text.is_empty() || !self.thinking.is_empty() || !self.tool_acc.is_empty()
    }

    fn feed(&mut self, r: Response, on_delta: &mut dyn FnMut(Delta)) -> Flow {
        match r {
            Response::TextPart(p) => {
                if self.ttft_ms.is_none() {
                    self.ttft_ms = Some(self.started.elapsed().as_millis() as u64);
                }
                if !p.text.is_empty() {
                    let plan = plan_text_chunk(&self.text, &p.text, &self.stops, self.max_out);
                    self.text = plan.next_text;
                    if !plan.emit.is_empty() {
                        on_delta(Delta::Text(plan.emit));
                    }
                    if let Some(reason) = plan.stop {
                        self.finish = Some(reason);
                        return Flow::Stop;
                    }
                }
            }
            Response::ThinkingPart(p) => {
                if !p.text.is_empty() {
                    if self.ttft_ms.is_none() {
                        self.ttft_ms = Some(self.started.elapsed().as_millis() as u64);
                    }
                    self.thinking.push_str(&p.text);
                    on_delta(Delta::Thinking(p.text));
                }
            }
            Response::Usage(u) => {
                if u.prompt_tokens > 0 {
                    self.usage.input_tokens = u.prompt_tokens as u32;
                }
                if u.completion_tokens > 0 {
                    self.usage.output_tokens = u.completion_tokens as u32;
                }
                self.measured = true;
            }
            Response::ExtendedUsage(u) => {
                if u.input_tokens > 0 {
                    self.usage.input_tokens = u.input_tokens as u32;
                }
                if u.output_tokens > 0 {
                    self.usage.output_tokens = u.output_tokens as u32;
                }
                self.usage.cache_read_tokens = u.cache_read_tokens.max(0) as u32;
                self.usage.cache_write_tokens = u.cache_write_tokens.max(0) as u32;
                self.measured = true;
            }
            Response::ToolCallPart(p) => {
                // 一次工具调用在流里是三段（与官方客户端 4883.js 的 `case"toolCallPart"` 同一套语义）：
                // 开头 `{id, name, args:""}`、若干增量 `{id, args:片段}`、收尾 `{id, name, args:完整,
                // is_complete:true}`。api2 现在这三段都**不带 tool_index**，只靠 id 归组；之前按
                // 「没 index 就开新槽」攒，真机上每次调用都多出一个同 id、参数 `{}` 的假调用。
                // 收尾那段带的是完整参数，不是增量：直接覆盖，免得和增量拼成两份。
                let key = match (p.tool_index, p.tool_call_id.is_empty()) {
                    (Some(i), _) => ToolSlot::Index(i),
                    (None, false) => ToolSlot::Id(p.tool_call_id.clone()),
                    (None, true) => ToolSlot::Last,
                };
                let pos = match key {
                    ToolSlot::Last if !self.tool_acc.is_empty() => self.tool_acc.len() - 1,
                    ToolSlot::Last => {
                        self.tool_acc.push((ToolSlot::Last, ToolAcc::default()));
                        0
                    }
                    key => match self.tool_acc.iter().position(|(k, _)| *k == key) {
                        Some(pos) => pos,
                        None => {
                            self.tool_acc.push((key, ToolAcc::default()));
                            self.tool_acc.len() - 1
                        }
                    },
                };
                let slot = &mut self.tool_acc[pos].1;
                if !p.tool_call_id.is_empty() {
                    slot.id = p.tool_call_id;
                }
                if !p.tool_name.is_empty() {
                    slot.name = p.tool_name;
                }
                if p.is_complete {
                    if !p.args.is_empty() {
                        slot.args = p.args;
                    }
                } else {
                    slot.args.push_str(&p.args);
                }
            }
            Response::ResponseInfo(info) => {
                if !info.model.is_empty() {
                    self.routed_model = Some(info.model.clone());
                }
                if let Some(m) = info
                    .error_message
                    .as_deref()
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                {
                    if self.stream_err.is_none() {
                        self.stream_err = Some(InferenceStreamError {
                            message: m.to_string(),
                            ..Default::default()
                        });
                    }
                }
                // 4.7 / CUA 常常不推 ThinkingPart，只把思考搁在收尾的 ResponseInfo 里。
                // 不刮的话游乐场思考栏永远空，界面就像「空等几十秒再突然出全文」。
                self.absorb_response_reasoning(&info, on_delta);
            }
            Response::Error(e) => {
                // 上游按 max_tokens 截断了输出。这不是错误：标准 API 里它是 finish_reason=length
                // （Anthropic 叫 stop_reason=max_tokens），客户端要的是已生成的那部分加一个「被截断」
                // 的标记。protocol.js 在这里一律抛 400——那是它作为中继的保守选择；对本地网关来说，
                // 把已经流出去的文本再报成失败，客户端看到的是「出了半截然后报错」。
                // 真机探针（examples/probe.rs，max_output_tokens=200）就撞出了这个分支。
                // 什么都没生成就说超限的，仍按错误走——那多半是别的问题。
                let output_capped = e.is_output_token_limit_error
                    || e.error_type == InferenceStreamErrorType::OutputTokenLimit as i32;
                if output_capped && self.has_output() {
                    self.finish = Some(FinishReason::Length);
                    return Flow::Stop;
                }
                self.stream_err = Some(e);
                return Flow::Stop;
            }
            _ => {}
        }
        Flow::Continue
    }

    fn absorb_response_reasoning(
        &mut self,
        info: &InferenceResponseInfo,
        on_delta: &mut dyn FnMut(Delta),
    ) {
        if !self.thinking.is_empty() {
            return;
        }
        let mut buf = String::new();
        for msg in &info.messages {
            for part in &msg.reasoning_parts {
                if part.is_redacted || part.text.is_empty() {
                    continue;
                }
                buf.push_str(&part.text);
            }
        }
        if buf.is_empty() {
            return;
        }
        if self.ttft_ms.is_none() {
            self.ttft_ms = Some(self.started.elapsed().as_millis() as u64);
        }
        self.thinking.push_str(&buf);
        on_delta(Delta::Thinking(buf));
    }

    /// 收尾：流内错误优先；空流报错；用量兜底估算；攒齐的工具调用整理成形。
    fn finish(self, request: &ChatRequest, saw_end: bool) -> Result<Completion, UpstreamError> {
        if let Some(e) = self.stream_err {
            let (status, kind) = map_inference_error(&e);
            let detail = if !e.message.is_empty() {
                e.message.clone()
            } else if !e.code.is_empty() {
                e.code.clone()
            } else {
                "inference stream error".to_string()
            };
            tracing::warn!(code = %e.code, error_type = e.error_type, kind = kind.as_str(), "流内错误");
            return Err(UpstreamError::new(kind, status, detail).with_cursor_code(e.code));
        }

        if !saw_end && !self.has_output() {
            return Err(UpstreamError::new(
                UpstreamKind::Upstream,
                502,
                "上游没有返回任何内容就关闭了流",
            ));
        }

        let mut usage = self.usage;
        if usage.input_tokens == 0 {
            let joined = request
                .messages
                .iter()
                .map(|m| m.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            usage.input_tokens = estimate_tokens(&joined);
        }
        if usage.output_tokens == 0 {
            usage.output_tokens = estimate_tokens(&self.text);
        }

        let tool_calls: Vec<ToolCall> = self
            .tool_acc
            .into_iter()
            .filter(|(_, t)| !t.name.is_empty())
            .map(|(_, t)| ToolCall {
                id: if t.id.is_empty() {
                    uuid::Uuid::new_v4().to_string()
                } else {
                    t.id
                },
                name: t.name,
                arguments: if t.args.is_empty() {
                    "{}".into()
                } else {
                    t.args
                },
            })
            .collect();
        let finish_reason = self.finish.unwrap_or(if tool_calls.is_empty() {
            FinishReason::Stop
        } else {
            FinishReason::ToolCalls
        });

        Ok(Completion {
            text: self.text,
            thinking: self.thinking,
            tool_calls,
            finish_reason,
            usage,
            usage_measured: self.measured,
            routed_model: self.routed_model,
            ttft_ms: self.ttft_ms,
            turn_ms: self.started.elapsed().as_millis() as u64,
            raw_response: None,
        })
    }
}

/// 跑一轮对话。
///
/// 文本 / 思考增量经 `on_delta` 实时回调；工具调用按 `tool_index` / `tool_call_id` 分片攒齐后放进返回值，
/// `finish_reason == ToolCalls` 时由调用方执行并在下一轮带回结果。取消 = drop 这个 future
/// （客户端挂断时入站层自然会这么做），连接随之关闭。
pub async fn stream(
    client: &reqwest::Client,
    cfg: &StreamConfig,
    access_token: &str,
    identity: &DeviceIdentity,
    request: &ChatRequest,
    on_delta: &mut (dyn FnMut(Delta) + Send),
) -> Result<Completion, UpstreamError> {
    let started = Instant::now();
    let conversation_id = request
        .conversation_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let req = build_request_with(request, &conversation_id, cfg.force_model.as_deref());
    let headers = ai_headers(
        access_token,
        identity,
        &cfg.client_type,
        RequestNonce::now(),
    );
    let url = format!("{}{}", cfg.base_url.trim_end_matches('/'), STREAM_PATH);

    let mut stream = connect::call_server_stream(client, &url, &headers, &req.encode_to_vec())
        .await
        .map_err(failure_to_error)?;

    let mut collector = Collector::new(request, started);
    let mut saw_end = false;

    loop {
        let cap_left = cfg.max_turn.saturating_sub(started.elapsed());
        if cap_left.is_zero() {
            return Err(timeout_error(TimeoutReason::Cap, cfg.max_turn));
        }
        let wait = cfg.idle_timeout.min(cap_left);
        let item = match tokio::time::timeout(wait, stream.next()).await {
            Ok(r) => r.map_err(failure_to_error)?,
            Err(_) => {
                return Err(if cap_left < cfg.idle_timeout {
                    timeout_error(TimeoutReason::Cap, cfg.max_turn)
                } else {
                    timeout_error(TimeoutReason::Idle, cfg.idle_timeout)
                });
            }
        };

        match item {
            StreamItem::Eof => break,
            StreamItem::End(end) => {
                saw_end = true;
                if let Some(err) = end.error {
                    return Err(connect_error_to_upstream(err));
                }
                break;
            }
            StreamItem::Message(bytes) => {
                let resp = InferenceStreamResponse::decode(&bytes[..]).map_err(|e| {
                    UpstreamError::new(UpstreamKind::Upstream, 502, format!("响应解码失败：{e}"))
                })?;
                let Some(r) = resp.response else { continue };
                if collector.feed(r, on_delta) == Flow::Stop {
                    break;
                }
            }
        }
    }

    collector.finish(request, saw_end)
}

/// `sand-cua` 这类别名探针的结论。`resolved_model` 空时看 `note`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutedModelProbe {
    pub resolved_model: Option<String>,
    pub note: Option<String>,
}

/// 从流里挑 `sand-cua` 真正落到谁。
///
/// 旧探针「第一帧非空 model 就挂」会踩两处：thinking 模型（4.7）常先出思考、
/// `ResponseInfo` 很晚才来，4 token 上限把流掐了就记失败；Ultra 号第一帧又常
/// 带着账号默认的 opus，还可能挂着 error_message。只信已知分片，或出字之后
/// 那次干净的 ResponseInfo。
#[derive(Debug, Clone)]
struct CuaRouteAcc {
    requested: String,
    candidates: Vec<String>,
    chosen: Option<String>,
    saw_output: bool,
    error: Option<String>,
}

impl CuaRouteAcc {
    fn new(requested: &str) -> Self {
        Self {
            requested: requested.to_string(),
            candidates: Vec::new(),
            chosen: None,
            saw_output: false,
            error: None,
        }
    }

    fn on_output(&mut self) {
        self.saw_output = true;
    }

    fn on_response_info(&mut self, model: &str, error_message: Option<&str>) {
        let model = model.trim();
        if !model.is_empty() {
            self.candidates.push(model.to_string());
        }
        if let Some(e) = error_message.map(str::trim).filter(|e| !e.is_empty()) {
            if self.error.is_none() {
                self.error = Some(e.to_string());
            }
            return;
        }
        if model.is_empty() || model.eq_ignore_ascii_case(&self.requested) {
            return;
        }
        if crate::models::is_known_cua_shard(model) || self.saw_output {
            self.chosen = Some(model.to_string());
        }
    }

    fn on_stream_error(&mut self, message: &str) {
        let message = message.trim();
        if self.error.is_none() && !message.is_empty() {
            self.error = Some(message.to_string());
        }
    }

    fn ready(&self) -> bool {
        self.chosen
            .as_deref()
            .is_some_and(crate::models::is_known_cua_shard)
    }

    fn finish(self) -> Result<RoutedModelProbe, UpstreamError> {
        if let Some(model) = self.chosen {
            return Ok(RoutedModelProbe {
                resolved_model: Some(model),
                note: None,
            });
        }
        if let Some(error) = self.error {
            return Err(UpstreamError::new(UpstreamKind::Upstream, 502, error));
        }
        let note = if self.saw_output {
            "上游出了字，但没回落到哪个模型。".to_string()
        } else if let Some(first) = self.candidates.first() {
            format!("只看到 {first}，不像 sand-cua 落点（没出字）。")
        } else {
            "上游没回落到哪个模型。".to_string()
        };
        Ok(RoutedModelProbe {
            resolved_model: None,
            note: Some(note),
        })
    }
}

/// 打一发短 Stream，看别名落到谁。已知 CUA 分片一到就挂断，不把回答跑完。
///
/// 不设 `max_output_tokens`：thinking 模型常先吐思考，上限太低会在
/// `ResponseInfo` 到来前被掐。闲时/总时长仍由 [`StreamConfig`] 管。
pub async fn probe_routed_model(
    client: &reqwest::Client,
    cfg: &StreamConfig,
    access_token: &str,
    identity: &DeviceIdentity,
    model: &str,
) -> Result<RoutedModelProbe, UpstreamError> {
    let request = ChatRequest {
        model: model.to_string(),
        messages: vec![Message::text(Role::User, "Reply with exactly: 1")],
        ..ChatRequest::default()
    };
    let started = Instant::now();
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let req = build_request_with(&request, &conversation_id, cfg.force_model.as_deref());
    let headers = ai_headers(
        access_token,
        identity,
        &cfg.client_type,
        RequestNonce::now(),
    );
    let url = format!("{}{}", cfg.base_url.trim_end_matches('/'), STREAM_PATH);

    let mut stream = connect::call_server_stream(client, &url, &headers, &req.encode_to_vec())
        .await
        .map_err(failure_to_error)?;

    let mut acc = CuaRouteAcc::new(model);

    loop {
        if acc.ready() {
            break;
        }
        let cap_left = cfg.max_turn.saturating_sub(started.elapsed());
        if cap_left.is_zero() {
            return Err(timeout_error(TimeoutReason::Cap, cfg.max_turn));
        }
        let wait = cfg.idle_timeout.min(cap_left);
        let item = match tokio::time::timeout(wait, stream.next()).await {
            Ok(r) => r.map_err(failure_to_error)?,
            Err(_) => {
                if acc.ready()
                    || acc.chosen.is_some()
                    || acc.saw_output
                    || !acc.candidates.is_empty()
                {
                    break;
                }
                return Err(if cap_left < cfg.idle_timeout {
                    timeout_error(TimeoutReason::Cap, cfg.max_turn)
                } else {
                    timeout_error(TimeoutReason::Idle, cfg.idle_timeout)
                });
            }
        };

        match item {
            StreamItem::Eof => break,
            StreamItem::End(end) => {
                if let Some(err) = end.error {
                    if acc.ready() {
                        break;
                    }
                    if acc.chosen.is_none() {
                        return Err(connect_error_to_upstream(err));
                    }
                }
                break;
            }
            StreamItem::Message(bytes) => {
                let resp = InferenceStreamResponse::decode(&bytes[..]).map_err(|e| {
                    UpstreamError::new(UpstreamKind::Upstream, 502, format!("响应解码失败：{e}"))
                })?;
                let Some(r) = resp.response else { continue };
                match r {
                    Response::TextPart(p) if !p.text.is_empty() => acc.on_output(),
                    Response::ThinkingPart(p) if !p.text.is_empty() => acc.on_output(),
                    Response::ResponseInfo(info) => {
                        acc.on_response_info(&info.model, info.error_message.as_deref());
                    }
                    Response::Error(e) => {
                        if acc.ready() {
                            break;
                        }
                        acc.on_stream_error(&e.message);
                        if acc.chosen.is_none() {
                            return Err(UpstreamError::new(
                                UpstreamKind::Upstream,
                                502,
                                if e.message.is_empty() {
                                    "inference stream error".into()
                                } else {
                                    e.message
                                },
                            ));
                        }
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    acc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalized::{ToolDef, ToolResult};
    use crate::proto_tests::{
        unhex, ASSISTANT_WITH_TOOL_CALL, TOOL_RESULT, USER_WITH_FOLDED_SYSTEM, USER_WITH_IMAGE,
    };

    fn tool(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            description: String::new(),
            parameters: serde_json::json!({ "type": "object" }),
            ..ToolDef::default()
        }
    }

    /// 和 /tmp/vec-proto.mjs 完全相同的输入。
    fn js_vector_input() -> Vec<Message> {
        vec![
            Message::text(Role::System, "SYS"),
            Message::text(Role::User, "hello"),
            Message {
                tool_calls: vec![ToolCall {
                    id: "call_1".into(),
                    name: "get_weather".into(),
                    arguments: r#"{"city":"Paris","n":2}"#.into(),
                }],
                ..Message::text(Role::Assistant, "calling")
            },
            Message {
                tool_results: vec![ToolResult {
                    tool_call_id: "call_1".into(),
                    tool_name: "get_weather".into(),
                    text: "sunny".into(),
                    is_error: false,
                }],
                ..Message::text(Role::Tool, "")
            },
            Message {
                images: vec![ImageInput {
                    data: "AAEC".into(),
                    mime_type: "image/png".into(),
                }],
                ..Message::text(Role::User, "look")
            },
        ]
    }

    #[test]
    fn to_inference_messages_reproduces_what_protocol_js_encodes() {
        // 跨实现对拍：Rust 映射出来的消息，必须和线上 JS 编码器对同一输入产出的字节解出来一样。
        let ours = to_inference_messages(&js_vector_input(), Some("EXTRA"));
        let theirs: Vec<InferenceCoreMessage> = [
            USER_WITH_FOLDED_SYSTEM,
            ASSISTANT_WITH_TOOL_CALL,
            TOOL_RESULT,
            USER_WITH_IMAGE,
        ]
        .iter()
        .map(|h| InferenceCoreMessage::decode(&unhex(h)[..]).unwrap())
        .collect();
        assert_eq!(
            ours.len(),
            4,
            "system 不单独成条、tool 结果一条、其余各一条"
        );
        for (i, (a, b)) in ours.iter().zip(theirs.iter()).enumerate() {
            assert_eq!(a, b, "第 {i} 条与 protocol.js 不一致");
        }
    }

    #[test]
    fn only_system_gets_a_synthetic_user_carrier() {
        let out = to_inference_messages(&[Message::text(Role::System, "rules")], None);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].role, InferenceMessageRole::User as i32);
        assert_eq!(out[0].content, Some(Content::Text("rules".into())));
    }

    #[test]
    fn empty_messages_are_dropped_and_extra_system_alone_still_lands() {
        let out = to_inference_messages(
            &[
                Message::text(Role::User, "   "),
                Message::text(Role::Assistant, ""),
            ],
            Some("guide"),
        );
        // 空 user 承载了 guidance；空 assistant 被丢。
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].content, Some(Content::Text("guide".into())));
    }

    #[test]
    fn tool_call_with_bad_json_args_still_carries_the_raw_text() {
        let m = Message {
            tool_calls: vec![ToolCall {
                id: "c".into(),
                name: "f".into(),
                arguments: "not json".into(),
            }],
            ..Message::text(Role::Assistant, "")
        };
        let out = to_inference_messages(&[m], None);
        assert_eq!(out.len(), 1, "只有工具调用、没正文的 assistant 也要发");
        let tc = &out[0].tool_calls[0];
        assert_eq!(tc.raw_tool_call_args.as_deref(), Some("not json"));
        assert_eq!(
            tc.args,
            Some(prost_types::Struct::default()),
            "解不开就空 Struct，不打挂整轮"
        );
    }

    #[test]
    fn plan_tool_choice_covers_all_four_kinds() {
        let tools = vec![tool("a"), tool("b")];
        assert_eq!(
            plan_tool_choice(&ToolChoice::None, &tools),
            ToolPlan {
                suppress_tools: true,
                guidance: None
            }
        );
        assert_eq!(
            plan_tool_choice(&ToolChoice::Auto, &tools),
            ToolPlan {
                suppress_tools: false,
                guidance: None
            }
        );
        assert_eq!(
            plan_tool_choice(&ToolChoice::Required, &tools)
                .guidance
                .as_deref(),
            Some(TOOL_CHOICE_REQUIRED_GUIDANCE)
        );
        assert!(plan_tool_choice(&ToolChoice::Tool(" b ".into()), &tools)
            .guidance
            .unwrap()
            .contains("`b`"));
        assert_eq!(
            plan_tool_choice(&ToolChoice::Tool("zzz".into()), &tools)
                .guidance
                .as_deref(),
            Some(TOOL_CHOICE_REQUIRED_GUIDANCE),
            "点名了不存在的工具就退回「必须调一个」"
        );
        assert_eq!(
            plan_tool_choice(&ToolChoice::Required, &[]).guidance,
            None,
            "没工具可调时硬塞引导只会让模型去找不存在的工具"
        );
    }

    #[test]
    fn plan_text_chunk_passes_text_through_when_nothing_triggers() {
        let p = plan_text_chunk("ab", "cd", &[], 0);
        assert_eq!(p.emit, "cd");
        assert_eq!(p.next_text, "abcd");
        assert_eq!(p.stop, None);
    }

    #[test]
    fn plan_text_chunk_cuts_at_a_stop_inside_the_delta() {
        let p = plan_text_chunk("hello ", "world END more", &["END".into()], 0);
        assert_eq!(p.emit, "world ");
        assert_eq!(p.next_text, "hello world ");
        assert_eq!(p.stop, Some(FinishReason::Stop));
    }

    #[test]
    fn plan_text_chunk_catches_a_stop_straddling_the_boundary() {
        // stop = "bc"，b 在 prev、c 在 delta：全文要回退到 "a"，这次什么都不发。
        let p = plan_text_chunk("ab", "c", &["bc".into()], 0);
        assert_eq!(p.emit, "");
        assert_eq!(p.next_text, "a");
        assert_eq!(p.stop, Some(FinishReason::Stop));
    }

    #[test]
    fn plan_text_chunk_picks_the_earliest_of_several_stops() {
        let p = plan_text_chunk("", "xxAyyBzz", &["B".into(), "A".into()], 0);
        assert_eq!(p.next_text, "xx");
    }

    #[test]
    fn plan_text_chunk_enforces_the_output_cap_locally() {
        // "abcdefgh" ≈ 2 token；上限 2 → 这次发完就停，reason=length。
        let p = plan_text_chunk("abcd", "efgh", &[], 2);
        assert_eq!(p.emit, "efgh");
        assert_eq!(p.stop, Some(FinishReason::Length));
        assert_eq!(plan_text_chunk("a", "b", &[], 5).stop, None);
    }

    #[test]
    fn plan_text_chunk_is_utf8_safe_around_the_lookback_window() {
        // 多字节字符横跨回看窗口起点，不能在字符中间切片。
        let p = plan_text_chunk("你好世界", "！停", &["停".into()], 0);
        assert_eq!(p.emit, "！");
        assert_eq!(p.next_text, "你好世界！");
    }

    #[test]
    fn build_request_maps_model_tools_and_sampling() {
        let req = ChatRequest {
            model: "auto".into(),
            messages: vec![Message::text(Role::User, "hi")],
            tools: vec![ToolDef {
                name: "t".into(),
                description: "d".into(),
                parameters: serde_json::Value::Null,
                ..ToolDef::default()
            }],
            tool_choice: ToolChoice::Auto,
            sampling: Sampling {
                max_output_tokens: Some(50),
                temperature: Some(0.2),
                top_p: None,
                stop_sequences: vec!["".into(), "X".into()],
            },
            conversation_id: None,
            ..Default::default()
        };
        let r = build_request(&req, "conv");
        assert_eq!(r.model_id.as_deref(), Some("default"), "auto → default");
        assert_eq!(r.requested_model.as_ref().unwrap().model_id, "default");
        assert_eq!(r.conversation_id.as_deref(), Some("conv"));
        assert_eq!(r.tools.len(), 1);
        // 外面这层 `jsonSchema` 不是我们的风格选择，是上游要的形状：发裸 schema 时
        // Anthropic 那一腿回 400（抓官方请求比对得出）。少了它整条 Claude 工具链就是死的。
        let wrapper = r.tools[0].parameters.as_ref().unwrap();
        let Some(Kind::StructValue(schema)) = &wrapper.fields["jsonSchema"].kind else {
            panic!("schema 必须套在 jsonSchema 里");
        };
        assert_eq!(
            schema.fields["type"].kind,
            Some(Kind::StringValue("object".into())),
            "没给 schema 时补一个空对象 schema"
        );
        let mc = r.model_config.unwrap();
        assert_eq!(mc.max_tokens, Some(50));
        assert_eq!(mc.temperature, Some(0.2));
        assert_eq!(mc.top_p, None);
        assert_eq!(mc.stop_sequences, vec!["X".to_string()], "空 stop 被过滤");
    }

    #[test]
    fn build_request_omits_model_config_and_tools_when_there_is_nothing_to_say() {
        let req = ChatRequest {
            model: "claude-sonnet-5".into(),
            messages: vec![Message::text(Role::User, "hi")],
            tools: vec![tool("a")],
            tool_choice: ToolChoice::None,
            sampling: Sampling::default(),
            conversation_id: None,
            ..Default::default()
        };
        let r = build_request(&req, "c");
        assert!(r.model_config.is_none());
        assert!(
            r.tools.is_empty(),
            "tool_choice=none 是硬保证：一个工具都不声明"
        );
        assert_eq!(r.model_id.as_deref(), Some("claude-sonnet-5"));
    }

    #[test]
    fn struct_from_json_handles_nesting() {
        let s = struct_from_json(&serde_json::json!({
            "a": 1, "b": "x", "c": true, "d": null, "e": [1, "y"], "f": { "g": 2 }
        }));
        assert_eq!(s.fields["a"].kind, Some(Kind::NumberValue(1.0)));
        assert_eq!(s.fields["b"].kind, Some(Kind::StringValue("x".into())));
        assert_eq!(s.fields["c"].kind, Some(Kind::BoolValue(true)));
        assert_eq!(s.fields["d"].kind, Some(Kind::NullValue(0)));
        let Some(Kind::ListValue(l)) = &s.fields["e"].kind else {
            panic!()
        };
        assert_eq!(l.values.len(), 2);
        let Some(Kind::StructValue(inner)) = &s.fields["f"].kind else {
            panic!()
        };
        assert_eq!(inner.fields["g"].kind, Some(Kind::NumberValue(2.0)));
        assert_eq!(
            struct_from_json(&serde_json::json!("nope")),
            prost_types::Struct::default()
        );
    }

    #[test]
    fn map_upstream_error_follows_protocol_js_branch_order() {
        use UpstreamKind::*;
        assert_eq!(map_upstream_error("canceled", ""), (499, Canceled));
        assert_eq!(map_upstream_error("unauthenticated", ""), (401, Auth));
        assert_eq!(map_upstream_error("", "ERROR_NOT_LOGGED_IN"), (401, Auth));
        assert_eq!(
            map_upstream_error("", "ERROR_MODEL_BLOCKED"),
            (404, ModelUnsupported)
        );
        // 掉成 Free 的号套着限流码说「指名模型不可用」：是套餐差异，只避开（号 × 模型），别当限流。
        assert_eq!(
            map_upstream_error(
                "resource_exhausted",
                "ERROR_RATE_LIMITED_CHANGEABLE: Named models unavailable: Free plans can only use Auto. Switch to Auto or upgrade plans to continue."
            ),
            (404, ModelUnsupported)
        );
        assert_eq!(
            map_upstream_error("invalid_argument", ""),
            (400, BadRequest)
        );
        assert_eq!(
            map_upstream_error("", "ERROR_BAD_MODEL_NAME"),
            (400, BadRequest)
        );
        assert_eq!(
            map_upstream_error("internal", "ERROR_CUSTOM_MESSAGE: Looping detected"),
            (400, BadRequest),
            "内容驱动的错误换号必然重现，不重试不罚号"
        );
        assert_eq!(
            map_upstream_error("internal", "prompt is too long for the context window"),
            (400, BadRequest)
        );
        // 关键顺序：带 ResourceExhausted 状态码的 PROVIDER_ERROR 必须先认出来，否则整池被打冷。
        assert_eq!(
            map_upstream_error(
                "resource_exhausted",
                "ERROR_PROVIDER_ERROR: upstream failed"
            ),
            (429, Provider)
        );
        assert_eq!(
            map_upstream_error("resource_exhausted", ""),
            (429, RateLimit)
        );
        assert_eq!(
            map_upstream_error("", "You have been rate limited"),
            (429, RateLimit)
        );
        // 欠费套在限流码里发（真机 2026-09-04 抓到的原文），但它是整个号的事：归 quota 整号接力。
        assert_eq!(
            map_upstream_error(
                "resource_exhausted",
                "ERROR_RATE_LIMITED: You have an unpaid invoice: Visit cursor.com/dashboard and pay your invoice in Stripe to resume requests."
            ),
            (402, Quota)
        );
        assert_eq!(map_upstream_error("", "usage quota exceeded"), (402, Quota));
        assert_eq!(
            map_upstream_error("permission_denied", ""),
            (403, Forbidden)
        );
        assert_eq!(map_upstream_error("unavailable", "boom"), (502, Upstream));
    }

    #[test]
    fn map_inference_error_trusts_the_enum_then_falls_back_to_text() {
        use UpstreamKind::*;
        let e = |t: InferenceStreamErrorType, msg: &str| InferenceStreamError {
            error_type: t as i32,
            message: msg.into(),
            ..Default::default()
        };
        use InferenceStreamErrorType as T;
        assert_eq!(
            map_inference_error(&e(T::InputTokenLimit, "")),
            (400, BadRequest)
        );
        assert_eq!(map_inference_error(&e(T::RateLimit, "")), (429, RateLimit));
        assert_eq!(map_inference_error(&e(T::Authentication, "")), (401, Auth));
        assert_eq!(map_inference_error(&e(T::Permission, "")), (403, Forbidden));
        assert_eq!(map_inference_error(&e(T::Overloaded, "")), (429, Provider));
        assert_eq!(
            map_inference_error(&e(T::ContentFilter, "")),
            (400, BadRequest)
        );
        assert_eq!(
            map_inference_error(&InferenceStreamError {
                is_output_token_limit_error: true,
                ..Default::default()
            }),
            (400, BadRequest)
        );
        assert_eq!(
            map_inference_error(&e(T::Unknown, "rate_limit hit")),
            (429, RateLimit),
            "枚举认不出就按文本"
        );
        assert_eq!(map_inference_error(&e(T::Unknown, "???")), (502, Upstream));
    }

    #[test]
    fn http_level_failures_map_by_status_first() {
        let e = failure_to_error(ConnectFailure::Http {
            status: 401,
            body: "<html>".into(),
        });
        assert_eq!((e.status, e.kind), (401, UpstreamKind::Auth));
        let e = failure_to_error(ConnectFailure::Http {
            status: 503,
            body: "rate limited".into(),
        });
        assert_eq!((e.status, e.kind), (503, UpstreamKind::RateLimit));
        let e = failure_to_error(ConnectFailure::CompressedFrame);
        assert_eq!(e.kind, UpstreamKind::Upstream);
    }

    #[test]
    fn connect_error_canceled_from_upstream_is_an_upstream_failure_not_a_client_cancel() {
        let e = connect_error_to_upstream(ConnectError {
            code: "canceled".into(),
            message: "stream reset".into(),
            details: vec![],
        });
        assert_eq!(e.kind, UpstreamKind::Upstream);
        assert_eq!(e.status, 502);
        assert!(e.message.contains("stream reset"));
    }

    #[test]
    fn connect_error_carries_cursor_code_for_diagnosis() {
        let e = connect_error_to_upstream(ConnectError {
            code: "resource_exhausted".into(),
            message: "x".into(),
            details: vec![serde_json::json!({
                "debug": { "error": "ERROR_CUSTOM_MESSAGE", "details": { "title": "Too many computers", "detail": "d" } }
            })],
        });
        assert_eq!(e.cursor_code.as_deref(), Some("ERROR_CUSTOM_MESSAGE"));
        assert_eq!(e.kind, UpstreamKind::RateLimit);
        assert!(e
            .message
            .starts_with("ERROR_CUSTOM_MESSAGE: Too many computers"));
    }

    #[test]
    fn timeout_messages_say_which_limit_fired() {
        let idle = timeout_error(TimeoutReason::Idle, Duration::from_secs(180));
        assert_eq!((idle.status, idle.kind), (504, UpstreamKind::Timeout));
        assert!(idle.message.contains("180 秒没有任何响应"));
        let cap = timeout_error(TimeoutReason::Cap, Duration::from_secs(1800));
        assert!(cap.message.contains("1800 秒"));
    }

    #[test]
    fn force_model_overrides_whatever_the_client_asked_for() {
        let req = ChatRequest {
            model: "claude-sonnet-4-5-20250929".into(),
            messages: vec![Message::text(Role::User, "hi")],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            sampling: Sampling::default(),
            conversation_id: None,
            ..Default::default()
        };
        // 不强制：按别名映射走。
        let normal = build_request_with(&req, "c", None);
        assert_eq!(normal.model_id.as_deref(), Some("claude-sonnet-5"));
        // 强制 auto（账号只跑得了这一档）：客户端仍报它认识的名字，上游收到的是 default。
        let forced = build_request_with(&req, "c", Some("auto"));
        assert_eq!(forced.model_id.as_deref(), Some("default"));
        // 空串当没设。
        assert_eq!(
            build_request_with(&req, "c", Some("  "))
                .model_id
                .as_deref(),
            Some("claude-sonnet-5")
        );
    }

    #[test]
    fn requested_model_defaults_auto_and_empty() {
        assert_eq!(requested_model(""), "default");
        assert_eq!(requested_model("auto"), "default");
        assert_eq!(requested_model("gpt-5.6-sol"), "gpt-5.6-sol");
        // 别家的名字要在这一层就换掉，否则上游回 ERROR_BAD_MODEL_NAME。
        assert_eq!(
            requested_model("claude-sonnet-4-5-20250929"),
            "claude-sonnet-5"
        );
        assert_eq!(requested_model("gpt-4o"), "gpt-5.6-sol");
        assert_eq!(
            requested_model("llama-3-70b"),
            "default",
            "认不出就交给上游自选"
        );
        assert_eq!(requested_model("grok-4.7"), "sand-cua");
        assert_eq!(requested_model("grok-4-7-0910-xhigh"), "sand-cua");
        assert_eq!(requested_model("sand-cua"), "sand-cua");
    }

    // ---------- Collector：流内每个分支的行为，不联网钉住 ----------

    use crate::proto::{
        InferenceExtendedUsageInfo, InferenceReasoningPart, InferenceResponseInfo,
        InferenceResponseMessage, InferenceTextStreamPart, InferenceThinkingStreamPart,
        InferenceToolCallStreamPart, InferenceUsageInfo,
    };

    fn req_with(max_out: Option<u32>, stops: &[&str]) -> ChatRequest {
        ChatRequest {
            model: "m".into(),
            messages: vec![Message::text(Role::User, "hello there")],
            tools: vec![],
            tool_choice: ToolChoice::Auto,
            sampling: Sampling {
                max_output_tokens: max_out,
                stop_sequences: stops.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            conversation_id: None,
            ..Default::default()
        }
    }

    fn text(t: &str) -> Response {
        Response::TextPart(InferenceTextStreamPart {
            text: t.into(),
            is_final: false,
        })
    }

    fn output_limit_error() -> Response {
        Response::Error(InferenceStreamError {
            message: "Provider exceeded max output tokens.".into(),
            code: "3".into(),
            error_type: InferenceStreamErrorType::OutputTokenLimit as i32,
            ..Default::default()
        })
    }

    /// 把一串响应喂给 Collector，收集 delta，返回 (deltas, 收尾结果)。
    fn run(
        req: &ChatRequest,
        items: Vec<Response>,
        saw_end: bool,
    ) -> (Vec<Delta>, Result<Completion, UpstreamError>) {
        let mut deltas = Vec::new();
        let mut c = Collector::new(req, Instant::now());
        for r in items {
            if c.feed(r, &mut |d| deltas.push(d)) == Flow::Stop {
                break;
            }
        }
        (deltas, c.finish(req, saw_end))
    }

    #[test]
    fn text_deltas_stream_out_and_accumulate() {
        let req = req_with(None, &[]);
        let (deltas, done) = run(&req, vec![text("Hel"), text("lo"), text("")], true);
        assert_eq!(
            deltas,
            vec![Delta::Text("Hel".into()), Delta::Text("lo".into())]
        );
        let c = done.unwrap();
        assert_eq!(c.text, "Hello");
        assert_eq!(c.finish_reason, FinishReason::Stop);
        assert!(c.ttft_ms.is_some());
        assert!(!c.usage_measured, "没收到 usage 就是估的");
        assert!(
            c.usage.input_tokens > 0 && c.usage.output_tokens > 0,
            "估算兜底不能给 0"
        );
    }

    #[test]
    fn output_token_limit_after_content_is_a_length_finish_not_an_error() {
        // 真机探针撞出来的分支：max_output_tokens=200，上游流完文本后报 OUTPUT_TOKEN_LIMIT。
        let req = req_with(Some(200), &[]);
        let (deltas, done) = run(
            &req,
            vec![text("partial answer"), output_limit_error()],
            false,
        );
        assert_eq!(deltas.len(), 1);
        let c = done.expect("已生成的文本不能被当成失败丢掉");
        assert_eq!(c.text, "partial answer");
        assert_eq!(c.finish_reason, FinishReason::Length);
    }

    #[test]
    fn output_token_limit_with_nothing_generated_is_still_an_error() {
        let req = req_with(Some(200), &[]);
        let (_, done) = run(&req, vec![output_limit_error()], false);
        let e = done.unwrap_err();
        assert_eq!(e.kind, UpstreamKind::BadRequest);
        assert_eq!(e.cursor_code.as_deref(), Some("3"));
    }

    #[test]
    fn other_stream_errors_win_over_content() {
        let req = req_with(None, &[]);
        let rate_limited = Response::Error(InferenceStreamError {
            message: "slow down".into(),
            error_type: InferenceStreamErrorType::RateLimit as i32,
            ..Default::default()
        });
        let (_, done) = run(&req, vec![text("some"), rate_limited], false);
        let e = done.unwrap_err();
        assert_eq!((e.status, e.kind), (429, UpstreamKind::RateLimit));
    }

    #[test]
    fn response_info_error_message_counts_as_an_error_and_routed_model_is_kept() {
        let req = req_with(None, &[]);
        let info = Response::ResponseInfo(InferenceResponseInfo {
            model: "claude-sonnet-5".into(),
            error_message: Some("model unavailable".into()),
            ..Default::default()
        });
        let (_, done) = run(&req, vec![info, text("x")], true);
        let e = done.unwrap_err();
        assert!(e.message.contains("model unavailable"));

        let ok_info = Response::ResponseInfo(InferenceResponseInfo {
            model: "gpt-5.6-sol".into(),
            ..Default::default()
        });
        let (_, done) = run(&req, vec![ok_info, text("x")], true);
        assert_eq!(done.unwrap().routed_model.as_deref(), Some("gpt-5.6-sol"));
    }

    #[test]
    fn extended_usage_is_measured_and_overrides_the_plain_usage() {
        let req = req_with(None, &[]);
        let plain = Response::Usage(InferenceUsageInfo {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: None,
        });
        let extended = Response::ExtendedUsage(InferenceExtendedUsageInfo {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: 40,
            cache_write_tokens: 0,
            max_tokens: 0,
        });
        let (_, done) = run(&req, vec![text("a"), plain, extended], true);
        let c = done.unwrap();
        assert!(c.usage_measured);
        assert_eq!(c.usage.input_tokens, 100);
        assert_eq!(c.usage.output_tokens, 50);
        assert_eq!(c.usage.cache_read_tokens, 40);
    }

    #[test]
    fn tool_calls_are_assembled_by_index_in_arrival_order() {
        let req = req_with(None, &[]);
        let part = |idx: i32, id: &str, name: &str, args: &str| {
            Response::ToolCallPart(InferenceToolCallStreamPart {
                tool_call_id: id.into(),
                tool_name: name.into(),
                args: args.into(),
                is_complete: false,
                tool_index: Some(idx),
            })
        };
        let items = vec![
            part(1, "c1", "second", r#"{"b":"#),
            part(0, "c0", "first", r#"{"a":1}"#),
            part(1, "", "", "2}"),
            part(2, "", "", ""), // 没名字的空槽不会成为工具调用
        ];
        let c = run(&req, items, true).1.unwrap();
        assert_eq!(c.finish_reason, FinishReason::ToolCalls);
        assert_eq!(c.tool_calls.len(), 2);
        assert_eq!(
            (
                c.tool_calls[0].name.as_str(),
                c.tool_calls[0].arguments.as_str()
            ),
            ("second", r#"{"b":2}"#),
            "到达顺序，不是 index 顺序（与 protocol.js 的 Map 一致）"
        );
        assert_eq!(c.tool_calls[1].id, "c0");
    }

    /// 2026-09-04 真机（grok-4.6 经 api2 InferenceService/Stream）录到的原始序列：每次调用是
    /// 「开头（id+name）→ 若干增量（只有 id 和 args 片段，可能为空）→ 收尾（id+name+完整 args，
    /// is_complete）」，全程没有 tool_index。之前按「没 index 就开新槽」攒，结果每次调用都多出一个
    /// 同 id、参数 `{}` 的假调用（`[('get_weather','{}'),('get_weather','{"city":"Paris"}')]`）。
    #[test]
    fn api2_tool_call_parts_without_index_are_grouped_by_id_and_completed_by_the_final_part() {
        let req = req_with(None, &[]);
        let part = |id: &str, name: &str, args: &str, complete: bool| {
            Response::ToolCallPart(InferenceToolCallStreamPart {
                tool_call_id: id.into(),
                tool_name: name.into(),
                args: args.into(),
                is_complete: complete,
                tool_index: None,
            })
        };
        let (a, b) = ("call-b833-0\nfc_q1b_0", "call-b833-1\nfc_q1b_1");
        let items = vec![
            part(a, "get_weather", "", false),
            part(a, "", "", false),
            part(a, "", r#"{"city":"Paris"#, false),
            part(a, "", r#""}"#, false),
            part(a, "get_weather", r#"{"city":"Paris"}"#, true),
            part(b, "get_weather", "", false),
            part(b, "", "", false),
            part(b, "", r#"{"city":"Tokyo"#, false),
            part(b, "", r#""}"#, false),
            part(b, "get_weather", r#"{"city":"Tokyo"}"#, true),
        ];
        let c = run(&req, items, true).1.unwrap();
        assert_eq!(c.finish_reason, FinishReason::ToolCalls);
        let calls: Vec<(&str, &str, &str)> = c
            .tool_calls
            .iter()
            .map(|t| (t.id.as_str(), t.name.as_str(), t.arguments.as_str()))
            .collect();
        assert_eq!(
            calls,
            vec![
                (a, "get_weather", r#"{"city":"Paris"}"#),
                (b, "get_weather", r#"{"city":"Tokyo"}"#),
            ]
        );

        // 收尾段没到（流被掐断）：用攒起来的增量。
        let cut = vec![
            part(a, "get_weather", "", false),
            part(a, "", r#"{"city":"Par"#, false),
            part(a, "", r#"is"}"#, false),
        ];
        let c = run(&req, cut, true).1.unwrap();
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].arguments, r#"{"city":"Paris"}"#);

        // 只有一段、直接 is_complete（有的模型不流参数）：也成一个调用。
        let single = vec![part(a, "get_weather", r#"{"city":"Rome"}"#, true)];
        let c = run(&req, single, true).1.unwrap();
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].arguments, r#"{"city":"Rome"}"#);
    }

    #[test]
    fn a_tool_call_without_an_id_gets_one_and_without_args_gets_an_empty_object() {
        let req = req_with(None, &[]);
        let c = run(
            &req,
            vec![Response::ToolCallPart(InferenceToolCallStreamPart {
                tool_name: "f".into(),
                ..Default::default()
            })],
            true,
        )
        .1
        .unwrap();
        assert!(!c.tool_calls[0].id.is_empty());
        assert_eq!(c.tool_calls[0].arguments, "{}");
    }

    #[test]
    fn thinking_streams_separately_from_text() {
        let req = req_with(None, &[]);
        let (deltas, done) = run(
            &req,
            vec![
                Response::ThinkingPart(InferenceThinkingStreamPart {
                    text: "hmm".into(),
                    ..Default::default()
                }),
                text("ok"),
            ],
            true,
        );
        assert_eq!(
            deltas,
            vec![Delta::Thinking("hmm".into()), Delta::Text("ok".into())]
        );
        let c = done.unwrap();
        assert_eq!((c.thinking.as_str(), c.text.as_str()), ("hmm", "ok"));
    }

    #[test]
    fn response_info_reasoning_fills_thinking_when_no_thinking_part() {
        let req = req_with(None, &[]);
        let (deltas, done) = run(
            &req,
            vec![
                Response::ResponseInfo(InferenceResponseInfo {
                    model: "grok-4-7-0910-xhigh".into(),
                    messages: vec![InferenceResponseMessage {
                        reasoning_parts: vec![InferenceReasoningPart {
                            text: "先想清楚".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                text("答"),
            ],
            true,
        );
        assert_eq!(
            deltas,
            vec![Delta::Thinking("先想清楚".into()), Delta::Text("答".into())]
        );
        let c = done.unwrap();
        assert_eq!(c.thinking, "先想清楚");
        assert_eq!(c.routed_model.as_deref(), Some("grok-4-7-0910-xhigh"));
    }

    #[test]
    fn response_info_reasoning_does_not_duplicate_streamed_thinking() {
        let req = req_with(None, &[]);
        let (deltas, done) = run(
            &req,
            vec![
                Response::ThinkingPart(InferenceThinkingStreamPart {
                    text: "hmm".into(),
                    ..Default::default()
                }),
                Response::ResponseInfo(InferenceResponseInfo {
                    messages: vec![InferenceResponseMessage {
                        reasoning_parts: vec![InferenceReasoningPart {
                            text: "hmm".into(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                text("ok"),
            ],
            true,
        );
        assert_eq!(
            deltas,
            vec![Delta::Thinking("hmm".into()), Delta::Text("ok".into())]
        );
        assert_eq!(done.unwrap().thinking, "hmm");
    }

    #[test]
    fn a_stop_sequence_ends_the_stream_early() {
        let req = req_with(None, &["END"]);
        let (deltas, done) = run(&req, vec![text("abc END def"), text("never")], false);
        assert_eq!(deltas, vec![Delta::Text("abc ".into())]);
        let c = done.unwrap();
        assert_eq!(c.text, "abc ");
        assert_eq!(c.finish_reason, FinishReason::Stop);
    }

    #[test]
    fn an_empty_stream_without_a_trailer_is_an_upstream_error_but_an_empty_clean_stream_is_not() {
        let req = req_with(None, &[]);
        assert_eq!(
            run(&req, vec![], false).1.unwrap_err().kind,
            UpstreamKind::Upstream
        );
        assert!(
            run(&req, vec![], true).1.is_ok(),
            "上游正常收尾但没内容，交回一个空回答"
        );
    }

    #[test]
    fn cua_probe_ignores_early_opus_and_error_info() {
        let mut acc = CuaRouteAcc::new("sand-cua");
        acc.on_response_info(
            "claude-opus-5-thinking-xhigh",
            Some("This model is unavailable for Grok Bot inference"),
        );
        assert!(!acc.ready());
        assert!(acc.chosen.is_none());

        acc.on_response_info("claude-opus-5-thinking-xhigh", None);
        assert!(acc.chosen.is_none(), "没出字的默认模型不当落点");

        acc.on_output();
        acc.on_response_info("gpt-5.6-luna-high", None);
        assert!(acc.ready());
        let p = acc.finish().unwrap();
        assert_eq!(p.resolved_model.as_deref(), Some("gpt-5.6-luna-high"));
    }

    #[test]
    fn cua_probe_trusts_grok47_without_waiting_for_tokens() {
        let mut acc = CuaRouteAcc::new("sand-cua");
        acc.on_response_info("grok-4-7-0910-xhigh", None);
        assert!(acc.ready());
        let p = acc.finish().unwrap();
        assert_eq!(p.resolved_model.as_deref(), Some("grok-4-7-0910-xhigh"));
    }

    #[test]
    fn cua_probe_thinking_without_model_explains_itself() {
        let mut acc = CuaRouteAcc::new("sand-cua");
        acc.on_output();
        let p = acc.finish().unwrap();
        assert!(p.resolved_model.is_none());
        assert!(p.note.as_deref().unwrap().contains("出了字"));
    }

    #[test]
    fn cua_probe_session_default_without_output_is_not_a_hit() {
        let mut acc = CuaRouteAcc::new("sand-cua");
        acc.on_response_info("claude-opus-5-thinking-xhigh", None);
        let p = acc.finish().unwrap();
        assert!(p.resolved_model.is_none());
        assert!(p
            .note
            .as_deref()
            .unwrap()
            .contains("claude-opus-5-thinking-xhigh"));
        assert!(p.note.as_deref().unwrap().contains("不像"));
    }

    #[test]
    fn cua_probe_accepts_unknown_shard_only_after_output() {
        let mut acc = CuaRouteAcc::new("sand-cua");
        acc.on_output();
        acc.on_response_info("grok-4.8-preview", None);
        assert!(!acc.ready(), "未知分片不提前挂断");
        let p = acc.finish().unwrap();
        assert_eq!(p.resolved_model.as_deref(), Some("grok-4.8-preview"));
    }
}
