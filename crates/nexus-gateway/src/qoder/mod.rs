//! Qoder 直连：COSY 签名的 `agent_chat_generation` SSE。
//!
//! 出站不是 CLI。PAT 事先换成 job token（在账号层），这里用那把短票和账号身份
//! 签一个请求，打到这个号所属的网关（国际 `api3.qoder.sh` / 国内
//! `gateway.qoder.com.cn`）。响应是 OpenAI 形状的增量，包在 SSE 信封里。
//!
//! 对外模型名是 `qoder/Qwen3.8-Max` 这种公开名字。同一个名字在两边的上游 key
//! 可能不同，所以 key 要等拿到号的 `backend` 再定。这个号没有的模型记成
//! 「这个号不支持」，接力队会换下一个号。

use crate::error::{UpstreamError, UpstreamKind};
use crate::inference::{DEFAULT_IDLE_TIMEOUT, DEFAULT_MAX_TURN};
use crate::lane::{BoxFuture, Credential};
use crate::normalized::{
    ChatRequest, Completion, Delta, FinishReason, Message, Role, ToolCall, Usage,
};
use crate::sse::{SseDecoder, SseEvent};
use crate::upstream::{DeltaSink, Upstream};
use nexus_qoder::chat::{
    self, InImage, InRole, InTool, InToolCall, InTurn, SplitOut, StreamPiece, ThinkingSplit,
};
use nexus_qoder::cosy::{self, CosyIdentity};
use nexus_qoder::model::{QoderBackend, QoderIdentity};
use nexus_qoder::protocol::{self, ResolvedModel};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub trait QoderRouting: Send + Sync {
    fn identity(&self, label: &str) -> Option<QoderIdentity>;
    fn invalidate(&self, label: &str);
}

pub struct FixedIdentity(pub QoderIdentity);

impl QoderRouting for FixedIdentity {
    fn identity(&self, _label: &str) -> Option<QoderIdentity> {
        Some(self.0.clone())
    }

    fn invalidate(&self, _label: &str) {}
}

pub struct QoderUpstream {
    client: reqwest::Client,
    routing: Arc<dyn QoderRouting>,
    endpoint_override: Option<String>,
    idle_timeout: Duration,
    max_turn: Duration,
}

impl QoderUpstream {
    pub fn new(routing: Arc<dyn QoderRouting>) -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .no_proxy()
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            client,
            routing,
            endpoint_override: None,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
        }
    }

    #[cfg(test)]
    pub fn with_endpoint(routing: Arc<dyn QoderRouting>, endpoint: String) -> Self {
        Self {
            endpoint_override: Some(endpoint),
            ..Self::new(routing)
        }
    }

    async fn run(
        &self,
        credential: &Credential,
        request: &ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<Completion, UpstreamError> {
        let identity = self.routing.identity(&credential.label).ok_or_else(|| {
            UpstreamError::new(
                UpstreamKind::BadRequest,
                400,
                "找不到这个 Qoder 账号的身份。重新导入一次 PAT。",
            )
        })?;
        let model = match protocol::resolve_model(identity.backend, &request.model) {
            Some(model) => model,
            None if protocol::known_model(&request.model) => {
                return Err(UpstreamError::new(
                    UpstreamKind::ModelUnsupported,
                    404,
                    format!(
                        "这个 {} 账号没有模型 {}。换一个 {} 的号，或者换一个它目录里的模型。",
                        identity.backend.product(),
                        request.model,
                        other_region(identity.backend)
                    ),
                ));
            }
            None => {
                return Err(UpstreamError::new(
                    UpstreamKind::BadRequest,
                    400,
                    format!("Qoder 不认识模型 {}。", request.model),
                ));
            }
        };

        let body = chat::build_body(
            &model,
            &identity.user_id,
            request.conversation_id.as_deref().unwrap_or(""),
            &turns_of(request),
            &tools_of(request),
            request.sampling.max_output_tokens,
        );
        let plaintext = serde_json::to_vec(&body).map_err(|e| {
            UpstreamError::new(
                UpstreamKind::BadRequest,
                400,
                format!("请求体无法序列化：{e}"),
            )
        })?;
        let encoded = cosy::encode_body(&plaintext);
        let url = self
            .endpoint_override
            .clone()
            .unwrap_or_else(|| protocol::chat_url(identity.backend));
        let headers = cosy::auth_headers(
            &encoded,
            &url,
            &CosyIdentity {
                user_id: &identity.user_id,
                auth_token: &credential.access_token,
                name: &identity.name,
                email: &identity.email,
                machine_id: &identity.machine_id,
            },
        )
        .map_err(|message| UpstreamError::new(UpstreamKind::BadRequest, 400, message))?;

        let mut req = self
            .client
            .post(&url)
            .timeout(self.max_turn)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .header(reqwest::header::ACCEPT_ENCODING, "identity")
            .header("Cache-Control", "no-cache")
            .header("X-Model-Key", &model.key)
            .header("X-Model-Source", "system")
            .body(encoded);
        for (k, v) in headers {
            req = req.header(k, v);
        }

        let res = match req.send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                return Err(UpstreamError::new(
                    UpstreamKind::Timeout,
                    504,
                    format!("连 Qoder 超时：{e}"),
                ));
            }
            Err(e) => {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    502,
                    format!("连不上 Qoder：{e}"),
                ));
            }
        };
        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            let text = res.text().await.unwrap_or_default();
            if is_auth(status, &text) {
                self.routing.invalidate(&credential.label);
            }
            return Err(map_status(status, &text));
        }
        on_delta(Delta::Headers(Vec::new()));
        match read_sse(res, Instant::now(), self.idle_timeout, &model, on_delta).await {
            Err(err) if err.kind == UpstreamKind::Auth => {
                self.routing.invalidate(&credential.label);
                Err(err)
            }
            other => other,
        }
    }
}

