//! Gemini 网页协议后端。和反重力通道互不共用账号、端点、凭证。

use crate::error::{UpstreamError, UpstreamKind};
use crate::lane::{BoxFuture, Credential};
use crate::normalized::{
    estimate_tokens, ChatRequest, Completion, Delta, FinishReason, InputBasis, Message, Role,
    ToolCall, ToolChoice, Usage,
};
use crate::upstream::{DeltaSink, Upstream};
use nexus_gemini_web::protocol::{normalize_cookie, parse_tool_calls, FollowUp};
use nexus_gemini_web::session;
use nexus_gemini_web::GeminiWebService;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct GeminiWebUpstream {
    client: reqwest::Client,
    accounts: Arc<GeminiWebService>,
    /// 上一轮网页会话。键是「不含最后一条新消息」的历史指纹。
    conversations: Mutex<HashMap<String, StoredConv>>,
}

struct StoredConv {
    follow: FollowUp,
    at: Instant,
}

impl GeminiWebUpstream {
    pub fn new(accounts: Arc<GeminiWebService>) -> Self {
        Self {
            client: session::http_client(),
            accounts,
            conversations: Mutex::new(HashMap::new()),
        }
    }
}

impl Upstream for GeminiWebUpstream {
    fn stream<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ChatRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
        Box::pin(async move {
            let mut cookie =
                session::refresh_session(&self.client, &normalize_cookie(&credential.access_token))
                    .await;
            let parent = history_key(&request.messages, true);
            let follow = self.recall(&parent);
            let prompt = match &follow {
                Some(_) => latest_turn(request),
                None => render_prompt(request, true),
            };
            let run = self
                .drive(request, &cookie, &prompt, follow.as_ref(), on_delta)
                .await;
            let output = match run {
                Ok(output) => output,
                Err(_err) if follow.is_some() => {
                    self.forget(&parent);
                    self.drive(
                        request,
                        &cookie,
                        &render_prompt(request, true),
                        None,
                        on_delta,
                    )
                    .await?
                }
                Err(err) => return Err(err),
            };
            if output.cookie != cookie {
                cookie = output.cookie.clone();
                let id = account_id(&self.accounts, &credential.label);
                if !id.is_empty() {
                    let _ = self.accounts.update_cookie(&id, &cookie);
                }
            }
            if let Some(next) = output.follow.clone() {
                let child = child_key(&request.messages, &output.text);
                self.remember(child, next);
            }
            let text = output.text.clone();
            let (clean, parsed) = parse_tool_calls(&text);
            let tool_calls = parsed
                .into_iter()
                .map(|call| ToolCall {
                    id: format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..8]),
                    name: call.name,
                    arguments: call.arguments,
                })
                .collect::<Vec<_>>();
            let finish = if tool_calls.is_empty() {
                FinishReason::Stop
            } else {
                FinishReason::ToolCalls
            };
            let input = request
                .messages
                .iter()
                .map(|m| estimate_tokens(&m.text))
                .sum();
            Ok(Completion {
                text: if clean.is_empty() {
                    text.clone()
                } else {
                    clean
                },
                thinking: output.thinking,
                tool_calls,
                finish_reason: finish,
                usage: Usage {
                    input_tokens: input,
                    output_tokens: estimate_tokens(&text),
                    input_basis: InputBasis::Unspecified,
                    ..Usage::default()
                },
                usage_measured: false,
                routed_model: Some(output.upstream_model.unwrap_or(request.model.clone())),
                ttft_ms: None,
                turn_ms: 0,
                raw_response: None,
                thinking_signature: None,
                redacted_thinking: None,
            })
        })
    }
}

impl GeminiWebUpstream {
    async fn drive(
        &self,
        request: &ChatRequest,
        cookie: &str,
        prompt: &str,
        follow: Option<&FollowUp>,
        on_delta: DeltaSink<'_>,
    ) -> Result<session::TurnOutput, UpstreamError> {
        let mut shown = String::new();
        let mut raw = String::new();
        let output = session::generate(
            &self.client,
            cookie,
            &request.model,
            prompt,
            follow,
            &mut |thinking, delta| {
                if thinking {
                    on_delta(Delta::Thinking(delta));
                    return;
                }
                raw.push_str(&delta);
                let safe = visible_text(&raw);
                if safe.len() > shown.len() && safe.starts_with(&shown) {
                    let piece = safe[shown.len()..].to_string();
                    shown = safe;
                    if !piece.is_empty() {
                        on_delta(Delta::Text(piece));
                    }
                }
            },
        )
        .await
        .map_err(|err| {
            let kind = if err.contains("Cookie") || err.contains("令牌") || err.contains("失效")
            {
                UpstreamKind::Auth
            } else {
                UpstreamKind::Upstream
            };
            let code = if kind == UpstreamKind::Auth { 401 } else { 502 };
            UpstreamError::new(kind, code, err)
        })?;
        Ok(output)
    }

