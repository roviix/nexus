//! 嵌入式 Broker 协调引擎 (Embedded Broker Engine)。
//!
//! 在开发测试、私有部署或云端集群中作为信赖中枢运行：
//! 1. 接收 Provider A 的反向隧道接入并完成握手注册；
//! 2. 当 Consumer B 请求模型推理时，向目标 Provider 发送 `TunnelFrame::Invoke` 并在流式管道回传；
//! 3. 亦可向 Provider A 申请建立指向上游（如 api.claude.ai:443）的 L4 虚拟流。

use crate::protocol::{StreamId, TunnelFrame};
use crate::types::{MarketNode, NodeId, SessionLease};
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info};

pub struct EmbeddedBroker {
    providers: Arc<RwLock<HashMap<NodeId, ProviderSession>>>,
    next_stream_id: AtomicU64,
}

type PendingConnects = Arc<RwLock<HashMap<StreamId, oneshot::Sender<Result<(), String>>>>>;

struct ProviderSession {
    device_name: Option<String>,
    outbound_tx: mpsc::Sender<TunnelFrame>,
    pending_connects: PendingConnects,
    active_stream_txs: Arc<RwLock<HashMap<StreamId, mpsc::Sender<Bytes>>>>,
    active_invoke_txs: Arc<RwLock<HashMap<StreamId, mpsc::Sender<TunnelFrame>>>>,
    leases: Vec<SessionLease>,
}

impl Default for EmbeddedBroker {
    fn default() -> Self {
        Self::new()
    }
}

impl EmbeddedBroker {
    pub fn new() -> Self {
        Self {
            providers: Arc::new(RwLock::new(HashMap::new())),
            next_stream_id: AtomicU64::new(1),
        }
    }

    /// 检查特定 Provider 节点是否在线。
    pub async fn is_provider_online(&self, node_id: &NodeId) -> bool {
        let providers = self.providers.read().await;
        providers.contains_key(node_id)
    }

    /// 获取当前所有在线 Provider 列表。
    pub async fn online_providers(&self) -> Vec<NodeId> {
        let providers = self.providers.read().await;
        providers.keys().cloned().collect()
    }

    /// 根据设备名或 NodeId 前缀精准匹配在线 Provider。
    pub async fn resolve_target_provider(&self, target_prefix: &str) -> Option<NodeId> {
        let clean = target_prefix.trim().to_ascii_lowercase();
        let providers = self.providers.read().await;

        // 1. 精确匹配 node_id
        for (id, _) in providers.iter() {
            let id_str = id.0.to_ascii_lowercase();
            if id_str == clean || id_str.starts_with(&clean) || clean.starts_with(&id_str) {
                return Some(id.clone());
            }
        }

        // 2. 匹配 device_name
        for (id, session) in providers.iter() {
            if let Some(ref name) = session.device_name {
                let name_clean = name.trim().to_ascii_lowercase();
                let name_hyphen = name_clean.replace(' ', "-");
                if name_clean == clean
                    || name_hyphen == clean
                    || clean.starts_with(&name_clean)
                    || clean.starts_with(&name_hyphen)
                {
                    return Some(id.clone());
                }
            }
        }

        None
    }

