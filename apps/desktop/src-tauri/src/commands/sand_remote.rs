//! 远程 Cursor server 的 Sand 补丁 + 回到本机的隧道。
//!
//! 与本机 Sand 的分工：本机改 `/Applications/Cursor.app`，远程改 `~/.cursor-server`；两边用的是
//! **同一份规则表**（`nexus_sand::rules::catalog`），只是期望值按 `LayoutProfile::Server` 走。
//! 这一点是刻意的：过去 `gateway/scripts/sand-remote-server.py` 另维护一套只有 5 类的规则，
//! 漏掉的正是 `extensionHostProcess.js` 上的 client-type，远程于是一直以 `ide` 身份发请求。
//!
//! ## 远程怎么出网：两条路，都要一条隧道
//!
//! 远程那台默认出不去网（或出口地区拿不到 claude / gpt），所以**出网方式**是这一页的主轴，
//! 见 [`RemoteRoute`]：
//!
//! - [`RemoteRoute::Gateway`]：把推理端点改道到 `http://127.0.0.1:<远程端口>`，隧道接回本机
//!   网关透传口。号池接力、记账、面板拦截都长在这条路上，代价是链路多一跳、网关的 client-type 得是 sand。
//! - [`RemoteRoute::Proxy`]：**不改端点**，远程照旧打官方 api2，只是把本机的 HTTP 代理经隧道送过去，
//!   并让 Cursor 的 remote-ssh 扩展把 `HTTP(S)_PROXY` 注入远程会话（见 [`nexus_sand::remote::proxy`]）。
//!   链路短、用官方端点，但没有号池轮换——用的就是远程当前登录的那个号。
//! - [`RemoteRoute::Direct`]：远程自己出得去网，什么都不做。
//!
//! 隧道是 ssh 会话里的多路复用中继（[`nexus_sand::Tunnel`]），不是 `ssh -R`——为什么，见那个
//! 模块的说明。它只活在本进程里，而远程盘上那个端点 / 那份代理设置是永久的：两者一旦不同步就是
//! 「静默不通」（§9.5）。所以启动时要把隧道拉回来，界面上还要能一键走一遍
//! [`nexus_sand::RemoteSand::probe`] 实地验。
//!
//! 组装（谁依赖谁）全在 [`RemoteSandHub`] 里，`nexus-sand` 与 `nexus-gateway` 互不认识。

use crate::commands::events;
use crate::commands::switcher::run_blocking;
use crate::state::AppState;
use nexus_core::{AppError, ErrorCode, Result};
use nexus_gateway::GatewayService;
use nexus_sand::remote::{proxy, sshcfg};
use nexus_sand::{
    InstallOptions, ProbeReport, ProbeTarget, RemoteOutcome, RemoteSand, RemoteStatus,
    SandProgress, Tunnel, TunnelSpec, TunnelStatus,
};
use nexus_store::{settings, Db};
use serde::{Deserialize, Deserializer, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 已保存的远程主机列表（JSON 数组）。
const SETTING_REMOTE_HOSTS: &str = "sand.remote_hosts";

/// 代理模式下 CONNECT 的目标：探针拿它验「经代理真能到 Cursor 那儿」。
const PROBE_API_HOST: &str = "api2.cursor.sh";

/// 猜本机代理端口时按顺序试的那几个。都不通就退回第一个，让界面把它显示成待确认的值。
///
/// 顺序按国内常见程度：Clash / Mihomo 的 7890、Clash Verge 的 7897、Surge 的 6152、
/// 通用的 1080 / 8080 / 8888。
const COMMON_PROXY_PORTS: &[u16] = &[7890, 7897, 6152, 1080, 8080, 8888, 7891, 10809];

/// 远程那头中继默认听的端口。
///
/// **刻意不用本机网关 / 代理的端口号**：远程上 7890 / 7897 常常已经有别的东西在听（平台自带的
/// 出网代理、用户自己挂的 clash），第一版「两端同口」在 devbox-01 上就撞上了。取一个高位的、
/// 不像任何常见服务的号；每台主机可以另改。
pub const DEFAULT_REMOTE_PORT: u16 = 41777;

/// 远程怎么出网。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteRoute {
    /// 经本机网关：端点改道 + 隧道接到网关透传口。
    #[default]
    Gateway,
    /// 经本机代理：不改端点，隧道把本机代理送过去，远程照旧打官方 api2。
    Proxy,
    /// 远程自己出网：不改端点、不起隧道。
    Direct,
}

impl RemoteRoute {
    /// 这条路要不要隧道。
    pub fn needs_tunnel(self) -> bool {
        matches!(self, Self::Gateway | Self::Proxy)
    }

    /// 装补丁时要不要写端点改道。**只有网关模式要**——代理模式故意不改端点，那正是它「走官方
    /// 端点」的全部含义。
    pub fn rewrites_endpoint(self) -> bool {
        matches!(self, Self::Gateway)
    }
}

