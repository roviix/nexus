//! Cursor Electron 跨域中间件（处理 `vscode-file://vscode-app` 与自定义 Header 预检）。

use axum::extract::Request;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// 允许 Cursor 渲染层（Electron vscode-file://）以及所有来自 127.0.0.1 的跨域请求。
pub async fn cursor_cors_middleware(req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("*"));

    if req.method() == Method::OPTIONS {
        // 预检请求直接通过并返回通配权限
        let mut res = StatusCode::OK.into_response();
        let headers = res.headers_mut();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("POST, GET, OPTIONS"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("*"),
        );
        headers.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("connect-protocol-version, content-type, x-cursor-checksum"),
        );
        headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("86400"),
        );
        return res;
    }

    let mut res = next.run(req).await;
    let headers = res.headers_mut();
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("connect-protocol-version, content-type, x-cursor-checksum"),
    );
    res
}
