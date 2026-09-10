//! Kiro（Amazon Q）直连：`POST generateAssistantResponse`。
//!
//! 第一版只做对话文本：工具调用、eventstream 里的 `toolUseEvent` 都丢掉。对外模型名是
//! `kiro-claude-*`，发给上游时剥前缀。

use crate::error::{UpstreamError, UpstreamKind};
use crate::inference::{DEFAULT_IDLE_TIMEOUT, DEFAULT_MAX_TURN};
use crate::lane::{BoxFuture, Credential};
use crate::normalized::{
    estimate_tokens, ChatRequest, Completion, Delta, FinishReason, Role, ToolCall, Usage,
};
use crate::upstream::{DeltaSink, Upstream};
use nexus_kiro::protocol::{chat_headers, Q_ENDPOINT};
use nexus_kiro::{is_kiro_model, upstream_model};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

pub struct KiroUpstream {
    client: reqwest::Client,
    endpoint: String,
    idle_timeout: Duration,
    max_turn: Duration,
}

impl KiroUpstream {
    pub fn new() -> Self {
        Self {
            client: crate::inference::http_client(),
            endpoint: Q_ENDPOINT.to_string(),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
        }
    }

    #[cfg(test)]
    pub fn with_endpoint(endpoint: String) -> Self {
        Self {
            client: crate::inference::http_client(),
            endpoint,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
        }
    }

    async fn run(
        &self,
        credential: &Credential,
        request: &ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<Completion, UpstreamError> {
        let model = upstream_model(&request.model);
        let body = build_body(request, &model);
        let payload = serde_json::to_vec(&body).map_err(|e| {
            UpstreamError::new(
                UpstreamKind::BadRequest,
                400,
                format!("请求体无法序列化：{e}"),
            )
        })?;
        let mut req = self
            .client
            .post(&self.endpoint)
            .timeout(self.max_turn)
            .body(payload);
        for (k, v) in chat_headers(&credential.access_token) {
            req = req.header(k, v);
        }
        let res = match req.send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                return Err(UpstreamError::new(
                    UpstreamKind::Timeout,
                    504,
                    format!("连 Amazon Q 超时：{e}"),
                ));
            }
            Err(e) => {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    502,
                    format!("连不上 Amazon Q：{e}"),
                ));
            }
        };
        let status = res.status().as_u16();
        let ctype = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !(200..300).contains(&status) {
            let text = res.text().await.unwrap_or_default();
            return Err(map_status(status, &text));
        }
        on_delta(Delta::Headers(Vec::new()));
        let started = Instant::now();
        if ctype.contains("eventstream") || ctype.contains("octet-stream") {
            read_eventstream(res, started, self.idle_timeout, on_delta).await
        } else {
            let text_body = res.text().await.unwrap_or_default();
            emit_json_text(&text_body, started, on_delta)
        }
    }
}

impl Default for KiroUpstream {
    fn default() -> Self {
        Self::new()
    }
}

impl Upstream for KiroUpstream {
    fn stream<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ChatRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
        Box::pin(self.run(credential, request, on_delta))
    }
}

pub fn accepts(base_model: &str) -> bool {
    is_kiro_model(base_model)
}

