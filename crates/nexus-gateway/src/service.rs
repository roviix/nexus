//! 编排：起停 / 状态 / 设置 / 当前用哪个号。Tauri 层只跟它打交道。
//!
//! 网关**默认关闭**，由用户点开（ARCHITECTURE §3.4：可整体拆卸，坏了只影响网关）。开着的时候是一个
//! 常驻的 `127.0.0.1` 监听；关掉就是把监听收掉，所有在途请求随之结束。
//!
//! 本地口令（api key）是必填的：所有客户端都有对应的环境变量可以填；有它，本机别的进程就不能
//! 顺手用你的号。第一次起服务时随机生成，
//! 进 `SecretStore`，界面上显式点「显示」才给看——它不算上游凭证，但也没必要常驻在界面上。

use crate::channel::{self, Capability, ChannelId, ChannelRegistry};
use crate::inference::StreamConfig;
use crate::lane::{
    CursorLoginSource, Lane, LaneSnapshot, LoginReader, RelayLane, Roster, Source,
    StoredAccountsSource, SubscriptionAccounts,
};
use crate::ledger::{Ledger, UsageSummary};
use crate::media::{MediaJob, MediaJobs};
use crate::provider::{self, ProviderLane};
use crate::reqlog::{LogEntry, RequestLog};
use crate::routes::{self, ClientRoute, ClientRoutes};
use crate::server::{self, Gateway};
use crate::subscriptions;
use crate::upstream::CursorUpstream;
use nexus_accounts::AccountsService;
use nexus_antigravity::AntigravityService;
use nexus_chatgpt::ChatGptService;
use nexus_claude::ClaudeService;
use nexus_core::{AppError, ErrorCode, Result, Secret};
use nexus_gemini_web::GeminiWebService;
use nexus_grok::GrokService;
use nexus_kiro::KiroService;
use nexus_qoder::QoderService;
use nexus_store::keys::SecretRef;
use nexus_store::{settings, Db, SecretStore};
use nexus_zcode::ZcodeService;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;
use tokio::sync::oneshot;

pub const SETTING_PORT: &str = "gateway.port";
pub const SETTING_AUTOSTART: &str = "gateway.autostart";
pub const SETTING_FORCE_MODEL: &str = "gateway.force_model";
pub const SETTING_DEFAULT_CHANNEL: &str = "gateway.default_channel";
/// 按客户端的路由（`routes::ClientRoutes` 的 JSON）。
pub const SETTING_CLIENT_ROUTES: &str = "gateway.client_routes";
const SECRET_API_KEY: &str = "gateway/api_key";
pub const DEFAULT_PORT: u16 = 8787;
/// 端口被占时往后最多试这么多个。端口不是用户该操心的事：占了就自动换一个，换到的那个
/// **落库**成新设置——地址稳定下来，客户端里抄过的配置才不会每次开机都失效。
const PORT_PROBE_SPAN: u16 = 40;