/// 老配置里这一项是布尔 `routeViaLocal`（那时只有「经网关」和「不经」两种）。
/// `true` → 网关，`false` → 远程自己出网。
fn de_route<'de, D: Deserializer<'de>>(d: D) -> std::result::Result<RemoteRoute, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Legacy(bool),
        Named(RemoteRoute),
    }
    Ok(match Repr::deserialize(d)? {
        Repr::Legacy(true) => RemoteRoute::Gateway,
        Repr::Legacy(false) => RemoteRoute::Direct,
        Repr::Named(r) => r,
    })
}

fn default_remote_port() -> u16 {
    DEFAULT_REMOTE_PORT
}

/// 一台已保存的远程主机。`host` 是 `ssh` 认的名字：`~/.ssh/config` 里的 Host 别名，或
/// `user@hostname`。**不保存任何凭证**——认证完全交给用户自己的 ssh 配置（密钥 / agent /
/// ProxyCommand），这也是用系统 `ssh` 而不是 Rust SSH 库的原因。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteHost {
    pub host: String,
    /// 界面上的显示名；空则显示 host。
    #[serde(default)]
    pub label: String,
    /// 出网方式。`alias` 让老配置里的 `routeViaLocal` 还能读进来。
    #[serde(default, alias = "routeViaLocal", deserialize_with = "de_route")]
    pub route: RemoteRoute,
    /// 隧道在远程那头监听的端口。网关模式下补丁写进远程 bundle 的端点就是它；代理模式下
    /// Cursor 设置里的 `HTTP_PROXY` 就是它。老配置（`remoteProxyPort`，可能为 null）由
    /// [`StoredHost`] 归一，这里只认新名字。
    #[serde(default = "default_remote_port")]
    pub remote_port: u16,
    /// 代理模式用：本机代理监听的端口。`None` = 用探测到的值。
    #[serde(default)]
    pub proxy_port: Option<u16>,
}

/// 老配置里的 `remoteProxyPort` 可能是 `null`；serde 的 alias 撞上 null 会报错，所以先读成
/// 松一点的形状再收紧。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredHost {
    host: String,
    #[serde(default)]
    label: String,
    #[serde(default, alias = "routeViaLocal", deserialize_with = "de_route")]
    route: RemoteRoute,
    #[serde(default)]
    remote_port: Option<u16>,
    #[serde(default)]
    remote_proxy_port: Option<u16>,
    #[serde(default)]
    proxy_port: Option<u16>,
}

impl From<StoredHost> for RemoteHost {
    fn from(s: StoredHost) -> Self {
        RemoteHost {
            host: s.host,
            label: s.label,
            route: s.route,
            remote_port: s
                .remote_port
                .or(s.remote_proxy_port)
                .filter(|p| *p != 0)
                .unwrap_or(DEFAULT_REMOTE_PORT),
            proxy_port: s.proxy_port,
        }
    }
}

/// 启动时该给哪几台主机拉隧道：这条路要隧道的都算。**不**再走一次 ssh 去问远程盘上到底装
/// 没装——那是每台一次往返（走跳板十几秒），会把启动拖住；而猜多了的代价只是一条闲置的
/// ssh，猜少了的代价却是远程静默连不上。
fn hosts_to_restore(hosts: &[RemoteHost]) -> Vec<String> {
    hosts
        .iter()
        .filter(|h| h.route.needs_tunnel())
        .map(|h| h.host.clone())
        .collect()
}

/// 本机 loopback 上有人在听这个端口。用来猜代理端口、也用来在装之前拦住「代理没开」。
fn port_listening(port: u16) -> bool {
    use std::net::{Ipv4Addr, SocketAddr, TcpStream};
    use std::time::Duration;
    TcpStream::connect_timeout(
        &SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        Duration::from_millis(120),
    )
    .is_ok()
}

