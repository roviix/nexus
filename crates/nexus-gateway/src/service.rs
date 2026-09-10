//! 编排：起停 / 状态 / 设置 / 当前用哪个号。Tauri 层只跟它打交道。
//!
//! 网关**默认关闭**，由用户点开（ARCHITECTURE §3.4：可整体拆卸，坏了只影响网关）。开着的时候是一个
//! 常驻的 `127.0.0.1` 监听；关掉就是把监听收掉，所有在途请求随之结束。
//!
//! 本地口令（api key）是必填的：Cursor local mode 的配置表单本来就要求填一个，其他客户端
//! 也都有对应的环境变量；有它，本机别的进程就不能顺手用你的号。第一次起服务时随机生成，
//! 进 `SecretStore`，界面上显式点「显示」才给看——它不算上游凭证，但也没必要常驻在界面上。

use crate::channel::{self, Capability, ChannelId, ChannelRegistry};
use crate::grokbot::{GrokBotStreamAuth, SETTING_GROKBOT_STREAM};
use crate::inference::StreamConfig;
use crate::intercept::{InterceptHub, InterceptSnapshot, RewriteRule};
use crate::lane::{
    CursorLoginSource, LaneSnapshot, RelayLane, Roster, Source, StoredAccountsSource,
    SubscriptionAccounts,
};
use crate::ledger::{Ledger, UsageSummary, SOURCE_IDE_AGENT};
use crate::media::{MediaJob, MediaJobs};
use crate::passthrough::{self, PassthroughContext};
use crate::server::{self, Gateway};
use crate::subscriptions;
use crate::upstream::CursorUpstream;
use nexus_accounts::AccountsService;
use nexus_chatgpt::ChatGptService;
use nexus_core::{AppError, ErrorCode, Result, Secret};
use nexus_cursor::Cursor;
use nexus_grok::GrokService;
use nexus_kiro::KiroService;
use nexus_store::keys::SecretRef;
use nexus_store::{settings, Db, SecretStore};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;
use tokio::sync::oneshot;

pub const SETTING_PORT: &str = "gateway.port";
pub const SETTING_CLIENT_TYPE: &str = "gateway.client_type";
pub const SETTING_AUTOSTART: &str = "gateway.autostart";
pub const SETTING_FORCE_MODEL: &str = "gateway.force_model";
pub const SETTING_PASSTHROUGH_PORT: &str = "gateway.passthrough_port";
/// IDE Agent 面板拦截的改写规则（JSON）。不在 `GatewaySettings` 里：它热改即生效，不该触发
/// 「重启生效」的提示。
pub const SETTING_IDE_REWRITE: &str = "gateway.ide_rewrite";
const SECRET_API_KEY: &str = "gateway/api_key";
pub const DEFAULT_PORT: u16 = 8787;
/// 透传（模式⑤）另起一个端口，不跟翻译模式共用：BiDi 要求入站讲纯 h2c，翻译模式的 axum
/// 只讲 HTTP/1.1，两套接受循环没法叠在同一个监听上（见 `passthrough` 模块文档）。
/// `cursor-agent` 的 `-e` 和 `--agent-endpoint` 正好各接受一个 URL，天然填得下两个端口。
pub const DEFAULT_PASSTHROUGH_PORT: u16 = 8788;
/// 端口被占时往后最多试这么多个。端口不是用户该操心的事：占了就自动换一个，换到的那个
/// **落库**成新设置——地址稳定下来，客户端里抄过的配置才不会每次开机都失效。
const PORT_PROBE_SPAN: u16 = 40;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySettings {
    pub port: u16,
    /// 透传模式（cursor-agent -e / --agent-endpoint）的端口，和 `port` 分开监听。
    pub passthrough_port: u16,
    /// 额度通道标签。`cli` 缺省；`sand` 是显式的高风险选项，界面上单独说明。
    pub client_type: String,
    /// 应用启动时自动开网关。
    pub autostart: bool,
    /// 强制所有请求发这个上游模型（空 = 不强制）。
    ///
    /// 给「账号只开得了某一档」的情形：便宜号常常只能跑 `auto`，而 Claude Code 会
    /// 客户端校验模型名、不认识 `auto`。让客户端继续报它认识的名字、由网关换成账号
    /// 真能跑的那个。改写用户要的模型是件该由他自己点头的事，所以默认关。
    pub force_model: Option<String>,
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            passthrough_port: DEFAULT_PASSTHROUGH_PORT,
            client_type: crate::inference::DEFAULT_CLIENT_TYPE.into(),
            autostart: false,
            force_model: None,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub port: Option<u16>,
    pub passthrough_port: Option<u16>,
    pub client_type: Option<String>,
    pub autostart: Option<bool>,
    /// `Some("")` / `Some("  ")` = 清空；`None` = 不动。
    pub force_model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningInfo {
    pub addr: String,
    pub base_url: String,
    /// 透传监听地址；这条服务和翻译模式一起起停，地址不会缺席。
    pub passthrough_addr: String,
    pub passthrough_base_url: String,
    pub started_at: String,
}

/// 一个接入方式：给用户照着填。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entrance {
    pub id: &'static str,
    pub title: &'static str,
    pub how: String,
    /// 环境变量形态（能复制粘贴进 shell 的那种），`<KEY>` 占位待用户替换成口令。
    pub env: Vec<String>,
    pub available: bool,
    pub note: Option<&'static str>,
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
    /// Cursor 通道（默认通道）的接力队。
    pub lane: LaneSnapshot,
    /// 订阅通道（ChatGPT / Grok Build / Kiro …），按选路顺序。
    pub channels: Vec<ChannelSnapshot>,
    /// 最近的异步媒体任务（生视频），新的在前。
    pub media_jobs: Vec<MediaJob>,
    pub entrances: Vec<Entrance>,
    /// IDE Agent 面板拦截：当前改写规则 + 本进程内最近的请求。透传口一直带着它，
    /// 有没有流量取决于 Sand 那边是否把推理改道到了本机。
    pub intercept: InterceptSnapshot,
    /// 透传口的 Grok Bot 额度开关（只对 `InferenceService/Stream`）及本地直连凭证状态。
    pub grokbot_stream: crate::grokbot::GrokBotStreamSnapshot,
}

