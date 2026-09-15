//! IDE Agent 面板拦截：透传口上唯一「看懂内容」的路径——`/aiserver.v1.InferenceService/Stream`。
//!
//! 打了 Sand 补丁、且开了「推理经本机网关」的 Cursor，Agent 面板每一次模型调用都是一发这个
//! RPC：请求是**一个** Connect 信封（`InferenceStreamRequest`，整段对话历史都在 `messages` 里），
//! 响应是一串信封（文本 / 思考 / 工具调用 / 用量 / 错误）。透传口原本对它和别的路径一样盲转发；
//! 这里给它两件事：
//!
//! - **请求侧：可选的上下文改写。** 先只做一件最小的事——往目标 user 消息里塞一个哨兵字符串
//!   （[`RewriteRule`]）。目的不是功能，是**证明改写到达了模型**：在 Agent 面板让模型复述哨兵，
//!   复述得出来 + 账本里那一行 `rewritten=true`，两个证据对上才算通。改写在 protobuf 线格式的
//!   顶层逐字段做（`wire`），只重编码被改的那一条消息，其余字段字节不动——`proto.rs` 是按旧版
//!   Cursor 生成的，整包 decode → encode 会把新版才有的字段抹掉，上游不报错、语义悄悄少一段。
//!   Bot 通道：GLM 5.2 钉 [`GROKBOT_FORCED_MODEL_ID`]（`premium` → Codex）；
//!   grok 4.7 钉 [`GROKBOT_CUA_MODEL_ID`]（`sand-cua`）；其余原样。
//! - **响应侧：只读解码，记用量。** 帧原样转给 IDE，同时喂一份进解码器挑出 `extended_usage` /
//!   `response_info.model`（实际路由到的模型）/ 流内错误 / 流尾错误。一次 RPC 一行账本，粒度和
//!   Cursor dashboard 的「一次模型调用一行」一致。只读解码不怕 proto 旧：不认识的字段被忽略而已。
//!
//! 记的是**数字与名字**（会话 id、模型、token、耗时、有没有改写），不记对话内容；要看内容只在
//! 显式开 `NEXUS_PASSTHROUGH_DUMP_DIR` 取证时落盘。

use crate::connect::{EndStream, Envelope, EnvelopeDecoder};
use crate::error::UpstreamKind;
use crate::inference::{map_inference_error, map_upstream_error};
use crate::normalized::Usage;
use crate::proto::{
    inference_content_part, inference_stream_response, InferenceContentPart, InferenceContentParts,
    InferenceRequestedModel, InferenceStreamResponse, InferenceTextPart,
};
use crate::wire;
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::Instant;

pub const INFERENCE_STREAM_PATH: &str = "/aiserver.v1.InferenceService/Stream";

/// GLM 5.2 在 Bot 通道会被拒；改写成这个 routed alias，本号落到 Codex。
pub const GROKBOT_FORCED_MODEL_ID: &str = "premium";
/// grok 4.7 没有可直打的 slug；Bot 通道只能发这个 CUA 别名。
pub const GROKBOT_CUA_MODEL_ID: &str = "sand-cua";

/// 面板选 GLM 5.2 时走 `premium` 别名（隐藏入口）。其它模型一律原样。
pub fn grokbot_rewrites_to_premium(model_id: &str) -> bool {
    let k = model_id.trim().to_ascii_lowercase();
    k.contains("glm-5.2") || k.contains("glm5.2") || k.contains("glm_5.2")
}

/// 面板选 grok 4.7 时走 `sand-cua`。不要误伤 4.6 / opus-4-7。
pub fn grokbot_rewrites_to_cua(model_id: &str) -> bool {
    crate::models::is_grok47_request(model_id)
}

/// Bot 通道要把请求钉成哪个 routed alias。`None` = 原样转发。
pub fn grokbot_rewrite_target(model_id: &str) -> Option<&'static str> {
    if grokbot_rewrites_to_premium(model_id) {
        Some(GROKBOT_FORCED_MODEL_ID)
    } else if grokbot_rewrites_to_cua(model_id) {
        Some(GROKBOT_CUA_MODEL_ID)
    } else {
        None
    }
}

/// Bot 通道里不改写的请求。
pub fn grokbot_keeps_requested_model(model_id: &str) -> bool {
    grokbot_rewrite_target(model_id).is_none()
}

/// 最近记录只留这么多条给界面看；历史数字在账本里。
const RECENT_KEEP: usize = 50;
/// 哨兵不该长到能当 prompt 用。
pub const MARKER_MAX_CHARS: usize = 2000;

// `InferenceMessageRole::User`。用数字而不用 enum，是因为这里按线格式读 varint，不经过 prost。
const ROLE_USER: u64 = 1;
const TAG_MESSAGES: u32 = 1;
const TAG_MODEL_ID: u32 = 5;
const TAG_REQUESTED_MODEL: u32 = 7;
const TAG_CONVERSATION_ID: u32 = 8;
const TAG_MSG_ROLE: u32 = 1;
const TAG_MSG_TEXT: u32 = 2;
const TAG_MSG_PARTS: u32 = 3;