/// 猜本机代理端口：先认环境变量里的，再挨个试常见端口。
///
/// 猜错不致命——界面上是个可改的输入框，装之前还会校验「这个端口真有人听」。
fn detect_proxy_port() -> Option<u16> {
    for key in [
        "https_proxy",
        "HTTPS_PROXY",
        "http_proxy",
        "HTTP_PROXY",
        "all_proxy",
        "ALL_PROXY",
    ] {
        if let Some(port) = std::env::var(key).ok().as_deref().and_then(port_of_url) {
            if port_listening(port) {
                return Some(port);
            }
        }
    }
    COMMON_PROXY_PORTS
        .iter()
        .copied()
        .find(|p| port_listening(*p))
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// 从 `http://127.0.0.1:7890` 这种地址里抠出端口。只认 loopback——隧道转发的是本机地址，
/// 指向别处的代理经隧道是送不过去的。
fn port_of_url(raw: &str) -> Option<u16> {
    let rest = raw
        .trim()
        .rsplit_once("://")
        .map(|(_, r)| r)
        .unwrap_or(raw.trim());
    let host_port = rest.split('/').next()?;
    let (host, port) = host_port.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    matches!(host, "127.0.0.1" | "localhost" | "::1").then(|| port.parse().ok())?
}

/// 界面一次拿全：主机、补丁状态（可能失败——ssh 连不上）、隧道状态、网关是否在跑。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteHostView {
    pub host: RemoteHost,
    /// `Err` 时给字符串：连不上不该让整个列表都拿不到。
    pub status: std::result::Result<RemoteStatus, String>,
    pub tunnel: TunnelStatus,
    /// 隧道本机这头接到哪个端口（网关透传口 / 本机代理口）。`None` = 这条路不用隧道，或代理
    /// 模式下一个代理端口都猜不到。
    pub local_port: Option<u16>,
    /// 代理模式：Cursor 那三个设置此刻给这台主机配的是什么（`None` = 没配）。
    /// 它和「盘上的端点」一样是**永久状态**，得和隧道对着看。
    pub proxy_configured: Option<String>,
    /// 本机那头（网关口 / 代理口）现在有没有人听。
    pub local_listening: bool,
    /// 装补丁时会写进远程 bundle 的端点（网关模式）。界面拿它和盘上的 `inference_endpoint`
    /// 对：不一样 = 远程指着一个旧端口，要重装。
    pub expected_endpoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteOverview {
    pub hosts: Vec<RemoteHostView>,
    /// 本机 passthrough 网关是否在跑；不跑，隧道那头就没人接。
    pub gateway_running: bool,
    pub gateway_passthrough_port: u16,
    /// 网关转发时改写进 `x-cursor-client-type` 的值。远程请求经网关出去时用的是**这个**身份，
    /// 不是远程补丁写的那个：网关设成 `cli` / `ide`，远程的 sand 身份就在这里被换掉、记到普通额度上，
    /// 表现和九月那次 `resource_exhausted` 一模一样。所以界面要在它不是 `sand` 时明说。
    pub gateway_client_type: String,
    /// 本机 Cursor 的 commit，用来提示「远程 server 与本机是否同版本」。
    pub local_commit: Option<String>,
    /// 猜出来的本机代理端口（代理模式的默认值）。`None` = 一个常见端口都没人听。
    pub detected_proxy_port: Option<u16>,
}

pub struct RemoteSandHub {
    db: Arc<Db>,
    sand: Arc<RemoteSand>,
    gateway: Arc<GatewayService>,
    tunnels: Mutex<HashMap<String, Arc<Tunnel>>>,
    local_commit: Option<String>,
    /// Cursor 的用户设置（`…/User/settings.json`）。代理模式要往里写那三个
    /// `remote.SSH.*Proxy`；找不到 Cursor 时为 `None`，代理模式届时报错而不是静默不生效。
    cursor_settings: Option<PathBuf>,
    /// 改用户设置之前把原文备份到这里（我们自己的数据目录，不在用户那边留垃圾）。
    settings_backup_dir: PathBuf,
    /// 用户的 `~/.ssh/config`。只用来把早期版本写进去的 `RemoteForward` 清掉。
    ssh_config: Option<PathBuf>,
}

impl RemoteSandHub {
    pub fn new(
        db: Arc<Db>,
        data_dir: &Path,
        gateway: Arc<GatewayService>,
        local_commit: Option<String>,
    ) -> Self {
        Self {
            sand: Arc::new(RemoteSand::with_system_ssh(data_dir, local_commit.clone())),
            db,
            gateway,
            tunnels: Mutex::new(HashMap::new()),
            local_commit,
            cursor_settings: nexus_cursor::CursorPaths::detect()
                .ok()
                .map(|p| p.user_dir.join("User").join("settings.json")),
            settings_backup_dir: data_dir.join("sand").join("cursor-settings"),
            ssh_config: home_dir().map(|h| h.join(".ssh").join("config")),
        }
    }

    pub fn hosts(&self) -> Vec<RemoteHost> {
        let stored: Vec<StoredHost> = settings::get_or(&self.db, SETTING_REMOTE_HOSTS, Vec::new());
        stored.into_iter().map(RemoteHost::from).collect()
    }

    fn save_hosts(&self, hosts: &[RemoteHost]) -> Result<()> {
        settings::set(&self.db, SETTING_REMOTE_HOSTS, &hosts.to_vec())
    }

    pub fn add_host(&self, mut host: RemoteHost) -> Result<Vec<RemoteHost>> {
        host.host = host.host.trim().to_string();
        if host.host.is_empty() {
            return Err(AppError::invalid("主机名不能为空。"));
        }
        if host
            .host
            .chars()
            .any(|c| c.is_whitespace() || c == '\'' || c == '"' || c == ';' || c == '&' || c == '|')
        {
            return Err(AppError::invalid(
                "主机名只能是 ssh 认的形式：config 里的别名，或 user@hostname。",
            ));
        }
        if host.remote_port == 0 {
            host.remote_port = DEFAULT_REMOTE_PORT;
        }
        let mut hosts = self.hosts();
        if hosts.iter().any(|h| h.host == host.host) {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                format!("{} 已经在列表里。", host.host),
            ));
        }
        hosts.push(host);
        self.save_hosts(&hosts)?;
        Ok(hosts)
    }

    pub fn remove_host(&self, host: &str) -> Result<Vec<RemoteHost>> {
        // 主机没了，写在 Cursor 设置里的那一处也得跟着走。
        self.clear_persistent_state(host);
        let mut hosts = self.hosts();
        hosts.retain(|h| h.host != host);
        self.save_hosts(&hosts)?;
        Ok(hosts)
    }

    /// 改一台主机的设置。
    ///
    /// 出网方式一改，Cursor 的代理设置就要跟着动。**先落这一步再存主机**：写失败（代理没开、
    /// 用户有个全局代理）时列表保持原样，界面上看到的和盘上永远是一致的。
    pub fn update_host(&self, mut host: RemoteHost) -> Result<Vec<RemoteHost>> {
        if host.remote_port == 0 {
            return Err(AppError::invalid("远程端口不能是 0。"));
        }
        let mut hosts = self.hosts();
        let Some(slot) = hosts.iter_mut().find(|h| h.host == host.host) else {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                format!("{} 不在列表里。", host.host),
            ));
        };
        host.label = host.label.trim().to_string();
        self.sync_persistent_state(&host)?;
        *slot = host;
        self.save_hosts(&hosts)?;
        Ok(hosts)
    }

    fn tunnel_for(&self, host: &str) -> Arc<Tunnel> {
        self.tunnels
            .lock()
            .expect("tunnels lock")
            .entry(host.to_string())
            .or_insert_with(|| Arc::new(Tunnel::new()))
            .clone()
    }

    /// 本机网关的透传端口：网关模式下隧道本机这头接的就是它。
    pub fn passthrough_port(&self) -> u16 {
        self.gateway.settings().passthrough_port
    }

    /// 网关模式下写进远程 bundle 的推理端点：远程那头的中继口。
    pub fn endpoint_for(&self, host: &RemoteHost) -> String {
        format!("http://127.0.0.1:{}", host.remote_port)
    }

    // ------------------------------------------------------- 代理模式的设置

    /// 代理模式下本机那头的端口。
    fn proxy_local_port(&self, host: &RemoteHost) -> Result<u16> {
        host.proxy_port.or_else(detect_proxy_port).ok_or_else(|| {
            AppError::invalid("没找到本机的 HTTP 代理端口。")
                .with_hint("先把代理开起来（Clash 一般是 7890），或在这台主机的设置里手填端口。")
        })
    }

    fn settings_path(&self) -> Result<&Path> {
        self.cursor_settings.as_deref().ok_or_else(|| {
            AppError::new(
                ErrorCode::CursorNotFound,
                "没找到 Cursor 的用户设置目录，代理模式没法配置。",
            )
            .with_hint("代理模式要往 Cursor 的 settings.json 写 remote.SSH.httpProxy——那是让远程会话经代理出网的唯一入口。")
        })
    }

    fn read_settings(&self) -> String {
        self.cursor_settings
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_else(|| "{}\n".into())
    }

    /// 写回 Cursor 的用户设置。**先把原文备份到我们自己的数据目录**——这是用户的文件，
    /// 哪怕定点编辑器有测试兜着，也得留一条退路。
    fn write_settings(&self, text: &str) -> Result<()> {
        let path = self.settings_path()?;
        if let Ok(original) = std::fs::read(path) {
            let _ = std::fs::create_dir_all(&self.settings_backup_dir);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = std::fs::write(
                self.settings_backup_dir
                    .join(format!("settings-{stamp}.json")),
                &original,
            );
        }
        nexus_sand::commit::atomic_write(path, text.as_bytes())
    }

    /// 把代理设置写成「这台主机走 `http://127.0.0.1:<远程端口>`」。已经是目标状态就不动盘。
    fn apply_proxy_settings(&self, host: &RemoteHost) -> Result<u16> {
        let local_port = self.proxy_local_port(host)?;
        if !port_listening(local_port) {
            return Err(
                AppError::invalid(format!("本机 127.0.0.1:{local_port} 没有代理在监听。"))
                    .with_hint(
                        "先把代理开起来，或改成正确的端口——隧道那头没人接的话远程照样出不去网。",
                    ),
            );
        }
        // 远程会话里看到的地址是**远程**那一头的端口（隧道的入口）。
        let url = self.endpoint_for(host);
        let current = self.read_settings();
        if let Some(next) = proxy::apply(&current, &host.host, &url)? {
            self.write_settings(&next)?;
        }
        Ok(local_port)
    }

    /// 摘掉这台主机的代理设置。切走代理模式、移除主机、卸载补丁都要做——留着它，
    /// 远程会话下次还会去连一个已经不存在的隧道端口。
    fn clear_proxy_settings(&self, host: &str) -> Result<()> {
        if self.cursor_settings.is_none() {
            return Ok(());
        }
        let current = self.read_settings();
        if let Some(next) = proxy::remove(&current, host)? {
            self.write_settings(&next)?;
        }
        Ok(())
    }

    // ------------------------------------------------- 早期版本留在 ssh config 里的东西

    /// 早期版本有一种「挂在 Cursor 的连接上」的隧道，往 `~/.ssh/config` 里写 `RemoteForward`。
    /// 那行在很多平台上只会让每次 ssh 都报 `remote port forwarding failed`，现在没人用它了，
    /// 启动时和移除主机时都顺手删掉。删不掉不报错——它只是碍眼，不影响新隧道。
    fn clear_ssh_forward(&self, host: &str) {
        let Some(path) = self.ssh_config.as_deref() else {
            return;
        };
        let Ok(current) = std::fs::read_to_string(path) else {
            return;
        };
        if let Ok(Some(next)) = sshcfg::remove(&current, host) {
            let _ = std::fs::create_dir_all(&self.settings_backup_dir);
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = std::fs::write(
                self.settings_backup_dir.join(format!("ssh-config-{stamp}")),
                current.as_bytes(),
            );
            if nexus_sand::commit::atomic_write(path, next.as_bytes()).is_ok() {
                tracing::info!(%host, "已从 ~/.ssh/config 移除早期版本写的 RemoteForward");
            }
        }
    }

    /// 把这台主机的永久状态调成它当前设置该有的样子：Cursor 的代理设置。
    ///
    /// 每次改设置 / 装补丁都整体过一遍，而不是各处零敲碎打 —— 「切走某个模式时忘了摘掉上一个
    /// 模式留下的东西」是这类联动最容易漏的地方，而漏掉的表现是远程去连一个早就不存在的端口。
    fn sync_persistent_state(&self, host: &RemoteHost) -> Result<()> {
        if host.route == RemoteRoute::Proxy {
            self.apply_proxy_settings(host)?;
        } else {
            self.clear_proxy_settings(&host.host)?;
        }
        self.clear_ssh_forward(&host.host);
        Ok(())
    }

    /// 把这台主机留在系统里的一切都摘掉（移除主机 / 卸载补丁时）。
    fn clear_persistent_state(&self, host: &str) {
        let _ = self.clear_proxy_settings(host);
        self.clear_ssh_forward(host);
    }

    /// 只读总览。每台主机的 status 要走一次 ssh；连不上的记成字符串，不拖累其它主机。
    pub fn overview(&self) -> RemoteOverview {
        let detected = detect_proxy_port();
        let settings_text = self.read_settings();
        let hosts = self
            .hosts()
            .into_iter()
            .map(|h| {
                let status = self.sand.status(&h.host).map_err(|e| {
                    let mut msg = e.message;
                    if let Some(h) = e.hint {
                        msg.push('\n');
                        msg.push_str(&h);
                    }
                    msg
                });
                let local_port = match h.route {
                    RemoteRoute::Gateway => Some(self.passthrough_port()),
                    RemoteRoute::Proxy => h.proxy_port.or(detected),
                    RemoteRoute::Direct => None,
                };
                RemoteHostView {
                    tunnel: self.tunnel_for(&h.host).status(),
                    local_port,
                    proxy_configured: proxy::read(&settings_text, &h.host).http,
                    local_listening: local_port.map(port_listening).unwrap_or(false),
                    expected_endpoint: h.route.rewrites_endpoint().then(|| self.endpoint_for(&h)),
                    host: h,
                    status,
                }
            })
            .collect();
        let settings = self.gateway.settings();
        RemoteOverview {
            hosts,
            gateway_running: self.gateway.is_running(),
            gateway_passthrough_port: settings.passthrough_port,
            gateway_client_type: settings.client_type,
            local_commit: self.local_commit.clone(),
            detected_proxy_port: detected,
        }
    }

    /// 单台主机的即时状态（添加主机时做连通性检查也用它）。
    pub fn status(&self, host: &str) -> Result<RemoteStatus> {
        self.sand.status(host)
    }

    fn find(&self, host: &str) -> Result<RemoteHost> {
        self.hosts()
            .into_iter()
            .find(|h| h.host == host)
            .ok_or_else(|| AppError::invalid("这台主机不在列表里。"))
    }

    /// 装补丁。出网方式决定「写不写端点改道」和「要不要动 Cursor 的代理设置」。
    ///
    /// 代理设置在**补丁之前**配：它可能因为「代理没开」「用户有个全局代理」而失败，那时候
    /// 远程一个字节都还没被改动 —— 先做能失败的事，是这一整套流程的一贯顺序（§4.1）。
    pub fn install(
        &self,
        host: &str,
        route: RemoteRoute,
        progress: &dyn Fn(SandProgress),
    ) -> Result<RemoteOutcome> {
        let mut entry = self.find(host).unwrap_or_else(|_| RemoteHost {
            host: host.to_string(),
            label: String::new(),
            route,
            remote_port: DEFAULT_REMOTE_PORT,
            proxy_port: None,
        });
        entry.route = route;
        self.sync_persistent_state(&entry)?;
        let options = InstallOptions {
            inference_endpoint: route.rewrites_endpoint().then(|| self.endpoint_for(&entry)),
            // 远程没有「重启 Cursor.app」这回事；对应动作是杀 server 进程，install 里自己做。
            relaunch: false,
            ..InstallOptions::default()
        };
        self.sand.install(host, options, progress)
    }

    pub fn uninstall(&self, host: &str, progress: &dyn Fn(SandProgress)) -> Result<RemoteOutcome> {
        // 补丁没了，那处永久状态也就没有意义了——留着只会让远程会话白连一个端口。
        self.clear_persistent_state(host);
        self.sand.uninstall(host, progress)
    }

    /// 从远程实地走一遍出网链路。网关模式验到本机网关，代理模式一路验到 api2。
    /// 打的是常驻中继的远程口——远程 Agent 用的正是它。
    pub fn probe(&self, host: &str) -> Result<ProbeReport> {
        let entry = self.find(host)?;
        match entry.route {
            RemoteRoute::Gateway => self
                .sand
                .probe(host, entry.remote_port, &ProbeTarget::Local),
            RemoteRoute::Proxy => self.sand.probe(
                host,
                entry.remote_port,
                &ProbeTarget::Proxy {
                    host: PROBE_API_HOST.to_string(),
                },
            ),
            RemoteRoute::Direct => Err(AppError::invalid(
                "这台主机设的是「远程自己出网」，没有经本机的链路可验。",
            )
            .with_hint("要验的话先把出网方式改成经本机网关或经本机代理。")),
        }
    }

    /// 这台主机的隧道两端。
    fn tunnel_spec(&self, host: &RemoteHost) -> Result<TunnelSpec> {
        let local_port = match host.route {
            RemoteRoute::Gateway => self.passthrough_port(),
            RemoteRoute::Proxy => self.proxy_local_port(host)?,
            RemoteRoute::Direct => {
                return Err(AppError::invalid(
                    "这台主机设的是「远程自己出网」，不需要隧道。",
                ))
            }
        };
        Ok(TunnelSpec {
            host: host.host.clone(),
            remote_port: host.remote_port,
            local_port,
        })
    }

    pub async fn tunnel_start(&self, host: &str) -> Result<TunnelStatus> {
        let entry = self.find(host)?;
        let spec = self.tunnel_spec(&entry)?;
        self.tunnel_for(host).start(spec).await
    }

    /// 应用启动时把该有的隧道拉回来。返回拉起了哪几台（给日志 / 活动记录用）。
    ///
    /// 这两个状态的生命周期必须对齐，否则 remote SSH 会**静默**失效：远程 bundle 里的推理端点是
    /// 写进文件的永久状态，隧道却只活在本进程里。应用一重启（换包时尤其频繁）隧道就没了，而远程
    /// 那一侧毫不知情，照样往 `127.0.0.1:<port>` 发——表现是远程 Agent 每轮
    /// `ECONNREFUSED 127.0.0.1:<port>` → 重试 → 用户在 Cursor 那边只看得到一直转圈，看不到原因。
    /// 2026-09-04 真机：17:50 换包重启，到 22:33 手动开隧道之前，远程 Agent Host 日志里 12 次
    /// ECONNREFUSED，而桌面端这边「补丁已装、网关在跑」两个灯都是绿的。
    pub async fn restore_tunnels(&self) -> Vec<String> {
        let hosts = self.hosts();
        // 早期版本写进 ~/.ssh/config 的转发行：这次启动顺手清掉。
        for h in &hosts {
            self.clear_ssh_forward(&h.host);
        }
        let mut started = Vec::new();
        for host in hosts_to_restore(&hosts) {
            match self.tunnel_start(&host).await {
                Ok(_) => started.push(host),
                // `start` 只有端口为 0 时才失败；连不上不算失败——隧道自己退避重连，
                // 状态在界面上看得见：隧道的健康必须可见且自愈。
                Err(err) => tracing::warn!(%host, %err, "远程隧道自动恢复失败"),
            }
        }
        started
    }

    pub async fn tunnel_stop(&self, host: &str) -> TunnelStatus {
        self.tunnel_for(host).stop().await
    }

    /// 应用退出时把隧道都收掉。`kill_on_drop` 兜底，但显式停更干净。
    pub async fn shutdown(&self) {
        let tunnels: Vec<Arc<Tunnel>> = self
            .tunnels
            .lock()
            .expect("tunnels lock")
            .values()
            .cloned()
            .collect();
        for t in tunnels {
            t.stop().await;
        }
    }
}

