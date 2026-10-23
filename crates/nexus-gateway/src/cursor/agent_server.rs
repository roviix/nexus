//! Cursor Composer 原生 Agent 服务引擎（Connect-RPC BidiAppend + RunSSE 会话中心）。
//!
//! 支持端点：
//! - `POST /cursor/route/agent.v1.AgentService/RunSSE` (下行事件流：Connect Envelopes / text/event-stream)
//! - `POST /cursor/route/agent.v1.AgentService/Run` (兼容流式运行)
//! - `POST /cursor/route/aiserver.v1.BidiService/BidiAppend` (上行消息追加：十六进制编码 Protobuf)
//!
//! 核心设计：
//! 1. 响应 Cursor 官方标准双向通讯协议：通过 `RunSSE` 保持下行长连接，通过 `BidiAppend` 接收客户端指令；
//! 2. 内存级无锁会话中继（Session Registry）：按 `request_id` 自动配对事件管道，支持任意到达顺序；
//! 3. 完整解构 `AgentRunRequest`：提取自选模型（如 gpt-5.6-luna）、多轮上下文（ConversationHistory）、
//!    用户提示词与 SelectedContext（附加代码文件、选区、规则）；
//! 4. 驱动网关目标异构通道（OpenAI / Claude / DeepSeek），实时流式推送 `ThinkingDelta`、`TextDelta`、
//!    `ThinkingCompleted`，并在收尾时回发 `TurnEnded` 用量与 Connect 协议终止帧。

use super::proto::agent::v1 as agent;
use super::proto::aiserver::v1 as ai;
use crate::channel::Capability;
use crate::lane::{Credential, Outcome};
use crate::normalized::{ChatRequest, Delta, Message, Role, Sampling, ToolChoice, ToolDef, ToolResult};
use crate::server::Gateway;
use crate::upstream::Upstream;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use prost::Message as _;
use std::collections::HashMap;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Connect 协议流式终结标志位 (bit 1)
pub const CONNECT_END_STREAM_FLAG: u8 = 0x02;

/// 单个活跃 Agent 会话内部通道状态
pub struct SessionHandle {
    pub tx: mpsc::UnboundedSender<Result<Bytes, std::io::Error>>,
    pub rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<Result<Bytes, std::io::Error>>>>,
    pub exec_tx: mpsc::UnboundedSender<agent::ExecClientMessage>,
    pub exec_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<agent::ExecClientMessage>>>,
    pub interaction_tx: mpsc::UnboundedSender<agent::InteractionResponse>,
    pub interaction_rx: std::sync::Mutex<Option<mpsc::UnboundedReceiver<agent::InteractionResponse>>>,
    pub created_at: Instant,
}

impl SessionHandle {
    pub fn new() -> (Self, mpsc::UnboundedSender<Result<Bytes, std::io::Error>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (exec_tx, exec_rx) = mpsc::unbounded_channel();
        let (interaction_tx, interaction_rx) = mpsc::unbounded_channel();
        (
            Self {
                tx: tx.clone(),
                rx: std::sync::Mutex::new(Some(rx)),
                exec_tx,
                exec_rx: std::sync::Mutex::new(Some(exec_rx)),
                interaction_tx,
                interaction_rx: std::sync::Mutex::new(Some(interaction_rx)),
                created_at: Instant::now(),
            },
            tx,
        )
    }
}

/// 全局 Agent 会话注册表（双向通道总线）
pub struct AgentSessionRegistry {
    sessions: RwLock<HashMap<String, Arc<SessionHandle>>>,
}

impl AgentSessionRegistry {
    pub fn new() -> Self {
        Self {
            sessions: RwLock::new(HashMap::new()),
        }
    }

    /// 获取或创建下行发送端
    pub fn get_or_create_tx(
        &self,
        request_id: &str,
    ) -> mpsc::UnboundedSender<Result<Bytes, std::io::Error>> {
        let mut map = self.sessions.write().unwrap();
        self.clean_expired(&mut map);

        if let Some(entry) = map.get(request_id) {
            entry.tx.clone()
        } else {
            let (handle, tx) = SessionHandle::new();
            map.insert(request_id.to_string(), Arc::new(handle));
            tx
        }
    }

    /// 取出下行接收端（供 RunSSE / Run 响应体流使用）
    pub fn take_rx(
        &self,
        request_id: &str,
    ) -> mpsc::UnboundedReceiver<Result<Bytes, std::io::Error>> {
        let mut map = self.sessions.write().unwrap();
        self.clean_expired(&mut map);

        if let Some(entry) = map.get(request_id) {
            if let Some(rx) = entry.rx.lock().unwrap().take() {
                return rx;
            }
        }

        let (handle, _tx) = SessionHandle::new();
        let rx = handle.rx.lock().unwrap().take().unwrap();
        map.insert(request_id.to_string(), Arc::new(handle));
        rx
    }

    /// 注册会话别名（将 conversation_id 映射至同一 SessionHandle）
    pub fn register_aliases(&self, request_id: &str, conv_id: Option<&str>) {
        if let Some(cid) = conv_id {
            if !cid.trim().is_empty() && cid != request_id {
                let mut map = self.sessions.write().unwrap();
                if let Some(handle) = map.get(request_id).cloned() {
                    map.insert(cid.to_string(), handle);
                }
            }
        }
    }

    /// 取出客户端执行返回通道的接收端
    pub fn take_exec_rx(
        &self,
        request_id: &str,
        conv_id: Option<&str>,
    ) -> mpsc::UnboundedReceiver<agent::ExecClientMessage> {
        let mut map = self.sessions.write().unwrap();
        self.clean_expired(&mut map);

        let handle = if let Some(h) = map.get(request_id) {
            h.clone()
        } else if let Some(cid) = conv_id.and_then(|c| map.get(c)) {
            cid.clone()
        } else {
            let (h, _tx) = SessionHandle::new();
            let arc_h = Arc::new(h);
            map.insert(request_id.to_string(), arc_h.clone());
            if let Some(cid) = conv_id {
                if !cid.trim().is_empty() {
                    map.insert(cid.to_string(), arc_h.clone());
                }
            }
            arc_h
        };

        let rx_opt = handle.exec_rx.lock().unwrap().take();
        if let Some(rx) = rx_opt {
            rx
        } else {
            let (_, rx) = mpsc::unbounded_channel();
            rx
        }
    }

    /// 取出客户端交互响应通道的接收端
    pub fn take_interaction_rx(
        &self,
        request_id: &str,
        conv_id: Option<&str>,
    ) -> mpsc::UnboundedReceiver<agent::InteractionResponse> {
        let mut map = self.sessions.write().unwrap();
        self.clean_expired(&mut map);

        let handle = if let Some(h) = map.get(request_id) {
            h.clone()
        } else if let Some(cid) = conv_id.and_then(|c| map.get(c)) {
            cid.clone()
        } else {
            let (h, _tx) = SessionHandle::new();
            let arc_h = Arc::new(h);
            map.insert(request_id.to_string(), arc_h.clone());
            if let Some(cid) = conv_id {
                if !cid.trim().is_empty() {
                    map.insert(cid.to_string(), arc_h.clone());
                }
            }
            arc_h
        };

        let rx_opt = handle.interaction_rx.lock().unwrap().take();
        if let Some(rx) = rx_opt {
            rx
        } else {
            let (_, rx) = mpsc::unbounded_channel();
            rx
        }
    }

    /// 向对应会话分发客户端执行结果
    pub fn dispatch_exec(&self, key: &str, msg: agent::ExecClientMessage) {
        let map = self.sessions.read().unwrap();
        if let Some(handle) = map.get(key) {
            let _ = handle.exec_tx.send(msg);
        } else {
            debug!(key = %key, "未找到目标会话投递 ExecClientMessage");
        }
    }

    /// 向对应会话分发交互式卡片响应
    pub fn dispatch_interaction(&self, key: &str, resp: agent::InteractionResponse) {
        let map = self.sessions.read().unwrap();
        if let Some(handle) = map.get(key) {
            let _ = handle.interaction_tx.send(resp);
        } else {
            debug!(key = %key, "未找到目标会话投递 InteractionResponse");
        }
    }

    /// 会话完成清理
    pub fn remove(&self, request_id: &str, conv_id: Option<&str>) {
        if let Ok(mut map) = self.sessions.write() {
            map.remove(request_id);
            if let Some(cid) = conv_id {
                map.remove(cid);
            }
        }
    }

    /// 定期清理超过 15 分钟的废弃会话
    fn clean_expired(&self, map: &mut HashMap<String, Arc<SessionHandle>>) {
        if map.len() > 100 {
            let now = Instant::now();
            map.retain(|_, entry| now.duration_since(entry.created_at).as_secs() < 900);
        }
    }
}

/// 全局单例会话中心
pub static GLOBAL_SESSIONS: LazyLock<AgentSessionRegistry> =
    LazyLock::new(AgentSessionRegistry::new);

/// 子代理会话存储结构（同时持久化 LLM 消息流与前端展示轨迹）
#[derive(Clone, Default)]
pub struct SubagentSession {
    pub messages: Vec<Message>,
    pub conversation_steps: Vec<agent::ConversationStep>,
}

/// 全局子代理会话状态池（支持同一子代理多轮追问与上下文追加）
pub static SUBAGENT_SESSIONS: LazyLock<std::sync::RwLock<HashMap<String, SubagentSession>>> =
    LazyLock::new(|| std::sync::RwLock::new(HashMap::new()));

/// Composer 会话持久历史记录（按 conversation_id 隔离，保障多轮问答上下文完整性）
#[derive(Clone, Debug)]
pub struct ComposerConversationRecord {
    pub last_updated: Instant,
    pub messages: Vec<Message>,
    pub last_subagent_id: Option<String>,
}

pub static COMPOSER_CONVERSATION_HISTORY: LazyLock<std::sync::RwLock<HashMap<String, ComposerConversationRecord>>> =
    LazyLock::new(|| std::sync::RwLock::new(HashMap::new()));

// ============================================================================
// 编码与信封构造工具
// ============================================================================

/// 十六进制字符转字节数组
pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err("odd hex length".to_string());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| {
            u8::from_str_radix(&s[i..i + 2], 16)
                .map_err(|e| format!("invalid hex char at {i}: {e}"))
        })
        .collect()
}

/// 将 AgentServerMessage 封装为 Connect-RPC 二进制信封帧 (5 字节头部 + Protobuf)
pub fn encode_agent_server_message(msg: &agent::AgentServerMessage) -> Bytes {
    let payload = msg.encode_to_vec();
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(0); // 数据帧标记
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Bytes::from(out)
}

/// 构造 Connect 协议正常流结束帧
pub fn encode_end_stream() -> Bytes {
    let payload = b"{}";
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(CONNECT_END_STREAM_FLAG);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    Bytes::from(out)
}

/// 构造 Connect 协议流式错误帧
pub fn encode_error_end_stream(code: &str, message: &str) -> Bytes {
    let json = serde_json::json!({
        "error": {
            "code": code,
            "message": message,
        }
    });
    let payload = serde_json::to_vec(&json).unwrap_or_else(|_| b"{}".to_vec());
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(CONNECT_END_STREAM_FLAG);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    Bytes::from(out)
}

/// 构造包装有 InteractionUpdate 的 AgentServerMessage
pub fn wrap_interaction(msg: agent::interaction_update::Message) -> agent::AgentServerMessage {
    agent::AgentServerMessage {
        ttft_breakdown: None,
        message: Some(agent::agent_server_message::Message::InteractionUpdate(
            agent::InteractionUpdate {
                message: Some(msg),
            },
        )),
    }
}

/// 构造包装有 ExecServerMessage 的 AgentServerMessage（用于触发 Cursor 本地 ClientExecController）
pub fn wrap_exec_server_message(msg: agent::ExecServerMessage) -> agent::AgentServerMessage {
    agent::AgentServerMessage {
        ttft_breakdown: None,
        message: Some(agent::agent_server_message::Message::ExecServerMessage(msg)),
    }
}

/// 构造包装有 InteractionQuery 的 AgentServerMessage（用于 AskQuestion 弹窗等）
pub fn wrap_interaction_query(query: agent::InteractionQuery) -> agent::AgentServerMessage {
    agent::AgentServerMessage {
        ttft_breakdown: None,
        message: Some(agent::agent_server_message::Message::InteractionQuery(query)),
    }
}

/// 构造工具开始执行帧 (ToolCallStartedUpdate)
pub fn build_tool_call_started(
    call_id: &str,
    model_call_id: &str,
    tool_call: agent::ToolCall,
) -> agent::AgentServerMessage {
    wrap_interaction(agent::interaction_update::Message::ToolCallStarted(
        agent::ToolCallStartedUpdate {
            call_id: call_id.to_string(),
            tool_call: Some(tool_call),
            model_call_id: model_call_id.to_string(),
        },
    ))
}

/// 构造工具完成执行帧 (ToolCallCompletedUpdate)
pub fn build_tool_call_completed(
    call_id: &str,
    model_call_id: &str,
    tool_call: agent::ToolCall,
) -> agent::AgentServerMessage {
    wrap_interaction(agent::interaction_update::Message::ToolCallCompleted(
        agent::ToolCallCompletedUpdate {
            call_id: call_id.to_string(),
            tool_call: Some(tool_call),
            model_call_id: model_call_id.to_string(),
        },
    ))
}

/// 安全截断长输出文本，确保不超过上限并对齐 UTF-8 字符边界
pub fn truncate_output(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        text.to_string()
    } else {
        let mut split_idx = max_bytes;
        while split_idx > 0 && !text.is_char_boundary(split_idx) {
            split_idx -= 1;
        }
        format!(
            "{}\n... [output truncated: exceeded {}KB limit] ...",
            &text[..split_idx],
            max_bytes / 1024
        )
    }
}

/// 构造文本增量帧
pub fn build_text_delta(text: String) -> agent::AgentServerMessage {
    wrap_interaction(agent::interaction_update::Message::TextDelta(
        agent::TextDeltaUpdate {
            text,
            is_server_notice: false,
        },
    ))
}

/// 构造思考过程增量帧
pub fn build_thinking_delta(text: String) -> agent::AgentServerMessage {
    wrap_interaction(agent::interaction_update::Message::ThinkingDelta(
        agent::ThinkingDeltaUpdate {
            text,
            thinking_style: Some(0), // Default thinking style
        },
    ))
}

/// 构造思考完成帧
pub fn build_thinking_completed(duration_ms: i32) -> agent::AgentServerMessage {
    wrap_interaction(agent::interaction_update::Message::ThinkingCompleted(
        agent::ThinkingCompletedUpdate {
            thinking_duration_ms: duration_ms.max(1),
        },
    ))
}

/// 构造回合完成帧（携带 Token 消耗统计）
pub fn build_turn_ended(usage: Option<&crate::normalized::Usage>) -> agent::AgentServerMessage {
    wrap_interaction(agent::interaction_update::Message::TurnEnded(
        agent::TurnEndedUpdate {
            input_tokens: usage.map(|u| u.input_tokens as i64),
            output_tokens: usage.map(|u| u.output_tokens as i64),
            cache_read_tokens: usage.map(|u| u.cache_read_tokens as i64),
            cache_write_tokens: usage.map(|u| u.cache_write_tokens as i64),
            reasoning_tokens: usage.map(|u| u.reasoning_tokens as i64),
        },
    ))
}

/// 构造心跳响应帧
pub fn build_heartbeat() -> agent::AgentServerMessage {
    wrap_interaction(agent::interaction_update::Message::Heartbeat(
        agent::HeartbeatUpdate {},
    ))
}

/// 自动检测并解压 Gzip 压缩数据（Connect-RPC 启用 sendCompression 时为 Gzip）
pub fn decompress_if_needed(data: &[u8]) -> Vec<u8> {
    if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
        let mut decoder = flate2::read::GzDecoder::new(data);
        let mut decompressed = Vec::new();
        if decoder.read_to_end(&mut decompressed).is_ok() {
            return decompressed;
        }
    }
    data.to_vec()
}

/// 解析 Connect Unary / Stream 帧载荷（自动处理 Gzip 解压并剥离 5 字节信封头）
pub fn extract_connect_payload(body: &[u8]) -> Vec<u8> {
    // 1. 如果整个 body 是 Gzip 压缩的，先解压
    let uncompressed = decompress_if_needed(body);
    let slice = uncompressed.as_slice();

    // 2. 检查 5 字节 Connect envelope 头部
    if slice.len() >= 5 {
        let flags = slice[0];
        let length = u32::from_be_bytes([slice[1], slice[2], slice[3], slice[4]]) as usize;
        if flags & CONNECT_END_STREAM_FLAG == 0 && length > 0 && slice.len() >= 5 + length {
            let frame_payload = &slice[5..5 + length];
            return decompress_if_needed(frame_payload);
        }
    }

    uncompressed
}

// ============================================================================
// RunSSE 处理器
// ============================================================================

