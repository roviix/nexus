//! `/cursor/*` 的来源门禁。
//!
//! 这组口子会替请求方带上本机 Cursor 账号的 token 去打 `api2.cursor.sh`，也能花网关里的付费通道。
//! 真正的调用方只有两个：CRSR 补丁改过的 Cursor 扩展进程（Node 发的请求，不带 `Origin`），
//! 和 Nexus 自己的界面（「测一下」按钮，Tauri 的来源）。浏览器里任何网页都能往 127.0.0.1 发请求，
//! 而浏览器发跨域请求一定带 `Origin`——所以带了陌生 `Origin` 的一律拒掉，不再把对方的来源原样
//! 回成「允许」。本机进程本来就有用户的权限（能直接读凭证文件），挡它们没有意义。

use axum::extract::Request;
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// 浏览器自己标的请求来路。Node / Electron 主进程发的请求没有这个头。
fn browser_cross_site(req: &Request) -> bool {
    req.headers()
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| matches!(v.trim(), "cross-site" | "same-site"))
}

/// 允许的浏览器来源：Cursor 自己的渲染层、Nexus 的界面（打包后与开发时）。
fn allowed_origin(origin: &str) -> bool {
    let o = origin.trim().to_ascii_lowercase();
    o == "vscode-file://vscode-app"
        || o.starts_with("vscode-webview://")
        || o == "tauri://localhost"
        || o == "http://tauri.localhost"
        || o == "https://tauri.localhost"
        || o == "http://localhost:1420"
        || o == "http://127.0.0.1:1420"
}

pub async fn cursor_cors_middleware(req: Request, next: Next) -> Response {
    let origin = req.headers().get(header::ORIGIN).cloned();
    if let Some(o) = &origin {
        let ok = o.to_str().is_ok_and(allowed_origin);
        if !ok {
            tracing::warn!(origin = ?o, "拒绝来自陌生网页的 /cursor 请求");
            return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
        }
    } else if browser_cross_site(&req) {
        // 没带 Origin 但浏览器标了跨站：`<img>`、页面跳转这类，同样是别的网页发起的。
        tracing::warn!("拒绝浏览器跨站发起、且没带来源的 /cursor 请求");
        return (StatusCode::FORBIDDEN, "cross-site request not allowed").into_response();
    }

    if req.method() == Method::OPTIONS {
        let mut res = StatusCode::NO_CONTENT.into_response();
        if let Some(o) = origin {
            // Cursor 的请求头很多（x-cursor-*、x-ghost-mode、traceparent……）且随版本变，
            // 来源已经核过，按浏览器报上来的清单原样放行。`*` 不覆盖 authorization，不能用。
            let requested = req
                .headers()
                .get(header::ACCESS_CONTROL_REQUEST_HEADERS)
                .cloned()
                .unwrap_or_else(|| HeaderValue::from_static("authorization, content-type"));
            let headers = res.headers_mut();
            headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, o);
            headers.insert(
                header::VARY,
                HeaderValue::from_static("origin, access-control-request-headers"),
            );
            headers.insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("POST, GET, OPTIONS"),
            );
            headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, requested);
            headers.insert(
                header::ACCESS_CONTROL_MAX_AGE,
                HeaderValue::from_static("600"),
            );
        }
        return res;
    }

    let mut res = next.run(req).await;
    if let Some(o) = origin {
        let headers = res.headers_mut();
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, o);
        headers.insert(header::VARY, HeaderValue::from_static("origin"));
        headers.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("connect-protocol-version, content-type, x-cursor-checksum"),
        );
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_cursor_and_nexus_origins_are_let_through() {
        assert!(allowed_origin("vscode-file://vscode-app"));
        assert!(allowed_origin("tauri://localhost"));
        assert!(allowed_origin("http://tauri.localhost"));
        assert!(allowed_origin("http://localhost:1420"));
        assert!(!allowed_origin("https://evil.example"));
        assert!(
            !allowed_origin("null"),
            "沙箱 iframe 的来源也是 null，网页能伪造"
        );
        assert!(!allowed_origin("http://localhost:3000"));
    }
}