/// 哨兵放哪。`Tail`：最后一条 user 消息的末尾（模型最容易复述出来，验证最直接）；
/// `Head`：第一条 user 消息的开头（Cursor 把 system 折在这里，等于测「注入 system 级指令」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarkerPosition {
    Tail,
    Head,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RewriteRule {
    /// 默认关。改写是件该由用户自己点头的事。
    pub enabled: bool,
    pub position: MarkerPosition,
    /// 原样插入，不加分隔符——用户写什么进去就是什么。
    pub marker: String,
}

impl Default for RewriteRule {
    fn default() -> Self {
        Self {
            enabled: false,
            position: MarkerPosition::Tail,
            marker: "[nexus-mark]".into(),
        }
    }
}

impl RewriteRule {
    /// 界面传来的规则先过一遍：空哨兵等于没开；超长拒绝。
    pub fn validate(&self) -> Result<(), String> {
        if self.marker.chars().count() > MARKER_MAX_CHARS {
            return Err(format!("哨兵最多 {MARKER_MAX_CHARS} 个字符。"));
        }
        Ok(())
    }

    fn active(&self) -> bool {
        self.enabled && !self.marker.is_empty()
    }
}

/// 从请求里读出来的、可以公开记的几个名字。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RequestInfo {
    pub conversation_id: Option<String>,
    pub model_id: Option<String>,
    pub message_count: u32,
}

pub struct Rewritten {
    /// 要发给上游的信封字节。没改写时就是入参的拷贝。
    pub body: Vec<u8>,
    pub info: RequestInfo,
    pub rewritten: bool,
}

#[derive(Debug)]
pub enum InterceptError {
    /// 不是恰好一个未压缩的 Connect 信封。
    Envelope(&'static str),
    Wire(wire::WireError),
    Decode(prost::DecodeError),
}

impl fmt::Display for InterceptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InterceptError::Envelope(why) => write!(f, "信封不对：{why}"),
            InterceptError::Wire(e) => write!(f, "线格式：{e}"),
            InterceptError::Decode(e) => write!(f, "子消息解码：{e}"),
        }
    }
}

impl std::error::Error for InterceptError {}

impl From<wire::WireError> for InterceptError {
    fn from(e: wire::WireError) -> Self {
        InterceptError::Wire(e)
    }
}

impl From<prost::DecodeError> for InterceptError {
    fn from(e: prost::DecodeError) -> Self {
        InterceptError::Decode(e)
    }
}

/// 只读：请求里有哪个会话、要哪个模型、几条消息。
pub fn inspect_request(envelope: &[u8]) -> Result<RequestInfo, InterceptError> {
    let (_, payload) = split_envelope(envelope)?;
    let fields = wire::fields(payload)?;
    Ok(request_info(&fields))
}

/// 按规则改写请求。规则没开 / 找不到可改的 user 消息时 `rewritten == false`、字节原样。
///
/// 任何一步解不开都返回错误而不是半改半不改——调用方据此退回盲转发。
pub fn rewrite_request(envelope: &[u8], rule: &RewriteRule) -> Result<Rewritten, InterceptError> {
    let (flags, payload) = split_envelope(envelope)?;
    let fields = wire::fields(payload)?;
    let info = request_info(&fields);
    if !rule.active() {
        return Ok(Rewritten {
            body: envelope.to_vec(),
            info,
            rewritten: false,
        });
    }

    let message_slots: Vec<usize> = fields
        .iter()
        .enumerate()
        .filter(|(_, f)| f.tag == TAG_MESSAGES && f.wire_type == wire::WT_LEN)
        .map(|(i, _)| i)
        .collect();
    let order: Box<dyn Iterator<Item = &usize>> = match rule.position {
        MarkerPosition::Tail => Box::new(message_slots.iter().rev()),
        MarkerPosition::Head => Box::new(message_slots.iter()),
    };
    let mut replacement: Option<(usize, Vec<u8>)> = None;
    for &slot in order {
        let msg = fields[slot].value;
        if message_role(msg)? != Some(ROLE_USER) {
            continue;
        }
        if let Some(next) = rewrite_message(msg, &rule.marker, rule.position)? {
            replacement = Some((slot, next));
            break;
        }
    }
    let Some((slot, next)) = replacement else {
        return Ok(Rewritten {
            body: envelope.to_vec(),
            info,
            rewritten: false,
        });
    };

    let mut out = Vec::with_capacity(payload.len() + rule.marker.len() + 8);
    for (i, f) in fields.iter().enumerate() {
        if i == slot {
            wire::put_len_field(TAG_MESSAGES, &next, &mut out);
        } else {
            out.extend_from_slice(f.raw);
        }
    }
    Ok(Rewritten {
        body: Envelope { flags, data: out }.encode(),
        info,
        rewritten: true,
    })
}