struct Running {
    addr: SocketAddr,
    passthrough_addr: SocketAddr,
    started_at: SystemTime,
    /// 起服务时用的设置；和当前设置比对得出 `restart_needed`。
    settings: GatewaySettings,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    passthrough_stop: Option<oneshot::Sender<()>>,
    passthrough_task: tokio::task::JoinHandle<()>,
}

pub struct GatewayService {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
    cursor: Arc<Cursor>,
    accounts: Arc<AccountsService>,
    /// 订阅号服务。它们的号只有网关这一个用途，进不进队看账号自己的开关，不走 `roster`。
    chatgpt: Arc<ChatGptService>,
    grok: Arc<GrokService>,
    kiro: Arc<KiroService>,
    /// 哪些号进接力队。跨 lane 重建、跨启停都是同一份，落库。
    roster: Arc<Roster>,
    /// 接力状态跨启停保留（当前号、冷却），只在 client-type 变了才重建。
    lane: RwLock<Arc<RelayLane>>,
    /// 每条订阅通道自己的接力队，和 Cursor 的互不混。跨启停保留。顺序就是选路顺序。
    channel_lanes: Vec<(ChannelId, Arc<RelayLane>)>,
    lane_client_type: Mutex<String>,
    running: Mutex<Option<Running>>,
    /// 请求账本。跨启停同一份——它记的是历史，不是运行态。
    ledger: Arc<Ledger>,
    /// 异步媒体任务登记簿。
    media_jobs: Arc<MediaJobs>,
    /// IDE 拦截的规则与最近记录。跨启停同一份：规则落库、记录留在内存。
    intercept: Arc<InterceptHub>,
    /// 透传口的 Grok Bot 额度开关。热开关，落库；凭证由 `nexus-grokbot` 维护。
    grokbot: Arc<GrokBotStreamAuth>,
}

impl GatewayService {
    pub fn new(
        db: Arc<Db>,
        secrets: Arc<dyn SecretStore>,
        cursor: Arc<Cursor>,
        accounts: Arc<AccountsService>,
        chatgpt: Arc<ChatGptService>,
    ) -> Self {
        // 没人递 GrokBotService 进来（测试 / examples）：给一个离线的，绝不去碰钥匙串。
        let grokbot = Arc::new(nexus_grokbot::GrokBotService::offline(
            &std::env::temp_dir().join("nexus-gateway-grokbot"),
        ));
        let grok = Arc::new(GrokService::new(db.clone(), secrets.clone()));
        let kiro = Arc::new(KiroService::new(db.clone(), secrets.clone()));
        Self::with_grokbot(db, secrets, cursor, accounts, chatgpt, grok, kiro, grokbot)
    }

