//! Kiro（Amazon Q）直连：`POST generateAssistantResponse`。
//!
//! 请求体按 Kiro IDE 的 `conversationState`：system 单独成一轮，工具声明和结果挂在
//! 当前 user 上，图片走 `images[]`。流是 AWS eventstream，文本、`<thinking>`、工具调用
//! 和 token 用量都拆出来。对外模型名是 `kiro-claude-*`。

use crate::error::{UpstreamError, UpstreamKind};
use crate::inference::{DEFAULT_IDLE_TIMEOUT, DEFAULT_MAX_TURN};
use crate::lane::{BoxFuture, Credential};
use crate::normalized::{
    estimate_tokens, ChatRequest, Completion, Delta, FinishReason, ImageInput, Message, Role,
    ToolCall, ToolChoice, Usage,
};
use crate::upstream::{DeltaSink, Upstream};
use nexus_kiro::protocol::{self, chat_headers, Q_ENDPOINT};
use nexus_kiro::{is_kiro_model, upstream_model, KiroService};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct KiroUpstream {
    client: reqwest::Client,
    endpoint: String,
    idle_timeout: Duration,
    max_turn: Duration,
    accounts: Option<Arc<KiroService>>,
}

impl KiroUpstream {
    pub fn new(accounts: Arc<KiroService>) -> Self {
        Self {
            client: crate::inference::http_client(),
            endpoint: Q_ENDPOINT.to_string(),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
            accounts: Some(accounts),
        }
    }

    #[cfg(test)]
    pub fn with_endpoint(endpoint: String) -> Self {
        Self {
            client: crate::inference::http_client(),
            endpoint,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
            accounts: None,
        }
    }