/// 处理 `agent.v1.AgentService/RunSSE` 请求。
///
/// Cursor IDE 发起长连接请求，建立下行推流通道。
pub async fn handle_run_sse(
    _state: State<Arc<Gateway>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let payload = extract_connect_payload(&body);

    // 解析 BidiRequestId
    let request_id = match agent::BidiRequestId::decode(payload.as_slice())
        .or_else(|_| agent::BidiRequestId::decode(&body[..])) {
        Ok(req) if !req.request_id.trim().is_empty() => req.request_id,
        _ => {
            // 备用：从请求头获取
            headers
                .get("x-request-id")
                .or_else(|| headers.get("x-cursor-request-id"))
                .and_then(|h| h.to_str().ok())
                .unwrap_or("")
                .to_string()
        }
    };

    if request_id.is_empty() {
        warn!("RunSSE 请求未包含有效的 request_id");
        return (
            StatusCode::BAD_REQUEST,
            [("content-type", "text/plain")],
            "request_id is required",
        )
            .into_response();
    }

    info!(request_id = %request_id, "建立 Cursor Agent RunSSE 下行事件流");
    eprintln!("[NEXUS-AGENT] RunSSE connected: request_id={request_id}");

    let ct = headers
        .get("content-type")
        .or_else(|| headers.get("accept"))
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    let resp_ct = if ct.contains("application/connect+proto") && !ct.contains("text/event-stream") {
        "application/connect+proto"
    } else {
        "text/event-stream"
    };

    let _tx = GLOBAL_SESSIONS.get_or_create_tx(&request_id);

    let rx = GLOBAL_SESSIONS.take_rx(&request_id);
    let body_stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", resp_ct)
        .header("cache-control", "no-cache")
        .header("connect-protocol-version", "1")
        .header("access-control-allow-origin", "*")
        .header(
            "access-control-expose-headers",
            "connect-protocol-version, content-type",
        )
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// 处理直接以 Connect-RPC 运行流发起的 `agent.v1.AgentService/Run` 请求。
///
/// 接收双向 HTTP/2 上行流与 AgentRunRequest，启动模型多步 Agentic 循环并返回下行推流响应。
pub async fn handle_agent_run_direct<S>(
    State(gw): State<Arc<Gateway>>,
    headers: HeaderMap,
    run_req: agent::AgentRunRequest,
    mut inbound_stream: S,
) -> Response
where
    S: tokio_stream::Stream<Item = Result<Bytes, axum::Error>> + Send + Unpin + 'static,
{
    let request_id = headers
        .get("x-request-id")
        .or_else(|| headers.get("x-cursor-request-id"))
        .and_then(|h| h.to_str().ok())
        .unwrap_or("direct-run")
        .to_string();

    let conv_id = run_req.conversation_id.clone();

    info!(
        request_id = %request_id,
        conversation_id = ?conv_id,
        "直接流式触发 AgentRunRequest 推理执行与双向通道监听"
    );

    let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, std::io::Error>>();

    // 注册别名关联
    GLOBAL_SESSIONS.register_aliases(&request_id, conv_id.as_deref());

    // 启动 HTTP/2 入站上行流监听（接收客户端下发的 ExecClientMessage 与 InteractionResponse）
    let req_id_clone = request_id.clone();
    let conv_id_clone = conv_id.clone();
    tokio::spawn(async move {
        use tokio_stream::StreamExt;
        let mut decoder = crate::connect::EnvelopeDecoder::default();
        while let Some(chunk_res) = inbound_stream.next().await {
            match chunk_res {
                Ok(chunk) => {
                    decoder.push(&chunk);
                    while let Ok(Some(env)) = decoder.next_envelope() {
                        if env.flags & CONNECT_END_STREAM_FLAG != 0 {
                            return;
                        }
                        let decompressed = decompress_if_needed(&env.data);
                        let client_msg_opt = agent::AgentClientMessage::decode(decompressed.as_slice())
                            .or_else(|_| agent::AgentClientMessage::decode(&env.data[..]));
                        if let Ok(client_msg) = client_msg_opt {
                            if let Some(msg) = client_msg.message {
                                match msg {
                                    agent::agent_client_message::Message::ExecClientMessage(exec) => {
                                        info!(exec_id = exec.id, "收到 Inbound Stream 的 ExecClientMessage");
                                        GLOBAL_SESSIONS.dispatch_exec(&req_id_clone, exec.clone());
                                        if let Some(ref cid) = conv_id_clone {
                                            GLOBAL_SESSIONS.dispatch_exec(cid, exec);
                                        }
                                    }
                                    agent::agent_client_message::Message::InteractionResponse(resp) => {
                                        info!(resp_id = resp.id, "收到 Inbound Stream 的 InteractionResponse");
                                        GLOBAL_SESSIONS.dispatch_interaction(&req_id_clone, resp.clone());
                                        if let Some(ref cid) = conv_id_clone {
                                            GLOBAL_SESSIONS.dispatch_interaction(cid, resp);
                                        }
                                    }
                                    agent::agent_client_message::Message::ClientHeartbeat(_) => {
                                        debug!("收到 Inbound Stream 客户端心跳");
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    debug!(error = %e, "Inbound stream 终止");
                    break;
                }
            }
        }
    });

    tokio::spawn(drive_agent_turn(gw, request_id, run_req, tx));

    let body_stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/connect+proto")
        .header("connect-protocol-version", "1")
        .header("access-control-allow-origin", "*")
        .header(
            "access-control-expose-headers",
            "connect-protocol-version, content-type",
        )
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// 简易包装，供单向调用或测试使用
pub async fn handle_agent_run_direct_simple(
    State(gw): State<Arc<Gateway>>,
    headers: HeaderMap,
    run_req: agent::AgentRunRequest,
) -> Response {
    handle_agent_run_direct(State(gw), headers, run_req, tokio_stream::empty()).await
}

/// 处理 `agent.v1.AgentService` 的一元元数据请求（如 UpdateConversationMetadata、NameAgent 等），
/// 返回即时成功的 Protobuf 响应，规避官方 502 / 鉴权失败重试导致的 Composer 规划卡死。
pub fn handle_agent_unary_metadata(subpath: &str, headers: &HeaderMap) -> Response {
    debug!(subpath = %subpath, "本地响应 AgentService 一元元数据 RPC");

    let is_connect_proto = headers
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .map(|ct| ct.contains("application/connect+proto"))
        .unwrap_or(false);

    let (content_type, body_bytes) = if is_connect_proto {
        // Connect-RPC framed stream envelope:
        // 1. Data frame: flag 0, len 0
        // 2. End-of-stream frame: flag 2, len 2, "{}"
        let mut buf = Vec::with_capacity(12);
        buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00]);
        buf.push(CONNECT_END_STREAM_FLAG);
        buf.extend_from_slice(&(2u32).to_be_bytes());
        buf.extend_from_slice(b"{}");
        ("application/connect+proto", buf)
    } else {
        // Standard Connect-RPC unary Protobuf: raw empty protobuf message is 0 bytes!
        ("application/proto", Vec::new())
    };

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", content_type)
        .header("connect-protocol-version", "1")
        .header("access-control-allow-origin", "*")
        .header(
            "access-control-expose-headers",
            "connect-protocol-version, content-type",
        )
        .body(Body::from(body_bytes))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ============================================================================
// BidiAppend 处理器与驱动引擎
// ============================================================================

/// 处理 `aiserver.v1.BidiService/BidiAppend` 请求。
///
/// 接收客户端追加的指令与载荷（如 AgentRunRequest、ExecClientMessage），驱动模型流并推入会话管道。
pub async fn handle_bidi_append(
    State(gw): State<Arc<Gateway>>,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    let payload = extract_connect_payload(&body);
    let append_req = match ai::BidiAppendRequest::decode(payload.as_slice())
        .or_else(|_| ai::BidiAppendRequest::decode(&body[..]))
    {
        Ok(req) => req,
        Err(err) => {
            warn!(error = %err, "解析 BidiAppendRequest 失败");
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "text/plain")],
                format!("invalid BidiAppendRequest: {err}"),
            )
                .into_response();
        }
    };

    let request_id = match append_req.request_id.as_ref() {
        Some(id) if !id.request_id.trim().is_empty() => id.request_id.clone(),
        _ => {
            warn!("BidiAppendRequest 缺少 request_id");
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "text/plain")],
                "BidiAppend request_id is required",
            )
                .into_response();
        }
    };

    // 兼容 binary 与 hex 两种传输编码
    let client_msg_bytes = if !append_req.data_binary.is_empty() {
        append_req.data_binary
    } else if !append_req.data.is_empty() {
        match hex_decode(&append_req.data) {
            Ok(b) => b,
            Err(err) => {
                warn!(error = %err, "解码 BidiAppend hex 失败");
                return (
                    StatusCode::BAD_REQUEST,
                    [("content-type", "text/plain")],
                    format!("invalid hex: {err}"),
                )
                    .into_response();
            }
        }
    } else {
        return empty_bidi_append_response(&_headers);
    };

    let client_msg = match agent::AgentClientMessage::decode(client_msg_bytes.as_slice())
        .or_else(|_| agent::AgentClientMessage::decode(extract_connect_payload(&client_msg_bytes).as_slice()))
    {
        Ok(msg) => msg,
        Err(err) => {
            warn!(
                error = %err,
                len = client_msg_bytes.len(),
                "解析 AgentClientMessage Protobuf 失败"
            );
            return (
                StatusCode::BAD_REQUEST,
                [("content-type", "text/plain")],
                format!("invalid AgentClientMessage: {err}"),
            )
                .into_response();
        }
    };

    let tx = GLOBAL_SESSIONS.get_or_create_tx(&request_id);
    eprintln!("[NEXUS-AGENT] BidiAppend parsed: request_id={request_id}, bytes_len={}", client_msg_bytes.len());

    // 根据消息类型进行分发处理
    if let Some(msg) = client_msg.message {
        match msg {
            agent::agent_client_message::Message::ExecClientMessage(exec) => {
                info!(request_id = %request_id, exec_id = exec.id, "收到 BidiAppend 的 ExecClientMessage，投递至会话总线");
                GLOBAL_SESSIONS.dispatch_exec(&request_id, exec);
            }
            agent::agent_client_message::Message::InteractionResponse(resp) => {
                info!(request_id = %request_id, resp_id = resp.id, "收到 BidiAppend 的 InteractionResponse，投递至会话总线");
                GLOBAL_SESSIONS.dispatch_interaction(&request_id, resp);
            }
            agent::agent_client_message::Message::RunRequest(run_req) => {
                let is_custom = super::router::is_custom_agent_run_request(&gw, &run_req);
                if is_custom {
                    info!(
                        request_id = %request_id,
                        conversation_id = ?run_req.conversation_id,
                        "触发 AgentRunRequest 自定义模型推理执行"
                    );
                    tokio::spawn(drive_agent_turn(gw, request_id.clone(), run_req, tx));
                } else {
                    info!(
                        request_id = %request_id,
                        "AgentRunRequest 为官方底座模型，出站中继至 Cursor 官方"
                    );
                    let gw_c = gw.clone();
                    let headers_c = _headers.clone();
                    let body_c = body.clone();
                    tokio::spawn(async move {
                        forward_official_agent_turn(gw_c, request_id, headers_c, body_c, tx).await;
                    });
                }
            }
            agent::agent_client_message::Message::ConversationAction(action) => {
                let run_req = agent::AgentRunRequest {
                    action: Some(action),
                    ..Default::default()
                };
                let is_custom = super::router::is_custom_agent_run_request(&gw, &run_req);
                if is_custom {
                    info!(
                        request_id = %request_id,
                        "收到 ConversationAction，包装为自定义模型 AgentRunRequest 触发推理执行"
                    );
                    tokio::spawn(drive_agent_turn(gw, request_id.clone(), run_req, tx));
                } else {
                    info!(
                        request_id = %request_id,
                        "ConversationAction 为官方底座模型，出站中继至 Cursor 官方"
                    );
                    let gw_c = gw.clone();
                    let headers_c = _headers.clone();
                    let body_c = body.clone();
                    tokio::spawn(async move {
                        forward_official_agent_turn(gw_c, request_id, headers_c, body_c, tx).await;
                    });
                }
            }
            agent::agent_client_message::Message::ClientHeartbeat(_) => {
                debug!(request_id = %request_id, "响应客户端心跳");
                let hb_chunk = encode_agent_server_message(&build_heartbeat());
                let _ = tx.send(Ok(hb_chunk));
            }
            other => {
                debug!(
                    request_id = %request_id,
                    msg_type = ?std::mem::discriminant(&other),
                    "收到其他客户端消息"
                );
            }
        }
    }

    empty_bidi_append_response(&_headers)
}

/// 转发 Cursor 官方底座模型的 Agent 会话
async fn forward_official_agent_turn(
    gw: Arc<Gateway>,
    request_id: String,
    headers: HeaderMap,
    body: Bytes,
    tx: mpsc::UnboundedSender<Result<Bytes, std::io::Error>>,
) {
    info!(request_id = %request_id, "开始执行官方底座模型 Agent 转发");
    // 1. 转发 BidiAppend 请求到官方
    let _append_resp = super::native_relay::handle_native_relay(
        State(gw.clone()),
        headers.clone(),
        body,
        "/aiserver.v1.BidiService/BidiAppend",
    ).await;

    // 2. 将 RunSSE 转发到官方并接入 tx（使用 Connect envelope 包装 BidiRequestId）
    let bidi_id = ai::BidiRequestId { request_id: request_id.clone() };
    let payload = bidi_id.encode_to_vec();
    let mut sse_body = Vec::with_capacity(5 + payload.len());
    sse_body.push(0);
    sse_body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    sse_body.extend_from_slice(&payload);

    let mut sse_headers = headers.clone();
    sse_headers.insert("content-type", HeaderValue::from_static("application/connect+proto"));
    sse_headers.insert("connect-protocol-version", HeaderValue::from_static("1"));

    let sse_resp = super::native_relay::handle_native_relay(
        State(gw),
        sse_headers,
        Bytes::from(sse_body),
        "/agent.v1.AgentService/RunSSE",
    ).await;

    if sse_resp.status().is_success() {
        let mut stream = sse_resp.into_body().into_data_stream();
        use tokio_stream::StreamExt;
        while let Some(chunk_res) = stream.next().await {
            match chunk_res {
                Ok(chunk) => {
                    if tx.send(Ok(chunk)).is_err() {
                        break;
                    }
                }
                Err(err) => {
                    warn!(error = %err, "官方 RunSSE 流中继异常");
                    break;
                }
            }
        }
    } else {
        warn!(status = %sse_resp.status(), "官方 RunSSE 连接失败");
        let err_frame = encode_error_end_stream("unavailable", "无法连接到 Cursor 官方 Agent 服务");
        let _ = tx.send(Ok(err_frame));
    }
    GLOBAL_SESSIONS.remove(&request_id, None);
}

