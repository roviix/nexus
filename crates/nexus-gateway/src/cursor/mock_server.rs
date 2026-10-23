//! Cursor Connect-RPC 推理服务（将 Cursor 原生请求转译为 Claude / OpenAI / Provider 渠道）。
//!
//! 监听端点：
//! - `POST /cursor/mock/aiserver.v1.InferenceService/Stream`
//! - `POST /cursor/mock/agent.v1.AgentService/Run`
//!
//! 核心机制：
//! 1. 接收并解密 Cursor Connect-RPC 信封与 `InferenceStreamRequest` Protobuf 载荷；
//! 2. 映射为规范化 `ChatRequest`，并按客户端路由配置分发到目标通道（如 Claude Messages / OpenAI）；
//! 3. 逐帧监听上游产生的 `Delta`（文本、思考增量、Thinking 签名），转化为 `InferenceStreamResponse`；
//! 4. 结尾发送携带元数据与 Tool Calls 的信封，并以 `FLAG_END_STREAM` 正常关闭连接。

use super::proto_bridge::{
    encode_end_stream, encode_stream_error, encode_text_part, encode_thinking_part,
    encode_tool_call_part, from_inference_request,
};
use crate::channel::Capability;
use crate::connect::EnvelopeDecoder;
use crate::lane::Outcome;
use crate::normalized::Delta;
use crate::proto::InferenceStreamRequest;
use crate::server::Gateway;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use prost::Message as _;
use std::sync::Arc;
use tokio::sync::mpsc;

const CONNECT_CONTENT_TYPE: &str = "application/connect+proto";
const CONNECT_VERSION_HEADER: &str = "connect-protocol-version";

