//! 隧道：让远程 `127.0.0.1:<remote_port>` 上的连接回到本机 `127.0.0.1:<local_port>`
//! （网关透传口，或本机代理口），远程 Agent 的推理从本机出网。
//!
//! ## 为什么不是 `ssh -R`
//!
//! 第一版就是 `ssh -N -R`。2026-09-08 在 Brain++ 的工作区上真机排查：`-R` 请求被 ssh 网关
//! **接受**了，工作区里却什么都没在听——那类容器平台的 ssh 网关只把命令转进工作区，反向端口
//! 转发要么丢在网关那台机器上、要么直接吞掉；恰好工作区里 7897 又有平台自带的出网代理在听，
//! 于是「远程端口有人听」全是误判、常驻 `RemoteForward` 每次连接都 `remote port forwarding
//! failed`，而 Agent 实际走的是平台那个不稳的代理（`read ECONNRESET`）。这条路在这类平台上
//! 没有修法。
//!
//! 现在的做法：在 ssh 会话里跑一条**多路复用中继**。远程用 cursor-server 自带的 `node` 跑
//! [`RELAY_JS`]，在远程 loopback 上监听，每条连接经这条 ssh 的 stdin/stdout 回到本机；本机这头
//! 把每一路接到 `local_port`。只依赖「ssh 能执行命令」和「exec 通道对二进制透明」——两者到处
//! 都成立（Cursor Remote 自己就靠前者活着）。不动 `~/.ssh/config`，不受 sshd 转发策略影响，
//! 端口由我们选、由我们验（起不来会明说是被谁占了）。
//!
//! ## 受监管
//!
//! 一个 tokio 任务循环拉起 ssh（`kill_on_drop`），退出就退避重连（2s → 30s，撑过一分钟归零）。
//! 状态（连接中 / 已连接 / 重连中、重连次数、最后一条原因、此刻几条连接）放在 `watch` 里给界面读。
//! 「已连接」以中继打出 `READY` 为准——进程活着不代表远程在听。

use nexus_core::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch, Mutex};

/// 远程那头跑的中继脚本。
pub const RELAY_JS: &str = include_str!("relay.js");

/// 中继握手行的前缀；脚本和这里必须一致。
const PROTO: &str = "NEXUS-RELAY 1";

const OPEN: u8 = 1;
const DATA: u8 = 2;
const EOF: u8 = 3;
const CLOSE: u8 = 4;

/// 单帧载荷上限：读本机 socket 一次最多这么多，也是对远程来帧的合法性上限（远程脚本一次
/// `data` 事件不会超过 64K，超了就是流被搅乱了）。
const MAX_FRAME: usize = 1 << 20;

/// 等中继报 READY 的耐心：走跳板的 ssh 握手加远程起 node，十几秒是正常的。
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// 连本机接收方的耐心。本机回环，连不上就是没人听。
const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// 远程 `127.0.0.1:remote_port` → 本机 `127.0.0.1:local_port`。
///
/// 两个端口分开写：网关模式下本机那头是透传口；代理模式下是用户自己的代理（7890 之类），
/// 远程那头得另挑一个不撞的号——远程上常常已经有别的东西占着 7890 / 7897。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelSpec {
    pub host: String,
    /// 远程 loopback 上监听的端口。
    pub remote_port: u16,
    /// 本机 loopback 上的接收方（网关透传口 / 本机代理口）。
    pub local_port: u16,
}