/// Cursor 通道打上游时贴的 `x-cursor-client-type`。**不是设置**：早先界面上有过 CLI / IDE / Sand
/// 三选一，其中 `sand` 那一档实际是「换成 Grok Bot 凭证、旁路整个号池」，和 Sand 补丁同名却不是
/// 一回事，用户分不清；`ide` 与 `cli` 之间也没有量过任何差别。现在钉死一个，网关只回答
/// 「拿号池的号、讲标准方言」这一件事。
pub const CLIENT_TYPE: &str = crate::inference::DEFAULT_CLIENT_TYPE;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySettings {
    pub port: u16,
    /// 应用启动时自动开网关。
    pub autostart: bool,
    /// 强制所有请求发这个上游模型（空 = 不强制）。
    ///
    /// 给「账号只开得了某一档」的情形：便宜号常常只能跑 `auto`，而 Claude Code 会
    /// 客户端校验模型名、不认识 `auto`。让客户端继续报它认识的名字、由网关换成账号
    /// 真能跑的那个。改写用户要的模型是件该由他自己点头的事，所以默认关。
    pub force_model: Option<String>,
    /// 裸名 / 空模型走哪条通道。出厂 `cursor`。改了立刻生效，不用重启网关。
    pub default_channel: String,
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            autostart: false,
            force_model: None,
            default_channel: channel::CURSOR.to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub port: Option<u16>,
    pub autostart: Option<bool>,
    /// `Some("")` / `Some("  ")` = 清空；`None` = 不动。
    pub force_model: Option<String>,
    pub default_channel: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningInfo {
    pub addr: String,
    pub base_url: String,
    pub started_at: String,
}

/// 一条订阅通道在状态快照里的样子：谁在队里、此刻能不能接、报哪些模型。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSnapshot {
    pub id: ChannelId,
    pub label: &'static str,
    pub vendor: &'static str,
    /// 有没有号能接聊天。
    pub ready: bool,
    /// 有没有号能接媒体（生图 / 生视频）。没有媒体能力的通道恒为 false。
    pub media_ready: bool,
    pub lane: LaneSnapshot,
    pub chat_models: Vec<String>,
    pub image_models: Vec<String>,
    pub video_models: Vec<String>,
    pub prefixes: Vec<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayStatus {
    pub running: Option<RunningInfo>,
    pub settings: GatewaySettings,
    /// 改了设置但还没重启网关，界面提示「重启生效」。
    pub restart_needed: bool,
    pub api_key_set: bool,
    /// Cursor 标准对话中转的平台级停用原因。号池仍原样保留；已有路由不会静默改投别家。
    pub cursor_inference_unavailable_reason: Option<&'static str>,
    /// Cursor 通道的接力队。
    pub lane: LaneSnapshot,
    /// 订阅通道（ChatGPT / Grok Build / Kiro …）和供应商通道，按选路顺序。
    pub channels: Vec<ChannelSnapshot>,
    /// 最近的异步媒体任务（生视频），新的在前。
    pub media_jobs: Vec<MediaJob>,
    /// 按客户端的路由。接入页看它知道每个客户端此刻走哪条通道、哪个模型。
    pub routes: ClientRoutes,
}

struct Running {
    addr: SocketAddr,
    started_at: SystemTime,
    /// 起服务时用的设置；和当前设置比对得出 `restart_needed`。
    settings: GatewaySettings,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

/// 应用里那几份订阅号服务。
///
/// 收成一个结构体而不是继续往 [`GatewayService::with_services`] 上加参数：平台还会再加，
/// 八个位置参数的调用点已经没人看得懂谁是谁了。
pub struct SubscriptionServices {
    pub chatgpt: Arc<ChatGptService>,
    pub grok: Arc<GrokService>,
    pub kiro: Arc<KiroService>,
    pub zcode: Arc<ZcodeService>,
    pub qoder: Arc<QoderService>,
    pub claude: Arc<ClaudeService>,
    pub antigravity: Arc<AntigravityService>,
    pub gemini_web: Arc<GeminiWebService>,
    pub mesh: Arc<nexus_mesh::MeshService>,
}

pub struct GatewayService {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
    /// 订阅号服务。它们的号只有网关这一个用途，进不进队看账号自己的开关，不走 `roster`。
    chatgpt: Arc<ChatGptService>,
    grok: Arc<GrokService>,
    kiro: Arc<KiroService>,
    zcode: Arc<ZcodeService>,
    qoder: Arc<QoderService>,
    claude: Arc<ClaudeService>,
    antigravity: Arc<AntigravityService>,
    gemini_web: Arc<GeminiWebService>,
    mesh: Arc<nexus_mesh::MeshService>,
    mesh_lane: Arc<dyn crate::lane::Lane>,
    /// 哪些号进接力队。跨启停都是同一份，落库。
    roster: Arc<Roster>,
    /// Cursor 正登着的号。设置页改目录时换读者，接力队还是这一份。
    cursor_login: Arc<CursorLoginSource>,
    /// Cursor 通道的接力队。接力状态（当前号、冷却）跨启停保留。
    lane: Arc<RelayLane>,
    /// 每条订阅通道自己的接力队，和 Cursor 的互不混。跨启停保留。顺序就是选路顺序。
    channel_lanes: Vec<(ChannelId, Arc<RelayLane>)>,
    running: Mutex<Option<Running>>,
    /// 请求账本。跨启停同一份——它记的是历史，不是运行态。
    ledger: Arc<Ledger>,
    /// 异步媒体任务登记簿。
    media_jobs: Arc<MediaJobs>,
    /// 用户指定的默认通道。跟正在听的 `ChannelRegistry` 共用这一把锁。
    default_channel: Arc<RwLock<String>>,
    /// 供应商通道的「号队」：几家供应商之间的接力与冷却。跨启停保留。
    provider_lane: Arc<ProviderLane>,
    /// 按客户端的路由。跟正在听的网关共用这一把锁，改了不用重启。
    routes: Arc<RwLock<ClientRoutes>>,
    /// 最近请求的明细（只在内存里）。跨启停同一份。
    log: Arc<RequestLog>,
}

impl GatewayService {
    fn parse_mesh_fallback_request(
        parsed: &serde_json::Value,
        effective_model: &str,
    ) -> crate::normalized::ChatRequest {
        let messages = if let Some(arr) = parsed.get("messages").and_then(|v| v.as_array()) {
            arr.iter()
                .map(|m| {
                    let role_str = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                    let role = match role_str {
                        "system" => crate::normalized::Role::System,
                        "assistant" => crate::normalized::Role::Assistant,
                        "tool" => crate::normalized::Role::Tool,
                        _ => crate::normalized::Role::User,
                    };
                    let text = if let Some(s) = m.get("content").and_then(|c| c.as_str()) {
                        s.to_string()
                    } else if let Some(blocks) = m.get("content").and_then(|c| c.as_array()) {
                        blocks
                            .iter()
                            .filter_map(|b| {
                                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                                    b.get("text").and_then(|t| t.as_str())
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    } else {
                        String::new()
                    };

                    let mut tool_calls = Vec::new();
                    if let Some(calls) = m.get("tool_calls").and_then(|v| v.as_array()) {
                        for tc in calls {
                            let id = tc
                                .get("id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("call_0")
                                .to_string();
                            let name = tc
                                .pointer("/function/name")
                                .and_then(|v| v.as_str())
                                .or_else(|| tc.get("name").and_then(|v| v.as_str()))
                                .unwrap_or("")
                                .to_string();
                            let arguments = tc
                                .pointer("/function/arguments")
                                .and_then(|v| v.as_str())
                                .or_else(|| tc.get("arguments").and_then(|v| v.as_str()))
                                .unwrap_or("{}")
                                .to_string();
                            if !name.is_empty() {
                                tool_calls.push(crate::normalized::ToolCall {
                                    id,
                                    name,
                                    arguments,
                                });
                            }
                        }
                    }

                    let mut tool_results = Vec::new();
                    if role == crate::normalized::Role::Tool {
                        let tool_call_id = m
                            .get("tool_call_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let tool_name = m
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        tool_results.push(crate::normalized::ToolResult {
                            tool_call_id,
                            tool_name,
                            text: text.clone(),
                            is_error: false,
                            images: Vec::new(),
                        });
                    }

                    crate::normalized::Message {
                        role,
                        text: if role == crate::normalized::Role::Tool {
                            String::new()
                        } else {
                            text
                        },
                        images: Vec::new(),
                        tool_calls,
                        tool_results,
                        reasoning: Vec::new(),
                    }
                })
                .collect()
        } else {
            vec![]
        };

        let tools = if let Some(arr) = parsed.get("tools").and_then(|v| v.as_array()) {
            arr.iter()
                .filter_map(|t| {
                    let name = t
                        .pointer("/function/name")
                        .and_then(|v| v.as_str())
                        .or_else(|| t.get("name").and_then(|v| v.as_str()))?;
                    let description = t
                        .pointer("/function/description")
                        .and_then(|v| v.as_str())
                        .or_else(|| t.get("description").and_then(|v| v.as_str()))
                        .unwrap_or("")
                        .to_string();
                    let parameters = t
                        .pointer("/function/parameters")
                        .cloned()
                        .or_else(|| t.get("parameters").cloned())
                        .unwrap_or(serde_json::json!({}));
                    Some(crate::normalized::ToolDef {
                        name: name.to_string(),
                        description,
                        parameters,
                        grammar: false,
                        namespace: None,
                    })
                })
                .collect()
        } else {
            vec![]
        };

        crate::normalized::ChatRequest {
            model: effective_model.to_string(),
            messages,
            tools,
            ..Default::default()
        }
    }

    pub fn new(
        db: Arc<Db>,
        secrets: Arc<dyn SecretStore>,
        cursor: Arc<nexus_cursor::Cursor>,
        accounts: Arc<AccountsService>,
        chatgpt: Arc<ChatGptService>,
    ) -> Self {
        let grok = Arc::new(GrokService::new(db.clone(), secrets.clone()));
        let kiro = Arc::new(KiroService::new(db.clone(), secrets.clone()));
        let zcode = Arc::new(ZcodeService::new(db.clone(), secrets.clone()));
        let qoder = Arc::new(QoderService::new(db.clone(), secrets.clone()));
        let claude = Arc::new(ClaudeService::new(db.clone(), secrets.clone()));
        let antigravity = Arc::new(AntigravityService::new(db.clone(), secrets.clone()));
        let gemini_web = Arc::new(GeminiWebService::new(db.clone(), secrets.clone()));
        let mesh_cfg =
            nexus_store::settings::get_or(&db, "mesh.config", nexus_mesh::MeshConfig::default());
        let db_mesh = db.clone();
        let on_mesh_save: nexus_mesh::MeshSaveHook = Arc::new(move |cfg| {
            if let Err(e) = nexus_store::settings::set(&db_mesh, "mesh.config", cfg) {
                tracing::warn!(error = ?e, "Failed to persist mesh config to sqlite");
            }
        });
        let mesh = Arc::new(nexus_mesh::MeshService::with_persistence(
            mesh_cfg,
            Some(on_mesh_save),
        ));
        Self::with_services(
            db,
            secrets,
            cursor,
            accounts,
            SubscriptionServices {
                chatgpt,
                grok,
                kiro,
                zcode,
                qoder,
                claude,
                antigravity,
                gemini_web,
                mesh,
            },
        )
    }

    /// 与应用其余部分共用同一份订阅号服务（账号页改了开关，网关这边立刻看得到）。
    pub fn with_services(
        db: Arc<Db>,
        secrets: Arc<dyn SecretStore>,
        cursor: Arc<nexus_cursor::Cursor>,
        accounts: Arc<AccountsService>,
        subs: SubscriptionServices,
    ) -> Self {
        let SubscriptionServices {
            chatgpt,
            grok,
            kiro,
            zcode,
            qoder,
            claude,
            antigravity,
            gemini_web,
            mesh,
        } = subs;
        let mesh_lane: Arc<dyn crate::lane::Lane> =
            Arc::new(crate::lane::StaticLane::new(crate::lane::Credential {
                label: "mesh-peer".to_string(),
                access_token: String::new(),
                identity: crate::identity::DeviceIdentity::derived("mesh-peer"),
            }));
        let settings = read_settings(&db);
        let roster = Arc::new(Roster::load(db.clone()));
        let (cursor_login, lane) = build_lane(cursor, accounts.clone(), roster.clone());
        // 顺序只影响状态快照怎么排；裸名不再按「谁拥有」选路。
        let channel_lanes: Vec<(ChannelId, Arc<RelayLane>)> = vec![
            (
                channel::CHATGPT,
                Arc::new(subscriptions::subscription_lane(
                    chatgpt.clone() as Arc<dyn SubscriptionAccounts>
                )),
            ),
            (
                channel::GROK,
                Arc::new(subscriptions::subscription_lane(
                    grok.clone() as Arc<dyn SubscriptionAccounts>
                )),
            ),
            (
                channel::KIRO,
                Arc::new(subscriptions::subscription_lane(
                    kiro.clone() as Arc<dyn SubscriptionAccounts>
                )),
            ),
            (
                channel::ZCODE,
                Arc::new(subscriptions::subscription_lane(
                    zcode.clone() as Arc<dyn SubscriptionAccounts>
                )),
            ),
            (
                channel::QODER,
                Arc::new(subscriptions::subscription_lane(
                    qoder.clone() as Arc<dyn SubscriptionAccounts>
                )),
            ),
            (
                channel::CLAUDE,
                Arc::new(subscriptions::subscription_lane(
                    claude.clone() as Arc<dyn SubscriptionAccounts>
                )),
            ),
            (
                channel::ANTIGRAVITY,
                Arc::new(subscriptions::subscription_lane(
                    antigravity.clone() as Arc<dyn SubscriptionAccounts>
                )),
            ),
            (
                channel::GEMINI_WEB,
                Arc::new(subscriptions::subscription_lane(
                    gemini_web.clone() as Arc<dyn SubscriptionAccounts>
                )),
            ),
        ];
        let ledger = Arc::new(Ledger::new(db.clone()));
        if let Err(err) = ledger.prune() {
            tracing::warn!(%err, "裁剪网关请求账本失败");
        }
        let media_jobs = Arc::new(MediaJobs::new(db.clone()));
        media_jobs.prune();
        let default_channel = Arc::new(RwLock::new(
            channel::parse_id(&settings.default_channel)
                .unwrap_or(channel::CURSOR)
                .to_string(),
        ));
        let provider_lane = Arc::new(ProviderLane::new(db.clone(), secrets.clone()));
        let routes = Arc::new(RwLock::new(read_routes(&db)));

        let channel_lanes_map: std::collections::HashMap<ChannelId, Arc<RelayLane>> =
            channel_lanes.iter().cloned().collect();

        // 注册 Mesh 真实调用执行钩子 (当外部对等节点通过隧道调用本机 Channel 时触发真实推理)
        let (claude_srv, chatgpt_srv, antigravity_srv, grok_srv, kiro_srv, zcode_srv, qoder_srv) = (
            claude.clone(),
            chatgpt.clone(),
            antigravity.clone(),
            grok.clone(),
            kiro.clone(),
            zcode.clone(),
            qoder.clone(),
        );

        let mesh_for_hook = mesh.clone();
        let mesh_invoke_hook: nexus_mesh::tunnel::client::MeshInvokeHook = Arc::new(
            move |channel_id, model, payload_json, outbound_tx, stream_id| {
                let claude = claude_srv.clone();
                let chatgpt = chatgpt_srv.clone();
                let antigravity = antigravity_srv.clone();
                let grok = grok_srv.clone();
                let kiro = kiro_srv.clone();
                let zcode = zcode_srv.clone();
                let qoder = qoder_srv.clone();
                let mesh_inst = mesh_for_hook.clone();
                let lanes_map = channel_lanes_map.clone();

                Box::pin(async move {
                    let run_impl = async {
                        // 1. 严格检查本地 Host 开放配置与模型共享白名单
                        mesh_inst
                            .is_channel_and_model_allowed(&channel_id, &model)
                            .await
                            .map_err(|e| format!("Mesh 外部调用鉴权拒绝: {e}"))?;

                        // 2. 清洗模型前缀 (去除 peer_id/ 或 channel_id/ 嵌套前缀)
                        let effective_model = if let Some((_, stripped)) = model.split_once('/') {
                            stripped.to_string()
                        } else {
                            model.clone()
                        };

                        let parsed: serde_json::Value = serde_json::from_str(&payload_json)
                            .map_err(|e| format!("解析请求体失败: {e}"))?;

                        // 优先识别方言并恢复完整的 ChatRequest (包含 messages、tools 等高级调用)
                        let is_anthropic_flavor = parsed.get("anthropic_version").is_some()
                            || parsed.pointer("/tools/0/input_schema").is_some()
                            || parsed.get("system").is_some();

                        let mut chat_req = if is_anthropic_flavor {
                            if let Ok(pr) = crate::inbound::parse::parse_request(
                                crate::inbound::Dialect::AnthropicMessages,
                                &parsed,
                                None,
                            ) {
                                pr.request
                            } else if let Ok(pr) = crate::inbound::parse::parse_request(
                                crate::inbound::Dialect::OpenAiChat,
                                &parsed,
                                None,
                            ) {
                                pr.request
                            } else {
                                Self::parse_mesh_fallback_request(&parsed, &effective_model)
                            }
                        } else if let Ok(pr) = crate::inbound::parse::parse_request(
                            crate::inbound::Dialect::OpenAiChat,
                            &parsed,
                            None,
                        ) {
                            pr.request
                        } else if let Ok(pr) = crate::inbound::parse::parse_request(
                            crate::inbound::Dialect::AnthropicMessages,
                            &parsed,
                            None,
                        ) {
                            pr.request
                        } else {
                            Self::parse_mesh_fallback_request(&parsed, &effective_model)
                        };
                        chat_req.model = effective_model.clone();

                        let lane: Arc<dyn Lane> = lanes_map
                            .get(channel_id.as_str())
                            .cloned()
                            .map(|l| l as Arc<dyn Lane>)
                            .ok_or_else(|| format!("本地 Provider 未配置该通道: {channel_id}"))?;

                        let upstream: Arc<dyn crate::upstream::Upstream> = match channel_id.as_str()
                        {
                            channel::CLAUDE => {
                                subscriptions::claude_channel(lane.clone(), claude).upstream
                            }
                            channel::CHATGPT => {
                                subscriptions::chatgpt_channel(lane.clone(), chatgpt).upstream
                            }
                            channel::ANTIGRAVITY => {
                                subscriptions::antigravity_channel(lane.clone(), antigravity)
                                    .upstream
                            }
                            channel::GROK => {
                                subscriptions::grok_channel(lane.clone(), grok).upstream
                            }
                            channel::KIRO => {
                                subscriptions::kiro_channel(lane.clone(), kiro).upstream
                            }
                            channel::ZCODE => {
                                subscriptions::zcode_channel(lane.clone(), zcode).upstream
                            }
                            channel::QODER => {
                                subscriptions::qoder_channel(lane.clone(), qoder).upstream
                            }
                            other => return Err(format!("本地 Provider 不支持该通道: {other}")),
                        };

                        let mut last_err: Option<String> = None;
                        let switch_on_upstream = lane.switch_helps_on_upstream_error();

                        for attempt in 1..=3 {
                            let cred = match lane.acquire(&effective_model).await {
                                Ok(c) => c,
                                Err(e) => {
                                    let err_msg = format!("获取本地通道凭据失败: {e}");
                                    return Err(last_err.unwrap_or(err_msg));
                                }
                            };

                            let tx = outbound_tx.clone();
                            let emitted = Arc::new(std::sync::atomic::AtomicBool::new(false));
                            let emitted_clone = emitted.clone();
                            let mut sink = move |d: crate::normalized::Delta| {
                                if !matches!(
                                    d,
                                    crate::normalized::Delta::Headers(_)
                                        | crate::normalized::Delta::ThinkingSignature(_)
                                ) {
                                    emitted_clone.store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                                match d {
                                    crate::normalized::Delta::Text(t) => {
                                        let _ =
                                            tx.try_send(nexus_mesh::protocol::TunnelFrame::Delta {
                                                stream_id,
                                                text: t,
                                                is_thinking: false,
                                            });
                                    }
                                    crate::normalized::Delta::Thinking(t) => {
                                        let _ =
                                            tx.try_send(nexus_mesh::protocol::TunnelFrame::Delta {
                                                stream_id,
                                                text: t,
                                                is_thinking: true,
                                            });
                                    }
                                    _ => {}
                                }
                            };

                            let result = upstream.stream(&cred, &chat_req, &mut sink).await;
                            match &result {
                                Ok(res) => {
                                    lane.report(
                                        &cred,
                                        &effective_model,
                                        crate::lane::Outcome::Ok(&res.usage),
                                    );
                                    let tool_calls_wire = if res.tool_calls.is_empty() {
                                        None
                                    } else {
                                        Some(
                                            res.tool_calls
                                                .iter()
                                                .map(|tc| nexus_mesh::protocol::ToolCallWire {
                                                    id: tc.id.clone(),
                                                    name: tc.name.clone(),
                                                    arguments: tc.arguments.clone(),
                                                })
                                                .collect(),
                                        )
                                    };
                                    let finish_reason_str = if !res.tool_calls.is_empty() {
                                        "tool_use".to_string()
                                    } else {
                                        res.finish_reason.as_str().to_string()
                                    };
                                    let _ = outbound_tx
                                        .send(nexus_mesh::protocol::TunnelFrame::Finish {
                                            stream_id,
                                            total_tokens: Some(res.usage.output_tokens),
                                            finish_reason: Some(finish_reason_str),
                                            tool_calls: tool_calls_wire,
                                            thinking_signature: res.thinking_signature.clone(),
                                        })
                                        .await;
                                    return Ok(());
                                }
                                Err(e) => {
                                    lane.report(
                                        &cred,
                                        &effective_model,
                                        crate::lane::Outcome::Err(e),
                                    );
                                    if !emitted.load(std::sync::atomic::Ordering::Relaxed)
                                        && attempt < 3
                                        && (e.kind.blames_account()
                                            || (switch_on_upstream
                                                && matches!(
                                                    e.kind,
                                                    crate::error::UpstreamKind::Upstream
                                                        | crate::error::UpstreamKind::Timeout
                                                )))
                                    {
                                        tracing::info!(
                                            account = %cred.label,
                                            model = %effective_model,
                                            status = e.status,
                                            attempt,
                                            "Mesh 本地回环调用的通道账号报错，换号重试 (第 {attempt} 次)"
                                        );
                                        last_err =
                                            Some(format!("本地通道上游执行错误: {}", e.message));
                                        continue;
                                    }
                                    return Err(format!("本地通道上游执行错误: {}", e.message));
                                }
                            }
                        }

                        Err(last_err.unwrap_or_else(|| "所有账号重试均失败".to_string()))
                    };

                    let res: std::result::Result<(), String> = run_impl.await;
                    if let Err(ref err_msg) = res {
                        tracing::error!(%err_msg, "Mesh 本地调用执行失败");
                        let _ = outbound_tx
                            .send(nexus_mesh::protocol::TunnelFrame::Error {
                                stream_id,
                                code: "500".to_string(),
                                message: err_msg.clone(),
                            })
                            .await;
                    }
                    res
                })
            },
        );

        mesh.set_invoke_hook(Some(mesh_invoke_hook));

        Self {
            db,
            secrets,
            chatgpt,
            grok,
            kiro,
            zcode,
            qoder,
            claude,
            antigravity,
            gemini_web,
            mesh: mesh.clone(),
            mesh_lane: mesh_lane.clone(),
            roster,
            cursor_login,
            lane: Arc::new(lane),
            channel_lanes,
            running: Mutex::new(None),
            ledger,
            media_jobs,
            default_channel,
            provider_lane,
            routes,
            log: Arc::new(RequestLog::new()),
        }
    }

    /// 网关的全部通道（默认 Cursor + 六条订阅通道 + 供应商 + Mesh 共享网络）。每次起服务时装一份；门禁现查，
    /// 加号 / 关号 / 拉到新目录 / 加一家供应商都不用重启。
    fn registry(&self, cursor_cfg: StreamConfig) -> ChannelRegistry {
        let mut reg = ChannelRegistry::new(channel::unavailable_cursor_channel(
            self.lane(),
            Arc::new(CursorUpstream::new(cursor_cfg)),
        ));
        for (id, lane) in &self.channel_lanes {
            let lane: Arc<dyn crate::lane::Lane> = lane.clone();
            let ch = match *id {
                channel::CHATGPT => subscriptions::chatgpt_channel(lane, self.chatgpt.clone()),
                channel::GROK => subscriptions::grok_channel(lane, self.grok.clone()),
                channel::KIRO => subscriptions::kiro_channel(lane, self.kiro.clone()),
                channel::ZCODE => subscriptions::zcode_channel(lane, self.zcode.clone()),
                channel::QODER => subscriptions::qoder_channel(lane, self.qoder.clone()),
                channel::CLAUDE => subscriptions::claude_channel(lane, self.claude.clone()),
                channel::ANTIGRAVITY => {
                    subscriptions::antigravity_channel(lane, self.antigravity.clone())
                }
                channel::GEMINI_WEB => {
                    subscriptions::gemini_web_channel(lane, self.gemini_web.clone())
                }
                other => unreachable!("未知通道 {other}"),
            };
            reg = reg.with(ch);
        }
        reg.with(provider::provider_channel(
            self.provider_lane.clone(),
            self.db.clone(),
        ))
        .with(crate::mesh::mesh_channel(
            self.mesh_lane.clone(),
            self.mesh.clone(),
        ))
        .share_default(self.default_channel.clone())
    }

    fn channel_lane(&self, id: &str) -> Result<&Arc<RelayLane>> {
        self.channel_lanes
            .iter()
            .find(|(cid, _)| *cid == id)
            .map(|(_, l)| l)
            .ok_or_else(|| AppError::invalid(format!("没有这条通道：{id}")))
    }

    /// 通道视角的快照：谁在队里、能不能接、报哪些模型。
    fn channel_snapshots(&self) -> Vec<ChannelSnapshot> {
        let reg = self.registry(StreamConfig::default());
        reg.extras()
            .map(|ch| ChannelSnapshot {
                id: ch.id,
                label: ch.label,
                vendor: ch.vendor,
                ready: ch.gate.ready(),
                media_ready: ch.gate.media_ready()
                    && (!ch.gate.models(Capability::Image).is_empty()
                        || !ch.gate.models(Capability::Video).is_empty()),
                lane: if ch.id == channel::PROVIDER {
                    self.provider_lane.snapshot()
                } else {
                    self.channel_lane(ch.id)
                        .map(|l| l.snapshot())
                        .unwrap_or_else(|_| LaneSnapshot {
                            current: None,
                            candidates: vec![],
                            missing: vec![],
                            available: vec![],
                        })
                },
                chat_models: ch
                    .gate
                    .models(Capability::Chat)
                    .into_iter()
                    .map(|id| channel::qualify(ch.id, &id))
                    .collect(),
                image_models: ch
                    .gate
                    .models(Capability::Image)
                    .into_iter()
                    .map(|id| channel::qualify(ch.id, &id))
                    .collect(),
                video_models: ch
                    .gate
                    .models(Capability::Video)
                    .into_iter()
                    .map(|id| channel::qualify(ch.id, &id))
                    .collect(),
                prefixes: ch.prefixes.to_vec(),
            })
            .collect()
    }

    /// 模型广场用的目录：每条通道各自报 `{通道}/{模型}`，同名不再互斥。
    /// 订阅通道只在有号可接时报——没号的通道写了前缀也会打到空队上，与其列出来骗人不如不列。
    pub fn catalog(&self) -> Vec<crate::models::CatalogEntry> {
        let reg = self.registry(StreamConfig::default());
        let chatgpt = reg.get(channel::CHATGPT).filter(|ch| ch.gate.ready());
        let mut out = match chatgpt {
            Some(ch) => crate::models::merged_catalog(Some(&ch.gate.models(Capability::Chat))),
            None => crate::models::merged_catalog(None),
        };
        // Cursor 的生图是另一条 AiService RPC，保留；只有已经被上游关掉的 Stream 对话目录隐藏。
        out.retain(|entry| {
            !(entry.modality == "chat" && entry.id.strip_prefix("cursor/").is_some())
        });
        for ch in reg.extras().filter(|ch| ch.id != channel::CHATGPT) {
            let vendor_label: &'static str = match ch.id {
                channel::GROK => "xAI",
                channel::KIRO => "Amazon",
                channel::ZCODE => "智谱",
                channel::QODER => "Qoder",
                channel::CLAUDE => "Anthropic",
                channel::ANTIGRAVITY => "Google",
                channel::GEMINI_WEB => "Google",
                channel::PROVIDER => "供应商",
                _ => ch.label,
            };
            let mut push =
                |id: String, modality: &'static str, media_note: Option<&'static str>| {
                    let qualified = if ch.id == channel::MESH {
                        id.clone()
                    } else {
                        channel::qualify(ch.id, &id)
                    };
                    let (series_id, variant) = if let Some(base) = id.strip_suffix("-thinking") {
                        (base.to_string(), "thinking".to_string())
                    } else {
                        (id.clone(), "standard".to_string())
                    };
                    let series = if ch.id == channel::MESH {
                        series_id
                    } else {
                        channel::qualify(ch.id, &series_id)
                    };
                    let note = media_note.or_else(|| Self::channel_model_note(ch.id, &id));
                    out.push(crate::models::CatalogEntry {
                        id: qualified.clone(),
                        vendor: ch.vendor,
                        vendor_label,
                        modality,
                        series,
                        variant,
                        aliases: Vec::new(),
                        note,
                        fixed_size: None,
                    });
                };
            if ch.gate.ready() {
                for id in ch.gate.models(Capability::Chat) {
                    push(id, "chat", None);
                }
            }
            if ch.gate.media_ready() {
                for id in ch.gate.models(Capability::Image) {
                    push(
                        id,
                        "image",
                        Some("xAI Imagine 生图 / 改图。免费档没有额度；付费档按订阅计。"),
                    );
                }
                for id in ch.gate.models(Capability::Video) {
                    push(
                        id,
                        "video",
                        Some("xAI Imagine 生视频（异步：提交后轮询 /v1/videos/{id}）。"),
                    );
                }
            }
        }
        out
    }

    /// 模型广场展示用：根据通道和具体模型生成精炼的说明
    fn channel_model_note(channel_id: &str, model_id: &str) -> Option<&'static str> {
        let raw = model_id.to_ascii_lowercase();
        match channel_id {
            channel::ANTIGRAVITY => {
                if raw.contains("gemini-3") || raw.contains("gemini-2.5") {
                    Some("Google 新一代多模态大模型，超长上下文与深度分析能力")
                } else if raw.contains("claude-3-7") || raw.contains("claude-3.7") {
                    Some("Anthropic 旗舰混合推理模型，支持思考与代码生成")
                } else if raw.contains("claude-3-5") || raw.contains("claude-3.5") {
                    Some("业界标杆级代码与通用分析模型")
                } else {
                    None
                }
            }
            channel::CLAUDE => {
                if raw.contains("opus") {
                    Some("Anthropic 旗舰深度推理模型，攻坚复杂架构与数学分析")
                } else if raw.contains("sonnet") {
                    Some("主力高智能通用与编码模型，速度与智力俱佳")
                } else if raw.contains("haiku") {
                    Some("轻量低延迟模型，快速响应日常任务")
                } else {
                    None
                }
            }
            channel::GROK => {
                if raw.contains("imagine") {
                    Some("xAI 图像创作模型")
                } else if raw.contains("grok-3") || raw.contains("grok-4") {
                    Some("xAI 旗舰前沿模型，知识库丰富且支持深度思考")
                } else {
                    None
                }
            }
            channel::KIRO => {
                if raw.contains("opus") {
                    Some("Amazon Q 驱动的 Claude Opus 深度推理模型，攻坚复杂架构与系统设计")
                } else if raw.contains("sonnet") {
                    Some("Amazon Q 驱动的 Claude Sonnet 主力编程与通用模型")
                } else if raw.contains("haiku") {
                    Some("Amazon Q 驱动的 Claude Haiku 极速轻量模型")
                } else if raw.contains("gpt") {
                    Some("Amazon Q 驱动的 GPT 系列模型")
                } else {
                    Some("Amazon Q 开发者云端编码与分析模型")
                }
            }
            channel::ZCODE => Some("智谱 GLM 编码特化模型，支持代码补全与技术分析"),
            channel::QODER => Some("通义千问 Qwen 编码特化模型"),
            channel::MESH => Some("来自 Nexus Mesh 远端节点共享的实时推理算力"),
            _ => None,
        }
    }

    /// 订阅通道：手动指定当前号（按账号标签）。供应商通道：把这一家排到它所有模型的最前面。
    pub fn channel_set_current(&self, channel: &str, label: &str) -> Result<GatewayStatus> {
        if channel == channel::PROVIDER {
            self.provider_lane.prefer(label);
        } else {
            self.channel_lane(channel)?.set_current(label);
        }
        self.status()
    }

    /// 订阅通道：清掉耗尽 / 冷却记录。
    pub fn channel_reset_lane(&self, channel: &str) -> Result<GatewayStatus> {
        if channel == channel::PROVIDER {
            self.provider_lane.reset();
        } else {
            self.channel_lane(channel)?.reset();
        }
        self.status()
    }

    /// 账号被删 / 关掉：它不再是当前号，关于它的记录也一起忘。
    pub fn channel_forget(&self, channel: &str, label: &str) {
        if channel == channel::PROVIDER {
            self.provider_lane.forget(label);
        } else if let Ok(lane) = self.channel_lane(channel) {
            lane.forget(label);
        }
    }

    /// 按客户端的路由。
    pub fn routes(&self) -> ClientRoutes {
        self.routes.read().expect("client routes").clone()
    }

    /// 改一个客户端的路由（`None` = 删掉，回到跟随默认通道）。落库，正在听的网关下一发就照新的走。
    ///
    /// `written` 是这次写进客户端配置的模型名：记进路由的旧名单，客户端没重开之前发来的
    /// 旧名字也跟着新路由走。旧名单本身跨改动保留。
    pub fn set_route(
        &self,
        client: &str,
        route: Option<ClientRoute>,
        written: Option<&str>,
    ) -> Result<ClientRoutes> {
        let Some(client) = routes::parse_client(client) else {
            return Err(AppError::invalid(format!("没有这个客户端：{client}")));
        };
        let mut next = self.routes();
        match route.map(ClientRoute::cleaned) {
            Some(r) if r.model.is_empty() => {
                return Err(AppError::invalid("先选一个模型。"));
            }
            Some(mut r) => {
                if let Some(old) = next.get(client) {
                    for a in &old.aliases {
                        if !r.aliases.iter().any(|x| x.eq_ignore_ascii_case(a)) {
                            r.aliases.push(a.clone());
                        }
                    }
                }
                if let Some(w) = written {
                    r.remember(w);
                }
                next.insert(client.to_string(), r.cleaned());
            }
            None => {
                next.remove(client);
            }
        }
        settings::set(&self.db, SETTING_CLIENT_ROUTES, &next)?;
        *self.routes.write().expect("client routes") = next.clone();
        Ok(next)
    }

    /// 最近请求的明细，新的在前。
    pub fn requests(&self, limit: usize) -> Vec<LogEntry> {
        self.log.recent(limit)
    }

    pub fn clear_requests(&self) {
        self.log.clear();
    }

    /// 最近的媒体任务。
    pub fn media_jobs(&self, limit: usize) -> Vec<MediaJob> {
        self.media_jobs.recent(limit)
    }

    /// 最近 `days` 天的本地用量。`tz_offset_min` 见 [`Ledger::summary`]。
    pub fn usage(&self, days: u32, tz_offset_min: i32) -> Result<UsageSummary> {
        self.ledger.summary(days, tz_offset_min)
    }

    /// 某条通道按账号的合计，给账号卡用。
    pub fn channel_account_totals(
        &self,
        channel: &str,
        days: u32,
    ) -> Result<Vec<crate::NamedUsage>> {
        self.ledger.channel_account_totals(channel, days)
    }

    /// 把号放进接力队。名单外的号网关一概不碰，所以这一步只能由用户点出来。
    pub fn enroll(&self, emails: &[String]) -> Result<GatewayStatus> {
        for e in emails {
            self.roster.add(e)?;
        }
        self.status()
    }

    /// 把号移出接力队；它若正是当前号，接力立刻转到下一个。
    pub fn unenroll(&self, email: &str) -> Result<GatewayStatus> {
        self.roster.remove(email)?;
        self.lane().forget(email);
        self.status()
    }

    /// 设置页改了 Cursor 目录之后立刻换读者。网关不用重启：下一请求读新库。
    pub fn retarget_cursor(&self, cursor: nexus_cursor::Cursor) {
        self.cursor_login.retarget(Arc::new(cursor));
    }

    pub fn settings(&self) -> GatewaySettings {
        read_settings(&self.db)
    }

    /// 改设置并落库。端口的改动要重启网关才生效，`status().restart_needed` 会说。
    pub fn update_settings(&self, patch: SettingsPatch) -> Result<GatewaySettings> {
        let mut s = self.settings();
        if let Some(p) = patch.port {
            if p < 1024 {
                return Err(AppError::invalid("端口要在 1024 以上。"));
            }
            s.port = p;
        }
        if let Some(a) = patch.autostart {
            s.autostart = a;
        }
        if let Some(fm) = patch.force_model {
            let trimmed = fm.trim().to_string();
            s.force_model = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            };
        }
        if let Some(dc) = patch.default_channel {
            let Some(id) = channel::parse_id(&dc) else {
                return Err(AppError::invalid(format!(
                    "默认通道只能是 {}，给的是 {dc}",
                    channel::KNOWN_IDS
                )));
            };
            s.default_channel = id.to_string();
            *self.default_channel.write().expect("default channel") = id.to_string();
            if let Ok(mut running) = self.running.lock() {
                if let Some(r) = running.as_mut() {
                    r.settings.default_channel = id.to_string();
                }
            }
        }
        settings::set(&self.db, SETTING_PORT, &s.port)?;
        settings::set(&self.db, SETTING_AUTOSTART, &s.autostart)?;
        settings::set(
            &self.db,
            SETTING_FORCE_MODEL,
            &s.force_model.clone().unwrap_or_default(),
        )?;
        settings::set(&self.db, SETTING_DEFAULT_CHANNEL, &s.default_channel)?;
        Ok(s)
    }

    fn lane(&self) -> Arc<RelayLane> {
        self.lane.clone()
    }

    /// 取本地口令；没有就生成一把存起来。
    pub fn api_key(&self) -> Result<String> {
        let key = SecretRef::from_raw(SECRET_API_KEY);
        if let Some(existing) = self.secrets.get(&key)? {
            return Ok(existing.expose().to_string());
        }
        let fresh = format!("nx-{}", uuid::Uuid::new_v4().simple());
        self.secrets.set(&key, &Secret::new(fresh.clone()))?;
        Ok(fresh)
    }

    /// 已有的口令；还没生成过就是 `None`（只读，不顺手生成）。
    pub fn api_key_if_set(&self) -> Result<Option<String>> {
        let key = SecretRef::from_raw(SECRET_API_KEY);
        Ok(self.secrets.get(&key)?.map(|s| s.expose().to_string()))
    }

    /// 客户端该连的端口：开着就是正在听的那个，关着就是设置里的。
    pub fn effective_port(&self) -> u16 {
        match self.running.lock().expect("running").as_ref() {
            Some(r) => r.addr.port(),
            None => self.settings().port,
        }
    }

    /// 换一把新口令。已配置旧口令的客户端会立刻 401，界面要提醒。
    pub fn rotate_api_key(&self) -> Result<String> {
        let key = SecretRef::from_raw(SECRET_API_KEY);
        let fresh = format!("nx-{}", uuid::Uuid::new_v4().simple());
        self.secrets.set(&key, &Secret::new(fresh.clone()))?;
        Ok(fresh)
    }

    pub fn is_running(&self) -> bool {
        self.running.lock().expect("running").is_some()
    }

    pub async fn start(&self) -> Result<GatewayStatus> {
        if self.is_running() {
            return self.status();
        }
        let mut settings = self.settings();
        let api_key = self.api_key()?;
        let cfg = StreamConfig {
            client_type: CLIENT_TYPE.into(),
            force_model: settings.force_model.clone(),
            ..StreamConfig::default()
        };
        let gw = Arc::new(Gateway {
            channels: self.registry(cfg),
            api_key: Some(api_key),
            ledger: Some(self.ledger.clone()),
            media_jobs: Some(self.media_jobs.clone()),
            routes: self.routes.clone(),
            log: Some(self.log.clone()),
            mesh: Some(self.mesh.clone()),
        });

        // 端口被占就往后找一个空的。找到的那个写回设置：下次开还是它，客户端里抄过的地址
        // 才不会失效。
        let (listener, bound) = bind_near(settings.port).await?;
        if bound.port() != settings.port && settings.port != 0 {
            tracing::warn!(
                from = settings.port,
                to = bound.port(),
                "配置的端口被占，已改用空闲端口并落库"
            );
            settings.port = bound.port();
            settings::set(&self.db, SETTING_PORT, &settings.port)?;
        }

        let (stop_tx, stop_rx) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            if let Err(err) = server::serve(listener, server::router(gw), async {
                let _ = stop_rx.await;
            })
            .await
            {
                tracing::error!(%err, "网关服务异常退出");
            }
        });

        tracing::info!(%bound, "网关已开");

        // 开网关时顺手拉一次各通道的模型目录与额度（有号才会真去拉）。失败无所谓——
        // 静态清单照旧能用，目录是为了新模型不用改代码就对客可见。
        if subscriptions::channel_ready(self.chatgpt.as_ref()) {
            let chatgpt = self.chatgpt.clone();
            tokio::spawn(async move {
                match chatgpt.refresh_models_any().await {
                    Ok(list) => tracing::info!(count = list.len(), "Codex 模型目录已更新"),
                    Err(err) => tracing::info!(%err, "拉 Codex 模型目录失败，沿用现有清单"),
                }
            });
        }
        if subscriptions::channel_ready(self.grok.as_ref()) {
            let grok = self.grok.clone();
            tokio::spawn(async move {
                match grok.refresh_models_any().await {
                    Ok(list) => tracing::info!(count = list.len(), "Grok 模型目录已更新"),
                    Err(err) => tracing::info!(%err, "拉 Grok 模型目录失败，沿用现有清单"),
                }
                if let Ok(list) = grok.list() {
                    for a in list.into_iter().filter(|a| a.usable()) {
                        grok.ensure_quota_fresh(&a.id).await;
                    }
                }
            });
        }
        *self.running.lock().expect("running") = Some(Running {
            addr: bound,
            started_at: SystemTime::now(),
            settings,
            stop: Some(stop_tx),
            task,
        });
        self.status()
    }

    pub async fn stop(&self) -> Result<GatewayStatus> {
        let running = self.running.lock().expect("running").take();
        if let Some(mut r) = running {
            if let Some(tx) = r.stop.take() {
                let _ = tx.send(());
            }
            // 优雅关停最多等两秒；在途的长流就让它断，用户点了关就是要它关。
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), &mut r.task).await;
            r.task.abort();
            tracing::info!(addr = %r.addr, "网关已关");
        }
        self.status()
    }

    pub fn set_current(&self, label: &str) -> Result<GatewayStatus> {
        self.lane().set_current(label);
        self.status()
    }

    pub fn reset_lane(&self) -> Result<GatewayStatus> {
        self.lane().reset();
        self.status()
    }

    pub fn status(&self) -> Result<GatewayStatus> {
        let settings = self.settings();
        let running = self.running.lock().expect("running");
        let (info, restart_needed) = match running.as_ref() {
            Some(r) => (
                Some(RunningInfo {
                    addr: r.addr.to_string(),
                    base_url: format!("http://{}", r.addr),
                    started_at: nexus_core::clock::iso_from_millis(
                        r.started_at
                            .duration_since(SystemTime::UNIX_EPOCH)
                            .map(|d| d.as_millis() as i64)
                            .unwrap_or(0),
                    )
                    .unwrap_or_default(),
                }),
                r.settings != settings,
            ),
            None => (None, false),
        };
        drop(running);
        let key_ref = SecretRef::from_raw(SECRET_API_KEY);
        let api_key_set = self.secrets.get(&key_ref)?.is_some();
        Ok(GatewayStatus {
            lane: self.lane().snapshot(),
            channels: self.channel_snapshots(),
            media_jobs: self.media_jobs.recent(20),
            routes: self.routes(),
            running: info,
            restart_needed,
            api_key_set,
            cursor_inference_unavailable_reason: self
                .registry(StreamConfig::default())
                .get(channel::CURSOR)
                .and_then(|cursor| cursor.gate.unavailable_reason()),
            settings,
        })
    }
}

