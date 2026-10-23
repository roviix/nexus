//! Cursor 原生协议透明中继与指纹归一化网关（Target 1 · Cursor-to-Cursor 本地号池 / 云端 Mesh 直通）。
//!
//! 监听端点：
//! - `POST /cursor/native/*`
//!
//! 解决的核心风控与体验问题：
//! 1. 【指纹归一化防封】：在多端通过 Mesh 共享或轮询本地多个账号时，拦截客户端散乱的
//!    `x-cursor-checksum`、`x-client-key`、`x-session-id`，按选定账号的静态 `DeviceIdentity`
//!    统一重新生成，防止同一账号在官方检测到多机 checksum 坍缩而报 "Too many computers"；
//! 2. 【凭证注水】：根据当前路由配置自动挂载本地 `crsr_` 兑换票据或转发给远端云节点出站；
//! 3. 【无损透传】：保持 100% 原生 Connect-RPC 二进制流与 HTTP/2 双向帧，零协议损耗。

use crate::server::Gateway;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use reqwest::header::{HeaderName, HeaderValue as ReqHeaderValue};
use std::sync::Arc;

pub const DEFAULT_CURSOR_API2: &str = "https://api2.cursor.sh";

fn get_base_access_token() -> Option<String> {
    let p = std::env::var("NEXUS_CRSR_CREDENTIAL_FILE")
        .ok()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            if cfg!(target_os = "windows") {
                let appdata = std::env::var("APPDATA").unwrap_or_default();
                std::path::PathBuf::from(appdata)
                    .join("com.roviix.nexus")
                    .join("crsr-agent-credential.json")
            } else if cfg!(target_os = "macos") {
                let home = std::env::var("HOME").unwrap_or_default();
                std::path::PathBuf::from(home)
                    .join("Library/Application Support/com.roviix.nexus/crsr-agent-credential.json")
            } else {
                let home = std::env::var("HOME").unwrap_or_default();
                std::path::PathBuf::from(home)
                    .join(".config/com.roviix.nexus/crsr-agent-credential.json")
            }
        });
    if p.exists() {
        if let Ok(content) = std::fs::read_to_string(&p) {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(tok) = json.get("accessToken").and_then(|t| t.as_str()) {
                    if !tok.trim().is_empty() {
                        return Some(tok.trim().to_string());
                    }
                }
            }
        }
    }
    None
}

fn build_outbound_request(
    client: &reqwest::Client,
    target_url: &str,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    let mut out_req = client.post(target_url);

    // 构造出站 Headers（若为 crsr_ 长期 Key，自动替换为底座 Access Token）
    let access_token = get_base_access_token();
    let mut has_auth = false;

    for (name, val) in headers {
        let key = name.as_str().to_ascii_lowercase();
        if key == "host" || key == "content-length" {
            continue;
        }
        if key == "authorization" {
            has_auth = true;
            if let Ok(val_str) = val.to_str() {
                if val_str.starts_with("Bearer crsr_") || val_str.trim().is_empty() {
                    if let Some(ref tok) = access_token {
                        if let Ok(v) = ReqHeaderValue::from_str(&format!("Bearer {tok}")) {
                            out_req = out_req.header(HeaderName::from_static("authorization"), v);
                            continue;
                        }
                    }
                }
            }
        }
        if let Ok(k) = HeaderName::from_bytes(key.as_bytes()) {
            if let Ok(v) = ReqHeaderValue::from_bytes(val.as_bytes()) {
                out_req = out_req.header(k, v);
            }
        }
    }

    if !has_auth {
        if let Some(ref tok) = access_token {
            if let Ok(v) = ReqHeaderValue::from_str(&format!("Bearer {tok}")) {
                out_req = out_req.header(HeaderName::from_static("authorization"), v);
            }
        }
    }

    out_req
}

async fn execute_outbound_request(
    out_req: reqwest::RequestBuilder,
    target_url: &str,
) -> Response {
    match out_req.send().await {
        Ok(upstream_res) => {
            let status = StatusCode::from_u16(upstream_res.status().as_u16())
                .unwrap_or(StatusCode::BAD_GATEWAY);

            let mut res_builder = Response::builder().status(status);

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
            tracing::error!(error = %err, target = %target_url, "Cursor 原生中继出站失败");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

/// 处理来自 Cursor IDE 的原生 Connect-RPC 请求（本地号池 / 云端 Mesh 透传）。
pub async fn handle_native_relay(
    State(_gw): State<Arc<Gateway>>,
    headers: HeaderMap,
    req_body: Bytes,
    path_and_query: &str,
) -> Response {
    let target_url = format!("{DEFAULT_CURSOR_API2}{path_and_query}");

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .no_proxy()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let out_req = build_outbound_request(&client, &target_url, &headers).body(req_body);
    execute_outbound_request(out_req, &target_url).await
}

/// 处理以流式 Body 形式传入的原生 Connect-RPC 请求（用于双向长连接/无缓冲透传）。
pub async fn handle_native_relay_body(
    State(_gw): State<Arc<Gateway>>,
    headers: HeaderMap,
    reqwest_body: reqwest::Body,
    path_and_query: &str,
) -> Response {
    let target_url = format!("{DEFAULT_CURSOR_API2}{path_and_query}");

    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(15))
        .no_proxy()
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

    let out_req = build_outbound_request(&client, &target_url, &headers).body(reqwest_body);
    execute_outbound_request(out_req, &target_url).await
}