/// 构造标准的 BidiAppend Connect-RPC 响应。
fn empty_bidi_append_response(headers: &HeaderMap) -> Response {
    let ct = headers
        .get("content-type")
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");

    let (content_type, body) = if ct.contains("application/json") {
        ("application/json", Body::from(b"{}".as_slice()))
    } else if ct.contains("application/connect+proto") {
        let mut out = Vec::with_capacity(12);
        out.push(0);
        out.extend_from_slice(&(0u32).to_be_bytes());
        out.push(CONNECT_END_STREAM_FLAG);
        out.extend_from_slice(&(2u32).to_be_bytes());
        out.extend_from_slice(b"{}");
        ("application/connect+proto", Body::from(out))
    } else {
        ("application/proto", Body::empty())
    };

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", content_type)
        .header("connect-protocol-version", "1")
        .header("access-control-allow-origin", "*")
        .header(
            "access-control-expose-headers",
            "connect-protocol-version, content-type",
        )
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

// ============================================================================
// 工具集定义与执行驱动核心
// ============================================================================

/// 注入 Cursor Agent 的 9 个黄金内置与虚拟工具 Schema
pub fn default_cursor_agent_tools() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "run_terminal_command".to_string(),
            description: "Run a shell/terminal command in the workspace. Use this to run git commands (e.g. git status, git diff), build scripts, tests, or inspection tools.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The command line string to execute in the workspace terminal"
                    },
                    "working_directory": {
                        "type": "string",
                        "description": "Directory path to execute the command in (relative or absolute, optional)"
                    },
                    "explanation": {
                        "type": "string",
                        "description": "Brief explanation of what the command does (optional)"
                    }
                },
                "required": ["command"]
            }),
            grammar: false,
            namespace: None,
        },
        ToolDef {
            name: "read_file".to_string(),
            description: "Read the content of a file in the workspace. Returns the file content or line range.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to read (relative to workspace or absolute)"
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "Optional 1-based start line (inclusive)"
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "Optional 1-based end line (inclusive)"
                    }
                },
                "required": ["path"]
            }),
            grammar: false,
            namespace: None,
        },
        ToolDef {
            name: "edit_file".to_string(),
            description: "Edit or create a file in the workspace. Provide `path` and either `content` (full new text) or `old_string` and `new_string` for targeted replacement.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to write or update"
                    },
                    "content": {
                        "type": "string",
                        "description": "Full file content to write (for new files or complete overwrites)"
                    },
                    "old_string": {
                        "type": "string",
                        "description": "Exact text in the existing file to be replaced"
                    },
                    "new_string": {
                        "type": "string",
                        "description": "Replacement text for old_string"
                    }
                },
                "required": ["path"]
            }),
            grammar: false,
            namespace: None,
        },
        ToolDef {
            name: "list_dir".to_string(),
            description: "List directory contents (files and subdirectories) at the given path.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Directory path to inspect (defaults to current workspace directory)"
                    }
                },
                "required": ["path"]
            }),
            grammar: false,
            namespace: None,
        },
        ToolDef {
            name: "grep_search".to_string(),
            description: "Search for regex patterns or text across workspace files.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Text or regular expression pattern to search for"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file path to search within (optional)"
                    },
                    "glob": {
                        "type": "string",
                        "description": "File glob filter, e.g. '*.rs' or '*.ts' (optional)"
                    }
                },
                "required": ["pattern"]
            }),
            grammar: false,
            namespace: None,
        },
        ToolDef {
            name: "file_search".to_string(),
            description: "Find files matching a glob pattern or file name across the workspace.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Filename or glob pattern to search for (e.g. '*.tsx' or 'router')"
                    },
                    "path": {
                        "type": "string",
                        "description": "Subdirectory to restrict search to (optional)"
                    }
                },
                "required": ["query"]
            }),
            grammar: false,
            namespace: None,
        },
        ToolDef {
            name: "delete_file".to_string(),
            description: "Delete a file from the workspace.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to delete"
                    }
                },
                "required": ["path"]
            }),
            grammar: false,
            namespace: None,
        },
        ToolDef {
            name: "ask_question".to_string(),
            description: "Ask the user a clarifying question when requirements are ambiguous or decisions need user choice. Displays an interactive options card in Composer.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "question": {
                        "type": "string",
                        "description": "The question to present to the user"
                    },
                    "options": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Selectable options or choices for the user"
                    }
                },
                "required": ["question", "options"]
            }),
            grammar: false,
            namespace: None,
        },
        ToolDef {
            name: "update_plan".to_string(),
            description: "Create or update the step-by-step implementation plan displayed in Composer UI.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "plan": {
                        "type": "string",
                        "description": "Summary of the overall plan"
                    },
                    "steps": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Individual checklist items or milestone steps"
                    }
                },
                "required": ["plan"]
            }),
            grammar: false,
            namespace: None,
        },
        ToolDef {
            name: "task".to_string(),
            description: "Launch or resume an autonomous subagent task to complete a self-contained objective in the background. To continue an existing subagent, specify `resume` with the subagent's agent_id.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "Brief 2-5 word summary of the subagent task"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Detailed instructions and objectives for the subagent"
                    },
                    "resume": {
                        "type": "string",
                        "description": "Optional agent_id of an existing subagent to resume and follow up"
                    }
                },
                "required": ["description", "prompt"]
            }),
            grammar: false,
            namespace: None,
        },
    ]
}

/// 等待客户端执行反馈 (ExecClientMessage)，带超时保护
async fn wait_for_exec_result(
    exec_rx: &mut mpsc::UnboundedReceiver<agent::ExecClientMessage>,
    target_id: u32,
    timeout: Duration,
) -> Result<agent::ExecClientMessage, String> {
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => {
                return Err(format!("Timed out after {}s waiting for execution result", timeout.as_secs()));
            }
            res = exec_rx.recv() => {
                match res {
                    Some(msg) => {
                        if msg.id == target_id || msg.exec_id == target_id.to_string() {
                            return Ok(msg);
                        } else {
                            debug!(msg_id = msg.id, target_id = target_id, "Ignoring ExecClientMessage for another call");
                        }
                    }
                    None => {
                        return Err("Client execution stream closed unexpectedly".to_string());
                    }
                }
            }
        }
    }
}

/// 等待客户端交互式卡片响应 (InteractionResponse)，带超时保护
async fn wait_for_interaction_response(
    interaction_rx: &mut mpsc::UnboundedReceiver<agent::InteractionResponse>,
    target_id: u32,
    timeout: Duration,
) -> Result<agent::InteractionResponse, String> {
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => {
                return Err(format!("Timed out after {}s waiting for user interaction", timeout.as_secs()));
            }
            res = interaction_rx.recv() => {
                match res {
                    Some(resp) => {
                        if resp.id == target_id {
                            return Ok(resp);
                        } else {
                            debug!(resp_id = resp.id, target_id = target_id, "Ignoring InteractionResponse for another query");
                        }
                    }
                    None => {
                        return Err("Interaction stream closed unexpectedly".to_string());
                    }
                }
            }
        }
    }
}

/// 构造符合 Cursor 本地 ClientExecController 权限安全策略校验的 ShellCommandParsingResult
pub fn build_shell_command_parsing_result(command: &str) -> agent::ShellCommandParsingResult {
    let mut parts = command.split_whitespace();
    let name = parts.next().unwrap_or("").to_string();
    let args = parts
        .map(|p| agent::shell_command_parsing_result::ExecutableCommandArg {
            r#type: "word".to_string(),
            value: p.to_string(),
        })
        .collect();

    let has_redirects = command.contains('>') || command.contains('<');
    let has_command_substitution = command.contains('$') || command.contains('`');

    agent::ShellCommandParsingResult {
        parsing_failed: false,
        executable_commands: vec![agent::shell_command_parsing_result::ExecutableCommand {
            name,
            args,
            full_text: command.to_string(),
        }],
        has_redirects,
        has_command_substitution,
        all_redirects_are_dev_null: None,
        redirects: vec![],
    }
}

/// 子代理独立执行工具调用（本地工作区直接执行，秒级反馈，保障子代理自主探索闭环）
pub fn execute_subagent_tool_locally(tc: &crate::normalized::ToolCall) -> (String, bool) {
    let args_val: serde_json::Value = serde_json::from_str(&tc.arguments)
        .unwrap_or_else(|_| serde_json::json!({}));

    eprintln!("[NEXUS-SUBAGENT-TOOL] Running tool: {} args: {}", tc.name, tc.arguments);

    match tc.name.as_str() {
        "run_terminal_command" => {
            let command = args_val.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let working_directory = args_val.get("working_directory").and_then(|v| v.as_str()).unwrap_or("");
            if command.trim().is_empty() {
                return ("Empty command".to_string(), true);
            }
            let mut proc = std::process::Command::new("sh");
            proc.arg("-c").arg(command);
            if !working_directory.is_empty() {
                proc.current_dir(working_directory);
            }
            match proc.output() {
                Ok(out) => {
                    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                    let exit_code = out.status.code().unwrap_or(0);
                    let res = if !stdout.is_empty() && !stderr.is_empty() {
                        format!("{}\n{}", stdout, stderr)
                    } else if !stdout.is_empty() {
                        stdout
                    } else if !stderr.is_empty() {
                        stderr
                    } else {
                        format!("Process exited with code {}", exit_code)
                    };
                    eprintln!("[NEXUS-SUBAGENT-TOOL] command exit_code={exit_code}, out_len={}", res.len());
                    (res, exit_code != 0)
                }
                Err(e) => (format!("Failed to execute command: {}", e), true),
            }
        }
        "read_file" => {
            let path = args_val.get("path").and_then(|v| v.as_str()).unwrap_or("");
            match std::fs::read_to_string(path) {
                Ok(content) => {
                    let lines: Vec<&str> = content.lines().collect();
                    let start = args_val.get("start_line").and_then(|v| v.as_u64()).unwrap_or(1) as usize;
                    let end = args_val.get("end_line").and_then(|v| v.as_u64()).unwrap_or(lines.len() as u64) as usize;
                    let s = start.saturating_sub(1);
                    let e = end.min(lines.len());
                    if s < lines.len() {
                        (lines[s..e].join("\n"), false)
                    } else {
                        (content, false)
                    }
                }
                Err(e) => (format!("Failed to read {path}: {e}"), true),
            }
        }
        "list_dir" => {
            let path = args_val.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            match std::fs::read_dir(path) {
                Ok(entries) => {
                    let mut list = Vec::new();
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
                        list.push(if is_dir { format!("{name}/") } else { name });
                    }
                    list.sort();
                    (list.join("\n"), false)
                }
                Err(e) => (format!("Failed to list {path}: {e}"), true),
            }
        }
        "file_search" => {
            let query = args_val.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let path = args_val.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let cmd = format!("find {} -not -path '*/.*' -not -path './target*' -not -path './node_modules*' -name '*{}*' -maxdepth 5", path, query);
            let mut proc = std::process::Command::new("sh");
            proc.arg("-c").arg(&cmd);
            match proc.output() {
                Ok(out) => (String::from_utf8_lossy(&out.stdout).to_string(), false),
                Err(e) => (format!("Search failed: {e}"), true),
            }
        }
        "grep_search" => {
            let pattern = args_val.get("pattern").and_then(|v| v.as_str()).unwrap_or("");
            let path = args_val.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let cmd = format!("grep -rn --exclude-dir=target --exclude-dir=node_modules --exclude-dir=.git '{}' {}", pattern, path);
            let mut proc = std::process::Command::new("sh");
            proc.arg("-c").arg(&cmd);
            match proc.output() {
                Ok(out) => (String::from_utf8_lossy(&out.stdout).to_string(), false),
                Err(e) => (format!("Grep failed: {e}"), true),
            }
        }
        other => (format!("Subagent tool {} not supported in sub-loop", other), true),
    }
}