impl Upstream for QoderUpstream {
    fn stream<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ChatRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
        Box::pin(self.run(credential, request, on_delta))
    }
}

fn other_region(backend: QoderBackend) -> &'static str {
    match backend {
        QoderBackend::Global => "国内版",
        QoderBackend::Cn => "国际版",
    }
}

fn turns_of(request: &ChatRequest) -> Vec<InTurn> {
    request.messages.iter().flat_map(turn_of).collect()
}

fn turn_of(message: &Message) -> Vec<InTurn> {
    match message.role {
        Role::System => vec![InTurn {
            role: InRole::System,
            text: message.text.clone(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_call_id: String::new(),
        }],
        Role::User => vec![InTurn {
            role: InRole::User,
            text: message.text.clone(),
            images: message
                .images
                .iter()
                .map(|img| InImage {
                    mime: img.mime_type.clone(),
                    data: img.data.clone(),
                })
                .collect(),
            tool_calls: Vec::new(),
            tool_call_id: String::new(),
        }],
        Role::Assistant => vec![InTurn {
            role: InRole::Assistant,
            text: message.text.clone(),
            images: Vec::new(),
            tool_calls: message
                .tool_calls
                .iter()
                .map(|call| InToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                })
                .collect(),
            tool_call_id: String::new(),
        }],
        Role::Tool => {
            if message.tool_results.is_empty() {
                return vec![InTurn {
                    role: InRole::Tool,
                    text: message.text.clone(),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: String::new(),
                }];
            }
            message
                .tool_results
                .iter()
                .map(|result| InTurn {
                    role: InRole::Tool,
                    text: result.text.clone(),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_call_id: result.tool_call_id.clone(),
                })
                .collect()
        }
    }
}

fn tools_of(request: &ChatRequest) -> Vec<InTool> {
    request
        .tools
        .iter()
        .map(|tool| InTool {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.parameters.clone(),
        })
        .collect()
}

struct Collected {
    text: String,
    thinking: String,
    calls: BTreeMap<usize, ToolAcc>,
    usage: Usage,
    usage_measured: bool,
    finish: Option<String>,
    ttft: Option<u64>,
    splitter: ThinkingSplit,
    failed: Option<String>,
    auth_failed: bool,
}

struct ToolAcc {
    id: String,
    name: String,
    arguments: String,
}