/// 中间表示 → Amazon Q 的 `conversationState`。
///
/// 形状（与 Kiro IDE / CLIProxyAPI 一致）：最后一条 user 是 `currentMessage`，其余按顺序进
/// `history`，user / assistant 必须成对交替。工具：声明放在当前 user 消息的
/// `userInputMessageContext.tools`（`toolSpecification`），模型的调用记在 assistant 的
/// `toolUses`，客户端回的结果作为**下一条 user 消息**的 `toolResults`——Q 没有独立的 tool
/// 角色，工具结果就是 user 那一轮的一部分，所以 `Role::Tool` 会并进紧随其后的 user 消息
/// （没有的话自己成为一条内容为空的 user 消息）。
pub fn build_body(request: &ChatRequest, model: &str) -> Value {
    let mut history: Vec<Value> = Vec::new();
    let mut current: Option<Value> = None;
    let mut system = String::new();
    // 攒着等下一条 user 消息一起发的工具结果。
    let mut pending_results: Vec<Value> = Vec::new();

    let user_message = |content: String, results: &mut Vec<Value>| -> Value {
        let mut msg = json!({
            "content": content,
            "modelId": model,
            "origin": "AI_EDITOR",
        });
        if !results.is_empty() {
            msg["userInputMessageContext"] = json!({ "toolResults": std::mem::take(results) });
        }
        json!({ "userInputMessage": msg })
    };

    for m in &request.messages {
        match m.role {
            Role::System => {
                if !system.is_empty() {
                    system.push('\n');
                }
                system.push_str(m.text.trim());
            }
            Role::User => {
                if let Some(prev) = current.take() {
                    history.push(prev);
                }
                let mut content = m.text.clone();
                if !system.is_empty() && history.is_empty() {
                    content = format!("{system}\n\n{content}");
                    system.clear();
                }
                // 图片输入这一版不带：Q 的 images[] 字段形状未经真机确认，猜错会让整条请求 400。
                current = Some(user_message(content, &mut pending_results));
            }
            Role::Assistant => {
                if let Some(prev) = current.take() {
                    history.push(prev);
                } else if !pending_results.is_empty() || history.is_empty() {
                    // 上一轮工具结果还没被 user 消息带走、或开头就是 assistant：补一条空 user，保持交替。
                    history.push(user_message(String::new(), &mut pending_results));
                }
                let mut msg = json!({ "content": m.text });
                if !m.tool_calls.is_empty() {
                    let uses: Vec<Value> = m
                        .tool_calls
                        .iter()
                        .map(|c| {
                            let input: Value = serde_json::from_str(&c.arguments).unwrap_or_else(|_| json!({}));
                            json!({
                                "toolUseId": if c.id.is_empty() { "tooluse_0".to_string() } else { c.id.clone() },
                                "name": if c.name.is_empty() { "tool".to_string() } else { c.name.clone() },
                                "input": if input.is_object() { input } else { json!({ "input": input }) },
                            })
                        })
                        .collect();
                    msg["toolUses"] = Value::Array(uses);
                }
                history.push(json!({ "assistantResponseMessage": msg }));
            }
            Role::Tool => {
                for r in &m.tool_results {
                    pending_results.push(json!({
                        "toolUseId": if r.tool_call_id.is_empty() { "tooluse_0".to_string() } else { r.tool_call_id.clone() },
                        "content": [{ "text": r.text }],
                        "status": if r.is_error { "error" } else { "success" },
                    }));
                }
                if m.tool_results.is_empty() && !m.text.is_empty() {
                    pending_results.push(json!({
                        "toolUseId": "tooluse_0",
                        "content": [{ "text": m.text }],
                        "status": "success",
                    }));
                }
            }
        }
    }
    let mut current = current.unwrap_or_else(|| {
        // 尾巴是工具结果（Claude Code 的常态）：它们就是这一轮的 user 消息。
        user_message(
            if system.is_empty() {
                String::new()
            } else {
                std::mem::take(&mut system)
            },
            &mut pending_results,
        )
    });
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "toolSpecification": {
                        "name": t.name,
                        "description": if t.description.is_empty() { t.name.as_str() } else { t.description.as_str() },
                        "inputSchema": { "json": if t.parameters.is_object() { t.parameters.clone() } else { json!({ "type": "object" }) } },
                    }
                })
            })
            .collect();
        let ctx = current["userInputMessage"]
            .as_object_mut()
            .expect("刚拼的对象")
            .entry("userInputMessageContext")
            .or_insert_with(|| json!({}));
        ctx["tools"] = Value::Array(tools);
    }
    let conv = request
        .conversation_id
        .clone()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    json!({
        "conversationState": {
            "conversationId": conv,
            "chatTriggerType": "MANUAL",
            "currentMessage": current,
            "history": history,
        }
    })
}

fn map_status(status: u16, text: &str) -> UpstreamError {
    let head: String = text.chars().take(200).collect();
    let kind = match status {
        401 | 403 => UpstreamKind::Auth,
        429 => UpstreamKind::RateLimit,
        400 => UpstreamKind::BadRequest,
        _ => UpstreamKind::Upstream,
    };
    UpstreamError::new(kind, status, format!("Kiro {status}：{head}"))
}

fn emit_json_text(
    raw: &str,
    started: Instant,
    on_delta: DeltaSink<'_>,
) -> Result<Completion, UpstreamError> {
    let v: Value = serde_json::from_str(raw).unwrap_or(Value::Null);
    let text = extract_text(&v).unwrap_or_default();
    if !text.is_empty() {
        on_delta(Delta::Text(text.clone()));
    }
    Ok(done(text, started))
}

