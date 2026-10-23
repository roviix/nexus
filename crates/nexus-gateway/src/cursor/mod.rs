//! Cursor 客户端原生接入与 Connect-RPC 转译模块。
//!
//! 提供两大核心能力：
//! - Target 1: Cursor-to-Cursor 原生直通（本地号池自动轮询 / 跨机云端 Mesh 透传，伴随指纹归一化）
//! - Target 2: 跨协议模型转译（将 Claude Messages / OpenAI / DeepSeek 原生接入 Cursor Composer Agent，
//!   完整保留思考签名、Tool Calling 与 FastApply 混合分流保护）

pub mod agent_server;
pub mod cors;
pub mod mock_server;
pub mod model_catalog;
pub mod native_relay;
pub mod proto;
pub mod proto_bridge;
pub mod router;
pub mod split_tunnel;

use crate::server::Gateway;
use axum::extract::{Request, State};
use axum::http::HeaderMap;
use axum::middleware;
use axum::routing::{any, post};
use axum::Router;
use std::sync::Arc;

/// 构建 Cursor 专属路由树。
pub fn cursor_routes(gw: Arc<Gateway>) -> Router {
    Router::new()
        // Target 1 + 2: 智能双轨路由（官方模型直通，扩展模型转译，辅助 RPC 自动分流）
        .route("/route", any(router::handle_cursor_route))
        .route("/route/{*path}", any(router::handle_cursor_route))
        // Target 2: Connect-RPC 推理转译服务
        .route(
            "/mock/aiserver.v1.InferenceService/Stream",
            post(mock_server::handle_inference_stream),
        )
        .route(
            "/mock/agent.v1.AgentService/Run",
            post(mock_server::handle_inference_stream),
        )
        // Target 2 混合分流：非推理辅助 RPC（FastApply, RepositoryService 等）透传给官方底座
        .route(
            "/mock/{*path}",
            any(|headers: HeaderMap, req: Request| async move {
                let path = req.uri().path().to_string();
                let stripped = path
                    .strip_prefix("/cursor/mock")
                    .or_else(|| path.strip_prefix("/mock"))
                    .unwrap_or(&path);
                let subpath = if stripped.is_empty() || !stripped.starts_with('/') {
                    format!("/{stripped}")
                } else {
                    stripped.to_string()
                };
                let body = axum::body::to_bytes(req.into_body(), usize::MAX)
                    .await
                    .unwrap_or_default();
                split_tunnel::transparent_proxy_to_cursor(headers, body, &subpath).await
            }),
        )
        // Target 1: 原生 Connect-RPC 透明中继与指纹归一化
        .route(
            "/native/{*path}",
            any(|State(gw): State<Arc<Gateway>>, headers: HeaderMap, req: Request| async move {
                let path = req.uri().path().to_string();
                let stripped = path
                    .strip_prefix("/cursor/native")
                    .or_else(|| path.strip_prefix("/native"))
                    .unwrap_or(&path);
                let subpath = if stripped.is_empty() || !stripped.starts_with('/') {
                    format!("/{stripped}")
                } else {
                    stripped.to_string()
                };
                let body = axum::body::to_bytes(req.into_body(), usize::MAX)
                    .await
                    .unwrap_or_default();
                native_relay::handle_native_relay(State(gw), headers, body, &subpath).await
            }),
        )
        .layer(middleware::from_fn(cors::cursor_cors_middleware))
        .with_state(gw)
}
