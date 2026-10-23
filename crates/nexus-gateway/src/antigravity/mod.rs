//! Google 反重力 (Antigravity) 推理后端实现。
//!
//! 对接 Cloud Code `v1internal`（Daily → Sandbox → Prod）：
//! 1. `streamGenerateContent?alt=sse` 流式输出；
//! 2. 原生 Thinking 与 XML `<thinking>` 标签双重解析，并回放 `thoughtSignature`；
//! 3. 稳定 `sessionId`，请求号按官方 `agent/{会话}/{毫秒}/{轨迹}/{步}`；
//! 4. Function Calling 带上调用 id，签名落在工具调用锚点上；
//! 5. `countTokens` 预检，失败时退回本地估算。

use crate::error::{UpstreamError, UpstreamKind};
use crate::inference::{DEFAULT_IDLE_TIMEOUT, DEFAULT_MAX_TURN};
use crate::lane::{BoxFuture, Credential};
use crate::normalized::{
    estimate_tokens, is_placeholder_signature, ChatRequest, Completion, Delta, FinishReason,
    InputBasis, Message, Role, ToolCall, ToolChoice, ToolResult, Usage,
};
use crate::sse::SseDecoder;
use crate::upstream::{DeltaSink, Upstream};
use nexus_antigravity::protocol::{
    cap_max_output, clean_tool_schema, derive_session_id, official_request_id,
    should_try_next_endpoint, thinking_config_for, upstream_model, upstream_user_agent, v1_url,
    wrap_system_reminder, ContentMessage, ContentPart, FunctionCall, FunctionCallingConfig,
    FunctionDeclaration, FunctionResponse, GenerateContentRequest, GenerateContentResponse,
    GenerationConfig, InlineData, ThinkingTagParser, ToolConfig, ToolDeclaration, CLAUDE_BETA,
    CONTENT_ORIGINS, ENVELOPE_USER_AGENT, RPC_COUNT_TOKENS, RPC_STREAM_GENERATE_CONTENT,
};
use nexus_antigravity::signatures::{seal_model_turns, signature_for_client};
use nexus_antigravity::usage::{is_auth_expired, model_cooldown_ms};
use nexus_antigravity::{AntigravityService, SignatureLedger};
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

pub struct AntigravityUpstream {
    client: reqwest::Client,
    accounts: Arc<AntigravityService>,
    /// 按顺序尝试。默认 Daily → Sandbox → Prod。测试可以只留一个。
    endpoints: Vec<String>,
    /// 客户端丢掉的 thoughtSignature，按会话轮次补回。
    ledger: SignatureLedger,
    idle_timeout: Duration,
    max_turn: Duration,
}

impl AntigravityUpstream {
    pub fn new(accounts: Arc<AntigravityService>) -> Self {
        Self {
            client: crate::inference::http_client(),
            accounts,
            endpoints: CONTENT_ORIGINS.iter().map(|s| (*s).to_string()).collect(),
            ledger: SignatureLedger::default(),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
        }
    }

    #[cfg(test)]
    pub fn with_endpoint(accounts: Arc<AntigravityService>, endpoint: String) -> Self {
        Self {
            client: crate::inference::http_client(),
            accounts,
            endpoints: vec![endpoint],
            ledger: SignatureLedger::default(),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
        }
    }

