//! pod 里的两个 RPC：exec daemon（:1337，读续期种子）与 `SandBoxService/EnsureSandBox`
//! （唤醒 pod、拿 exec daemon 地址——descriptor 推不出地址时的兜底）。
//!
//! 都是 Connect RPC。消息字段照 0.18 源码 / 探针（`gateway/scripts/lib/grokbot-pod-credential.mjs`）
//! 手写 `prost` derive，只列用到的字段——protobuf 对多余 / 缺失字段都宽容。

use crate::descriptor::BoxRelayDescriptor;
use nexus_core::{AppError, ErrorCode, Result};
use prost::Message;
use std::time::Duration;

const READ_SBI_CMD: &str = "for p in /proc/[0-9]*/environ; do tr '\\0' '\\n' < \"$p\" 2>/dev/null; done | grep ^SAND_INFERENCE_RENEWAL_CREDENTIAL= | head -1 | cut -d= -f2-";
const EXEC_PATH: &str = "/agent.v1.ExecService/Exec";
const ENSURE_SANDBOX_URL: &str = "https://api2.cursor.sh/aiserver.v1.SandBoxService/EnsureSandBox";
const TIMEOUT: Duration = Duration::from_secs(45);

// ---------- ExecService ----------

#[derive(Clone, PartialEq, Message)]
struct ShellCommandParsingResult {
    #[prost(bool, tag = "1")]
    parsing_failed: bool,
    #[prost(bool, tag = "3")]
    has_redirects: bool,
    #[prost(bool, tag = "4")]
    has_command_substitution: bool,
}

#[derive(Clone, PartialEq, Message)]
struct ShellArgs {
    #[prost(string, tag = "1")]
    command: String,
    #[prost(string, tag = "2")]
    working_directory: String,
    #[prost(int32, tag = "3")]
    timeout: i32,
    #[prost(string, tag = "4")]
    tool_call_id: String,
    #[prost(string, repeated, tag = "5")]
    simple_commands: Vec<String>,
    #[prost(message, optional, tag = "8")]
    parsing_result: Option<ShellCommandParsingResult>,
    #[prost(bool, tag = "12")]
    skip_approval: bool,
}

#[derive(Clone, PartialEq, Message)]
struct ExecServerMessage {
    #[prost(uint32, tag = "1")]
    id: u32,
    #[prost(message, optional, tag = "2")]
    shell_args: Option<ShellArgs>,
    #[prost(string, tag = "15")]
    exec_id: String,
}

#[derive(Clone, PartialEq, Message)]
struct ShellSuccess {
    #[prost(string, tag = "5")]
    stdout: String,
    #[prost(string, tag = "6")]
    stderr: String,
}

#[derive(Clone, PartialEq, Message)]
struct ShellSpawnError {
    #[prost(string, tag = "3")]
    error: String,
}

#[derive(Clone, PartialEq, Message)]
struct ShellResult {
    #[prost(message, optional, tag = "1")]
    success: Option<ShellSuccess>,
    #[prost(message, optional, tag = "5")]
    spawn_error: Option<ShellSpawnError>,
}

#[derive(Clone, PartialEq, Message)]
struct ExecClientMessage {
    #[prost(message, optional, tag = "2")]
    shell_result: Option<ShellResult>,
}

#[derive(Clone, PartialEq, Message)]
struct ExecStreamElement {
    #[prost(message, optional, tag = "1")]
    exec_client_message: Option<ExecClientMessage>,
}

// ---------- SandBoxService ----------

#[derive(Clone, PartialEq, Message)]
struct EnsureSandBoxRequest {
    #[prost(bool, optional, tag = "2")]
    wake: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
pub struct EnsureSandBoxResponse {
    #[prost(string, tag = "3")]
    pub pod_id: String,
    #[prost(string, tag = "4")]
    pub network_token: String,
    #[prost(string, tag = "5")]
    pub exec_daemon_auth_token: String,
    #[prost(string, tag = "6")]
    pub exec_daemon_url: String,
}

/// exec daemon 的连接参数。
#[derive(Debug, Clone)]
pub struct ExecTarget {
    pub base_url: String,
    pub auth_token: String,
    pub network_token: String,
}

impl ExecTarget {
    /// 从 descriptor 推：同 pod 的 :1337，`Bearer local`，网络 token 同 descriptor 头。
    pub fn from_descriptor(d: &BoxRelayDescriptor) -> Option<Self> {
        Some(Self {
            base_url: d.exec_daemon_url()?,
            auth_token: "local".into(),
            network_token: d.network_token()?.to_string(),
        })
    }