    async fn run(
        &self,
        credential: &Credential,
        request: &ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<Completion, UpstreamError> {
        let model = upstream_model(&request.model);
        let outbound = self
            .accounts
            .as_ref()
            .map(|accounts| accounts.outbound_for(&credential.label));
        let machine = outbound
            .as_ref()
            .map(|o| o.machine_id.clone())
            .unwrap_or_else(|| protocol::machine_id(None, &credential.label));
        let profile = outbound.as_ref().and_then(|o| o.profile_arn.clone());
        let (body, names) = build_body(request, &model, profile.as_deref());
        let thinking_on = thinking_dir(request).is_on();
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
        let invocation = uuid::Uuid::new_v4().to_string();
        for (k, v) in chat_headers(
            &credential.access_token,
            &machine,
            profile.as_deref(),
            &invocation,
        ) {
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
            read_eventstream(
                res,
                started,
                self.idle_timeout,
                names,
                thinking_on,
                on_delta,
            )
            .await
        } else {
            let text_body = res.text().await.unwrap_or_default();
            emit_json_text(&text_body, started, on_delta)
        }
    }
}

impl Default for KiroUpstream {
    fn default() -> Self {
        Self {
            client: crate::inference::http_client(),
            endpoint: Q_ENDPOINT.to_string(),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
            accounts: None,
        }
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
/// 最后一条 user 是 `currentMessage`，其余进 `history`。system 单独成一轮
/// （user 说明 + assistant 确认），不塞进用户的第一句话。工具声明挂在当前 user 的
/// `userInputMessageContext.tools`，调用记在 assistant 的 `toolUses`，结果并进下一条 user。
/// 返回的表把被截短的工具名映回客户端原来的名字。
pub fn build_body(
    request: &ChatRequest,
    model: &str,
    profile_arn: Option<&str>,
) -> (Value, HashMap<String, String>) {
    let messages = merge_adjacent(&request.messages);
    let mut history: Vec<Value> = Vec::new();
    let mut current: Option<Value> = None;
    let mut system = String::new();
    let mut pending_results: Vec<Value> = Vec::new();
    let mut names: HashMap<String, String> = HashMap::new();

    let user_message =
        |content: String, images: &[ImageInput], results: &mut Vec<Value>| -> Value {
            let content = if content.trim().is_empty() {
                if results.is_empty() {
                    "Continue".to_string()
                } else {
                    "Tool results provided.".to_string()
                }
            } else {
                content
            };
            let mut msg = json!({
                "content": content,
                "modelId": model,
                "origin": "AI_EDITOR",
            });
            let images = kiro_images(images);
            if !images.is_empty() {
                msg["images"] = Value::Array(images);
            }
            if !results.is_empty() {
                msg["userInputMessageContext"] = json!({ "toolResults": std::mem::take(results) });
            }
            json!({ "userInputMessage": msg })
        };

    for m in &messages {
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
                current = Some(user_message(
                    m.text.clone(),
                    &m.images,
                    &mut pending_results,
                ));
            }
            Role::Assistant => {
                if let Some(prev) = current.take() {
                    history.push(prev);
                } else if !pending_results.is_empty() || history.is_empty() {
                    let filler = if pending_results.is_empty() {
                        "."
                    } else {
                        "Tool results provided."
                    };
                    history.push(user_message(filler.to_string(), &[], &mut pending_results));
                }
                let content = if m.text.trim().is_empty() {
                    " ".to_string()
                } else {
                    m.text.clone()
                };
                let mut msg = json!({ "content": content });
                if !m.tool_calls.is_empty() {
                    let uses: Vec<Value> = m
                        .tool_calls
                        .iter()
                        .map(|c| {
                            let input: Value =
                                serde_json::from_str(&c.arguments).unwrap_or_else(|_| json!({}));
                            let name = wire_tool_name(
                                if c.name.is_empty() { "tool" } else { &c.name },
                                &mut names,
                            );
                            json!({
                                "toolUseId": if c.id.is_empty() { "tooluse_0".to_string() } else { c.id.clone() },
                                "name": name,
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
                    let text = if r.text.is_empty() {
                        "Tool use was cancelled by the user"
                    } else {
                        r.text.as_str()
                    };
                    pending_results.push(json!({
                        "toolUseId": if r.tool_call_id.is_empty() { "tooluse_0".to_string() } else { r.tool_call_id.clone() },
                        "content": [{ "text": text }],
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
    let mut current =
        current.unwrap_or_else(|| user_message(String::new(), &[], &mut pending_results));
    let system_text = injected_system(&system, request, &mut names);
    if !system_text.is_empty() {
        let prefix = vec![
            user_message(system_text, &[], &mut Vec::new()),
            json!({ "assistantResponseMessage": { "content": "I will follow these instructions." } }),
        ];
        let mut merged = prefix;
        merged.append(&mut history);
        history = merged;
    }
    let mut tools = Vec::new();
    if request.tool_choice != ToolChoice::None {
        tools = request
            .tools
            .iter()
            .map(|t| {
                let name = wire_tool_name(&t.name, &mut names);
                let described = if t.name == "web_search" {
                    REMOTE_WEB_SEARCH_DESCRIPTION
                } else {
                    t.description.as_str()
                };
                let description =
                    protocol::truncate_tool_description(&tool_description(&t.name, described));
                json!({
                    "toolSpecification": {
                        "name": name,
                        "description": description,
                        "inputSchema": { "json": kiro_schema(&t.parameters) },
                    }
                })
            })
            .collect();
    }
    reconcile_tools(
        &mut history,
        &mut current,
        request.tool_choice != ToolChoice::None,
        &mut tools,
    );
    if !tools.is_empty() {
        let ctx = current["userInputMessage"]
            .as_object_mut()
            .expect("刚拼的对象")
            .entry("userInputMessageContext")
            .or_insert_with(|| json!({}));
        ctx["tools"] = Value::Array(tools);
    }
    let (conv, continuation) = session_ids(request);
    let mut body = json!({
        "conversationState": {
            "agentTaskType": "vibe",
            "conversationId": conv,
            "chatTriggerType": "MANUAL",
            "currentMessage": current,
            "history": history,
        }
    });
    if let Some(id) = continuation {
        body["conversationState"]["agentContinuationId"] = json!(id);
    }
    if let Some(cfg) = inference_config(request) {
        body["inferenceConfig"] = cfg;
    }
    if let Some(arn) = profile_arn.map(str::trim).filter(|s| s.starts_with("arn:")) {
        body["profileArn"] = json!(arn);
    }
    (body, names)
}

const REMOTE_WEB_SEARCH_DESCRIPTION: &str = "WebSearch looks up information outside the model's training data. Supports multiple queries to gather comprehensive information.";

const WRITE_CHUNK_SUFFIX: &str = "IMPORTANT: If the content to write exceeds 150 lines, write only the first 50 lines with this tool, then append the remaining content using Edit calls in chunks of no more than 50 lines. Use a unique placeholder if needed. Do not write the whole file in one call.";

const EDIT_CHUNK_SUFFIX: &str = "IMPORTANT: If new content exceeds 50 lines, split it into multiple Edit calls, replacing or appending no more than 50 lines per call. If appending, use a unique placeholder and remove it in the final chunk.";

/// 连续的同角色合成一条。Kiro 要 user / assistant 交替，两条 user 连着去会被拒。
/// tool 消息各自带着结果，不并。
fn merge_adjacent(messages: &[Message]) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::new();
    for message in messages {
        if let Some(prev) = out.last_mut() {
            if prev.role == message.role && prev.role != Role::Tool {
                if !prev.text.is_empty() && !message.text.is_empty() {
                    prev.text.push_str("\n\n");
                }
                prev.text.push_str(&message.text);
                prev.images.extend(message.images.clone());
                prev.tool_calls.extend(message.tool_calls.clone());
                prev.tool_results.extend(message.tool_results.clone());
                continue;
            }
        }
        out.push(message.clone());
    }
    out
}

/// `additional_kwargs` 里的会话号优先。没有就用入站已经算出的 conversation id。
fn session_ids(request: &ChatRequest) -> (String, Option<String>) {
    let mut from_meta = None;
    let mut continuation = None;
    if let Some(raw) = request.raw_inbound.as_ref() {
        if let Some(messages) = raw.body.get("messages").and_then(Value::as_array) {
            for message in messages.iter().rev() {
                let extra = message.get("additional_kwargs");
                if from_meta.is_none() {
                    from_meta = extra
                        .and_then(|v| v.get("conversationId"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                }
                if continuation.is_none() {
                    continuation = extra
                        .and_then(|v| v.get("continuationId"))
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string);
                }
            }
        }
    }
    let conv = from_meta
        .or_else(|| {
            request
                .conversation_id
                .clone()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    (conv, continuation)
}

fn wire_tool_name(name: &str, names: &mut HashMap<String, String>) -> String {
    let original = if name == "web_search" {
        "remote_web_search".to_string()
    } else {
        protocol::shorten_tool_name(name)
    };
    if original != name {
        names.insert(original.clone(), name.to_string());
    }
    original
}

fn tool_description(name: &str, description: &str) -> String {
    let base = if description.trim().is_empty() {
        format!("Tool: {name}")
    } else {
        description.trim().to_string()
    };
    let suffix = match name.to_ascii_lowercase().as_str() {
        "write" | "write_to_file" | "fswrite" | "create_file" => Some(WRITE_CHUNK_SUFFIX),
        "edit" | "edit_file" | "str_replace_editor" | "apply_diff" => Some(EDIT_CHUNK_SUFFIX),
        _ => None,
    };
    match suffix {
        Some(suffix) if !base.contains(suffix) => format!("{base}\n{suffix}"),
        _ => base,
    }
}

fn kiro_schema(schema: &Value) -> Value {
    normalize_schema(schema, true)
}

fn default_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "required": [],
        "additionalProperties": true
    })
}

/// Kiro 只收整理过的 JSON Schema：缺 type 补 object，嵌套的 properties / items / anyOf 同样整理。
fn normalize_schema(schema: &Value, enforce_object: bool) -> Value {
    let Some(obj) = schema.as_object() else {
        return default_schema();
    };
    let mut normalized = serde_json::Map::new();
    for (key, value) in obj {
        normalized.insert(key.clone(), normalize_schema_child(key, value));
    }
    let type_missing = normalized
        .get("type")
        .and_then(Value::as_str)
        .is_none_or(|s| s.trim().is_empty());
    if type_missing {
        normalized.insert("type".into(), json!("object"));
    }
    let typ = normalized
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let needs_object = enforce_object
        || typ == "object"
        || normalized.contains_key("properties")
        || normalized.contains_key("required")
        || normalized.contains_key("additionalProperties");
    if needs_object {
        let props = match normalized.get("properties").cloned() {
            Some(Value::Object(map)) => {
                let mut out = serde_json::Map::new();
                for (key, value) in map {
                    out.insert(key, normalize_schema(&value, false));
                }
                Value::Object(out)
            }
            _ => json!({}),
        };
        normalized.insert("properties".into(), props);
        let required = normalized
            .get("required")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .map(Value::String)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        normalized.insert("required".into(), Value::Array(required));
        match normalized.get("additionalProperties").cloned() {
            Some(Value::Bool(_)) => {}
            Some(Value::Object(_)) => {
                let nested = normalized
                    .get("additionalProperties")
                    .cloned()
                    .unwrap_or(json!({}));
                normalized.insert(
                    "additionalProperties".into(),
                    normalize_schema(&nested, false),
                );
            }
            _ => {
                normalized.insert("additionalProperties".into(), json!(true));
            }
        }
    }
    Value::Object(normalized)
}

fn normalize_schema_child(key: &str, value: &Value) -> Value {
    match key {
        "items" | "not" => match value {
            Value::Object(_) => normalize_schema(value, false),
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| normalize_schema(item, false))
                    .collect(),
            ),
            other => other.clone(),
        },
        "oneOf" | "anyOf" | "allOf" => match value {
            Value::Array(items) => Value::Array(
                items
                    .iter()
                    .map(|item| normalize_schema(item, false))
                    .collect(),
            ),
            other => other.clone(),
        },
        _ => value.clone(),
    }
}

/// 历史里的 tool_use 必须有对应结果，否则 Kiro 拒收整段。本轮没声明、但历史用过的工具补一个占位。
fn reconcile_tools(
    history: &mut [Value],
    current: &mut Value,
    allow_tools: bool,
    tools: &mut Vec<Value>,
) {
    let mut all_ids = std::collections::HashSet::new();
    let mut paired = std::collections::HashSet::new();
    for msg in history.iter() {
        if let Some(uses) = msg
            .pointer("/assistantResponseMessage/toolUses")
            .and_then(Value::as_array)
        {
            for use_ in uses {
                if let Some(id) = use_.get("toolUseId").and_then(Value::as_str) {
                    all_ids.insert(id.to_string());
                }
            }
        }
        if let Some(results) = msg
            .pointer("/userInputMessage/userInputMessageContext/toolResults")
            .and_then(Value::as_array)
        {
            for result in results {
                if let Some(id) = result.get("toolUseId").and_then(Value::as_str) {
                    paired.insert(id.to_string());
                }
            }
        }
    }
    let existing = current
        .pointer("/userInputMessage/userInputMessageContext/toolResults")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut kept = Vec::new();
    for result in existing {
        let id = result
            .get("toolUseId")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if all_ids.contains(&id) && paired.insert(id) {
            kept.push(result);
        }
    }
    if let Some(ctx) = current
        .pointer_mut("/userInputMessage/userInputMessageContext")
        .and_then(Value::as_object_mut)
    {
        if kept.is_empty() {
            ctx.remove("toolResults");
            if ctx.is_empty() {
                current
                    .pointer_mut("/userInputMessage")
                    .and_then(Value::as_object_mut)
                    .map(|msg| msg.remove("userInputMessageContext"));
            }
            if current
                .pointer("/userInputMessage/content")
                .and_then(Value::as_str)
                == Some("Tool results provided.")
            {
                if let Some(content) = current.pointer_mut("/userInputMessage/content") {
                    *content = json!("Continue");
                }
            }
        } else {
            ctx.insert("toolResults".into(), Value::Array(kept));
        }
    }
    for msg in history.iter_mut() {
        let Some(uses) = msg
            .pointer_mut("/assistantResponseMessage/toolUses")
            .and_then(Value::as_array_mut)
        else {
            continue;
        };
        uses.retain(|use_| {
            use_.get("toolUseId")
                .and_then(Value::as_str)
                .is_some_and(|id| paired.contains(id))
        });
        if uses.is_empty() {
            msg.pointer_mut("/assistantResponseMessage")
                .and_then(Value::as_object_mut)
                .map(|assistant| assistant.remove("toolUses"));
        }
    }
    if !allow_tools {
        tools.clear();
        return;
    }
    let mut seen: std::collections::HashSet<String> = tools
        .iter()
        .filter_map(|tool| {
            tool.pointer("/toolSpecification/name")
                .and_then(Value::as_str)
                .map(|name| name.to_ascii_lowercase())
        })
        .collect();
    for name in history_tool_names(history) {
        let key = name.to_ascii_lowercase();
        if !seen.insert(key) {
            continue;
        }
        tools.push(json!({
            "toolSpecification": {
                "name": name,
                "description": "Tool used in conversation history",
                "inputSchema": { "json": default_schema() },
            }
        }));
    }
}

fn history_tool_names(history: &[Value]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut names = Vec::new();
    for msg in history {
        let Some(uses) = msg
            .pointer("/assistantResponseMessage/toolUses")
            .and_then(Value::as_array)
        else {
            continue;
        };
        for use_ in uses {
            let Some(name) = use_
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                continue;
            };
            if seen.insert(name.to_ascii_lowercase()) {
                names.push(name.to_string());
            }
        }
    }
    names
}

fn kiro_images(images: &[ImageInput]) -> Vec<Value> {
    images
        .iter()
        .filter(|img| !img.data.is_empty())
        .map(|img| {
            let format = img
                .mime_type
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or("png");
            json!({ "format": format, "source": { "bytes": img.data } })
        })
        .collect()
}

fn injected_system(
    system: &str,
    request: &ChatRequest,
    names: &mut HashMap<String, String>,
) -> String {
    let now = time::OffsetDateTime::now_utc();
    let stamp = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
        now.year(),
        now.month() as u8,
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    );
    let mut text = format!("[Context: Current time is {stamp}]");
    if !system.trim().is_empty() {
        text.push_str("\n\n");
        text.push_str(system.trim());
    }
    let hint = match &request.tool_choice {
        ToolChoice::None => "[INSTRUCTION: Do not use any tools. Respond with text only.]".to_string(),
        ToolChoice::Required => "[INSTRUCTION: You MUST use at least one of the available tools to respond. Do not respond with text only - always make a tool call.]".to_string(),
        ToolChoice::Tool(name) => {
            let wired = wire_tool_name(name, names);
            format!("[INSTRUCTION: You MUST use the tool named '{wired}' to respond. Do not use any other tool or respond with text only.]")
        }
        ToolChoice::Auto => String::new(),
    };
    if !hint.is_empty() {
        text.push('\n');
        text.push_str(&hint);
    }
    text.push_str("\nWhen Write or Edit tools include chunking limits, comply silently and complete the operation through multiple tool calls when needed.");
    match thinking_dir(request) {
        Think::Off => text,
        Think::Adaptive { effort } => format!(
            "<thinking_mode>adaptive</thinking_mode>\n<thinking_effort>{effort}</thinking_effort>\n\n{text}"
        ),
        Think::Enabled { budget } => format!(
            "<thinking_mode>enabled</thinking_mode>\n<max_thinking_length>{budget}</max_thinking_length>\n\n{text}"
        ),
    }
}

enum Think {
    Off,
    Adaptive { effort: String },
    Enabled { budget: u32 },
}

impl Think {
    fn is_on(&self) -> bool {
        !matches!(self, Think::Off)
    }
}

/// 模型名带 `-thinking`，或者客户端在原始请求里打开了 thinking / reasoning。
fn thinking_dir(request: &ChatRequest) -> Think {
    let model = request.model.to_ascii_lowercase();
    if model.contains("thinking") {
        if model.contains("opus-4.6") || model.contains("opus-4-6") {
            return Think::Adaptive {
                effort: "high".into(),
            };
        }
        return Think::Enabled { budget: 20_000 };
    }
    let Some(raw) = request.raw_inbound.as_ref() else {
        return Think::Off;
    };
    let body = raw.body.as_ref();
    match body
        .pointer("/thinking/type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "adaptive" => {
            let effort = body
                .pointer("/output_config/effort")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("high");
            return Think::Adaptive {
                effort: effort.to_string(),
            };
        }
        "enabled" => {
            let budget = body
                .pointer("/thinking/budget_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(16_000) as u32;
            return Think::Enabled {
                budget: if budget == 0 { 16_000 } else { budget },
            };
        }
        _ => {}
    }
    let interleaved = raw.headers.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("anthropic-beta")
            && v.to_ascii_lowercase().contains("interleaved-thinking")
    });
    if interleaved {
        return Think::Enabled { budget: 16_000 };
    }
    if let Some(effort) = body.get("reasoning_effort").and_then(Value::as_str) {
        if !effort.is_empty() && !effort.eq_ignore_ascii_case("none") {
            return Think::Enabled { budget: 16_000 };
        }
    }
    Think::Off
}

fn inference_config(request: &ChatRequest) -> Option<Value> {
    let mut cfg = serde_json::Map::new();
    if let Some(max) = request.sampling.max_output_tokens {
        if max > 0 {
            cfg.insert("maxTokens".into(), json!(max.min(32_000)));
        }
    }
    if let Some(temp) = request.sampling.temperature {
        cfg.insert("temperature".into(), json!(temp));
    }
    if let Some(top_p) = request.sampling.top_p {
        cfg.insert("topP".into(), json!(top_p));
    }
    if !cfg.contains_key("maxTokens") {
        let unlimited = request.raw_inbound.as_ref().is_some_and(|raw| {
            raw.body.get("max_tokens").and_then(Value::as_i64) == Some(-1)
                || raw
                    .body
                    .get("max_completion_tokens")
                    .and_then(Value::as_i64)
                    == Some(-1)
        });
        if unlimited {
            cfg.insert("maxTokens".into(), json!(32_000));
        }
    }
    (!cfg.is_empty()).then(|| Value::Object(cfg))
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
    thinking: String,
    tool_calls: Vec<ToolCall>,
    pending: Vec<(String, ToolUseBuf)>,
    ttft: Option<u64>,
    /// 上游明确说的错误（`errorEvent` / 异常帧）。
    error: Option<String>,
    names: HashMap<String, String>,
    /// 已经交给客户端的工具，按名字 + 参数去重。正文里的 `[Called …]` 和 toolUseEvent 会撞车。
    seen_tools: HashSet<String>,
    thinking_on: bool,
    in_thinking: bool,
    in_fence: bool,
    hold: String,
    /// 还没确认是不是 `[Called name with args: {…}]` 的正文尾巴。
    tool_hold: String,
    usage: Option<Usage>,
    stop: Option<String>,
}

async fn read_eventstream(
    mut res: reqwest::Response,
    started: Instant,
    idle: Duration,
    names: HashMap<String, String>,
    thinking_on: bool,
    on_delta: DeltaSink<'_>,
) -> Result<Completion, UpstreamError> {
    let mut buf = Vec::new();
    let mut c = Collected {
        names,
        thinking_on,
        ..Collected::default()
    };
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
    if let Some(msg) = c.error.take() {
        return Err(UpstreamError::new(
            UpstreamKind::Upstream,
            502,
            format!("Kiro：{msg}"),
        ));
    }
    flush_hold(&mut c, started, on_delta, false);
    // 没等到 stop 的调用也交出去：上游偶尔不发 stop 就结束流。
    let pending = std::mem::take(&mut c.pending);
    for (id, buf) in pending {
        if let Some(call) = finish_tool(&c.names, id, buf) {
            accept_tool(&mut c, call);
        }
    }
    drain_embedded(&mut c, true, started, on_delta);
    Ok(finish(c, started))
}

fn finish(c: Collected, started: Instant) -> Completion {
    let mut out = done(c.text, started);
    out.thinking = c.thinking;
    out.ttft_ms = c.ttft;
    if let Some(usage) = c.usage {
        out.usage = usage;
        out.usage_measured = true;
    }
    let has_tools = !c.tool_calls.is_empty();
    if has_tools {
        out.finish_reason = FinishReason::ToolCalls;
        out.tool_calls = c.tool_calls;
    }
    if let Some(reason) = c.stop.as_deref() {
        out.finish_reason = map_stop(reason);
    } else if has_tools {
        out.finish_reason = FinishReason::ToolCalls;
    }
    if c.thinking_on
        && out.text.trim().is_empty()
        && out.tool_calls.is_empty()
        && !out.thinking.is_empty()
    {
        out.text = " ".into();
        out.finish_reason = FinishReason::Length;
    }
    out
}

fn map_stop(reason: &str) -> FinishReason {
    match reason {
        "max_tokens" | "length" | "max_output_tokens" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "content_filter" | "refusal" => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

/// 流被截断时工具参数经常少半个括号。补上引号和括号，补完仍不是 JSON 就原样交回去。
fn repair_json(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return "{}".into();
    }
    if serde_json::from_str::<Value>(trimmed).is_ok() {
        return trimmed.to_string();
    }
    let mut s = escape_controls_in_strings(trimmed);
    s = strip_trailing_commas(&s);
    let mut balance = json_balance(&s);
    if balance.in_string {
        s.push('"');
        balance = json_balance(&s);
    }
    for _ in 0..balance.braces.max(0) {
        s.push('}');
    }
    for _ in 0..balance.brackets.max(0) {
        s.push(']');
    }
    if serde_json::from_str::<Value>(&s).is_ok() {
        s
    } else {
        trimmed.to_string()
    }
}

fn escape_controls_in_strings(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escape = false;
    for ch in input.chars() {
        if escape {
            out.push(ch);
            escape = false;
            continue;
        }
        if ch == '\\' {
            out.push(ch);
            escape = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            out.push(ch);
            continue;
        }
        if in_string {
            match ch {
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push(ch),
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn strip_trailing_commas(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ',' {
            let mut j = i + 1;
            while j < chars.len() && chars[j].is_whitespace() {
                j += 1;
            }
            if j < chars.len() && (chars[j] == '}' || chars[j] == ']') {
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

struct JsonBalance {
    braces: i32,
    brackets: i32,
    in_string: bool,
}

fn json_balance(input: &str) -> JsonBalance {
    let mut braces = 0i32;
    let mut brackets = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for ch in input.chars() {
        if escape {
            escape = false;
            continue;
        }
        if ch == '\\' {
            escape = true;
            continue;
        }
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '{' => braces += 1,
            '}' => braces -= 1,
            '[' => brackets += 1,
            ']' => brackets -= 1,
            _ => {}
        }
    }
    JsonBalance {
        braces,
        brackets,
        in_string,
    }
}

fn finish_tool(names: &HashMap<String, String>, id: String, buf: ToolUseBuf) -> Option<ToolCall> {
    let wire = if buf.name.is_empty() {
        "tool".to_string()
    } else {
        buf.name
    };
    let raw = buf.input.trim();
    let repaired = repair_json(&buf.input);
    if tool_is_truncated(&wire, raw, &repaired) {
        return None;
    }
    Some(ToolCall {
        id,
        name: names.get(&wire).cloned().unwrap_or(wire),
        arguments: repaired,
    })
}

fn accept_tool(c: &mut Collected, call: ToolCall) {
    let key = tool_content_key(&call.name, &call.arguments);
    if key.is_empty() || !c.seen_tools.insert(key) {
        return;
    }
    c.tool_calls.push(call);
}

fn tool_content_key(name: &str, arguments: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return String::new();
    }
    let canonical = serde_json::from_str::<Value>(arguments)
        .ok()
        .map(|v| v.to_string())
        .unwrap_or_else(|| arguments.trim().to_string());
    format!("{}:{canonical}", name.to_ascii_lowercase())
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
    if let Some(reason) = read_stop(body).or_else(|| read_stop(&v)) {
        c.stop = Some(reason);
    }
    if let Some(usage) = read_usage(body).or_else(|| read_usage(&v)) {
        c.usage = Some(usage);
    }
    match kind.as_str() {
        "assistantResponseEvent" => {
            if let Some(piece) = body
                .get("content")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                feed_assistant_text(c, piece, started, on_delta);
            }
        }
        "reasoningContentEvent" => {
            if let Some(piece) = body
                .get("text")
                .or_else(|| body.get("content"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                emit_thinking(c, piece, started, on_delta);
            }
        }
        "toolUseEvent" => {
            flush_hold(c, started, on_delta, false);
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
                    if let Some(call) = finish_tool(&c.names, id, buf) {
                        accept_tool(c, call);
                    }
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

fn read_stop(v: &Value) -> Option<String> {
    v.get("stopReason")
        .or_else(|| v.get("stop_reason"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn json_u32(v: Option<&Value>) -> Option<u32> {
    let n = v?
        .as_u64()
        .or_else(|| v?.as_i64().filter(|n| *n >= 0).map(|n| n as u64))?;
    u32::try_from(n).ok()
}

fn read_usage(v: &Value) -> Option<Usage> {
    let token = v.get("tokenUsage")?;
    let mut usage = Usage::default();
    let mut any = false;
    if let Some(n) = json_u32(token.get("uncachedInputTokens")) {
        usage.input_tokens = n;
        any = true;
    }
    if let Some(n) = json_u32(token.get("outputTokens")) {
        usage.output_tokens = n;
        any = true;
    }
    if let Some(n) = json_u32(token.get("cacheReadInputTokens")) {
        usage.cache_read_tokens = n;
        usage.input_tokens = usage.input_tokens.saturating_add(n);
        any = true;
    }
    any.then_some(usage)
}

const THINK_OPEN: &str = "<thinking>";
const THINK_CLOSE: &str = "</thinking>";

fn feed_assistant_text(c: &mut Collected, piece: &str, started: Instant, on_delta: DeltaSink<'_>) {
    if !c.thinking_on {
        emit_text(c, piece, started, on_delta);
        return;
    }
    c.hold.push_str(piece);
    loop {
        if !c.in_thinking {
            if let Some(pos) = find_real_tag(&c.hold, THINK_OPEN, c.in_fence) {
                let before = c.hold[..pos].to_string();
                let rest = c.hold[pos + THINK_OPEN.len()..].to_string();
                c.hold = rest;
                c.in_thinking = true;
                if !before.is_empty() {
                    note_fences(&before, &mut c.in_fence);
                    emit_text(c, &before, started, on_delta);
                }
                continue;
            }
            let keep = partial_tag_len(&c.hold, THINK_OPEN);
            let emit_len = c.hold.len() - keep;
            if emit_len > 0 {
                let text = c.hold[..emit_len].to_string();
                c.hold = c.hold[emit_len..].to_string();
                note_fences(&text, &mut c.in_fence);
                emit_text(c, &text, started, on_delta);
            }
            break;
        }
        if let Some(pos) = find_thinking_end(&c.hold, c.in_fence, false) {
            let before = c.hold[..pos].to_string();
            let after_tag = pos + THINK_CLOSE.len();
            let rest = if c.hold[after_tag..].starts_with("\n\n") {
                c.hold[after_tag + 2..].to_string()
            } else {
                c.hold[after_tag..].trim_start().to_string()
            };
            c.hold = rest;
            c.in_thinking = false;
            if !before.is_empty() {
                emit_thinking(c, &before, started, on_delta);
            }
            continue;
        }
        let emit_len = split_keeping_tail(&c.hold, THINK_CLOSE.len() + 2);
        if emit_len > 0 {
            let text = c.hold[..emit_len].to_string();
            c.hold = c.hold[emit_len..].to_string();
            emit_thinking(c, &text, started, on_delta);
        }
        break;
    }
}

fn flush_hold(c: &mut Collected, started: Instant, on_delta: DeltaSink<'_>, eof: bool) {
    if !c.hold.is_empty() {
        if c.in_thinking {
            if let Some(pos) = find_thinking_end(&c.hold, c.in_fence, true) {
                let before = c.hold[..pos].to_string();
                let after = c.hold[pos + THINK_CLOSE.len()..].trim_start().to_string();
                c.hold.clear();
                c.in_thinking = false;
                if !before.is_empty() {
                    emit_thinking(c, &before, started, on_delta);
                }
                if !after.is_empty() {
                    note_fences(&after, &mut c.in_fence);
                    emit_text(c, &after, started, on_delta);
                }
            } else {
                let rest = std::mem::take(&mut c.hold);
                emit_thinking(c, &rest, started, on_delta);
                c.in_thinking = false;
            }
        } else {
            let rest = std::mem::take(&mut c.hold);
            note_fences(&rest, &mut c.in_fence);
            emit_text(c, &rest, started, on_delta);
        }
    }
    drain_embedded(c, eof, started, on_delta);
}

/// 流里的 `</thinking>` 后面必须是空行。缓冲结束时，后面只剩空白也算收口。
fn find_thinking_end(buf: &str, in_fence: bool, at_end: bool) -> Option<usize> {
    let mut search_from = 0;
    while let Some(pos) = find_real_tag_from(buf, THINK_CLOSE, in_fence, search_from) {
        let rest = &buf[pos + THINK_CLOSE.len()..];
        if rest.starts_with("\n\n") || (at_end && rest.trim().is_empty()) {
            return Some(pos);
        }
        search_from = pos + 1;
    }
    None
}

fn find_real_tag_from(
    buf: &str,
    tag: &str,
    in_fence: bool,
    mut search_from: usize,
) -> Option<usize> {
    while let Some(rel) = buf[search_from..].find(tag) {
        let pos = search_from + rel;
        let after = pos + tag.len();
        let quoted = quoted_tag(buf, pos, after);
        let fenced = fence_before(buf, pos, in_fence);
        let quoted_line = line_is_blockquote(buf, pos);
        if !quoted && !fenced && !quoted_line {
            return Some(pos);
        }
        search_from = pos + 1;
    }
    None
}

fn find_real_tag(buf: &str, tag: &str, in_fence: bool) -> Option<usize> {
    find_real_tag_from(buf, tag, in_fence, 0)
}

fn quoted_tag(buf: &str, start: usize, after: usize) -> bool {
    let bytes = buf.as_bytes();
    let before = start > 0 && matches!(bytes[start - 1], b'`' | b'"' | b'\'' | b'\\');
    let next = after < bytes.len() && matches!(bytes[after], b'`' | b'"' | b'\'' | b'\\');
    before || next
}

fn line_is_blockquote(buf: &str, pos: usize) -> bool {
    let line_start = buf[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
    buf[line_start..pos].trim_start().starts_with('>')
}

fn fence_before(buf: &str, pos: usize, mut in_fence: bool) -> bool {
    note_fences(&buf[..pos], &mut in_fence);
    in_fence
}

fn note_fences(text: &str, in_fence: &mut bool) {
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            *in_fence = !*in_fence;
        }
    }
}

fn partial_tag_len(buf: &str, tag: &str) -> usize {
    let bytes = buf.as_bytes();
    let tag_b = tag.as_bytes();
    let max = tag_b.len().saturating_sub(1).min(bytes.len());
    for n in (1..=max).rev() {
        let start = bytes.len() - n;
        if buf.is_char_boundary(start) && &bytes[start..] == &tag_b[..n] {
            return n;
        }
    }
    0
}

fn split_keeping_tail(buf: &str, keep: usize) -> usize {
    if buf.len() <= keep {
        return 0;
    }
    let mut pos = buf.len() - keep;
    while pos > 0 && !buf.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
}

fn emit_text(c: &mut Collected, piece: &str, started: Instant, on_delta: DeltaSink<'_>) {
    if piece.is_empty() {
        return;
    }
    c.tool_hold.push_str(piece);
    drain_embedded(c, false, started, on_delta);
}

fn push_text(c: &mut Collected, piece: &str, started: Instant, on_delta: DeltaSink<'_>) {
    if piece.is_empty() {
        return;
    }
    if c.ttft.is_none() {
        c.ttft = Some(started.elapsed().as_millis() as u64);
    }
    c.text.push_str(piece);
    on_delta(Delta::Text(piece.to_string()));
}

const CALLED: &str = "[Called ";
const ARGS_MARK: &str = " with args:";

/// 把正文里写完整的 `[Called name with args: {…}]` 拆成工具调用。
/// 没写完的留在 `tool_hold`；流结束时丢掉，不把这句标记漏给客户端。
fn drain_embedded(c: &mut Collected, eof: bool, started: Instant, on_delta: DeltaSink<'_>) {
    loop {
        let Some(idx) = c.tool_hold.find(CALLED) else {
            if eof {
                let rest = std::mem::take(&mut c.tool_hold);
                push_text(c, &rest, started, on_delta);
            } else {
                let emit_len = c.tool_hold.len() - partial_tag_len(&c.tool_hold, CALLED);
                if emit_len == 0 {
                    return;
                }
                let text = c.tool_hold[..emit_len].to_string();
                c.tool_hold = c.tool_hold[emit_len..].to_string();
                push_text(c, &text, started, on_delta);
            }
            return;
        };
        if let Some((name, raw, end)) = parse_embedded_at(&c.tool_hold, idx) {
            let before = c.tool_hold[..idx].to_string();
            let rest = c.tool_hold[end..].to_string();
            c.tool_hold = rest;
            push_text(c, &before, started, on_delta);
            if let Some(call) = embedded_tool(&c.names, &name, &raw) {
                accept_tool(c, call);
            }
            continue;
        }
        let before = c.tool_hold[..idx].to_string();
        let rest = c.tool_hold[idx..].to_string();
        c.tool_hold = rest;
        push_text(c, &before, started, on_delta);
        if eof {
            c.tool_hold.clear();
        }
        return;
    }
}

fn parse_embedded_at(text: &str, start: usize) -> Option<(String, String, usize)> {
    if !text[start..].starts_with(CALLED) {
        return None;
    }
    let pos = start + CALLED.len();
    let rel = text[pos..].find(ARGS_MARK)?;
    let args_at = pos + rel;
    let name = text[pos..args_at].trim();
    if name.is_empty() {
        return None;
    }
    let bytes = text.as_bytes();
    let mut json_start = args_at + ARGS_MARK.len();
    while json_start < bytes.len() && matches!(bytes[json_start], b' ' | b'\t' | b'\n') {
        json_start += 1;
    }
    if json_start >= bytes.len() || bytes[json_start] != b'{' {
        return None;
    }
    let json_end = matching_brace(text, json_start)?;
    let mut end = json_end + 1;
    while end < bytes.len() && bytes[end] != b']' {
        end += 1;
    }
    if end >= bytes.len() {
        return None;
    }
    Some((
        name.to_string(),
        text[json_start..=json_end].to_string(),
        end + 1,
    ))
}

fn matching_brace(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, &ch) in bytes.iter().enumerate().skip(start) {
        if escape {
            escape = false;
            continue;
        }
        if ch == b'\\' {
            escape = true;
            continue;
        }
        if ch == b'"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn embedded_tool(names: &HashMap<String, String>, name: &str, raw: &str) -> Option<ToolCall> {
    let wire = name.trim();
    if wire.is_empty() {
        return None;
    }
    let repaired = repair_json(raw);
    if tool_is_truncated(wire, raw, &repaired) {
        return None;
    }
    let id = format!("toolu_{}", &uuid::Uuid::new_v4().simple().to_string()[..24]);
    Some(ToolCall {
        id,
        name: names.get(wire).cloned().unwrap_or_else(|| wire.to_string()),
        arguments: repaired,
    })
}

fn tool_is_truncated(name: &str, raw: &str, repaired: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() {
        return tool_has_requirements(name);
    }
    if looks_truncated_json(raw) {
        return true;
    }
    let parsed = serde_json::from_str::<Value>(repaired).unwrap_or(Value::Null);
    let obj = if parsed.is_object() {
        parsed
    } else {
        json!({})
    };
    missing_required_fields(name, &obj)
}

fn looks_truncated_json(raw: &str) -> bool {
    let raw = raw.trim();
    if raw.is_empty() || !raw.starts_with('{') {
        return false;
    }
    let balance = json_balance(raw);
    if balance.braces > 0 || balance.brackets > 0 || balance.in_string {
        return true;
    }
    matches!(raw.as_bytes().last(), Some(b':' | b','))
}

fn tool_has_requirements(name: &str) -> bool {
    required_fields(name).is_some()
}

fn missing_required_fields(name: &str, input: &Value) -> bool {
    let Some(groups) = required_fields(name) else {
        return false;
    };
    let obj = input.as_object();
    groups.iter().any(|group| {
        !group
            .iter()
            .any(|field| obj.is_some_and(|map| map.contains_key(*field)))
    })
}

fn required_fields(name: &str) -> Option<&'static [&'static [&'static str]]> {
    const WRITE: &[&[&str]] = &[&["file_path", "path"], &["content"]];
    const PATH_CONTENT: &[&[&str]] = &[&["path"], &["content"]];
    const PATH: &[&[&str]] = &[&["path"]];
    const APPLY: &[&[&str]] = &[&["path"], &["diff"]];
    const REPLACE: &[&[&str]] = &[&["path"], &["old_str"], &["new_str"]];
    const BASH: &[&[&str]] = &[&["cmd", "command"]];
    const COMMAND: &[&[&str]] = &[&["command"]];
    match name.trim().to_ascii_lowercase().as_str() {
        "write" => Some(WRITE),
        "write_to_file" | "fswrite" | "create_file" => Some(PATH_CONTENT),
        "edit_file" => Some(PATH),
        "apply_diff" => Some(APPLY),
        "str_replace_editor" => Some(REPLACE),
        "bash" => Some(BASH),
        "execute" | "run_command" => Some(COMMAND),
        _ => None,
    }
}

fn emit_thinking(c: &mut Collected, piece: &str, started: Instant, on_delta: DeltaSink<'_>) {
    if piece.is_empty() {
        return;
    }
    if c.ttft.is_none() {
        c.ttft = Some(started.elapsed().as_millis() as u64);
    }
    c.thinking.push_str(piece);
    on_delta(Delta::Thinking(piece.to_string()));
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
        let (body, _) = build_body(&req, "claude-sonnet-4.5", None);
        assert_eq!(body["conversationState"]["agentTaskType"], "vibe");
        assert!(body.get("profileArn").is_none());
        let current = body
            .pointer("/conversationState/currentMessage/userInputMessage/content")
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(current, "again");
        let hist = body
            .pointer("/conversationState/history")
            .and_then(|v| v.as_array())
            .unwrap();
        assert_eq!(hist.len(), 4);
        let system = hist[0]["userInputMessage"]["content"].as_str().unwrap();
        assert!(system.contains("[Context: Current time is "));
        assert!(system.contains("be brief"));
        assert_eq!(
            hist[1]["assistantResponseMessage"]["content"]
                .as_str()
                .unwrap(),
            "I will follow these instructions."
        );
        assert_eq!(
            hist[2]["userInputMessage"]["content"].as_str().unwrap(),
            "hi"
        );
        assert_eq!(
            hist[3]["assistantResponseMessage"]["content"]
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
        let (body, _) = build_body(&req, "claude-sonnet-4.5", None);
        let cur = &body["conversationState"]["currentMessage"]["userInputMessage"];
        assert_eq!(cur["content"], "Tool results provided.");
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
        assert_eq!(hist.len(), 4);
        assert_eq!(
            hist[3]["assistantResponseMessage"]["toolUses"][0]["input"]["path"],
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

    #[test]
    fn truncated_tool_json_is_closed_before_it_reaches_the_client() {
        let started = Instant::now();
        let mut c = Collected::default();
        let mut sink = |_: Delta| {};
        let mut buf = Vec::new();
        buf.extend(frame(
            Some("toolUseEvent"),
            br#"{"toolUseId":"tu_cut","name":"read_file","input":"{\"path\":\"a.rs\"","stop":true}"#,
        ));
        let mut off = 0;
        while let Some((n, ev)) = next_event(&buf[off..]) {
            off += n;
            handle_event(&ev, &mut c, started, &mut sink);
        }
        assert!(c.tool_calls.is_empty(), "没写完的参数不交给客户端");
    }

    #[test]
    fn embedded_tool_text_becomes_a_tool_call_and_duplicates_collapse() {
        let started = Instant::now();
        let mut c = Collected::default();
        let mut sink = |_: Delta| {};
        feed_assistant_text(
            &mut c,
            "Look. [Called read_file with args: {\"path\":\"a.rs\"}] Next",
            started,
            &mut sink,
        );
        flush_hold(&mut c, started, &mut sink, true);
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].name, "read_file");
        assert_eq!(c.tool_calls[0].arguments, r#"{"path":"a.rs"}"#);
        assert!(c.text.contains("Look."));
        assert!(c.text.contains("Next"));
        assert!(!c.text.contains("[Called"));

        let mut buf = Vec::new();
        buf.extend(frame(
            Some("toolUseEvent"),
            br#"{"toolUseId":"tu_same","name":"read_file","input":"{\"path\":\"a.rs\"}","stop":true}"#,
        ));
        let mut off = 0;
        while let Some((n, ev)) = next_event(&buf[off..]) {
            off += n;
            handle_event(&ev, &mut c, started, &mut sink);
        }
        assert_eq!(c.tool_calls.len(), 1, "同一调用不执行两次");
    }

    #[test]
    fn incomplete_embedded_call_is_dropped_at_the_end_of_the_stream() {
        let started = Instant::now();
        let mut c = Collected::default();
        let mut sink = |_: Delta| {};
        feed_assistant_text(
            &mut c,
            "hi [Called read_file with args: {\"path\"",
            started,
            &mut sink,
        );
        flush_hold(&mut c, started, &mut sink, true);
        assert!(c.tool_calls.is_empty());
        assert_eq!(c.text, "hi ");
        assert!(!c.text.contains("[Called"));
    }

    #[test]
    fn write_missing_content_is_not_forwarded() {
        let started = Instant::now();
        let mut c = Collected::default();
        let mut sink = |_: Delta| {};
        let mut buf = Vec::new();
        buf.extend(frame(
            Some("toolUseEvent"),
            br#"{"toolUseId":"tu_w","name":"write","input":"{\"path\":\"a.rs\"}","stop":true}"#,
        ));
        let mut off = 0;
        while let Some((n, ev)) = next_event(&buf[off..]) {
            off += n;
            handle_event(&ev, &mut c, started, &mut sink);
        }
        assert!(c.tool_calls.is_empty());
    }

    #[test]
    fn adjacent_user_turns_merge_and_continuation_id_is_forwarded() {
        use crate::inbound::Dialect;
        use crate::normalized::RawInbound;
        let mut req = ChatRequest {
            messages: vec![
                Message::text(Role::User, "first"),
                Message::text(Role::User, "second"),
            ],
            ..Default::default()
        };
        req.raw_inbound = Some(RawInbound {
            dialect: Dialect::AnthropicMessages,
            body: Arc::new(json!({
                "messages": [{
                    "role": "user",
                    "content": "second",
                    "additional_kwargs": {
                        "conversationId": "conv-kiro",
                        "continuationId": "cont-1"
                    }
                }]
            })),
            stream: true,
            headers: Vec::new(),
        });
        let (body, _) = build_body(&req, "claude-sonnet-4.5", None);
        assert_eq!(body["conversationState"]["conversationId"], "conv-kiro");
        assert_eq!(body["conversationState"]["agentContinuationId"], "cont-1");
        assert_eq!(
            body["conversationState"]["currentMessage"]["userInputMessage"]["content"],
            "first\n\nsecond"
        );
        let hist = body["conversationState"]["history"].as_array().unwrap();
        assert_eq!(hist.len(), 2, "两条 user 合成一条，不再各占一轮");
    }

    #[test]
    fn web_search_uses_kiro_description_and_write_mentions_the_placeholder() {
        use crate::normalized::ToolDef;
        let req = ChatRequest {
            tools: vec![
                ToolDef {
                    name: "web_search".into(),
                    description: "client text".into(),
                    ..Default::default()
                },
                ToolDef {
                    name: "write".into(),
                    description: "write a file".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let (body, names) = build_body(&req, "claude-sonnet-4.5", None);
        let tools = body["conversationState"]["currentMessage"]["userInputMessage"]
            ["userInputMessageContext"]["tools"]
            .as_array()
            .unwrap();
        assert_eq!(tools[0]["toolSpecification"]["name"], "remote_web_search");
        assert!(tools[0]["toolSpecification"]["description"]
            .as_str()
            .unwrap()
            .contains("outside the model's training data"));
        assert!(tools[1]["toolSpecification"]["description"]
            .as_str()
            .unwrap()
            .contains("unique placeholder"));
        assert_eq!(
            names.get("remote_web_search").map(String::as_str),
            Some("web_search")
        );
    }

    #[test]
    fn nested_schema_is_normalized_and_history_tools_get_placeholders() {
        use crate::normalized::{ToolDef, ToolResult};
        let mut asst = Message::text(Role::Assistant, "ok");
        asst.tool_calls.push(ToolCall {
            id: "tu_old".into(),
            name: "read_file".into(),
            arguments: "{}".into(),
        });
        let mut tool = Message::text(Role::Tool, "");
        tool.tool_results.push(ToolResult {
            tool_call_id: "tu_old".into(),
            tool_name: "read_file".into(),
            text: "1".into(),
            is_error: false,
        });
        let mut orphan = Message::text(Role::Assistant, "dangling");
        orphan.tool_calls.push(ToolCall {
            id: "tu_orphan".into(),
            name: "write".into(),
            arguments: "{}".into(),
        });
        let req = ChatRequest {
            model: "kiro-claude-sonnet-4.5".into(),
            messages: vec![
                Message::text(Role::User, "go"),
                asst,
                tool,
                orphan,
                Message::text(Role::User, "next"),
            ],
            tools: vec![ToolDef {
                name: "other".into(),
                description: "other".into(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "child": { "properties": { "n": { "type": "integer" } } },
                        "tags": {
                            "type": "array",
                            "items": { "properties": { "id": { "type": "string" } } }
                        }
                    }
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let (body, _) = build_body(&req, "claude-sonnet-4.5", None);
        let tools = body["conversationState"]["currentMessage"]["userInputMessage"]
            ["userInputMessageContext"]["tools"]
            .as_array()
            .unwrap();
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|t| {
                t.pointer("/toolSpecification/name")
                    .and_then(|v| v.as_str())
            })
            .collect();
        assert!(names.contains(&"other"));
        assert!(names.contains(&"read_file"), "历史里用过的工具要有占位声明");
        assert!(!names.contains(&"write"), "没有结果的调用不占位");
        let child = &tools[0]["toolSpecification"]["inputSchema"]["json"]["properties"]["child"];
        assert_eq!(child["type"], "object");
        assert_eq!(child["additionalProperties"], true);
        let item =
            &tools[0]["toolSpecification"]["inputSchema"]["json"]["properties"]["tags"]["items"];
        assert_eq!(item["type"], "object");
        let hist = body["conversationState"]["history"].as_array().unwrap();
        let dangling = hist.iter().find(|m| {
            m.pointer("/assistantResponseMessage/content")
                .and_then(|v| v.as_str())
                == Some("dangling")
        });
        let dangling = dangling.unwrap();
        assert!(dangling["assistantResponseMessage"]
            .get("toolUses")
            .is_none());
    }

    #[test]
    fn quoted_thinking_tags_stay_in_the_text() {
        let started = Instant::now();
        let mut c = Collected {
            thinking_on: true,
            ..Collected::default()
        };
        let mut seen = Vec::new();
        let mut sink = |d: Delta| seen.push(d);
        feed_assistant_text(
            &mut c,
            "see `<thinking>` then <thinking>plan</thinking>\n\n done",
            started,
            &mut sink,
        );
        flush_hold(&mut c, started, &mut sink, true);
        assert_eq!(c.thinking, "plan");
        assert!(c.text.contains("`<thinking>`"));
        assert!(c.text.contains("done"));
    }
}
