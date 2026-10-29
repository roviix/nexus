//! Cursor 智能双轨路由分流器（Hybrid Smart Router）。
//!
//! 监听端点：
//! - `ALL /cursor/route/{*path}`
//!
//! 核心路由原则：
//! 1. 【辅助 RPC 100% 直通】：非推理类 RPC（如 FastApply, RepositoryService, Ping 等）永远
//!    直通官方底座（通过 `native_relay`），不被任何转译逻辑干扰；
//! 2. 【官方模型原生直通】：当用户在 Cursor 中选用官方模型（如 claude-3.7-sonnet, gpt-4o,
//!    cursor-small 等）时，100% 走 `native_relay`，直通官方或本地号池；
//! 3. 【扩展模型跨协议转译】：只有当用户在 Cursor 模型菜单中显式选用通过 Nexus 接入的扩展模型
//!    （如 deepseek-r1, claude-sonnet-4-6 等）时，才由 `mock_server` 拦截并转译为目标通道协议。

use super::agent_server;
use super::mock_server;
use super::model_catalog;
use super::native_relay;
use super::proto::agent::v1 as agent;
use crate::connect::EnvelopeDecoder;
use crate::proto::InferenceStreamRequest;
use crate::server::Gateway;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use prost::Message as _;
use std::sync::Arc;
use tracing::{info, warn};

/// 判断是否为 Cursor 官方原生内置模型名。
pub fn is_official_model_name(name: &str) -> bool {
    let m = name.trim().to_ascii_lowercase();
    if m.is_empty() {
        return true;
    }
    const OFFICIAL_MODELS: &[&str] = &[
        "default",
        "auto",
        "cursor-small",
        "cursor-fast",
        "cursor-default",
        "composer-default",
        "claude-3-5-sonnet",
        "claude-3-5-sonnet-20241022",
        "claude-3.5-sonnet",
        "claude-3.5-sonnet-thinking",
        "claude-3-7-sonnet",
        "claude-3.7-sonnet",
        "claude-3-7-sonnet-thinking",
        "claude-3.7-sonnet-thinking",
        "claude-3-opus",
        "claude-3.5-haiku",
        "gpt-4o",
        "gpt-4o-mini",
        "gpt-4",
        "gpt-4.5",
        "gpt-4.5-preview",
        "o1",
        "o1-mini",
        "o1-preview",
        "o3-mini",
        "gemini-2.0-flash",
        "gemini-2.5-pro",
    ];

    OFFICIAL_MODELS.contains(&m.as_str())
}

/// 判断给定的模型标识是否属于 Nexus 注入 / 自定义模型。
pub fn is_custom_model_id(gw: &Gateway, raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }

    // 1. 显式带有 Nexus 标识（如 "(Nexus)" 或 "[Nexus]"），100% 判定为 Nexus 接入模型
    if trimmed.ends_with("(Nexus)")
        || trimmed.ends_with("[Nexus]")
        || trimmed.to_ascii_lowercase().ends_with("(nexus)")
        || trimmed.to_ascii_lowercase().ends_with("[nexus]")
    {
        return true;
    }

    let clean = trimmed
        .trim_end_matches("(Nexus)")
        .trim_end_matches("[Nexus]")
        .trim_end_matches("(nexus)")
        .trim_end_matches("[nexus]")
        .trim();

    // 2. 带有异构通道前缀，如 chatgpt/..., claude/..., kiro/..., mesh/... 且该通道在网关中存在
    if let Some((channel, _)) = clean.split_once('/') {
        if gw.channels.get(channel).is_some() || channel.starts_with("node_") {
            return true;
        }
    }

    // 3. 被网关就绪通道认领（如 MeshGate 认领的 kiro-claude-* 或其它对等节点共享模型）
    for ch in gw.channels.extras() {
        if ch.gate.ready()
            && (ch.gate.claims_bare(clean) || ch.gate.owns(crate::channel::Capability::Chat, clean))
        {
            return true;
        }
    }

    // 4. 与网关配置的 cursor 转发目标模型或别名一致
    if let Ok(routes) = gw.routes.read() {
        if let Some(cursor_route) = routes.get("cursor") {
            let bare_requested = nexus_store::key_providers::bare_model(clean);
            let model_requested = bare_requested
                .split_once('/')
                .map(|(_, r)| r)
                .unwrap_or(bare_requested);

            let configured = cursor_route.model.trim();
            if !configured.is_empty() {
                let bare_configured = nexus_store::key_providers::bare_model(configured);
                let model_configured = bare_configured
                    .split_once('/')
                    .map(|(_, r)| r)
                    .unwrap_or(bare_configured);
                if clean.eq_ignore_ascii_case(configured)
                    || bare_requested.eq_ignore_ascii_case(bare_configured)
                    || model_requested.eq_ignore_ascii_case(model_configured)
                {
                    return true;
                }
            }

            for alias in &cursor_route.aliases {
                let alias_clean = alias.trim();
                if alias_clean.is_empty() {
                    continue;
                }
                let bare_alias = nexus_store::key_providers::bare_model(alias_clean);
                let model_alias = bare_alias
                    .split_once('/')
                    .map(|(_, r)| r)
                    .unwrap_or(bare_alias);
                if clean.eq_ignore_ascii_case(alias_clean)
                    || bare_requested.eq_ignore_ascii_case(bare_alias)
                    || model_requested.eq_ignore_ascii_case(model_alias)
                {
                    return true;
                }
            }
        }
    }

    false
}