    pub fn from_ensure(r: &EnsureSandBoxResponse) -> Option<Self> {
        if r.exec_daemon_url.is_empty() {
            return None;
        }
        Some(Self {
            base_url: r.exec_daemon_url.trim_end_matches('/').to_string(),
            auth_token: if r.exec_daemon_auth_token.is_empty() {
                "local".into()
            } else {
                r.exec_daemon_auth_token.clone()
            },
            network_token: r.network_token.clone(),
        })
    }
}

// ---------- Connect 信封 ----------

fn envelope(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(0);
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(payload);
    out
}

/// 拆 Connect 流：返回 (flags, payload) 序列。`flags & 2` 是流尾（JSON 错误 / trailer）。
fn split_envelopes(mut buf: &[u8]) -> Vec<(u8, &[u8])> {
    let mut out = Vec::new();
    while buf.len() >= 5 {
        let flags = buf[0];
        let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
        if buf.len() < 5 + len {
            break;
        }
        out.push((flags, &buf[5..5 + len]));
        buf = &buf[5 + len..];
    }
    out
}

fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .map_err(|e| AppError::internal(format!("建 HTTP 客户端失败：{e}")))
}

/// 在 pod 里跑一条 shell，返回 stdout。
pub async fn shell(target: &ExecTarget, command: &str) -> Result<String> {
    let req = ExecServerMessage {
        id: 1,
        exec_id: uuid::Uuid::new_v4().to_string(),
        shell_args: Some(ShellArgs {
            command: command.to_string(),
            working_directory: "/workspace".into(),
            timeout: 45_000,
            tool_call_id: "nexus-grokbot".into(),
            simple_commands: vec![command
                .split_whitespace()
                .next()
                .unwrap_or("sh")
                .to_string()],
            parsing_result: Some(ShellCommandParsingResult::default()),
            skip_approval: true,
        }),
    };
    let body = envelope(&req.encode_to_vec());
    let resp = http()?
        .post(format!("{}{EXEC_PATH}", target.base_url))
        .header("content-type", "application/connect+proto")
        .header("connect-protocol-version", "1")
        .header("authorization", format!("Bearer {}", target.auth_token))
        .header("x-anyrun-network-token", &target.network_token)
        .body(body)
        .send()
        .await
        .map_err(|e| AppError::network(format!("连 pod exec daemon 失败：{e}")))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::network(format!("读 pod exec 响应失败：{e}")))?;
    if !status.is_success() {
        return Err(AppError::upstream(format!(
            "pod exec daemon HTTP {status}：{}",
            String::from_utf8_lossy(&bytes[..bytes.len().min(200)])
        ))
        .with_hint("pod 可能在休眠；先在 Grok Bot 里随便发一句让它醒来，或稍后重试。"));
    }
    let mut stdout = String::new();
    let mut stderr = String::new();
    for (flags, payload) in split_envelopes(&bytes) {
        if flags & 2 != 0 {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(payload) {
                if let Some(err) = v.get("error") {
                    return Err(AppError::upstream(format!("pod exec 返回错误：{err}")));
                }
            }
            continue;
        }
        if let Ok(el) = ExecStreamElement::decode(payload) {
            if let Some(r) = el.exec_client_message.and_then(|m| m.shell_result) {
                if let Some(s) = r.success {
                    stdout = s.stdout;
                    stderr = s.stderr;
                } else if let Some(e) = r.spawn_error {
                    stderr = e.error;
                }
            }
        }
    }
    if stdout.trim().is_empty() && !stderr.is_empty() {
        return Err(AppError::upstream(format!("pod shell 失败：{stderr}")));
    }
    Ok(stdout.trim().to_string())
}

