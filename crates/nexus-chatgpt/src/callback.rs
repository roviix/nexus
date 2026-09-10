//! 本机 `127.0.0.1:1455` 上的一次性回调服务器。
//!
//! Codex 的 OAuth client 把回调地址登记死在 `http://localhost:1455/auth/callback`。云端控制面
//! 收不到这个回调（它不在用户机器上），只能让用户把地址栏整条 URL 贴回来；桌面端就在用户
//! 机器上，直接在这个口上听——用户在浏览器里点完同意，授权码自己就到了。
//!
//! 只做一件事：等**一个**带着正确 `state` 的 `GET /auth/callback?code=…`，回一页「可以关掉了」，
//! 把 code 交出去，然后关掉监听。别的请求（favicon、state 对不上的、别的路径）礼貌地回掉，
//! 继续等。不引入 HTTP 框架：一条请求行加几个头，手解比拖一个 server 栈进来更好控制。
//!
//! 端口被占（多半是 `codex login` 正在跑）时绑定失败，调用方退回「贴地址」的路径。

use crate::oauth::{parse_query, CALLBACK_PORT};
use nexus_core::{AppError, ErrorCode, Result};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// 一次请求头最多读这么多；回调 URL 几百字节，超了就是别的东西。
const MAX_HEAD: usize = 16 * 1024;
/// 单条连接读请求头的耐心。浏览器的预连接可能只开不发。
const READ_PATIENCE: Duration = Duration::from_secs(5);

pub struct CallbackServer {
    listener: TcpListener,
}

/// 回调里带回来的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Received {
    pub code: String,
    pub state: Option<String>,
}

enum Reply {
    /// 拿到了：回成功页，结束。
    Done(Received),
    /// 回掉这条，继续等。
    Continue,
}

impl CallbackServer {
    /// 绑 `127.0.0.1:1455`。占着的时候 `Err`，调用方据此决定要不要退回手贴。
    pub async fn bind() -> std::io::Result<Self> {
        Self::bind_port(CALLBACK_PORT).await
    }

    pub async fn bind_port(port: u16) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port)).await?;
        Ok(Self { listener })
    }

    pub fn port(&self) -> u16 {
        self.listener
            .local_addr()
            .map(|a| a.port())
            .unwrap_or(CALLBACK_PORT)
    }

    /// 等到带着 `expected_state` 的授权码，或超时。`cancelled` 每秒看一眼。
    pub async fn wait(
        self,
        expected_state: &str,
        timeout: Duration,
        cancelled: &(dyn Fn() -> bool + Sync),
    ) -> Result<Received> {
        let started = Instant::now();
        loop {
            if cancelled() {
                return Err(AppError::new(ErrorCode::Cancelled, "已取消这次授权。"));
            }
            let remaining = timeout.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(
                    AppError::new(ErrorCode::OauthTimeout, "等待浏览器回调超时。").with_hint(
                        "登录可能没走完。重新发起一次授权，或把浏览器地址栏的回调地址贴进来。",
                    ),
                );
            }
            let tick = remaining.min(Duration::from_secs(1));
            let accepted = match tokio::time::timeout(tick, self.listener.accept()).await {
                Ok(Ok((stream, _))) => stream,
                Ok(Err(err)) => {
                    tracing::warn!(%err, "回调监听 accept 失败");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
                Err(_) => continue,
            };
            match handle(accepted, expected_state).await {
                Ok(Reply::Done(r)) => return Ok(r),
                Ok(Reply::Continue) => {}
                Err(err) => tracing::debug!(%err, "回调连接处理失败，继续等"),
            }
        }
    }
}