    fn recall(&self, key: &str) -> Option<FollowUp> {
        if key.is_empty() {
            return None;
        }
        let mut map = self.conversations.lock().expect("gemini web conversations");
        map.retain(|_, conv| conv.at.elapsed() < std::time::Duration::from_secs(90 * 60));
        map.get(key).map(|conv| conv.follow.clone())
    }

    fn remember(&self, key: String, follow: FollowUp) {
        if key.is_empty() {
            return;
        }
        self.conversations
            .lock()
            .expect("gemini web conversations")
            .insert(
                key,
                StoredConv {
                    follow,
                    at: Instant::now(),
                },
            );
    }

    fn forget(&self, key: &str) {
        self.conversations
            .lock()
            .expect("gemini web conversations")
            .remove(key);
    }
}

fn visible_text(raw: &str) -> String {
    let fences = raw.matches("```").count();
    let held = if fences % 2 == 1 {
        raw.rfind("```").map(|idx| &raw[..idx]).unwrap_or(raw)
    } else {
        raw
    };
    parse_tool_calls(held).0
}

fn latest_turn(request: &ChatRequest) -> String {
    let Some(message) = request.messages.last() else {
        return String::new();
    };
    if message.role == Role::Tool {
        return message
            .tool_results
            .iter()
            .map(|result| format!("[Tool result for {}]: {}", result.tool_name, result.text))
            .collect::<Vec<_>>()
            .join("\n");
    }
    message.text.clone()
}

fn render_prompt(request: &ChatRequest, with_tools: bool) -> String {
    let mut parts = Vec::new();
    if with_tools && !request.tools.is_empty() && !matches!(request.tool_choice, ToolChoice::None) {
        let defs = serde_json::to_string_pretty(
            &request
                .tools
                .iter()
                .map(|tool| {
                    serde_json::json!({
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    })
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        let rule = match &request.tool_choice {
            ToolChoice::Required => {
                "You MUST call one of the tools. Reply with the tool_call block and nothing else."
                    .to_string()
            }
            ToolChoice::Tool(name) => format!(
                "You MUST call the tool {name}. Reply with the tool_call block and nothing else."
            ),
            _ => "Only use tool_call blocks when needed.".to_string(),
        };
        parts.push(format!(
            "[System instruction]: You have access to tools. To call a tool, respond with:\n```tool_call\n{{\"name\": \"func_name\", \"arguments\": {{}}}}\n```\n{rule}\n\nAvailable tools:\n{defs}"
        ));
    }
    for message in &request.messages {
        match message.role {
            Role::System => parts.push(format!("[System instruction]: {}", message.text)),
            Role::Assistant => {
                let mut body = message.text.clone();
                for call in &message.tool_calls {
                    body.push_str(&format!(
                        "\n```tool_call\n{{\"name\": \"{}\", \"arguments\": {}}}\n```",
                        call.name, call.arguments
                    ));
                }
                if !body.trim().is_empty() {
                    parts.push(format!("[Assistant]: {body}"));
                }
            }
            Role::Tool => {
                for result in &message.tool_results {
                    parts.push(format!(
                        "[Tool result for {}]: {}",
                        result.tool_name, result.text
                    ));
                }
            }
            Role::User => {
                if !message.text.is_empty() {
                    parts.push(message.text.clone());
                }
            }
        }
    }
    parts.retain(|part| !part.trim().is_empty());
    parts.join("\n\n")
}

fn history_key(messages: &[Message], drop_last: bool) -> String {
    let end = if drop_last && !messages.is_empty() {
        messages.len() - 1
    } else {
        messages.len()
    };
    if end == 0 {
        return String::new();
    }
    fingerprint(&messages[..end], "")
}

fn child_key(messages: &[Message], assistant: &str) -> String {
    fingerprint(messages, assistant)
}

fn fingerprint(messages: &[Message], extra_assistant: &str) -> String {
    let mut raw = String::new();
    for message in messages {
        raw.push_str(role_name(message.role));
        raw.push('\u{1f}');
        raw.push_str(&message.text);
        for call in &message.tool_calls {
            raw.push('\u{1f}');
            raw.push_str(&call.name);
            raw.push_str(&call.arguments);
        }
        for result in &message.tool_results {
            raw.push('\u{1f}');
            raw.push_str(&result.tool_name);
            raw.push_str(&result.text);
        }
        raw.push('\u{1e}');
    }
    if !extra_assistant.is_empty() {
        raw.push_str("assistant");
        raw.push('\u{1f}');
        raw.push_str(extra_assistant);
        raw.push('\u{1e}');
    }
    format!("{:x}", Sha256::digest(raw.as_bytes()))
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn account_id(accounts: &GeminiWebService, label: &str) -> String {
    accounts
        .list()
        .ok()
        .and_then(|list| {
            list.into_iter()
                .find(|account| account.label == label || account.email.as_deref() == Some(label))
                .map(|account| account.id.as_str().to_string())
        })
        .unwrap_or_default()
}
