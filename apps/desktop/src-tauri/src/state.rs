//! 应用状态：所有 crate 在这里被组装成一个整体。
//!
//! 这是唯一知道全部业务 crate 的地方。`nexus-switcher` 和 `nexus-accounts` 互相不认识
//! （ARCHITECTURE R1），它们之间的数据流动只发生在这一层的显式拷贝里（见
//! `commands::accounts::accounts_add_to_switch_book`）。

use crate::commands::sand_remote::RemoteSandHub;
use nexus_accounts::{AccountsService, OauthSession};
use nexus_chatgpt::ChatGptService;
use nexus_cursor::Cursor;
use nexus_gateway::GatewayService;
use nexus_grok::GrokService;
use nexus_grokbot::GrokBotService;
use nexus_kiro::KiroService;
use nexus_playground::PlaygroundService;
use nexus_sand::SandService;
use nexus_store::{settings, Backups, Db, SecretStore, SqliteSecrets};
use nexus_switcher::Switcher;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub struct AppState {
    pub db: Arc<Db>,
    /// 整库快照，落在用户目录下的 `~/.roviix/backups`（ARCHITECTURE §1.1）。放在应用数据目录
    /// **之外**是有意的：卸载应用、换机器搬家时，那个目录要还在。
    pub backups: Arc<Backups>,
    /// 账号清单导出落在这里（`~/.roviix/exports`）。同上，不在应用数据目录里。
    pub exports_dir: PathBuf,
    /// 用户目录。一键接入要找 `~/.claude` / `~/.codex`；权限预检要看它们能不能写。
    pub home_dir: PathBuf,
    /// 一键接入改客户端配置前的备份（`~/.roviix/backups/clients`）。和整库备份同一个屋顶。
    pub client_backups_dir: PathBuf,
    pub switcher: Arc<Switcher>,
    pub accounts: Arc<AccountsService>,
    /// Grok Bot 桥：Sand 补丁与本机网关按需借 Grok Bot 额度时的凭证来源。**不是账号系统**——
    /// Cursor 账号在 `accounts`。一份实例三处共用（这里 / sand / gateway），钥匙串口令只弹一次。
    pub grokbot: Arc<GrokBotService>,
    /// Sand 补丁。与 switcher 一样只由用户点击触发；两者互不认识，只共享 nexus-cursor。
    pub sand: Arc<SandService>,
    /// 远程 Cursor server 的 Sand 补丁 + 反向隧道。同一份规则表，只是对象在远端；
    /// 隧道端口跟着网关的透传端口走，所以它认识 gateway（组装在这一层完成）。
    pub sand_remote: Arc<RemoteSandHub>,
    /// ChatGPT 订阅号：本地网关的第二种号源。和 Cursor 账号两张表、互不认识（ARCHITECTURE §5.3）。
    pub chatgpt: Arc<ChatGptService>,
    /// Grok Build / Kiro：第三、四种号源。同样分表。
    pub grok: Arc<GrokService>,
    pub kiro: Arc<KiroService>,
    /// 本地推理网关。默认关着，由用户点开；读 accounts 取号、读 cursor 拿当前登录号，
    /// 不碰 switcher（ARCHITECTURE §3.4）。
    pub gateway: Arc<GatewayService>,
    /// 游乐场：会话、消息、图片的持久化与进行中的请求。它不认识网关——地址与口令由
    /// `commands::gateway::local_endpoint` 在每次发送时解出来交给它。
    pub playground: Arc<PlaygroundService>,
    /// 进行中的 OAuth 会话，按 uuid 索引。用户可以同时给几个号授权。
    pub oauth: Mutex<HashMap<String, Arc<OauthSession>>>,
}

impl AppState {
    /// 组装。
    ///
    /// `data_dir` 是应用数据目录（库、日志、补丁备份）；`roviix_dir` 是用户目录下的
    /// `~/.roviix`，只放用户主动生成、要带走的东西（整库备份、账号导出）。
    pub fn build(data_dir: &Path, roviix_dir: &Path) -> nexus_core::Result<Self> {
        let db = Arc::new(Db::open(data_dir.join("nexus.db"))?);

        // 秘密全在本地库里，不碰 OS 钥匙串（理由见 `nexus_store::SqliteSecrets`）。
        let secrets: Arc<dyn SecretStore> = Arc::new(SqliteSecrets::new(db.clone()));

        // 用户在设置里手动指定过目录就用它，否则按平台默认位置探测。
        // 安装目录单独一条：探不到它只影响启动 Cursor 与 Sand，不影响读写登录态。
        let app_dir = settings::get_raw(&db, settings::CURSOR_APP_DIR)?;
        let cursor =
            match settings::get_raw(&db, settings::CURSOR_USER_DIR)?.filter(|s| !s.is_empty()) {
                Some(dir) => Cursor::at(dir),
                None => Cursor::detect()?,
            }
            .with_app_override(app_dir.as_deref());

        let grokbot = Arc::new(GrokBotService::new(data_dir));
        let sand = Arc::new(SandService::with_grokbot(
            db.clone(),
            data_dir,
            cursor.paths.clone(),
            Arc::new(cursor.control()),
            grokbot.clone(),
        ));

        let accounts = Arc::new(AccountsService::new(db.clone(), secrets.clone()));
        let chatgpt = Arc::new(ChatGptService::new(db.clone(), secrets.clone()));
        let grok = Arc::new(GrokService::new(db.clone(), secrets.clone()));
        let kiro = Arc::new(KiroService::new(db.clone(), secrets.clone()));
        let gateway = Arc::new(GatewayService::with_grokbot(
            db.clone(),
            secrets.clone(),
            Arc::new(cursor.clone()),
            accounts.clone(),
            chatgpt.clone(),
            grok.clone(),
            kiro.clone(),
            grokbot.clone(),
        ));

        // 本机 Cursor 的 commit：远程往往堆着好几个 commit 的 server，只有和本机同 commit 的
        // 那一份是当前真正在用的。
        let local_commit = nexus_sand::SandLayout::resolve(&cursor.paths)
            .ok()
            .and_then(|l| read_commit(&l.product_json));
        let sand_remote = Arc::new(RemoteSandHub::new(
            db.clone(),
            data_dir,
            gateway.clone(),
            local_commit,
        ));

        Ok(Self {
            backups: Arc::new(Backups::new(roviix_dir.join("backups"))),
            exports_dir: roviix_dir.join("exports"),
            // `~/.roviix` 的上一级就是用户目录；万一给的是根目录，就退回它自己。
            home_dir: roviix_dir
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| roviix_dir.to_path_buf()),
            client_backups_dir: roviix_dir.join("backups").join("clients"),
            grokbot,
            sand,
            sand_remote,
            chatgpt,
            grok,
            kiro,
            gateway,
            playground: Arc::new(PlaygroundService::new(db.clone(), data_dir)),
            switcher: Arc::new(Switcher::new(db.clone(), secrets.clone(), cursor)),
            accounts,
            oauth: Mutex::new(HashMap::new()),
            db,
        })
    }
}

fn read_commit(product_json: &Path) -> Option<String> {
    let raw = std::fs::read(product_json).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    v.get("commit")?.as_str().map(str::to_string)
}
