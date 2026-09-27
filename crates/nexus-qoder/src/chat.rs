//! 聊天请求体，以及 SSE 信封里的增量。
//!
//! 上游要的是 OpenAI 形状的 messages / tools，外面再套一层 Qoder 自己的
//! `chat_context` / `model_config`。`system` 顶层字段服务端会丢掉，系统提示词
//! 必须是一条 `role: system` 的消息。

use crate::protocol::{ResolvedModel, MAX_OUTPUT_TOKENS};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InRole {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone)]
pub struct InImage {
    pub mime: String,
    pub data: String,
}

#[derive(Debug, Clone)]
pub struct InToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone)]
pub struct InTurn {
    pub role: InRole,
    pub text: String,
    pub images: Vec<InImage>,
    pub tool_calls: Vec<InToolCall>,
    pub tool_call_id: String,
}

#[derive(Debug, Clone)]
pub struct InTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StreamUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub reasoning_tokens: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamPiece {
    Text(String),
    Thinking(String),
    /// 同一个 index 的参数是拼起来的。id / name 哪一帧到了哪一帧带。
    Tool {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: String,
    },
    Usage(StreamUsage),
    Finish(String),
    Done,
    Failed(String),
}

pub fn build_body(
    model: &ResolvedModel,
    user_id: &str,
    session_id: &str,
    turns: &[InTurn],
    tools: &[InTool],
    max_tokens: Option<u32>,
) -> Value {
    let max_tokens = max_tokens
        .unwrap_or(MAX_OUTPUT_TOKENS)
        .clamp(1, MAX_OUTPUT_TOKENS);
    let messages = messages_of(turns);
    let tool_value = tools_of(tools);
    let last_user = last_user_text(&messages);
    let record_id = uuid::Uuid::new_v4().simple().to_string();
    let record = &record_id[..16];
    let session = format!(
        "{}-{}",
        short_hash(&["qoder-session", user_id, &model.key]),
        if session_id.trim().is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            session_id.trim().to_string()
        }
    );
    let mut parameters = json!({ "max_tokens": max_tokens });
    if model.reasoning || model.effort.is_some() {
        parameters["enable_thinking"] = json!(true);
        if let Some(effort) = model.effort.as_deref().filter(|_| model.supports_effort) {
            parameters["reasoning_effort"] = json!(effort);
        }
    } else {
        parameters["enable_thinking"] = json!(false);
    }
    let model_config = json!({
        "key": model.key,
        "display_name": model.id,
        "is_reasoning": model.reasoning,
        "enable": true,
        "source": "system",
    });
    json!({
        "request_id": uuid::Uuid::new_v4().to_string(),
        "request_set_id": record,
        "chat_record_id": record,
        "session_id": session,
        "stream": true,
        "chat_task": "FREE_INPUT",
        "is_reply": true,
        "is_retry": false,
        "source": 1,
        "version": "3",
        "session_type": "qodercli",
        "agent_id": "agent_common",
        "task_id": "common",
        "code_language": "",
        "chat_prompt": "",
        "image_urls": Value::Null,
        "aliyun_user_type": "",
        "system": "",
        "messages": messages,
        "tools": tool_value,
        "parameters": parameters,
        "chat_context": {
            "chatPrompt": "",
            "imageUrls": Value::Null,
            "extra": {
                "context": [],
                "modelConfig": {
                    "key": model.key,
                    "is_reasoning": model.reasoning,
                },
                "originalContent": last_user,
            },
            "features": [],
            "text": last_user,
        },
        "model_config": model_config,
        "business": {
            "product": "cli",
            "version": "1.0.0",
            "type": "agent",
            "stage": "start",
            "id": uuid::Uuid::new_v4().to_string(),
            "name": last_user.chars().take(30).collect::<String>(),
            "begin_at": unix_millis(),
        },
    })
}

fn messages_of(turns: &[InTurn]) -> Vec<Value> {
    let mut out = Vec::new();
    let system = turns
        .iter()
        .filter(|t| t.role == InRole::System)
        .map(|t| t.text.trim())
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if !system.is_empty() {
        out.push(json!({ "role": "system", "content": system }));
    }
    for turn in turns {
        match turn.role {
            InRole::System => {}
            InRole::User => out.push(user_message(turn)),
            InRole::Assistant => out.push(assistant_message(turn)),
            InRole::Tool => {
                let id = if !turn.tool_call_id.is_empty() {
                    turn.tool_call_id.clone()
                } else {
                    turn.tool_calls
                        .first()
                        .map(|c| c.id.clone())
                        .unwrap_or_default()
                };
                if id.is_empty() && turn.text.is_empty() {
                    continue;
                }
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": turn.text,
                }));
            }
        }
    }
    out
}