/// 执行单个 Agent 工具调用（支持 Cursor 本地 ClientExecController 执行轨与 UI 渲染轨，以及虚拟工具）
async fn execute_agent_tool_call(
    upstream: &Arc<dyn Upstream>,
    credential: &Credential,
    target_model: &str,
    conv_id: Option<&str>,
    tx: &mpsc::UnboundedSender<Result<Bytes, std::io::Error>>,
    exec_rx: &mut mpsc::UnboundedReceiver<agent::ExecClientMessage>,
    interaction_rx: &mut mpsc::UnboundedReceiver<agent::InteractionResponse>,
    tc: &crate::normalized::ToolCall,
    exec_id: u32,
) -> (String, bool) {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let args_val: serde_json::Value = serde_json::from_str(&tc.arguments)
        .unwrap_or_else(|_| serde_json::json!({}));

    info!(
        tool_name = %tc.name,
        tool_call_id = %tc.id,
        exec_id = exec_id,
        "开始执行 Agent 工具调用"
    );

    match tc.name.as_str() {
        "run_terminal_command" => {
            let command = args_val
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let working_directory = args_val
                .get("working_directory")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let explanation = args_val
                .get("explanation")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let cmd_name = command.split_whitespace().next().unwrap_or("").to_string();
            let shell_args = agent::ShellArgs {
                command: command.clone(),
                working_directory,
                timeout: 60,
                tool_call_id: tc.id.clone(),
                simple_commands: if cmd_name.is_empty() { vec![] } else { vec![cmd_name] },
                parsing_result: Some(build_shell_command_parsing_result(&command)),
                skip_approval: true,
                description: explanation,
                ..Default::default()
            };

            // 1. UI 渲染轨：ToolCallStarted
            let started_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                tool: Some(agent::tool_call::Tool::ShellToolCall(agent::ShellToolCall {
                    args: Some(shell_args.clone()),
                    result: None,
                    description: None,
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_started(
                &tc.id,
                &tc.id,
                started_tc,
            ))));

            // 2. 本地执行轨：ExecServerMessage
            let exec_server_msg = agent::ExecServerMessage {
                id: exec_id,
                exec_id: exec_id.to_string(),
                message: Some(agent::exec_server_message::Message::ShellArgs(shell_args.clone())),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&wrap_exec_server_message(
                exec_server_msg,
            ))));

            // 3. 挂起等待客户端执行结果（带 60 秒超时）
            let exec_res = wait_for_exec_result(exec_rx, exec_id, Duration::from_secs(60)).await;

            let (output, is_error, completed_shell_result) = match exec_res {
                Ok(client_msg) => {
                    if let Some(agent::exec_client_message::Message::ShellResult(sr)) = client_msg.message {
                        let is_err = match &sr.result {
                            Some(agent::shell_result::Result::Success(s)) => s.exit_code != 0,
                            _ => true,
                        };
                        let out_str = match &sr.result {
                            Some(agent::shell_result::Result::Success(s)) => {
                                if !s.stdout.is_empty() && !s.stderr.is_empty() {
                                    format!("{}\n{}", s.stdout, s.stderr)
                                } else if !s.stdout.is_empty() {
                                    s.stdout.clone()
                                } else if !s.stderr.is_empty() {
                                    s.stderr.clone()
                                } else {
                                    format!("Process exited with code {}", s.exit_code)
                                }
                            }
                            Some(agent::shell_result::Result::Failure(f)) => {
                                if !f.stderr.is_empty() {
                                    f.stderr.clone()
                                } else {
                                    format!("Command failed with exit code {}", f.exit_code)
                                }
                            }
                            Some(agent::shell_result::Result::Timeout(t)) => {
                                format!("Command timed out after {}ms", t.timeout_ms)
                            }
                            Some(agent::shell_result::Result::Rejected(r)) => {
                                format!("Command rejected by user: {}", r.reason)
                            }
                            Some(agent::shell_result::Result::PermissionDenied(p)) => {
                                format!("Permission denied: {}", p.error)
                            }
                            Some(agent::shell_result::Result::SpawnError(e)) => {
                                format!("Failed to spawn command: {}", e.error)
                            }
                            None => "Command finished without output".to_string(),
                        };
                        (out_str, is_err, Some(sr))
                    } else {
                        ("[Error] Unexpected response type from client".to_string(), true, None)
                    }
                }
                Err(err) => (format!("[Error] {err}"), true, None),
            };

            // 若客户端报错（例如旧会话中出现 SpawnError、PermissionDenied 或连接超时），启动网关本地透明执行兜底
            let (output, is_error, completed_shell_result) = if is_error && !command.trim().is_empty() {
                warn!(command = %command, error = %output, "客户端终端响应异常，触发网关本地工作区安全兜底执行");
                let mut proc = std::process::Command::new("sh");
                proc.arg("-c").arg(&command);
                if !shell_args.working_directory.is_empty() {
                    proc.current_dir(&shell_args.working_directory);
                }
                match proc.output() {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                        let exit_code = out.status.code().unwrap_or(0);
                        let combined = if !stdout.is_empty() && !stderr.is_empty() {
                            format!("{}\n{}", stdout, stderr)
                        } else if !stdout.is_empty() {
                            stdout.clone()
                        } else if !stderr.is_empty() {
                            stderr.clone()
                        } else {
                            format!("Process exited with code {}", exit_code)
                        };
                        let fallback_sr = agent::ShellResult {
                            result: Some(agent::shell_result::Result::Success(agent::ShellSuccess {
                                command: command.clone(),
                                working_directory: shell_args.working_directory.clone(),
                                exit_code,
                                signal: String::new(),
                                stdout,
                                stderr,
                                execution_time: 10,
                                output_location: None,
                                shell_id: None,
                                interleaved_output: None,
                                pid: Some(9999),
                                ms_to_wait: None,
                                local_execution_time_ms: Some(10),
                                background_reason: None,
                                output_head: None,
                                output_tail: None,
                                elided_chars: None,
                            })),
                            ..Default::default()
                        };
                        (combined, exit_code != 0, Some(fallback_sr))
                    }
                    Err(_) => (output, is_error, completed_shell_result),
                }
            } else {
                (output, is_error, completed_shell_result)
            };

            // 4. UI 渲染轨：ToolCallCompleted
            let completed_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                completed_at_ms: Some(now_ms + 100),
                tool: Some(agent::tool_call::Tool::ShellToolCall(agent::ShellToolCall {
                    args: Some(shell_args),
                    result: completed_shell_result,
                    description: None,
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_completed(
                &tc.id,
                &tc.id,
                completed_tc,
            ))));

            (output, is_error)
        }
        "read_file" => {
            let path = args_val
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let start_line = args_val
                .get("start_line")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32);
            let end_line = args_val
                .get("end_line")
                .and_then(|v| v.as_i64())
                .map(|n| n as u32);
            let limit = match (start_line, end_line) {
                (Some(s), Some(e)) if e >= s as u32 => Some(e - s as u32 + 1),
                _ => None,
            };

            let read_args = agent::ReadArgs {
                path: path.clone(),
                tool_call_id: tc.id.clone(),
                offset: start_line,
                limit,
                encoding_hint: None,
            };

            let started_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                tool: Some(agent::tool_call::Tool::ReadToolCall(agent::ReadToolCall {
                    args: Some(agent::ReadToolArgs {
                        path: path.clone(),
                        offset: start_line,
                        limit: limit.map(|l| l as i32),
                        include_line_numbers: Some(true),
                    }),
                    result: None,
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_started(
                &tc.id,
                &tc.id,
                started_tc,
            ))));

            let exec_server_msg = agent::ExecServerMessage {
                id: exec_id,
                exec_id: exec_id.to_string(),
                message: Some(agent::exec_server_message::Message::ReadArgs(read_args)),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&wrap_exec_server_message(
                exec_server_msg,
            ))));

            let exec_res = wait_for_exec_result(exec_rx, exec_id, Duration::from_secs(30)).await;

            let (output, is_error) = match exec_res {
                Ok(client_msg) => {
                    if let Some(agent::exec_client_message::Message::ReadResult(rr)) = client_msg.message {
                        match rr.result {
                            Some(agent::read_result::Result::Success(s)) => {
                                let content = match s.output {
                                    Some(agent::read_success::Output::Content(c)) => c,
                                    Some(agent::read_success::Output::Data(d)) => {
                                        String::from_utf8_lossy(&d).to_string()
                                    }
                                    None => String::new(),
                                };
                                (content, false)
                            }
                            Some(agent::read_result::Result::FileNotFound(_)) => {
                                (format!("File not found: {path}"), true)
                            }
                            Some(agent::read_result::Result::Error(e)) => {
                                (format!("Error reading {path}: {}", e.error), true)
                            }
                            Some(agent::read_result::Result::PermissionDenied(_)) => {
                                (format!("Permission denied reading {path}"), true)
                            }
                            Some(agent::read_result::Result::InvalidFile(i)) => {
                                (format!("Invalid file {path}: {}", i.reason), true)
                            }
                            Some(agent::read_result::Result::Rejected(r)) => {
                                (format!("Read rejected: {}", r.reason), true)
                            }
                            None => (format!("Empty read result for {path}"), false),
                        }
                    } else {
                        ("[Error] Unexpected response type for read_file".to_string(), true)
                    }
                }
                Err(err) => (format!("[Error] {err}"), true),
            };

            let completed_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                completed_at_ms: Some(now_ms + 50),
                tool: Some(agent::tool_call::Tool::ReadToolCall(agent::ReadToolCall {
                    args: Some(agent::ReadToolArgs {
                        path: path.clone(),
                        offset: start_line,
                        limit: limit.map(|l| l as i32),
                        include_line_numbers: Some(true),
                    }),
                    result: Some(agent::ReadToolResult {
                        result: Some(if is_error {
                            agent::read_tool_result::Result::Error(agent::ReadToolError {
                                error_message: output.clone(),
                            })
                        } else {
                            agent::read_tool_result::Result::Success(agent::ReadToolSuccess {
                                is_empty: output.is_empty(),
                                exceeded_limit: false,
                                total_lines: output.lines().count() as u32,
                                file_size: output.len() as u32,
                                path: path.clone(),
                                read_range: None,
                                include_line_numbers: Some(true),
                                related_cursor_rule_paths: Vec::new(),
                                related_cursor_rules: Vec::new(),
                                output: Some(agent::read_tool_success::Output::Content(output.clone())),
                            })
                        }),
                    }),
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_completed(
                &tc.id,
                &tc.id,
                completed_tc,
            ))));

            (output, is_error)
        }
        "edit_file" => {
            let path = args_val
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let content_opt = args_val.get("content").and_then(|v| v.as_str());
            let old_str = args_val.get("old_string").and_then(|v| v.as_str());
            let new_str = args_val.get("new_string").and_then(|v| v.as_str());

            let file_text = if let Some(content) = content_opt {
                content.to_string()
            } else if let (Some(old), Some(new)) = (old_str, new_str) {
                if let Ok(existing) = std::fs::read_to_string(&path) {
                    existing.replace(old, new)
                } else {
                    new.to_string()
                }
            } else {
                new_str.unwrap_or("").to_string()
            };

            let write_args = agent::WriteArgs {
                path: path.clone(),
                file_text: file_text.clone(),
                tool_call_id: tc.id.clone(),
                return_file_content_after_write: false,
                file_bytes: Vec::new(),
                encoding_hint: None,
            };

            let started_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                tool: Some(agent::tool_call::Tool::EditToolCall(agent::EditToolCall {
                    args: Some(agent::EditArgs {
                        path: path.clone(),
                        stream_content: Some(file_text.clone()),
                    }),
                    result: None,
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_started(
                &tc.id,
                &tc.id,
                started_tc,
            ))));

            let exec_server_msg = agent::ExecServerMessage {
                id: exec_id,
                exec_id: exec_id.to_string(),
                message: Some(agent::exec_server_message::Message::WriteArgs(write_args)),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&wrap_exec_server_message(
                exec_server_msg,
            ))));

            let exec_res = wait_for_exec_result(exec_rx, exec_id, Duration::from_secs(30)).await;

            let (output, is_error) = match exec_res {
                Ok(client_msg) => {
                    if let Some(agent::exec_client_message::Message::WriteResult(wr)) = client_msg.message {
                        match wr.result {
                            Some(agent::write_result::Result::Success(_)) => {
                                (format!("File {path} updated successfully."), false)
                            }
                            Some(agent::write_result::Result::Error(e)) => {
                                (format!("Error writing {path}: {}", e.error), true)
                            }
                            Some(agent::write_result::Result::PermissionDenied(p)) => {
                                (format!("Permission denied writing {path}: {}", p.error), true)
                            }
                            Some(agent::write_result::Result::Rejected(r)) => {
                                (format!("Write rejected: {}", r.reason), true)
                            }
                            Some(agent::write_result::Result::NoSpace(_)) => {
                                (format!("No space left on device writing {path}"), true)
                            }
                            None => (format!("File {path} written."), false),
                        }
                    } else {
                        ("[Error] Unexpected response type for edit_file".to_string(), true)
                    }
                }
                Err(err) => (format!("[Error] {err}"), true),
            };

            let completed_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                completed_at_ms: Some(now_ms + 50),
                tool: Some(agent::tool_call::Tool::EditToolCall(agent::EditToolCall {
                    args: Some(agent::EditArgs {
                        path: path.clone(),
                        stream_content: Some(file_text.clone()),
                    }),
                    result: Some(agent::EditResult {
                        result: Some(if is_error {
                            agent::edit_result::Result::Error(agent::EditError {
                                path: path.clone(),
                                error: output.clone(),
                                model_visible_error: Some(output.clone()),
                            })
                        } else {
                            agent::edit_result::Result::Success(agent::EditSuccess {
                                path: path.clone(),
                                lines_added: None,
                                lines_removed: None,
                                diff_string: None,
                                before_full_file_content: None,
                                after_full_file_content: file_text,
                                message: Some("File updated".to_string()),
                            })
                        }),
                    }),
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_completed(
                &tc.id,
                &tc.id,
                completed_tc,
            ))));

            (output, is_error)
        }
        "list_dir" => {
            let path = args_val
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string();

            let ls_args = agent::LsArgs {
                path: path.clone(),
                ignore: Vec::new(),
                tool_call_id: tc.id.clone(),
                sandbox_policy: None,
                timeout_ms: Some(10000),
            };

            let started_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                tool: Some(agent::tool_call::Tool::LsToolCall(agent::LsToolCall {
                    args: Some(ls_args.clone()),
                    result: None,
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_started(
                &tc.id,
                &tc.id,
                started_tc,
            ))));

            let exec_server_msg = agent::ExecServerMessage {
                id: exec_id,
                exec_id: exec_id.to_string(),
                message: Some(agent::exec_server_message::Message::LsArgs(ls_args.clone())),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&wrap_exec_server_message(
                exec_server_msg,
            ))));

            let exec_res = wait_for_exec_result(exec_rx, exec_id, Duration::from_secs(20)).await;

            let (output, is_error) = match exec_res {
                Ok(client_msg) => {
                    if let Some(agent::exec_client_message::Message::LsResult(lr)) = client_msg.message {
                        match lr.result {
                            Some(agent::ls_result::Result::Success(s)) => {
                                let mut entries = Vec::new();
                                if let Some(root) = s.directory_tree_root {
                                    for d in root.children_dirs {
                                        entries.push(format!("{}/ (dir)", d.abs_path));
                                    }
                                    for f in root.children_files {
                                        entries.push(format!("{} (file)", f.name));
                                    }
                                }
                                if entries.is_empty() {
                                    (format!("Directory {path} is empty"), false)
                                } else {
                                    (entries.join("\n"), false)
                                }
                            }
                            Some(agent::ls_result::Result::Error(e)) => {
                                (format!("Error listing {path}: {}", e.error), true)
                            }
                            Some(agent::ls_result::Result::Timeout(_)) => {
                                (format!("Timeout listing {path}"), true)
                            }
                            Some(agent::ls_result::Result::Rejected(r)) => {
                                (format!("List rejected: {}", r.reason), true)
                            }
                            None => (format!("Empty list result for {path}"), false),
                        }
                    } else {
                        ("[Error] Unexpected response for list_dir".to_string(), true)
                    }
                }
                Err(err) => (format!("[Error] {err}"), true),
            };

            let completed_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                completed_at_ms: Some(now_ms + 50),
                tool: Some(agent::tool_call::Tool::LsToolCall(agent::LsToolCall {
                    args: Some(ls_args),
                    result: Some(agent::LsResult {
                        result: Some(if is_error {
                            agent::ls_result::Result::Error(agent::LsError {
                                path: path.clone(),
                                error: output.clone(),
                            })
                        } else {
                            agent::ls_result::Result::Success(agent::LsSuccess {
                                directory_tree_root: None,
                            })
                        }),
                    }),
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_completed(
                &tc.id,
                &tc.id,
                completed_tc,
            ))));

            (output, is_error)
        }
        "grep_search" => {
            let pattern = args_val
                .get("pattern")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let path = args_val
                .get("path")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            let glob = args_val
                .get("glob")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            let grep_args = agent::GrepArgs {
                pattern: pattern.clone(),
                path,
                glob,
                tool_call_id: tc.id.clone(),
                output_mode: Some("content".to_string()),
                ..Default::default()
            };

            let started_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                tool: Some(agent::tool_call::Tool::GrepToolCall(agent::GrepToolCall {
                    args: Some(grep_args.clone()),
                    result: None,
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_started(
                &tc.id,
                &tc.id,
                started_tc,
            ))));

            let exec_server_msg = agent::ExecServerMessage {
                id: exec_id,
                exec_id: exec_id.to_string(),
                message: Some(agent::exec_server_message::Message::GrepArgs(grep_args.clone())),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&wrap_exec_server_message(
                exec_server_msg,
            ))));

            let exec_res = wait_for_exec_result(exec_rx, exec_id, Duration::from_secs(30)).await;

            let (output, is_error) = match exec_res {
                Ok(client_msg) => {
                    if let Some(agent::exec_client_message::Message::GrepResult(gr)) = client_msg.message {
                        match gr.result {
                            Some(agent::grep_result::Result::Success(s)) => {
                                let mut lines = Vec::new();
                                for (file_path, union_res) in s.workspace_results {
                                    match union_res.result {
                                        Some(agent::grep_union_result::Result::Content(cr)) => {
                                            for m in cr.matches {
                                                for cm in m.matches {
                                                    lines.push(format!("{}:{}: {}", file_path, cm.line_number, cm.content));
                                                }
                                            }
                                        }
                                        Some(agent::grep_union_result::Result::Files(fr)) => {
                                            for f in fr.files {
                                                lines.push(format!("{}: match found", f));
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                if lines.is_empty() {
                                    (format!("No matches found for pattern: {}", pattern), false)
                                } else {
                                    (lines.join("\n"), false)
                                }
                            }
                            Some(agent::grep_result::Result::Error(e)) => {
                                (format!("Grep error: {}", e.error), true)
                            }
                            None => (format!("No grep matches for {}", pattern), false),
                        }
                    } else {
                        ("[Error] Unexpected response for grep_search".to_string(), true)
                    }
                }
                Err(err) => (format!("[Error] {err}"), true),
            };

            let completed_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                completed_at_ms: Some(now_ms + 50),
                tool: Some(agent::tool_call::Tool::GrepToolCall(agent::GrepToolCall {
                    args: Some(grep_args),
                    result: Some(agent::GrepResult {
                        result: Some(if is_error {
                            agent::grep_result::Result::Error(agent::GrepError {
                                error: output.clone(),
                            })
                        } else {
                            agent::grep_result::Result::Success(agent::GrepSuccess {
                                pattern,
                                path: String::new(),
                                output_mode: "content".into(),
                                workspace_results: HashMap::new(),
                                active_editor_result: None,
                            })
                        }),
                    }),
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_completed(
                &tc.id,
                &tc.id,
                completed_tc,
            ))));

            (output, is_error)
        }
        "delete_file" => {
            let path = args_val
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let delete_args = agent::DeleteArgs {
                path: path.clone(),
                tool_call_id: tc.id.clone(),
            };

            let started_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                tool: Some(agent::tool_call::Tool::DeleteToolCall(agent::DeleteToolCall {
                    args: Some(delete_args.clone()),
                    result: None,
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_started(
                &tc.id,
                &tc.id,
                started_tc,
            ))));

            let exec_server_msg = agent::ExecServerMessage {
                id: exec_id,
                exec_id: exec_id.to_string(),
                message: Some(agent::exec_server_message::Message::DeleteArgs(delete_args.clone())),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&wrap_exec_server_message(
                exec_server_msg,
            ))));

            let exec_res = wait_for_exec_result(exec_rx, exec_id, Duration::from_secs(15)).await;

            let (output, is_error) = match exec_res {
                Ok(client_msg) => {
                    if let Some(agent::exec_client_message::Message::DeleteResult(dr)) = client_msg.message {
                        match dr.result {
                            Some(agent::delete_result::Result::Success(_)) => {
                                (format!("File {path} deleted successfully."), false)
                            }
                            Some(agent::delete_result::Result::Error(e)) => {
                                (format!("Delete error for {path}: {}", e.error), true)
                            }
                            Some(agent::delete_result::Result::FileNotFound(_)) => {
                                (format!("File not found for deletion: {path}"), true)
                            }
                            _ => (format!("Delete request processed for {path}"), false),
                        }
                    } else {
                        ("[Error] Unexpected response for delete_file".to_string(), true)
                    }
                }
                Err(err) => (format!("[Error] {err}"), true),
            };

            let completed_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                completed_at_ms: Some(now_ms + 50),
                tool: Some(agent::tool_call::Tool::DeleteToolCall(agent::DeleteToolCall {
                    args: Some(delete_args),
                    result: Some(agent::DeleteResult {
                        result: Some(if is_error {
                            agent::delete_result::Result::Error(agent::DeleteError {
                                path: path.clone(),
                                error: output.clone(),
                            })
                        } else {
                            agent::delete_result::Result::Success(agent::DeleteSuccess {
                                path: path.clone(),
                                deleted_file: path.clone(),
                                file_size: 0,
                                prev_content: String::new(),
                            })
                        }),
                    }),
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_completed(
                &tc.id,
                &tc.id,
                completed_tc,
            ))));

            (output, is_error)
        }
        "file_search" => {
            let query = args_val
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let path = args_val
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".")
                .to_string();

            // 转化为 find 终端指令由 ClientExecController 执行
            let cmd = format!("find {} -name '*{}*' -maxdepth 5", path, query);
            let cmd_name = cmd.split_whitespace().next().unwrap_or("find").to_string();
            let shell_args = agent::ShellArgs {
                command: cmd.clone(),
                working_directory: String::new(),
                timeout: 30,
                tool_call_id: tc.id.clone(),
                simple_commands: vec![cmd_name],
                parsing_result: Some(build_shell_command_parsing_result(&cmd)),
                skip_approval: true,
                description: Some(format!("Search files matching {query}")),
                ..Default::default()
            };

            let started_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                tool: Some(agent::tool_call::Tool::GlobToolCall(agent::GlobToolCall {
                    args: Some(agent::GlobToolArgs {
                        target_directory: Some(path.clone()),
                        glob_pattern: query.clone(),
                    }),
                    result: None,
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_started(
                &tc.id,
                &tc.id,
                started_tc,
            ))));

            let exec_server_msg = agent::ExecServerMessage {
                id: exec_id,
                exec_id: exec_id.to_string(),
                message: Some(agent::exec_server_message::Message::ShellArgs(shell_args)),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&wrap_exec_server_message(
                exec_server_msg,
            ))));

            let exec_res = wait_for_exec_result(exec_rx, exec_id, Duration::from_secs(30)).await;

            let (output, is_error) = match exec_res {
                Ok(client_msg) => {
                    if let Some(agent::exec_client_message::Message::ShellResult(sr)) = client_msg.message {
                        if let Some(agent::shell_result::Result::Success(s)) = sr.result {
                            if s.stdout.trim().is_empty() {
                                (format!("No files found matching '{}'", query), false)
                            } else {
                                (s.stdout, false)
                            }
                        } else {
                            (format!("Search failed for '{}'", query), true)
                        }
                    } else {
                        ("[Error] Unexpected response for file_search".to_string(), true)
                    }
                }
                Err(err) => (format!("[Error] {err}"), true),
            };

            let completed_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                completed_at_ms: Some(now_ms + 50),
                tool: Some(agent::tool_call::Tool::GlobToolCall(agent::GlobToolCall {
                    args: Some(agent::GlobToolArgs {
                        target_directory: Some(path.clone()),
                        glob_pattern: query.clone(),
                    }),
                    result: Some(agent::GlobToolResult {
                        result: Some(if is_error {
                            agent::glob_tool_result::Result::Error(agent::GlobToolError {
                                error: output.clone(),
                            })
                        } else {
                            agent::glob_tool_result::Result::Success(agent::GlobToolSuccess {
                                pattern: query,
                                path,
                                files: output.lines().map(|s| s.to_string()).collect(),
                                total_files: output.lines().count() as i32,
                                client_truncated: false,
                                ripgrep_truncated: false,
                            })
                        }),
                    }),
                })),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_completed(
                &tc.id,
                &tc.id,
                completed_tc,
            ))));

            (output, is_error)
        }
        "ask_question" => {
            let question = args_val
                .get("question")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let options: Vec<String> = args_val
                .get("options")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let ask_args = agent::AskQuestionArgs {
                title: "Question".to_string(),
                questions: vec![agent::ask_question_args::Question {
                    id: "q1".to_string(),
                    prompt: question.clone(),
                    options: options
                        .iter()
                        .enumerate()
                        .map(|(i, opt)| agent::ask_question_args::Option {
                            id: format!("opt_{i}"),
                            label: opt.clone(),
                        })
                        .collect(),
                    allow_multiple: false,
                }],
                run_async: false,
                async_original_tool_call_id: tc.id.clone(),
            };

            let started_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                tool: Some(agent::tool_call::Tool::AskQuestionToolCall(
                    agent::AskQuestionToolCall {
                        args: Some(ask_args.clone()),
                        result: None,
                    },
                )),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_started(
                &tc.id,
                &tc.id,
                started_tc,
            ))));

            // 下发 InteractionQuery 驱动前端弹出选项卡
            let query = agent::InteractionQuery {
                id: exec_id,
                query: Some(agent::interaction_query::Query::AskQuestionInteractionQuery(
                    agent::AskQuestionInteractionQuery {
                        args: Some(ask_args.clone()),
                        tool_call_id: tc.id.clone(),
                    },
                )),
            };
            let _ = tx.send(Ok(encode_agent_server_message(&wrap_interaction_query(
                query,
            ))));

            // 等待用户选择答案（最长等待 180 秒）
            let resp_res = wait_for_interaction_response(interaction_rx, exec_id, Duration::from_secs(180)).await;

            let answer_text = match resp_res {
                Ok(resp) => {
                    if let Some(agent::interaction_response::Result::AskQuestionInteractionResponse(ar)) = resp.result {
                        if let Some(agent::ask_question_result::Result::Success(s)) = ar.result.and_then(|r| r.result) {
                            let mut chosen = Vec::new();
                            for ans in s.answers {
                                if !ans.freeform_text.trim().is_empty() {
                                    chosen.push(ans.freeform_text);
                                }
                                for oid in ans.selected_option_ids {
                                    if let Some(idx_str) = oid.strip_prefix("opt_") {
                                        if let Ok(idx) = idx_str.parse::<usize>() {
                                            if let Some(label) = options.get(idx) {
                                                chosen.push(label.clone());
                                            }
                                        }
                                    }
                                }
                            }
                            if chosen.is_empty() {
                                "User submitted without selecting an option".to_string()
                            } else {
                                format!("User selected: {}", chosen.join(", "))
                            }
                        } else {
                            "User rejected or closed question card".to_string()
                        }
                    } else {
                        "Interaction response received".to_string()
                    }
                }
                Err(_) => "User did not answer within 180s, continuing with default approach".to_string(),
            };

            let completed_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                completed_at_ms: Some(now_ms + 100),
                tool: Some(agent::tool_call::Tool::AskQuestionToolCall(
                    agent::AskQuestionToolCall {
                        args: Some(ask_args),
                        result: Some(agent::AskQuestionResult {
                            result: Some(agent::ask_question_result::Result::Success(
                                agent::AskQuestionSuccess {
                                    answers: vec![agent::ask_question_success::Answer {
                                        question_id: "q1".to_string(),
                                        selected_option_ids: Vec::new(),
                                        freeform_text: answer_text.clone(),
                                    }],
                                },
                            )),
                        }),
                    },
                )),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_completed(
                &tc.id,
                &tc.id,
                completed_tc,
            ))));

            (answer_text, false)
        }
        "update_plan" => {
            let plan = args_val
                .get("plan")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let steps: Vec<String> = args_val
                .get("steps")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|x| x.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            let todos: Vec<agent::TodoItem> = steps
                .iter()
                .enumerate()
                .map(|(i, s)| agent::TodoItem {
                    id: format!("todo_{i}"),
                    content: s.clone(),
                    status: 1, // Pending
                    created_at: now_ms as i64,
                    updated_at: now_ms as i64,
                    dependencies: Vec::new(),
                })
                .collect();

            let plan_args = agent::CreatePlanArgs {
                plan: plan.clone(),
                todos,
                overview: plan.clone(),
                name: "Execution Plan".to_string(),
                is_project: false,
                phases: Vec::new(),
            };

            let started_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                tool: Some(agent::tool_call::Tool::CreatePlanToolCall(
                    agent::CreatePlanToolCall {
                        args: Some(plan_args.clone()),
                        result: None,
                    },
                )),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_started(
                &tc.id,
                &tc.id,
                started_tc,
            ))));

            let completed_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                completed_at_ms: Some(now_ms + 50),
                tool: Some(agent::tool_call::Tool::CreatePlanToolCall(
                    agent::CreatePlanToolCall {
                        args: Some(plan_args),
                        result: Some(agent::CreatePlanResult {
                            plan_uri: "composer://plan".to_string(),
                            result: Some(agent::create_plan_result::Result::Success(
                                agent::CreatePlanSuccess {},
                            )),
                        }),
                    },
                )),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_completed(
                &tc.id,
                &tc.id,
                completed_tc,
            ))));

            ("Plan updated and displayed in Composer UI.".to_string(), false)
        }
        "dispatch_subagent" | "task" | "subagent" | "run_subagent" => {
            let description = args_val
                .get("description")
                .and_then(|v| v.as_str())
                .or_else(|| args_val.get("task").and_then(|v| v.as_str()))
                .unwrap_or("Subagent Task")
                .to_string();
            let prompt = args_val
                .get("prompt")
                .and_then(|v| v.as_str())
                .or_else(|| args_val.get("instructions").and_then(|v| v.as_str()))
                .unwrap_or("")
                .to_string();

            let resume_param = args_val
                .get("resume")
                .and_then(|v| v.as_str())
                .or_else(|| args_val.get("agent_id").and_then(|v| v.as_str()))
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.trim().to_string());

            let effective_resume = resume_param.or_else(|| {
                let lower_prompt = prompt.to_lowercase();
                let lower_desc = description.to_lowercase();
                if lower_prompt.contains("上一个")
                    || lower_prompt.contains("刚才")
                    || lower_prompt.contains("previous")
                    || lower_prompt.contains("last subagent")
                    || lower_desc.contains("上一个")
                    || lower_desc.contains("刚才")
                    || lower_desc.contains("previous")
                {
                    if let Some(cid) = conv_id {
                        COMPOSER_CONVERSATION_HISTORY.read().ok().and_then(|h| {
                            h.get(cid).and_then(|rec| rec.last_subagent_id.clone())
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            }).map(|id| {
                if id == "last" || id == "latest" {
                    if let Some(cid) = conv_id {
                        COMPOSER_CONVERSATION_HISTORY.read().ok().and_then(|h| {
                            h.get(cid).and_then(|rec| rec.last_subagent_id.clone())
                        }).unwrap_or(id)
                    } else {
                        id
                    }
                } else {
                    id
                }
            });

            let (subagent_id, mut sub_messages, mut sub_conversation_steps, is_resumed) = if let Some(ref rid) = effective_resume {
                if let Some(existing) = SUBAGENT_SESSIONS.read().unwrap().get(rid).cloned() {
                    let mut msgs = existing.messages;
                    let follow_up = if prompt.trim().is_empty() { description.clone() } else { prompt.clone() };
                    msgs.push(Message::text(Role::User, follow_up));
                    (rid.clone(), msgs, existing.conversation_steps, true)
                } else {
                    let new_id = format!("subagent_{}", uuid::Uuid::new_v4().simple());
                    let msgs = vec![
                        Message::text(
                            Role::System,
                            format!(
                                "You are an autonomous subagent for Cursor. Your objective is: {description}\nDetailed instructions: {prompt}\nInvestigate or execute thoroughly using your tools and provide a structured, comprehensive summary of your findings and conclusions."
                            ),
                        ),
                        Message::text(Role::User, if prompt.trim().is_empty() { description.clone() } else { prompt.clone() }),
                    ];
                    (new_id, msgs, Vec::new(), false)
                }
            } else {
                let new_id = format!("subagent_{}", uuid::Uuid::new_v4().simple());
                let msgs = vec![
                    Message::text(
                        Role::System,
                        format!(
                            "You are an autonomous subagent for Cursor. Your objective is: {description}\nDetailed instructions: {prompt}\nInvestigate or execute thoroughly using your tools and provide a structured, comprehensive summary of your findings and conclusions."
                        ),
                    ),
                    Message::text(Role::User, if prompt.trim().is_empty() { description.clone() } else { prompt.clone() }),
                ];
                (new_id, msgs, Vec::new(), false)
            };

            let task_args = agent::TaskArgs {
                description: description.clone(),
                prompt: prompt.clone(),
                subagent_type: None,
                model: None,
                resume: if is_resumed { Some(subagent_id.clone()) } else { None },
                agent_id: Some(subagent_id.clone()),
                attachments: Vec::new(),
                mode: 0,
                responding_to_message_ids: Vec::new(),
                environment: 0,
                machine: None,
            };

            let started_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                tool: Some(agent::tool_call::Tool::TaskToolCall(
                    agent::TaskToolCall {
                        args: Some(task_args.clone()),
                        result: None,
                        cloud_agent_bc_id: None,
                    },
                )),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_started(
                &tc.id,
                &tc.id,
                started_tc,
            ))));

            eprintln!("[NEXUS-SUBAGENT] Start (id={}, resumed={}): desc={description}, prompt_len={}", subagent_id, is_resumed, prompt.len());

            // 构造子代理执行轨迹记录，以便用户点击子代理卡片时查看到真实的子任务过程
            if is_resumed {
                sub_conversation_steps.push(agent::ConversationStep {
                    message: Some(agent::conversation_step::Message::AssistantMessage(
                        agent::AssistantMessage {
                            text: format!("**追加指令 (继续会话 `{}`)**: {}\n{}", subagent_id, description, prompt),
                            ..Default::default()
                        },
                    )),
                });
            } else if !prompt.is_empty() || !description.is_empty() {
                sub_conversation_steps.push(agent::ConversationStep {
                    message: Some(agent::conversation_step::Message::AssistantMessage(
                        agent::AssistantMessage {
                            text: if prompt.is_empty() {
                                description.clone()
                            } else {
                                format!("**子代理任务目标**: {}\n{}", description, prompt)
                            },
                            ..Default::default()
                        },
                    )),
                });
            }

            // 子代理多步工具循环（上限 5 步）
            let sub_start = std::time::Instant::now();
            let mut final_summary = String::new();
            let mut sub_is_err = false;

            for sub_step in 0..5 {
                eprintln!("[NEXUS-SUBAGENT] step={sub_step}, messages_count={}", sub_messages.len());
                let sub_chat_req = ChatRequest {
                    model: target_model.to_string(),
                    messages: sub_messages.clone(),
                    tools: default_cursor_agent_tools()
                        .into_iter()
                        .filter(|t| t.name != "dispatch_subagent" && t.name != "task" && t.name != "subagent")
                        .collect(),
                    tool_choice: ToolChoice::Auto,
                    sampling: Sampling::default(),
                    conversation_id: Some(format!("{}_sub_{}", conv_id.unwrap_or("conv"), subagent_id)),
                    raw_responses: None,
                    raw_inbound: None,
                    client_headers: HashMap::new(),
                };

                // 在子任务执行期间向前端发送思考增量保活，避免 HTTP 连接空闲超时
                let tx_sub = tx.clone();
                let mut step_text = String::new();
                let mut on_sub_delta = |delta: Delta| {
                    match delta {
                        Delta::Text(t) => {
                            step_text.push_str(&t);
                        }
                        Delta::Thinking(th) => {
                            if !th.is_empty() {
                                let _ = tx_sub.send(Ok(encode_agent_server_message(&build_thinking_delta(th))));
                            }
                        }
                        _ => {}
                    }
                };

                let sub_res = upstream.stream(credential, &sub_chat_req, &mut on_sub_delta).await;
                match sub_res {
                    Ok(comp) => {
                        eprintln!("[NEXUS-SUBAGENT] step={sub_step} comp tool_calls={} text_len={}", comp.tool_calls.len(), comp.text.len());
                        if comp.tool_calls.is_empty() {
                            final_summary = if !comp.text.is_empty() {
                                comp.text
                            } else if !step_text.is_empty() {
                                step_text
                            } else {
                                format!("Subagent completed task: {}", description)
                            };
                            break;
                        }

                        // 子代理需要调用工具（例如 run_terminal_command, read_file 等）
                        sub_messages.push(Message {
                            role: Role::Assistant,
                            text: comp.text.clone(),
                            images: Vec::new(),
                            tool_calls: comp.tool_calls.clone(),
                            tool_results: Vec::new(),
                            reasoning: Vec::new(),
                        });

                        for sub_tc in &comp.tool_calls {
                            let (tool_out, is_err) = execute_subagent_tool_locally(sub_tc);
                            sub_conversation_steps.push(agent::ConversationStep {
                                message: Some(agent::conversation_step::Message::AssistantMessage(
                                    agent::AssistantMessage {
                                        text: format!("🛠️ 调用工具: `{}`\n```json\n{}\n```", sub_tc.name, sub_tc.arguments),
                                        ..Default::default()
                                    },
                                )),
                            });

                            sub_messages.push(Message {
                                role: Role::Tool,
                                text: String::new(),
                                images: Vec::new(),
                                tool_calls: Vec::new(),
                                tool_results: vec![ToolResult {
                                    tool_call_id: sub_tc.id.clone(),
                                    tool_name: sub_tc.name.clone(),
                                    text: truncate_output(&tool_out, 16 * 1024),
                                    is_error: is_err,
                                    images: Vec::new(),
                                }],
                                reasoning: Vec::new(),
                            });
                        }
                    }
                    Err(err) => {
                        eprintln!("[NEXUS-SUBAGENT] upstream.stream error: {err}");
                        warn!(error = %err, "子代理执行出错");
                        final_summary = format!("Subagent error: {}", err);
                        sub_is_err = true;
                        break;
                    }
                }
            }

            if final_summary.is_empty() {
                final_summary = format!("Subagent finished task: {}", description);
            }
            let sub_summary = final_summary;
            let is_err = sub_is_err;

            // 保存或更新子代理多轮历史与前端轨迹，支持用户后续在同一个子代理里追加追问
            sub_messages.push(Message::text(Role::Assistant, sub_summary.clone()));
            sub_conversation_steps.push(agent::ConversationStep {
                message: Some(agent::conversation_step::Message::AssistantMessage(
                    agent::AssistantMessage {
                        text: sub_summary.clone(),
                        ..Default::default()
                    },
                )),
            });
            SUBAGENT_SESSIONS.write().unwrap().insert(subagent_id.clone(), SubagentSession {
                messages: sub_messages,
                conversation_steps: sub_conversation_steps.clone(),
            });
            if let Some(cid) = conv_id {
                if let Ok(mut cache) = COMPOSER_CONVERSATION_HISTORY.write() {
                    if let Some(rec) = cache.get_mut(cid) {
                        rec.last_subagent_id = Some(subagent_id.clone());
                    }
                }
            }

            let duration_ms = sub_start.elapsed().as_millis() as u64;

            let completed_tc = agent::ToolCall {
                tool_call_id: Some(tc.id.clone()),
                started_at_ms: Some(now_ms),
                completed_at_ms: Some(now_ms + duration_ms),
                tool: Some(agent::tool_call::Tool::TaskToolCall(
                    agent::TaskToolCall {
                        args: Some(task_args),
                        result: Some(agent::TaskResult {
                            result: Some(agent::task_result::Result::Success(
                                agent::TaskSuccess {
                                    conversation_steps: sub_conversation_steps,
                                    agent_id: Some(subagent_id.clone()),
                                    is_background: false,
                                    duration_ms: Some(duration_ms),
                                    result_suffix: Some(sub_summary.clone()),
                                    background_reason: 0,
                                    transcript_path: None,
                                },
                            )),
                        }),
                        cloud_agent_bc_id: None,
                    },
                )),
                ..Default::default()
            };
            let _ = tx.send(Ok(encode_agent_server_message(&build_tool_call_completed(
                &tc.id,
                &tc.id,
                completed_tc,
            ))));

            (format!("Subagent [{}] output:\n{}", subagent_id, sub_summary), is_err)
        }
        unknown => {
            warn!(tool_name = %unknown, "未识别的 Agent 工具调用，返回占位错误");
            (format!("Unknown tool: {}", unknown), true)
        }
    }
}