    async fn post_v1(
        &self,
        rpc: &str,
        access_token: &str,
        model: &str,
        payload: &Value,
        accept_sse: bool,
        label: &str,
    ) -> Result<reqwest::Response, UpstreamError> {
        let mut last_detail = String::from("没有可用的 Cloud Code 端点");
        for (idx, origin) in self.endpoints.iter().enumerate() {
            let url = v1_url(origin, rpc);
            let has_next = idx + 1 < self.endpoints.len();
            let mut builder = self
                .client
                .post(&url)
                .bearer_auth(access_token)
                .header("User-Agent", upstream_user_agent())
                .header("Content-Type", "application/json")
                .timeout(self.max_turn)
                .json(payload);
            if accept_sse {
                builder = builder.header("Accept", "text/event-stream");
            }
            if model.to_ascii_lowercase().contains("claude") {
                builder = builder.header("anthropic-beta", CLAUDE_BETA);
            }
            match builder.send().await {
                Ok(resp) => {
                    let status = resp.status();
                    if status.is_success() {
                        if idx > 0 {
                            tracing::info!(%url, "Antigravity 换端点后请求成功");
                        }
                        return Ok(resp);
                    }
                    let code = status.as_u16();
                    let body = resp.text().await.unwrap_or_default();
                    if is_auth_expired(code, &body) {
                        return Err(UpstreamError::new(
                            UpstreamKind::Auth,
                            401,
                            format!("Google Antigravity 账号 [{label}] 凭证失效，准备重新授权"),
                        ));
                    }
                    if let Some(wait_ms) = model_cooldown_ms(code, &body) {
                        tracing::warn!(%code, %body, %label, %model, wait_ms, "Antigravity 上游模型限频/受限");
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as i64)
                            .unwrap_or(0);
                        return Err(UpstreamError::new(
                            UpstreamKind::RateLimit,
                            429,
                            format!("Google Antigravity 账号 [{label}] 的模型 {model} 暂时受限：{body}"),
                        )
                        .with_reset_at_ms(now_ms.saturating_add(wait_ms as i64)));
                    }
                    last_detail = format!("Google Antigravity 上游返回错误 ({status})：{body}");
                    if has_next && should_try_next_endpoint(code) {
                        tracing::warn!(%url, %status, "Antigravity 端点不可用，尝试下一个");
                        continue;
                    }
                    return Err(UpstreamError::new(
                        UpstreamKind::Upstream,
                        code,
                        last_detail,
                    ));
                }
                Err(err) => {
                    last_detail = format!("连接 Google Antigravity 服务失败：{err}");
                    if has_next {
                        tracing::warn!(%url, %err, "Antigravity 端点连接失败，尝试下一个");
                        continue;
                    }
                }
            }
        }
        Err(UpstreamError::new(UpstreamKind::Upstream, 502, last_detail))
    }
}