/// 处理来自 Cursor IDE 的 Connect-RPC 推理流请求。
pub async fn handle_inference_stream(
    State(gw): State<Arc<Gateway>>,
    _headers: HeaderMap,
    body: Bytes,
) -> Response {
    // 1. 拆解 Connect 信封
    let mut decoder = EnvelopeDecoder::default();
    decoder.push(&body);

    let envelope = match decoder.next_envelope() {
        Ok(Some(env)) => env,
        Ok(None) => {
            return bad_request_response("请求体缺少有效的 Connect-RPC 信封");
        }
        Err(err) => {
            return bad_request_response(&format!("信封解析失败: {err}"));
        }
    };

    // 2. 解码 InferenceStreamRequest Protobuf
    let stream_req = match InferenceStreamRequest::decode(&envelope.data[..]) {
        Ok(req) => req,
        Err(err) => {
            return bad_request_response(&format!("Protobuf 解码失败: {err}"));
        }
    };

    // 3. 转换为规范化请求并按客户端路由解析目标模型
    let mut chat_req = from_inference_request(&stream_req);
    let resolved_model = gw.scoped_model(Some("cursor"), &chat_req.model);
    chat_req.model = resolved_model.clone();

    // 自动补全通道前缀（若尚未带前缀且已有通道未直接认领）
    if !chat_req.model.contains('/') {
        let m_lower = chat_req.model.to_ascii_lowercase();
        if m_lower.starts_with("kiro-") {
            if gw.channels.get("mesh").is_some_and(|c| c.gate.owns(Capability::Chat, &chat_req.model)) {
                // 由 MeshGate 直接认领
            } else if gw.channels.get("kiro").is_some() {
                chat_req.model = format!("kiro/{}", chat_req.model);
            }
        } else if m_lower.contains("gpt-5") || m_lower.contains("gpt-4") || m_lower.starts_with("o1") || m_lower.starts_with("o3") {
            if gw.channels.get("chatgpt").is_some() {
                chat_req.model = format!("chatgpt/{}", chat_req.model);
            }
        } else if m_lower.contains("claude-") {
            if gw.channels.get("mesh").is_some_and(|c| c.gate.owns(Capability::Chat, &chat_req.model)) {
                // 由 MeshGate 直接认领
            } else if gw.channels.get("claude").is_some() {
                chat_req.model = format!("claude/{}", chat_req.model);
            }
        }
    }

    // 4. 选路与门禁检查
    let route = gw.route(&chat_req.model, Capability::Chat);
    if let Some(reason) = route.channel.gate.unavailable_reason() {
        return connect_error_response("unavailable", reason);
    }
    if !route.channel.gate.ready() {
        return connect_error_response("resource_exhausted", "所选通道当前没有可用号源");
    }

    // 5. 管道化流式驱动（使用非阻塞 unbounded_channel，消除异步线程中 blocking_send 导致的崩溃）
    let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, std::io::Error>>();

    let lane = Arc::clone(&route.channel.lane);
    let upstream = Arc::clone(&route.channel.upstream);
    let target_model = chat_req.model.clone();

    tokio::spawn(async move {
        let credential = match lane.acquire(&target_model).await {
            Ok(cred) => cred,
            Err(err) => {
                let err_chunk = encode_stream_error("resource_exhausted", &err.to_string());
                let _ = tx.send(Ok(Bytes::from(err_chunk)));
                return;
            }
        };

        let _started = std::time::Instant::now();
        let tx_delta = tx.clone();

        // 增量回调：转换为 Connect-RPC 二进制信封并推入下行通道
        let mut on_delta = move |delta: Delta| {
            let chunk_opt = match delta {
                Delta::Text(text) => Some(encode_text_part(&text)),
                Delta::Thinking(th) => Some(encode_thinking_part(&th, None, false)),
                Delta::ThinkingSignature(sig) => {
                    Some(encode_thinking_part("", Some(&sig), true))
                }
                Delta::RedactedThinking(data) => {
                    Some(encode_thinking_part(&data, None, true))
                }
                Delta::Headers(_) | Delta::Raw { .. } => None,
            };

            if let Some(chunk) = chunk_opt {
                let _ = tx_delta.send(Ok(Bytes::from(chunk)));
            }
        };

        let stream_res = upstream.stream(&credential, &chat_req, &mut on_delta).await;

        match stream_res {
            Ok(completion) => {
                // 如果产生 Tool Calls，以 ToolCallPart 逐个下发给 Cursor
                for (idx, tc) in completion.tool_calls.iter().enumerate() {
                    let tool_chunk = encode_tool_call_part(tc, true, Some(idx as i32));
                    let _ = tx.send(Ok(Bytes::from(tool_chunk)));
                }

                // 若有未派发的思考签名，补充一帧以保全下一轮上下文
                if let Some(sig) = completion.thinking_signature.as_deref() {
                    let sig_chunk = encode_thinking_part("", Some(sig), true);
                    let _ = tx.send(Ok(Bytes::from(sig_chunk)));
                }

                // 汇报用量与结局
                lane.report(&credential, &target_model, Outcome::Ok(&completion.usage));

                // 正常终止帧
                let end_chunk = encode_end_stream();
                let _ = tx.send(Ok(Bytes::from(end_chunk)));
            }
            Err(err) => {
                lane.report(&credential, &target_model, Outcome::Err(&err));
                let err_chunk = encode_stream_error("internal", &err.message);
                let _ = tx.send(Ok(Bytes::from(err_chunk)));
            }
        }
    });

    let body_stream = tokio_stream::wrappers::UnboundedReceiverStream::new(rx);

    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", CONNECT_CONTENT_TYPE)
        .header(CONNECT_VERSION_HEADER, "1")
        .header("access-control-allow-origin", "*")
        .header(
            "access-control-expose-headers",
            "connect-protocol-version, content-type",
        )
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

fn bad_request_response(msg: &str) -> Response {
    let err_bytes = encode_stream_error("invalid_argument", msg);
    Response::builder()
        .status(StatusCode::BAD_REQUEST)
        .header("content-type", CONNECT_CONTENT_TYPE)
        .header(CONNECT_VERSION_HEADER, "1")
        .body(Body::from(err_bytes))
        .unwrap_or_else(|_| StatusCode::BAD_REQUEST.into_response())
}

pub fn connect_error_response(code: &str, msg: &str) -> Response {
    let err_bytes = encode_stream_error(code, msg);
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", CONNECT_CONTENT_TYPE)
        .header(CONNECT_VERSION_HEADER, "1")
        .body(Body::from(err_bytes))
        .unwrap_or_else(|_| StatusCode::OK.into_response())
}
