//! Nexus Mesh 核心数据类型与模型。

use serde::{Deserialize, Serialize};

/// 节点唯一身份标识。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct NodeId(pub String);

impl NodeId {
    pub fn generate() -> Self {
        Self(format!("node_{}", uuid::Uuid::new_v4().simple()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 节点在 Mesh 网络中的工作角色。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MeshRole {
    /// 关闭共享网络
    #[default]
    Disabled,
    /// 供给方模式：通过本机的干净宽带网络与闲置订阅为他人提供中继出口
    Provider,
    /// 消费方模式：使用 Mesh 网络的订阅节点资源进行推理
    Consumer,
    /// 对等对联模式：既使用受信任设备算力，也可选择性对等共享通道
    Peer,
}

/// 临时配对码信息 (6 位验证码，10 分钟有效)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairingCodeInfo {
    pub code: String,
    pub created_at: i64,
    pub expires_at: i64,
}

/// 已配对的受信任对等设备 (Trusted Peer)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshPeer {
    pub node_id: String,
    pub device_name: String,
    pub status: String, // "connected" | "connecting" | "offline"
    pub latency_ms: u32,
    pub is_self: bool,
    pub shared_channels: Vec<String>, // 本机授权给该设备的通道列表 (如 ["claude", "ollama"])
    pub remote_models: Vec<String>,   // 该设备向我公开的可用模型清单 (如 ["claude-3-7-sonnet"])
    pub paired_at: i64,
    pub last_seen_at: i64,
}

/// 本机托管的通道共享规则 (Host Channel)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostChannel {
    pub channel_id: String,
    pub label: String,
    pub tier: String, // "tier1_local" | "tier2_byok" | "tier3_residential"
    pub enabled: bool,
    pub max_concurrency: usize,
    pub reserve_quota_percent: u8,
    pub allowed_models: Vec<String>,
}

/// 本地所有者优先抢占状态
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PreemptionStatus {
    pub is_preempting: bool,
    pub cooldown_remaining_secs: u64,
    pub last_local_activity: Option<i64>,
}

/// Mesh 网络配置项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshConfig {
    /// 启用角色
    pub role: MeshRole,
    /// 本节点 NodeId
    pub node_id: NodeId,
    /// 本机显示名称 (例如 "MacBook Pro", "家中台式机")
    #[serde(default = "default_device_name")]
    pub device_name: String,
    /// 协调中继服务器地址
    pub broker_url: String,
    /// 鉴权凭证 / 访问密钥 (可选)
    pub auth_token: Option<String>,
    /// 允许共享的账号平台 (例如 ["claude", "chatgpt"])
    pub share_platforms: Vec<String>,
    /// 最大并发转发连接数
    pub max_concurrency: usize,
    /// 自留额度百分比 (0-100)，默认保留 20%
    pub reserve_quota_percent: u8,
    /// 是否开启本地使用优先抢占 (当 A 本地发请求时自动挂起或限流外部 Mesh 流量)
    pub local_preemption: bool,
    /// 严格域名白名单 (只有在此名单内的目标主机才允许由本地网卡发起出站连接)
    pub allowed_domains: Vec<String>,
    /// 用户明确勾选共享的具体账号 ID 列表
    #[serde(default)]
    pub shared_account_ids: Vec<String>,
    /// 仅在电脑闲置时接单共享
    #[serde(default)]
    pub idle_only: bool,
    /// 私人圈子配对邀请码
    #[serde(default)]
    pub invite_code: Option<String>,
    /// 受信任的配对设备表
    #[serde(default)]
    pub peers: Vec<MeshPeer>,
    /// 本地通道开放规则
    #[serde(default = "default_host_channels")]
    pub host_channels: Vec<HostChannel>,
}

fn default_device_name() -> String {
    format!("Nexus-{}", &uuid::Uuid::new_v4().simple().to_string()[..6])
}

pub fn default_host_channels() -> Vec<HostChannel> {
    Vec::new()
}

impl Default for MeshConfig {
    fn default() -> Self {
        Self {
            role: MeshRole::Disabled,
            node_id: NodeId::generate(),
            device_name: default_device_name(),
            broker_url: "https://nexus.roviix.com".to_string(),
            auth_token: None,
            share_platforms: vec!["claude".to_string(), "chatgpt".to_string(), "cursor".to_string()],
            max_concurrency: 3,
            reserve_quota_percent: 20,
            local_preemption: true,
            allowed_domains: vec![
                "api.claude.ai".to_string(),
                "claude.ai".to_string(),
                "api.anthropic.com".to_string(),
                "api.openai.com".to_string(),
                "chatgpt.com".to_string(),
                "api.x.ai".to_string(),
                "grok.com".to_string(),
                "api.kiro.ai".to_string(),
                "open.bigmodel.cn".to_string(),
                "*.cursor.sh".to_string(),
                "api2.cursor.sh".to_string(),
                "agent.global.api5.cursor.sh".to_string(),
                "cursor.sh".to_string(),
            ],
            shared_account_ids: Vec::new(),
            idle_only: true,
            invite_code: None,
            peers: Vec::new(),
            host_channels: default_host_channels(),
        }
    }
}

/// 临时租约对象 (Session Lease)：由供给方发布、有效生命周期由心跳维持。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLease {
    pub lease_id: String,
    pub provider_node_id: NodeId,
    pub platform: String,
    pub model_alias: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub is_active: bool,
}

/// 运行时统计指标。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct EgressMetrics {
    /// 累计服务请求数
    pub total_requests_served: u64,
    /// 累计转发字节数
    pub total_bytes_forwarded: u64,
    /// 当前活跃连接数
    pub active_connections: usize,
    /// 拒绝连接数 (如白名单拦截或本地抢占)
    pub rejected_connections: u64,
    /// 今日预估收益/贡献点
    pub contribution_points: u64,
}

/// 供给方或消费方当前的运行状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshStatus {
    pub role: MeshRole,
    pub node_id: String,
    pub device_name: String,
    pub connected: bool,
    pub broker_url: String,
    pub pairing_code: Option<String>,
    pub pairing_code_expires_in: Option<u64>,
    pub peers: Vec<MeshPeer>,
    pub host_channels: Vec<HostChannel>,
    pub preemption: PreemptionStatus,
    pub metrics: EgressMetrics,
    pub active_leases: Vec<SessionLease>,
    pub local_preempted: bool,
    pub last_heartbeat_at: Option<i64>,
}

/// 市场中可用的订阅资源池节点概览。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketNode {
    pub node_id: String,
    pub platform: String,
    pub model: String,
    pub region: String,
    pub latency_ms: u32,
    pub available_quota: String,
}