// ---------------------------------------------------------------------------
// Tauri 命令
// ---------------------------------------------------------------------------

#[tauri::command(async)]
pub fn sand_remote_overview(state: tauri::State<'_, AppState>) -> RemoteOverview {
    state.sand_remote.overview()
}

#[tauri::command(async)]
pub fn sand_remote_hosts(state: tauri::State<'_, AppState>) -> Vec<RemoteHost> {
    state.sand_remote.hosts()
}

/// 添加主机时先探一次：能连上、能找到 server 才收。避免收一个永远连不上的名字进列表。
#[tauri::command]
pub async fn sand_remote_add_host(
    state: tauri::State<'_, AppState>,
    host: RemoteHost,
) -> Result<RemoteStatus> {
    let hub = state.sand_remote.clone();
    let probe_host = host.host.trim().to_string();
    let status = run_blocking({
        let hub = hub.clone();
        move || hub.status(&probe_host)
    })
    .await?;
    hub.add_host(host)?;
    Ok(status)
}

#[tauri::command]
pub async fn sand_remote_remove_host(
    state: tauri::State<'_, AppState>,
    host: String,
) -> Result<Vec<RemoteHost>> {
    let hub = state.sand_remote.clone();
    hub.tunnel_stop(&host).await;
    hub.remove_host(&host)
}