async fn handle(mut stream: TcpStream, expected_state: &str) -> std::io::Result<Reply> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    let deadline = Instant::now() + READ_PATIENCE;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() || buf.len() > MAX_HEAD {
            return Ok(Reply::Continue);
        }
        let n = match tokio::time::timeout(remaining, stream.read(&mut chunk)).await {
            Ok(Ok(0)) | Err(_) => return Ok(Reply::Continue),
            Ok(Ok(n)) => n,
            Ok(Err(e)) => return Err(e),
        };
        buf.extend_from_slice(&chunk[..n]);
        if buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let head = String::from_utf8_lossy(&buf);
    let request_line = head.lines().next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");

    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    if method != "GET" || path.trim_end_matches('/') != "/auth/callback" {
        respond(&mut stream, 404, "没有这个地址。").await?;
        return Ok(Reply::Continue);
    }
    let pairs = parse_query(query);
    let get = |k: &str| {
        pairs
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    if let Some(err) = get("error") {
        let desc = get("error_description").unwrap_or_default();
        respond(&mut stream, 400, &format!("授权被拒绝：{err} {desc}")).await?;
        return Ok(Reply::Continue);
    }
    let Some(code) = get("code") else {
        respond(&mut stream, 400, "回调里没有授权码。").await?;
        return Ok(Reply::Continue);
    };
    let state = get("state");
    if state.as_deref() != Some(expected_state) {
        respond(
            &mut stream,
            400,
            "这不是当前这次授权的回调（state 不匹配）。回到 Nexus 重新发起一次。",
        )
        .await?;
        return Ok(Reply::Continue);
    }
    respond(&mut stream, 200, "授权完成，可以关掉这个页面，回到 Nexus。").await?;
    Ok(Reply::Done(Received { code, state }))
}

async fn respond(stream: &mut TcpStream, status: u16, text: &str) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let body = format!(
        "<!doctype html><html lang=\"zh\"><head><meta charset=\"utf-8\"><title>Nexus</title>\
         <style>body{{font:15px/1.6 -apple-system,system-ui,sans-serif;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#0f1115;color:#e6e6e6}}\
         main{{max-width:28em;padding:2em;text-align:center}}h1{{font-size:18px;margin:0 0 .5em}}</style></head>\
         <body><main><h1>{}</h1><p>{}</p></main></body></html>",
        if status == 200 { "Nexus · ChatGPT 授权" } else { "Nexus" },
        html_escape(text)
    );
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\ncache-control: no-store\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body.as_bytes()).await?;
    stream.flush().await?;
    let _ = stream.shutdown().await;
    Ok(())
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(port: u16, path: &str) -> (u16, String) {
        let res = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{port}{path}"))
            .send()
            .await
            .unwrap();
        (res.status().as_u16(), res.text().await.unwrap())
    }

    #[tokio::test]
    async fn waits_past_junk_and_wrong_state_until_the_right_callback_arrives() {
        let server = CallbackServer::bind_port(0).await.unwrap();
        let port = server.port();
        let waiter = tokio::spawn(async move {
            server
                .wait("good-state", Duration::from_secs(10), &|| false)
                .await
        });
        // 浏览器会先要 favicon；有人贴错了 state；最后才是正确的回调。
        let (s1, _) = get(port, "/favicon.ico").await;
        assert_eq!(s1, 404);
        let (s2, body2) = get(port, "/auth/callback?code=c1&state=bad").await;
        assert_eq!(s2, 400);
        assert!(body2.contains("state"));
        let (s3, _) = get(
            port,
            "/auth/callback?error=access_denied&error_description=nope",
        )
        .await;
        assert_eq!(s3, 400);
        let (s4, body4) = get(port, "/auth/callback?code=the%2Fcode&state=good-state").await;
        assert_eq!(s4, 200);
        assert!(body4.contains("授权完成"));
        let got = waiter.await.unwrap().unwrap();
        assert_eq!(got.code, "the/code");
        assert_eq!(got.state.as_deref(), Some("good-state"));
    }

    #[tokio::test]
    async fn timeout_and_cancel_are_reported_with_their_codes() {
        let server = CallbackServer::bind_port(0).await.unwrap();
        let err = server
            .wait("s", Duration::from_millis(50), &|| false)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::OauthTimeout);

        let server = CallbackServer::bind_port(0).await.unwrap();
        let err = server
            .wait("s", Duration::from_secs(5), &|| true)
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Cancelled);
    }

    #[tokio::test]
    async fn a_busy_port_fails_to_bind_instead_of_stealing_it() {
        let first = CallbackServer::bind_port(0).await.unwrap();
        let port = first.port();
        assert!(CallbackServer::bind_port(port).await.is_err());
    }
}