    /// 注册一个已连接的 Provider 反向隧道，并启动帧调度循环。
    pub async fn accept_provider<S>(
        &self,
        node_id: NodeId,
        stream: S,
        initial_leases: Vec<SessionLease>,
    ) where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        self.accept_provider_with_meta(node_id, None, stream, initial_leases)
            .await;
    }

    /// 附带设备名称注册 Provider 反向隧道。
    pub async fn accept_provider_with_meta<S>(
        &self,
        node_id: NodeId,
        device_name: Option<String>,
        stream: S,
        initial_leases: Vec<SessionLease>,
    ) where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        info!(%node_id, ?device_name, "接收到 Provider 节点的反向隧道连接");

        let (mut reader, mut writer) = tokio::io::split(stream);
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<TunnelFrame>(1024);
        let pending_connects = Arc::new(RwLock::new(HashMap::new()));
        let active_stream_txs = Arc::new(RwLock::new(HashMap::new()));
        let active_invoke_txs = Arc::new(RwLock::new(HashMap::new()));

        let session = ProviderSession {
            device_name,
            outbound_tx: outbound_tx.clone(),
            pending_connects: Arc::clone(&pending_connects),
            active_stream_txs: Arc::clone(&active_stream_txs),
            active_invoke_txs: Arc::clone(&active_invoke_txs),
            leases: initial_leases,
        };

        {
            let mut providers = self.providers.write().await;
            providers.insert(node_id.clone(), session);
        }

        // 写任务
        let writer_task = async move {
            while let Some(frame) = outbound_rx.recv().await {
                if let Ok(encoded) = frame.encode() {
                    if writer.write_all(&encoded).await.is_err() {
                        break;
                    }
                }
            }
        };

        // 读任务
        let pending_clone = Arc::clone(&pending_connects);
        let active_clone = Arc::clone(&active_stream_txs);
        let active_invoke_clone = Arc::clone(&active_invoke_txs);
        let outbound_tx_clone = outbound_tx.clone();

        let reader_task = async move {
            let mut read_buf = bytes::BytesMut::with_capacity(16 * 1024);
            let mut temp_buf = [0u8; 8192];

            loop {
                while let Ok(Some(frame)) = TunnelFrame::decode(&mut read_buf) {
                    match frame {
                        TunnelFrame::Ping { timestamp } => {
                            let _ = outbound_tx_clone
                                .send(TunnelFrame::Pong { timestamp })
                                .await;
                        }
                        TunnelFrame::Connected {
                            stream_id,
                            success,
                            error,
                        } => {
                            let mut pend = pending_clone.write().await;
                            if let Some(tx) = pend.remove(&stream_id) {
                                let res = if success {
                                    Ok(())
                                } else {
                                    Err(error.unwrap_or_else(|| "连接失败".to_string()))
                                };
                                let _ = tx.send(res);
                            }
                        }
                        TunnelFrame::Data { stream_id, data } => {
                            let active = active_clone.read().await;
                            if let Some(tx) = active.get(&stream_id) {
                                let _ = tx.send(data).await;
                            }
                        }
                        TunnelFrame::Close { stream_id, .. } => {
                            {
                                let mut active = active_clone.write().await;
                                active.remove(&stream_id);
                            }
                            {
                                let mut invokes = active_invoke_clone.write().await;
                                invokes.remove(&stream_id);
                            }
                        }
                        TunnelFrame::Delta { stream_id, .. }
                        | TunnelFrame::Finish { stream_id, .. }
                        | TunnelFrame::Error { stream_id, .. } => {
                            let is_end = matches!(
                                frame,
                                TunnelFrame::Finish { .. } | TunnelFrame::Error { .. }
                            );
                            let mut invokes = active_invoke_clone.write().await;
                            if let Some(tx) = invokes.get(&stream_id) {
                                let _ = tx.send(frame).await;
                            }
                            if is_end {
                                invokes.remove(&stream_id);
                            }
                        }
                        _ => {}
                    }
                }

                // 45 秒心跳保活检测：若超期未收到任何帧（客户端应每 15 秒发送 Ping），主动断开清理死链
                match tokio::time::timeout(
                    std::time::Duration::from_secs(45),
                    reader.read(&mut temp_buf),
                )
                .await
                {
                    Ok(Ok(0)) | Ok(Err(_)) | Err(_) => break,
                    Ok(Ok(n)) => read_buf.extend_from_slice(&temp_buf[..n]),
                }
            }
        };

        tokio::select! {
            _ = writer_task => {},
            _ = reader_task => {},
        }

        // 清理注销
        {
            let mut providers = self.providers.write().await;
            providers.remove(&node_id);
        }
        info!(%node_id, "Provider 反向隧道连接已断开");
    }

    /// 向指定的 Provider 发起结构化推理调用 (L7 Invoke)，返回帧接收队列。
    pub async fn invoke_via_provider(
        &self,
        node_id: &NodeId,
        channel_id: &str,
        model: &str,
        payload_json: String,
    ) -> Result<(StreamId, mpsc::Receiver<TunnelFrame>), String> {
        let stream_id = self.next_stream_id.fetch_add(1, Ordering::SeqCst);
        let (outbound_tx, active_invoke_txs) = {
            let providers = self.providers.read().await;
            let session = providers
                .get(node_id)
                .ok_or_else(|| format!("Provider 节点 {node_id} 当前不在线"))?;
            (
                session.outbound_tx.clone(),
                Arc::clone(&session.active_invoke_txs),
            )
        };

        let (resp_tx, resp_rx) = mpsc::channel::<TunnelFrame>(64);
        {
            let mut invokes = active_invoke_txs.write().await;
            invokes.insert(stream_id, resp_tx);
        }

        outbound_tx
            .send(TunnelFrame::Invoke {
                stream_id,
                channel_id: channel_id.to_string(),
                model: model.to_string(),
                payload_json,
            })
            .await
            .map_err(|e| format!("向 Provider 发送 Invoke 帧失败: {e}"))?;

        Ok((stream_id, resp_rx))
    }

    /// 向指定的 Provider 发起出站虚拟流申请，返回本地虚拟 Socket (DuplexStream)。
    pub async fn dial_via_provider(
        &self,
        node_id: &NodeId,
        host: &str,
        port: u16,
    ) -> Result<DuplexStream, String> {
        let stream_id = self.next_stream_id.fetch_add(1, Ordering::SeqCst);

        let (outbound_tx, pending_connects, active_stream_txs) = {
            let providers = self.providers.read().await;
            let session = providers
                .get(node_id)
                .ok_or_else(|| format!("Provider 节点 {node_id} 当前不在线"))?;
            (
                session.outbound_tx.clone(),
                Arc::clone(&session.pending_connects),
                Arc::clone(&session.active_stream_txs),
            )
        };

        let (connect_tx, connect_rx) = oneshot::channel();
        {
            let mut pend = pending_connects.write().await;
            pend.insert(stream_id, connect_tx);
        }

        // 发送 Connect 帧
        outbound_tx
            .send(TunnelFrame::Connect {
                stream_id,
                host: host.to_string(),
                port,
            })
            .await
            .map_err(|e| format!("发送 Connect 帧失败: {e}"))?;

        // 等待 Provider 反馈出站 TCP 连接结果
        match connect_rx.await {
            Ok(Ok(())) => {
                debug!(stream_id, host, port, "Provider 出站 TCP 成功建立");
            }
            Ok(Err(e)) => return Err(format!("Provider 出站连接被拒绝: {e}")),
            Err(_) => return Err("等待 Provider 连接超时或中断".to_string()),
        }

        // 创建本地虚拟全双工套接字
        let (local_stream, remote_side) = tokio::io::duplex(64 * 1024);
        let (mut remote_read, mut remote_write) = tokio::io::split(remote_side);
        let (stream_data_tx, mut stream_data_rx) = mpsc::channel::<Bytes>(64);
        {
            let mut active = active_stream_txs.write().await;
            active.insert(stream_id, stream_data_tx);
        }

        // 任务 1: 本地写 -> 发送 Data 帧到 Provider
        let outbound_tx_clone = outbound_tx.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            loop {
                match remote_read.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = Bytes::copy_from_slice(&buf[..n]);
                        if outbound_tx_clone
                            .send(TunnelFrame::Data {
                                stream_id,
                                data: chunk,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            let _ = outbound_tx_clone
                .send(TunnelFrame::Close {
                    stream_id,
                    reason: None,
                })
                .await;
        });

        // 任务 2: Provider 发来的 Data 帧 -> 写入本地读端
        tokio::spawn(async move {
            while let Some(chunk) = stream_data_rx.recv().await {
                if remote_write.write_all(&chunk).await.is_err() {
                    break;
                }
            }
        });

        Ok(local_stream)
    }

    /// 列出当前在线的订阅资源列表。
    pub async fn list_market_nodes(&self) -> Vec<MarketNode> {
        let providers = self.providers.read().await;
        let mut list = Vec::new();
        for (node_id, session) in providers.iter() {
            for lease in &session.leases {
                list.push(MarketNode {
                    node_id: node_id.0.clone(),
                    platform: lease.platform.clone(),
                    model: lease.model_alias.clone(),
                    region: "CN-Residential".to_string(),
                    latency_ms: 38,
                    available_quota: "100% (活跃)".to_string(),
                });
            }
        }
        list
    }
}
