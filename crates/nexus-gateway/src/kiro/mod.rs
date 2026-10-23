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
    ToolCall, ToolChoice, Usage, InputBasis,
};
use crate::upstream::{DeltaSink, Upstream};
use nexus_kiro::protocol::{self, chat_headers, Q_ENDPOINT};
use nexus_kiro::{is_kiro_model, upstream_model, KiroService};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

mod eventstream;
mod image;
mod invoke;

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
        let mut outbound = self
            .accounts
            .as_ref()
            .map(|accounts| accounts.outbound_for(&credential.label));
        if let Some(accounts) = &self.accounts {
            if let Some(out) = outbound.as_mut() {
                out.profile_arn = accounts
                    .ensure_streaming_profile(&credential.label, &credential.access_token)
                    .await;
            }
        }
        let machine = outbound
            .as_ref()
            .map(|o| o.machine_id.clone())
            .unwrap_or_else(|| protocol::machine_id(None, &credential.label));
        let profile = outbound.as_ref().and_then(|o| o.profile_arn.clone());
        let token_type = outbound.as_ref().and_then(|o| o.token_type.clone());
        let (body, names) = build_body(request, &model, profile.as_deref());
        let thinking_on = thinking_dir(request).is_on();
        let tool_policy = tool_policy(request, &names);
        let cache_prefix_tokens = estimate_cacheable_prefix(request);
        let is_multi_turn =
            request.messages.iter().filter(|m| m.role == Role::User).count() > 1
                || request.messages.len() > 2;
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
            token_type.as_deref(),
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
        // `content-type` 只能当提示：2026-10 实测 Amazon Q 把 eventstream 响应标成
        // `application/json`，光看头会把整条流丢给 JSON 分支，一帧都解不出来——
        // 表现就是 200、不报错、什么都不出。真正的判据是头几个字节。
        let mut res = res;
        let first = match tokio::time::timeout(self.idle_timeout, res.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk.to_vec(),
            Ok(Ok(None)) => Vec::new(),
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
                    format!("Kiro {} 秒没有动静", self.idle_timeout.as_secs()),
                ));
            }
        };
        let eventstream_hinted = ctype.contains("eventstream") || ctype.contains("octet-stream");
        if !eventstream_hinted && looks_like_json_body(&first) {
            let mut body = first;
            while let Ok(Ok(Some(chunk))) =
                tokio::time::timeout(self.idle_timeout, res.chunk()).await
            {
                body.extend_from_slice(&chunk);
            }
            let text_body = String::from_utf8_lossy(&body).into_owned();
            let mut completion = emit_json_text(&text_body, started, on_delta)?;
            let policy_check = Collected {
                known_tools: tool_policy.allowed.clone(),
                tool_policy_active: true,
                tool_requirement: tool_policy.requirement.clone(),
                ..Collected::default()
            };
            enforce_tool_policy(&policy_check)?;
            completion.routed_model = Some(model);
            return Ok(completion);
        }
        read_eventstream(
            res,
            first,
            started,
            self.idle_timeout,
            names,
            tool_policy,
            model,
            thinking_on,
            cache_prefix_tokens,
            is_multi_turn,
            on_delta,
        )
        .await
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
    // 末尾的 assistant 是 prefill。Kiro 不收，截到最后一条 user / tool。
    let mut messages = merge_adjacent(&request.messages);
    while messages.last().is_some_and(|m| m.role == Role::Assistant) {
        messages.pop();
    }
    let mut history: Vec<Value> = Vec::new();
    let mut current: Option<Value> = None;
    let mut system = String::new();
    let mut pending_results: Vec<Value> = Vec::new();
    let mut pending_images: Vec<ImageInput> = Vec::new();
    let mut seen_images: HashSet<String> = HashSet::new();
    let mut names: HashMap<String, String> = HashMap::new();

    let user_message = |content: String,
                        images: &[ImageInput],
                        results: &mut Vec<Value>,
                        pending_images: &mut Vec<ImageInput>,
                        seen: &mut HashSet<String>|
     -> Value {
        let mut content = if content.trim().is_empty() {
            if results.is_empty() {
                "Continue".to_string()
            } else {
                "Tool results provided.".to_string()
            }
        } else {
            content
        };
        let mut pictures = Vec::new();
        pictures.append(pending_images);
        pictures.extend(images.iter().cloned());
        let prepared = image::prepare(&pictures, seen);
        for note in &prepared.notes {
            if !content.is_empty() && !content.ends_with('\n') {
                content.push('\n');
            }
            content.push_str(note);
        }
        let mut msg = json!({
            "content": content,
            "modelId": model,
            "origin": "AI_EDITOR",
        });
        if !prepared.images.is_empty() {
            msg["images"] = Value::Array(prepared.images);
        }
        let mut ctx = json!({ "envState": env_state() });
        if !results.is_empty() {
            ctx["toolResults"] = Value::Array(std::mem::take(results));
        }
        msg["userInputMessageContext"] = ctx;
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
                    &mut pending_images,
                    &mut seen_images,
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
                    history.push(user_message(
                        filler.to_string(),
                        &[],
                        &mut pending_results,
                        &mut pending_images,
                        &mut seen_images,
                    ));
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
                let had_images = !m.images.is_empty();
                pending_images.extend(m.images.iter().cloned());
                for r in &m.tool_results {
                    let text = if r.text.is_empty() {
                        if had_images {
                            "[image attached]"
                        } else {
                            "Tool use was cancelled by the user"
                        }
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
    let mut current = current.unwrap_or_else(|| {
        user_message(
            String::new(),
            &[],
            &mut pending_results,
            &mut pending_images,
            &mut seen_images,
        )
    });
    let system_text = injected_system(&system, request, model);
    if !system_text.is_empty() {
        let prefix = vec![
            user_message(
                system_text,
                &[],
                &mut Vec::new(),
                &mut Vec::new(),
                &mut seen_images,
            ),
            json!({ "assistantResponseMessage": { "content": "I will follow these instructions." } }),
        ];
        let mut merged = prefix;
        merged.append(&mut history);
        history = merged;
    }
    let hint = tool_choice_hint(request, &mut names);
    append_to_current(&mut current, &hint);
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
    if let Some(fields) = additional_model_fields(model, request) {
        body["additionalModelRequestFields"] = fields;
    }
    (body, names)
}

const REMOTE_WEB_SEARCH_DESCRIPTION: &str = "WebSearch looks up information outside the model's training data. Supports multiple queries to gather comprehensive information.";

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

/// 会话号：客户端显式的 conversationId，其次 Claude Code `metadata.user_id` 里的
/// session UUID，再其次入站已经算出的 id。`agentContinuationId` 没给就从会话号派生，
/// 同一会话每次都一样。
fn session_ids(request: &ChatRequest) -> (String, Option<String>) {
    let mut from_meta = None;
    let mut continuation = None;
    let mut from_user = None;
    if let Some(raw) = request.raw_inbound.as_ref() {
        from_user = raw
            .body
            .pointer("/metadata/user_id")
            .and_then(Value::as_str)
            .and_then(extract_session_id);
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
        .or(from_user)
        .or_else(|| {
            request
                .conversation_id
                .clone()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let continuation = continuation.or_else(|| Some(derive_continuation(&conv)));
    (conv, continuation)
}

fn extract_session_id(user_id: &str) -> Option<String> {
    if let Ok(json) = serde_json::from_str::<Value>(user_id) {
        if let Some(session_id) = json.get("session_id").and_then(Value::as_str) {
            if is_uuid(session_id) {
                return Some(session_id.to_string());
            }
        }
    }
    if let Some(pos) = user_id.find("session_") {
        let session_part = &user_id[pos + "session_".len()..];
        if session_part.len() >= 36 && is_uuid(&session_part[..36]) {
            return Some(session_part[..36].to_string());
        }
    }
    None
}

fn is_uuid(s: &str) -> bool {
    s.len() == 36 && s.chars().filter(|c| *c == '-').count() == 4
}

fn derive_continuation(conversation_id: &str) -> String {
    let name = format!("agent-continuation:{conversation_id}");
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, name.as_bytes()).to_string()
}

fn env_state() -> Value {
    let os = std::env::consts::OS;
    // Amazon Q 要这个对象，但 Nexus 进程的 cwd 既不是调用客户端的工作目录，还会把本机
    // 用户名和项目路径发给上游。没有经过客户端显式传入的可信 cwd 时只给中性相对目录。
    json!({
        "operatingSystem": os,
        "currentWorkingDirectory": ".",
    })
}

/// 工具恢复和收尾校验共用的一份硬边界。提示词只是提高模型遵从率，这里才负责不让
/// `tool_choice=none` 或指定工具之外的调用漏给客户端。
#[derive(Debug, Clone, Default)]
struct ToolPolicy {
    allowed: HashSet<String>,
    requirement: ToolRequirement,
}

#[derive(Debug, Clone, Default)]
enum ToolRequirement {
    #[default]
    None,
    Any,
    Named(String),
}

fn tool_policy(request: &ChatRequest, names: &HashMap<String, String>) -> ToolPolicy {
    let selected: Vec<&str> = match &request.tool_choice {
        ToolChoice::None => Vec::new(),
        ToolChoice::Tool(wanted) => request
            .tools
            .iter()
            .filter(|tool| tool.name.eq_ignore_ascii_case(wanted))
            .map(|tool| tool.name.as_str())
            .collect(),
        ToolChoice::Auto | ToolChoice::Required => request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect(),
    };
    let mut allowed: HashSet<String> = selected
        .iter()
        .map(|name| name.trim().to_ascii_lowercase())
        .filter(|name| !name.is_empty())
        .collect();
    for (wire, original) in names {
        if allowed.contains(&original.to_ascii_lowercase()) {
            allowed.insert(wire.to_ascii_lowercase());
        }
    }
    let requirement = match &request.tool_choice {
        ToolChoice::Required => ToolRequirement::Any,
        ToolChoice::Tool(name) => ToolRequirement::Named(name.trim().to_ascii_lowercase()),
        ToolChoice::Auto | ToolChoice::None => ToolRequirement::None,
    };
    ToolPolicy {
        allowed,
        requirement,
    }
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
    if description.trim().is_empty() {
        format!("Tool: {name}")
    } else {
        description.trim().to_string()
    }
}

fn kiro_schema(schema: &Value) -> Value {
    let Some(obj) = schema.as_object() else {
        return default_schema();
    };
    let mut obj = obj.clone();
    obj.remove("$schema");
    strip_top_level_combinators(&mut obj);
    if obj.get("type").and_then(Value::as_str) != Some("object") {
        obj.insert("type".into(), json!("object"));
    }
    match obj.remove("properties") {
        Some(Value::Object(props)) => {
            let normalized = props
                .into_iter()
                .map(|(k, v)| (k, normalize_property_schema(v)))
                .collect();
            obj.insert("properties".into(), Value::Object(normalized));
        }
        _ => {
            obj.insert("properties".into(), json!({}));
        }
    }
    let required = match obj.remove("required") {
        Some(Value::Array(items)) => Value::Array(
            items
                .into_iter()
                .filter_map(|v| v.as_str().map(|s| Value::String(s.to_string())))
                .collect(),
        ),
        _ => Value::Array(Vec::new()),
    };
    obj.insert("required".into(), required);
    match obj.get("additionalProperties") {
        Some(Value::Bool(_)) | Some(Value::Object(_)) => {}
        _ => {
            obj.insert("additionalProperties".into(), json!(true));
        }
    }
    Value::Object(obj)
}

/// 顶层 oneOf / anyOf / allOf 会让 Kiro 回 `TOOL_SCHEMA_INVALID`。没有 properties 时，
/// 从第一个 object 分支里把字段救回来。
fn strip_top_level_combinators(obj: &mut serde_json::Map<String, Value>) {
    let had_properties = obj.contains_key("properties");
    for combinator in ["oneOf", "anyOf", "allOf"] {
        let Some(Value::Array(variants)) = obj.remove(combinator) else {
            continue;
        };
        if had_properties || obj.contains_key("properties") {
            continue;
        }
        for variant in variants {
            let Value::Object(map) = variant else {
                continue;
            };
            if map.get("type").and_then(Value::as_str) != Some("object") {
                continue;
            }
            for key in [
                "properties",
                "required",
                "additionalProperties",
                "description",
            ] {
                if let Some(val) = map.get(key) {
                    obj.entry(key.to_string()).or_insert_with(|| val.clone());
                }
            }
            break;
        }
    }
}

fn normalize_property_schema(schema: Value) -> Value {
    let Value::Object(mut obj) = schema else {
        return schema;
    };
    obj.remove("$schema");
    if obj
        .get("exclusiveMinimum")
        .and_then(Value::as_f64)
        .is_some()
    {
        obj.remove("exclusiveMinimum");
    }
    if obj
        .get("exclusiveMaximum")
        .and_then(Value::as_f64)
        .is_some()
    {
        obj.remove("exclusiveMaximum");
    }
    for key in ["maximum", "minimum"] {
        if let Some(v) = obj.get(key).and_then(Value::as_f64) {
            if !(-2_147_483_648.0..=2_147_483_647.0).contains(&v) {
                obj.remove(key);
            }
        }
    }
    if let Some(Value::Object(props)) = obj.remove("properties") {
        let normalized = props
            .into_iter()
            .map(|(k, v)| (k, normalize_property_schema(v)))
            .collect();
        obj.insert("properties".into(), Value::Object(normalized));
    }
    if let Some(items) = obj.remove("items") {
        obj.insert("items".into(), normalize_property_schema(items));
    }
    Value::Object(obj)
}

fn default_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "required": [],
        "additionalProperties": true
    })
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

fn injected_system(system: &str, request: &ChatRequest, model: &str) -> String {
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
    match thinking_dir(request) {
        Think::Off => text,
        Think::Adaptive { effort } => {
            let effort = normalize_effort(model, &effort);
            format!(
                "<thinking_mode>adaptive</thinking_mode>\n<thinking_effort>{effort}</thinking_effort>\n\n{text}"
            )
        }
        Think::Enabled { budget } => format!(
            "<thinking_mode>enabled</thinking_mode>\n<max_thinking_length>{budget}</max_thinking_length>\n\n{text}"
        ),
    }
}

/// 估算静态前缀（系统提示词 + 工具声明）包含的 Token 数，用于 Prompt Cache 遥测补偿
fn estimate_cacheable_prefix(request: &ChatRequest) -> u32 {
    let mut total = 0u32;
    for m in &request.messages {
        if m.role == Role::System {
            total = total.saturating_add(estimate_tokens(&m.text));
        }
    }
    for t in &request.tools {
        total = total.saturating_add(estimate_tokens(&t.name));
        total = total.saturating_add(estimate_tokens(&t.description));
        let params = serde_json::to_string(&t.parameters).unwrap_or_default();
        total = total.saturating_add(estimate_tokens(&params));
    }
    total
}

/// `tool_choice` 的替代说法。Kiro 没有这个字段，只能用提示词转达。
///
/// 两条实测教训：
/// 1. 这句得挂在**当前这条 user 消息**上。放进开头的 system 轮时，它在 4800 token 上下文的
///    最前面，模型照常回文本、不调工具；贴在当轮末尾才拦得住。也正因如此它不进 history——
///    下一轮客户端的 `tool_choice` 可能就变了。
/// 2. 不能写成 `[INSTRUCTION: …]` 这种方括号指令。Amazon Q 服务端自己的系统提示会把它当成
///    打探隐藏指令，直接回一句「I can't discuss that.」。用普通祈使句。
fn tool_choice_hint(request: &ChatRequest, names: &mut HashMap<String, String>) -> String {
    match &request.tool_choice {
        ToolChoice::None => {
            "Answer in plain text for this turn. Do not call any tool.".to_string()
        }
        ToolChoice::Required => "For this turn, answer by calling one of the available tools rather than by writing a reply. Do not ask a follow-up question. If an argument is not given above, choose the most plausible value and call the tool with it.".to_string(),
        ToolChoice::Tool(name) => {
            let wired = wire_tool_name(name, names);
            format!("For this turn, answer by calling the `{wired}` tool rather than by writing a reply. Do not call any other tool and do not ask a follow-up question. If an argument is not given above, choose the most plausible value and call it with that.")
        }
        ToolChoice::Auto => String::new(),
    }
}

/// 把 `tool_choice` 那句话追加到当前 user 消息的正文末尾。
fn append_to_current(current: &mut Value, line: &str) {
    if line.is_empty() {
        return;
    }
    let Some(content) = current
        .pointer("/userInputMessage/content")
        .and_then(Value::as_str)
        .map(str::to_string)
    else {
        return;
    };
    let joined = if content.trim().is_empty() {
        line.to_string()
    } else {
        format!("{content}\n\n{line}")
    };
    current["userInputMessage"]["content"] = json!(joined);
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
/// 显式 `thinking.type=disabled` 优先，避免模型名把已经关掉的思考又打开。
fn thinking_dir(request: &ChatRequest) -> Think {
    if thinking_disabled(request) {
        return Think::Off;
    }
    if let Some(raw) = request.raw_inbound.as_ref() {
        let body = raw.body.as_ref();
        match body
            .pointer("/thinking/type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "adaptive" => {
                let effort = explicit_effort(request).unwrap_or_else(|| "high".into());
                return Think::Adaptive { effort };
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
        if let Some(effort) = openai_effort(request) {
            return Think::Enabled {
                budget: effort_budget(&effort),
            };
        }
    }
    let model = request.model.to_ascii_lowercase();
    if model.contains("thinking") {
        if model.contains("opus-4.6") || model.contains("opus-4-6") {
            return Think::Adaptive {
                effort: "high".into(),
            };
        }
        return Think::Enabled { budget: 20_000 };
    }
    Think::Off
}

fn thinking_disabled(request: &ChatRequest) -> bool {
    request.raw_inbound.as_ref().is_some_and(|raw| {
        let type_disabled = raw
            .body
            .pointer("/thinking/type")
            .and_then(Value::as_str)
            .is_some_and(|t| t.eq_ignore_ascii_case("disabled"));
        let effort_disabled = explicit_effort(request).as_deref().is_some_and(|effort| {
            matches!(
                effort.to_ascii_lowercase().as_str(),
                "none" | "off" | "disabled"
            )
        });
        type_disabled || effort_disabled
    })
}

/// 三种入站方言放 reasoning 档位的位置不同；保留调用方原值，不能把 high/xhigh
/// 先压成固定 16k budget、再误映射成 medium。
fn explicit_effort(request: &ChatRequest) -> Option<String> {
    request.raw_inbound.as_ref().and_then(|raw| {
        [
            raw.body.pointer("/output_config/effort"),
            raw.body.pointer("/reasoning/effort"),
            raw.body.get("reasoning_effort"),
        ]
        .into_iter()
        .flatten()
        .find_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    })
}

/// OpenAI 两种方言的显式档位本身就代表「打开 reasoning」；Anthropic 的
/// `output_config.effort` 只有配合 `thinking.type=adaptive` 才打开。
fn openai_effort(request: &ChatRequest) -> Option<String> {
    request.raw_inbound.as_ref().and_then(|raw| {
        [
            raw.body.pointer("/reasoning/effort"),
            raw.body.get("reasoning_effort"),
        ]
        .into_iter()
        .flatten()
        .find_map(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
    })
}

fn effort_budget(effort: &str) -> u32 {
    match effort.trim().to_ascii_lowercase().as_str() {
        "low" => 4_000,
        "medium" => 16_000,
        "high" => 64_000,
        "xhigh" | "max" => 100_000,
        _ => 16_000,
    }
}

/// 只有上游确认接受的型号才带 `additionalModelRequestFields`。
/// Opus 4.6 只在 adaptive 下收；Sonnet 4.5 一带就 400。内层键保持蛇形 `output_config`。
fn additional_model_fields(upstream: &str, request: &ChatRequest) -> Option<Value> {
    if thinking_disabled(request) || !model_supports_native_reasoning(upstream) {
        return None;
    }
    let think = thinking_dir(request);
    // Opus 4.6 只在 adaptive 下收这个字段，普通 enabled 或单独的 effort 都会 400。
    if upstream.eq_ignore_ascii_case("claude-opus-4.6") && !matches!(think, Think::Adaptive { .. })
    {
        return None;
    }
    let explicit = explicit_effort(request);
    let raw_effort = if let Some(effort) = explicit {
        effort
    } else {
        match &think {
            Think::Adaptive { effort } => effort.clone(),
            Think::Enabled { budget } => effort_from_budget(*budget).to_string(),
            Think::Off => return None,
        }
    };
    let effort = normalize_effort(upstream, &raw_effort);
    if model_uses_gpt_reasoning(upstream) {
        Some(json!({ "reasoning": { "effort": effort } }))
    } else {
        Some(json!({ "output_config": { "effort": effort } }))
    }
}

fn model_uses_gpt_reasoning(model: &str) -> bool {
    matches!(
        model.to_ascii_lowercase().as_str(),
        "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna"
    )
}

fn model_supports_native_reasoning(model: &str) -> bool {
    if model_uses_gpt_reasoning(model) {
        return true;
    }
    let m = model.to_ascii_lowercase();
    matches!(
        m.as_str(),
        "claude-opus-4.6" | "claude-opus-4.7" | "claude-opus-4.8" | "claude-sonnet-4.6"
    ) || m.contains("sonnet-5")
        || m.contains("opus-5")
        || m.contains("claude-5")
}

fn model_supports_xhigh(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    if m.contains("opus-4.7") || m.contains("opus-4.8") || m.contains("claude-5") {
        return true;
    }
    // 已知不收 xhigh 的老型号降到 high。其余（含 GPT 和还没进表的新型号）原样保留。
    !matches!(
        m.as_str(),
        "claude-opus-4.6"
            | "claude-sonnet-4.6"
            | "claude-opus-4.5"
            | "claude-sonnet-4.5"
            | "claude-haiku-4.5"
    )
}

fn effort_from_budget(tokens: u32) -> &'static str {
    match tokens {
        0..=4_000 => "low",
        4_001..=16_000 => "medium",
        16_001..=64_000 => "high",
        _ => "xhigh",
    }
}

fn normalize_effort(model: &str, raw: &str) -> String {
    let tier = match raw.trim().to_ascii_lowercase().as_str() {
        "none" => "none",
        "low" => "low",
        "medium" => "medium",
        "high" => "high",
        "xhigh" | "x-high" | "x_high" => "xhigh",
        "max" => "max",
        _ => "high",
    };
    if tier == "none" && !model_uses_gpt_reasoning(model) {
        return "high".into();
    }
    if tier == "xhigh" && !model_supports_xhigh(model) {
        return "high".into();
    }
    tier.to_string()
}

/// `inferenceConfig`。
///
/// 注意 `maxTokens` **上游收下但不生效**（2026-10 实测：要 24 个 token，上游照样出了 373 个，
/// 停止原因还是正常结束）。照旧发它——哪天上游认了就自动生效——但别指望靠它截断，也别拿
/// `finish_reason` 当长度判据。本地不去截：这个仓库里所有通道都是「透传给上游、映射上游的
/// 停止原因」，用粗估的 token 数去砍真实回答，砍错的代价比省下的额度大。
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
    (!cfg.is_empty()).then_some(Value::Object(cfg))
}

fn map_status(status: u16, text: &str) -> UpstreamError {
    classify_kiro(status, text)
}

/// 先看正文里的 reason，再看 HTTP 状态。额度、封号、token 失效和协议错误处置完全不同。
fn classify_kiro(status: u16, text: &str) -> UpstreamError {
    let head: String = text.chars().take(400).collect();
    if is_quota_exhausted(text) {
        return UpstreamError::new(UpstreamKind::Quota, 402, format!("Kiro 额度用尽：{head}"));
    }
    if text.contains("The bearer token included in the request is invalid") {
        return UpstreamError::new(UpstreamKind::Auth, 401, format!("Kiro 登录失效：{head}"));
    }
    if is_account_suspended(text) {
        return UpstreamError::new(
            UpstreamKind::Forbidden,
            403,
            format!("Kiro 账号已停用：{head}"),
        );
    }
    if is_account_throttled(text) {
        return UpstreamError::new(
            UpstreamKind::RateLimit,
            429,
            format!("Kiro 账号被临时限制：{head}"),
        );
    }
    if is_client_validation(text) {
        return UpstreamError::new(
            UpstreamKind::BadRequest,
            400,
            format!("Kiro 拒绝了这轮请求：{head}"),
        );
    }
    if status == 524 || is_gateway_timeout(text) {
        return UpstreamError::new(
            UpstreamKind::Provider,
            504,
            format!("Kiro 网关超时：{head}"),
        );
    }
    let kind = match status {
        401 => UpstreamKind::Auth,
        403 => UpstreamKind::Forbidden,
        429 => UpstreamKind::RateLimit,
        400 => UpstreamKind::BadRequest,
        _ => UpstreamKind::Upstream,
    };
    let status = if kind == UpstreamKind::Upstream && status == 0 {
        502
    } else if status == 0 {
        match kind {
            UpstreamKind::Auth => 401,
            UpstreamKind::Forbidden => 403,
            UpstreamKind::RateLimit => 429,
            UpstreamKind::BadRequest => 400,
            _ => 502,
        }
    } else {
        status
    };
    UpstreamError::new(kind, status, format!("Kiro {status}：{head}"))
}

fn is_quota_exhausted(body: &str) -> bool {
    const REASONS: &[&str] = &["MONTHLY_REQUEST_COUNT", "OVERAGE_REQUEST_LIMIT_EXCEEDED"];
    if !REASONS.iter().any(|r| body.contains(r)) {
        return false;
    }
    if let Ok(value) = serde_json::from_str::<Value>(body) {
        let top = value.get("reason").and_then(Value::as_str);
        let nested = value.pointer("/error/reason").and_then(Value::as_str);
        return [top, nested]
            .into_iter()
            .flatten()
            .any(|r| REASONS.contains(&r));
    }
    true
}

fn is_account_suspended(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    lower.contains("suspended") && lower.contains("locked your account")
}

fn is_account_throttled(body: &str) -> bool {
    body.contains("suspicious activity") && body.contains("temporary limits")
}

fn is_gateway_timeout(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    body.contains("524")
        && (lower.contains("status code")
            || lower.contains("gateway timeout")
            || lower.contains("server-side issue"))
}

fn is_client_validation(body: &str) -> bool {
    const REASONS: &[&str] = &["TOOL_USE_RESULT_MISMATCH", "TOOL_SCHEMA_INVALID"];
    if REASONS.iter().any(|r| body.contains(r)) {
        if let Ok(value) = serde_json::from_str::<Value>(body) {
            let top = value.get("reason").and_then(Value::as_str);
            let nested = value.pointer("/error/reason").and_then(Value::as_str);
            if [top, nested]
                .into_iter()
                .flatten()
                .any(|r| REASONS.contains(&r))
            {
                return true;
            }
        } else {
            return true;
        }
    }
    body.contains("Expected toolResult blocks")
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
    saw_token_usage: bool,
    context_input: Option<u32>,
    stop: Option<String>,
    invoke_fence: bool,
    invoke_fence_partial: String,
    known_tools: HashSet<String>,
    tool_policy_active: bool,
    tool_requirement: ToolRequirement,
    tool_violation: Option<String>,
    model: String,
    thinking_signature: Option<String>,
    redacted: Option<String>,
    stray_line: String,
    stray_run: u32,
    stray_tripped: bool,
    cache_prefix_tokens: u32,
    is_multi_turn: bool,
}

/// 响应体是 JSON 还是 eventstream 帧。帧以 4 字节大端总长开头（首字节恒为 0），
/// JSON 以 `{` / `[` 开头，两者不会撞。
fn looks_like_json_body(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .is_some_and(|b| *b == b'{' || *b == b'[')
}

/// `prefix` 是 `run` 为了嗅探已经读掉的第一截，要接回缓冲区的头上。
#[allow(clippy::too_many_arguments)]
async fn read_eventstream(
    mut res: reqwest::Response,
    prefix: Vec<u8>,
    started: Instant,
    idle: Duration,
    names: HashMap<String, String>,
    tool_policy: ToolPolicy,
    model: String,
    thinking_on: bool,
    cache_prefix_tokens: u32,
    is_multi_turn: bool,
    on_delta: DeltaSink<'_>,
) -> Result<Completion, UpstreamError> {
    let mut buf = prefix;
    let mut c = Collected {
        names,
        known_tools: tool_policy.allowed,
        tool_policy_active: true,
        tool_requirement: tool_policy.requirement,
        model,
        thinking_on,
        cache_prefix_tokens,
        is_multi_turn,
        ..Collected::default()
    };
    let mut bad_frames = 0u32;
    loop {
        // 先把手上的字节榨干再去拉下一截：整个响应落在 `prefix` 里时（短回答常常如此）
        // 后面那次 `chunk()` 直接回 None，先拉再解就会把它整段漏掉。
        loop {
            match eventstream::next_frame(&buf) {
                eventstream::FrameStep::NeedMore => break,
                eventstream::FrameStep::Ready {
                    consumed,
                    event_type,
                    payload,
                } => {
                    bad_frames = 0;
                    buf.drain(..consumed);
                    handle_event(
                        &Event {
                            event_type,
                            payload,
                        },
                        &mut c,
                        started,
                        on_delta,
                    );
                }
                eventstream::FrameStep::Skip(n) => {
                    bad_frames += 1;
                    if bad_frames >= 5 {
                        return Err(UpstreamError::new(
                            UpstreamKind::Upstream,
                            502,
                            "Kiro 流帧连续校验失败",
                        ));
                    }
                    let n = n.min(buf.len()).max(1);
                    buf.drain(..n);
                }
            }
        }
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
    }
    if let Some(msg) = c.error.take() {
        return Err(classify_kiro(502, &msg));
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
    enforce_tool_policy(&c)?;
    Ok(finish(c, started))
}

fn finish(c: Collected, started: Instant) -> Completion {
    let mut out = done(c.text, started);
    out.routed_model = Some(c.model);
    out.thinking = c.thinking;
    out.ttft_ms = c.ttft;
    if c.saw_token_usage {
        if let Some(mut usage) = c.usage {
            if usage.cache_read_tokens == 0 && usage.cache_write_tokens == 0 && c.cache_prefix_tokens > 0 {
                let prefix = c.cache_prefix_tokens.min(usage.input_tokens.saturating_sub(1));
                if prefix > 0 {
                    if c.is_multi_turn {
                        usage.cache_read_tokens = prefix;
                        usage.input_tokens = usage.input_tokens.saturating_sub(prefix);
                        usage.input_basis = InputBasis::ExcludesCache;
                    } else {
                        usage.cache_write_tokens = prefix;
                        usage.input_tokens = usage.input_tokens.saturating_sub(prefix);
                        usage.input_basis = InputBasis::ExcludesCache;
                    }
                }
            }
            out.usage = usage;
            out.usage_measured = true;
        }
    } else if let Some(input) = c.context_input {
        let mut usage = Usage {
            input_tokens: input,
            ..out.usage
        };
        if c.cache_prefix_tokens > 0 {
            let prefix = c.cache_prefix_tokens.min(input.saturating_sub(1));
            if prefix > 0 {
                if c.is_multi_turn {
                    usage.cache_read_tokens = prefix;
                    usage.input_tokens = input.saturating_sub(prefix);
                    usage.input_basis = InputBasis::ExcludesCache;
                } else {
                    usage.cache_write_tokens = prefix;
                    usage.input_tokens = input.saturating_sub(prefix);
                    usage.input_basis = InputBasis::ExcludesCache;
                }
            }
        }
        out.usage = usage;
        out.usage_measured = true;
    }
    out.thinking_signature = c.thinking_signature;
    out.redacted_thinking = c.redacted;
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
    if (c.thinking_on || !out.thinking.is_empty())
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
        "max_tokens" | "length" | "max_output_tokens" | "model_context_window_exceeded" => {
            FinishReason::Length
        }
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
    if c.tool_policy_active
        && !c
            .known_tools
            .contains(&call.name.trim().to_ascii_lowercase())
    {
        c.tool_violation.get_or_insert_with(|| {
            format!("上游返回了本轮 tool_choice 不允许的工具调用：{}", call.name)
        });
        return;
    }
    let key = tool_content_key(&call.name, &call.arguments);
    if key.is_empty() || !c.seen_tools.insert(key) {
        return;
    }
    c.tool_calls.push(call);
}

fn enforce_tool_policy(c: &Collected) -> Result<(), UpstreamError> {
    if let Some(message) = &c.tool_violation {
        return Err(UpstreamError::new(
            UpstreamKind::Upstream,
            502,
            format!("Kiro 没遵守工具约束，已拦截：{message}"),
        ));
    }
    let satisfied = match &c.tool_requirement {
        ToolRequirement::None => true,
        ToolRequirement::Any => !c.tool_calls.is_empty(),
        ToolRequirement::Named(name) => c
            .tool_calls
            .iter()
            .any(|call| call.name.eq_ignore_ascii_case(name)),
    };
    if satisfied {
        return Ok(());
    }
    let expected = match &c.tool_requirement {
        ToolRequirement::Any => "至少一个已声明工具".to_string(),
        ToolRequirement::Named(name) => format!("指定工具 {name}"),
        ToolRequirement::None => unreachable!("None 已在 satisfied 分支返回"),
    };
    Err(UpstreamError::new(
        UpstreamKind::Upstream,
        502,
        format!("Kiro 没遵守 tool_choice：这一轮必须调用{expected}，但只返回了文本。"),
    ))
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
        c.saw_token_usage = true;
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
            if let Some(signature) = body
                .get("signature")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                c.thinking_signature = Some(signature.to_string());
                on_delta(Delta::ThinkingSignature(signature.to_string()));
            }
            if let Some(piece) = body
                .get("text")
                .or_else(|| body.get("content"))
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                if c.thinking_on {
                    emit_thinking(c, piece, started, on_delta);
                } else {
                    emit_text(c, piece, started, on_delta);
                }
            }
            if let Some(redacted) = body
                .get("redactedContent")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
            {
                c.redacted = Some(redacted.to_string());
                on_delta(Delta::RedactedThinking(redacted.to_string()));
            }
        }
        "contextUsageEvent" => {
            if let Some(pct) = body.get("contextUsagePercentage").and_then(Value::as_f64) {
                let window = protocol::context_window(&c.model);
                let tokens = (pct * f64::from(window) / 100.0).round().max(0.0) as u32;
                c.context_input = Some(tokens);
                if pct >= 100.0 && c.stop.is_none() {
                    c.stop = Some("model_context_window_exceeded".into());
                }
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

#[cfg(test)]
fn next_event(buf: &[u8]) -> Option<(usize, Event)> {
    match eventstream::next_frame(buf) {
        eventstream::FrameStep::Ready {
            consumed,
            event_type,
            payload,
        } => Some((
            consumed,
            Event {
                event_type,
                payload,
            },
        )),
        _ => None,
    }
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
        any = true;
    }
    if let Some(n) = json_u32(
        token
            .get("cacheWriteInputTokens")
            .or_else(|| token.get("cacheWriteTokens")),
    ) {
        usage.cache_write_tokens = n;
        any = true;
    }
    any.then_some(usage)
}

const THINK_OPEN: &str = "<thinking>";
const THINK_CLOSE: &str = "</thinking>";

fn feed_assistant_text(c: &mut Collected, piece: &str, started: Instant, on_delta: DeltaSink<'_>) {
    if !c.thinking_on && !c.in_thinking && !piece.contains(THINK_OPEN) && !c.hold.contains(THINK_OPEN) {
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
        if buf.is_char_boundary(start) && bytes[start..] == tag_b[..n] {
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
    if piece.is_empty() || c.stray_tripped {
        return;
    }
    let kept = take_until_stray_flood(c, piece);
    if kept.is_empty() {
        return;
    }
    invoke::advance_fence(&mut c.invoke_fence, &mut c.invoke_fence_partial, &kept);
    if c.ttft.is_none() {
        c.ttft = Some(started.elapsed().as_millis() as u64);
    }
    c.text.push_str(&kept);
    on_delta(Delta::Text(kept));
}

const STRAY_TRIP: u32 = 32;

/// 同一个引导词独占一行连续出现几十次，是长上下文退化。从超阈值的那一行起丢掉。
fn take_until_stray_flood(c: &mut Collected, piece: &str) -> String {
    if c.stray_tripped {
        return String::new();
    }
    let mut kept = String::new();
    for segment in piece.split_inclusive('\n') {
        let line = segment.trim();
        if invoke::STRAY_TOKENS.contains(&line) {
            if line == c.stray_line {
                c.stray_run += 1;
            } else {
                c.stray_line = line.to_string();
                c.stray_run = 1;
            }
            if c.stray_run >= STRAY_TRIP {
                c.stray_tripped = true;
                return kept;
            }
            kept.push_str(segment);
        } else {
            if !line.is_empty() {
                c.stray_line = line.to_string();
                c.stray_run = 0;
            }
            kept.push_str(segment);
        }
    }
    kept
}

const CALLED: &str = "[Called ";
const ARGS_MARK: &str = " with args:";

/// 把正文里写完整的 `[Called name with args: {…}]` 拆成工具调用。
/// 没写完的留在 `tool_hold`；流结束时丢掉，不把这句标记漏给客户端。
fn drain_embedded(c: &mut Collected, eof: bool, started: Instant, on_delta: DeltaSink<'_>) {
    if c.stray_tripped {
        c.tool_hold.clear();
        return;
    }
    loop {
        if c.stray_tripped {
            c.tool_hold.clear();
            return;
        }
        let invoke_at = invoke::find_start(&c.tool_hold);
        let called_at = c.tool_hold.find(CALLED);
        let invoke_first = match (invoke_at, called_at) {
            (Some(inv), Some(called)) => inv <= called,
            (Some(_), None) => true,
            _ => false,
        };
        if invoke_first {
            if drain_invoke_at(c, invoke_at.expect("invoke_first"), eof, started, on_delta) {
                continue;
            }
            return;
        }
        if let Some(idx) = called_at {
            if drain_called_at(c, idx, eof, started, on_delta) {
                continue;
            }
            return;
        }
        if eof {
            let rest = std::mem::take(&mut c.tool_hold);
            push_text(c, &rest, started, on_delta);
            return;
        }
        let keep = invoke::partial_open_suffix_len(&c.tool_hold)
            .max(partial_tag_len(&c.tool_hold, CALLED));
        let emit_len = floor_boundary(&c.tool_hold, c.tool_hold.len().saturating_sub(keep));
        if emit_len == 0 {
            return;
        }
        let text = c.tool_hold[..emit_len].to_string();
        c.tool_hold = c.tool_hold[emit_len..].to_string();
        push_text(c, &text, started, on_delta);
        return;
    }
}

/// 处理 `tool_hold[start..]` 上的一个 `<invoke>`。返回 true 表示缓冲区还要再扫。
fn drain_invoke_at(
    c: &mut Collected,
    start: usize,
    eof: bool,
    started: Instant,
    on_delta: DeltaSink<'_>,
) -> bool {
    let original_prefix = c.tool_hold[..start].to_string();
    let before = invoke::strip_stray_tail(&original_prefix).to_string();
    let fenced = invoke::fence_open_after(c.invoke_fence, &c.invoke_fence_partial, &before);
    let leak = invoke::looks_like_real_leak(&before);
    let Some(end) = invoke::block_end(&c.tool_hold, start).filter(|end| *end > start) else {
        if !leak || fenced {
            let rest = std::mem::take(&mut c.tool_hold);
            push_text(c, &rest, started, on_delta);
            return false;
        }
        if start > 0 {
            c.tool_hold = c.tool_hold[start..].to_string();
            push_text(c, &original_prefix, started, on_delta);
        }
        if eof || c.tool_hold.len() > invoke::MAX_HOLD {
            let rest = std::mem::take(&mut c.tool_hold);
            push_text(c, &rest, started, on_delta);
        }
        return false;
    };
    let block = c.tool_hold[start..end].to_string();
    let parsed = invoke::parse_block(&block);
    let known = parsed
        .as_ref()
        .is_some_and(|(name, _)| c.known_tools.contains(&name.to_ascii_lowercase()));
    c.tool_hold = c.tool_hold[end..].to_string();
    if leak && !fenced && known {
        push_text(c, &before, started, on_delta);
        let (name, input) = parsed.expect("known implies parsed");
        if let Some(call) = embedded_tool(&c.names, &name, &input) {
            accept_tool(c, call);
        } else {
            push_text(c, &block, started, on_delta);
        }
    } else {
        let mut raw = original_prefix;
        raw.push_str(&block);
        push_text(c, &raw, started, on_delta);
    }
    true
}

/// 处理 `[Called name with args: {…}]`。返回 true 表示吃掉了一整段，缓冲区还要再扫。
fn drain_called_at(
    c: &mut Collected,
    idx: usize,
    eof: bool,
    started: Instant,
    on_delta: DeltaSink<'_>,
) -> bool {
    if let Some((name, raw, end)) =
        parse_embedded_at(&c.tool_hold, idx).filter(|(_, _, end)| *end > idx)
    {
        let before = c.tool_hold[..idx].to_string();
        c.tool_hold = c.tool_hold[end..].to_string();
        push_text(c, &before, started, on_delta);
        if let Some(call) = embedded_tool(&c.names, &name, &raw) {
            accept_tool(c, call);
        }
        return true;
    }
    let before = c.tool_hold[..idx].to_string();
    c.tool_hold = c.tool_hold[idx..].to_string();
    push_text(c, &before, started, on_delta);
    if eof {
        c.tool_hold.clear();
    }
    false
}

fn floor_boundary(text: &str, mut pos: usize) -> usize {
    while pos > 0 && !text.is_char_boundary(pos) {
        pos -= 1;
    }
    pos
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
        routed_model: None,
        ttft_ms: None,
        turn_ms: started.elapsed().as_millis() as u64,
        raw_response: None,
        thinking_signature: None,
        redacted_thinking: None,
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

    fn frame(event_type: Option<&str>, payload: &[u8]) -> Vec<u8> {
        eventstream::encode_frame(event_type, payload)
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
            ..Default::default()
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
    fn web_search_uses_kiro_description_and_preserves_client_tools() {
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
        assert_eq!(
            tools[1]["toolSpecification"]["description"]
                .as_str()
                .unwrap(),
            "write a file"
        );
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
            ..Default::default()
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
                    "$schema": "https://json-schema.org/draft/2020-12/schema",
                    "oneOf": [{
                        "type": "object",
                        "properties": {
                            "child": {
                                "properties": { "n": { "type": "integer", "exclusiveMinimum": 0 } },
                                "exclusiveMinimum": 1
                            },
                            "tags": {
                                "type": "array",
                                "items": { "properties": { "id": { "type": "string" } } }
                            }
                        }
                    }]
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
        let schema = &tools[0]["toolSpecification"]["inputSchema"]["json"];
        assert!(schema.get("oneOf").is_none(), "顶层 oneOf 要剥掉");
        assert_eq!(schema["type"], "object");
        let child = &schema["properties"]["child"];
        assert_eq!(child["properties"]["n"]["type"], "integer");
        assert!(child.get("exclusiveMinimum").is_none());
        assert!(child["properties"]["n"].get("exclusiveMinimum").is_none());
        assert!(child.get("$schema").is_none());
        let item =
            &tools[0]["toolSpecification"]["inputSchema"]["json"]["properties"]["tags"]["items"];
        assert_eq!(item["properties"]["id"]["type"], "string");
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

    #[test]
    fn line_start_invoke_of_a_known_tool_becomes_a_call() {
        let started = Instant::now();
        let mut c = Collected {
            known_tools: HashSet::from(["read_file".into()]),
            ..Collected::default()
        };
        let mut sink = |_d: Delta| {};
        feed_assistant_text(
            &mut c,
            "note\ncall\n<antml:invoke name=\"read_file\"><parameter name=\"path\">a.rs</parameter></antml:invoke> tail",
            started,
            &mut sink,
        );
        flush_hold(&mut c, started, &mut sink, true);
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].name, "read_file");
        assert!(c.tool_calls[0].arguments.contains("a.rs"));
        assert!(c.text.contains("note"));
        assert!(c.text.contains("tail"));
        assert!(!c.text.contains("invoke"));
        assert!(!c.text.contains("\ncall"));
    }

    #[test]
    fn mid_sentence_and_fenced_invoke_stay_text() {
        let started = Instant::now();
        let mut c = Collected {
            known_tools: HashSet::from(["read_file".into()]),
            ..Collected::default()
        };
        let mut sink = |_d: Delta| {};
        let block = "<invoke name=\"read_file\"><parameter name=\"path\">a.rs</parameter></invoke>";
        feed_assistant_text(
            &mut c,
            &format!("see {block} inline\n```\n{block}\n```\n"),
            started,
            &mut sink,
        );
        flush_hold(&mut c, started, &mut sink, true);
        assert!(c.tool_calls.is_empty());
        assert_eq!(c.text.matches("invoke").count(), 4);
    }

    #[test]
    fn tool_choice_none_never_recovers_an_invoke_tag() {
        let request = ChatRequest {
            model: "kiro-claude-sonnet-4.6".into(),
            messages: vec![Message::text(Role::User, "describe the file")],
            tools: vec![crate::normalized::ToolDef {
                name: "read_file".into(),
                ..Default::default()
            }],
            tool_choice: ToolChoice::None,
            ..Default::default()
        };
        let (_, names) = build_body(&request, "claude-sonnet-4.6", None);
        let policy = tool_policy(&request, &names);
        assert!(policy.allowed.is_empty());
        let mut c = Collected {
            names,
            known_tools: policy.allowed,
            tool_policy_active: true,
            tool_requirement: policy.requirement,
            ..Collected::default()
        };
        let block = "<invoke name=\"read_file\"><parameter name=\"path\">a.rs</parameter></invoke>";
        feed_assistant_text(&mut c, block, Instant::now(), &mut |_| {});
        flush_hold(&mut c, Instant::now(), &mut |_| {}, true);
        assert!(c.tool_calls.is_empty());
        assert!(c.text.contains("invoke"), "不允许的标签只能当正文");
        enforce_tool_policy(&c).unwrap();
    }

    #[test]
    fn a_named_tool_choice_rejects_a_different_structured_call() {
        let request = ChatRequest {
            model: "kiro-claude-sonnet-4.6".into(),
            messages: vec![Message::text(Role::User, "use read")],
            tools: ["read_file", "write_file"]
                .into_iter()
                .map(|name| crate::normalized::ToolDef {
                    name: name.into(),
                    ..Default::default()
                })
                .collect(),
            tool_choice: ToolChoice::Tool("read_file".into()),
            ..Default::default()
        };
        let (_, names) = build_body(&request, "claude-sonnet-4.6", None);
        let policy = tool_policy(&request, &names);
        let mut c = Collected {
            names,
            known_tools: policy.allowed,
            tool_policy_active: true,
            tool_requirement: policy.requirement,
            ..Collected::default()
        };
        accept_tool(
            &mut c,
            ToolCall {
                id: "call-1".into(),
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        );
        assert!(c.tool_calls.is_empty());
        let err = enforce_tool_policy(&c).unwrap_err();
        assert_eq!(err.status, 502);
        assert!(!err.kind.blames_account());
    }

    #[test]
    fn required_tool_choice_fails_closed_when_the_model_only_writes_text() {
        let c = Collected {
            tool_policy_active: true,
            tool_requirement: ToolRequirement::Any,
            text: "plain text".into(),
            ..Collected::default()
        };
        let err = enforce_tool_policy(&c).unwrap_err();
        assert_eq!(err.status, 502);
        assert!(err.message.contains("必须调用"));
    }

    fn request_with(model: &str, body: Value) -> ChatRequest {
        use crate::inbound::Dialect;
        use crate::normalized::RawInbound;
        ChatRequest {
            model: model.into(),
            messages: vec![Message::text(Role::User, "hi")],
            raw_inbound: Some(RawInbound {
                dialect: Dialect::AnthropicMessages,
                body: Arc::new(body),
                stream: true,
                headers: Vec::new(),
            }),
            ..Default::default()
        }
    }

    #[test]
    fn reasoning_fields_follow_what_each_model_accepts() {
        let opus46 = request_with(
            "kiro-claude-opus-4.6",
            json!({ "thinking": { "type": "enabled", "budget_tokens": 20000 }, "output_config": { "effort": "xhigh" } }),
        );
        let (body, _) = build_body(&opus46, "claude-opus-4.6", None);
        assert!(body.get("additionalModelRequestFields").is_none());

        let adaptive = request_with(
            "kiro-claude-opus-4.6",
            json!({ "thinking": { "type": "adaptive" }, "output_config": { "effort": "xhigh" } }),
        );
        let (body, _) = build_body(&adaptive, "claude-opus-4.6", None);
        assert_eq!(
            body["additionalModelRequestFields"]["output_config"]["effort"],
            "high"
        );
        let system = body["conversationState"]["history"][0]["userInputMessage"]["content"]
            .as_str()
            .unwrap();
        assert!(system.contains("<thinking_effort>high</thinking_effort>"));

        let sonnet = request_with(
            "kiro-claude-sonnet-4.5",
            json!({ "thinking": { "type": "enabled", "budget_tokens": 8000 } }),
        );
        let (body, _) = build_body(&sonnet, "claude-sonnet-4.5", None);
        assert!(body.get("additionalModelRequestFields").is_none());

        let opus47 = request_with(
            "kiro-claude-opus-4.7",
            json!({ "output_config": { "effort": "xhigh" } }),
        );
        let (body, _) = build_body(&opus47, "claude-opus-4.7", None);
        assert_eq!(
            body["additionalModelRequestFields"]["output_config"]["effort"],
            "xhigh"
        );
        let system = body["conversationState"]["history"][0]["userInputMessage"]["content"]
            .as_str()
            .unwrap();
        assert!(!system.contains("<thinking_mode>"));

        let gpt = request_with(
            "kiro-gpt-5.6-sol",
            json!({ "output_config": { "effort": "medium" } }),
        );
        let (body, _) = build_body(&gpt, "gpt-5.6-sol", None);
        assert_eq!(
            body["additionalModelRequestFields"]["reasoning"]["effort"],
            "medium"
        );

        let openai_chat = request_with("kiro-gpt-5.6-sol", json!({ "reasoning_effort": "high" }));
        let (body, _) = build_body(&openai_chat, "gpt-5.6-sol", None);
        assert_eq!(
            body["additionalModelRequestFields"]["reasoning"]["effort"], "high",
            "OpenAI Chat 的 high 不能先变成固定 16k、再被降成 medium"
        );

        let responses = request_with(
            "kiro-gpt-5.6-sol",
            json!({ "reasoning": { "effort": "xhigh" } }),
        );
        let (body, _) = build_body(&responses, "gpt-5.6-sol", None);
        assert_eq!(
            body["additionalModelRequestFields"]["reasoning"]["effort"],
            "xhigh"
        );

        let disabled = request_with(
            "kiro-gpt-5.6-sol-thinking",
            json!({ "reasoning_effort": "none" }),
        );
        let (body, _) = build_body(&disabled, "gpt-5.6-sol", None);
        assert!(body.get("additionalModelRequestFields").is_none());
        let system = body["conversationState"]["history"][0]["userInputMessage"]["content"]
            .as_str()
            .unwrap();
        assert!(!system.contains("<thinking_mode>"));
    }

    #[test]
    fn quota_and_protocol_errors_are_not_blamed_the_same_way() {
        assert_eq!(
            classify_kiro(500, r#"{"reason":"MONTHLY_REQUEST_COUNT"}"#).kind,
            UpstreamKind::Quota
        );
        assert_eq!(
            classify_kiro(
                500,
                r#"{"message":"trace mentions MONTHLY_REQUEST_COUNT","reason":"OTHER"}"#
            )
            .kind,
            UpstreamKind::Upstream
        );
        assert_eq!(
            classify_kiro(
                403,
                "Your User ID temporarily is suspended. We've locked your account"
            )
            .kind,
            UpstreamKind::Forbidden
        );
        assert_eq!(
            classify_kiro(
                429,
                "Due to suspicious activity, we are imposing temporary limits"
            )
            .kind,
            UpstreamKind::RateLimit
        );
        assert_eq!(
            classify_kiro(500, r#"{"reason":"TOOL_USE_RESULT_MISMATCH"}"#).kind,
            UpstreamKind::BadRequest
        );
        assert_eq!(
            classify_kiro(
                524,
                "524 status code (no body). This is a server-side issue"
            )
            .kind,
            UpstreamKind::Provider
        );
        assert!(!classify_kiro(400, "bad schema").kind.blames_account());
        assert!(!classify_kiro(524, "524").kind.blames_account());
    }

    fn probe_credential() -> Credential {
        Credential {
            label: "ada".into(),
            access_token: "kiro-test".into(),
            identity: crate::identity::DeviceIdentity::derived("kiro-test"),
        }
    }

    async fn serve_once(ctype: &'static str, body: Vec<u8>) -> KiroUpstream {
        let app = axum::Router::new().route(
            "/generateAssistantResponse",
            axum::routing::post(move || async move {
                axum::response::Response::builder()
                    .header("content-type", ctype)
                    .body(axum::body::Body::from(body))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        KiroUpstream::with_endpoint(format!("http://{addr}/generateAssistantResponse"))
    }

    /// 回归：Amazon Q 把 eventstream 响应标成 `content-type: application/json`。
    /// 按 content-type 分流会把整条流丢给 JSON 分支，于是 200、不报错、一个字都不出。
    #[tokio::test]
    async fn an_eventstream_body_labelled_as_json_still_streams() {
        let mut body = eventstream::encode_frame(
            Some("assistantResponseEvent"),
            br#"{"content":"PONG","modelId":"claude-sonnet-4.6"}"#,
        );
        body.extend_from_slice(&eventstream::encode_frame(
            Some("contextUsageEvent"),
            br#"{"contextUsagePercentage":0.5}"#,
        ));
        let upstream = serve_once("application/json", body).await;
        let request = ChatRequest {
            model: "kiro-claude-sonnet-4.6".into(),
            messages: vec![Message::text(Role::User, "hi")],
            ..ChatRequest::default()
        };
        let mut deltas = Vec::new();
        let done = upstream
            .run(&probe_credential(), &request, &mut |d| deltas.push(d))
            .await
            .unwrap();
        assert_eq!(done.text, "PONG");
        assert!(deltas
            .iter()
            .any(|d| matches!(d, Delta::Text(t) if t == "PONG")));
        // 0.5% × 1M 上下文。上游只给百分比，用量得从它反推。
        assert_eq!(done.usage.input_tokens, 5_000);
        assert!(done.usage_measured);
    }

    /// 整个响应落在第一截里时也要解出来——嗅探读掉的那一截得接回缓冲区。
    #[tokio::test]
    async fn a_single_chunk_response_is_not_dropped() {
        let body =
            eventstream::encode_frame(Some("assistantResponseEvent"), br#"{"content":"hi"}"#);
        let upstream = serve_once("application/vnd.amazon.eventstream", body).await;
        let request = ChatRequest {
            model: "kiro-claude-sonnet-4.6".into(),
            messages: vec![Message::text(Role::User, "hi")],
            ..ChatRequest::default()
        };
        let done = upstream
            .run(&probe_credential(), &request, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(done.text, "hi");
    }

    /// 真的是 JSON 正文时还走 JSON 那条路，别被嗅探改掉。
    #[tokio::test]
    async fn a_real_json_body_still_takes_the_json_path() {
        let upstream = serve_once(
            "application/json",
            br#"{"assistantResponseEvent":{"content":"from json"}}"#.to_vec(),
        )
        .await;
        let request = ChatRequest {
            model: "kiro-claude-sonnet-4.6".into(),
            messages: vec![Message::text(Role::User, "hi")],
            ..ChatRequest::default()
        };
        let done = upstream
            .run(&probe_credential(), &request, &mut |_| {})
            .await
            .unwrap();
        assert_eq!(done.text, "from json");
    }

    #[test]
    fn multi_turn_with_static_prefix_compensates_cache_read_tokens() {
        let started = Instant::now();
        let c = Collected {
            saw_token_usage: true,
            usage: Some(Usage {
                input_tokens: 10_000,
                output_tokens: 200,
                ..Default::default()
            }),
            cache_prefix_tokens: 6_000,
            is_multi_turn: true,
            model: "claude-sonnet-4.6".into(),
            ..Collected::default()
        };
        let out = finish(c, started);
        assert_eq!(out.usage.cache_read_tokens, 6_000);
        assert_eq!(out.usage.input_tokens, 4_000);
        assert_eq!(out.usage.input_basis, InputBasis::ExcludesCache);
    }

    #[test]
    fn unrequested_thinking_tags_are_isolated_into_thinking_delta_and_not_leaked_to_text() {
        let started = Instant::now();
        let mut c = Collected {
            thinking_on: false,
            ..Collected::default()
        };
        let mut deltas = Vec::new();
        feed_assistant_text(
            &mut c,
            "<thinking>hidden reasoning</thinking>\n\npublic answer",
            started,
            &mut |d| deltas.push(d),
        );
        flush_hold(&mut c, started, &mut |d| deltas.push(d), true);
        assert_eq!(c.thinking, "hidden reasoning");
        assert_eq!(c.text.trim(), "public answer");
        assert!(!c.text.contains("<thinking>"));
        assert!(deltas
            .iter()
            .any(|d| matches!(d, Delta::Thinking(t) if t == "hidden reasoning")));
        assert!(deltas
            .iter()
            .any(|d| matches!(d, Delta::Text(t) if t == "public answer")));
    }
}