fn extract_text(v: &Value) -> Option<String> {
    v.pointer("/assistantResponseEvent/content")
        .or_else(|| v.pointer("/content"))
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .or_else(|| {
            v.get("assistantResponseMessage")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
                .map(str::to_string)
        })
}

/// 一个正在攒的工具调用：Q 把 `input` 按片段流下来，`stop: true` 才算齐。
#[derive(Default)]
struct ToolUseBuf {
    name: String,
    input: String,
}

/// 流里攒出来的东西。
#[derive(Default)]
struct Collected {
    text: String,
    tool_calls: Vec<ToolCall>,
    pending: Vec<(String, ToolUseBuf)>,
    ttft: Option<u64>,
    /// 上游明确说的错误（`errorEvent` / 异常帧）。
    error: Option<String>,
}

async fn read_eventstream(
    mut res: reqwest::Response,
    started: Instant,
    idle: Duration,
    on_delta: DeltaSink<'_>,
) -> Result<Completion, UpstreamError> {
    let mut buf = Vec::new();
    let mut c = Collected::default();
    loop {
        let chunk = match tokio::time::timeout(idle, res.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    502,
                    format!("Kiro 流中断：{e}"),
                ));
            }
            Err(_) => {
                return Err(UpstreamError::new(
                    UpstreamKind::Timeout,
                    504,
                    format!("Kiro {} 秒没有动静", idle.as_secs()),
                ));
            }
        };
        buf.extend_from_slice(&chunk);
        while let Some((n, event)) = next_event(&buf) {
            buf.drain(..n);
            handle_event(&event, &mut c, started, on_delta);
        }
    }
    if let Some(msg) = c.error {
        return Err(UpstreamError::new(
            UpstreamKind::Upstream,
            502,
            format!("Kiro：{msg}"),
        ));
    }
    // 没等到 stop 的调用也交出去：上游偶尔不发 stop 就结束流。
    for (id, buf) in c.pending.drain(..) {
        c.tool_calls.push(ToolCall {
            id,
            name: buf.name,
            arguments: if buf.input.is_empty() {
                "{}".into()
            } else {
                buf.input
            },
        });
    }
    let mut out = done(c.text, started);
    out.ttft_ms = c.ttft;
    if !c.tool_calls.is_empty() {
        out.finish_reason = FinishReason::ToolCalls;
        out.tool_calls = c.tool_calls;
    }
    Ok(out)
}

/// 一帧 eventstream 解出来的东西：`:event-type` 头 + JSON 载荷。
struct Event {
    event_type: String,
    payload: Vec<u8>,
}

fn handle_event(ev: &Event, c: &mut Collected, started: Instant, on_delta: DeltaSink<'_>) {
    let Ok(v) = serde_json::from_slice::<Value>(&ev.payload) else {
        return;
    };
    // 头里没有事件名（老形态）时按载荷的顶层键认。
    let kind = if ev.event_type.is_empty() {
        v.as_object()
            .and_then(|o| o.keys().next().cloned())
            .unwrap_or_default()
    } else {
        ev.event_type.clone()
    };
    let body = v.get(&kind).unwrap_or(&v);
    match kind.as_str() {
        "assistantResponseEvent" => {
            if let Some(piece) = body
                .get("content")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                if c.ttft.is_none() {
                    c.ttft = Some(started.elapsed().as_millis() as u64);
                }
                c.text.push_str(piece);
                on_delta(Delta::Text(piece.to_string()));
            }
        }
        "toolUseEvent" => {
            let id = body
                .get("toolUseId")
                .and_then(Value::as_str)
                .unwrap_or("tooluse_0")
                .to_string();
            let entry = match c.pending.iter_mut().position(|(k, _)| *k == id) {
                Some(i) => &mut c.pending[i].1,
                None => {
                    c.pending.push((id.clone(), ToolUseBuf::default()));
                    &mut c.pending.last_mut().expect("刚 push 的").1
                }
            };
            if let Some(name) = body
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                entry.name = name.to_string();
            }
            match body.get("input") {
                Some(Value::String(s)) => entry.input.push_str(s),
                Some(other) if !other.is_null() => entry.input.push_str(&other.to_string()),
                _ => {}
            }
            if body.get("stop").and_then(Value::as_bool) == Some(true) {
                if let Some(i) = c.pending.iter().position(|(k, _)| *k == id) {
                    let (id, buf) = c.pending.remove(i);
                    c.tool_calls.push(ToolCall {
                        id,
                        name: if buf.name.is_empty() {
                            "tool".into()
                        } else {
                            buf.name
                        },
                        arguments: if buf.input.trim().is_empty() {
                            "{}".into()
                        } else {
                            buf.input
                        },
                    });
                }
            }
        }
        // 上游的异常帧：`message` 里是原因。
        k if k.ends_with("Exception") || k == "errorEvent" || k == "error" => {
            let msg = body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or(k)
                .to_string();
            c.error = Some(msg);
        }
        // messageMetadataEvent / followupPromptEvent / codeReferenceEvent / supplementaryWebLinksEvent：不关心。
        _ => {}
    }
}