/// 判断该推理请求是否应当被拦截并转译为异构通道模型。
pub fn is_custom_model_request(gw: &Gateway, req: &InferenceStreamRequest) -> bool {
    let raw = req
        .requested_model
        .as_ref()
        .map(|m| m.model_id.as_str())
        .or(req.model_id.as_deref())
        .unwrap_or("");
    is_custom_model_id(gw, raw)
}

/// 判断 AgentRunRequest 是否请求了自定义接入模型。
pub fn is_custom_agent_run_request(gw: &Gateway, req: &agent::AgentRunRequest) -> bool {
    let raw = req
        .requested_model
        .as_ref()
        .map(|m| m.model_id.as_str())
        .or_else(|| req.model_details.as_ref().map(|m| m.model_id.as_str()))
        .or(req.dev_raw_model_slug.as_deref())
        .unwrap_or("");
    is_custom_model_id(gw, raw)
}

/// 处理来自 Cursor IDE 的双轨智能请求分流。
pub async fn handle_cursor_route(
    State(gw): State<Arc<Gateway>>,
    headers: HeaderMap,
    req: Request,
) -> Response {
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let stripped = path
        .strip_prefix("/cursor/route")
        .or_else(|| path.strip_prefix("/route"))
        .unwrap_or(&path);
    let subpath = if stripped.is_empty() || !stripped.starts_with('/') {
        format!("/{stripped}")
    } else {
        stripped.to_string()
    };
    let path_and_query = match req.uri().query() {
        Some(q) => format!("{subpath}?{q}"),
        None => subpath.clone(),
    };

    info!(
        method = %method,
        subpath = %subpath,
        "Cursor 网关分流收到请求"
    );

    // 1. 一元辅助元数据（立即响应，无需拉取或阻塞 body，规避官方 502 / 未授权重试导致 Composer 规划卡死）
    if subpath.contains("agent.v1.AgentService/UpdateConversationMetadata")
        || subpath.contains("agent.v1.AgentService/NameAgent")
        || subpath.contains("agent.v1.AgentService/CreateTranscriptOverview")
        || subpath.contains("agent.v1.AgentService/GetAllowedModelIntents")
        || subpath.contains("agent.v1.AgentService/GetDefaultModelForCli")
        || subpath.contains("agent.v1.AgentService/GetPromptContextUsage")
    {
        return agent_server::handle_agent_unary_metadata(&subpath, &headers);
    }

    // 2. RunSSE 是服务端流：请求体只有一个 BidiRequestId，读完整体后建立本地下行管道，
    //    模型归属由随后的 BidiAppend 判定。必须排在 Run 之前，否则会被 "Run" 前缀吞掉。
    if subpath.contains("agent.v1.AgentService/RunSSE") {
        let body = match axum::body::to_bytes(req.into_body(), crate::server::MAX_BODY_BYTES).await
        {
            Ok(b) => b,
            Err(_) => return StatusCode::BAD_REQUEST.into_response(),
        };
        return agent_server::handle_run_sse(State(gw), headers, body).await;
    }

    // 3. Connect-RPC 双向流式 Agent 运行流 (AgentService/Run)
    // 注意：双向流式客户端在接收到服务端首个响应前不会发送 EOF，绝不能在入口通过 to_bytes 阻塞等待整个 body！
    if subpath.contains("agent.v1.AgentService/Run") {
        return handle_agent_service_run_stream(gw, headers, req.into_body(), &path_and_query)
            .await;
    }

    // 其他非双向长流端点，安全读取完整 body
    let body = match axum::body::to_bytes(req.into_body(), crate::server::MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };

    // 4. 动态模型目录与能力查询（Module 2）
    if subpath.contains("AvailableModels") {
        return model_catalog::handle_available_models(State(gw), headers, body, &path_and_query)
            .await;
    }
    if subpath.contains("GetUsableModels") {
        return model_catalog::handle_usable_models(State(gw), headers, body, &path_and_query)
            .await;
    }

    // 5. 原生 Agent 会话引擎（BidiAppend 上行）
    if subpath.contains("aiserver.v1.BidiService/BidiAppend") {
        return agent_server::handle_bidi_append(State(gw), headers, body).await;
    }

    let is_inference_rpc = subpath.contains("aiserver.v1.InferenceService/Stream");

    if !is_inference_rpc {
        // 辅助 RPC（FastApply, RepositoryService 等）直接直通官方底座
        return native_relay::handle_native_relay(State(gw), headers, body, &path_and_query).await;
    }

    // 5. 检查 InferenceStreamRequest 推理请求中的模型信息
    let mut decoder = EnvelopeDecoder::default();
    decoder.push(&body);

    let is_custom = match decoder.next_envelope() {
        Ok(Some(env)) => match InferenceStreamRequest::decode(&env.data[..]) {
            Ok(stream_req) => is_custom_model_request(&gw, &stream_req),
            Err(_) => false,
        },
        _ => false,
    };

    if is_custom {
        mock_server::handle_inference_stream(State(gw), headers, body).await
    } else {
        native_relay::handle_native_relay(State(gw), headers, body, &path_and_query).await
    }
}

