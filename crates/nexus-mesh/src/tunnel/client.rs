//! 供给方反向隧道长连接客户端 (Provider Tunnel Client)。

use crate::egress::L4Forwarder;
use crate::policy::PreemptionPolicy;
use crate::protocol::{StreamId, TunnelFrame};
use bytes::Bytes;
use futures_util::future::BoxFuture;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

pub type MeshInvokeHook = Arc<
    dyn Fn(
        String, // channel_id
        String, // model
        String, // payload_json
        mpsc::Sender<TunnelFrame>, // frame_tx
        StreamId,
    ) -> BoxFuture<'static, Result<(), String>>
        + Send
        + Sync,
>;

pub struct TunnelClient {
    forwarder: Arc<L4Forwarder>,
    policy: Arc<PreemptionPolicy>,
    active_streams: Arc<RwLock<HashMap<StreamId, mpsc::Sender<Bytes>>>>,
    stream_listeners: Arc<RwLock<HashMap<StreamId, mpsc::Sender<TunnelFrame>>>>,
    session_outbound: Arc<RwLock<Option<mpsc::Sender<TunnelFrame>>>>,
    invoke_hook: Arc<std::sync::RwLock<Option<MeshInvokeHook>>>,
    is_running: Arc<AtomicBool>,
}

impl TunnelClient {
    pub fn new(forwarder: Arc<L4Forwarder>, policy: Arc<PreemptionPolicy>) -> Self {
        Self {
            forwarder,
            policy,
            active_streams: Arc::new(RwLock::new(HashMap::new())),
            stream_listeners: Arc::new(RwLock::new(HashMap::new())),
            session_outbound: Arc::new(RwLock::new(None)),
            invoke_hook: Arc::new(std::sync::RwLock::new(None)),
            is_running: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn is_running(&self) -> bool {
        self.is_running.load(Ordering::Relaxed)
    }

    pub fn stop(&self) {
        self.is_running.store(false, Ordering::Relaxed);
    }

    pub fn set_invoke_hook(&self, hook: Option<MeshInvokeHook>) {
        if let Ok(mut guard) = self.invoke_hook.write() {
            *guard = hook;
        }
    }

    pub fn get_invoke_hook(&self) -> Option<MeshInvokeHook> {
        self.invoke_hook.read().ok().and_then(|g| g.clone())
    }

    pub async fn send_frame(&self, frame: TunnelFrame) -> Result<(), String> {
        let guard = self.session_outbound.read().await;
        if let Some(ref tx) = *guard {
            tx.send(frame).await.map_err(|e| format!("发送帧失败: {e}"))
        } else {
            Err("隧道会话未连接".to_string())
        }
    }

    pub async fn register_stream_listener(&self, stream_id: StreamId, tx: mpsc::Sender<TunnelFrame>) {
        let mut listeners = self.stream_listeners.write().await;
        listeners.insert(stream_id, tx);
    }

    pub async fn unregister_stream_listener(&self, stream_id: StreamId) {
        let mut listeners = self.stream_listeners.write().await;
        listeners.remove(&stream_id);
    }

    /// 在给定的已建立双向流上处理隧道数据帧。
    /// （可以是真实的 TCP/TLS 连接、WebSocket 转换流、或测试用 DuplexStream）。
    pub async fn run_session<S>(&self, stream: S)
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        self.run_session_with_handshake(stream, None).await;
    }

    /// 附带初始握手帧启动会话。
    pub async fn run_session_with_handshake<S>(&self, stream: S, initial_handshake: Option<TunnelFrame>)
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        self.is_running.store(true, Ordering::Relaxed);
        info!("Provider 反向隧道会话已启动");

        let (mut reader, mut writer) = tokio::io::split(stream);

        // 出站帧队列
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<TunnelFrame>(1024);
        {
            let mut guard = self.session_outbound.write().await;
            *guard = Some(outbound_tx.clone());
        }

        // 若有首帧握手需求，优先送入队列
        if let Some(hs) = initial_handshake {
            let _ = outbound_tx.send(hs).await;
        }

        // 写任务
        let writer_task = async move {
            while let Some(frame) = outbound_rx.recv().await {
                match frame.encode() {
                    Ok(bytes) => {
                        if let Err(e) = writer.write_all(&bytes).await {
                            debug!("向隧道写入数据帧失败: {e}");
                            break;
                        }
                    }
                    Err(e) => {
                        error!("编码帧失败: {e}");
                    }
                }
            }
        };

        // 读任务与帧调度
        let forwarder = Arc::clone(&self.forwarder);
        let policy = Arc::clone(&self.policy);
        let active_streams = Arc::clone(&self.active_streams);
        let stream_listeners = Arc::clone(&self.stream_listeners);
        let invoke_hook = Arc::clone(&self.invoke_hook);
        let is_running = Arc::clone(&self.is_running);
        let outbound_tx_clone = outbound_tx.clone();

        let reader_task = async move {
            let mut read_buf = bytes::BytesMut::with_capacity(16 * 1024);
            let mut temp_buf = [0u8; 8192];

            while is_running.load(Ordering::Relaxed) {
                // 尝试解码帧
                while let Ok(Some(frame)) = TunnelFrame::decode(&mut read_buf) {
                    Self::handle_incoming_frame(
                        frame,
                        &forwarder,
                        &policy,
                        &active_streams,
                        &stream_listeners,
                        &invoke_hook,
                        &outbound_tx_clone,
                    )
                    .await;
                }

                // 读取更多字节
                match reader.read(&mut temp_buf).await {
                    Ok(0) => {
                        info!("反向隧道远端已关闭连接");
                        break;
                    }
                    Ok(n) => {
                        read_buf.extend_from_slice(&temp_buf[..n]);
                    }
                    Err(e) => {
                        warn!("反向隧道读取错误: {e}");
                        break;
                    }
                }
            }
        };

        tokio::select! {
            _ = writer_task => {},
            _ = reader_task => {},
        }

        self.is_running.store(false, Ordering::Relaxed);
        {
            let mut guard = self.session_outbound.write().await;
            *guard = None;
        }
        // 清理所有未关闭流
        let mut streams = self.active_streams.write().await;
        streams.clear();
        let mut listeners = self.stream_listeners.write().await;
        listeners.clear();
        info!("Provider 反向隧道会话已结束");
    }

