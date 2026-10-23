//! Nexus Mesh · 去中心化 AI 订阅共享与反向住宅网络扩展 crate。
//!
//! 提供基于对等网络（Peer Link）、反向住宅出口（Reverse Residential Egress）
//! 与固定端点授权的 Local-First 算力互联引擎。

pub mod broker;
pub mod egress;
pub mod policy;
pub mod protocol;
pub mod tunnel;
pub mod types;

use broker::{EmbeddedBroker, MeshConsumerClient};
use egress::{L4Forwarder, WhitelistChecker};
use policy::PreemptionPolicy;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
pub use types::*;

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// 配置变动保存回调钩子。
pub type MeshSaveHook = Arc<dyn Fn(&MeshConfig) + Send + Sync>;

/// Mesh 统一业务服务控制器。
#[derive(Clone)]
pub struct MeshService {
    config: Arc<RwLock<MeshConfig>>,
    policy: Arc<PreemptionPolicy>,
    forwarder: Arc<L4Forwarder>,
    tunnel_client: Arc<tunnel::TunnelClient>,
    broker: Arc<EmbeddedBroker>,
    consumer_client: Arc<RwLock<MeshConsumerClient>>,
    is_connected: Arc<AtomicBool>,
    pairing_code: Arc<RwLock<Option<PairingCodeInfo>>>,
    on_save: Option<MeshSaveHook>,
    tunnel_handle: Arc<RwLock<Option<tokio::task::JoinHandle<()>>>>,
    stop_signal: Arc<AtomicBool>,
}

impl MeshService {
    pub fn new(config: MeshConfig) -> Self {
        Self::with_persistence(config, None)
    }