/// AWS eventstream：prelude 12 字节（total / headers_len / prelude CRC）+ headers + payload + message CRC。
/// 头是 `name_len(1) name type(1) …`，只认 type 7（字符串），拿 `:event-type` 与 `:message-type`。
fn next_event(buf: &[u8]) -> Option<(usize, Event)> {
    if buf.len() < 16 {
        return None;
    }
    let total = u32::from_be_bytes(buf[0..4].try_into().ok()?) as usize;
    if total < 16 || buf.len() < total {
        return None;
    }
    let headers_len = u32::from_be_bytes(buf[4..8].try_into().ok()?) as usize;
    let payload_start = 12usize.checked_add(headers_len)?;
    let payload_end = total.checked_sub(4)?;
    if payload_end < payload_start {
        return None;
    }
    let mut event_type = String::new();
    let mut exception_type = String::new();
    let mut message_type = String::new();
    let mut i = 12;
    while i < payload_start {
        let name_len = *buf.get(i)? as usize;
        let name = std::str::from_utf8(buf.get(i + 1..i + 1 + name_len)?).unwrap_or("");
        let ty = *buf.get(i + 1 + name_len)?;
        i += 2 + name_len;
        let value_len = match ty {
            0 | 1 => 0,
            2 => 1,
            3 => 2,
            4 => 4,
            5 | 8 => 8,
            6 | 7 => {
                let l = u16::from_be_bytes(buf.get(i..i + 2)?.try_into().ok()?) as usize;
                i += 2;
                l
            }
            9 => 16,
            _ => return None,
        };
        if ty == 7 {
            let v = std::str::from_utf8(buf.get(i..i + value_len)?).unwrap_or("");
            match name {
                ":event-type" => event_type = v.to_string(),
                ":exception-type" => exception_type = v.to_string(),
                ":message-type" => message_type = v.to_string(),
                _ => {}
            }
        }
        i += value_len;
    }
    if message_type == "exception" || message_type == "error" {
        event_type = if exception_type.is_empty() {
            "errorEvent".into()
        } else {
            exception_type
        };
    }
    Some((
        total,
        Event {
            event_type,
            payload: buf[payload_start..payload_end].to_vec(),
        },
    ))
}