/// 改设置。端口 / 出网方式变了的话，正在跑的隧道要按新的 spec 重起——`Tunnel::start` 遇到
/// 不同的 spec 会先停旧的。
#[tauri::command]
pub async fn sand_remote_update_host(
    state: tauri::State<'_, AppState>,
    host: RemoteHost,
) -> Result<Vec<RemoteHost>> {
    let hub = state.sand_remote.clone();
    let name = host.host.clone();
    let was_running = hub.tunnel_for(&name).status().phase != nexus_sand::TunnelPhase::Stopped;
    let hosts = run_blocking({
        let hub = hub.clone();
        move || hub.update_host(host)
    })
    .await?;
    if let Some(h) = hosts.iter().find(|h| h.host == name) {
        if h.route.needs_tunnel() {
            if was_running {
                let _ = hub.tunnel_start(&name).await;
            }
        } else {
            hub.tunnel_stop(&name).await;
        }
    }
    Ok(hosts)
}

#[tauri::command]
pub async fn sand_remote_install(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    host: String,
    route: Option<RemoteRoute>,
) -> Result<RemoteOutcome> {
    use tauri::Emitter;
    let hub = state.sand_remote.clone();
    let route = route.unwrap_or_default();
    let outcome = run_blocking({
        let hub = hub.clone();
        let host = host.clone();
        move || {
            hub.install(&host, route, &|p: SandProgress| {
                let _ = app.emit(events::SAND_REMOTE_PROGRESS, p);
            })
        }
    })
    .await?;
    // 装完了隧道不起等于白装；顺手拉起来（已经在跑同一条就是空操作）。停不停由用户决定。
    if route.needs_tunnel() {
        let _ = hub.tunnel_start(&host).await;
    }
    Ok(outcome)
}