/// Bot 通道把顶层 `model_id` 和 `requested_model` 钉成 routed tier（默认 [`GROKBOT_FORCED_MODEL_ID`]）。
///
/// 只重编码这两个字段，其余字节不动。`rewritten` 仍表示哨兵注入，这里不改那个旗标——调用方
/// 自己换 body / `info.model_id`。解不开就返回错误，让调用方退回原字节。
pub fn force_requested_model(
    envelope: &[u8],
    model_id: &str,
) -> Result<(Vec<u8>, RequestInfo), InterceptError> {
    let (flags, payload) = split_envelope(envelope)?;
    let fields = wire::fields(payload)?;
    let rm = InferenceRequestedModel {
        model_id: model_id.to_string(),
        ..Default::default()
    }
    .encode_to_vec();

    let mut out = Vec::with_capacity(payload.len() + model_id.len() + rm.len() + 16);
    let mut saw_model = false;
    let mut saw_rm = false;
    for f in &fields {
        if f.tag == TAG_MODEL_ID && f.wire_type == wire::WT_LEN {
            wire::put_len_field(TAG_MODEL_ID, model_id.as_bytes(), &mut out);
            saw_model = true;
        } else if f.tag == TAG_REQUESTED_MODEL && f.wire_type == wire::WT_LEN {
            wire::put_len_field(TAG_REQUESTED_MODEL, &rm, &mut out);
            saw_rm = true;
        } else {
            out.extend_from_slice(f.raw);
        }
    }
    if !saw_model {
        wire::put_len_field(TAG_MODEL_ID, model_id.as_bytes(), &mut out);
    }
    if !saw_rm {
        wire::put_len_field(TAG_REQUESTED_MODEL, &rm, &mut out);
    }
    let info = request_info(&wire::fields(&out)?);
    Ok((Envelope { flags, data: out }.encode(), info))
}

/// 请求体必须是**恰好一个**未压缩信封——ServerStreaming 的请求就是这样，多了少了都不对。
fn split_envelope(envelope: &[u8]) -> Result<(u8, &[u8]), InterceptError> {
    if envelope.len() < 5 {
        return Err(InterceptError::Envelope("不足 5 字节"));
    }
    let flags = envelope[0];
    let len = u32::from_be_bytes([envelope[1], envelope[2], envelope[3], envelope[4]]) as usize;
    if 5 + len != envelope.len() {
        return Err(InterceptError::Envelope(
            "长度前缀与请求体不符（不止一个信封？）",
        ));
    }
    if flags & Envelope::FLAG_COMPRESSED != 0 {
        return Err(InterceptError::Envelope("信封被压缩"));
    }
    Ok((flags, &envelope[5..]))
}