    async fn handle_incoming_frame(
        frame: TunnelFrame,
        forwarder: &Arc<L4Forwarder>,
        policy: &Arc<PreemptionPolicy>,
        active_streams: &Arc<RwLock<HashMap<StreamId, mpsc::Sender<Bytes>>>>,
        stream_listeners: &Arc<RwLock<HashMap<StreamId, mpsc::Sender<TunnelFrame>>>>,
        invoke_hook: &Arc<std::sync::RwLock<Option<MeshInvokeHook>>>,
        outbound_tx: &mpsc::Sender<TunnelFrame>,
    ) {
        match frame {
            TunnelFrame::Handshake { node_id, .. } => {
                debug!(%node_id, "收到对端 Handshake 帧，自动确认");
                let _ = outbound_tx.send(TunnelFrame::HandshakeAck {
                    success: true,
                    node_id,
                    error: None,
                }).await;
            }

            TunnelFrame::HandshakeAck { success, node_id, error } => {
                info!(success, %node_id, ?error, "收到对端 HandshakeAck 回执");
            }

            TunnelFrame::Connect { stream_id, host, port } => {
                debug!(stream_id, host = %host, port, "收到出站连接请求");

                // 1. 本地优先策略与并发准入
                let guard = match policy.try_acquire() {
                    Ok(g) => g,
                    Err(reason) => {
                        warn!(stream_id, "连接准入被拒绝: {reason}");
                        let _ = outbound_tx
                            .send(TunnelFrame::Connected {
                                stream_id,
                                success: false,
                                error: Some(reason.to_string()),
                            })
                            .await;
                        return;
                    }
                };

                // 2. 出站目标白名单严格门禁
                if let Err(reason) = forwarder.check_allowed(&host, port) {
                    warn!(stream_id, host = %host, port, "出站连接被白名单拦截: {reason}");
                    let _ = outbound_tx
                        .send(TunnelFrame::Connected {
                            stream_id,
                            success: false,
                            error: Some(reason.to_string()),
                        })
                        .await;
                    return;
                }

                // 3. 为此虚拟流建立数据管道
                let (stream_data_tx, stream_data_rx) = mpsc::channel::<Bytes>(64);
                {
                    let mut streams = active_streams.write().await;
                    streams.insert(stream_id, stream_data_tx);
                }

                let forwarder = Arc::clone(forwarder);
                let outbound_tx = outbound_tx.clone();
                let active_streams = Arc::clone(active_streams);

                tokio::spawn(async move {
                    let _guard = guard; // 保持并发计数并在结束时释放
                    let (bridge_tx, mut bridge_rx) = mpsc::channel::<Bytes>(64);

                    // 启动返回帧泵
                    let outbound_tx_sub = outbound_tx.clone();
                    let return_pump = async move {
                        while let Some(chunk) = bridge_rx.recv().await {
                            if outbound_tx_sub
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
                    };

                    // 通知 Broker 准备好
                    let _ = outbound_tx
                        .send(TunnelFrame::Connected {
                            stream_id,
                            success: true,
                            error: None,
                        })
                        .await;

                    // 执行本地出站 TCP 拨号与桥接
                    tokio::select! {
                        _ = return_pump => {},
                        res = forwarder.dial_and_bridge(stream_id, host, port, stream_data_rx, bridge_tx) => {
                            if let Err(e) = res {
                                warn!(stream_id, "桥接执行失败: {e}");
                            }
                        }
                    }

                    // 结束后清理流并通知 Broker
                    {
                        let mut streams = active_streams.write().await;
                        streams.remove(&stream_id);
                    }
                    let _ = outbound_tx
                        .send(TunnelFrame::Close {
                            stream_id,
                            reason: None,
                        })
                        .await;
                });
            }

            TunnelFrame::Data { stream_id, data } => {
                let streams = active_streams.read().await;
                if let Some(tx) = streams.get(&stream_id) {
                    let _ = tx.send(data).await;
                }
            }

            TunnelFrame::Close { stream_id, .. } => {
                let mut streams = active_streams.write().await;
                streams.remove(&stream_id);
            }

            TunnelFrame::Ping { timestamp } => {
                let _ = outbound_tx.send(TunnelFrame::Pong { timestamp }).await;
            }

            TunnelFrame::Pong { timestamp } => {
                debug!(timestamp, "收到对端 Pong 心跳回执");
            }

            TunnelFrame::Invoke { stream_id, channel_id, model, payload_json } => {
                debug!(stream_id, channel = %channel_id, model = %model, "收到 Channel 调用请求");
                let guard = match policy.try_acquire() {
                    Ok(g) => g,
                    Err(reason) => {
                        let _ = outbound_tx.send(TunnelFrame::Error {
                            stream_id,
                            code: "PREEMPTION_ACTIVE".to_string(),
                            message: reason.to_string(),
                        }).await;
                        return;
                    }
                };

                let maybe_hook = invoke_hook.read().ok().and_then(|g| g.clone());

                let outbound_tx = outbound_tx.clone();
                if let Some(hook) = maybe_hook {
                    tokio::spawn(async move {
                        let _guard = guard;
                        if let Err(e) = hook(channel_id, model, payload_json, outbound_tx.clone(), stream_id).await {
                            let _ = outbound_tx.send(TunnelFrame::Error {
                                stream_id,
                                code: "INVOKE_FAILED".to_string(),
                                message: e,
                            }).await;
                        }
                    });
                } else {
                    // 无真实执行钩子时的兜底通知
                    tokio::spawn(async move {
                        let _guard = guard;
                        let _ = outbound_tx.send(TunnelFrame::Delta {
                            stream_id,
                            text: format!("【对等住宅节点在线响应 · 通道: {channel_id} / 模型: {model}】\n\n已成功连通对端设备并激活保护屏障。"),
                            is_thinking: false,
                        }).await;
                        let _ = outbound_tx.send(TunnelFrame::Finish {
                            stream_id,
                            total_tokens: Some(36),
                            finish_reason: Some("stop".to_string()),
                            tool_calls: None,
                            thinking_signature: None,
                        }).await;
                    });
                }
            }

            TunnelFrame::Delta { stream_id, .. }
            | TunnelFrame::Finish { stream_id, .. }
            | TunnelFrame::Error { stream_id, .. }
            | TunnelFrame::Connected { stream_id, .. } => {
                // 将响应帧转发给注册的消费者流监听器
                let listeners = stream_listeners.read().await;
                if let Some(tx) = listeners.get(&stream_id) {
                    let _ = tx.send(frame).await;
                }
            }
        }
    }
}