fn user_message(turn: &InTurn) -> Value {
    if turn.images.is_empty() {
        return json!({ "role": "user", "content": turn.text });
    }
    let mut parts = Vec::new();
    if !turn.text.is_empty() {
        parts.push(json!({ "type": "text", "text": turn.text }));
    }
    for image in &turn.images {
        parts.push(json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{};base64,{}", image.mime, image.data) },
        }));
    }
    json!({ "role": "user", "content": parts })
}

fn assistant_message(turn: &InTurn) -> Value {
    let calls: Vec<Value> = turn
        .tool_calls
        .iter()
        .map(|call| {
            json!({
                "id": call.id,
                "type": "function",
                "function": { "name": call.name, "arguments": call.arguments },
            })
        })
        .collect();
    let content = if !turn.text.is_empty() {
        Value::String(turn.text.clone())
    } else if !calls.is_empty() {
        // 网关会丢掉 content 为 null 的 assistant，后面的 tool 结果就成了孤儿。
        Value::String(" ".into())
    } else {
        Value::Null
    };
    let mut message = json!({ "role": "assistant", "content": content });
    if !calls.is_empty() {
        message["tool_calls"] = Value::Array(calls);
    }
    message
}

fn tools_of(tools: &[InTool]) -> Value {
    Value::Array(
        tools
            .iter()
            .filter(|t| !t.name.trim().is_empty())
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    },
                })
            })
            .collect(),
    )
}

fn last_user_text(messages: &[Value]) -> String {
    for message in messages.iter().rev() {
        if message.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        return match message.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(""),
            _ => String::new(),
        };
    }
    String::new()
}

/// 解析一条 `data:` 后面的载荷。认信封（`{ body }`）也认直接的 chat chunk。
pub fn parse_data(data: &str) -> Vec<StreamPiece> {
    let data = data.trim();
    if data.is_empty() {
        return Vec::new();
    }
    if data == "[DONE]" {
        return vec![StreamPiece::Done];
    }
    let Ok(value) = serde_json::from_str::<Value>(data) else {
        return Vec::new();
    };
    if let Some(code) = value.get("statusCodeValue").and_then(|v| v.as_i64()) {
        if code != 200 {
            let body = value.get("body").map(value_text).unwrap_or_default();
            return vec![StreamPiece::Failed(format!("上游状态 {code}：{body}"))];
        }
    }
    if let Some(body) = value.get("body") {
        let text = value_text(body);
        if text.trim() == "[DONE]" {
            return vec![StreamPiece::Done];
        }
        if text.is_empty() {
            return Vec::new();
        }
        let Ok(inner) = serde_json::from_str::<Value>(&text) else {
            return Vec::new();
        };
        return pieces_of(&inner);
    }
    pieces_of(&value)
}