fn request_info(fields: &[wire::Field<'_>]) -> RequestInfo {
    let mut info = RequestInfo::default();
    for f in fields {
        if f.wire_type != wire::WT_LEN {
            continue;
        }
        match f.tag {
            TAG_MESSAGES => info.message_count += 1,
            TAG_CONVERSATION_ID => {
                info.conversation_id = std::str::from_utf8(f.value).ok().map(str::to_string)
            }
            TAG_REQUESTED_MODEL => {
                if let Ok(rm) = InferenceRequestedModel::decode(f.value) {
                    if !rm.model_id.is_empty() {
                        info.model_id = Some(rm.model_id);
                    }
                }
            }
            TAG_MODEL_ID if info.model_id.is_none() => {
                info.model_id = std::str::from_utf8(f.value)
                    .ok()
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            }
            _ => {}
        }
    }
    info
}

/// 一条 `InferenceCoreMessage` 的 role；没写 role 字段就是 `None`（proto3 默认 0 = UNSPECIFIED）。
fn message_role(msg: &[u8]) -> Result<Option<u64>, InterceptError> {
    for f in wire::fields(msg)? {
        if f.tag == TAG_MSG_ROLE && f.wire_type == wire::WT_VARINT {
            return Ok(Some(wire::varint_value(&f)?));
        }
    }
    Ok(None)
}

/// 往一条 user 消息里塞哨兵。`None` = 这条没有可改的文本（tool_content / 空消息），换下一条。
///
/// 只改 `text`（field 2）或 `parts`（field 3）那一个字段的字节，消息里其它字段——包括我们不认识的
/// ——原样保留。`parts` 那条走 prost 重编码（它的结构没有版本差异），其余都是线格式操作。
fn rewrite_message(
    msg: &[u8],
    marker: &str,
    position: MarkerPosition,
) -> Result<Option<Vec<u8>>, InterceptError> {
    let fields = wire::fields(msg)?;
    let text_slot = fields
        .iter()
        .position(|f| f.tag == TAG_MSG_TEXT && f.wire_type == wire::WT_LEN);
    let parts_slot = fields
        .iter()
        .position(|f| f.tag == TAG_MSG_PARTS && f.wire_type == wire::WT_LEN);

    let (slot, next_value) = if let Some(slot) = text_slot {
        let text = std::str::from_utf8(fields[slot].value)
            .map_err(|_| InterceptError::Envelope("user 文本不是 UTF-8"))?;
        (slot, splice(text, marker, position).into_bytes())
    } else if let Some(slot) = parts_slot {
        let mut parts = InferenceContentParts::decode(fields[slot].value)?;
        splice_parts(&mut parts, marker, position);
        (slot, parts.encode_to_vec())
    } else {
        return Ok(None);
    };

    let mut out = Vec::with_capacity(msg.len() + marker.len() + 8);
    for (i, f) in fields.iter().enumerate() {
        if i == slot {
            wire::put_len_field(f.tag, &next_value, &mut out);
        } else {
            out.extend_from_slice(f.raw);
        }
    }
    Ok(Some(out))
}

fn splice(text: &str, marker: &str, position: MarkerPosition) -> String {
    match position {
        MarkerPosition::Tail => format!("{text}{marker}"),
        MarkerPosition::Head => format!("{marker}{text}"),
    }
}

/// 多段内容（带图的消息）：改末尾 / 开头那个文本段；一个文本段都没有就补一段。
fn splice_parts(parts: &mut InferenceContentParts, marker: &str, position: MarkerPosition) {
    let text_index = match position {
        MarkerPosition::Tail => parts.parts.iter().rposition(is_text_part),
        MarkerPosition::Head => parts.parts.iter().position(is_text_part),
    };
    match text_index {
        Some(i) => {
            if let Some(inference_content_part::Part::Text(t)) = parts.parts[i].part.as_mut() {
                t.text = splice(&t.text, marker, position);
            }
        }
        None => {
            let part = InferenceContentPart {
                part: Some(inference_content_part::Part::Text(InferenceTextPart {
                    text: marker.to_string(),
                    provider_options: None,
                })),
            };
            match position {
                MarkerPosition::Tail => parts.parts.push(part),
                MarkerPosition::Head => parts.parts.insert(0, part),
            }
        }
    }
}

fn is_text_part(p: &InferenceContentPart) -> bool {
    matches!(p.part, Some(inference_content_part::Part::Text(_)))
}

// ---------------------------------------------------------------- 响应侧

/// 一次 Stream 响应看完之后攒下来的东西。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Collected {
    pub usage: Usage,
    /// 上游报了 `extended_usage`（或至少 `usage`）。
    pub measured: bool,
    /// `response_info.model`：上游实际路由到的模型。
    pub routed: Option<String>,
    /// 流内 `error` 或流尾 JSON 的 error，二者先到先记。
    pub error: Option<CollectedError>,
    /// 第一个正文 / 思考帧到达的时刻。
    pub first_output_at: Option<Instant>,
    /// 见到了 END_STREAM 信封。没见到 = 连接在中途断了。
    pub saw_end: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollectedError {
    pub status: u16,
    pub kind: UpstreamKind,
    pub message: String,
}

/// 响应帧的只读解码器。字节按到达顺序 `push` 进来，`finish` 拿结果。
#[derive(Default)]
pub struct UsageCollector {
    decoder: EnvelopeDecoder,
    collected: Collected,
    framing_broken: bool,
}

impl UsageCollector {
    pub fn push(&mut self, bytes: &[u8]) {
        if self.framing_broken {
            return;
        }
        self.decoder.push(bytes);
        loop {
            match self.decoder.next_envelope() {
                Ok(Some(env)) => self.on_envelope(env),
                Ok(None) => break,
                Err(err) => {
                    tracing::debug!(%err, "IDE 拦截：响应分帧失败，停止解码（帧仍原样转发）");
                    self.framing_broken = true;
                    break;
                }
            }
        }
    }

    fn on_envelope(&mut self, env: Envelope) {
        if env.is_compressed() {
            return;
        }
        if env.is_end_stream() {
            self.collected.saw_end = true;
            if env.data.is_empty() {
                return;
            }
            if let Ok(end) = serde_json::from_slice::<EndStream>(&env.data) {
                if let Some(e) = end.error {
                    let detail = e.cursor_info().format();
                    let (status, kind) = map_upstream_error(&e.code, &detail);
                    self.collected.error.get_or_insert(CollectedError {
                        status,
                        kind,
                        message: detail,
                    });
                }
            }
            return;
        }
        let Ok(resp) = InferenceStreamResponse::decode(&env.data[..]) else {
            return;
        };
        use inference_stream_response::Response as R;
        match resp.response {
            Some(R::TextPart(_)) | Some(R::ThinkingPart(_)) | Some(R::ToolCallPart(_)) => {
                self.collected
                    .first_output_at
                    .get_or_insert_with(Instant::now);
            }
            Some(R::ExtendedUsage(u)) => {
                self.collected.usage = Usage {
                    input_tokens: clamp(u.input_tokens),
                    output_tokens: clamp(u.output_tokens),
                    cache_read_tokens: clamp(u.cache_read_tokens),
                    cache_write_tokens: clamp(u.cache_write_tokens),
                    reasoning_tokens: 0,
                };
                self.collected.measured = true;
            }
            Some(R::Usage(u)) if !self.collected.measured => {
                self.collected.usage.input_tokens = clamp(u.prompt_tokens);
                self.collected.usage.output_tokens = clamp(u.completion_tokens);
                self.collected.measured = true;
            }
            Some(R::ResponseInfo(i)) => {
                if !i.model.is_empty() {
                    self.collected.routed = Some(i.model);
                }
            }
            Some(R::Error(e)) => {
                let (status, kind) = map_inference_error(&e);
                let message = if e.message.is_empty() {
                    e.code
                } else {
                    e.message
                };
                self.collected.error.get_or_insert(CollectedError {
                    status,
                    kind,
                    message,
                });
            }
            _ => {}
        }
    }

    pub fn finish(self) -> Collected {
        self.collected
    }
}

fn clamp(v: i32) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

// ---------------------------------------------------------------- 记录与状态

/// 界面「最近请求」里的一行。只有名字和数字。
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InterceptRecord {
    pub at: String,
    pub account: String,
    pub conversation_id: Option<String>,
    pub model: String,
    pub routed: Option<String>,
    pub ok: bool,
    pub status: u16,
    pub kind: Option<String>,
    pub error: Option<String>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub measured: bool,
    pub rewritten: bool,
    pub message_count: u32,
    pub ttft_ms: Option<u64>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InterceptSnapshot {
    pub rule: RewriteRule,
    /// 本进程内的累计（网关重启归零）；跨天的数字看账本。
    pub calls: u64,
    pub rewritten: u64,
    pub errors: u64,
    pub recent: Vec<InterceptRecord>,
}

/// 透传口与服务层共享的一份状态：当前规则（可热改，不用重启网关）+ 最近记录。
pub struct InterceptHub {
    rule: RwLock<RewriteRule>,
    recent: Mutex<VecDeque<InterceptRecord>>,
    calls: AtomicU64,
    rewritten: AtomicU64,
    errors: AtomicU64,
}

impl InterceptHub {
    pub fn new(rule: RewriteRule) -> Self {
        Self {
            rule: RwLock::new(rule),
            recent: Mutex::new(VecDeque::with_capacity(RECENT_KEEP)),
            calls: AtomicU64::new(0),
            rewritten: AtomicU64::new(0),
            errors: AtomicU64::new(0),
        }
    }

    pub fn rule(&self) -> RewriteRule {
        self.rule.read().expect("intercept rule").clone()
    }

    pub fn set_rule(&self, rule: RewriteRule) {
        *self.rule.write().expect("intercept rule") = rule;
    }

    pub fn record(&self, rec: InterceptRecord) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if rec.rewritten {
            self.rewritten.fetch_add(1, Ordering::Relaxed);
        }
        if !rec.ok {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
        let mut recent = self.recent.lock().expect("intercept recent");
        if recent.len() == RECENT_KEEP {
            recent.pop_back();
        }
        recent.push_front(rec);
    }

    pub fn snapshot(&self) -> InterceptSnapshot {
        InterceptSnapshot {
            rule: self.rule(),
            calls: self.calls.load(Ordering::Relaxed),
            rewritten: self.rewritten.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            recent: self
                .recent
                .lock()
                .expect("intercept recent")
                .iter()
                .cloned()
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::{
        inference_core_message, InferenceCoreMessage, InferenceExtendedUsageInfo,
        InferenceResponseInfo, InferenceStreamError, InferenceStreamErrorType,
        InferenceStreamRequest, InferenceTextStreamPart,
    };

    fn text_msg(role: i32, text: &str) -> InferenceCoreMessage {
        InferenceCoreMessage {
            role,
            content: Some(inference_core_message::Content::Text(text.into())),
            ..Default::default()
        }
    }

    fn request(messages: Vec<InferenceCoreMessage>) -> InferenceStreamRequest {
        InferenceStreamRequest {
            messages,
            requested_model: Some(InferenceRequestedModel {
                model_id: "claude-opus-5".into(),
                ..Default::default()
            }),
            conversation_id: Some("conv-42".into()),
            ..Default::default()
        }
    }

    fn envelope_of(req: &InferenceStreamRequest) -> Vec<u8> {
        Envelope::message(req.encode_to_vec()).encode()
    }

    fn decode_body(body: &[u8]) -> InferenceStreamRequest {
        let (_, payload) = split_envelope(body).unwrap();
        InferenceStreamRequest::decode(payload).unwrap()
    }

    fn rule(position: MarkerPosition) -> RewriteRule {
        RewriteRule {
            enabled: true,
            position,
            marker: "[nexus-mark]".into(),
        }
    }

    fn text_of(m: &InferenceCoreMessage) -> &str {
        match &m.content {
            Some(inference_core_message::Content::Text(t)) => t,
            _ => panic!("不是文本消息"),
        }
    }

    #[test]
    fn inspect_reads_conversation_model_and_message_count() {
        let env = envelope_of(&request(vec![
            text_msg(1, "a"),
            text_msg(2, "b"),
            text_msg(1, "c"),
        ]));
        let info = inspect_request(&env).unwrap();
        assert_eq!(info.conversation_id.as_deref(), Some("conv-42"));
        assert_eq!(info.model_id.as_deref(), Some("claude-opus-5"));
        assert_eq!(info.message_count, 3);
    }

    #[test]
    fn disabled_or_empty_marker_leaves_bytes_untouched() {
        let env = envelope_of(&request(vec![text_msg(1, "hi")]));
        let off = rewrite_request(&env, &RewriteRule::default()).unwrap();
        assert!(!off.rewritten);
        assert_eq!(off.body, env);
        let empty = rewrite_request(
            &env,
            &RewriteRule {
                enabled: true,
                marker: String::new(),
                ..RewriteRule::default()
            },
        )
        .unwrap();
        assert!(!empty.rewritten);
        assert_eq!(empty.body, env);
    }

    #[test]
    fn tail_appends_to_the_last_user_message_only() {
        let env = envelope_of(&request(vec![
            text_msg(1, "first"),
            text_msg(2, "reply"),
            text_msg(1, "last"),
        ]));
        let out = rewrite_request(&env, &rule(MarkerPosition::Tail)).unwrap();
        assert!(out.rewritten);
        let req = decode_body(&out.body);
        assert_eq!(text_of(&req.messages[0]), "first");
        assert_eq!(text_of(&req.messages[1]), "reply");
        assert_eq!(text_of(&req.messages[2]), "last[nexus-mark]");
        assert_eq!(req.conversation_id.as_deref(), Some("conv-42"));
        assert_eq!(out.info.message_count, 3);
    }

    #[test]
    fn head_prepends_to_the_first_user_message_only() {
        let env = envelope_of(&request(vec![
            text_msg(2, "assistant-first?"),
            text_msg(1, "first user"),
            text_msg(1, "second user"),
        ]));
        let out = rewrite_request(&env, &rule(MarkerPosition::Head)).unwrap();
        let req = decode_body(&out.body);
        assert_eq!(text_of(&req.messages[0]), "assistant-first?");
        assert_eq!(text_of(&req.messages[1]), "[nexus-mark]first user");
        assert_eq!(text_of(&req.messages[2]), "second user");
    }

    /// 最后一条 user 是 tool 结果 / 没有文本时不能硬塞，要往前找上一条真正的 user 文本。
    #[test]
    fn tail_skips_user_messages_without_text_content() {
        let tool_result = InferenceCoreMessage {
            role: 1,
            content: Some(inference_core_message::Content::ToolContent(
                Default::default(),
            )),
            ..Default::default()
        };
        let env = envelope_of(&request(vec![
            text_msg(1, "ask"),
            text_msg(2, "calling tool"),
            tool_result,
        ]));
        let out = rewrite_request(&env, &rule(MarkerPosition::Tail)).unwrap();
        assert!(out.rewritten);
        let req = decode_body(&out.body);
        assert_eq!(text_of(&req.messages[0]), "ask[nexus-mark]");
        assert!(matches!(
            req.messages[2].content,
            Some(inference_core_message::Content::ToolContent(_))
        ));
    }

    #[test]
    fn no_user_message_means_no_rewrite() {
        let env = envelope_of(&request(vec![text_msg(2, "only assistant")]));
        let out = rewrite_request(&env, &rule(MarkerPosition::Tail)).unwrap();
        assert!(!out.rewritten);
        assert_eq!(out.body, env);
    }

    #[test]
    fn parts_messages_get_the_marker_on_a_text_part_or_a_new_one() {
        let with_text = InferenceCoreMessage {
            role: 1,
            content: Some(inference_core_message::Content::Parts(
                InferenceContentParts {
                    parts: vec![
                        InferenceContentPart {
                            part: Some(inference_content_part::Part::Text(InferenceTextPart {
                                text: "look".into(),
                                provider_options: None,
                            })),
                        },
                        InferenceContentPart {
                            part: Some(inference_content_part::Part::Image(Default::default())),
                        },
                    ],
                },
            )),
            ..Default::default()
        };
        let env = envelope_of(&request(vec![with_text]));
        let req = decode_body(
            &rewrite_request(&env, &rule(MarkerPosition::Tail))
                .unwrap()
                .body,
        );
        let Some(inference_core_message::Content::Parts(p)) = &req.messages[0].content else {
            panic!()
        };
        assert_eq!(p.parts.len(), 2, "图片段保留，不新增段");
        let Some(inference_content_part::Part::Text(t)) = &p.parts[0].part else {
            panic!()
        };
        assert_eq!(t.text, "look[nexus-mark]");

        let image_only = InferenceCoreMessage {
            role: 1,
            content: Some(inference_core_message::Content::Parts(
                InferenceContentParts {
                    parts: vec![InferenceContentPart {
                        part: Some(inference_content_part::Part::Image(Default::default())),
                    }],
                },
            )),
            ..Default::default()
        };
        let env = envelope_of(&request(vec![image_only]));
        let req = decode_body(
            &rewrite_request(&env, &rule(MarkerPosition::Head))
                .unwrap()
                .body,
        );
        let Some(inference_core_message::Content::Parts(p)) = &req.messages[0].content else {
            panic!()
        };
        assert_eq!(p.parts.len(), 2);
        let Some(inference_content_part::Part::Text(t)) = &p.parts[0].part else {
            panic!("Head 该在最前面补一段文本")
        };
        assert_eq!(t.text, "[nexus-mark]");
    }

    /// 核心保证：`proto.rs` 不认识的字段（模拟新版 Cursor 才有的 75 / 77 / 79）在改写后**逐字节**还在。
    #[test]
    fn unknown_top_level_and_message_fields_survive_a_rewrite() {
        let mut payload = request(vec![text_msg(1, "hi")]).encode_to_vec();
        // 顶层：field 75 bool、field 79 bytes —— 随便编，proto.rs 里没有。
        wire::put_varint((75 << 3) | u64::from(wire::WT_VARINT), &mut payload);
        wire::put_varint(1, &mut payload);
        wire::put_len_field(79, b"secret-key-bytes", &mut payload);
        // 消息内：给那条 user 消息末尾追加一个 field 40 字符串，然后把消息重新包回 field 1。
        let fields = wire::fields(&payload).unwrap();
        let mut rebuilt = Vec::new();
        for f in &fields {
            if f.tag == TAG_MESSAGES {
                let mut msg = f.value.to_vec();
                wire::put_len_field(40, b"future-field", &mut msg);
                wire::put_len_field(TAG_MESSAGES, &msg, &mut rebuilt);
            } else {
                rebuilt.extend_from_slice(f.raw);
            }
        }
        let env = Envelope::message(rebuilt).encode();

        let out = rewrite_request(&env, &rule(MarkerPosition::Tail)).unwrap();
        assert!(out.rewritten);
        let (_, after) = split_envelope(&out.body).unwrap();
        let top = wire::fields(after).unwrap();
        let f75 = top.iter().find(|f| f.tag == 75).expect("field 75 丢了");
        assert_eq!(wire::varint_value(f75).unwrap(), 1);
        let f79 = top.iter().find(|f| f.tag == 79).expect("field 79 丢了");
        assert_eq!(f79.value, b"secret-key-bytes");
        let msg = top.iter().find(|f| f.tag == TAG_MESSAGES).unwrap();
        let inner = wire::fields(msg.value).unwrap();
        assert_eq!(
            inner.iter().find(|f| f.tag == 40).unwrap().value,
            b"future-field"
        );
        assert_eq!(
            inner.iter().find(|f| f.tag == TAG_MSG_TEXT).unwrap().value,
            b"hi[nexus-mark]"
        );
        // 顶层字段顺序不变。
        let tags: Vec<u32> = top.iter().map(|f| f.tag).collect();
        assert_eq!(tags, vec![1, 7, 8, 75, 79]);
    }

    #[test]
    fn force_requested_model_pins_premium_and_keeps_unknown_fields() {
        let mut req = request(vec![text_msg(1, "hi")]);
        req.model_id = Some("claude-opus-5".into());
        req.requested_model = Some(InferenceRequestedModel {
            model_id: "claude-opus-5".into(),
            max_mode: true,
            ..Default::default()
        });
        let mut payload = req.encode_to_vec();
        wire::put_varint((75 << 3) | u64::from(wire::WT_VARINT), &mut payload);
        wire::put_varint(1, &mut payload);
        wire::put_len_field(79, b"secret-key-bytes", &mut payload);
        let env = Envelope::message(payload).encode();

        let (out, info) = force_requested_model(&env, GROKBOT_FORCED_MODEL_ID).unwrap();
        let decoded = decode_body(&out);
        assert_eq!(decoded.model_id.as_deref(), Some("premium"));
        let rm = decoded.requested_model.expect("requested_model");
        assert_eq!(rm.model_id, "premium");
        assert!(!rm.max_mode);
        assert!(rm.parameters.is_empty());
        assert_eq!(info.model_id.as_deref(), Some("premium"));
        assert_eq!(decoded.conversation_id.as_deref(), Some("conv-42"));

        let (_, after) = split_envelope(&out).unwrap();
        let top = wire::fields(after).unwrap();
        assert_eq!(
            top.iter().find(|f| f.tag == 79).unwrap().value,
            b"secret-key-bytes"
        );
        assert!(matches!(
            force_requested_model(b"x", GROKBOT_FORCED_MODEL_ID),
            Err(InterceptError::Envelope(_))
        ));
    }

    #[test]
    fn grokbot_only_rewrites_glm52_to_premium() {
        for id in [
            "glm-5.2",
            "GLM-5.2",
            "glm-5.2-high",
            "cursor-glm-5.2",
            "glm5.2",
            "glm_5.2",
        ] {
            assert!(grokbot_rewrites_to_premium(id), "{id} 该钉 premium");
            assert!(!grokbot_keeps_requested_model(id), "{id} 不该原样");
        }
        for id in [
            "grok-4.6",
            "composer-2.5",
            "default",
            "sand-default",
            "auto",
            "auto-high",
            "premium",
            "claude-opus-5",
            "gpt-5.6-luna",
            "gpt-5.3-codex",
            "gemini-3-flash",
            "claude-haiku-4-5",
            "sand-cua",
            "",
        ] {
            assert!(grokbot_keeps_requested_model(id), "{id} 该原样");
        }
        for id in ["grok-4.7", "grok-4-7-0910-xhigh", "cursor-grok-4.7-high"] {
            assert_eq!(
                grokbot_rewrite_target(id),
                Some(GROKBOT_CUA_MODEL_ID),
                "{id}"
            );
            assert!(!grokbot_keeps_requested_model(id), "{id} 该钉 sand-cua");
        }
    }

    #[test]
    fn malformed_envelopes_are_refused_not_half_rewritten() {
        let req = request(vec![text_msg(1, "hi")]).encode_to_vec();
        let mut two = Envelope::message(req.clone()).encode();
        two.extend(Envelope::message(req.clone()).encode());
        assert!(matches!(
            rewrite_request(&two, &rule(MarkerPosition::Tail)),
            Err(InterceptError::Envelope(_))
        ));
        let compressed = Envelope {
            flags: Envelope::FLAG_COMPRESSED,
            data: req,
        }
        .encode();
        assert!(matches!(
            rewrite_request(&compressed, &rule(MarkerPosition::Tail)),
            Err(InterceptError::Envelope(_))
        ));
        assert!(matches!(
            rewrite_request(&[1, 2, 3], &rule(MarkerPosition::Tail)),
            Err(InterceptError::Envelope(_))
        ));
    }

    #[test]
    fn rule_validation_caps_marker_length() {
        assert!(RewriteRule::default().validate().is_ok());
        let long = RewriteRule {
            marker: "x".repeat(MARKER_MAX_CHARS + 1),
            ..RewriteRule::default()
        };
        assert!(long.validate().is_err());
    }

    fn frame(resp: inference_stream_response::Response) -> Vec<u8> {
        Envelope::message(
            InferenceStreamResponse {
                response: Some(resp),
            }
            .encode_to_vec(),
        )
        .encode()
    }

    #[test]
    fn collector_reads_usage_routed_model_and_a_clean_end() {
        use inference_stream_response::Response as R;
        let mut wire_bytes = Vec::new();
        wire_bytes.extend(frame(R::ResponseInfo(InferenceResponseInfo {
            model: "claude-opus-5-thinking-high".into(),
            ..Default::default()
        })));
        wire_bytes.extend(frame(R::TextPart(InferenceTextStreamPart {
            text: "hello".into(),
            is_final: false,
        })));
        wire_bytes.extend(frame(R::ExtendedUsage(InferenceExtendedUsageInfo {
            input_tokens: 120,
            output_tokens: 30,
            cache_read_tokens: 100,
            cache_write_tokens: 5,
            max_tokens: 200_000,
        })));
        wire_bytes.extend(
            Envelope {
                flags: Envelope::FLAG_END_STREAM,
                data: b"{}".to_vec(),
            }
            .encode(),
        );

        // 一个字节一个字节喂，分片边界不影响结果。
        let mut c = UsageCollector::default();
        for b in &wire_bytes {
            c.push(std::slice::from_ref(b));
        }
        let got = c.finish();
        assert!(got.saw_end);
        assert!(got.measured);
        assert_eq!(got.usage.input_tokens, 120);
        assert_eq!(got.usage.output_tokens, 30);
        assert_eq!(got.usage.cache_read_tokens, 100);
        assert_eq!(got.usage.cache_write_tokens, 5);
        assert_eq!(got.routed.as_deref(), Some("claude-opus-5-thinking-high"));
        assert!(got.first_output_at.is_some());
        assert!(got.error.is_none());
    }

    #[test]
    fn collector_records_in_stream_and_end_stream_errors() {
        use inference_stream_response::Response as R;
        let mut c = UsageCollector::default();
        c.push(&frame(R::Error(InferenceStreamError {
            message: "rate limited".into(),
            code: "ERROR_RATE_LIMITED".into(),
            error_type: InferenceStreamErrorType::RateLimit as i32,
            ..Default::default()
        })));
        let got = c.finish();
        let err = got.error.expect("流内错误该被记下");
        assert_eq!(err.kind, UpstreamKind::RateLimit);
        assert_eq!(err.status, 429);
        assert!(!got.saw_end);

        let mut c = UsageCollector::default();
        c.push(
            &Envelope {
                flags: Envelope::FLAG_END_STREAM,
                data: br#"{"error":{"code":"unauthenticated","message":"bad token"}}"#.to_vec(),
            }
            .encode(),
        );
        let got = c.finish();
        assert!(got.saw_end);
        assert_eq!(got.error.unwrap().kind, UpstreamKind::Auth);
    }

    #[test]
    fn hub_keeps_the_newest_records_first_and_counts() {
        let hub = InterceptHub::new(RewriteRule::default());
        for i in 0..(RECENT_KEEP + 3) {
            hub.record(InterceptRecord {
                at: String::new(),
                account: "a".into(),
                conversation_id: Some(format!("c{i}")),
                model: "m".into(),
                routed: None,
                ok: i % 2 == 0,
                status: 200,
                kind: None,
                error: None,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                measured: true,
                rewritten: i % 3 == 0,
                message_count: 1,
                ttft_ms: None,
                duration_ms: 0,
            });
        }
        let snap = hub.snapshot();
        assert_eq!(snap.recent.len(), RECENT_KEEP);
        assert_eq!(snap.recent[0].conversation_id.as_deref(), Some("c52"));
        assert_eq!(snap.calls, 53);
        assert_eq!(snap.errors, 26);
        assert_eq!(snap.rewritten, 18);
        hub.set_rule(RewriteRule {
            enabled: true,
            ..RewriteRule::default()
        });
        assert!(hub.snapshot().rule.enabled);
    }
}