/// 处理 Connect-RPC 双向流式 `agent.v1.AgentService/Run` 请求。
/// 客户端建立双向流后，会先推入首个包含 AgentClientMessage (RunRequest / ConversationAction) 的 Envelope。
/// 服务端异步读取首包以判断模型归属：
/// - 自定义模型 (Nexus 注入)：由 agent_server 实时接管并流式下发 AgentServerMessage 响应；
/// - 官方底座模型：拼装已读数据并透明反向代理流至 Cursor 官方 api2.cursor.sh。
async fn handle_agent_service_run_stream(
    gw: Arc<Gateway>,
    headers: HeaderMap,
    req_body: axum::body::Body,
    path_and_query: &str,
) -> Response {
    use futures_util::TryStreamExt;
    use tokio_stream::StreamExt;

    let mut stream = req_body.into_data_stream();
    let mut buffered = Vec::new();
    let mut decoder = EnvelopeDecoder::default();
    let mut run_request = None;
    let mut bidi_request = None;

    // 快速读取首个完整 Envelope（超时 3 秒兜底）
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while let Some(chunk_res) = stream.next().await {
            if let Ok(chunk) = chunk_res {
                decoder.push(&chunk);
                buffered.extend_from_slice(&chunk);

                while let Ok(Some(env)) = decoder.next_envelope() {
                    if env.flags & agent_server::CONNECT_END_STREAM_FLAG == 0 {
                        // 1. 尝试从 AgentClientMessage 解码
                        if let Ok(client_msg) = agent::AgentClientMessage::decode(&env.data[..]) {
                            match client_msg.message {
                                Some(agent::agent_client_message::Message::RunRequest(r)) => {
                                    info!("AgentService/Run: 从 AgentClientMessage 成功解析 RunRequest");
                                    run_request = Some(r);
                                    return;
                                }
                                Some(agent::agent_client_message::Message::ConversationAction(act)) => {
                                    info!("AgentService/Run: 从 AgentClientMessage 成功解析 ConversationAction");
                                    run_request = Some(agent::AgentRunRequest {
                                        action: Some(act),
                                        ..Default::default()
                                    });
                                    return;
                                }
                                _ => {}
                            }
                        }

                        // 2. 尝试解压后从 AgentClientMessage 解码
                        let decompressed = agent_server::decompress_if_needed(&env.data);
                        if let Ok(client_msg) = agent::AgentClientMessage::decode(decompressed.as_slice()) {
                            match client_msg.message {
                                Some(agent::agent_client_message::Message::RunRequest(r)) => {
                                    info!("AgentService/Run: 从解压后 AgentClientMessage 成功解析 RunRequest");
                                    run_request = Some(r);
                                    return;
                                }
                                Some(agent::agent_client_message::Message::ConversationAction(act)) => {
                                    info!("AgentService/Run: 从解压后 AgentClientMessage 成功解析 ConversationAction");
                                    run_request = Some(agent::AgentRunRequest {
                                        action: Some(act),
                                        ..Default::default()
                                    });
                                    return;
                                }
                                _ => {}
                            }
                        }

                        // 3. 尝试直接从 Envelope 解码 AgentRunRequest
                        if let Ok(r) = agent::AgentRunRequest::decode(&env.data[..]) {
                            if r.requested_model.is_some() || r.dev_raw_model_slug.is_some() || r.conversation_id.is_some() {
                                info!("AgentService/Run: 直接从 Envelope 成功解析 AgentRunRequest");
                                run_request = Some(r);
                                return;
                            }
                        }
                        if let Ok(r) = agent::AgentRunRequest::decode(decompressed.as_slice()) {
                            if r.requested_model.is_some() || r.dev_raw_model_slug.is_some() || r.conversation_id.is_some() {
                                info!("AgentService/Run: 从解压后 Envelope 成功解析 AgentRunRequest");
                                run_request = Some(r);
                                return;
                            }
                        }

                        // 4. 尝试解码 BidiRequestId
                        if let Ok(bidi) = agent::BidiRequestId::decode(&env.data[..]) {
                            if !bidi.request_id.trim().is_empty() {
                                info!("AgentService/Run: 成功解析 BidiRequestId");
                                bidi_request = Some(bidi);
                                return;
                            }
                        }
                        if let Ok(bidi) = agent::BidiRequestId::decode(decompressed.as_slice()) {
                            if !bidi.request_id.trim().is_empty() {
                                info!("AgentService/Run: 从解压后成功解析 BidiRequestId");
                                bidi_request = Some(bidi);
                                return;
                            }
                        }
                    }
                }
            } else {
                break;
            }
        }
    }).await;

    // 如果客户端发送的是 BidiRequestId (RunSSE 协议变体)
    if let Some(bidi_req) = bidi_request {
        if !bidi_req.request_id.trim().is_empty() {
            return agent_server::handle_run_sse(State(gw), headers, Bytes::from(buffered)).await;
        }
    }

    if let Some(run_req) = run_request {
        let is_custom = is_custom_agent_run_request(&gw, &run_req);
        info!(
            requested_model = ?run_req.requested_model.as_ref().map(|m| &m.model_id),
            dev_slug = ?run_req.dev_raw_model_slug,
            is_custom = is_custom,
            "AgentRunRequest 模型路由判定"
        );
        if is_custom {
            let pending = decoder.unconsumed();
            let chunk_vec = if pending.is_empty() {
                vec![]
            } else {
                vec![Ok(Bytes::from(pending))]
            };
            let inbound = tokio_stream::StreamExt::chain(tokio_stream::iter(chunk_vec), stream);
            return agent_server::handle_agent_run_direct(State(gw), headers, run_req, inbound)
                .await;
        }
    } else {
        warn!("AgentService/Run: 未能在首帧中解析出 AgentRunRequest 或 BidiRequestId，将透传至官方底座");
    }

    // 官方底座模型或未识别流：无缝拼装已读数据与后续流，直通 Cursor 官方
    let initial = tokio_stream::once(Ok(Bytes::from(buffered)));
    let rest = stream.map_err(std::io::Error::other);
    let full_stream = initial.chain(rest);
    let reqwest_body = reqwest::Body::wrap_stream(full_stream);

    native_relay::handle_native_relay_body(State(gw), headers, reqwest_body, path_and_query).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::InferenceRequestedModel;

    #[test]
    fn test_is_official_model_name() {
        assert!(is_official_model_name("claude-3.7-sonnet"));
        assert!(is_official_model_name("claude-3-7-sonnet"));
        assert!(is_official_model_name("claude-3.5-sonnet"));
        assert!(is_official_model_name("gpt-4o"));
        assert!(is_official_model_name("cursor-small"));
        assert!(is_official_model_name("auto"));
        assert!(is_official_model_name("default"));

        assert!(!is_official_model_name("deepseek-r1"));
        assert!(!is_official_model_name("claude-sonnet-4-6"));
        assert!(!is_official_model_name("gpt-5.6-luna"));
    }

    #[test]
    fn test_is_custom_model_request_checks_builtin() {
        struct DummyUpstream;
        impl crate::upstream::Upstream for DummyUpstream {
            fn stream<'a>(
                &'a self,
                _credential: &'a crate::lane::Credential,
                _request: &'a crate::normalized::ChatRequest,
                _on_delta: crate::upstream::DeltaSink<'a>,
            ) -> crate::lane::BoxFuture<
                'a,
                Result<crate::normalized::Completion, crate::error::UpstreamError>,
            > {
                Box::pin(async { unimplemented!() })
            }
        }

        let lane = Arc::new(crate::lane::StaticLane::new(crate::lane::Credential {
            label: "test".into(),
            access_token: "".into(),
            identity: crate::identity::DeviceIdentity::derived("tok"),
        }));
        let gw = Arc::new(Gateway::single(lane, Arc::new(DummyUpstream)));

        let official_req = InferenceStreamRequest {
            requested_model: Some(InferenceRequestedModel {
                model_id: "claude-3.7-sonnet".to_string(),
                built_in_model: true,
                max_mode: false,
                parameters: vec![],
                is_variant_string_representation: false,
            }),
            ..Default::default()
        };
        assert!(!is_custom_model_request(&gw, &official_req));

        let custom_req = InferenceStreamRequest {
            requested_model: Some(InferenceRequestedModel {
                model_id: "deepseek-r1".to_string(),
                built_in_model: false,
                max_mode: false,
                parameters: vec![],
                is_variant_string_representation: false,
            }),
            ..Default::default()
        };
        // Without cursor route configured, should return false (native fallback)
        assert!(!is_custom_model_request(&gw, &custom_req));

        // When cursor route is set
        gw.routes.write().unwrap().insert(
            "cursor".to_string(),
            crate::routes::ClientRoute {
                model: "deepseek/deepseek-r1".to_string(),
                opus: None,
                haiku: None,
                fable: None,
                context1m: false,
                aliases: vec![],
            },
        );
        assert!(is_custom_model_request(&gw, &custom_req));

        // Even if built_in_model was set to true on custom model name, it should still route to custom
        let custom_req_with_builtin_flag = InferenceStreamRequest {
            requested_model: Some(InferenceRequestedModel {
                model_id: "deepseek-r1".to_string(),
                built_in_model: true,
                max_mode: false,
                parameters: vec![],
                is_variant_string_representation: false,
            }),
            ..Default::default()
        };
        assert!(is_custom_model_request(&gw, &custom_req_with_builtin_flag));

        // Explicit (Nexus) suffix always routes to custom
        let nexus_tagged_req = InferenceStreamRequest {
            requested_model: Some(InferenceRequestedModel {
                model_id: "gpt-5.6-luna (Nexus)".to_string(),
                built_in_model: true,
                max_mode: false,
                parameters: vec![],
                is_variant_string_representation: false,
            }),
            ..Default::default()
        };
        assert!(is_custom_model_request(&gw, &nexus_tagged_req));
    }

    #[test]
    fn test_agent_client_message_run_request_decoding() {
        let run_req = agent::AgentRunRequest {
            requested_model: Some(agent::RequestedModel {
                model_id: "chatgpt/gpt-5.6-luna (Nexus)".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let client_msg = agent::AgentClientMessage {
            message: Some(agent::agent_client_message::Message::RunRequest(run_req)),
        };
        let bytes = client_msg.encode_to_vec();

        let decoded = agent::AgentClientMessage::decode(&bytes[..]).expect("decode client msg");
        if let Some(agent::agent_client_message::Message::RunRequest(req)) = decoded.message {
            assert_eq!(
                req.requested_model.unwrap().model_id,
                "chatgpt/gpt-5.6-luna (Nexus)"
            );
        } else {
            panic!("expected RunRequest");
        }
    }
}
