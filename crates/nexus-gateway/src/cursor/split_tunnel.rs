//! 混合分流器（Split-Tunneling Proxy）。
//!
//! Cursor 的 Agent 体验依赖背后多种协同的 Connect-RPC 服务：
//! 1. `InferenceService/Stream`：核心思考与推理（走本地 Mock Server 转译到 Claude / OpenAI）
//! 2. `FastApplyService/*` / `SpeculativeEdits`：代码快速合并 Diff（白嫖 Cursor 官方小模型）
//! 3. `RepositoryService/*`：代码库语义向量索引（@Codebase 依赖官方服务）
//! 4. `CheckUserStatus` / 遥测等基础 RPC
//!
//! 本模块负责将非推理类的辅助 RPC 透明透传给 Cursor 官方后端（`api2.cursor.sh`），
//! 保障「换了大脑，但 FastApply 与向量索引等基建手脚不残疾」。

use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use reqwest::header::{HeaderName, HeaderValue as ReqHeaderValue};

pub const DEFAULT_CURSOR_UPSTREAM: &str = "https://api2.cursor.sh";
pub const AGENT_GLOBAL_UPSTREAM: &str = "https://agent.global.api5.cursor.sh";

/// 透明代理分流转发给官方 Cursor 后端。
pub async fn transparent_proxy_to_cursor(
    headers: HeaderMap,
    req_body: Bytes,
    path_and_query: &str,
) -> Response {
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .no_proxy()
        .http2_prior_knowledge()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    // 确定上游服务主机：agent.v1 路由到 api5，其余走 api2
    let base = if path_and_query.contains("agent.v1.") {
        AGENT_GLOBAL_UPSTREAM
    } else {
        DEFAULT_CURSOR_UPSTREAM
    };

    let target_url = format!("{base}{path_and_query}");

    let mut out_req = client.post(&target_url);

    // 搬运请求头（保留 connect-protocol-version, content-type, authorization 以及 x-cursor-*）
    for (name, val) in &headers {
        let key = name.as_str().to_ascii_lowercase();
        if key == "host" || key == "content-length" {
            continue;
        }
        if let Ok(k) = HeaderName::from_bytes(key.as_bytes()) {
            if let Ok(v) = ReqHeaderValue::from_bytes(val.as_bytes()) {
                out_req = out_req.header(k, v);
            }
        }
    }

    out_req = out_req.body(req_body);

    match out_req.send().await {
        Ok(upstream_res) => {
            let status = StatusCode::from_u16(upstream_res.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY);

            let mut res_builder = Response::builder().status(status);

            // 转发回程响应头
            if let Some(headers_mut) = res_builder.headers_mut() {
                for (name, val) in upstream_res.headers() {
                    let key = name.as_str().to_ascii_lowercase();
                    if key == "content-length" || key == "transfer-encoding" {
                        continue;
                    }
                    if let Ok(v) = HeaderValue::from_bytes(val.as_bytes()) {
                        headers_mut.insert(name.clone(), v);
                    }
                }
            }

            let stream = upstream_res.bytes_stream();
            res_builder
                .body(Body::from_stream(stream))
                .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
        Err(err) => {
            tracing::warn!(error = %err, target = %target_url, "Cursor 辅助 RPC 分流透传失败");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}