/// 从远程实地走一遍出网链路，报出断在哪一跳。要走一条 ssh，慢（几秒），所以是显式动作。
#[tauri::command]
pub async fn sand_remote_probe(
    state: tauri::State<'_, AppState>,
    host: String,
) -> Result<ProbeReport> {
    let hub = state.sand_remote.clone();
    run_blocking(move || hub.probe(&host)).await
}

#[tauri::command]
pub async fn sand_remote_uninstall(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    host: String,
) -> Result<RemoteOutcome> {
    use tauri::Emitter;
    let hub = state.sand_remote.clone();
    hub.tunnel_stop(&host).await;
    run_blocking({
        let hub = hub.clone();
        move || {
            hub.uninstall(&host, &|p: SandProgress| {
                let _ = app.emit(events::SAND_REMOTE_PROGRESS, p);
            })
        }
    })
    .await
}

#[tauri::command]
pub async fn sand_remote_tunnel_start(
    state: tauri::State<'_, AppState>,
    host: String,
) -> Result<TunnelStatus> {
    state.sand_remote.tunnel_start(&host).await
}

#[tauri::command]
pub async fn sand_remote_tunnel_stop(
    state: tauri::State<'_, AppState>,
    host: String,
) -> Result<TunnelStatus> {
    Ok(state.sand_remote.tunnel_stop(&host).await)
}