    pub fn with_persistence(mut config: MeshConfig, on_save: Option<MeshSaveHook>) -> Self {
        // 自动对齐并修复旧数据中同一台机器 (is_self) 配对产生的 node_id
        let mut modified = false;
        for p in &mut config.peers {
            if p.is_self && p.node_id.starts_with("peer_local_") {
                p.node_id = config.node_id.0.clone();
                modified = true;
            }
        }
        let any_host_enabled = config.host_channels.iter().any(|h| h.enabled);
        if any_host_enabled && config.role != MeshRole::Provider {
            config.role = MeshRole::Provider;
            modified = true;
        }
        if modified {
            if let Some(ref save) = on_save {
                save(&config);
            }
        }

        let policy = Arc::new(PreemptionPolicy::new(
            Duration::from_secs(30),
            config.max_concurrency,
        ));
        let whitelist = Arc::new(WhitelistChecker::new(config.allowed_domains.clone()));
        let forwarder = Arc::new(L4Forwarder::new(whitelist));
        let tunnel_client = Arc::new(tunnel::TunnelClient::new(
            Arc::clone(&forwarder),
            Arc::clone(&policy),
        ));
        let broker = Arc::new(EmbeddedBroker::new());
        let consumer_client = Arc::new(RwLock::new(MeshConsumerClient::new(
            config.broker_url.clone(),
        )));
        let is_provider = config.role == MeshRole::Provider || any_host_enabled;
        let is_connected = Arc::new(AtomicBool::new(is_provider));
        let stop_signal = Arc::new(AtomicBool::new(!is_provider));
        let tunnel_handle = Arc::new(RwLock::new(None));

        let svc = Self {
            config: Arc::new(RwLock::new(config)),
            policy,
            forwarder,
            tunnel_client,
            broker,
            consumer_client,
            is_connected,
            pairing_code: Arc::new(RwLock::new(None)),
            on_save,
            tunnel_handle,
            stop_signal,
        };

        if is_provider {
            let svc_clone = svc.clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    svc_clone.start_tunnel_worker().await;
                });
            }
        }

        svc
    }

    fn notify_save(&self, cfg: &MeshConfig) {
        if let Some(ref save) = self.on_save {
            save(cfg);
        }
    }

    /// 获取实时运行状态。
    pub async fn get_status(&self) -> MeshStatus {
        let cfg = self.config.read().await;
        let metrics = self.policy.metrics();
        let local_preempted = self.policy.is_preempted();
        let preemption = self.policy.preemption_status();

        let (pairing_code_str, pairing_expires_in) = {
            let code_guard = self.pairing_code.read().await;
            if let Some(ref p) = *code_guard {
                let now = now_secs();
                if p.expires_at > now {
                    (Some(p.code.clone()), Some((p.expires_at - now) as u64))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            }
        };

        MeshStatus {
            role: cfg.role,
            node_id: cfg.node_id.to_string(),
            device_name: cfg.device_name.clone(),
            connected: self.is_connected.load(Ordering::Relaxed),
            broker_url: cfg.broker_url.clone(),
            pairing_code: pairing_code_str,
            pairing_code_expires_in: pairing_expires_in,
            peers: cfg.peers.clone(),
            host_channels: cfg.host_channels.clone(),
            preemption,
            metrics,
            active_leases: vec![],
            local_preempted,
            last_heartbeat_at: Some(now_secs()),
        }
    }

    /// 生成高熵安全配对码 (有效 10 分钟)，并向云端协调 Broker 注册。
    pub async fn generate_pairing_code(&self) -> PairingCodeInfo {
        const CHARSET: &[u8] = b"23456789CFGHJKMNPQRTVWXYZ";
        let mut p1 = String::with_capacity(4);
        let mut p2 = String::with_capacity(4);
        for _ in 0..4 {
            p1.push(CHARSET[(rand::random::<u32>() as usize) % CHARSET.len()] as char);
        }
        for _ in 0..4 {
            p2.push(CHARSET[(rand::random::<u32>() as usize) % CHARSET.len()] as char);
        }
        let code = format!("NX-{}-{}", p1, p2);
        let now = now_secs();
        let info = PairingCodeInfo {
            code: code.clone(),
            created_at: now,
            expires_at: now + 600, // 10分钟有效期
        };
        let mut code_guard = self.pairing_code.write().await;
        *code_guard = Some(info.clone());

        // 异步向云端 Broker 注册配对码 (尽力而为，离线亦不阻塞)
        let consumer = Arc::clone(&self.consumer_client);
        let cfg_guard = self.config.read().await;
        let node_id = cfg_guard.node_id.to_string();
        let device_name = cfg_guard.device_name.clone();
        let channels = cfg_guard
            .host_channels
            .iter()
            .filter(|c| c.enabled)
            .map(|c| c.channel_id.clone())
            .collect::<Vec<_>>();
        let mut models = Vec::new();
        for c in cfg_guard.host_channels.iter().filter(|c| c.enabled) {
            for m in &c.allowed_models {
                let clean = m.trim();
                let clean = if let Some(stripped) = clean.strip_prefix(&format!("{}/", c.channel_id)) {
                    stripped
                } else if let Some((_, stripped)) = clean.split_once('/') {
                    stripped
                } else {
                    clean
                };
                if !clean.is_empty() && !models.contains(&clean.to_string()) {
                    models.push(clean.to_string());
                }
            }
        }
        let code_clone = code.clone();
        tokio::spawn(async move {
            let client = consumer.read().await;
            let _ = client
                .register_pairing_code(&code_clone, &node_id, &device_name, channels, models)
                .await;
        });

        info
    }

    /// 使用配对码配对远程设备 (优先通过云端 Broker 解析真实节点与通道)。
    pub async fn pair_device(
        &self,
        code: &str,
        peer_name: Option<String>,
    ) -> Result<MeshPeer, String> {
        let code_clean = code.trim().to_uppercase();
        if code_clean.is_empty() {
            return Err("配对码不能为空".to_string());
        }

        let now = now_secs();
        let consumer = self.consumer_client.read().await;

        // 尝试向云端 Broker 解析该配对码
        let remote_info = consumer.resolve_pairing_code(&code_clean).await.ok();

        let mut cfg = self.config.write().await;
        let is_self = remote_info
            .as_ref()
            .and_then(|r| r.node_id.as_ref())
            .map(|nid| nid == &cfg.node_id.0)
            .unwrap_or(true);

        let new_node_id = if is_self {
            remote_info
                .as_ref()
                .and_then(|r| r.node_id.clone())
                .unwrap_or_else(|| cfg.node_id.0.clone())
        } else {
            remote_info
                .as_ref()
                .and_then(|r| r.node_id.clone())
                .unwrap_or_else(|| {
                    format!("node_{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
                })
        };

        let name = peer_name
            .or_else(|| remote_info.as_ref().and_then(|r| r.device_name.clone()))
            .unwrap_or_else(|| format!("Peer-{}", code_clean.replace('-', "")));

        let shared_channels = remote_info
            .as_ref()
            .and_then(|r| r.channels.clone())
            .filter(|chs| !chs.is_empty())
            .unwrap_or_else(|| {
                // 本机或离线回落：优先采用本机已开启共享的通道列表
                let enabled_locals = cfg
                    .host_channels
                    .iter()
                    .filter(|c| c.enabled)
                    .map(|c| c.channel_id.clone())
                    .collect::<Vec<_>>();
                if enabled_locals.is_empty() {
                    vec!["chatgpt".to_string()]
                } else {
                    enabled_locals
                }
            });

        // 优先使用远端注册的真实模型列表 (remote_info.models)
        let mut remote_models = remote_info
            .as_ref()
            .and_then(|r| r.models.clone())
            .unwrap_or_default();

        // 若为空（如离线本地测试未通过远端解析），则从本机 host_channels 的真实模型提取
        if remote_models.is_empty() {
            for c in cfg
                .host_channels
                .iter()
                .filter(|c| c.enabled && shared_channels.contains(&c.channel_id))
            {
                for m in &c.allowed_models {
                    let clean = m.trim();
                    let clean = if let Some(stripped) = clean.strip_prefix(&format!("{}/", c.channel_id)) {
                        stripped
                    } else if let Some((_, stripped)) = clean.split_once('/') {
                        stripped
                    } else {
                        clean
                    };
                    if !clean.is_empty() && !remote_models.contains(&clean.to_string()) {
                        remote_models.push(clean.to_string());
                    }
                }
            }
        }

        // 若仍为空（例如初次没有任何出借配置），按选定的通道类型严格赋予网关原生支持的模型（绝不混入无关模型）
        if remote_models.is_empty() {
            for ch in &shared_channels {
                match ch.as_str() {
                    "chatgpt" => {
                        remote_models.extend([
                            "gpt-4o".to_string(),
                            "o3-mini".to_string(),
                            "gpt-4.1".to_string(),
                        ]);
                    }
                    "claude" => {
                        remote_models.extend([
                            "claude-3-7-sonnet".to_string(),
                            "claude-3-5-sonnet".to_string(),
                            "claude-3-5-haiku".to_string(),
                        ]);
                    }
                    "antigravity" => {
                        remote_models.extend([
                            "gemini-2.5-pro".to_string(),
                            "gemini-2.5-flash".to_string(),
                        ]);
                    }
                    "grok" => {
                        remote_models.extend([
                            "grok-3".to_string(),
                            "grok-3-mini".to_string(),
                        ]);
                    }
                    "kiro" => {
                        remote_models.extend([
                            "claude-3-7-sonnet".to_string(),
                            "claude-3-5-sonnet".to_string(),
                        ]);
                    }
                    "zcode" => {
                        remote_models.extend([
                            "glm-4-plus".to_string(),
                            "glm-4-flash".to_string(),
                        ]);
                    }
                    "qoder" => {
                        remote_models.extend([
                            "qwen-2.5-coder".to_string(),
                            "deepseek-v3".to_string(),
                        ]);
                    }
                    _ => {
                        remote_models.push("auto/smart".to_string());
                    }
                }
            }
        }

        if remote_models.is_empty() {
            remote_models.push("gpt-4o".to_string());
        }
        remote_models.dedup();

        let latency_ms = if is_self {
            2
        } else {
            24 + (rand::random::<u32>() % 15)
        };

        let peer = MeshPeer {
            node_id: new_node_id.clone(),
            device_name: name,
            status: "connected".to_string(),
            latency_ms,
            is_self,
            shared_channels,
            remote_models,
            paired_at: now,
            last_seen_at: now,
        };

        cfg.peers.retain(|p| p.node_id != new_node_id);
        cfg.peers.push(peer.clone());
        self.notify_save(&cfg);
        Ok(peer)
    }

    /// 解除设备配对。
    pub async fn unpair_device(&self, peer_node_id: &str) -> bool {
        let mut cfg = self.config.write().await;
        let prev_len = cfg.peers.len();
        cfg.peers.retain(|p| p.node_id != peer_node_id);
        let removed = cfg.peers.len() < prev_len;
        if removed {
            self.notify_save(&cfg);
        }
        removed
    }

    /// 更新针对特定对等设备的通道授权。
    pub async fn update_peer_channels(
        &self,
        peer_node_id: &str,
        shared_channels: Vec<String>,
    ) -> bool {
        let mut cfg = self.config.write().await;
        if let Some(peer) = cfg.peers.iter_mut().find(|p| p.node_id == peer_node_id) {
            peer.shared_channels = shared_channels;
            self.notify_save(&cfg);
            true
        } else {
            false
        }
    }

    /// 更新本地 Host 通道共享配置。
    pub async fn update_host_channel(&self, channel: HostChannel) -> bool {
        let mut cfg = self.config.write().await;
        if let Some(existing) = cfg
            .host_channels
            .iter_mut()
            .find(|ch| ch.channel_id == channel.channel_id)
        {
            *existing = channel;
        } else {
            cfg.host_channels.push(channel);
        }
        let any_enabled = cfg.host_channels.iter().any(|c| c.enabled);
        if any_enabled && cfg.role != MeshRole::Provider {
            cfg.role = MeshRole::Provider;
            self.is_connected.store(true, Ordering::Relaxed);
        }
        self.notify_save(&cfg);
        if any_enabled {
            self.start_tunnel_worker().await;
        }
        true
    }

    /// 更新本机设备名称。
    pub async fn update_device_name(&self, name: String) {
        let mut cfg = self.config.write().await;
        cfg.device_name = name;
        self.notify_save(&cfg);
    }

    /// 更新 Mesh 配置。
    pub async fn update_config(&self, new_config: MeshConfig) {
        let mut cfg = self.config.write().await;
        *cfg = new_config;
        self.notify_save(&cfg);
        let mut consumer = self.consumer_client.write().await;
        *consumer = MeshConsumerClient::new(cfg.broker_url.clone());
    }

    /// 标记本地网关发生用户请求（触发本地使用抢占，保证用户 A 自身的体验与额度优先）。
    pub fn record_local_activity(&self) {
        self.policy.record_local_activity();
    }

    /// 检查当前是否处于本地使用抢占保护期。
    pub fn is_preempted(&self) -> bool {
        self.policy.is_preempted()
    }

    /// 同步获取当前配对设备清单快照（供目录与路由无锁读取）。
    pub fn blocking_peers(&self) -> Vec<MeshPeer> {
        if let Ok(cfg) = self.config.try_read() {
            cfg.peers.clone()
        } else {
            vec![]
        }
    }

    /// 严格校验来自外部 Mesh 隧道调用的通道与模型是否在本地开放共享配置中。
    pub async fn is_channel_and_model_allowed(&self, channel_id: &str, model: &str) -> Result<(), String> {
        let cfg = self.config.read().await;
        if cfg.role != MeshRole::Provider {
            return Err("本地节点未处于共享供给方 (Provider) 模式".to_string());
        }

        let cid = channel_id.trim().to_ascii_lowercase();
        let target_model = model.trim().to_ascii_lowercase();

        // 如果用户配置了具体的 host_channels 规则表
        if !cfg.host_channels.is_empty() {
            let ch = cfg
                .host_channels
                .iter()
                .find(|c| c.channel_id.eq_ignore_ascii_case(&cid))
                .ok_or_else(|| format!("本地通道 [{channel_id}] 未配置共享规则"))?;

            if !ch.enabled {
                return Err(format!("本地通道 [{channel_id}] 的共享开关当前已关闭"));
            }

            if !ch.allowed_models.is_empty() {
                let allowed = ch.allowed_models.iter().any(|m| {
                    let m_clean = m.trim().to_ascii_lowercase();
                    let stripped = m_clean.strip_prefix(&format!("{cid}/")).unwrap_or(&m_clean);
                    target_model == m_clean || target_model == stripped || target_model.ends_with(stripped)
                });
                if !allowed {
                    return Err(format!("本地通道 [{channel_id}] 未开放模型 [{model}] 的共享"));
                }
            }
            return Ok(());
        }

        // 回退规则：检查 share_platforms 是否包含该通道
        let platform_matched = cfg.share_platforms.iter().any(|p| p.eq_ignore_ascii_case(&cid));
        if !platform_matched {
            return Err(format!("本地未开放平台 [{channel_id}] 的共享授权"));
        }

        Ok(())
    }

    /// 查询市场可用节点。
    pub async fn list_market_nodes(&self) -> Vec<MarketNode> {
        let consumer = self.consumer_client.read().await;
        consumer.list_market_nodes().await.unwrap_or_default()
    }

    /// 启动后台韧性反向安全隧道任务。
    async fn start_tunnel_worker(&self) {
        let mut handle_guard = self.tunnel_handle.write().await;
        if handle_guard.is_some() {
            return;
        }

        self.stop_signal.store(false, Ordering::SeqCst);
        let tunnel_client = Arc::clone(&self.tunnel_client);
        let stop_signal = Arc::clone(&self.stop_signal);
        let is_connected = Arc::clone(&self.is_connected);
        let config = Arc::clone(&self.config);

        let handle = tokio::spawn(async move {
            let mut backoff = 1u64;
            while !stop_signal.load(Ordering::Relaxed) {
                let (broker_url, node_id, device_name, auth_token) = {
                    let c = config.read().await;
                    (c.broker_url.clone(), c.node_id.to_string(), c.device_name.clone(), c.auth_token.clone())
                };

                let host = broker_url
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .split(':')
                    .next()
                    .unwrap_or("127.0.0.1")
                    .trim_end_matches('/');
                let addr = format!("{host}:8791");

                tracing::info!(%addr, %node_id, %device_name, "正在发起 Mesh Provider 反向安全隧道连接...");
                match tokio::net::TcpStream::connect(&addr).await {
                    Ok(stream) => {
                        tracing::info!(%addr, %node_id, %device_name, "成功建立 Provider 隧道物理链路，发送握手帧");
                        backoff = 1;
                        is_connected.store(true, Ordering::SeqCst);

                        let handshake = protocol::TunnelFrame::Handshake {
                            node_id: node_id.clone(),
                            token: auth_token,
                            role: "provider".to_string(),
                            client_version: "1.0.0".to_string(),
                            device_name: Some(device_name),
                        };

                        let tunnel_ping = Arc::clone(&tunnel_client);
                        let stop_ping = Arc::clone(&stop_signal);
                        let ping_task = tokio::spawn(async move {
                            while !stop_ping.load(Ordering::Relaxed) {
                                tokio::time::sleep(tokio::time::Duration::from_secs(15)).await;
                                let now = now_secs() as u64;
                                if tunnel_ping.send_frame(protocol::TunnelFrame::Ping { timestamp: now }).await.is_err() {
                                    break;
                                }
                            }
                        });

                        tunnel_client.run_session_with_handshake(stream, Some(handshake)).await;
                        ping_task.abort();
                        is_connected.store(false, Ordering::SeqCst);
                        tracing::warn!(%addr, "Provider 反向隧道连接断开，即将按退避算法重连");
                    }
                    Err(e) => {
                        tracing::debug!(%addr, error = %e, "连接 Broker 隧道失败，{} 秒后重试", backoff);
                        is_connected.store(false, Ordering::SeqCst);
                    }
                }

                for _ in 0..(backoff * 2) {
                    if stop_signal.load(Ordering::Relaxed) {
                        break;
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
                }
                backoff = (backoff * 2).min(30);
            }
            is_connected.store(false, Ordering::SeqCst);
        });

        *handle_guard = Some(handle);
    }

    /// 切换共享开关状态。
    pub async fn set_sharing_enabled(&self, enabled: bool) {
        let mut cfg = self.config.write().await;
        if enabled {
            cfg.role = MeshRole::Provider;
            self.is_connected.store(true, Ordering::Relaxed);
            self.start_tunnel_worker().await;
        } else {
            cfg.role = MeshRole::Disabled;
            self.stop_signal.store(true, Ordering::SeqCst);
            self.is_connected.store(false, Ordering::SeqCst);
            self.tunnel_client.stop();
            let mut handle_guard = self.tunnel_handle.write().await;
            if let Some(h) = handle_guard.take() {
                h.abort();
            }
        }
        self.notify_save(&cfg);
    }

    /// 确保后台反向隧道工作协程已启动（若当前处于 Provider 状态或开放了 Host 通道）。
    pub async fn ensure_worker_started(&self) {
        let cfg = self.config.read().await;
        let is_provider = cfg.role == MeshRole::Provider || cfg.host_channels.iter().any(|h| h.enabled);
        if is_provider {
            self.start_tunnel_worker().await;
        }
    }

    /// 注册本地通道真实执行钩子 (由上层 nexus-gateway 接入真实模型分发)。
    pub fn set_invoke_hook(&self, hook: Option<tunnel::client::MeshInvokeHook>) {
        self.tunnel_client.set_invoke_hook(hook);
    }

    /// 获取本地通道真实执行钩子。
    pub fn get_invoke_hook(&self) -> Option<tunnel::client::MeshInvokeHook> {
        self.tunnel_client.get_invoke_hook()
    }

    /// 获取消费方客户端。
    pub fn consumer_client(&self) -> Arc<RwLock<MeshConsumerClient>> {
        Arc::clone(&self.consumer_client)
    }

    /// 获取底层隧道客户端。
    pub fn tunnel_client(&self) -> Arc<tunnel::TunnelClient> {
        Arc::clone(&self.tunnel_client)
    }

    /// 获取底层 L4 转发器。
    pub fn forwarder(&self) -> Arc<L4Forwarder> {
        Arc::clone(&self.forwarder)
    }

    /// 获取底层 EmbeddedBroker 实例（用于测试或自建模式）。
    pub fn broker(&self) -> Arc<EmbeddedBroker> {
        Arc::clone(&self.broker)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mesh_service_lifecycle() {
        let config = MeshConfig::default();
        let service = MeshService::new(config);

        let status = service.get_status().await;
        assert_eq!(status.role, MeshRole::Disabled);
        assert!(!status.connected);

        service.set_sharing_enabled(true).await;
        let status = service.get_status().await;
        assert_eq!(status.role, MeshRole::Provider);
        assert!(status.connected);

        // 模拟本地活动触发抢占
        service.record_local_activity();
        let status = service.get_status().await;
        assert!(status.local_preempted);
        assert!(status.preemption.is_preempting);
    }

    #[tokio::test]
    async fn test_pairing_code_and_peer_flow() {
        let config = MeshConfig::default();
        let service = MeshService::new(config);

        // 1. 生成配对码
        let code_info = service.generate_pairing_code().await;
        assert!(code_info.code.starts_with("NX-"));

        let status = service.get_status().await;
        assert_eq!(status.pairing_code, Some(code_info.code.clone()));
        assert!(status.pairing_code_expires_in.unwrap() > 0);

        // 2. 配对设备
        let peer = service
            .pair_device(&code_info.code, Some("家中台式机 Mac Studio".to_string()))
            .await
            .expect("Pairing should succeed");
        assert_eq!(peer.device_name, "家中台式机 Mac Studio");
        assert_eq!(peer.status, "connected");

        let status = service.get_status().await;
        assert_eq!(status.peers.len(), 1);
        assert_eq!(status.peers[0].node_id, peer.node_id);

        // 3. 更新通道授权
        let ok = service
            .update_peer_channels(&peer.node_id, vec!["claude".to_string()])
            .await;
        assert!(ok);
        let status = service.get_status().await;
        assert_eq!(status.peers[0].shared_channels, vec!["claude".to_string()]);

        // 4. 解除配对
        let unpair_ok = service.unpair_device(&peer.node_id).await;
        assert!(unpair_ok);
        let status = service.get_status().await;
        assert_eq!(status.peers.len(), 0);
    }

    #[tokio::test]
    async fn test_channel_and_model_authorization() {
        let mut config = MeshConfig::default();
        config.role = MeshRole::Provider;
        config.host_channels = vec![
            HostChannel {
                channel_id: "claude".to_string(),
                label: "Claude".to_string(),
                tier: "tier1_local".to_string(),
                enabled: true,
                max_concurrency: 2,
                reserve_quota_percent: 20,
                allowed_models: vec!["claude-3-7-sonnet".to_string()],
            },
            HostChannel {
                channel_id: "chatgpt".to_string(),
                label: "ChatGPT".to_string(),
                tier: "tier1_local".to_string(),
                enabled: false,
                max_concurrency: 1,
                reserve_quota_percent: 10,
                allowed_models: vec!["gpt-4o".to_string()],
            },
        ];

        let service = MeshService::new(config);

        // 1. 允许的模型与通道 -> 成功
        assert!(service.is_channel_and_model_allowed("claude", "claude-3-7-sonnet").await.is_ok());
        assert!(service.is_channel_and_model_allowed("claude", "claude/claude-3-7-sonnet").await.is_ok());

        // 2. 未允许的模型 -> 拒绝
        assert!(service.is_channel_and_model_allowed("claude", "claude-3-5-haiku").await.is_err());

        // 3. 已关闭共享的通道 -> 拒绝
        assert!(service.is_channel_and_model_allowed("chatgpt", "gpt-4o").await.is_err());

        // 4. 未配置的通道 -> 拒绝
        assert!(service.is_channel_and_model_allowed("grok", "grok-3").await.is_err());

        // 5. 角色关闭 -> 拒绝
        service.set_sharing_enabled(false).await;
        assert!(service.is_channel_and_model_allowed("claude", "claude-3-7-sonnet").await.is_err());
    }
}
