//! 本机 `127.0.0.1:51121` 上的 Google OAuth 回调服务器。
//!
//! Antigravity 的官方 / 开源客户端约定将回调登记在 `http://localhost:51121/oauth-callback`。
//! 用户在系统浏览器完成 Google 登录授权后，授权码会自动重定向至此处。
//!
//! 回调服务接收携带正确 `state` 的请求，返回友好的 HTML 成功页面，并将 `code` 传递给下游，
//! 随后关闭监听。如果端口被占用，则降级为在应用界面手动粘贴地址或授权码。

use nexus_core::{AppError, ErrorCode, Result};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

pub const DEFAULT_CALLBACK_PORT: u16 = 51121;
const MAX_HEAD: usize = 16 * 1024;
const READ_PATIENCE: Duration = Duration::from_secs(5);

pub struct CallbackServer {
    listener: TcpListener,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    pub code: String,
    pub state: Option<String>,
}

enum Reply {
    Done(Received),
    Continue,
}

impl CallbackServer {
    pub async fn bind() -> std::io::Result<Self> {
        Self::bind_port(DEFAULT_CALLBACK_PORT).await
    }

    pub async fn bind_port(port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        Ok(Self { listener })
    }

    pub fn port(&self) -> u16 {
        self.listener
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(DEFAULT_CALLBACK_PORT)
    }

    pub async fn wait(
        self,
        expected_state: &str,
        timeout: Duration,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Received> {
        let started = Instant::now();
        loop {
            if cancelled() {
                return Err(AppError::new(ErrorCode::Cancelled, "已取消授权等待"));
            }
            if started.elapsed() >= timeout {
                return Err(AppError::new(
                    ErrorCode::OauthTimeout,
                    "等待 Google 授权超时，请在浏览器中完成登录并重试",
                ));
            }

            let accept_future = self.listener.accept();
            tokio::pin!(accept_future);

            let (stream, _) = tokio::select! {
                res = &mut accept_future => {
                    match res {
                        Ok(conn) => conn,
                        Err(err) => {
                            tracing::warn!(%err, "接收回调连接失败，继续等待");
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            continue;
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(200)) => {
                    continue;
                }
            };

            if let Some(received) = handle_connection(stream, expected_state).await {
                return Ok(received);
            }
        }
    }
}

async fn handle_connection(mut stream: TcpStream, expected_state: &str) -> Option<Received> {
    let mut buf = Vec::with_capacity(2048);
    let mut temp = [0u8; 1024];
    let read_start = Instant::now();

    loop {
        if read_start.elapsed() > READ_PATIENCE || buf.len() > MAX_HEAD {
            let _ = stream
                .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
                .await;
            return None;
        }

        let n = match tokio::time::timeout(Duration::from_millis(500), stream.read(&mut temp)).await
        {
            Ok(Ok(0)) | Err(_) => break,
            Ok(Ok(n)) => n,
            Ok(Err(_)) => return None,
        };
        buf.extend_from_slice(&temp[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }

    let req = String::from_utf8_lossy(&buf);
    let first_line = req.lines().next()?;
    let mut parts = first_line.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;

    if method != "GET" {
        let _ = stream
            .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nConnection: close\r\n\r\n")
            .await;
        return None;
    }

    match parse_request_path(path, expected_state) {
        Reply::Done(received) => {
            let html = r#"<!DOCTYPE html>
<html>
<head>
  <meta charset="utf-8">
  <title>Nexus · Google 授权成功</title>
  <style>
    body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; display: flex; align-items: center; justify-content: center; height: 100vh; margin: 0; background: #0f1117; color: #e6edf3; }
    .card { background: #161b22; border: 1px solid #30363d; border-radius: 12px; padding: 32px 40px; text-align: center; max-width: 440px; box-shadow: 0 8px 24px rgba(0,0,0,0.4); }
    h1 { font-size: 20px; margin: 0 0 12px; color: #58a6ff; }
    p { font-size: 14px; color: #8b949e; line-height: 1.6; margin: 0; }
  </style>
</head>
<body>
  <div class="card">
    <h1>Google 反重力 · 授权成功</h1>
    <p>凭证已安全收进 Nexus。你可以关闭此标签页并回到应用界面继续。</p>
  </div>
</body>
</html>"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                html.len(),
                html
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.flush().await;
            Some(received)
        }
        Reply::Continue => {
            let _ = stream
                .write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
                .await;
            None
        }
    }
}

fn parse_request_path(path: &str, expected_state: &str) -> Reply {
    let query_str = path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = None;
    let mut state = None;

    for pair in query_str.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            match k {
                "code" => code = Some(percent_decode(v)),
                "state" => state = Some(percent_decode(v)),
                _ => {}
            }
        }
    }

    if let Some(code) = code {
        if let Some(ref st) = state {
            if st != expected_state {
                tracing::warn!(got = %st, expected = %expected_state, "OAuth state 不匹配");
                return Reply::Continue;
            }
        }
        Reply::Done(Received { code, state })
    } else {
        Reply::Continue
    }
}

pub fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(byte);
                i += 3;
                continue;
            }
        } else if bytes[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}