async fn read_sse(
    mut res: reqwest::Response,
    started: Instant,
    idle: Duration,
    model: &ResolvedModel,
    on_delta: DeltaSink<'_>,
) -> Result<Completion, UpstreamError> {
    let mut decoder = SseDecoder::default();
    let mut collected = Collected {
        text: String::new(),
        thinking: String::new(),
        calls: BTreeMap::new(),
        usage: Usage::default(),
        usage_measured: false,
        finish: None,
        ttft: None,
        splitter: ThinkingSplit::default(),
        failed: None,
        auth_failed: false,
    };
    loop {
        let chunk = match tokio::time::timeout(idle, res.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    502,
                    format!("Qoder 流中断：{e}"),
                ));
            }
            Err(_) => {
                return Err(UpstreamError::new(
                    UpstreamKind::Timeout,
                    504,
                    format!("Qoder {} 秒没有动静", idle.as_secs()),
                ));
            }
        };
        let mut done = false;
        for ev in decoder.push(&chunk) {
            if handle_event(&ev, &mut collected, started, on_delta) {
                done = true;
            }
        }
        if done {
            break;
        }
    }
    for ev in decoder.finish() {
        handle_event(&ev, &mut collected, started, on_delta);
    }
    for piece in collected.splitter.finish() {
        apply_split(piece, &mut collected, started, on_delta);
    }
    if collected.auth_failed {
        return Err(UpstreamError::new(
            UpstreamKind::Auth,
            401,
            collected
                .failed
                .unwrap_or_else(|| "Qoder 登录态失效了。".into()),
        ));
    }
    if let Some(msg) = collected.failed {
        return Err(map_status(502, &msg));
    }
    let tool_calls = collected
        .calls
        .into_values()
        .filter(|call| !call.name.is_empty() || !call.id.is_empty())
        .map(|call| ToolCall {
            id: if call.id.is_empty() {
                format!("call_{}", &uuid::Uuid::new_v4().simple().to_string()[..24])
            } else {
                call.id
            },
            name: call.name,
            arguments: if call.arguments.is_empty() {
                "{}".into()
            } else {
                call.arguments
            },
        })
        .collect::<Vec<_>>();
    let finish = if !tool_calls.is_empty() {
        FinishReason::ToolCalls
    } else {
        match collected.finish.as_deref() {
            Some("length") => FinishReason::Length,
            Some("content_filter") => FinishReason::ContentFilter,
            Some("tool_calls") | Some("tool_use") => FinishReason::ToolCalls,
            _ => FinishReason::Stop,
        }
    };
    if collected.usage.output_tokens == 0 && !collected.text.is_empty() {
        collected.usage.output_tokens = crate::normalized::estimate_tokens(&collected.text);
    }
    Ok(Completion {
        text: collected.text,
        thinking: collected.thinking,
        tool_calls,
        finish_reason: finish,
        usage: collected.usage,
        usage_measured: collected.usage_measured,
        routed_model: Some(model.key.clone()),
        ttft_ms: collected.ttft,
        turn_ms: started.elapsed().as_millis() as u64,
        raw_response: None,
    })
}