    /// 与 Sand 共用同一个 `GrokBotService`（同一份凭证文件、同一份钥匙串口令缓存）。
    #[allow(clippy::too_many_arguments)]
    pub fn with_grokbot(
        db: Arc<Db>,
        secrets: Arc<dyn SecretStore>,
        cursor: Arc<Cursor>,
        accounts: Arc<AccountsService>,
        chatgpt: Arc<ChatGptService>,
        grok: Arc<GrokService>,
        kiro: Arc<KiroService>,
        grokbot: Arc<nexus_grokbot::GrokBotService>,
    ) -> Self {
        let settings = read_settings(&db);
        let grokbot_on: bool = settings::get_or(&db, SETTING_GROKBOT_STREAM, false);
        let grokbot = Arc::new(GrokBotStreamAuth::new(grokbot, grokbot_on));
        let roster = Arc::new(Roster::load(db.clone()));
        let lane = build_lane(
            cursor.clone(),
            accounts.clone(),
            roster.clone(),
            &settings.client_type,
        );
        // 顺序就是选路顺序（前缀之外的裸模型名按这个顺序问「归不归你」）。名字不重叠，
        // 所以顺序此刻不影响结果；写死是为了状态快照稳定。
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
        ];
        let ledger = Arc::new(Ledger::new(db.clone()));
        if let Err(err) = ledger.prune() {
            tracing::warn!(%err, "裁剪网关请求账本失败");
        }
        let media_jobs = Arc::new(MediaJobs::new(db.clone()));
        media_jobs.prune();
        let rule: RewriteRule = settings::get_or(&db, SETTING_IDE_REWRITE, RewriteRule::default());
        Self {
            db,
            secrets,
            cursor,
            accounts,
            chatgpt,
            grok,
            kiro,
            roster,
            lane: RwLock::new(Arc::new(lane)),
            channel_lanes,
            lane_client_type: Mutex::new(settings.client_type),
            running: Mutex::new(None),
            ledger,
            media_jobs,
            intercept: Arc::new(InterceptHub::new(rule)),
            grokbot,
        }
    }

    /// 网关的全部通道（默认 Cursor + 三条订阅通道）。每次起服务时装一份；门禁现查，加号 / 关号 /
    /// 拉到新目录都不用重启。
    fn registry(&self, cursor_cfg: StreamConfig) -> ChannelRegistry {
        let mut reg = ChannelRegistry::new(channel::cursor_channel(
            self.lane(),
            Arc::new(CursorUpstream::new(cursor_cfg)),
        ));
        for (id, lane) in &self.channel_lanes {
            let lane: Arc<dyn crate::lane::Lane> = lane.clone();
            let ch = match *id {
                channel::CHATGPT => subscriptions::chatgpt_channel(lane, self.chatgpt.clone()),
                channel::GROK => subscriptions::grok_channel(lane, self.grok.clone()),
                channel::KIRO => subscriptions::kiro_channel(lane, self.kiro.clone()),
                other => unreachable!("未知通道 {other}"),
            };
            reg = reg.with(ch);
        }
        reg
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
                lane: self
                    .channel_lane(ch.id)
                    .map(|l| l.snapshot())
                    .unwrap_or_else(|_| LaneSnapshot {
                        current: None,
                        candidates: vec![],
                        missing: vec![],
                        available: vec![],
                    }),
                chat_models: ch.gate.models(Capability::Chat),
                image_models: ch.gate.models(Capability::Image),
                video_models: ch.gate.models(Capability::Video),
                prefixes: ch.prefixes.to_vec(),
            })
            .collect()
    }

    /// 透传口的 Grok Bot 额度开关：落库 + 立刻生效（每一发 Stream 现读，不用重启）。
    /// 开的时候顺手确认凭证在，不在就当场去 Grok Bot 生成——省得用户开完发现 Agent 面板 502。
    pub async fn set_grokbot_stream(&self, on: bool) -> Result<GatewayStatus> {
        if on {
            let svc = self.grokbot.service();
            let usable = svc
                .stream_credential()?
                .map(|c| !c.is_expired(nexus_grokbot::credential::now_ms()) || c.can_renew())
                .unwrap_or(false);
            if !usable {
                svc.mint_direct().await?;
            }
        }
        settings::set(&self.db, SETTING_GROKBOT_STREAM, &on)?;
        self.grokbot.set_enabled(on);
        self.status()
    }

    pub fn grokbot(&self) -> &Arc<GrokBotStreamAuth> {
        &self.grokbot
    }

    /// 模型广场用的目录：Cursor 的静态表，加上每条**有号可接**的订阅通道报的模型。条件和网关的
    /// 选路同一条：这里列出来的名字，此刻发过去就会走那条通道。
    pub fn catalog(&self) -> Vec<crate::models::CatalogEntry> {
        let reg = self.registry(StreamConfig::default());
        let chatgpt = reg.get(channel::CHATGPT).filter(|ch| ch.gate.ready());
        let mut out = match chatgpt {
            Some(ch) => crate::models::merged_catalog(Some(&ch.gate.models(Capability::Chat))),
            None => crate::models::merged_catalog(None),
        };
        for ch in reg.extras().filter(|ch| ch.id != channel::CHATGPT) {
            let (vendor_label, note): (&'static str, Option<&'static str>) = match ch.id {
                channel::GROK => (
                    "xAI",
                    Some("经 Grok 通道（订阅号走 cli-chat-proxy，API Key 走 api.x.ai）。加 grok/ 前缀可强制走它。"),
                ),
                channel::KIRO => (
                    "Amazon",
                    Some("经 Kiro（Amazon Q / Builder ID）。对外 kiro-claude-*，不抢 Cursor 的 claude。"),
                ),
                _ => (ch.label, None),
            };
            let mut push =
                |id: String, modality: &'static str, media_note: Option<&'static str>| {
                    out.retain(|e| e.id != id);
                    out.push(crate::models::CatalogEntry {
                        id: id.clone(),
                        vendor: ch.vendor,
                        vendor_label,
                        modality,
                        series: id,
                        variant: "standard".to_string(),
                        aliases: Vec::new(),
                        note: media_note.or(note),
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

    /// 订阅通道：手动指定当前号（按账号标签）。
    pub fn channel_set_current(&self, channel: &str, label: &str) -> Result<GatewayStatus> {
        self.channel_lane(channel)?.set_current(label);
        self.status()
    }

    /// 订阅通道：清掉耗尽 / 冷却记录。
    pub fn channel_reset_lane(&self, channel: &str) -> Result<GatewayStatus> {
        self.channel_lane(channel)?.reset();
        self.status()
    }

    /// 账号被删 / 关掉：它不再是当前号，关于它的记录也一起忘。
    pub fn channel_forget(&self, channel: &str, label: &str) {
        if let Ok(lane) = self.channel_lane(channel) {
            lane.forget(label);
        }
    }

    /// 最近的媒体任务。
    pub fn media_jobs(&self, limit: usize) -> Vec<MediaJob> {
        self.media_jobs.recent(limit)
    }

    /// 最近 `days` 天的本地用量。`tz_offset_min` 见 [`Ledger::summary`]。
    pub fn usage(&self, days: u32, tz_offset_min: i32) -> Result<UsageSummary> {
        self.ledger.summary(days, tz_offset_min)
    }

    /// IDE Agent 面板经本机网关的用量，与方言口的账分开看（口径不同，见 `ledger` 模块文档）。
    pub fn ide_usage(&self, days: u32, tz_offset_min: i32) -> Result<UsageSummary> {
        self.ledger
            .summary_source(days, tz_offset_min, SOURCE_IDE_AGENT)
    }

    /// 改 IDE 拦截的改写规则：落库 + 立刻生效（透传口每一发都现读，不用重启网关）。
    pub fn set_intercept_rule(&self, rule: RewriteRule) -> Result<GatewayStatus> {
        rule.validate().map_err(AppError::invalid)?;
        settings::set(&self.db, SETTING_IDE_REWRITE, &rule)?;
        self.intercept.set_rule(rule);
        self.status()
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

    pub fn settings(&self) -> GatewaySettings {
        read_settings(&self.db)
    }

    /// 改设置并落库。端口 / client-type 的改动要重启网关才生效，`status().restart_needed` 会说。
    pub fn update_settings(&self, patch: SettingsPatch) -> Result<GatewaySettings> {
        let mut s = self.settings();
        if let Some(p) = patch.port {
            if p < 1024 {
                return Err(AppError::invalid("端口要在 1024 以上。"));
            }
            s.port = p;
        }
        if let Some(p) = patch.passthrough_port {
            if p < 1024 {
                return Err(AppError::invalid("透传端口要在 1024 以上。"));
            }
            s.passthrough_port = p;
        }
        // 只在这次真的动了某个端口时才查重——不然测试里用 0（让系统挑）时，
        // 后续一次跟端口无关的 patch（比如只改 client-type）也会被这条挡住。
        if (patch.port.is_some() || patch.passthrough_port.is_some())
            && s.port == s.passthrough_port
        {
            return Err(AppError::invalid("翻译端口和透传端口不能相同。"));
        }
        if let Some(ct) = patch.client_type {
            let ct = ct.trim().to_lowercase();
            if !matches!(ct.as_str(), "cli" | "ide" | "sand") {
                return Err(AppError::invalid("client-type 只能是 cli / ide / sand。"));
            }
            s.client_type = ct;
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
        settings::set(&self.db, SETTING_PORT, &s.port)?;
        settings::set(&self.db, SETTING_PASSTHROUGH_PORT, &s.passthrough_port)?;
        settings::set(&self.db, SETTING_CLIENT_TYPE, &s.client_type)?;
        settings::set(&self.db, SETTING_AUTOSTART, &s.autostart)?;
        settings::set(
            &self.db,
            SETTING_FORCE_MODEL,
            &s.force_model.clone().unwrap_or_default(),
        )?;
        self.ensure_lane_for(&s.client_type);
        Ok(s)
    }

    fn ensure_lane_for(&self, client_type: &str) {
        let mut cur = self.lane_client_type.lock().expect("lane client type");
        if *cur != client_type {
            *self.lane.write().expect("lane") = Arc::new(build_lane(
                self.cursor.clone(),
                self.accounts.clone(),
                self.roster.clone(),
                client_type,
            ));
            *cur = client_type.to_string();
        }
    }

    fn lane(&self) -> Arc<RelayLane> {
        self.lane.read().expect("lane").clone()
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
        self.ensure_lane_for(&settings.client_type);
        let api_key = self.api_key()?;
        let cfg = StreamConfig {
            client_type: settings.client_type.clone(),
            force_model: settings.force_model.clone(),
            ..StreamConfig::default()
        };
        let gw = Arc::new(Gateway {
            channels: self.registry(cfg),
            api_key: Some(api_key),
            ledger: Some(self.ledger.clone()),
            media_jobs: Some(self.media_jobs.clone()),
        });

        // 端口被占就往后找一个空的。找到的那个写回设置：下次开还是它，客户端里抄过的地址
        // 才不会失效。两个监听独立绑定，任何一个失败都不留下另一个孤儿监听。
        let (listener, bound) = bind_near(settings.port, None).await?;
        let (passthrough_listener, passthrough_bound) =
            bind_near(settings.passthrough_port, Some(bound.port())).await?;
        let moved = bound.port() != settings.port && settings.port != 0
            || passthrough_bound.port() != settings.passthrough_port
                && settings.passthrough_port != 0;
        if moved {
            tracing::warn!(
                from = settings.port,
                to = bound.port(),
                pt_from = settings.passthrough_port,
                pt_to = passthrough_bound.port(),
                "配置的端口被占，已改用空闲端口并落库"
            );
            settings.port = bound.port();
            settings.passthrough_port = passthrough_bound.port();
            settings::set(&self.db, SETTING_PORT, &settings.port)?;
            settings::set(
                &self.db,
                SETTING_PASSTHROUGH_PORT,
                &settings.passthrough_port,
            )?;
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

        let passthrough_ctx = Arc::new(
            PassthroughContext::new(self.lane(), settings.client_type.clone())
                .with_intercept(self.intercept.clone(), Some(self.ledger.clone()))
                .with_grokbot(self.grokbot.clone()),
        );
        let (pt_stop_tx, pt_stop_rx) = oneshot::channel::<()>();
        let passthrough_task = tokio::spawn(async move {
            if let Err(err) = passthrough::serve(passthrough_listener, passthrough_ctx, async {
                let _ = pt_stop_rx.await;
            })
            .await
            {
                tracing::error!(%err, "透传服务异常退出");
            }
        });

        tracing::info!(%bound, %passthrough_bound, client_type = %settings.client_type, "网关已开");

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
            passthrough_addr: passthrough_bound,
            started_at: SystemTime::now(),
            settings,
            stop: Some(stop_tx),
            task,
            passthrough_stop: Some(pt_stop_tx),
            passthrough_task,
        });
        self.status()
    }

    pub async fn stop(&self) -> Result<GatewayStatus> {
        let running = self.running.lock().expect("running").take();
        if let Some(mut r) = running {
            if let Some(tx) = r.stop.take() {
                let _ = tx.send(());
            }
            if let Some(tx) = r.passthrough_stop.take() {
                let _ = tx.send(());
            }
            // 优雅关停最多等两秒；在途的长流就让它断，用户点了关就是要它关。
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), &mut r.task).await;
            r.task.abort();
            // abort 传导 JoinSet drop（见 `passthrough::serve` 文档），在途的透传连接跟着断。
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(2), &mut r.passthrough_task)
                    .await;
            r.passthrough_task.abort();
            tracing::info!(addr = %r.addr, passthrough_addr = %r.passthrough_addr, "网关已关");
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
                    passthrough_addr: r.passthrough_addr.to_string(),
                    passthrough_base_url: format!("http://{}", r.passthrough_addr),
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
        let base_url = info
            .as_ref()
            .map(|i| i.base_url.clone())
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", settings.port));
        let passthrough_base_url = info
            .as_ref()
            .map(|i| i.passthrough_base_url.clone())
            .unwrap_or_else(|| format!("http://127.0.0.1:{}", settings.passthrough_port));
        let key_ref = SecretRef::from_raw(SECRET_API_KEY);
        let api_key_set = self.secrets.get(&key_ref)?.is_some();
        Ok(GatewayStatus {
            entrances: entrances(&base_url, &passthrough_base_url),
            lane: self.lane().snapshot(),
            channels: self.channel_snapshots(),
            media_jobs: self.media_jobs.recent(20),
            running: info,
            restart_needed,
            api_key_set,
            settings,
            intercept: self.intercept.snapshot(),
            grokbot_stream: self.grokbot.snapshot(),
        })
    }
}

/// 在 `preferred` 上监听；被占了就往后逐个试到 `preferred + PORT_PROBE_SPAN`，跳过 `avoid`
/// （另一个口已经拿走的那个）。`preferred == 0` 是「让系统挑」，不探。
async fn bind_near(
    preferred: u16,
    avoid: Option<u16>,
) -> Result<(tokio::net::TcpListener, SocketAddr)> {
    let span = if preferred == 0 { 0 } else { PORT_PROBE_SPAN };
    let mut last_err: Option<std::io::Error> = None;
    for offset in 0..=span {
        let Some(port) = preferred.checked_add(offset) else {
            break;
        };
        if Some(port) == avoid {
            continue;
        }
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
        passthrough_port: settings::get_or(db, SETTING_PASSTHROUGH_PORT, d.passthrough_port),
        client_type: settings::get_or(db, SETTING_CLIENT_TYPE, d.client_type),
        autostart: settings::get_or(db, SETTING_AUTOSTART, d.autostart),
        force_model: {
            let t = force_raw.trim().to_string();
            if t.is_empty() {
                None
            } else {
                Some(t)
            }
        },
    }
}

fn build_lane(
    cursor: Arc<Cursor>,
    accounts: Arc<AccountsService>,
    roster: Arc<Roster>,
    client_type: &str,
) -> RelayLane {
    // 顺序就是接力顺序：Cursor 正登着的号先（真机码），再托管号。
    let sources: Vec<Arc<dyn Source>> = vec![
        Arc::new(CursorLoginSource::new(cursor)),
        Arc::new(StoredAccountsSource::new(accounts, client_type)),
    ];
    RelayLane::new(sources, roster)
}

fn entrances(base_url: &str, passthrough_base_url: &str) -> Vec<Entrance> {
    vec![
        Entrance {
            id: "cursor_local_mode",
            title: "Cursor IDE · Local Mode",
            how: format!(
                "Cursor 命令面板 → Configure Local Agent：Base URL 填 {base_url}，API Key 填网关口令；\
                 或在启动 Cursor 的环境里设下面两个变量。IDE 对网关讲 Anthropic Messages 协议。"
            ),
            env: vec![
                format!("CURSOR_LOCAL_AGENT_BASE_URL={base_url}"),
                "CURSOR_LOCAL_AGENT_API_KEY=<KEY>".into(),
            ],
            // 网关这一侧已就绪；卡在 IDE 那一侧：零售版 Cursor 的 buildFlags 里 `localMode` 编译成
            // 了 false，Configure Local Agent 命令一进来就返回。要用得先把那个布尔翻成 true
            // （一处锚点，可交给 Sand 通道那套补丁引擎），或者等透传模式。
            available: false,
            note: Some(
                "已否决：零售版 buildFlags.localMode=false，且 localMode 会硬关 Tab 补全（isAllowedCpp 直接返回 false）。IDE 继续用切号。",
            ),
        },
        Entrance {
            id: "claude_code",
            title: "Claude Code / Anthropic SDK",
            how: format!("把 Anthropic 的 Base URL 指到 {base_url}，token 填网关口令。"),
            env: vec![
                format!("ANTHROPIC_BASE_URL={base_url}"),
                "ANTHROPIC_AUTH_TOKEN=<KEY>".into(),
            ],
            available: true,
            note: None,
        },
        Entrance {
            id: "openai_sdk",
            title: "OpenAI SDK / Codex / 其他兼容客户端",
            how: format!("Base URL 填 {base_url}/v1，API Key 填网关口令。"),
            env: vec![
                format!("OPENAI_BASE_URL={base_url}/v1"),
                "OPENAI_API_KEY=<KEY>".into(),
            ],
            available: true,
            note: Some("Chat Completions 与 Responses 方言（Codex 主用）都已支持。"),
        },
        Entrance {
            id: "cursor_agent_cli",
            title: "cursor-agent CLI",
            how: format!(
                "透传模式（模式⑤），端口和上面的翻译端口是分开的：\
                 cursor-agent -e {passthrough_base_url} --agent-endpoint {passthrough_base_url}。\
                 两个都要设——探针确认过 -e / CURSOR_API_ENDPOINT 只覆盖 aiserver（auth / \
                 Dashboard / AiService…），agentic 主循环单独硬拨 api5，只有命令行的 \
                 --agent-endpoint 能把它也改过来（这是隐藏选项，没找到对应的环境变量，\
                 只能写进启动命令或包装脚本）。网关这一侧对两者都讲原生 Cursor 协议\
                 （h2c 透传，不翻译方言）。CLI 本身还要先登录：`cursor-agent login`，\
                 或设 CURSOR_API_KEY / CURSOR_AUTH_TOKEN（可用本机 Cursor IDE 当前登录的 \
                 access token 顶一次）。"
            ),
            env: vec![format!("CURSOR_API_ENDPOINT={passthrough_base_url}")],
            available: true,
            note: Some(
                "透传：不解 body，只换身份头，原样转发 protobuf 帧；同一条连接（含 agent.v1.AgentService/Run 这类 BiDi）全程用同一个号，连接断了才换。真机已通（aiserver + agent.v1 Run）。",
            ),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_store::MemorySecrets;

    fn service() -> GatewayService {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let secrets: Arc<dyn SecretStore> = Arc::new(MemorySecrets::new());
        let cursor = Arc::new(Cursor::at("/tmp/definitely-not-a-cursor-dir"));
        let accounts = Arc::new(AccountsService::new(db.clone(), secrets.clone()));
        let chatgpt = Arc::new(ChatGptService::new(db.clone(), secrets.clone()));
        GatewayService::new(db, secrets, cursor, accounts, chatgpt)
    }

    #[test]
    fn settings_default_and_persist() {
        let svc = service();
        assert_eq!(svc.settings(), GatewaySettings::default());
        let s = svc
            .update_settings(SettingsPatch {
                port: Some(9000),
                client_type: Some("IDE".into()),
                autostart: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(s.port, 9000);
        assert_eq!(s.client_type, "ide", "统一小写");
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
    fn settings_reject_bad_values() {
        let svc = service();
        assert!(svc
            .update_settings(SettingsPatch {
                port: Some(80),
                ..Default::default()
            })
            .is_err());
        assert!(svc
            .update_settings(SettingsPatch {
                client_type: Some("bot".into()),
                ..Default::default()
            })
            .is_err());
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
        settings::set(&svc.db, SETTING_PASSTHROUGH_PORT, &0u16).unwrap();

        let st = svc.start().await.unwrap();
        let info = st.running.expect("在跑");
        assert!(info.base_url.starts_with("http://127.0.0.1:"));
        assert!(info.passthrough_base_url.starts_with("http://127.0.0.1:"));
        assert_ne!(info.base_url, info.passthrough_base_url, "两个端口不该撞");
        assert!(!st.restart_needed);
        assert!(st.api_key_set, "起服务会顺手生成口令");
        assert_eq!(st.entrances.len(), 4);
        let has = |id: &str, available: bool| {
            st.entrances
                .iter()
                .any(|e| e.id == id && e.available == available)
        };
        assert!(has("claude_code", true));
        assert!(has("openai_sdk", true));
        // 透传落地了：cursor-agent 入口标可用；IDE local mode 还差零售版的一处布尔补丁。
        assert!(has("cursor_local_mode", false));
        assert!(has("cursor_agent_cli", true));

        // 真的在监听：healthz 通，且没带口令的请求被 401。
        let base = info.base_url.clone();
        let http = reqwest::Client::new();
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

        // 透传端口也真的在监听（TCP 层面能连上；协议层面的转发已经在
        // `passthrough` 模块的单元 / 集成测试里覆盖）。
        assert!(
            tokio::net::TcpStream::connect(&info.passthrough_addr)
                .await
                .is_ok(),
            "透传端口该在监听"
        );

        // 改了 client-type → 提示重启。
        svc.update_settings(SettingsPatch {
            client_type: Some("ide".into()),
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
        assert!(
            tokio::net::TcpStream::connect(&info.passthrough_addr)
                .await
                .is_err(),
            "透传端口也该跟着关"
        );
    }

    #[tokio::test]
    async fn a_busy_port_is_skipped_and_the_new_one_is_persisted() {
        // 先占住一个端口，让网关的首选端口撞上它。
        let squatter = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let busy = squatter.local_addr().unwrap().port();
        // 首选 = 被占的那个；透传首选 = 它 + 1。透传的首选很可能正是方言口顺延后拿到的端口，
        // 于是这条也要再往后挪——测的正是「两个口不会撞在一起」。
        let svc = service();
        settings::set(&svc.db, SETTING_PORT, &busy).unwrap();
        settings::set(&svc.db, SETTING_PASSTHROUGH_PORT, &(busy + 1)).unwrap();

        let st = svc.start().await.unwrap();
        let info = st.running.clone().expect("在跑");
        let port: u16 = info.addr.rsplit(':').next().unwrap().parse().unwrap();
        let pt: u16 = info
            .passthrough_addr
            .rsplit(':')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_ne!(port, busy, "被占的端口该被跳过");
        assert!(port > busy && port <= busy + PORT_PROBE_SPAN);
        assert_ne!(pt, port, "两个口不能撞");
        assert_ne!(pt, busy);
        // 换到的端口落库成了新设置，而且不算「设置已改需重启」。
        assert_eq!(svc.settings().port, port);
        assert_eq!(svc.settings().passthrough_port, pt);
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
        assert_eq!(
            st.entrances[0].env[0],
            "CURSOR_LOCAL_AGENT_BASE_URL=http://127.0.0.1:8787"
        );
    }

    #[test]
    fn stored_accounts_stay_out_of_the_lane_until_enrolled() {
        use nexus_accounts::model::NewAccount;
        let svc = service();
        svc.accounts
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
        let again = GatewayService::new(
            svc.db.clone(),
            svc.secrets.clone(),
            svc.cursor.clone(),
            svc.accounts.clone(),
            svc.chatgpt.clone(),
        );
        assert_eq!(again.status().unwrap().lane.candidates.len(), 1);

        let st = svc.unenroll("a@x.com").unwrap();
        assert!(st.lane.candidates.is_empty());
        assert_eq!(st.lane.available.len(), 1);
        // 换 client-type 会重建 lane，名单跟过去。
        svc.enroll(&["a@x.com".to_string()]).unwrap();
        svc.update_settings(SettingsPatch {
            client_type: Some("ide".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(svc.status().unwrap().lane.candidates.len(), 1);
    }
}