fn pieces_of(inner: &Value) -> Vec<StreamPiece> {
    let mut out = Vec::new();
    if let Some(usage) = inner.get("usage") {
        out.push(StreamPiece::Usage(usage_of(usage)));
    }
    let Some(choice) = inner
        .get("choices")
        .and_then(|c| c.as_array())
        .and_then(|a| a.first())
    else {
        return out;
    };
    if let Some(delta) = choice.get("delta") {
        if let Some(reasoning) = delta.get("reasoning_content").and_then(|v| v.as_str()) {
            let cleaned = strip_thinking_tags(reasoning);
            if !cleaned.is_empty() {
                out.push(StreamPiece::Thinking(cleaned));
            }
        }
        if let Some(content) = delta.get("content").and_then(|v| v.as_str()) {
            if !content.is_empty() {
                out.push(StreamPiece::Text(content.to_string()));
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for call in calls {
                let index = call.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                out.push(StreamPiece::Tool {
                    index,
                    id: call.get("id").and_then(|v| v.as_str()).map(str::to_string),
                    name: call
                        .pointer("/function/name")
                        .and_then(|v| v.as_str())
                        .map(str::to_string),
                    arguments: call
                        .pointer("/function/arguments")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                });
            }
        }
    }
    if let Some(reason) = choice.get("finish_reason").and_then(|v| v.as_str()) {
        if !reason.is_empty() && reason != "null" {
            out.push(StreamPiece::Finish(reason.to_string()));
        }
    }
    out
}

fn usage_of(usage: &Value) -> StreamUsage {
    let prompt = json_u32(usage.get("prompt_tokens"));
    let cache_read = json_u32(usage.pointer("/prompt_tokens_details/cached_tokens"));
    let cache_write = json_u32(usage.pointer("/prompt_tokens_details/cache_write_tokens"));
    StreamUsage {
        input_tokens: prompt
            .saturating_sub(cache_read)
            .saturating_sub(cache_write),
        output_tokens: json_u32(usage.get("completion_tokens")),
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        reasoning_tokens: json_u32(usage.pointer("/completion_tokens_details/reasoning_tokens")),
    }
}

fn json_u32(value: Option<&Value>) -> u32 {
    value
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .min(u32::MAX as u64) as u32
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

const THINKING_TAGS: &[(&str, &str)] = &[
    ("<thinking>", "</thinking>"),
    ("<think>", "</think>"),
    ("<reasoning>", "</reasoning>"),
    ("<thought>", "</thought>"),
];

pub fn strip_thinking_tags(text: &str) -> String {
    let mut out = text.to_string();
    for (open, close) in THINKING_TAGS {
        out = out.replace(open, "").replace(close, "");
    }
    out
}

/// 内容通道里的 `<thinking>` 块拆出来。标签可能跨好几个增量。
#[derive(Debug, Default)]
pub struct ThinkingSplit {
    buf: String,
    close: Option<&'static str>,
    /// 已经见过一对标签之后，后面的正文不再进思考。
    after: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitOut {
    Text(String),
    Thinking(String),
}

impl ThinkingSplit {
    pub fn push(&mut self, chunk: &str) -> Vec<SplitOut> {
        self.buf.push_str(chunk);
        self.drain(false)
    }

    pub fn finish(&mut self) -> Vec<SplitOut> {
        self.drain(true)
    }

    fn drain(&mut self, flush: bool) -> Vec<SplitOut> {
        let mut out = Vec::new();
        loop {
            if self.after {
                self.emit_text(&mut out, flush);
                break;
            }
            if let Some(close) = self.close {
                if let Some(pos) = self.buf.find(close) {
                    if pos > 0 {
                        out.push(SplitOut::Thinking(self.buf[..pos].to_string()));
                    }
                    self.buf = self.buf[pos + close.len()..].trim_start().to_string();
                    self.close = None;
                    self.after = true;
                    continue;
                }
                self.hold(&mut out, SplitOut::Thinking, &[close], flush);
                break;
            }
            if let (Some((open_at, open, close_tag)), close_hit) = (
                earliest_tag(&self.buf, true),
                earliest_tag(&self.buf, false),
            ) {
                if let Some((close_at, _, close)) = close_hit {
                    if close_at < open_at {
                        if close_at > 0 {
                            out.push(SplitOut::Text(self.buf[..close_at].to_string()));
                        }
                        self.buf = self.buf[close_at + close.len()..].trim_start().to_string();
                        continue;
                    }
                }
                if open_at > 0 {
                    out.push(SplitOut::Text(self.buf[..open_at].to_string()));
                }
                self.buf = self.buf[open_at + open.len()..].to_string();
                self.close = Some(close_tag);
                continue;
            }
            if let Some((pos, _open, close)) = earliest_tag(&self.buf, false) {
                if pos > 0 {
                    out.push(SplitOut::Text(self.buf[..pos].to_string()));
                }
                self.buf = self.buf[pos + close.len()..].trim_start().to_string();
                continue;
            }
            self.emit_text(&mut out, flush);
            break;
        }
        out
    }

    fn emit_text(&mut self, out: &mut Vec<SplitOut>, flush: bool) {
        let tags = open_and_close();
        self.hold(out, SplitOut::Text, &tags, flush);
    }

    fn hold(
        &mut self,
        out: &mut Vec<SplitOut>,
        kind: fn(String) -> SplitOut,
        tags: &[&str],
        flush: bool,
    ) {
        let hold = if flush { 0 } else { hold_len(&self.buf, tags) };
        let emit = self.buf.len() - hold;
        if emit > 0 {
            out.push(kind(self.buf[..emit].to_string()));
            self.buf = self.buf[emit..].to_string();
        }
        if flush && !self.buf.is_empty() {
            let rest = std::mem::take(&mut self.buf);
            out.push(kind(rest));
        }
    }
}

fn earliest_tag(text: &str, open: bool) -> Option<(usize, &'static str, &'static str)> {
    let mut best: Option<(usize, &'static str, &'static str)> = None;
    for (o, c) in THINKING_TAGS {
        let needle = if open { *o } else { *c };
        if let Some(pos) = text.find(needle) {
            let better = best.map(|(at, _, _)| pos < at).unwrap_or(true);
            if better {
                best = Some((pos, o, c));
            }
        }
    }
    best
}

fn open_and_close() -> Vec<&'static str> {
    let mut tags = Vec::new();
    for (o, c) in THINKING_TAGS {
        tags.push(*o);
        tags.push(*c);
    }
    tags
}

fn hold_len(text: &str, tags: &[&str]) -> usize {
    let bytes = text.as_bytes();
    let mut max = 0;
    for tag in tags {
        let tb = tag.as_bytes();
        let max_n = bytes.len().min(tb.len().saturating_sub(1));
        for n in (1..=max_n).rev() {
            if bytes[bytes.len() - n..] == tb[..n] {
                max = max.max(n);
                break;
            }
        }
    }
    max
}

fn short_hash(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update([0]);
        }
        hasher.update(part.as_bytes());
    }
    let digest = hasher.finalize();
    let mut out = String::with_capacity(16);
    for b in &digest[..8] {
        out.push(HEX[(*b >> 4) as usize] as char);
        out.push(HEX[(*b & 0xf) as usize] as char);
    }
    out
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn unix_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::QoderBackend;
    use crate::protocol::resolve_model;

    fn model() -> ResolvedModel {
        resolve_model(QoderBackend::Global, "Qwen3.8-Max-effort-high").unwrap()
    }

    #[test]
    fn system_prompt_is_a_message_and_empty_tool_text_is_a_space() {
        let body = build_body(
            &model(),
            "user-1",
            "sess",
            &[
                InTurn {
                    role: InRole::System,
                    text: "be brief".into(),
                    images: vec![],
                    tool_calls: vec![],
                    tool_call_id: String::new(),
                },
                InTurn {
                    role: InRole::Assistant,
                    text: String::new(),
                    images: vec![],
                    tool_calls: vec![InToolCall {
                        id: "call_1".into(),
                        name: "read".into(),
                        arguments: "{\"path\":\"a\"}".into(),
                    }],
                    tool_call_id: String::new(),
                },
                InTurn {
                    role: InRole::Tool,
                    text: "file".into(),
                    images: vec![],
                    tool_calls: vec![],
                    tool_call_id: "call_1".into(),
                },
            ],
            &[],
            Some(128),
        );
        assert_eq!(body["system"], "");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][0]["content"], "be brief");
        assert_eq!(body["messages"][1]["content"], " ");
        assert_eq!(
            body["messages"][1]["tool_calls"][0]["function"]["name"],
            "read"
        );
        assert_eq!(body["messages"][2]["role"], "tool");
        assert_eq!(body["model_config"]["key"], "qmodel_preview");
        assert_eq!(body["parameters"]["reasoning_effort"], "high");
        assert_eq!(body["parameters"]["enable_thinking"], true);
        assert_eq!(body["parameters"]["max_tokens"], 128);
    }

    #[test]
    fn an_envelope_yields_text_usage_and_a_tool_delta() {
        let data = r#"{"statusCodeValue":200,"body":"{\"choices\":[{\"delta\":{\"content\":\"hi\",\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"read\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2,\"prompt_tokens_details\":{\"cached_tokens\":4}}}"}"#;
        let pieces = parse_data(data);
        assert!(pieces
            .iter()
            .any(|p| matches!(p, StreamPiece::Text(t) if t == "hi")));
        assert!(pieces
            .iter()
            .any(|p| matches!(p, StreamPiece::Tool { name: Some(n), .. } if n == "read")));
        assert!(pieces
            .iter()
            .any(|p| matches!(p, StreamPiece::Finish(r) if r == "tool_calls")));
        let usage = pieces.iter().find_map(|p| match p {
            StreamPiece::Usage(u) => Some(u),
            _ => None,
        });
        assert_eq!(usage.unwrap().input_tokens, 6);
        assert_eq!(usage.unwrap().cache_read_tokens, 4);
        assert_eq!(parse_data("[DONE]"), vec![StreamPiece::Done]);
    }

    #[test]
    fn thinking_tags_in_the_content_channel_do_not_leak() {
        let mut split = ThinkingSplit::default();
        let mut text = String::new();
        let mut thinking = String::new();
        for chunk in ["hi<thi", "nking>sec", "ret</thinking>there"] {
            for out in split.push(chunk) {
                match out {
                    SplitOut::Text(t) => text.push_str(&t),
                    SplitOut::Thinking(t) => thinking.push_str(&t),
                }
            }
        }
        for out in split.finish() {
            match out {
                SplitOut::Text(t) => text.push_str(&t),
                SplitOut::Thinking(t) => thinking.push_str(&t),
            }
        }
        assert_eq!(text, "hithere");
        assert_eq!(thinking, "secret");
        assert!(!text.contains("secret"));
    }
}