/// 读 `SAND_INFERENCE_RENEWAL_CREDENTIAL`（`sbi_*`）。
pub async fn read_renewal_credential(target: &ExecTarget) -> Result<String> {
    let sbi = shell(target, READ_SBI_CMD).await?;
    if !sbi.starts_with("sbi_") {
        return Err(
            AppError::upstream("pod 里没读到 SAND_INFERENCE_RENEWAL_CREDENTIAL。").with_hint(
                "sand-host 可能还没起来；在 Grok Bot 里发一句话让 Box 完成初始化后重试。",
            ),
        );
    }
    Ok(sbi)
}

/// `EnsureSandBox(wake=true)`：唤醒 pod 并拿 exec daemon 地址。需要 Grok Bot 的 session token。
pub async fn ensure_sandbox(
    session_token: &str,
    machine_id: &str,
) -> Result<EnsureSandBoxResponse> {
    let body = EnsureSandBoxRequest { wake: Some(true) }.encode_to_vec();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let resp = http()?
        .post(ENSURE_SANDBOX_URL)
        .header("content-type", "application/proto")
        .header("connect-protocol-version", "1")
        .header("authorization", format!("Bearer {session_token}"))
        .header(
            "x-cursor-checksum",
            crate::credential::checksum(machine_id, now_ms),
        )
        .header("x-cursor-client-type", "sand")
        .header(
            "x-cursor-client-version",
            crate::credential::DEFAULT_CLIENT_VERSION,
        )
        .header("x-sand-box-namespace", crate::credential::DEFAULT_NAMESPACE)
        .header("x-ghost-mode", "false")
        .header("x-request-id", uuid::Uuid::new_v4().to_string())
        .body(body)
        .send()
        .await
        .map_err(|e| AppError::network(format!("EnsureSandBox 失败：{e}")))?;
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::network(format!("读 EnsureSandBox 响应失败：{e}")))?;
    if !status.is_success() {
        return Err(AppError::new(
            if status.as_u16() == 401 {
                ErrorCode::Unauthorized
            } else {
                ErrorCode::Upstream
            },
            format!(
                "EnsureSandBox HTTP {status}：{}",
                String::from_utf8_lossy(&bytes[..bytes.len().min(200)])
            ),
        ));
    }
    EnsureSandBoxResponse::decode(&bytes[..])
        .map_err(|e| AppError::upstream(format!("EnsureSandBox 响应解不开：{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_round_trips() {
        let e = envelope(b"abc");
        assert_eq!(e[0], 0);
        assert_eq!(&e[1..5], &3u32.to_be_bytes());
        let parts = split_envelopes(&e);
        assert_eq!(parts, vec![(0u8, &b"abc"[..])]);
    }

    #[test]
    fn split_envelopes_stops_at_truncated_frame() {
        let mut buf = envelope(b"ok");
        buf.extend_from_slice(&[0, 0, 0, 0, 9, 1]);
        assert_eq!(split_envelopes(&buf).len(), 1);
    }

    #[test]
    fn exec_request_encodes_with_expected_tags() {
        let msg = ExecServerMessage {
            id: 1,
            exec_id: "x".into(),
            shell_args: Some(ShellArgs {
                command: "echo".into(),
                ..Default::default()
            }),
        };
        let bytes = msg.encode_to_vec();
        let back = ExecServerMessage::decode(&bytes[..]).unwrap();
        assert_eq!(back, msg);
        // tag 15 varint key = (15<<3)|2 = 122 (0x7a)
        assert!(bytes.contains(&0x7a));
    }

    #[test]
    fn exec_target_from_descriptor_requires_pod_shape() {
        let d = BoxRelayDescriptor {
            version: 1,
            base_url: "https://a-pod-b-1340.us11.cursorvm.com".into(),
            token: "t".into(),
            headers: [("x-anyrun-network-token".to_string(), "nto".to_string())].into(),
            relay_path: crate::BOX_RELAY_PATH.into(),
            account_fingerprint: None,
        };
        let t = ExecTarget::from_descriptor(&d).unwrap();
        assert_eq!(t.base_url, "https://a-pod-b-1337.us11.cursorvm.com");
        assert_eq!(t.auth_token, "local");
        assert_eq!(t.network_token, "nto");
    }
}