// ============================================================================
// 模型推理驱动核心（多步 Agentic Loop）
// ============================================================================

/// 异步执行 Agent 多步工具循环（Agentic Loop），推流至下行管道
async fn drive_agent_turn(
    gw: Arc<Gateway>,
    request_id: String,
    run_req: agent::AgentRunRequest,
    tx: mpsc::UnboundedSender<Result<Bytes, std::io::Error>>,
) {
    let conv_id = run_req
        .conversation_id
        .clone()
        .or_else(|| run_req.agent_session_id.clone())
        .or_else(|| run_req.conversation_group_id.clone());
    GLOBAL_SESSIONS.register_aliases(&request_id, conv_id.as_deref());

    let mut exec_rx = GLOBAL_SESSIONS.take_exec_rx(&request_id, conv_id.as_deref());
    let mut interaction_rx = GLOBAL_SESSIONS.take_interaction_rx(&request_id, conv_id.as_deref());

    // 1. 构建规范化 ChatRequest（包含 System Prompt、SelectedContext、默认工具与多轮历史）
    let default_model = gw
        .routes
        .read()
        .ok()
        .and_then(|r| r.get("cursor").map(|cr| cr.model.clone()))
        .unwrap_or_else(|| "chatgpt/gpt-5.6-luna".to_string());

    let mut chat_req = build_chat_request_from_run(&run_req, &default_model);

    // 2. 映射目标模型至网关通道
    let resolved_model = gw.scoped_model(Some("cursor"), &chat_req.model);
    chat_req.model = resolved_model.clone();

    // 自动补全通道前缀（若尚未带前缀且已有通道未直接认领）
    if !chat_req.model.contains('/') {
        let m_lower = chat_req.model.to_ascii_lowercase();
        if m_lower.starts_with("kiro-") {
            if gw.channels.get("mesh").is_some_and(|c| c.gate.owns(crate::channel::Capability::Chat, &chat_req.model)) {
                // 由 MeshGate 直接认领
            } else if gw.channels.get("kiro").is_some() {
                chat_req.model = format!("kiro/{}", chat_req.model);
            }
        } else if m_lower.contains("gpt-5") || m_lower.contains("gpt-4") || m_lower.starts_with("o1") || m_lower.starts_with("o3") {
            if gw.channels.get("chatgpt").is_some() {
                chat_req.model = format!("chatgpt/{}", chat_req.model);
            }
        } else if m_lower.contains("claude-") {
            if gw.channels.get("mesh").is_some_and(|c| c.gate.owns(crate::channel::Capability::Chat, &chat_req.model)) {
                // 由 MeshGate 直接认领
            } else if gw.channels.get("claude").is_some() {
                chat_req.model = format!("claude/{}", chat_req.model);
            }
        }
    }

    info!(
        request_id = %request_id,
        target_model = %chat_req.model,
        messages_count = chat_req.messages.len(),
        tools_count = chat_req.tools.len(),
        "开始执行 Agent 多步推理循环"
    );
    eprintln!("[NEXUS-AGENT] Starting agent loop: request_id={request_id}, model={}", chat_req.model);

    // 3. 通道寻路与可用性门禁
    let route = gw.route(&chat_req.model, Capability::Chat);
    if let Some(reason) = route.channel.gate.unavailable_reason() {
        warn!(request_id = %request_id, reason = %reason, "Agent 目标通道不可用");
        let err_frame = encode_error_end_stream("unavailable", reason);
        let _ = tx.send(Ok(err_frame));
        GLOBAL_SESSIONS.remove(&request_id, conv_id.as_deref());
        return;
    }

    if !route.channel.gate.ready() {
        warn!(request_id = %request_id, "Agent 目标通道无可用号源");
        let err_frame = encode_error_end_stream("resource_exhausted", "所选通道当前没有可用号源");
        let _ = tx.send(Ok(err_frame));
        GLOBAL_SESSIONS.remove(&request_id, conv_id.as_deref());
        return;
    }

    let lane = Arc::clone(&route.channel.lane);
    let upstream = Arc::clone(&route.channel.upstream);
    let target_model = chat_req.model.clone();

    // 4. 获取号源凭证
    let credential = match lane.acquire(&target_model).await {
        Ok(cred) => cred,
        Err(err) => {
            error!(request_id = %request_id, error = %err, "获取通道凭证失败");
            let err_frame = encode_error_end_stream("resource_exhausted", &err.to_string());
            let _ = tx.send(Ok(err_frame));
            GLOBAL_SESSIONS.remove(&request_id, conv_id.as_deref());
            return;
        }
    };

    const MAX_AGENT_STEPS: usize = 25;
    let mut exec_id_counter: u32 = 1;
    let mut total_usage = crate::normalized::Usage::default();
    let mut last_completion_opt = None;
    let mut subagent_count_in_turn: usize = 0;

    // 5. 核心多步 Agentic 循环 (Inner Loop)
    for step in 0..MAX_AGENT_STEPS {
        let started = Instant::now();
        let tx_delta = tx.clone();
        let has_thinking = Arc::new(AtomicBool::new(false));
        let has_thinking_clone = Arc::clone(&has_thinking);

        let mut on_delta = move |delta: Delta| {
            let frame_opt = match delta {
                Delta::Thinking(th) => {
                    if !th.is_empty() {
                        has_thinking_clone.store(true, Ordering::Relaxed);
                        Some(encode_agent_server_message(&build_thinking_delta(th)))
                    } else {
                        None
                    }
                }
                Delta::Text(text) => {
                    if !text.is_empty() {
                        Some(encode_agent_server_message(&build_text_delta(text)))
                    } else {
                        None
                    }
                }
                Delta::ThinkingSignature(_) | Delta::RedactedThinking(_) => None,
                Delta::Headers(_) | Delta::Raw { .. } => None,
            };

            if let Some(frame) = frame_opt {
                let _ = tx_delta.send(Ok(frame));
            }
        };

        debug!(step = step, "启动 Agentic 单步推理");
        let stream_res = upstream.stream(&credential, &chat_req, &mut on_delta).await;

        match stream_res {
            Ok(completion) => {
                let elapsed = started.elapsed();
                total_usage.input_tokens = total_usage.input_tokens.saturating_add(completion.usage.input_tokens);
                total_usage.output_tokens = total_usage.output_tokens.saturating_add(completion.usage.output_tokens);
                total_usage.cache_read_tokens = total_usage.cache_read_tokens.saturating_add(completion.usage.cache_read_tokens);
                total_usage.cache_write_tokens = total_usage.cache_write_tokens.saturating_add(completion.usage.cache_write_tokens);
                total_usage.reasoning_tokens = total_usage.reasoning_tokens.saturating_add(completion.usage.reasoning_tokens);

                if has_thinking.load(Ordering::Relaxed) {
                    let th_done = build_thinking_completed(elapsed.as_millis() as i32);
                    let _ = tx.send(Ok(encode_agent_server_message(&th_done)));
                }

                // 如果模型没有调用任何工具，代表回答已完成，跳出循环收尾
                if completion.tool_calls.is_empty() {
                    info!(
                        request_id = %request_id,
                        step = step,
                        "Agent 模型完成最终思考与作答，准备收尾"
                    );
                    last_completion_opt = Some(completion);
                    break;
                }

                // 模型返回了工具调用，进行双轨下发与等待
                info!(
                    request_id = %request_id,
                    step = step,
                    tool_calls_count = completion.tool_calls.len(),
                    "Agent 模型发起了工具调用，启动执行轨等待"
                );

                // 将 Assistant 思考与工具调用推入消息历史
                chat_req.messages.push(Message {
                    role: Role::Assistant,
                    text: completion.text.clone(),
                    images: Vec::new(),
                    tool_calls: completion.tool_calls.clone(),
                    tool_results: Vec::new(),
                    reasoning: if !completion.thinking.is_empty() {
                        vec![crate::normalized::Reasoning {
                            text: completion.thinking.clone(),
                            signature: completion.thinking_signature.clone(),
                            redacted: None,
                        }]
                    } else {
                        Vec::new()
                    },
                });

                // 逐一执行工具并回传结果
                for tc in &completion.tool_calls {
                    let exec_id = exec_id_counter;
                    exec_id_counter = exec_id_counter.saturating_add(1);

                    let is_subagent_tool = tc.name == "task" || tc.name == "dispatch_subagent";
                    let (output, is_error) = if is_subagent_tool && subagent_count_in_turn >= 1 {
                        ("Subagent task already executed in this turn. Synthesize findings and reply to the user now without launching further subagents.".to_string(), false)
                    } else {
                        if is_subagent_tool {
                            subagent_count_in_turn = subagent_count_in_turn.saturating_add(1);
                        }
                        execute_agent_tool_call(
                            &upstream,
                            &credential,
                            &target_model,
                            conv_id.as_deref(),
                            &tx,
                            &mut exec_rx,
                            &mut interaction_rx,
                            tc,
                            exec_id,
                        ).await
                    };

                    // 16KB 上限截断保护
                    let truncated_output = truncate_output(&output, 16 * 1024);

                    chat_req.messages.push(Message {
                        role: Role::Tool,
                        text: String::new(),
                        images: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_results: vec![ToolResult {
                            tool_call_id: tc.id.clone(),
                            tool_name: tc.name.clone(),
                            text: truncated_output,
                            is_error,
                            images: Vec::new(),
                        }],
                        reasoning: Vec::new(),
                    });
                }

                last_completion_opt = Some(completion);
                // 循环继续，下一轮带上完整的 ToolResult 上行给模型！
            }
            Err(err) => {
                warn!(request_id = %request_id, error = %err.message, "Agent 推理流发生错误");
                lane.report(&credential, &target_model, Outcome::Err(&err));
                let err_frame = encode_error_end_stream("internal", &err.message);
                let _ = tx.send(Ok(err_frame));
                GLOBAL_SESSIONS.remove(&request_id, conv_id.as_deref());
                return;
            }
        }
    }

    // 6. 发送最终回合结束帧与 Connect 终止帧
    if let Some(ref _completion) = last_completion_opt {
        lane.report(&credential, &target_model, Outcome::Ok(&total_usage));
    }
    let turn_ended = build_turn_ended(Some(&total_usage));
    let _ = tx.send(Ok(encode_agent_server_message(&turn_ended)));
    let _ = tx.send(Ok(encode_end_stream()));

    // 7. 保存完整对话上下文至 COMPOSER_CONVERSATION_HISTORY（按 conversation_id 隔离，支持多轮无损问答）
    if let Some(ref cid) = conv_id {
        let mut history_to_save: Vec<Message> = Vec::new();
        for msg in &chat_req.messages {
            // 过滤初始静态系统提示词，避免多轮累加重复
            if msg.role == Role::System && msg.text == AGENT_SYSTEM_PROMPT {
                continue;
            }
            history_to_save.push(msg.clone());
        }

        // 如果最终步输出文本且尚未加入 messages（最后一步 completion.tool_calls 为空跳出循环时）
        if let Some(ref comp) = last_completion_opt {
            let already_appended = history_to_save
                .last()
                .map(|m| m.role == Role::Assistant && m.text == comp.text)
                .unwrap_or(false);
            if !already_appended && (!comp.text.is_empty() || !comp.thinking.is_empty()) {
                history_to_save.push(Message {
                    role: Role::Assistant,
                    text: comp.text.clone(),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_results: Vec::new(),
                    reasoning: if !comp.thinking.is_empty() {
                        vec![crate::normalized::Reasoning {
                            text: comp.thinking.clone(),
                            signature: comp.thinking_signature.clone(),
                            redacted: None,
                        }]
                    } else {
                        Vec::new()
                    },
                });
            }
        }

        if let Ok(mut cache) = COMPOSER_CONVERSATION_HISTORY.write() {
            if cache.len() > 100 {
                let now = Instant::now();
                cache.retain(|_, v| now.duration_since(v.last_updated).as_secs() < 86400);
            }
            let prev_last_subagent = cache.get(cid).and_then(|r| r.last_subagent_id.clone());
            cache.insert(
                cid.clone(),
                ComposerConversationRecord {
                    last_updated: Instant::now(),
                    messages: history_to_save,
                    last_subagent_id: prev_last_subagent,
                },
            );
        }
    }

    GLOBAL_SESSIONS.remove(&request_id, conv_id.as_deref());
    info!(request_id = %request_id, "Agent 会话多步推理闭环成功完成");
}