/// 在 `preferred` 上监听；被占了就往后逐个试到 `preferred + PORT_PROBE_SPAN`。
/// `preferred == 0` 是「让系统挑」，不探。
async fn bind_near(preferred: u16) -> Result<(tokio::net::TcpListener, SocketAddr)> {
    let span = if preferred == 0 { 0 } else { PORT_PROBE_SPAN };
    let mut last_err: Option<std::io::Error> = None;
    for offset in 0..=span {
        let Some(port) = preferred.checked_add(offset) else {
            break;
        };
        let addr: SocketAddr = format!("127.0.0.1:{port}")
            .parse()
            .map_err(|e| AppError::internal(format!("地址解析失败：{e}")))?;
        match server::bind(addr).await {
            Ok(listener) => {
                let bound = listener
                    .local_addr()
                    .map_err(|e| AppError::internal(format!("读取监听地址失败：{e}")))?;
                return Ok((listener, bound));
            }
            Err(err) => last_err = Some(err),
        }
    }
    let detail = last_err.map(|e| format!("：{e}")).unwrap_or_default();
    Err(AppError::new(
        ErrorCode::InvalidInput,
        format!(
            "从 {preferred} 起的 {} 个端口都被占了{detail}",
            u32::from(span) + 1
        ),
    )
    .with_hint("关掉占着这些端口的程序，或在网关设置里换一个起始端口。"))
}