impl Upstream for AntigravityUpstream {
    fn stream<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ChatRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
        Box::pin(async move {
            let model = upstream_model(&request.model);

            // 读取账号配置的 GCP Project ID
            let project_id = self.accounts.list().ok().and_then(|list| {
                list.into_iter()
                    .find(|a| {
                        a.label == credential.label || a.email.as_deref() == Some(&credential.label)
                    })
                    .and_then(|a| a.project_id)
            });

            let project_name = project_id.as_deref().unwrap_or("aicode-consumers");
            let mut inner_req = build_request(&model, request);
            if let Some(session) = inner_req.session_id.clone() {
                self.ledger
                    .restore(&session, &model, &mut inner_req.contents);
            }
            let session_for_sig = inner_req.session_id.clone();
            let prior_turns = request
                .messages
                .iter()
                .filter(|message| message.role == Role::Assistant)
                .count();
            let payload = envelope(&project_name, &model, &inner_req);

            let resp = self
                .post_v1(
                    RPC_STREAM_GENERATE_CONTENT,
                    &credential.access_token,
                    &model,
                    &payload,
                    true,
                    &credential.label,
                )
                .await?;

            // 处理 SSE 流式响应
            let mut resp = resp;
            let mut decoder = SseDecoder::default();
            let mut thinking_parser = ThinkingTagParser::new();

            let mut final_text = String::new();
            let mut final_thinking = String::new();
            let mut thinking_signature: Option<String> = None;
            let mut tool_calls = Vec::new();
            let mut finish_reason = FinishReason::Stop;
            let mut usage = Usage {
                input_basis: InputBasis::IncludesCache,
                ..Default::default()
            };

            while let Some(chunk) = tokio::time::timeout(self.idle_timeout, resp.chunk())
                .await
                .map_err(|_| {
                    UpstreamError::new(
                        UpstreamKind::Timeout,
                        504,
                        "等待 Google Antigravity 流式响应超时",
                    )
                })?
                .map_err(|err| {
                    UpstreamError::new(
                        UpstreamKind::Upstream,
                        502,
                        format!("读取 Google Antigravity 数据帧失败：{err}"),
                    )
                })?
            {
                for ev in decoder.push(&chunk) {
                    let data = ev.data.trim();
                    if data.is_empty() || data == "[DONE]" {
                        continue;
                    }

                    let parsed: GenerateContentResponse =
                        match serde_json::from_str::<GenerateContentResponse>(data) {
                            Ok(p) => p.unwrap_envelope(),
                            Err(e) => {
                                tracing::warn!(%e, %data, "解析 Antigravity SSE 数据帧失败");
                                continue;
                            }
                        };

                    if let Some(meta) = parsed.usage_metadata {
                        if let Some(p) = meta.prompt_token_count {
                            usage.input_tokens = p as u32;
                        }
                        if let Some(c) = meta.candidates_token_count {
                            usage.output_tokens = c as u32;
                        }
                        if let Some(cached) = meta.cached_content_token_count {
                            usage.cache_read_tokens = cached.max(0) as u32;
                        }
                        if let Some(thoughts) = meta.thoughts_token_count {
                            usage.reasoning_tokens = thoughts.max(0) as u32;
                        }
                    }

                    if let Some(candidates) = parsed.candidates {
                        for cand in candidates {
                            if let Some(fr) = cand.finish_reason.as_deref() {
                                finish_reason = match fr {
                                    "MAX_TOKENS" => FinishReason::Length,
                                    "SAFETY" | "BLOCKLIST" => FinishReason::ContentFilter,
                                    "STOP" => FinishReason::Stop,
                                    _ => FinishReason::Stop,
                                };
                            }

                            if let Some(content) = cand.content {
                                if let Some(parts) = content.parts {
                                    for part in parts {
                                        if let Some(sig) =
                                            part.thought_signature.filter(|s| !s.trim().is_empty())
                                        {
                                            let client_sig = signature_for_client(&model, &sig);
                                            thinking_signature = Some(client_sig.clone());
                                            on_delta(Delta::ThinkingSignature(client_sig));
                                        }
                                        // 1. 如果是原生思考块 (thought: true)
                                        if part.thought.unwrap_or(false) {
                                            if let Some(th) = part.text {
                                                final_thinking.push_str(&th);
                                                on_delta(Delta::Thinking(th));
                                            }
                                            continue;
                                        }

                                        // 2. 如果包含文本，经过思考标签解析器拆解
                                        if let Some(txt) = part.text {
                                            let (th_delta, tx_delta) = thinking_parser.push(&txt);
                                            if let Some(th) = th_delta {
                                                final_thinking.push_str(&th);
                                                on_delta(Delta::Thinking(th));
                                            }
                                            if let Some(tx) = tx_delta {
                                                final_text.push_str(&tx);
                                                on_delta(Delta::Text(tx));
                                            }
                                        }

                                        // 3. 如果包含函数调用
                                        if let Some(fc) = part.function_call {
                                            let tc = ToolCall {
                                                id: fc.id.unwrap_or_else(|| {
                                                    format!(
                                                        "call_{}",
                                                        uuid::Uuid::new_v4().simple()
                                                    )
                                                }),
                                                name: fc.name,
                                                arguments: fc.args.to_string(),
                                            };
                                            tool_calls.push(tc);
                                            finish_reason = FinishReason::ToolCalls;
                                        }

                                        if let Some(inline) = part.inline_data {
                                            let piece =
                                                inline_as_text(&inline.mime_type, &inline.data);
                                            final_text.push_str(&piece);
                                            on_delta(Delta::Text(piece));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // 冲刷末尾缓存
            let (last_th, last_tx) = thinking_parser.flush();
            if let Some(th) = last_th {
                final_thinking.push_str(&th);
                on_delta(Delta::Thinking(th));
            }
            if let Some(tx) = last_tx {
                final_text.push_str(&tx);
                on_delta(Delta::Text(tx));
            }

            // 兜底 token 估算
            if usage.input_tokens == 0 && usage.output_tokens == 0 {
                let estimated_in = request
                    .messages
                    .iter()
                    .map(|m| estimate_tokens(&m.text))
                    .sum::<u32>();
                let estimated_out = (final_text.len() / 4) as u32;
                usage.input_tokens = estimated_in;
                usage.output_tokens = estimated_out;
            }

            if let (Some(session), Some(sig)) = (session_for_sig, thinking_signature.as_deref()) {
                self.ledger.remember(&session, prior_turns, sig);
            }

            Ok(Completion {
                text: final_text,
                thinking: final_thinking,
                tool_calls,
                finish_reason,
                usage,
                usage_measured: true,
                routed_model: Some(model),
                ttft_ms: None,
                turn_ms: 0,
                raw_response: None,
                thinking_signature: thinking_signature.clone(),
                redacted_thinking: None,
            })
        })
    }

    fn count_tokens<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ChatRequest,
    ) -> BoxFuture<'a, Result<u64, UpstreamError>> {
        Box::pin(async move {
            let model = upstream_model(&request.model);
            let project_id = self.accounts.list().ok().and_then(|list| {
                list.into_iter()
                    .find(|a| {
                        a.label == credential.label || a.email.as_deref() == Some(&credential.label)
                    })
                    .and_then(|a| a.project_id)
            });
            let project_name = project_id.as_deref().unwrap_or("aicode-consumers");
            let inner_req = build_request(&model, request);
            let payload = envelope(&project_name, &model, &inner_req);

            let resp = match self
                .post_v1(
                    RPC_COUNT_TOKENS,
                    &credential.access_token,
                    &model,
                    &payload,
                    false,
                    &credential.label,
                )
                .await
            {
                Ok(resp) => resp,
                Err(_) => {
                    let sum = request
                        .messages
                        .iter()
                        .map(|m| estimate_tokens(&m.text) as u64)
                        .sum();
                    return Ok(sum);
                }
            };

            if !resp.status().is_success() {
                let sum = request
                    .messages
                    .iter()
                    .map(|m| estimate_tokens(&m.text) as u64)
                    .sum();
                return Ok(sum);
            }

            let val: Value = resp.json().await.unwrap_or(Value::Null);
            let count = val.get("totalTokens").and_then(|v| v.as_u64()).unwrap_or(0);
            Ok(count)
        })
    }
}

fn envelope(project: &str, model: &str, request: &GenerateContentRequest) -> Value {
    let step = request.contents.len() as u64;
    let session = request.session_id.as_deref().unwrap_or("default");
    let mut body = serde_json::Map::new();
    body.insert("project".into(), json!(project));
    body.insert(
        "requestId".into(),
        json!(official_request_id(session, step)),
    );
    body.insert(
        "request".into(),
        serde_json::to_value(request).unwrap_or(Value::Null),
    );
    body.insert("model".into(), json!(model));
    body.insert("userAgent".into(), json!(ENVELOPE_USER_AGENT));
    let kind = if model.to_ascii_lowercase().contains("image")
        || model.to_ascii_lowercase().contains("imagen")
    {
        Some("image_gen")
    } else {
        None
    };
    if let Some(kind) = kind {
        body.insert("requestType".into(), json!(kind));
    }
    Value::Object(body)
}

fn inline_as_text(mime: &str, data: &str) -> String {
    if mime.starts_with("image/") {
        format!("![image](data:{mime};base64,{data})")
    } else {
        format!("[{mime}](data:{mime};base64,{data})")
    }
}

fn replay_signature(message: &Message) -> Option<String> {
    message.reasoning.iter().rev().find_map(|reasoning| {
        let signature = reasoning.signature.as_deref()?.trim();
        if signature.is_empty() || is_placeholder_signature(&reasoning.text, signature) {
            None
        } else {
            Some(signature.to_string())
        }
    })
}

fn tool_result_part(tr: &ToolResult) -> ContentPart {
    let text = if tr.text.trim().is_empty() {
        "Command executed successfully."
    } else {
        tr.text.as_str()
    };
    let response_obj = match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(mut map)) => {
            if let Some(Value::String(output)) = map.get_mut("output") {
                if output.trim().is_empty() {
                    *output = "Command executed successfully.".into();
                }
            }
            Value::Object(map)
        }
        Ok(other) => json!({ "output": other }),
        Err(_) => json!({ "output": text }),
    };
    let id = normalize_tool_id(&tr.tool_call_id);
    ContentPart {
        text: None,
        thought: None,
        thought_signature: None,
        function_call: None,
        function_response: Some(FunctionResponse {
            id,
            name: tr.tool_name.clone(),
            response: response_obj,
        }),
        inline_data: None,
    }
}

/// 将统一中间表示 `ChatRequest` 转换为 Google Antigravity 请求体。
fn build_request(model: &str, request: &ChatRequest) -> GenerateContentRequest {
    let mut contents: Vec<ContentMessage> = Vec::new();
    let mut system_text = String::new();
    let mut first_user_text = String::new();
    let mut dialogue_started = false;

    for msg in &request.messages {
        match msg.role {
            Role::System => {
                // 开场系统提示留在 systemInstruction，前缀字节稳定，提示缓存才能命中。
                // 对话开始后插入的系统消息改写成用户侧 reminder，不再改写那份前缀。
                if !dialogue_started {
                    if !system_text.is_empty() {
                        system_text.push('\n');
                    }
                    system_text.push_str(&msg.text);
                } else if !msg.text.trim().is_empty() {
                    contents.push(ContentMessage::user(vec![ContentPart::text(
                        wrap_system_reminder(&msg.text),
                    )]));
                }
            }
            Role::User => {
                dialogue_started = true;
                if first_user_text.is_empty() {
                    first_user_text = msg.text.clone();
                }
                let mut text_parts = Vec::new();
                if !msg.text.is_empty() {
                    text_parts.push(ContentPart::text(&msg.text));
                }
                for img in &msg.images {
                    text_parts.push(inline_image(img));
                }
                push_tool_results(model, &mut contents, &mut text_parts, &msg.tool_results);
                if !text_parts.is_empty() {
                    contents.push(ContentMessage::user(text_parts));
                }
            }
            Role::Assistant => {
                dialogue_started = true;
                let signature = replay_signature(msg);
                let mut parts = Vec::new();
                for reasoning in &msg.reasoning {
                    if !reasoning.text.is_empty() {
                        parts.push(ContentPart::thought(&reasoning.text));
                    }
                }
                if !msg.text.is_empty() {
                    parts.push(ContentPart::text(&msg.text));
                }
                for tc in &msg.tool_calls {
                    let args = match serde_json::from_str::<Value>(&tc.arguments) {
                        Ok(Value::Object(map)) => Value::Object(map),
                        Ok(other) => json!({ "raw": other }),
                        Err(_) => json!({ "raw": tc.arguments }),
                    };
                    let id = normalize_tool_id(&tc.id);
                    parts.push(ContentPart {
                        text: None,
                        thought: None,
                        thought_signature: None,
                        function_call: Some(FunctionCall {
                            id,
                            name: tc.name.clone(),
                            args,
                        }),
                        function_response: None,
                        inline_data: None,
                    });
                }
                // 签名只放在这一轮的锚点上：有工具调用就放在最后一次 functionCall，否则放在思考块。
                if let Some(sig) = signature {
                    let anchor = parts
                        .iter()
                        .rposition(|part| part.function_call.is_some())
                        .or_else(|| parts.iter().rposition(|part| part.thought == Some(true)))
                        .or_else(|| parts.len().checked_sub(1));
                    if let Some(index) = anchor {
                        parts[index].thought_signature = Some(sig);
                    }
                }
                contents.push(ContentMessage::model(parts));
            }
            Role::Tool => {
                dialogue_started = true;
                let mut text_parts = Vec::new();
                push_tool_results(model, &mut contents, &mut text_parts, &msg.tool_results);
                if !text_parts.is_empty() {
                    contents.push(ContentMessage::user(text_parts));
                }
            }
        }
    }

    // 系统指令
    let system_instruction = if !system_text.is_empty() {
        Some(ContentMessage::system(system_text.clone()))
    } else {
        None
    };

    // 工具声明
    let tools = if !request.tools.is_empty() {
        let decls = request
            .tools
            .iter()
            .map(|t| FunctionDeclaration {
                name: t.name.clone(),
                description: if t.description.is_empty() {
                    None
                } else {
                    Some(t.description.clone())
                },
                parameters: {
                    let mut schema = t.parameters.clone();
                    clean_tool_schema(&mut schema);
                    Some(schema)
                },
            })
            .collect();
        Some(vec![ToolDeclaration {
            function_declarations: decls,
        }])
    } else {
        None
    };

    let generation_config = Some(GenerationConfig {
        temperature: request.sampling.temperature,
        max_output_tokens: cap_max_output(
            model,
            request.sampling.max_output_tokens.map(|v| v as i32),
        ),
        top_p: request.sampling.top_p,
        stop_sequences: if request.sampling.stop_sequences.is_empty() {
            None
        } else {
            Some(request.sampling.stop_sequences.clone())
        },
        thinking_config: thinking_config_for(model),
    });

    let derived = derive_session_id(
        &first_user_text,
        if system_text.is_empty() {
            None
        } else {
            Some(&system_text)
        },
    );
    let session_id = request
        .conversation_id
        .clone()
        .filter(|id| !id.trim().is_empty())
        .unwrap_or(derived);

    let mut built = GenerateContentRequest {
        contents,
        system_instruction,
        generation_config,
        tools,
        tool_config: tool_config_for(&request.tool_choice, request.tools.is_empty()),
        session_id: Some(session_id),
    };
    built.finalize();
    seal_model_turns(model, &mut built.contents);
    built
}

fn normalize_tool_id(id: &str) -> Option<String> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix("call") {
        let digits = !rest.is_empty()
            && !rest.starts_with('_')
            && (rest.chars().next().is_some_and(|ch| ch.is_ascii_digit())
                || (rest.len() >= 6 && rest.chars().all(|ch| ch.is_ascii_hexdigit())));
        if digits {
            return Some(format!("call_{rest}"));
        }
    }
    Some(trimmed.to_string())
}

fn tool_config_for(choice: &ToolChoice, no_tools: bool) -> Option<ToolConfig> {
    if no_tools {
        return None;
    }
    let config = match choice {
        ToolChoice::Auto => return None,
        ToolChoice::None => FunctionCallingConfig {
            mode: "NONE".to_string(),
            allowed_function_names: None,
        },
        ToolChoice::Required => FunctionCallingConfig {
            mode: "ANY".to_string(),
            allowed_function_names: None,
        },
        ToolChoice::Tool(name) => FunctionCallingConfig {
            mode: "ANY".to_string(),
            allowed_function_names: Some(vec![name.clone()]),
        },
    };
    Some(ToolConfig {
        function_calling_config: config,
    })
}

fn inline_image(img: &crate::normalized::ImageInput) -> ContentPart {
    ContentPart {
        text: None,
        thought: None,
        thought_signature: None,
        function_call: None,
        function_response: None,
        inline_data: Some(InlineData {
            mime_type: img.mime_type.clone(),
            data: img.data.clone(),
        }),
    }
}

fn push_tool_results(
    model: &str,
    contents: &mut Vec<ContentMessage>,
    text_parts: &mut Vec<ContentPart>,
    results: &[ToolResult],
) {
    if results.is_empty() {
        return;
    }
    let mut responses = Vec::new();
    let mut images = Vec::new();
    for result in results {
        responses.push(tool_result_part(result));
        for image in &result.images {
            images.push(inline_image(image));
        }
    }
    if model.to_ascii_lowercase().contains("claude") {
        let mut parts = responses;
        parts.append(text_parts);
        parts.extend(images);
        contents.push(ContentMessage::user(parts));
        return;
    }
    contents.push(ContentMessage::model(responses));
    if !images.is_empty() {
        contents.push(ContentMessage::user(images));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalized::{Sampling, ToolDef};

    fn request(messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: "gemini-3.1-pro-high".into(),
            messages,
            ..ChatRequest::default()
        }
    }

    #[test]
    fn system_prefix_stays_frozen_when_a_later_reminder_arrives() {
        let first = build_request(
            "gemini-pro-agent",
            &request(vec![
                Message::text(Role::System, "be brief"),
                Message::text(Role::User, "hi"),
            ]),
        );
        let second = build_request(
            "gemini-pro-agent",
            &request(vec![
                Message::text(Role::System, "be brief"),
                Message::text(Role::User, "hi"),
                Message::text(Role::Assistant, "hello"),
                Message::text(Role::System, "the file changed"),
                Message::text(Role::User, "continue"),
            ]),
        );
        let frozen = first.system_instruction.unwrap().parts[0].text.clone();
        assert_eq!(second.system_instruction.unwrap().parts[0].text, frozen);
        assert_eq!(first.session_id, second.session_id);
        let reminder = second
            .contents
            .iter()
            .flat_map(|message| message.parts.iter())
            .filter_map(|part| part.text.as_deref())
            .any(|text| text.contains("<system-reminder>") && text.contains("the file changed"));
        assert!(reminder);
        assert_eq!(
            second.contents.last().unwrap().role.as_deref(),
            Some("user")
        );
    }

    #[test]
    fn required_tool_choice_becomes_any() {
        let mut req = request(vec![Message::text(Role::User, "run it")]);
        req.tools = vec![ToolDef {
            name: "exec".into(),
            description: "run".into(),
            parameters: serde_json::json!({"type": "object"}),
            ..ToolDef::default()
        }];
        req.tool_choice = ToolChoice::Tool("exec".into());
        req.sampling = Sampling {
            stop_sequences: vec!["END".into()],
            ..Sampling::default()
        };
        let built = build_request("gemini-pro-agent", &req);
        let config = built.tool_config.unwrap();
        assert_eq!(config.function_calling_config.mode, "ANY");
        assert_eq!(
            config
                .function_calling_config
                .allowed_function_names
                .unwrap(),
            vec!["exec".to_string()]
        );
        assert_eq!(
            built.generation_config.unwrap().stop_sequences.unwrap(),
            vec!["END".to_string()]
        );
    }

    #[test]
    fn gemini_tool_results_stay_on_their_own_model_turn() {
        let mut assistant = Message::text(Role::Assistant, "checking");
        assistant.tool_calls = vec![
            ToolCall {
                id: "call573077".into(),
                name: "exec".into(),
                arguments: "not-json".into(),
            },
            ToolCall {
                id: "call_2".into(),
                name: "read".into(),
                arguments: "{\"path\":\"a\"}".into(),
            },
        ];
        assistant.reasoning = vec![crate::normalized::Reasoning {
            text: "hmm".into(),
            signature: Some("EqUreal".into()),
            redacted: None,
        }];
        let mut tool = Message::text(Role::Tool, "");
        tool.tool_results = vec![ToolResult {
            tool_call_id: "call573077".into(),
            tool_name: "exec".into(),
            text: "   ".into(),
            is_error: false,
            images: Vec::new(),
        }];
        let built = build_request(
            "gemini-pro-agent",
            &request(vec![
                Message::text(Role::User, "go"),
                assistant,
                tool,
                Message::text(Role::User, "next"),
            ]),
        );
        let model_turns: Vec<_> = built
            .contents
            .iter()
            .filter(|message| message.role.as_deref() == Some("model"))
            .collect();
        assert_eq!(model_turns.len(), 2);
        assert!(model_turns[0]
            .parts
            .iter()
            .any(|part| part.function_call.is_some()));
        assert!(model_turns[0]
            .parts
            .iter()
            .all(|part| part.function_response.is_none()));
        assert_eq!(
            model_turns[0]
                .parts
                .iter()
                .find(|part| part.function_call.is_some())
                .unwrap()
                .function_call
                .as_ref()
                .unwrap()
                .id
                .as_deref(),
            Some("call_573077")
        );
        assert_eq!(
            model_turns[0].parts[1]
                .thought_signature
                .as_deref()
                .or(model_turns[0]
                    .parts
                    .iter()
                    .find_map(|part| part.thought_signature.as_deref())),
            Some("EqUreal")
        );
        let response = model_turns[1].parts[0].function_response.as_ref().unwrap();
        assert_eq!(response.id.as_deref(), Some("call_573077"));
        assert_eq!(
            response.response["output"],
            "Command executed successfully."
        );
        assert!(model_turns[1]
            .parts
            .iter()
            .all(|part| part.thought_signature.is_none()));
        assert!(built.system_instruction.is_none());
    }

    #[test]
    fn claude_tool_results_stay_on_the_user_turn() {
        let mut assistant = Message::text(Role::Assistant, "");
        assistant.tool_calls = vec![ToolCall {
            id: "call_1".into(),
            name: "exec".into(),
            arguments: "{}".into(),
        }];
        let mut tool = Message::text(Role::Tool, "");
        tool.tool_results = vec![ToolResult {
            tool_call_id: "call_1".into(),
            tool_name: "exec".into(),
            text: "ok".into(),
            is_error: false,
            images: Vec::new(),
        }];
        let built = build_request(
            "claude-opus-4-6-thinking",
            &request(vec![Message::text(Role::User, "go"), assistant, tool]),
        );
        let response_turn = built
            .contents
            .iter()
            .find(|message| {
                message
                    .parts
                    .iter()
                    .any(|part| part.function_response.is_some())
            })
            .unwrap();
        assert_eq!(response_turn.role.as_deref(), Some("user"));
        assert!(built
            .contents
            .iter()
            .filter(|message| message.role.as_deref() == Some("model"))
            .all(|message| {
                message.parts.iter().all(|part| {
                    part.thought_signature.as_deref() != Some("skip_thought_signature_validator")
                })
            }));
    }
}