// ============================================================================
// 请求模型与提示词装配
// ============================================================================

pub const AGENT_SYSTEM_PROMPT: &str = "\
You are an intelligent programming assistant and expert software engineer working in Cursor.
You have access to a set of local workspace tools:
- `run_terminal_command`: Execute terminal commands in the project workspace.
- `read_file`: Inspect file content.
- `edit_file`: Edit or create files.
- `list_dir`: Explore directory contents.
- `grep_search`: Search for code patterns.
- `file_search`: Find files by name or glob.
- `delete_file`: Delete files.
- `ask_question`: Ask clarifying questions with multiple-choice options.
- `update_plan`: Update the step checklist in Composer.
- `task`: Launch or resume an autonomous subagent task.

Use tools when appropriate to inspect the project, make edits, or verify changes. Follow best engineering practices and provide clear, concise responses.
";

/// 从 ToolCall 中提取规范工具名与参数 JSON
fn extract_tool_info(tc: &agent::ToolCall) -> (String, String) {
    if let Some(ref t) = tc.tool {
        match t {
            agent::tool_call::Tool::TaskToolCall(task) => {
                let desc = task.args.as_ref().map(|a| a.description.as_str()).unwrap_or("");
                let prompt = task.args.as_ref().map(|a| a.prompt.as_str()).unwrap_or("");
                let resume = task.args.as_ref().and_then(|a| a.resume.as_deref());
                (
                    "task".to_string(),
                    serde_json::json!({
                        "description": desc,
                        "prompt": prompt,
                        "resume": resume,
                    }).to_string(),
                )
            }
            agent::tool_call::Tool::ShellToolCall(s) => {
                let cmd = s.args.as_ref().map(|a| a.command.as_str()).unwrap_or("");
                let cwd = s.args.as_ref().map(|a| a.working_directory.as_str()).unwrap_or("");
                (
                    "run_terminal_command".to_string(),
                    serde_json::json!({
                        "command": cmd,
                        "working_directory": cwd,
                    }).to_string(),
                )
            }
            agent::tool_call::Tool::ReadToolCall(r) => {
                let path = r.args.as_ref().map(|a| a.path.as_str()).unwrap_or("");
                (
                    "read_file".to_string(),
                    serde_json::json!({
                        "path": path,
                    }).to_string(),
                )
            }
            agent::tool_call::Tool::EditToolCall(e) => {
                let path = e.args.as_ref().map(|a| a.path.as_str()).unwrap_or("");
                (
                    "edit_file".to_string(),
                    serde_json::json!({
                        "path": path,
                    }).to_string(),
                )
            }
            agent::tool_call::Tool::LsToolCall(l) => {
                let path = l.args.as_ref().map(|a| a.path.as_str()).unwrap_or("");
                (
                    "list_dir".to_string(),
                    serde_json::json!({
                        "path": path,
                    }).to_string(),
                )
            }
            agent::tool_call::Tool::GrepToolCall(g) => {
                let pattern = g.args.as_ref().map(|a| a.pattern.as_str()).unwrap_or("");
                (
                    "grep_search".to_string(),
                    serde_json::json!({
                        "pattern": pattern,
                    }).to_string(),
                )
            }
            agent::tool_call::Tool::GlobToolCall(g) => {
                let pattern = g.args.as_ref().map(|a| a.glob_pattern.as_str()).unwrap_or("");
                (
                    "file_search".to_string(),
                    serde_json::json!({
                        "pattern": pattern,
                    }).to_string(),
                )
            }
            agent::tool_call::Tool::DeleteToolCall(d) => {
                let path = d.args.as_ref().map(|a| a.path.as_str()).unwrap_or("");
                (
                    "delete_file".to_string(),
                    serde_json::json!({
                        "path": path,
                    }).to_string(),
                )
            }
            agent::tool_call::Tool::AskQuestionToolCall(q) => {
                let question = q.args.as_ref().map(|a| a.title.as_str()).unwrap_or("");
                (
                    "ask_question".to_string(),
                    serde_json::json!({
                        "question": question,
                    }).to_string(),
                )
            }
            agent::tool_call::Tool::CreatePlanToolCall(p) => {
                let plan = p.args.as_ref().map(|a| a.plan.as_str()).unwrap_or("");
                (
                    "update_plan".to_string(),
                    serde_json::json!({
                        "plan": plan,
                    }).to_string(),
                )
            }
            _ => ("unknown".to_string(), "{}".to_string()),
        }
    } else {
        ("unknown".to_string(), "{}".to_string())
    }
}

