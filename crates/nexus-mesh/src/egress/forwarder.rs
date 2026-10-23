//! L4 盲 TCP 转发执行器。
//!
//! 在用户 A 的机器上运行：仅负责把反向隧道中的密文字节流与上游官方服务器的原始 TCP 连接互转。
//! 用户 A 的操作系统与网络抓包只能看到标准 TLS 1.3 密文，无解密私钥。

use super::whitelist::WhitelistChecker;
use bytes::Bytes;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

pub struct L4Forwarder {
    whitelist: Arc<WhitelistChecker>,
}

impl L4Forwarder {
    pub fn new(whitelist: Arc<WhitelistChecker>) -> Self {
        Self { whitelist }
    }

    pub fn check_allowed(&self, host: &str, port: u16) -> Result<(), &'static str> {
        self.whitelist.is_allowed(host, port)
    }

    /// 校验并建立到目标的原始 TCP 连接，启动双向数据流桥接。
    pub async fn dial_and_bridge(
        &self,
        stream_id: u64,
        host: String,
        port: u16,
        mut inbound_rx: mpsc::Receiver<Bytes>,
        outbound_tx: mpsc::Sender<Bytes>,
    ) -> Result<(), String> {
        // 1. 白名单安全门禁校验
        if let Err(reason) = self.whitelist.is_allowed(&host, port) {
            warn!(stream_id, host = %host, port, "出站连接被白名单拦截: {reason}");
            return Err(reason.to_string());
        }

        // 2. 使用本地住宅网卡建立 TCP 连接 (出口 IP = 本机家庭宽带)
        debug!(stream_id, host = %host, port, "正在发起本地出站 TCP 拨号...");
        let mut tcp_stream = match TcpStream::connect((host.as_str(), port)).await {
            Ok(s) => {
                info!(stream_id, host = %host, port, "出站 TCP 链路已建立");
                s
            }
            Err(e) => {
                error!(stream_id, host = %host, port, "出站 TCP 拨号失败: {e}");
                return Err(format!("TCP 连接失败: {e}"));
            }
        };

        // 3. 拆分读写流并执行双向桥接
        let (mut tcp_read, mut tcp_write) = tcp_stream.split();

        // 任务 A: 从反向隧道接收密文 -> 写入官方 TCP 连接
        let write_task = async move {
            while let Some(chunk) = inbound_rx.recv().await {
                if let Err(e) = tcp_write.write_all(&chunk).await {
                    debug!(stream_id, "写入上游 TCP 失败: {e}");
                    break;
                }
            }
            let _ = tcp_write.shutdown().await;
        };

        // 任务 B: 从官方 TCP 连接读取响应密文 -> 发送回反向隧道
        let read_task = async move {
            let mut buf = [0u8; 8192];
            loop {
                match tcp_read.read(&mut buf).await {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        let chunk = Bytes::copy_from_slice(&buf[..n]);
                        if outbound_tx.send(chunk).await.is_err() {
                            break; // 隧道已关闭
                        }
                    }
                    Err(e) => {
                        debug!(stream_id, "从上游 TCP 读取失败: {e}");
                        break;
                    }
                }
            }
        };

        tokio::select! {
            _ = write_task => {},
            _ = read_task => {},
        }

        debug!(stream_id, "L4 盲桥接流已正常终结");
        Ok(())
    }
}