#[tauri::command(async)]
pub fn sand_remote_tunnel_status(state: tauri::State<'_, AppState>, host: String) -> TunnelStatus {
    state.sand_remote.tunnel_for(&host).status()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(name: &str, route: RemoteRoute) -> RemoteHost {
        RemoteHost {
            host: name.into(),
            label: String::new(),
            route,
            remote_port: DEFAULT_REMOTE_PORT,
            proxy_port: None,
        }
    }

    fn load(json: &str) -> RemoteHost {
        let s: StoredHost = serde_json::from_str(json).expect("要能读");
        s.into()
    }

    /// 回归：应用重启后隧道必须自己回来。装了改道的远程是**永久**指着 `127.0.0.1:<port>` 的，
    /// 这边不拉隧道，那边每次推理都是 ECONNREFUSED，而且只有 Cursor 那个转圈能看出来。
    /// 代理模式同理：远程会话的 `HTTP_PROXY` 也是永久写在 Cursor 设置里的。
    #[test]
    fn restore_picks_every_host_whose_route_runs_through_us() {
        let hosts = [
            host("devbox-01", RemoteRoute::Gateway),
            host("走代理的那台", RemoteRoute::Proxy),
            host("自己出网的那台", RemoteRoute::Direct),
        ];
        assert_eq!(
            hosts_to_restore(&hosts),
            vec!["devbox-01".to_string(), "走代理的那台".to_string()]
        );
        assert!(hosts_to_restore(&[]).is_empty());
    }

    /// 老配置里这一项是布尔 `routeViaLocal`。`true` 必须读成「经网关」——读丢了会让这些主机
    /// 装出一个不改道的补丁，而远程出不去网，症状是装完就不通。
    #[test]
    fn hosts_saved_with_the_old_boolean_flag_still_load() {
        assert_eq!(
            load(r#"{"host":"box","routeViaLocal":true}"#).route,
            RemoteRoute::Gateway
        );
        assert_eq!(
            load(r#"{"host":"box","routeViaLocal":false}"#).route,
            RemoteRoute::Direct
        );
        // 更老的形状：连这个字段都没有。默认走网关（那时唯一的行为）。
        assert_eq!(load(r#"{"host":"box"}"#).route, RemoteRoute::Gateway);
        let new = load(r#"{"host":"box","route":"proxy","proxyPort":7890}"#);
        assert_eq!(new.route, RemoteRoute::Proxy);
        assert_eq!(new.proxy_port, Some(7890));
    }

    /// 上一版的形状：`remoteProxyPort`（可能 null）+ `tunnelMode`（现在没有这个概念了，忽略）。
    /// 远程端口从 `remoteProxyPort` 继承；没有就用默认的高位口——**不能**默认成本机网关的端口，
    /// 远程上那个号常常被别的东西占着（devbox-01 上 7897 是平台代理）。
    #[test]
    fn hosts_saved_by_the_previous_version_load_with_a_sane_remote_port() {
        let h = load(
            r#"{"host":"box","route":"proxy","proxyPort":7897,"remoteProxyPort":null,"tunnelMode":"ssh_config"}"#,
        );
        assert_eq!(h.remote_port, DEFAULT_REMOTE_PORT);
        assert_eq!(h.proxy_port, Some(7897));
        let h = load(r#"{"host":"box","route":"proxy","remoteProxyPort":21890}"#);
        assert_eq!(h.remote_port, 21890);
        let h = load(r#"{"host":"box","route":"gateway","remotePort":5000}"#);
        assert_eq!(h.remote_port, 5000);
        // 0 不是合法端口，当没写。
        assert_eq!(
            load(r#"{"host":"box","remotePort":0}"#).remote_port,
            DEFAULT_REMOTE_PORT
        );
    }

    /// 存回去用新名字；读的时候两个名字都认（`hosts()` 走 `StoredHost`）。
    #[test]
    fn hosts_round_trip_through_json() {
        let mut h = host("box", RemoteRoute::Proxy);
        h.remote_port = 4321;
        h.proxy_port = Some(7890);
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.contains("\"remotePort\":4321"));
        assert_eq!(load(&json), h);
    }

    /// 只有网关模式改端点。代理模式**故意不改** —— 那正是它「走官方端点」的全部含义，
    /// 改了就等于既走代理又绕网关，两套语义混在一起。
    #[test]
    fn only_the_gateway_route_rewrites_the_inference_endpoint() {
        assert!(RemoteRoute::Gateway.rewrites_endpoint());
        assert!(!RemoteRoute::Proxy.rewrites_endpoint());
        assert!(!RemoteRoute::Direct.rewrites_endpoint());
        assert!(RemoteRoute::Gateway.needs_tunnel());
        assert!(RemoteRoute::Proxy.needs_tunnel());
        assert!(!RemoteRoute::Direct.needs_tunnel());
    }

    /// 代理地址只认 loopback：隧道转发的是本机地址，指向别处的代理送不过去。
    #[test]
    fn only_loopback_proxy_urls_yield_a_port() {
        assert_eq!(port_of_url("http://127.0.0.1:7890"), Some(7890));
        assert_eq!(port_of_url("http://localhost:1080/"), Some(1080));
        assert_eq!(port_of_url("socks5://127.0.0.1:7891"), Some(7891));
        assert_eq!(port_of_url("127.0.0.1:8080"), Some(8080));
        // 指向别的机器：转发不过去，当没有。
        assert_eq!(port_of_url("http://10.0.0.1:3128"), None);
        assert_eq!(port_of_url("http://proxy.corp:8080"), None);
        assert_eq!(port_of_url(""), None);
        assert_eq!(port_of_url("http://127.0.0.1"), None);
    }

    /// 默认值不能悄悄变：`RemoteRoute::default()` 是网关，老用户升级后行为不变。
    #[test]
    fn the_default_route_stays_the_gateway() {
        assert_eq!(RemoteRoute::default(), RemoteRoute::Gateway);
        assert_eq!(
            serde_json::to_string(&RemoteRoute::Proxy).unwrap(),
            "\"proxy\""
        );
    }

    /// 默认远程端口不能撞常见服务：7890 / 7897 / 8080 那些在远程上常常有人。
    #[test]
    fn the_default_remote_port_avoids_common_services() {
        let port = default_remote_port();
        assert!(!COMMON_PROXY_PORTS.contains(&port));
        assert!(port > 10_000, "{port}");
    }
}