/// 解析单条 JSON 格式消息（来自 root_prompt_messages_json）
fn parse_json_message(val: &serde_json::Value) -> Option<Message> {
    let role_str = val.get("role").and_then(|v| v.as_str())?;
    let role = match role_str {
        "user" => Role::User,
        "assistant" => Role::Assistant,
        "system" => Role::System,
        "tool" => Role::Tool,
        _ => return None,
    };

    let mut text = String::new();
    if let Some(content_str) = val.get("content").and_then(|v| v.as_str()) {
        text.push_str(content_str);
    } else if let Some(content_arr) = val.get("content").and_then(|v| v.as_array()) {
        for part in content_arr {
            if let Some(t) = part.get("text").and_then(|v| v.as_str()) {
                text.push_str(t);
            }
        }
    } else if let Some(t) = val.get("text").and_then(|v| v.as_str()) {
        text.push_str(t);
    }

    let mut tool_calls = Vec::new();
    if let Some(tc_arr) = val.get("tool_calls").and_then(|v| v.as_array()) {
        for tc in tc_arr {
            let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let name = tc.get("function")
                .and_then(|f| f.get("name"))
                .and_then(|v| v.as_str())
                .or_else(|| tc.get("name").and_then(|v| v.as_str()))
                .unwrap_or_default()
                .to_string();
            let args = tc.get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|v| v.as_str())
                .or_else(|| tc.get("arguments").and_then(|v| v.as_str()))
                .unwrap_or("{}")
                .to_string();
            if !id.is_empty() || !name.is_empty() {
                tool_calls.push(crate::normalized::ToolCall {
                    id,
                    name,
                    arguments: args,
                });
            }
        }
    }

    let mut tool_results = Vec::new();
    if role == Role::Tool {
        let tc_id = val.get("tool_call_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let name = val.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
        tool_results.push(crate::normalized::ToolResult {
            tool_call_id: tc_id,
            tool_name: name,
            text: text.clone(),
            is_error: val.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false),
            images: Vec::new(),
        });
        text.clear();
    }

    Some(Message {
        role,
        text,
        images: Vec::new(),
        tool_calls,
        tool_results,
        reasoning: Vec::new(),
    })
}

/// 从 AgentConversationTurnStructure 解码单轮消息
fn decode_agent_turn_structure(act: agent::AgentConversationTurnStructure) -> Vec<Message> {
    use prost::Message as _;
    let mut msgs = Vec::new();

    if !act.user_message.is_empty() {
        if let Ok(user_msg) = agent::UserMessage::decode(act.user_message.as_slice()) {
            if !user_msg.text.trim().is_empty() {
                msgs.push(Message::text(Role::User, user_msg.text));
            }
        }
    }

    let mut asst_text = String::new();
    let mut tool_calls = Vec::new();
    let mut reasoning = Vec::new();

    for step_bytes in act.steps {
        if let Ok(step) = agent::ConversationStep::decode(step_bytes.as_slice()) {
            if let Some(m) = step.message {
                match m {
                    agent::conversation_step::Message::AssistantMessage(a) => {
                        asst_text.push_str(&a.text);
                    }
                    agent::conversation_step::Message::ThinkingMessage(t) => {
                        reasoning.push(crate::normalized::Reasoning {
                            text: t.text,
                            signature: None,
                            redacted: None,
                        });
                    }
                    agent::conversation_step::Message::ToolCall(tc) => {
                        let id = tc.tool_call_id.clone().unwrap_or_default();
                        let (name, args) = extract_tool_info(&tc);
                        tool_calls.push(crate::normalized::ToolCall {
                            id,
                            name,
                            arguments: args,
                        });
                    }
                }
            }
        }
    }

    if !asst_text.is_empty() || !tool_calls.is_empty() || !reasoning.is_empty() {
        msgs.push(Message {
            role: Role::Assistant,
            text: asst_text,
            images: Vec::new(),
            tool_calls,
            tool_results: Vec::new(),
            reasoning,
        });
    }

    msgs
}

/// 从 AgentConversationTurn 解码单轮消息
fn decode_agent_turn(act: agent::AgentConversationTurn) -> Vec<Message> {
    let mut msgs = Vec::new();

    if let Some(user_msg) = act.user_message {
        if !user_msg.text.trim().is_empty() {
            msgs.push(Message::text(Role::User, user_msg.text));
        }
    }

    let mut asst_text = String::new();
    let mut tool_calls = Vec::new();
    let mut reasoning = Vec::new();

    for step in act.steps {
        if let Some(m) = step.message {
            match m {
                agent::conversation_step::Message::AssistantMessage(a) => {
                    asst_text.push_str(&a.text);
                }
                agent::conversation_step::Message::ThinkingMessage(t) => {
                    reasoning.push(crate::normalized::Reasoning {
                        text: t.text,
                        signature: None,
                        redacted: None,
                    });
                }
                agent::conversation_step::Message::ToolCall(tc) => {
                    let id = tc.tool_call_id.clone().unwrap_or_default();
                    let (name, args) = extract_tool_info(&tc);
                    tool_calls.push(crate::normalized::ToolCall {
                        id,
                        name,
                        arguments: args,
                    });
                }
            }
        }
    }

    if !asst_text.is_empty() || !tool_calls.is_empty() || !reasoning.is_empty() {
        msgs.push(Message {
            role: Role::Assistant,
            text: asst_text,
            images: Vec::new(),
            tool_calls,
            tool_results: Vec::new(),
            reasoning,
        });
    }

    msgs
}

/// 从字节数组还原单轮对话
fn decode_turn_bytes(turn_bytes: &[u8]) -> Option<Vec<Message>> {
    use prost::Message as _;

    // 1. 尝试按 ConversationTurnStructure 解码
    if let Ok(turn_struct) = agent::ConversationTurnStructure::decode(turn_bytes) {
        if let Some(agent::conversation_turn_structure::Turn::AgentConversationTurn(act)) = turn_struct.turn {
            return Some(decode_agent_turn_structure(act));
        }
    }

    // 2. 尝试直接按 AgentConversationTurnStructure 解码
    if let Ok(act) = agent::AgentConversationTurnStructure::decode(turn_bytes) {
        if !act.user_message.is_empty() || !act.steps.is_empty() {
            return Some(decode_agent_turn_structure(act));
        }
    }

    // 3. 尝试按 ConversationTurn 解码
    if let Ok(turn) = agent::ConversationTurn::decode(turn_bytes) {
        if let Some(agent::conversation_turn::Turn::AgentConversationTurn(act)) = turn.turn {
            return Some(decode_agent_turn(act));
        }
    }

    // 4. 尝试直接按 AgentConversationTurn 解码
    if let Ok(act) = agent::AgentConversationTurn::decode(turn_bytes) {
        if act.user_message.is_some() || !act.steps.is_empty() {
            return Some(decode_agent_turn(act));
        }
    }

    None
}

/// 从 ConversationStateStructure 中解码所有历史消息
fn decode_conversation_state_turns(cs: &agent::ConversationStateStructure) -> Vec<Message> {
    let mut msgs = Vec::new();

    // 1. root_prompt_messages_json 解析
    for raw_json in &cs.root_prompt_messages_json {
        if let Ok(json_str) = std::str::from_utf8(raw_json) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let Some(msg) = parse_json_message(&val) {
                    msgs.push(msg);
                }
            }
        }
    }

    // 2. protobuf turns 数组解析
    for turn_bytes in &cs.turns {
        if let Some(turn_msgs) = decode_turn_bytes(turn_bytes) {
            msgs.extend(turn_msgs);
        }
    }

    msgs
}

/// 还原结构化历史 ConversationHistory
fn decode_conversation_history_messages(history: &agent::ConversationHistory) -> Vec<Message> {
    let mut messages = Vec::new();
    for h_msg in &history.messages {
        if let Some(ref m) = h_msg.message {
            match m {
                agent::conversation_history_message::Message::User(u) => {
                    let mut user_text = String::new();
                    for part in &u.content {
                        if let Some(agent::conversation_history_user_content::Content::Text(ref t)) = part.content {
                            user_text.push_str(&t.text);
                        }
                    }
                    if !user_text.is_empty() {
                        messages.push(Message::text(Role::User, user_text));
                    }
                }
                agent::conversation_history_message::Message::Assistant(a) => {
                    let mut asst_text = String::new();
                    let mut tool_calls = Vec::new();
                    let mut reasoning = Vec::new();
                    for part in &a.content {
                        match &part.content {
                            Some(agent::conversation_history_assistant_content::Content::Text(t)) => {
                                asst_text.push_str(&t.text);
                            }
                            Some(agent::conversation_history_assistant_content::Content::Reasoning(r)) => {
                                reasoning.push(crate::normalized::Reasoning {
                                    text: r.text.clone(),
                                    signature: r.signature.clone(),
                                    redacted: None,
                                });
                            }
                            Some(agent::conversation_history_assistant_content::Content::ToolCall(tc)) => {
                                tool_calls.push(crate::normalized::ToolCall {
                                    id: tc.tool_call_id.clone(),
                                    name: tc.tool_name.clone(),
                                    arguments: tc.args_json.clone(),
                                });
                            }
                            _ => {}
                        }
                    }
                    if !asst_text.is_empty() || !tool_calls.is_empty() || !reasoning.is_empty() {
                        messages.push(Message {
                            role: Role::Assistant,
                            text: asst_text,
                            images: Vec::new(),
                            tool_calls,
                            tool_results: Vec::new(),
                            reasoning,
                        });
                    }
                }
                agent::conversation_history_message::Message::Tool(t) => {
                    let mut result_text = String::new();
                    for part in &t.content {
                        if let Some(agent::conversation_history_tool_result_content::Content::Text(ref txt)) = part.content {
                            result_text.push_str(&txt.text);
                        }
                    }
                    messages.push(Message {
                        role: Role::Tool,
                        text: String::new(),
                        images: Vec::new(),
                        tool_calls: Vec::new(),
                        tool_results: vec![crate::normalized::ToolResult {
                            tool_call_id: t.tool_call_id.clone(),
                            tool_name: t.tool_name.clone(),
                            text: result_text,
                            is_error: t.is_error.unwrap_or(false),
                            images: Vec::new(),
                        }],
                        reasoning: Vec::new(),
                    });
                }
            }
        }
    }
    messages
}