impl TunnelSpec {
    /// 两端同口。
    pub fn same_port(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            remote_port: port,
            local_port: port,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelPhase {
    /// 正在起 ssh / 等中继报告就绪。
    Connecting,
    /// 中继已在远程监听。
    Connected,
    /// 上一条断了，退避中，马上重连。
    Reconnecting,
    /// 用户停掉了（或从未启动）。
    Stopped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelStatus {
    pub spec: Option<TunnelSpec>,
    pub phase: TunnelPhase,
    /// 自本次 start 以来重连了几次。
    pub reconnects: u32,
    /// 最后一条有信息量的原因（ssh 的 stderr、中继的 ERROR 行），给界面解释「为什么在重连」。
    pub last_error: Option<String>,
    /// 此刻经中继活着的连接数。有数就是真在用。
    #[serde(default)]
    pub streams: u32,
}

impl TunnelStatus {
    fn stopped() -> Self {
        Self {
            spec: None,
            phase: TunnelPhase::Stopped,
            reconnects: 0,
            last_error: None,
            streams: 0,
        }
    }
}

/// 怎么起那条会话。默认是系统 `ssh`；测试用本机 `sh` 直接跑同一份脚本。
pub type Launcher = Arc<dyn Fn(&TunnelSpec) -> tokio::process::Command + Send + Sync>;

struct Running {
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

pub struct Tunnel {
    status: watch::Sender<TunnelStatus>,
    running: Mutex<Option<Running>>,
    launcher: Launcher,
}

impl Default for Tunnel {
    fn default() -> Self {
        Self::new()
    }
}

impl Tunnel {
    pub fn new() -> Self {
        Self::with_launcher(Arc::new(ssh_launcher))
    }

    pub fn with_launcher(launcher: Launcher) -> Self {
        let (status, _) = watch::channel(TunnelStatus::stopped());
        Self {
            status,
            running: Mutex::new(None),
            launcher,
        }
    }

    pub fn status(&self) -> TunnelStatus {
        self.status.borrow().clone()
    }

    /// 订阅状态变化（界面用，也给测试等「已连接」用）。
    pub fn subscribe(&self) -> watch::Receiver<TunnelStatus> {
        self.status.subscribe()
    }

    /// 起隧道。已经在跑同一个 spec 就什么都不做；跑着别的就先停掉。
    pub async fn start(self: &Arc<Self>, spec: TunnelSpec) -> Result<TunnelStatus> {
        if spec.remote_port == 0 || spec.local_port == 0 {
            return Err(AppError::invalid("隧道端口不能是 0。"));
        }
        {
            let running = self.running.lock().await;
            // 「已经在跑」得是**监管任务还活着**，不能只看手柄还在：任务要是没了（只可能是 panic，
            // 因为正常退出的唯一出口 `stop` 会把手柄一起收走），手柄会一直留在这儿，于是相位卡在
            // 上一次的值、开关点了也没反应——症状正是「界面说在连、系统里一条 ssh 都没有」。
            let alive = running.as_ref().is_some_and(|r| !r.task.is_finished());
            if alive && self.status.borrow().spec.as_ref() == Some(&spec) {
                return Ok(self.status());
            }
        }
        self.stop().await;

        let (stop_tx, stop_rx) = oneshot::channel();
        let me = Arc::clone(self);
        let spec_for_task = spec.clone();
        let _ = self.status.send(TunnelStatus {
            spec: Some(spec.clone()),
            phase: TunnelPhase::Connecting,
            reconnects: 0,
            last_error: None,
            streams: 0,
        });
        let task = tokio::spawn(async move {
            me.supervise(spec_for_task, stop_rx).await;
        });
        *self.running.lock().await = Some(Running {
            stop: Some(stop_tx),
            task,
        });
        Ok(self.status())
    }

    pub async fn stop(&self) -> TunnelStatus {
        let running = self.running.lock().await.take();
        if let Some(mut r) = running {
            if let Some(tx) = r.stop.take() {
                let _ = tx.send(());
            }
            // 给监管循环一点时间把子进程杀干净；不等它自己退出就 abort。
            if tokio::time::timeout(Duration::from_secs(3), &mut r.task)
                .await
                .is_err()
            {
                r.task.abort();
            }
        }
        let _ = self.status.send(TunnelStatus::stopped());
        self.status()
    }

    fn update(&self, f: impl FnOnce(&mut TunnelStatus)) {
        self.status.send_modify(f);
    }

    async fn supervise(self: Arc<Self>, spec: TunnelSpec, mut stop_rx: oneshot::Receiver<()>) {
        let mut backoff = Duration::from_secs(2);
        loop {
            self.update(|s| {
                s.phase = TunnelPhase::Connecting;
                s.streams = 0;
            });
            let started = tokio::time::Instant::now();
            let outcome = tokio::select! {
                r = self.run_once(&spec) => r,
                _ = &mut stop_rx => return,
            };
            // 撑过一分钟就算这次是健康的，退避归零；否则加倍（上限 30s）。
            if started.elapsed() > Duration::from_secs(60) {
                backoff = Duration::from_secs(2);
            } else {
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
            self.update(|s| {
                s.phase = TunnelPhase::Reconnecting;
                s.reconnects += 1;
                s.streams = 0;
                if let Some(err) = outcome {
                    s.last_error = Some(err);
                }
            });
            tokio::select! {
                _ = tokio::time::sleep(backoff) => {}
                _ = &mut stop_rx => return,
            }
        }
    }

    /// 跑一条会话直到它退出。返回值是这次退出的原因（给界面看），`None` = 没抓到有用信息。
    async fn run_once(self: &Arc<Self>, spec: &TunnelSpec) -> Option<String> {
        let mut cmd = (self.launcher)(spec);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return Some(format!("起不来 ssh：{e}")),
        };
        let stdin = child.stdin.take().expect("已经 piped");
        let stdout = child.stdout.take().expect("已经 piped");
        let stderr = child.stderr.take().expect("已经 piped");

        // stderr 单独收：ssh 的失败原因都在那儿。
        let ssh_error: Arc<std::sync::Mutex<Option<String>>> = Arc::default();
        let stderr_task = tokio::spawn({
            let slot = Arc::clone(&ssh_error);
            async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(msg) = classify_ssh_line(&line) {
                        *slot.lock().expect("stderr slot") = Some(msg);
                    }
                }
            }
        });

        let mut reader = BufReader::new(stdout);
        let outcome = match self.handshake(&mut reader).await {
            Ok(()) => {
                self.update(|s| {
                    s.phase = TunnelPhase::Connected;
                    s.last_error = None;
                });
                // 活着的连接数由各路任务自己加减；这里每秒抄一次进状态，界面读得到。
                let live = Arc::new(AtomicU32::new(0));
                let mirror = tokio::spawn({
                    let me = Arc::clone(self);
                    let live = Arc::clone(&live);
                    async move {
                        let mut tick = tokio::time::interval(Duration::from_secs(1));
                        loop {
                            tick.tick().await;
                            let n = live.load(Ordering::Relaxed);
                            me.status.send_if_modified(|s| {
                                let changed = s.streams != n;
                                s.streams = n;
                                changed
                            });
                        }
                    }
                });
                let reason = pump_frames(reader, stdin, spec.local_port, live).await;
                mirror.abort();
                reason
            }
            Err(msg) => Some(msg),
        };

        let _ = child.kill().await;
        let _ = child.wait().await;
        let _ = tokio::time::timeout(Duration::from_secs(2), stderr_task).await;

        // 中继自己说的原因（ERROR 行）比 ssh 的 stderr 更具体，优先；两个都没有就看 ssh 说了什么。
        let from_ssh = ssh_error.lock().expect("stderr slot").take();
        outcome.or(from_ssh)
    }

    /// 读到 `READY` 为止。中间的杂行（远程 profile 的招呼）跳过；`ERROR` 行原样翻成人话。
    async fn handshake<R: tokio::io::AsyncBufRead + Unpin>(
        &self,
        reader: &mut R,
    ) -> std::result::Result<(), String> {
        let mut skipped = 0usize;
        let mut line = Vec::new();
        loop {
            line.clear();
            let n = match tokio::time::timeout(READY_TIMEOUT, reader.read_until(b'\n', &mut line))
                .await
            {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => return Err(format!("读 ssh 输出失败：{e}")),
                Err(_) => {
                    return Err(
                        "远程中继 60 秒内没有就绪（ssh 握手太慢，或远程起不来 node）。".into(),
                    )
                }
            };
            if n == 0 {
                // stdout 关了还没见 READY：ssh 自己退了（原因在 stderr，run_once 会补上）。
                return Err("ssh 会话在中继就绪前就结束了。".into());
            }
            let text = String::from_utf8_lossy(&line);
            let text = text.trim();
            if let Some(rest) = text.strip_prefix(PROTO) {
                let rest = rest.trim();
                if rest.starts_with("READY") {
                    return Ok(());
                }
                if let Some(err) = rest.strip_prefix("ERROR") {
                    return Err(describe_relay_error(err.trim()));
                }
            }
            skipped += n;
            if skipped > 64 * 1024 {
                return Err("远程输出了 64K 仍没有中继的就绪行：命令没跑起来。".into());
            }
        }
    }
}

/// 帧循环：远程来的 OPEN/DATA/EOF/CLOSE 分发到各路本机连接；各路读到的字节经写任务回去。
/// 返回时会话已经结束（stdout 断了，或帧不合法）。
async fn pump_frames<R: tokio::io::AsyncBufRead + Unpin>(
    mut reader: R,
    mut stdin: tokio::process::ChildStdin,
    local_port: u16,
    live: Arc<AtomicU32>,
) -> Option<String> {
    let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(256);
    let writer = tokio::spawn(async move {
        while let Some(buf) = out_rx.recv().await {
            if stdin.write_all(&buf).await.is_err() {
                break;
            }
        }
    });

    let mut streams: HashMap<u32, mpsc::Sender<Inbound>> = HashMap::new();
    let mut head = [0u8; 9];
    let reason = loop {
        if reader.read_exact(&mut head).await.is_err() {
            break None; // ssh 断了：正常的重连路径
        }
        let kind = head[0];
        let id = u32::from_be_bytes([head[1], head[2], head[3], head[4]]);
        let len = u32::from_be_bytes([head[5], head[6], head[7], head[8]]) as usize;
        if len > MAX_FRAME {
            break Some(format!("中继流错乱（帧长 {len}），重连。"));
        }
        let mut payload = vec![0u8; len];
        if len > 0 && reader.read_exact(&mut payload).await.is_err() {
            break None;
        }
        match kind {
            OPEN => {
                // 本机这头主动关掉的路（接收方没开、写出错）远程不会再回 CLOSE，它们的手柄留在
                // 表里只是个死的 Sender；每来一路新的顺手扫掉。
                streams.retain(|_, tx| !tx.is_closed());
                let (tx, rx) = mpsc::channel::<Inbound>(64);
                streams.insert(id, tx);
                tokio::spawn(pump_stream(
                    id,
                    local_port,
                    rx,
                    out_tx.clone(),
                    StreamCount::add(&live),
                ));
            }
            DATA => {
                if let Some(tx) = streams.get(&id) {
                    if tx.send(Inbound::Data(payload)).await.is_err() {
                        streams.remove(&id);
                    }
                }
            }
            EOF => {
                if let Some(tx) = streams.get(&id) {
                    let _ = tx.send(Inbound::Eof).await;
                }
            }
            CLOSE => {
                // 丢掉手柄，那一路的任务看到通道关了就收尾（计数由它自己减）。
                streams.remove(&id);
            }
            other => break Some(format!("中继流错乱（帧类型 {other}），重连。")),
        }
    };
    drop(streams);
    drop(out_tx);
    let _ = tokio::time::timeout(Duration::from_secs(2), writer).await;
    reason
}

enum Inbound {
    Data(Vec<u8>),
    Eof,
}

/// 活着的连接数：建时加一，任务无论从哪条路退出都减一。
struct StreamCount(Arc<AtomicU32>);

impl StreamCount {
    fn add(live: &Arc<AtomicU32>) -> Self {
        live.fetch_add(1, Ordering::Relaxed);
        Self(Arc::clone(live))
    }
}

impl Drop for StreamCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

fn frame(kind: u8, id: u32, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(9 + payload.len());
    buf.push(kind);
    buf.extend_from_slice(&id.to_be_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    buf
}

/// 一路连接：连本机接收方，远程来的字节写进去，读出来的字节送回远程。
///
/// 结束条件：远程 CLOSE（`rx` 关了）→ 直接丢掉本机 socket；本机读到 EOF → 发 EOF 帧但继续
/// 等远程那头把它的半边也收掉；本机读出错 → 发 CLOSE。连不上本机接收方（网关没开）也发
/// CLOSE，远程客户端立刻拿到连接被拒，比吊着好。
async fn pump_stream(
    id: u32,
    local_port: u16,
    mut rx: mpsc::Receiver<Inbound>,
    out: mpsc::Sender<Vec<u8>>,
    _count: StreamCount,
) {
    let sock = match tokio::time::timeout(
        LOCAL_CONNECT_TIMEOUT,
        TcpStream::connect(("127.0.0.1", local_port)),
    )
    .await
    {
        Ok(Ok(s)) => s,
        _ => {
            let _ = out.send(frame(CLOSE, id, &[])).await;
            return;
        }
    };
    let _ = sock.set_nodelay(true);
    let (mut rd, mut wr) = sock.into_split();

    let to_remote = {
        let out = out.clone();
        async move {
            let mut buf = vec![0u8; 32 * 1024];
            loop {
                match rd.read(&mut buf).await {
                    Ok(0) => {
                        let _ = out.send(frame(EOF, id, &[])).await;
                        return true; // 干净的半关，等对面收尾
                    }
                    Ok(n) => {
                        if out.send(frame(DATA, id, &buf[..n])).await.is_err() {
                            return false;
                        }
                    }
                    Err(_) => {
                        let _ = out.send(frame(CLOSE, id, &[])).await;
                        return false;
                    }
                }
            }
        }
    };
    let from_remote = async move {
        while let Some(m) = rx.recv().await {
            match m {
                Inbound::Data(b) => {
                    if wr.write_all(&b).await.is_err() {
                        return;
                    }
                }
                Inbound::Eof => {
                    let _ = wr.shutdown().await;
                }
            }
        }
    };

    tokio::pin!(to_remote);
    tokio::pin!(from_remote);
    tokio::select! {
        clean = &mut to_remote => {
            if clean {
                // 本机说完了；远程那头收到 EOF 后会把连接收掉、发 CLOSE 过来，rx 随之关闭。
                from_remote.await;
            }
        }
        _ = &mut from_remote => {}
    }
}

// ---------------------------------------------------------------------------
// 起会话
// ---------------------------------------------------------------------------

/// 远程要跑的那条命令。脚本经 base64 进 argv（stdin 要留给帧），远程 `sh` 解开后：在
/// cursor-server 的目录里找 node，找不到退回 PATH 上的；都没有就打一行 ERROR 退出。
pub fn relay_remote_command(remote_port: u16) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(relay_launcher(remote_port));
    format!("sh -c 'eval \"$(printf %s {b64} | base64 -d)\"'")
}

/// 远程 `sh` 里跑的启动器（测试里本机 `sh` 也跑它）。
pub fn relay_launcher(remote_port: u16) -> String {
    use base64::Engine;
    let js = base64::engine::general_purpose::STANDARD.encode(RELAY_JS);
    format!(
        r#"port={remote_port}
node=""
for cand in "$HOME"/.cursor-server/bin/linux-x64/*/node "$HOME"/.cursor-server/bin/*/node "$HOME"/.cursor-server/bin/*/*/node; do
  if [ -x "$cand" ]; then node="$cand"; break; fi
done
[ -n "$node" ] || node=$(command -v node 2>/dev/null || true)
if [ -z "$node" ]; then
  printf '{PROTO} ERROR ENONODE remote has no node (cursor-server not installed)\n'
  exit 2
fi
js=$(printf %s {js} | base64 -d)
exec "$node" -e "$js" "$port"
"#
    )
}

fn ssh_launcher(spec: &TunnelSpec) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args([
        // 不要 tty：stdout 必须对二进制透明。
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ServerAliveInterval=30",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "ConnectTimeout=20",
        // 早期版本往用户 ~/.ssh/config 写过 RemoteForward；那行在很多平台上只会报
        // `remote port forwarding failed`。这条会话不需要任何转发，全部清掉。
        "-o",
        "ClearAllForwardings=yes",
        // 不复用连接：这条会话要活很久，复用会把别的 status 命令也挂在它上面。
        "-o",
        "ControlMaster=no",
        "-o",
        "ControlPath=none",
        &spec.host,
        &relay_remote_command(spec.remote_port),
    ]);
    cmd
}

/// 中继 `ERROR <code> <msg>` 行翻成人话。
fn describe_relay_error(rest: &str) -> String {
    let (code, msg) = rest.split_once(' ').unwrap_or((rest, ""));
    match code {
        "EADDRINUSE" => "远程端口被别的程序占着（中继起不来）。给这台主机换一个远程端口。".into(),
        "EACCES" => "远程不允许监听这个端口（权限）。换一个 1024 以上的端口。".into(),
        "ENONODE" => "远程上没有 node：cursor-server 没装，或不在 ~/.cursor-server 下。先用 Cursor 连一次这台机器。".into(),
        _ => format!("远程中继起不来：{code} {msg}").trim().to_string(),
    }
}

/// 从 ssh 的 stderr 里挑出我们关心的那几行。其余都是噪音。
fn classify_ssh_line(line: &str) -> Option<String> {
    let l = line.trim();
    if l.contains("Host key verification failed") {
        return Some("Host key 未确认：先在终端里 ssh 一次。".into());
    }
    if l.contains("Permission denied") {
        return Some("认证失败：需要免密登录。".into());
    }
    if l.contains("Connection timed out")
        || l.contains("Could not resolve")
        || l.contains("Connection refused")
        || l.contains("Connection closed by")
        || l.contains("Timeout, server")
        || l.contains("Broken pipe")
        || l.contains("kex_exchange_identification")
    {
        return Some(
            l.trim_start_matches("debug1: ")
                .trim_start_matches("ssh: ")
                .to_string(),
        );
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::ErrorCode;
    #[cfg(unix)]
    use tokio::net::TcpListener;

    #[test]
    fn ssh_stderr_lines_are_classified() {
        assert!(classify_ssh_line("Connection closed by UNKNOWN port 65535").is_some());
        assert!(classify_ssh_line(
            "ssh: Could not resolve hostname box: nodename nor servname provided"
        )
        .is_some());
        assert!(
            classify_ssh_line("debug1: Authentications that can continue: publickey").is_none()
        );
        // 老版本会盯的这一行现在不该出现，出现了也不是我们的事（ClearAllForwardings）。
        assert!(
            classify_ssh_line("Warning: remote port forwarding failed for listen port 8790")
                .is_none()
        );
    }

    #[test]
    fn relay_errors_are_translated() {
        assert!(describe_relay_error(
            "EADDRINUSE listen EADDRINUSE: address already in use 127.0.0.1:41777"
        )
        .contains("换一个远程端口"));
        assert!(describe_relay_error("ENONODE remote has no node").contains("node"));
        assert!(describe_relay_error("EWEIRD boom").contains("EWEIRD"));
    }

    /// 远程命令只含 `[A-Za-z0-9+/=]` 和固定外壳，能原样穿过任何登录 shell。
    #[test]
    fn the_remote_command_carries_the_script_in_argv_only() {
        let cmd = relay_remote_command(41777);
        assert!(cmd.starts_with("sh -c 'eval \"$(printf %s "));
        assert!(cmd.ends_with(" | base64 -d)\"'"));
        assert!(!cmd.contains('\n'));
        let launcher = relay_launcher(41777);
        assert!(launcher.contains("port=41777"));
        assert!(launcher.contains(".cursor-server/bin"));
        assert!(launcher.contains("ERROR ENONODE"));
    }

    #[test]
    fn frames_are_laid_out_as_the_script_expects() {
        let f = frame(DATA, 0x0102_0304, b"hi");
        assert_eq!(f, vec![2, 1, 2, 3, 4, 0, 0, 0, 2, b'h', b'i']);
        assert_eq!(frame(CLOSE, 7, &[]), vec![4, 0, 0, 0, 7, 0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn stop_without_start_is_a_noop_and_reports_stopped() {
        let t = Arc::new(Tunnel::new());
        let st = t.stop().await;
        assert_eq!(st.phase, TunnelPhase::Stopped);
        assert!(st.spec.is_none());
    }

    #[tokio::test]
    async fn zero_port_is_rejected_on_either_end() {
        let t = Arc::new(Tunnel::new());
        for spec in [
            TunnelSpec {
                host: "box".into(),
                remote_port: 0,
                local_port: 7890,
            },
            TunnelSpec {
                host: "box".into(),
                remote_port: 7890,
                local_port: 0,
            },
        ] {
            let err = t.start(spec).await.unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidInput);
        }
    }

    /// 起一条注定失败的隧道（主机名解析不出来），验证：状态进入重连、错误被抓到、stop 能收尾。
    /// 不依赖真网络——`.invalid` 是 RFC 2606 保证解析不出来的顶级域。
    #[tokio::test]
    async fn a_failing_tunnel_reports_reconnecting_with_the_reason_and_stops_cleanly() {
        let t = Arc::new(Tunnel::new());
        let mut rx = t.subscribe();
        t.start(TunnelSpec::same_port("nexus-tunnel-test.invalid", 18790))
            .await
            .unwrap();
        let seen_reconnecting = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if rx.borrow().phase == TunnelPhase::Reconnecting {
                    return true;
                }
                if rx.changed().await.is_err() {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false);
        assert!(seen_reconnecting, "status={:?}", t.status());
        let st = t.status();
        assert!(st.reconnects >= 1);
        assert!(
            st.last_error.is_some(),
            "该抓到 ssh 的失败原因，实际 {:?}",
            st.last_error
        );
        let st = t.stop().await;
        assert_eq!(st.phase, TunnelPhase::Stopped);
    }

    // ----------------------------------------------------------------- 真跑中继（本机 node）
    // 启动器是给 Linux 远端写的 sh 脚本，本机模拟只在 Unix 上做。

    #[cfg(unix)]
    fn have_node() -> bool {
        std::process::Command::new("node")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// 用本机 `sh` 跑同一份启动器：中继在本机监听 `remote_port`，本机那头接到 `local_port`。
    /// 这就是远程会发生的事，只是两头都在这台机器上。
    #[cfg(unix)]
    fn local_launcher() -> Launcher {
        Arc::new(|spec: &TunnelSpec| {
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c").arg(relay_launcher(spec.remote_port));
            // 本机没有 ~/.cursor-server：让启动器走到 `command -v node` 那一步。
            cmd.env("HOME", std::env::temp_dir());
            cmd
        })
    }

    #[cfg(unix)]
    async fn free_port() -> u16 {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        l.local_addr().unwrap().port()
    }

    #[cfg(unix)]
    async fn wait_phase(rx: &mut watch::Receiver<TunnelStatus>, want: TunnelPhase) -> bool {
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if rx.borrow().phase == want {
                    return true;
                }
                if rx.changed().await.is_err() {
                    return false;
                }
            }
        })
        .await
        .unwrap_or(false)
    }

    /// 本机接收方：一个回显服务，收到什么原样写回，对面半关时自己也关。
    #[cfg(unix)]
    async fn echo_server() -> u16 {
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let (mut r, mut w) = s.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                    let _ = w.shutdown().await;
                });
            }
        });
        port
    }

    /// 整条链路：客户端连中继口 → 帧经「ssh」回来 → 本机回显 → 再回去。三路并发、各自 200K，
    /// 顺序和内容都不能错；对面关了这边也要看到 EOF。
    #[cfg(unix)]
    #[tokio::test]
    async fn bytes_round_trip_through_the_relay_on_several_streams() {
        if !have_node() {
            eprintln!("本机没有 node，跳过中继端到端测试");
            return;
        }
        let local_port = echo_server().await;
        let remote_port = free_port().await;
        let t = Arc::new(Tunnel::with_launcher(local_launcher()));
        let mut rx = t.subscribe();
        t.start(TunnelSpec {
            host: "local".into(),
            remote_port,
            local_port,
        })
        .await
        .unwrap();
        assert!(
            wait_phase(&mut rx, TunnelPhase::Connected).await,
            "status={:?}",
            t.status()
        );

        let mut tasks = Vec::new();
        for i in 0u8..3 {
            tasks.push(tokio::spawn(async move {
                let mut c = TcpStream::connect(("127.0.0.1", remote_port))
                    .await
                    .unwrap();
                let payload: Vec<u8> = (0..200_000u32).map(|n| (n as u8) ^ i).collect();
                let want = payload.clone();
                let (mut r, mut w) = c.split();
                let write = async {
                    w.write_all(&payload).await.unwrap();
                    w.shutdown().await.unwrap();
                };
                let read = async {
                    let mut got = Vec::new();
                    r.read_to_end(&mut got).await.unwrap();
                    got
                };
                let ((), got) = tokio::join!(write, read);
                assert_eq!(got.len(), want.len(), "第 {i} 路长度不对");
                assert!(got == want, "第 {i} 路内容不对");
            }));
        }
        for task in tasks {
            tokio::time::timeout(Duration::from_secs(30), task)
                .await
                .expect("一路 30 秒没收完")
                .unwrap();
        }
        // 连接都收掉之后计数要回零（给远程那头一点时间发 CLOSE）。
        tokio::time::timeout(Duration::from_secs(5), async {
            while t.status().streams != 0 {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("streams 没归零");

        let st = t.stop().await;
        assert_eq!(st.phase, TunnelPhase::Stopped);
    }

    /// HTTP/1.x 客户端的真实行为：发完请求**不**半关，等应答；服务端写完应答后自己关连接。
    /// 客户端必须拿到完整应答再看到 EOF——这正是 curl 在真机上报「Empty reply」时缺的那一段。
    #[cfg(unix)]
    #[tokio::test]
    async fn an_http_style_exchange_delivers_the_reply_before_closing() {
        if !have_node() {
            return;
        }
        // 本机接收方：读到空行就回一段固定应答，然后关。
        let l = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let local_port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let mut got = Vec::new();
                    while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                        let n = s.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        got.extend_from_slice(&buf[..n]);
                    }
                    let body = b"hello from local\n";
                    let head = format!(
                        "HTTP/1.0 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    s.write_all(head.as_bytes()).await.unwrap();
                    s.write_all(body).await.unwrap();
                    s.shutdown().await.unwrap();
                });
            }
        });
        let remote_port = free_port().await;
        let t = Arc::new(Tunnel::with_launcher(local_launcher()));
        let mut rx = t.subscribe();
        t.start(TunnelSpec {
            host: "local".into(),
            remote_port,
            local_port,
        })
        .await
        .unwrap();
        assert!(
            wait_phase(&mut rx, TunnelPhase::Connected).await,
            "status={:?}",
            t.status()
        );

        let mut c = TcpStream::connect(("127.0.0.1", remote_port))
            .await
            .unwrap();
        c.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut got = Vec::new();
        tokio::time::timeout(Duration::from_secs(10), c.read_to_end(&mut got))
            .await
            .expect("10 秒没收到应答")
            .unwrap();
        let text = String::from_utf8_lossy(&got);
        assert!(text.starts_with("HTTP/1.0 200 OK"), "{text:?}");
        assert!(text.ends_with("hello from local\n"), "{text:?}");
        t.stop().await;
    }

    /// 本机接收方没开（网关没跑）：远程客户端要立刻被拒，不能吊着。
    #[cfg(unix)]
    #[tokio::test]
    async fn a_missing_local_receiver_closes_the_stream_instead_of_hanging() {
        if !have_node() {
            return;
        }
        let dead_local = free_port().await;
        let remote_port = free_port().await;
        let t = Arc::new(Tunnel::with_launcher(local_launcher()));
        let mut rx = t.subscribe();
        t.start(TunnelSpec {
            host: "local".into(),
            remote_port,
            local_port: dead_local,
        })
        .await
        .unwrap();
        assert!(wait_phase(&mut rx, TunnelPhase::Connected).await);

        let mut c = TcpStream::connect(("127.0.0.1", remote_port))
            .await
            .unwrap();
        c.write_all(b"GET / HTTP/1.0\r\n\r\n").await.unwrap();
        let mut buf = Vec::new();
        let n = tokio::time::timeout(Duration::from_secs(10), c.read_to_end(&mut buf))
            .await
            .expect("对面应该在几秒内关掉连接")
            .unwrap_or(0);
        assert_eq!(n, 0);
        t.stop().await;
    }

    /// 远程端口被占：中继报 EADDRINUSE，状态进入重连并把原因写清楚。
    #[cfg(unix)]
    #[tokio::test]
    async fn an_occupied_remote_port_is_reported_by_name() {
        if !have_node() {
            return;
        }
        let taken = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let remote_port = taken.local_addr().unwrap().port();
        let t = Arc::new(Tunnel::with_launcher(local_launcher()));
        let mut rx = t.subscribe();
        t.start(TunnelSpec {
            host: "local".into(),
            remote_port,
            local_port: 1,
        })
        .await
        .unwrap();
        assert!(
            wait_phase(&mut rx, TunnelPhase::Reconnecting).await,
            "status={:?}",
            t.status()
        );
        let st = t.status();
        assert!(
            st.last_error.as_deref().is_some_and(|e| e.contains("占")),
            "{:?}",
            st.last_error
        );
        t.stop().await;
        drop(taken);
    }
}