/// 返回 true 表示流结束，不必再读。
fn handle_event(
    ev: &SseEvent,
    collected: &mut Collected,
    started: Instant,
    on_delta: DeltaSink<'_>,
) -> bool {
    let mut done = false;
    for piece in chat::parse_data(&ev.data) {
        match piece {
            StreamPiece::Text(text) => {
                for split in collected.splitter.push(&text) {
                    apply_split(split, collected, started, on_delta);
                }
            }
            StreamPiece::Thinking(text) => {
                note_ttft(collected, started, on_delta, Delta::Thinking(text.clone()))
            }
            StreamPiece::Tool {
                index,
                id,
                name,
                arguments,
            } => {
                let call = collected.calls.entry(index).or_insert_with(|| ToolAcc {
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
                if let Some(id) = id {
                    if !id.is_empty() {
                        call.id = id;
                    }
                }
                if let Some(name) = name {
                    if !name.is_empty() {
                        call.name = name;
                    }
                }
                call.arguments.push_str(&arguments);
            }
            StreamPiece::Usage(usage) => {
                collected.usage = Usage {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cache_read_tokens: usage.cache_read_tokens,
                    cache_write_tokens: usage.cache_write_tokens,
                    reasoning_tokens: usage.reasoning_tokens,
                };
                collected.usage_measured = true;
            }
            StreamPiece::Finish(reason) => collected.finish = Some(reason),
            StreamPiece::Done => done = true,
            StreamPiece::Failed(message) => {
                collected.auth_failed = message_says_login_expired(&message);
                collected.failed = Some(message);
                done = true;
            }
        }
    }
    done
}

fn apply_split(
    split: SplitOut,
    collected: &mut Collected,
    started: Instant,
    on_delta: DeltaSink<'_>,
) {
    match split {
        SplitOut::Text(text) => {
            if text.is_empty() {
                return;
            }
            collected.text.push_str(&text);
            note_ttft(collected, started, on_delta, Delta::Text(text));
        }
        SplitOut::Thinking(text) => {
            if text.is_empty() {
                return;
            }
            collected.thinking.push_str(&text);
            note_ttft(collected, started, on_delta, Delta::Thinking(text));
        }
    }
}

fn note_ttft(collected: &mut Collected, started: Instant, on_delta: DeltaSink<'_>, delta: Delta) {
    if collected.ttft.is_none() {
        collected.ttft = Some(started.elapsed().as_millis() as u64);
    }
    on_delta(delta);
}

fn message_says_login_expired(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("login expired") || lower.contains("unauthorized") || text.contains("登录")
}

fn is_auth(status: u16, text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    status == 401
        || lower.contains("login expired")
        || lower.contains("unauthorized")
        || lower.contains("invalid token")
}

fn map_status(status: u16, text: &str) -> UpstreamError {
    let message = clip(text);
    let kind = if is_auth(status, text) {
        UpstreamKind::Auth
    } else if status == 403 {
        UpstreamKind::Forbidden
    } else if status == 402 || message.to_ascii_lowercase().contains("quota") {
        UpstreamKind::Quota
    } else if status == 429 {
        UpstreamKind::RateLimit
    } else if status == 404 {
        UpstreamKind::ModelUnsupported
    } else if status == 400 {
        UpstreamKind::BadRequest
    } else {
        UpstreamKind::Upstream
    };
    let http = match kind {
        UpstreamKind::Auth => 401,
        UpstreamKind::Forbidden => 403,
        UpstreamKind::Quota => 402,
        UpstreamKind::RateLimit => 429,
        UpstreamKind::ModelUnsupported => 404,
        UpstreamKind::BadRequest => 400,
        UpstreamKind::Timeout => 504,
        _ => 502,
    };
    UpstreamError::new(
        kind,
        http,
        if message.is_empty() {
            format!("Qoder 返回 {status}")
        } else {
            format!("Qoder：{message}")
        },
    )
}

fn clip(text: &str) -> String {
    text.chars().take(300).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalized::Role;
    use nexus_qoder::model::QoderBackend;

    fn identity() -> QoderIdentity {
        QoderIdentity {
            backend: QoderBackend::Global,
            user_id: "user-1".into(),
            name: "Ada".into(),
            email: "ada@example.com".into(),
            machine_id: "machine-1".into(),
        }
    }

    #[tokio::test]
    async fn a_signed_request_reads_the_sse_envelope() {
        let app = axum::Router::new().route(
            "/algo/api/v2/service/pro/sse/agent_chat_generation",
            axum::routing::post(|headers: axum::http::HeaderMap, body: axum::body::Bytes| async move {
                assert!(headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .starts_with("Bearer COSY."));
                assert!(!body.is_empty());
                let payload = "\
data: {\"statusCodeValue\":200,\"body\":\"{\\\"choices\\\":[{\\\"delta\\\":{\\\"content\\\":\\\"hello\\\"}}]}\"}\n\
\n\
data: [DONE]\n\
\n\
";
                axum::response::Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(axum::body::Body::from(payload))
                    .unwrap()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let upstream = QoderUpstream::with_endpoint(
            Arc::new(FixedIdentity(identity())),
            format!("http://{addr}/algo/api/v2/service/pro/sse/agent_chat_generation?Encode=1"),
        );
        let request = ChatRequest {
            model: "Qwen3.8-Max".into(),
            messages: vec![Message::text(Role::User, "hi")],
            ..ChatRequest::default()
        };
        let credential = Credential {
            label: "ada".into(),
            access_token: "jt-test".into(),
            identity: crate::identity::DeviceIdentity::derived("jt-test"),
        };
        let mut deltas = Vec::new();
        let done = upstream
            .run(&credential, &request, &mut |d| deltas.push(d))
            .await
            .unwrap();
        assert_eq!(done.text, "hello");
        assert!(deltas
            .iter()
            .any(|d| matches!(d, Delta::Text(t) if t == "hello")));
        assert_eq!(done.routed_model.as_deref(), Some("qmodel_preview"));
    }

    #[tokio::test]
    async fn a_cn_only_model_on_a_global_account_is_unsupported() {
        let upstream = QoderUpstream::new(Arc::new(FixedIdentity(identity())));
        let request = ChatRequest {
            model: "Qwen3.6-Flash".into(),
            messages: vec![Message::text(Role::User, "hi")],
            ..ChatRequest::default()
        };
        let credential = Credential {
            label: "ada".into(),
            access_token: "jt-test".into(),
            identity: crate::identity::DeviceIdentity::derived("jt-test"),
        };
        let err = upstream
            .run(&credential, &request, &mut |_| {})
            .await
            .unwrap_err();
        assert_eq!(err.kind, UpstreamKind::ModelUnsupported);
    }
}