fn read_settings(db: &Db) -> GatewaySettings {
    let d = GatewaySettings::default();
    let force_raw: String = settings::get_or(db, SETTING_FORCE_MODEL, String::new());
    GatewaySettings {
        port: settings::get_or(db, SETTING_PORT, d.port),
        autostart: settings::get_or(db, SETTING_AUTOSTART, d.autostart),
        force_model: {
            let t = force_raw.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        },
        default_channel: channel::parse_id(&settings::get_or(
            db,
            SETTING_DEFAULT_CHANNEL,
            d.default_channel,
        ))
        .unwrap_or(channel::CURSOR)
        .to_string(),
    }
}

/// 读不出来（老库没有、JSON 坏了）就当没有路由：客户端作用域的请求回到默认通道，不至于起不来。
fn read_routes(db: &Db) -> ClientRoutes {
    let routes: ClientRoutes = settings::get_or(db, SETTING_CLIENT_ROUTES, ClientRoutes::new());
    routes
        .into_iter()
        .filter_map(|(client, route)| {
            let client = routes::parse_client(&client)?;
            let route = route.cleaned();
            (!route.model.is_empty()).then(|| (client.to_string(), route))
        })
        .collect()
}

fn build_lane(
    cursor: Arc<dyn LoginReader>,
    accounts: Arc<AccountsService>,
    roster: Arc<Roster>,
) -> (Arc<CursorLoginSource>, RelayLane) {
    // 顺序就是接力顺序：Cursor 正登着的号先（真机码），再托管号。
    let cursor_login = Arc::new(CursorLoginSource::new(cursor));
    let sources: Vec<Arc<dyn Source>> = vec![
        cursor_login.clone(),
        Arc::new(StoredAccountsSource::new(accounts, CLIENT_TYPE)),
    ];
    (cursor_login, RelayLane::new(sources, roster))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_cursor::Cursor;
    use nexus_store::MemorySecrets;

    struct Parts {
        db: Arc<Db>,
        secrets: Arc<dyn SecretStore>,
        cursor: Arc<Cursor>,
        accounts: Arc<AccountsService>,
        chatgpt: Arc<ChatGptService>,
    }

    fn parts() -> Parts {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::new());
        Parts {
            cursor: Arc::new(Cursor::at("/tmp/definitely-not-a-cursor-dir")),
            accounts: Arc::new(AccountsService::new(db.clone(), secrets.clone())),
            chatgpt: Arc::new(ChatGptService::new(db.clone(), secrets.clone())),
            db,
            secrets,
        }
    }

    fn build(p: &Parts) -> GatewayService {
        GatewayService::new(
            p.db.clone(),
            p.secrets.clone(),
            p.cursor.clone(),
            p.accounts.clone(),
            p.chatgpt.clone(),
        )
    }

    fn service() -> GatewayService {
        build(&parts())
    }

    #[test]
    fn settings_default_and_persist() {
        let svc = service();
        assert_eq!(svc.settings(), GatewaySettings::default());
        let s = svc
            .update_settings(SettingsPatch {
                port: Some(9000),
                autostart: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(s.port, 9000);
        assert!(s.autostart);
        assert_eq!(svc.settings(), s, "落库了");
    }

    #[test]
    fn force_model_persists_and_clears() {
        let svc = service();
        assert_eq!(svc.settings().force_model, None);
        let s = svc
            .update_settings(SettingsPatch {
                force_model: Some(" auto ".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(s.force_model.as_deref(), Some("auto"));
        assert_eq!(svc.settings().force_model.as_deref(), Some("auto"));
        let cleared = svc
            .update_settings(SettingsPatch {
                force_model: Some("".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(cleared.force_model, None);
    }

    #[test]
    fn default_channel_persists_and_rejects_unknown() {
        let svc = service();
        assert_eq!(svc.settings().default_channel, "cursor");
        let s = svc
            .update_settings(SettingsPatch {
                default_channel: Some(" ChatGPT ".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(s.default_channel, "chatgpt");
        assert_eq!(svc.settings().default_channel, "chatgpt");
        assert!(svc
            .update_settings(SettingsPatch {
                default_channel: Some("openai".into()),
                ..Default::default()
            })
            .is_err());
    }

    #[test]
    fn settings_reject_bad_values() {
        let svc = service();
        assert!(svc
            .update_settings(SettingsPatch {
                port: Some(80),
                ..Default::default()
            })
            .is_err());
    }

    /// 早先版本把 `gateway.client_type` / `gateway.passthrough_port` 落过库；升级上来这些键
    /// 还在，但网关不再读它们——设置回默认形状，不受旧值影响。
    #[test]
    fn legacy_client_type_and_passthrough_keys_are_ignored() {
        let svc = service();
        settings::set(&svc.db, "gateway.client_type", &"sand").unwrap();
        settings::set(&svc.db, "gateway.passthrough_port", &8788u16).unwrap();
        assert_eq!(svc.settings(), GatewaySettings::default());
    }

    #[test]
    fn api_key_is_generated_once_and_rotates_on_demand() {
        let svc = service();
        assert!(!svc.status().unwrap().api_key_set);
        let a = svc.api_key().unwrap();
        assert!(a.starts_with("nx-"));
        assert_eq!(svc.api_key().unwrap(), a, "第二次拿到同一把");
        assert!(svc.status().unwrap().api_key_set);
        let b = svc.rotate_api_key().unwrap();
        assert_ne!(a, b);
        assert_eq!(svc.api_key().unwrap(), b);
    }

    #[tokio::test]
    async fn start_stop_and_restart_needed() {
        let svc = service();
        svc.update_settings(SettingsPatch {
            port: Some(0), // 让系统挑端口
            ..Default::default()
        })
        .unwrap_err(); // 0 < 1024 被拒——改用直接写库绕过校验
        settings::set(&svc.db, SETTING_PORT, &0u16).unwrap();

        let st = svc.start().await.unwrap();
        let info = st.running.expect("在跑");
        assert!(info.base_url.starts_with("http://127.0.0.1:"));
        assert!(!st.restart_needed);
        assert!(st.api_key_set, "起服务会顺手生成口令");

        // 真的在监听：healthz 通，且没带口令的请求被 401。
        let base = info.base_url.clone();
        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            http.get(format!("{base}/healthz"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        let res = http
            .post(format!("{base}/v1/chat/completions"))
            .json(&serde_json::json!({ "model": "auto", "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401, "没口令就该被挡");

        // 改了端口 → 提示重启。
        svc.update_settings(SettingsPatch {
            port: Some(9100),
            ..Default::default()
        })
        .unwrap();
        assert!(svc.status().unwrap().restart_needed);

        // 再 start 是幂等的。
        let again = svc.start().await.unwrap();
        assert_eq!(again.running.unwrap().addr, info.addr);

        let stopped = svc.stop().await.unwrap();
        assert!(stopped.running.is_none());
        assert!(!stopped.restart_needed);
        assert!(
            http.get(format!("{base}/healthz")).send().await.is_err(),
            "关了就连不上"
        );
    }

    #[tokio::test]
    async fn a_busy_port_is_skipped_and_the_new_one_is_persisted() {
        // 先占住一个端口，让网关的首选端口撞上它。
        let squatter = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let busy = squatter.local_addr().unwrap().port();
        let svc = service();
        settings::set(&svc.db, SETTING_PORT, &busy).unwrap();

        let st = svc.start().await.unwrap();
        let info = st.running.clone().expect("在跑");
        let port: u16 = info.addr.rsplit(':').next().unwrap().parse().unwrap();
        assert_ne!(port, busy, "被占的端口该被跳过");
        assert!(port > busy && port <= busy + PORT_PROBE_SPAN);
        // 换到的端口落库成了新设置，而且不算「设置已改需重启」。
        assert_eq!(svc.settings().port, port);
        assert!(!st.restart_needed);
        svc.stop().await.unwrap();
        drop(squatter);
    }

    #[test]
    fn usage_is_empty_but_answerable_before_any_request() {
        let svc = service();
        let u = svc.usage(7, 480).unwrap();
        assert_eq!(u.days.len(), 7);
        assert_eq!(u.window.calls, 0);
        assert!(u.since.is_none());
    }

    #[test]
    fn status_without_cursor_or_accounts_is_still_answerable() {
        let svc = service();
        let st = svc.status().unwrap();
        assert!(st.running.is_none());
        assert!(st.lane.candidates.is_empty(), "没登录、没托管号");
        assert!(st.lane.available.is_empty());
        assert_eq!(st.settings.port, DEFAULT_PORT);
        // 供应商是和订阅号并列的一条通道，没有供应商时也在快照里（只是没号）。
        let provider = st
            .channels
            .iter()
            .find(|c| c.id == channel::PROVIDER)
            .expect("供应商通道在快照里");
        assert!(!provider.ready);
        assert_eq!(provider.prefixes, vec!["provider/"]);
    }

    #[test]
    fn provider_can_be_the_default_channel() {
        let svc = service();
        let s = svc
            .update_settings(SettingsPatch {
                default_channel: Some("provider".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(s.default_channel, "provider");
    }

    #[test]
    fn client_routes_persist_and_remember_written_names() {
        let p = parts();
        let svc = build(&p);
        assert!(svc.routes().is_empty());
        svc.set_route(
            "codex",
            Some(ClientRoute {
                model: " chatgpt/gpt-5.4 ".into(),
                ..ClientRoute::default()
            }),
            Some("gpt-5.4"),
        )
        .unwrap();
        svc.set_route(
            "codex",
            Some(ClientRoute {
                model: "cursor/claude-opus-5".into(),
                ..ClientRoute::default()
            }),
            Some("claude-opus-5"),
        )
        .unwrap();
        let again = build(&p);
        let r = &again.routes()["codex"];
        assert_eq!(r.model, "cursor/claude-opus-5");
        assert_eq!(r.aliases, vec!["claude-opus-5", "gpt-5.4"]);
        assert_eq!(
            again.status().unwrap().routes["codex"].model,
            "cursor/claude-opus-5"
        );

        assert!(svc
            .set_route(
                "unknown_client",
                Some(ClientRoute {
                    model: "cursor/a".into(),
                    ..ClientRoute::default()
                }),
                None
            )
            .is_err());
        svc.set_route(
            "cursor",
            Some(ClientRoute {
                model: "claude/claude-3-7-sonnet".into(),
                ..ClientRoute::default()
            }),
            None,
        )
        .unwrap();
        assert_eq!(svc.routes()["cursor"].model, "claude/claude-3-7-sonnet");
        assert!(svc
            .set_route("codex", Some(ClientRoute::default()), None)
            .is_err());
        svc.set_route("codex", None, None).unwrap();
        svc.set_route("cursor", None, None).unwrap();
        assert!(build(&p).routes().is_empty());
    }

    #[test]
    fn stored_accounts_stay_out_of_the_lane_until_enrolled() {
        use nexus_accounts::model::NewAccount;
        let p = parts();
        let svc = build(&p);
        p.accounts
            .repo
            .upsert(NewAccount {
                email: "A@x.com".into(),
                refresh_token: Some("rt".into()),
                ..Default::default()
            })
            .unwrap();

        let st = svc.status().unwrap();
        assert!(st.lane.candidates.is_empty(), "授权过也不自动进队");
        assert_eq!(st.lane.available.len(), 1);
        assert_eq!(st.lane.available[0].label, "a@x.com");

        let st = svc.enroll(&["A@x.com".to_string()]).unwrap();
        assert_eq!(st.lane.candidates.len(), 1);
        assert!(st.lane.available.is_empty());
        // 名单跨服务实例保留。
        let again = build(&p);
        assert_eq!(again.status().unwrap().lane.candidates.len(), 1);

        let st = svc.unenroll("a@x.com").unwrap();
        assert!(st.lane.candidates.is_empty());
        assert_eq!(st.lane.available.len(), 1);
    }
}