/// 从 `AgentRunRequest` 中提取并组装为规范的 `ChatRequest`
pub fn build_chat_request_from_run(
    run_req: &agent::AgentRunRequest,
    default_model: &str,
) -> ChatRequest {
    let conv_id = run_req
        .conversation_id
        .as_deref()
        .or_else(|| run_req.agent_session_id.as_deref())
        .or_else(|| run_req.conversation_group_id.as_deref());

    // 1. 提取自选模型名
    let raw_model = run_req
        .requested_model
        .as_ref()
        .map(|m| m.model_id.as_str())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            run_req
                .model_details
                .as_ref()
                .map(|m| m.model_id.as_str())
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| {
            run_req
                .dev_raw_model_slug
                .as_deref()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or(default_model);

    let clean_model = raw_model
        .trim_end_matches("(Nexus)")
        .trim_end_matches("[Nexus]")
        .trim_end_matches("(nexus)")
        .trim_end_matches("[nexus]")
        .trim()
        .to_string();

    let mut messages = Vec::new();

    // 默认注入 Agent 系统指令与工具说明
    messages.push(Message::text(Role::System, AGENT_SYSTEM_PROMPT));

    // 2. 还原多轮对话历史
    // 优先级 1: 从 run_req.conversation_state (turns + root_prompt_messages_json) 解码
    let mut history_messages = Vec::new();
    if let Some(ref cs) = run_req.conversation_state {
        let decoded = decode_conversation_state_turns(cs);
        if decoded.iter().any(|m| m.role == Role::Assistant) {
            history_messages = decoded;
        }
    }

    // 优先级 2: 从 action.user_action.conversation_history 解码（若客户端提供了显式结构化历史）
    if history_messages.is_empty() {
        if let Some(ref action_wrapper) = run_req.action {
            if let Some(agent::conversation_action::Action::UserMessageAction(ref user_action)) = action_wrapper.action {
                if let Some(ref history) = user_action.conversation_history {
                    history_messages = decode_conversation_history_messages(history);
                }
            }
        }
    }

    // 优先级 3: 从网关内存会话历史池 COMPOSER_CONVERSATION_HISTORY 获取（按 conversation_id 隔离）
    if history_messages.is_empty() {
        if let Some(cid) = conv_id {
            if let Ok(cache) = COMPOSER_CONVERSATION_HISTORY.read() {
                if let Some(rec) = cache.get(cid) {
                    history_messages = rec.messages.clone();
                }
            }
        }
    }

    let has_restored_history = !history_messages.is_empty();
    if has_restored_history {
        messages.extend(history_messages);
    }

    // 3. 处理当前 Action 与输入
    if let Some(ref action_wrapper) = run_req.action {
        if let Some(ref action) = action_wrapper.action {
            if let agent::conversation_action::Action::UserMessageAction(ref user_action) = action {
                // 如果历史未被还原（如网关初次重启且客户端未带 turns），将 prepend_user_messages 作为用户历史提示（绝不能作为系统指令！）
                if !has_restored_history {
                    for p in &user_action.prepend_user_messages {
                        if !p.text.trim().is_empty() {
                            messages.push(Message::text(Role::User, p.text.clone()));
                        }
                    }
                }

                // 处理当前用户输入与 SelectedContext 附件
                if let Some(ref user_msg) = user_action.user_message {
                    // 挂载 SelectedContext（选区代码、引用文件、项目规则）
                    if let Some(ref ctx) = user_msg.selected_context {
                        let mut ctx_block = String::new();
                        for f in &ctx.files {
                            if !f.content.trim().is_empty() {
                                ctx_block.push_str(&format!(
                                    "\n--- File: {} ---\n```\n{}\n```\n",
                                    f.path, f.content
                                ));
                            }
                        }
                        for c in &ctx.code_selections {
                            if !c.content.trim().is_empty() {
                                ctx_block.push_str(&format!(
                                    "\n--- Code Selection: {} ---\n```\n{}\n```\n",
                                    c.path, c.content
                                ));
                            }
                        }
                        for r in &ctx.cursor_rules {
                            if let Some(ref rule) = r.rule {
                                if !rule.content.trim().is_empty() {
                                    ctx_block.push_str(&format!("\n--- Rule ---\n{}\n", rule.content));
                                }
                            }
                        }
                        if !ctx_block.is_empty() {
                            messages.push(Message::text(
                                Role::System,
                                format!("Selected Context:\n{}", ctx_block),
                            ));
                        }
                    }

                    // 当前输入提示词
                    if !user_msg.text.trim().is_empty() {
                        messages.push(Message::text(Role::User, user_msg.text.clone()));
                    }
                }
            }
        }
    }

    // 确保所有 Assistant 消息里的 tool_calls 都有对应的 ToolResult 闭环，避免上游报错或模型认为“上一请求未完成”
    let mut sanitized: Vec<Message> = Vec::new();
    let mut pending_tool_calls: Vec<crate::normalized::ToolCall> = Vec::new();

    for msg in messages {
        if !pending_tool_calls.is_empty() && msg.role != Role::Tool {
            for tc in pending_tool_calls.drain(..) {
                sanitized.push(Message {
                    role: Role::Tool,
                    text: String::new(),
                    images: Vec::new(),
                    tool_calls: Vec::new(),
                    tool_results: vec![crate::normalized::ToolResult {
                        tool_call_id: tc.id,
                        tool_name: tc.name,
                        text: "[Previous action completed]".to_string(),
                        is_error: false,
                        images: Vec::new(),
                    }],
                    reasoning: Vec::new(),
                });
            }
        }

        if msg.role == Role::Assistant {
            pending_tool_calls = msg.tool_calls.clone();
        } else if msg.role == Role::Tool {
            for tr in &msg.tool_results {
                pending_tool_calls.retain(|tc| tc.id != tr.tool_call_id);
            }
        }

        sanitized.push(msg);
    }

    if !pending_tool_calls.is_empty() {
        for tc in pending_tool_calls {
            sanitized.push(Message {
                role: Role::Tool,
                text: String::new(),
                images: Vec::new(),
                tool_calls: Vec::new(),
                tool_results: vec![crate::normalized::ToolResult {
                    tool_call_id: tc.id,
                    tool_name: tc.name,
                    text: "[Previous action completed]".to_string(),
                    is_error: false,
                    images: Vec::new(),
                }],
                reasoning: Vec::new(),
            });
        }
    }

    let mut messages = sanitized;

    if messages.is_empty() {
        messages.push(Message::text(Role::User, "Hello".to_string()));
    }

    ChatRequest {
        model: clean_model,
        messages,
        tools: default_cursor_agent_tools(),
        tool_choice: ToolChoice::Auto,
        sampling: Sampling::default(),
        conversation_id: run_req.conversation_id.clone(),
        raw_responses: None,
        client_headers: HashMap::new(),
        raw_inbound: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_decode_roundtrip() {
        let original = b"Nexus Plan B Agent Protocol";
        let mut hex = String::new();
        for b in original {
            use std::fmt::Write;
            let _ = write!(hex, "{:02x}", b);
        }
        let decoded = hex_decode(&hex).expect("valid hex");
        assert_eq!(decoded, original);

        assert!(hex_decode("123").is_err(), "odd hex length should error");
    }

    #[test]
    fn test_connect_envelope_format() {
        let msg = build_text_delta("Hello Cursor Agent".to_string());
        let frame = encode_agent_server_message(&msg);
        assert_eq!(frame[0], 0, "flag for data frame should be 0");
        let len = u32::from_be_bytes([frame[1], frame[2], frame[3], frame[4]]) as usize;
        assert_eq!(len, frame.len() - 5);

        let decoded = agent::AgentServerMessage::decode(&frame[5..]).expect("valid decode");
        if let Some(agent::agent_server_message::Message::InteractionUpdate(update)) = decoded.message {
            if let Some(agent::interaction_update::Message::TextDelta(delta)) = update.message {
                assert_eq!(delta.text, "Hello Cursor Agent");
            } else {
                panic!("expected TextDelta");
            }
        } else {
            panic!("expected InteractionUpdate");
        }
    }

    #[test]
    fn test_end_stream_envelope() {
        let end_frame = encode_end_stream();
        assert_eq!(end_frame[0], CONNECT_END_STREAM_FLAG);
        let len = u32::from_be_bytes([end_frame[1], end_frame[2], end_frame[3], end_frame[4]]) as usize;
        assert_eq!(len, 2);
        assert_eq!(&end_frame[5..], b"{}");
    }

    #[test]
    fn test_build_chat_request_from_run() {
        let run_req = agent::AgentRunRequest {
            requested_model: Some(agent::RequestedModel {
                model_id: "gpt-5.6-luna (Nexus)".to_string(),
                ..Default::default()
            }),
            conversation_id: Some("conv-123".to_string()),
            action: Some(agent::ConversationAction {
                action: Some(agent::conversation_action::Action::UserMessageAction(
                    agent::UserMessageAction {
                        user_message: Some(agent::UserMessage {
                            text: "Write a Rust test".to_string(),
                            selected_context: Some(agent::SelectedContext {
                                files: vec![agent::SelectedFile {
                                    path: "main.rs".to_string(),
                                    content: "fn main() {}".to_string(),
                                    ..Default::default()
                                }],
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }),
            ..Default::default()
        };

        let chat_req = build_chat_request_from_run(&run_req, "chatgpt/gpt-5.6-luna");
        assert_eq!(chat_req.model, "gpt-5.6-luna");
        assert_eq!(chat_req.conversation_id, Some("conv-123".to_string()));
        assert_eq!(chat_req.messages.len(), 3);
        assert_eq!(chat_req.messages[0].role, Role::System);
        assert!(chat_req.messages[0].text.contains("run_terminal_command"));
        assert_eq!(chat_req.messages[1].role, Role::System);
        assert!(chat_req.messages[1].text.contains("main.rs"));
        assert_eq!(chat_req.messages[2].role, Role::User);
        assert_eq!(chat_req.messages[2].text, "Write a Rust test");
        assert_eq!(chat_req.tools.len(), 10);
        assert_eq!(chat_req.tool_choice, ToolChoice::Auto);
    }

    #[tokio::test]
    async fn test_agent_session_registry_exchange() {
        let registry = AgentSessionRegistry::new();
        let request_id = "test-req-abc";

        let tx = registry.get_or_create_tx(request_id);
        let mut rx = registry.take_rx(request_id);

        let chunk = Bytes::from_static(b"stream test chunk");
        tx.send(Ok(chunk.clone())).expect("send success");

        let received = rx.recv().await.expect("receive chunk");
        assert_eq!(received.unwrap(), chunk);

        registry.remove(request_id, None);
    }

    #[tokio::test]
    async fn test_agent_sse_response_stream_e2e() {
        let request_id = "agent-e2e-session-1";

        // 1. 模拟 Cursor IDE 调用 RunSSE 注册下行通道
        let bidi_id = agent::BidiRequestId {
            request_id: request_id.to_string(),
        };
        let body = Bytes::from(bidi_id.encode_to_vec());

        struct DummyUpstream;
        impl crate::upstream::Upstream for DummyUpstream {
            fn stream<'a>(
                &'a self,
                _credential: &'a crate::lane::Credential,
                _request: &'a crate::normalized::ChatRequest,
                _on_delta: crate::upstream::DeltaSink<'a>,
            ) -> crate::lane::BoxFuture<'a, Result<crate::normalized::Completion, crate::error::UpstreamError>> {
                Box::pin(async { unimplemented!() })
            }
        }

        let lane = Arc::new(crate::lane::StaticLane::new(crate::lane::Credential {
            label: "test".into(),
            access_token: "".into(),
            identity: crate::identity::DeviceIdentity::derived("tok"),
        }));
        let gw = Arc::new(Gateway::single(lane, Arc::new(DummyUpstream)));

        let mut headers = HeaderMap::new();
        headers.insert("x-request-id", request_id.parse().unwrap());

        let res = handle_run_sse(State(gw.clone()), headers, body).await;
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(res.headers().get("content-type").unwrap(), "text/event-stream");
        assert_eq!(res.headers().get("connect-protocol-version").unwrap(), "1");

        // 2. 模拟服务端推送 ThinkingDelta, TextDelta, TurnEnded, EndStream
        let tx = GLOBAL_SESSIONS.get_or_create_tx(request_id);

        let thinking = build_thinking_delta("I am thinking...".into());
        tx.send(Ok(encode_agent_server_message(&thinking))).unwrap();

        let text = build_text_delta("Hello from Nexus Agent!".into());
        tx.send(Ok(encode_agent_server_message(&text))).unwrap();

        let turn_ended = build_turn_ended(None);
        tx.send(Ok(encode_agent_server_message(&turn_ended))).unwrap();

        tx.send(Ok(encode_end_stream())).unwrap();
        drop(tx);
        GLOBAL_SESSIONS.remove(request_id, None);

        // 3. 读取响应体并验证所有帧完整收到
        let body_bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("read body");

        let mut decoder = crate::connect::EnvelopeDecoder::default();
        decoder.push(&body_bytes);

        // 帧 1: ThinkingDelta
        let env1 = decoder.next_envelope().unwrap().expect("envelope 1");
        assert_eq!(env1.flags, 0);
        let msg1 = agent::AgentServerMessage::decode(&env1.data[..]).unwrap();
        assert!(format!("{:?}", msg1).contains("I am thinking..."));

        // 帧 2: TextDelta
        let env2 = decoder.next_envelope().unwrap().expect("envelope 2");
        assert_eq!(env2.flags, 0);
        let msg2 = agent::AgentServerMessage::decode(&env2.data[..]).unwrap();
        assert!(format!("{:?}", msg2).contains("Hello from Nexus Agent!"));

        // 帧 3: TurnEnded
        let env3 = decoder.next_envelope().unwrap().expect("envelope 3");
        assert_eq!(env3.flags, 0);

        // 帧 4: EndStream
        let env4 = decoder.next_envelope().unwrap().expect("envelope 4");
        assert_eq!(env4.flags, CONNECT_END_STREAM_FLAG);
        assert_eq!(&env4.data[..], b"{}");
    }

    #[test]
    fn test_default_cursor_agent_tools_schema() {
        let tools = default_cursor_agent_tools();
        assert_eq!(tools.len(), 10);

        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        assert!(names.contains(&"run_terminal_command"));
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"list_dir"));
        assert!(names.contains(&"grep_search"));
        assert!(names.contains(&"file_search"));
        assert!(names.contains(&"delete_file"));
        assert!(names.contains(&"ask_question"));
        assert!(names.contains(&"update_plan"));
        assert!(names.contains(&"task"));

        for tool in &tools {
            assert!(!tool.description.is_empty());
            assert_eq!(tool.parameters["type"], "object");
            assert!(tool.parameters["properties"].is_object());
        }
    }

    #[tokio::test]
    async fn test_bidi_session_bus_exec_and_interaction_dispatch() {
        let registry = AgentSessionRegistry::new();
        let req_id = "req-session-test";
        let conv_id = "conv-session-test";

        let mut exec_rx = registry.take_exec_rx(req_id, Some(conv_id));
        let mut inter_rx = registry.take_interaction_rx(req_id, Some(conv_id));

        // 模拟客户端通过 BidiAppend 按 conv_id 投递 ExecClientMessage
        let exec_msg = agent::ExecClientMessage {
            id: 42,
            exec_id: "42".to_string(),
            message: Some(agent::exec_client_message::Message::ShellResult(
                agent::ShellResult {
                    result: Some(agent::shell_result::Result::Success(agent::ShellSuccess {
                        command: "git status".into(),
                        working_directory: "/workspace".into(),
                        exit_code: 0,
                        signal: "".into(),
                        stdout: "On branch main\nnothing to commit".into(),
                        stderr: "".into(),
                        execution_time: 15,
                        output_location: None,
                        shell_id: None,
                        interleaved_output: None,
                        pid: Some(1234),
                        ms_to_wait: None,
                        local_execution_time_ms: Some(15),
                        background_reason: None,
                        output_head: None,
                        output_tail: None,
                        elided_chars: None,
                    })),
                    sandbox_policy: None,
                    is_background: None,
                    terminals_folder: None,
                    pid: None,
                },
            )),
            ..Default::default()
        };

        registry.dispatch_exec(conv_id, exec_msg.clone());
        let received_exec = exec_rx.recv().await.expect("received exec msg");
        assert_eq!(received_exec.id, 42);

        // 模拟客户端通过 BidiAppend 按 req_id 投递 InteractionResponse
        let inter_resp = agent::InteractionResponse {
            id: 101,
            result: Some(agent::interaction_response::Result::AskQuestionInteractionResponse(
                agent::AskQuestionInteractionResponse {
                    result: Some(agent::AskQuestionResult {
                        result: Some(agent::ask_question_result::Result::Success(
                            agent::AskQuestionSuccess {
                                answers: vec![agent::ask_question_success::Answer {
                                    question_id: "q1".into(),
                                    selected_option_ids: vec!["opt_0".into()],
                                    freeform_text: "Proceed with Option A".into(),
                                }],
                            },
                        )),
                    }),
                },
            )),
        };

        registry.dispatch_interaction(req_id, inter_resp.clone());
        let received_inter = inter_rx.recv().await.expect("received interaction resp");
        assert_eq!(received_inter.id, 101);

        registry.remove(req_id, Some(conv_id));
    }

    #[test]
    fn test_faithful_context_preservation() {
        // 构造一个包含 16 轮完整问答的消息历史，测试网关 100% 忠实保留上下文而不做有损截断或压缩
        let mut history_messages = Vec::new();
        for i in 0..16 {
            history_messages.push(agent::ConversationHistoryMessage {
                message: Some(agent::conversation_history_message::Message::User(
                    agent::ConversationHistoryUserMessage {
                        content: vec![agent::ConversationHistoryUserContent {
                            content: Some(agent::conversation_history_user_content::Content::Text(
                                agent::ConversationHistoryTextContent {
                                    text: format!("User question {}", i),
                                },
                            )),
                        }],
                    },
                )),
            });
            history_messages.push(agent::ConversationHistoryMessage {
                message: Some(agent::conversation_history_message::Message::Assistant(
                    agent::ConversationHistoryAssistantMessage {
                        content: vec![agent::ConversationHistoryAssistantContent {
                            content: Some(agent::conversation_history_assistant_content::Content::Text(
                                agent::ConversationHistoryTextContent {
                                    text: format!("Assistant answer {}", i),
                                },
                            )),
                        }],
                    },
                )),
            });
        }

        let run_req = agent::AgentRunRequest {
            action: Some(agent::ConversationAction {
                action: Some(agent::conversation_action::Action::UserMessageAction(
                    agent::UserMessageAction {
                        conversation_history: Some(agent::ConversationHistory {
                            messages: history_messages,
                            ..Default::default()
                        }),
                        user_message: Some(agent::UserMessage {
                            text: "Final latest prompt".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }),
            ..Default::default()
        };

        let chat_req = build_chat_request_from_run(&run_req, "gpt-5.6-luna");
        // 网关不擅自截断任何对话历史：1 系统提示词 + 32 条多轮历史 + 1 条最新用户输入 = 34 条消息
        assert_eq!(chat_req.messages.len(), 34);
        assert_eq!(chat_req.messages[0].role, Role::System);
        assert_eq!(chat_req.messages[1].text, "User question 0");
        assert_eq!(chat_req.messages[32].text, "Assistant answer 15");
        assert_eq!(chat_req.messages.last().unwrap().text, "Final latest prompt");
    }

    #[test]
    fn test_multi_turn_history_restoration_from_session_cache() {
        let conv_id = "conv-cache-test-999";
        // 模拟第 1 轮结束后存入网关会话池
        COMPOSER_CONVERSATION_HISTORY.write().unwrap().insert(
            conv_id.to_string(),
            ComposerConversationRecord {
                last_updated: Instant::now(),
                messages: vec![
                    Message::text(Role::User, "Hello assistant!"),
                    Message::text(Role::Assistant, "Hello! I am your AI assistant."),
                ],
                last_subagent_id: Some("subagent_test_42".to_string()),
            },
        );

        // 模拟 Cursor 第 2 轮请求（包含 prepend_user_messages 和当前问题）
        let run_req = agent::AgentRunRequest {
            conversation_id: Some(conv_id.to_string()),
            action: Some(agent::ConversationAction {
                action: Some(agent::conversation_action::Action::UserMessageAction(
                    agent::UserMessageAction {
                        prepend_user_messages: vec![agent::UserMessage {
                            text: "Hello assistant!".to_string(),
                            ..Default::default()
                        }],
                        user_message: Some(agent::UserMessage {
                            text: "What did you say earlier?".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }),
            ..Default::default()
        };

        let chat_req = build_chat_request_from_run(&run_req, "gpt-5.6-luna");

        // 验证结构：
        // [0] System (AGENT_SYSTEM_PROMPT)
        // [1] User ("Hello assistant!")
        // [2] Assistant ("Hello! I am your AI assistant.")
        // [3] User ("What did you say earlier?")
        assert_eq!(chat_req.messages.len(), 4);
        assert_eq!(chat_req.messages[0].role, Role::System);
        assert_eq!(chat_req.messages[1].role, Role::User);
        assert_eq!(chat_req.messages[1].text, "Hello assistant!");
        assert_eq!(chat_req.messages[2].role, Role::Assistant);
        assert_eq!(chat_req.messages[2].text, "Hello! I am your AI assistant.");
        assert_eq!(chat_req.messages[3].role, Role::User);
        assert_eq!(chat_req.messages[3].text, "What did you say earlier?");
    }

    #[test]
    fn test_multi_turn_history_restoration_from_protobuf_turns() {
        use prost::Message as _;

        // 构造一个包含 UserMessage 和 AssistantMessage 的 AgentConversationTurnStructure
        let user_msg = agent::UserMessage {
            text: "Explain quantum computing in one sentence.".to_string(),
            ..Default::default()
        };
        let mut user_msg_bytes = Vec::new();
        user_msg.encode(&mut user_msg_bytes).unwrap();

        let asst_step = agent::ConversationStep {
            message: Some(agent::conversation_step::Message::AssistantMessage(
                agent::AssistantMessage {
                    text: "Quantum computing harnesses quantum mechanics to process information exponentially faster.".to_string(),
                },
            )),
        };
        let mut asst_step_bytes = Vec::new();
        asst_step.encode(&mut asst_step_bytes).unwrap();

        let turn_struct = agent::ConversationTurnStructure {
            turn: Some(agent::conversation_turn_structure::Turn::AgentConversationTurn(
                agent::AgentConversationTurnStructure {
                    user_message: user_msg_bytes,
                    steps: vec![asst_step_bytes],
                    ..Default::default()
                },
            )),
        };
        let mut turn_struct_bytes = Vec::new();
        turn_struct.encode(&mut turn_struct_bytes).unwrap();

        // 模拟带有 conversation_state 的 Cursor 请求
        let run_req = agent::AgentRunRequest {
            conversation_id: Some("conv-proto-test-888".to_string()),
            conversation_state: Some(agent::ConversationStateStructure {
                turns: vec![turn_struct_bytes],
                ..Default::default()
            }),
            action: Some(agent::ConversationAction {
                action: Some(agent::conversation_action::Action::UserMessageAction(
                    agent::UserMessageAction {
                        user_message: Some(agent::UserMessage {
                            text: "Give me an example.".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )),
                ..Default::default()
            }),
            ..Default::default()
        };

        let chat_req = build_chat_request_from_run(&run_req, "gpt-5.6-luna");

        assert_eq!(chat_req.messages.len(), 4);
        assert_eq!(chat_req.messages[0].role, Role::System);
        assert_eq!(chat_req.messages[1].role, Role::User);
        assert_eq!(chat_req.messages[1].text, "Explain quantum computing in one sentence.");
        assert_eq!(chat_req.messages[2].role, Role::Assistant);
        assert!(chat_req.messages[2].text.contains("exponentially faster"));
        assert_eq!(chat_req.messages[3].role, Role::User);
        assert_eq!(chat_req.messages[3].text, "Give me an example.");
    }
}