#[cfg(test)]
fn text_from_payload(payload: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(payload).ok()?;
    v.pointer("/assistantResponseEvent/content")
        .or_else(|| v.get("content"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn done(text: String, started: Instant) -> Completion {
    Completion {
        usage: Usage {
            output_tokens: estimate_tokens(&text),
            ..Default::default()
        },
        text,
        thinking: String::new(),
        tool_calls: Vec::new(),
        finish_reason: FinishReason::Stop,
        usage_measured: false,
        routed_model: Some("kiro".into()),
        ttft_ms: None,
        turn_ms: started.elapsed().as_millis() as u64,
        raw_response: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalized::Message;

    #[test]
    fn last_user_is_current_and_history_keeps_the_rest() {
        let req = ChatRequest {
            model: "kiro-claude-sonnet-4.5".into(),
            messages: vec![
                Message::text(Role::System, "be brief"),
                Message::text(Role::User, "hi"),
                Message::text(Role::Assistant, "hello"),
                Message::text(Role::User, "again"),
            ],
            ..Default::default()
        };
        let body = build_body(&req, "claude-sonnet-4.5");
        let current = body
            .pointer("/conversationState/currentMessage/userInputMessage/content")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(current, "again");
        let hist = body
            .pointer("/conversationState/history")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(hist.len(), 2);
        assert!(hist[0]["userInputMessage"]["content"]
            .as_str()
            .unwrap()
            .contains("be brief"));
        assert_eq!(
            hist[1]["assistantResponseMessage"]["content"]
                .as_str()
                .unwrap(),
            "hello"
        );
    }

    /// 拼一帧 eventstream：可带 `:event-type` 字符串头。
    fn frame(event_type: Option<&str>, payload: &[u8]) -> Vec<u8> {
        let mut headers = Vec::new();
        if let Some(t) = event_type {
            headers.push(b":event-type".len() as u8);
            headers.extend_from_slice(b":event-type");
            headers.push(7);
            headers.extend_from_slice(&(t.len() as u16).to_be_bytes());
            headers.extend_from_slice(t.as_bytes());
        }
        let total: u32 = 12 + headers.len() as u32 + payload.len() as u32 + 4;
        let mut buf = Vec::new();
        buf.extend_from_slice(&total.to_be_bytes());
        buf.extend_from_slice(&(headers.len() as u32).to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&headers);
        buf.extend_from_slice(payload);
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf
    }

    #[test]
    fn eventstream_splits_a_single_message() {
        let buf = frame(None, br#"{"assistantResponseEvent":{"content":"hi"}}"#);
        let (n, ev) = next_event(&buf).unwrap();
        assert_eq!(n, buf.len());
        assert!(ev.event_type.is_empty());
        assert_eq!(text_from_payload(&ev.payload).as_deref(), Some("hi"));
    }

    #[test]
    fn tools_go_into_the_current_message_and_results_become_the_next_user_turn() {
        use crate::normalized::{ToolDef, ToolResult};
        let mut asst = Message::text(Role::Assistant, "");
        asst.tool_calls.push(ToolCall {
            id: "tu_1".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"a.rs"}"#.into(),
        });
        let mut tool = Message::text(Role::Tool, "");
        tool.tool_results.push(ToolResult {
            tool_call_id: "tu_1".into(),
            tool_name: "read_file".into(),
            text: "fn main(){}".into(),
            is_error: false,
        });
        let req = ChatRequest {
            model: "kiro-claude-sonnet-4.5".into(),
            messages: vec![Message::text(Role::User, "read a.rs"), asst, tool],
            tools: vec![ToolDef {
                name: "read_file".into(),
                description: "read".into(),
                parameters: serde_json::json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let body = build_body(&req, "claude-sonnet-4.5");
        let cur = &body["conversationState"]["currentMessage"]["userInputMessage"];
        assert_eq!(cur["content"], "");
        assert_eq!(
            cur["userInputMessageContext"]["toolResults"][0]["toolUseId"],
            "tu_1"
        );
        assert_eq!(
            cur["userInputMessageContext"]["toolResults"][0]["status"],
            "success"
        );
        assert_eq!(
            cur["userInputMessageContext"]["tools"][0]["toolSpecification"]["name"],
            "read_file"
        );
        let hist = body["conversationState"]["history"].as_array().unwrap();
        assert_eq!(hist.len(), 2);
        assert_eq!(
            hist[1]["assistantResponseMessage"]["toolUses"][0]["input"]["path"],
            "a.rs"
        );
    }

    #[test]
    fn tool_use_events_are_assembled_from_fragments() {
        let started = Instant::now();
        let mut c = Collected::default();
        let mut seen: Vec<Delta> = vec![];
        let mut sink = |d: Delta| seen.push(d);
        let mut buf = Vec::new();
        buf.extend(frame(
            Some("assistantResponseEvent"),
            br#"{"content":"Let me look."}"#,
        ));
        buf.extend(frame(
            Some("toolUseEvent"),
            br#"{"toolUseId":"tu_9","name":"read_file","input":"{\"pa"}"#,
        ));
        buf.extend(frame(
            Some("toolUseEvent"),
            br#"{"toolUseId":"tu_9","input":"th\":\"a.rs\"}","stop":true}"#,
        ));
        let mut off = 0;
        while let Some((n, ev)) = next_event(&buf[off..]) {
            off += n;
            handle_event(&ev, &mut c, started, &mut sink);
        }
        assert_eq!(off, buf.len());
        assert_eq!(c.text, "Let me look.");
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].name, "read_file");
        assert_eq!(c.tool_calls[0].arguments, r#"{"path":"a.rs"}"#);
        assert!(c.pending.is_empty());
        assert_eq!(seen.len(), 1, "工具调用不流式下发");
    }
}
