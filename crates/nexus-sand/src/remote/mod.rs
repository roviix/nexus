//! 给**远程** Cursor server 打同一套补丁（remote SSH 场景）。
//!
//! ## 为什么需要它
//!
//! remote SSH 下 Agent 的编排与推理跑在远程 `~/.cursor-server` 上，本机 Cursor.app 只是 UI，
//! 所以 [`crate::SandService`] 改本机改不到它。而且远程那台**默认就是出不去网的**：要么没有外网，
//! 要么出口地区拿不到 claude / gpt（实测某公司代理走东京出口，grok 正常、claude 与 gpt 被 api2
//! 以 region 拒绝）。所以远程安装默认带上推理端点改道，配一条隧道（[`tunnel`]：ssh 会话里的
//! 多路复用中继，不是 `ssh -R`）把流量接回本机的 passthrough 网关。
//!
//! ## 为什么不另起一套规则
//!
//! `gateway/scripts/sand-remote-server.py` 曾经是另一套**只有 5 类**的规则，缺的那些里就包括
//! `extensionHostProcess.js` 上的 client-type —— 而 `x-cursor-client-type` 恰恰出在那个文件里
//! （`_??"ide"`）。结果是远程一直以 `ide` 身份发推理请求、记在账号自己的额度上，claude / gpt 报
//! `resource_exhausted`，只有免费的 grok 能用。**维护第二套规则本身就是那个 bug。**
//! 这里复用同一份 [`crate::rules::catalog`]，只是换一个 [`LayoutProfile::Server`] 的期望值。
//!
//! ## 怎么做到「几乎零改造」
//!
//! 不给整个 crate 抽一层远程文件系统，而是**暂存镜像**：把远程那几个目标文件拉到本地临时目录、
//! 按 `<staging>/resources/app/<rel>` 摆好，[`SandLayout::from_root`] 探测 app 根时的第二个候选
//! 形状正好接得住，于是 engine / integrity / commit / backup 全都原样复用（包括本地这一侧的
//! 原子写、写后校验、失败回滚），最后把变动的文件推回去。
//!
//! 推回去这一步自己保证原子性与可回退：先在远程把原文件备份到 `~/.nexus-sand-backup/<commit>/`，
//! 再把新内容解到**同一个文件系统上**的临时目录、逐个 `mv` 覆盖（同 fs 的 mv 是 rename）。

pub mod proxy;
mod ssh;
pub mod sshcfg;
pub mod tunnel;

use crate::backup::Backups;
use crate::commit::commit_plan;
use crate::layout::{SandLayout, TARGET_SPECS};
use crate::model::{
    InstallOptions, MarkerCounts, Operation, SandProgress, SandStep, SUPPORTED_CURSOR_VERSION,
};
use crate::rules::{self, LayoutProfile, RuleId};
use crate::service;
use nexus_core::{AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use ssh::{shell_quote, SshOutput, SshRunner, SystemSsh};
pub use tunnel::{Tunnel, TunnelPhase, TunnelSpec, TunnelStatus};

/// 远程备份根。放在远程 home 下，不放本机：**笔记本丢了远程也得能自己还原**。
const REMOTE_BACKUP_ROOT: &str = ".nexus-sand-backup";
/// 本地备份保留份数（和本机安装一致）。
const BACKUP_KEEP: usize = 10;

// ---------------------------------------------------------------------------
// 模型
// ---------------------------------------------------------------------------

/// 远程上的一份 Cursor server。同一台机器上常常堆着好几个 commit（客户端每升一次级留一份）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteServer {
    pub commit: String,
    pub version: String,
    /// 远程绝对路径，形如 `/home/u/.cursor-server/bin/linux-x64/<commit>`。
    pub root: String,
}

/// 一台远程主机的体检结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteStatus {
    pub host: String,
    /// 全部扫到的 server；按 commit 排序。
    pub servers: Vec<RemoteServer>,
    /// 选中的那一份（与本机 Cursor 同 commit 优先）。`None` = 没有可用的。
    pub selected: Option<RemoteServer>,
    /// 选中的那份版本是否是我们适配的。
    pub version_supported: bool,
    /// 与本机 Cursor 的 commit 一致。不一致时补丁锚点很可能对不上。
    pub commit_matches_local: bool,
    pub markers: MarkerCounts,
    pub patched_files: Vec<String>,
    /// 盘上装着的推理端点；`None` = 没改道（远程直连 api2）。
    pub inference_endpoint: Option<String>,
    /// marker 数与 [`LayoutProfile::Server`] 的期望一致。
    ///
    /// 这是个**指示**不是**判定**：它只数 marker，不做 client-type 残留 / 外部工具 marker 那些
    /// 需要完整内容才能算的检查（那些要把 30 多 MB 的 bundle 拉下来）。真正的硬校验在
    /// [`RemoteSand::install`] 里跑，那时候文件已经在本地了。
    pub complete: bool,
}

/// 探针要验哪条链路。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ProbeTarget {
    /// 网关模式：远程 → 隧道 → 本机那个端口，能不能拿到一个 HTTP 应答。
    Local,
    /// 代理模式：远程 → 隧道 → 本机代理 → `CONNECT <host>:443` → TLS → HTTP。
    Proxy { host: String },
}

/// 链路走到了哪一跳。失败时它就是**断点**——这是探针存在的全部意义：
/// 「连不上」和「代理拒绝 CONNECT」和「TLS 握手失败」的下一步动作完全不同。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStage {
    /// 远程连本机那个转发端口。断在这里 = 隧道没通，或本机没人监听。
    Tunnel,
    /// 代理接受 `CONNECT`。断在这里 = 隧道通了，但本机那个端口不是个 HTTP 代理。
    Proxy,
    /// 到 api2 的 TLS 握手。
    Tls,
    /// 拿到 HTTP 应答。
    Http,
}

/// 一次探针的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeReport {
    pub ok: bool,
    pub stage: ProbeStage,
    /// 拿到的 HTTP 状态码。**几百都算通**——我们验的是链路，不是鉴权：api2 对一个不带
    /// 凭证的 `GET /` 回 400 / 404 也证明请求真的到了它那儿。
    pub status: Option<u16>,
    pub detail: Option<String>,
    /// 这次用的临时远程端口，写在界面上好让人对着 `ss -ltn` 核。
    pub remote_port: u16,
}

/// 一次远程安装 / 卸载的结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteOutcome {
    pub operation: Operation,
    pub host: String,
    pub commit: String,
    pub wrote: bool,
    pub files_written: u32,
    pub backup_id: Option<String>,
    /// 是否已经把远程的 cursor-server 进程杀掉（让客户端重连时加载新 bundle）。
    pub server_restarted: bool,
    pub status: RemoteStatus,
}

// ---------------------------------------------------------------------------
// 服务
// ---------------------------------------------------------------------------

pub struct RemoteSand {
    data_dir: PathBuf,
    ssh: Arc<dyn SshRunner>,
    /// 本机 Cursor 的 commit，用来在远程一堆 commit 里挑对的那个。
    local_commit: Option<String>,
}

impl RemoteSand {
    pub fn new(
        data_dir: impl Into<PathBuf>,
        ssh: Arc<dyn SshRunner>,
        local_commit: Option<String>,
    ) -> Self {
        Self {
            data_dir: data_dir.into(),
            ssh,
            local_commit,
        }
    }

    /// 用系统 `ssh`，控制套接字放在 `data_dir/sand/ssh` 下。
    pub fn with_system_ssh(data_dir: impl Into<PathBuf>, local_commit: Option<String>) -> Self {
        let data_dir = data_dir.into();
        let ssh = Arc::new(SystemSsh::new(data_dir.join("sand").join("ssh")));
        Self::new(data_dir, ssh, local_commit)
    }

    // ------------------------------------------------------------------ 只读

    /// 扫远程有哪些 server。这是所有操作的第一步，也单独给「添加主机」时做连通性检查用。
    pub fn discover(&self, host: &str) -> Result<Vec<RemoteServer>> {
        let out = self.ssh.run(host, DISCOVER_SCRIPT, None)?;
        if !out.ok() {
            return Err(ssh::ssh_error(host, &out));
        }
        Ok(parse_discover(&out.stdout))
    }

    pub fn status(&self, host: &str) -> Result<RemoteStatus> {
        let servers = self.discover(host)?;
        let selected = self.select(&servers);
        let Some(server) = selected.clone() else {
            return Ok(RemoteStatus {
                host: host.into(),
                servers,
                selected: None,
                version_supported: false,
                commit_matches_local: false,
                markers: MarkerCounts::default(),
                patched_files: Vec::new(),
                inference_endpoint: None,
                complete: false,
            });
        };
        let scan = self.scan_markers(host, &server.root)?;
        let complete = RuleId::ALL.iter().all(|id| {
            id.expected_for(LayoutProfile::Server)
                .is_none_or(|n| id.get(&scan.markers) == n)
        });
        Ok(RemoteStatus {
            host: host.into(),
            commit_matches_local: self.local_commit.as_deref() == Some(server.commit.as_str()),
            version_supported: server.version == SUPPORTED_CURSOR_VERSION,
            servers,
            selected: Some(server),
            markers: scan.markers,
            patched_files: scan.patched_files,
            inference_endpoint: scan.inference_endpoint,
            complete,
        })
    }

    pub fn backups(&self, host: &str, commit: &str) -> Result<Vec<crate::model::SandBackup>> {
        Backups::with_key(&self.data_dir, &backup_key(host, commit)).list()
    }

    /// 从远程实地走一遍出网链路，报出断在哪一跳。
    ///
    /// 这是「三个灯都绿但用户那边一直转圈」唯一的解药。之前能查的只有各段的**状态**
    /// （补丁装了、隧道说已连接、网关在跑），而状态全绿仍然可能不通：远程端口被别的进程占着、
    /// 本机代理只听 `::1` 不听 `127.0.0.1`、公司代理拒绝 `CONNECT`……这些都要真发一次请求
    /// 才看得见。
    ///
    /// 打的就是**常驻那条中继**的远程口 `remote_port`——远程 Agent 用的正是它，验别的口验不到
    /// 真问题。第一版给探针另起临时 `ssh -R`，结果在 `-R` 被网关吞掉的平台上每次都「断在第一跳」，
    /// 而那和用户的链路半点关系没有（见 [`tunnel`] 模块说明）。
    pub fn probe(&self, host: &str, remote_port: u16, target: &ProbeTarget) -> Result<ProbeReport> {
        if remote_port == 0 {
            return Err(AppError::invalid("远程端口不能是 0。"));
        }
        let server = self.require_server(host)?;
        let script = probe_script(&server.root, remote_port, target);
        let out = self.ssh.run(host, &script, None)?;

        if let Some(report) = parse_probe(&out.stdout, remote_port) {
            return Ok(report);
        }
        // 没拿到 RESULT 行：ssh 自己就没连上，或远程的 node 起不来。都算断在第一跳。
        let stderr = out.stderr.trim();
        let detail = if stderr.is_empty() {
            format!("远程没有返回结果（ssh 退出码 {}）。", out.status)
        } else {
            stderr.lines().last().unwrap_or(stderr).to_string()
        };
        Ok(ProbeReport {
            ok: false,
            stage: ProbeStage::Tunnel,
            status: None,
            detail: Some(detail),
            remote_port,
        })
    }

    // ------------------------------------------------------------------ 写

    /// 打补丁。`options.inference_endpoint` 一般要给（远程默认出不去网）。
    pub fn install(
        &self,
        host: &str,
        options: InstallOptions,
        progress: &dyn Fn(SandProgress),
    ) -> Result<RemoteOutcome> {
        progress(SandProgress::new(
            SandStep::Preflight,
            "查找远程 Cursor server",
        ));
        let server = self.require_server(host)?;

        progress(SandProgress::new(
            SandStep::Preflight,
            "拉取远程 bundle（约 35 MB，压缩后小得多）",
        ));
        let staging = Staging::pull(self.ssh.as_ref(), host, &server)?;
        let layout = SandLayout::from_root(staging.root(), LayoutProfile::Server)?;
        if !layout.version_supported() {
            return Err(AppError::new(
                ErrorCode::SandUnsupportedVersion,
                format!(
                    "远程 server 是 {}，Sand 补丁只适配 {SUPPORTED_CURSOR_VERSION}。",
                    layout.version
                ),
            )
            .with_hint("远程没有被改动。等 Nexus 适配这个版本，或让远程跟上本机 Cursor 的版本。"));
        }

        progress(SandProgress::new(SandStep::Preflight, "校验锚点"));
        let contents = service::read_targets(&layout)?;
        service::check_preflight_anchors(&contents)?;
        // 规则表要知道盘上**已经装着**哪个端点：换端口（或从早期「两端同口」迁过来）时，新端点的
        // 规则要把旧端点当 legacy 原地迁移；只按选项组表的话新规则的 original 锚点已经不在，
        // 计划为空，这里会回一句「已经是目标状态」而远程照旧指着旧端口——2026-09-08 真机就是
        // 这样：bundle 里留着 8688，界面说重装成功，Agent 每一发 ECONNREFUSED 127.0.0.1:8688。
        // 本机那条路（`SandService::install`）一直是这么做的，远程漏了。
        let installed_ep = service::installed_endpoint(&contents);
        let rules = rules::catalog_with_installed(&options, installed_ep.as_deref())?;
        // 选项里不改道、盘上却装着：Apply 语义下 Literal 不会把 patched 还回去，先把旧端点剥掉。
        let strip = match (&options.inference_endpoint, &installed_ep) {
            (None, Some(old)) => rules::endpoint_rules(old, None)?,
            _ => Vec::new(),
        };
        let before = service::aggregate(&layout, &contents, &rules);
        if before.foreign > 0 {
            return Err(AppError::new(
                ErrorCode::SandForeignMarkers,
                format!("远程有 {} 处其它工具留下的 Sand 标记。", before.foreign),
            )
            .with_hint("先用原来的工具在远程卸载干净，再回来安装。"));
        }

        let want_endpoint_hits = if options.inference_endpoint.is_some() {
            2
        } else {
            0
        };
        let (plan, report) = service::build_plan_with_strip(
            &layout,
            &contents,
            &rules,
            service::Mode::Apply,
            &strip,
        )?;
        if plan.is_empty() {
            // 没东西可写只在两种情况下算「已经是目标状态」：补丁齐、端点也是选项要的那个。
            // 否则是锚点对不上，要报错而不是回一句无事发生。
            if service::is_complete(&before, LayoutProfile::Server)
                && before.markers.inference_endpoint == want_endpoint_hits
            {
                let status = self.status(host)?;
                return Ok(RemoteOutcome {
                    operation: Operation::Install,
                    host: host.into(),
                    commit: server.commit.clone(),
                    wrote: false,
                    files_written: 0,
                    backup_id: None,
                    server_restarted: false,
                    status,
                });
            }
            return Err(service::anchor_mismatch(&service::dry_run(
                &before,
                &report,
                0,
                LayoutProfile::Server,
            )));
        }
        let after = service::projected(&layout, &contents, &plan, &rules);
        let dr = service::dry_run(&after, &report, plan.len() as u32, LayoutProfile::Server);
        if !dr.anchors_complete {
            return Err(service::anchor_mismatch(&dr));
        }
        // 端点改道是可选项、不进通用硬校验，所以在这里单独确认它会落成选项要的样子——远程默认就靠它
        // 出网，静默漏掉的话表现就是「装完了但 Agent 一发推理就连不上」，最难查。
        if after.markers.inference_endpoint != want_endpoint_hits {
            return Err(AppError::new(
                ErrorCode::SandAnchorMismatch,
                format!(
                    "推理端点改道会命中 {} 处（需 {want_endpoint_hits} 处：建 transport + 挂路由）。",
                    after.markers.inference_endpoint
                ),
            )
            .with_hint("远程没有被改动。这个 server 版本的 transport 装配处结构与适配版本不同。"));
        }

        // 本地这一侧照常走完整的「备份 → 原子写 → 写后校验 → 失败回滚」。
        progress(SandProgress::new(
            SandStep::Write,
            format!("打补丁并校验 {} 个文件", plan.len()),
        ));
        let backups = Backups::with_key(&self.data_dir, &backup_key(host, &server.commit));
        let committed = commit_plan(
            &backups,
            &layout.app_root,
            &layout.version,
            Operation::Install,
            &plan,
            &|| {
                let contents = service::read_targets(&layout)?;
                let after = service::aggregate(&layout, &contents, &rules);
                if !service::is_complete(&after, LayoutProfile::Server)
                    || after.markers.inference_endpoint != want_endpoint_hits
                {
                    return Err(AppError::new(
                        ErrorCode::SandIntegrity,
                        format!(
                            "补丁校验失败：remainingIde={}，foreign={}，legacy={}，endpoint={}（需 {want_endpoint_hits}）",
                            after.remaining_ide,
                            after.foreign,
                            after.legacy,
                            after.markers.inference_endpoint
                        ),
                    ));
                }
                service::verify_integrity(&layout)
            },
        )?;
        let _ = backups.prune(BACKUP_KEEP);

        let changed: Vec<String> = plan.iter().map(|f| layout.relative(&f.path)).collect();
        progress(SandProgress::new(
            SandStep::Write,
            format!("推回远程（{} 个文件）", changed.len()),
        ));
        staging.push(self.ssh.as_ref(), host, &server, &changed)?;

        progress(SandProgress::new(
            SandStep::Launch,
            "重启远程 cursor-server",
        ));
        let restarted = self.restart_server(host).unwrap_or(false);
        progress(SandProgress::new(SandStep::Done, "完成"));

        Ok(RemoteOutcome {
            operation: Operation::Install,
            host: host.into(),
            commit: server.commit.clone(),
            wrote: true,
            files_written: committed.files_written,
            backup_id: Some(committed.backup_id),
            server_restarted: restarted,
            status: self.status(host)?,
        })
    }

    /// 卸载：直接用**远程自己的**备份还原，不走锚点。远程备份是安装时留下的原始字节。
    pub fn uninstall(&self, host: &str, progress: &dyn Fn(SandProgress)) -> Result<RemoteOutcome> {
        progress(SandProgress::new(SandStep::Preflight, "查找远程备份"));
        let server = self.require_server(host)?;
        let script = restore_script(&server);
        let out = self.ssh.run(host, &script, None)?;
        if !out.ok() {
            return Err(ssh::ssh_error(host, &out));
        }
        let restored: u32 = out.stdout.lines().filter(|l| !l.trim().is_empty()).count() as u32;
        if restored == 0 {
            let status = self.status(host)?;
            return Ok(RemoteOutcome {
                operation: Operation::Uninstall,
                host: host.into(),
                commit: server.commit.clone(),
                wrote: false,
                files_written: 0,
                backup_id: None,
                server_restarted: false,
                status,
            });
        }
        progress(SandProgress::new(
            SandStep::Launch,
            "重启远程 cursor-server",
        ));
        let restarted = self.restart_server(host).unwrap_or(false);
        progress(SandProgress::new(SandStep::Done, "完成"));
        Ok(RemoteOutcome {
            operation: Operation::Uninstall,
            host: host.into(),
            commit: server.commit.clone(),
            wrote: true,
            files_written: restored,
            backup_id: None,
            server_restarted: restarted,
            status: self.status(host)?,
        })
    }

    /// 杀掉远程的 cursor-server / multiplex-server，客户端重连时会用新 bundle 起来。
    /// **不动 sshd**，所以我们自己这条 ssh 会话不受影响。
    pub fn restart_server(&self, host: &str) -> Result<bool> {
        let out = self.ssh.run(host, RESTART_SCRIPT, None)?;
        Ok(out.ok() && out.stdout.contains("killed"))
    }

    // ------------------------------------------------------------------ 内部

    fn require_server(&self, host: &str) -> Result<RemoteServer> {
        let servers = self.discover(host)?;
        self.select(&servers).ok_or_else(|| {
            AppError::new(
                ErrorCode::CursorNotFound,
                format!("{host} 上没找到 Cursor server（~/.cursor-server/bin/*/*/product.json）。"),
            )
            .with_hint("先用 Cursor 的 Remote-SSH 连一次这台机器，让它把 server 装上。")
        })
    }

    /// 挑哪一份 server。优先与本机 Cursor 同 commit —— 远程 server 的版本是客户端决定的，
    /// 同 commit 才是**当前真正会被用到**的那一份；剩下的都是历史残留。
    /// 拿不到本机 commit 时退回「版本号对得上的那一个」，再不行就取最后一个。
    fn select(&self, servers: &[RemoteServer]) -> Option<RemoteServer> {
        if let Some(local) = &self.local_commit {
            if let Some(hit) = servers.iter().find(|s| &s.commit == local) {
                return Some(hit.clone());
            }
        }
        servers
            .iter()
            .find(|s| s.version == SUPPORTED_CURSOR_VERSION)
            .or_else(|| servers.last())
            .cloned()
    }

    /// 只数 marker，不拉 bundle。见 [`RemoteStatus::complete`] 的说明。
    fn scan_markers(&self, host: &str, root: &str) -> Result<MarkerScan> {
        let out = self.ssh.run(host, &scan_script(root), None)?;
        if !out.ok() {
            return Err(ssh::ssh_error(host, &out));
        }
        Ok(parse_scan(&out.stdout))
    }
}

// ---------------------------------------------------------------------------
// 暂存镜像
// ---------------------------------------------------------------------------

/// 本地临时目录，形状是 `<tmp>/resources/app/<rel>`，好让 [`SandLayout::from_root`] 认得。
struct Staging {
    dir: tempfile::TempDir,
}

impl Staging {
    fn pull(ssh: &dyn SshRunner, host: &str, server: &RemoteServer) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let app = dir.path().join("resources").join("app");
        std::fs::create_dir_all(&app)?;
        // 只拉存在的目标 + product.json。远程少 4 个 Electron/UI 文件，tar 遇到不存在的路径会
        // 整包失败，所以先问一遍。
        let rels = existing_targets(ssh, host, &server.root)?;
        if rels.is_empty() {
            return Err(AppError::new(
                ErrorCode::SandUnsupportedVersion,
                "远程 server 里没有任何可识别的补丁目标文件。",
            ));
        }
        ssh.pull_tar(host, &server.root, &rels, &app)?;
        Ok(Self { dir })
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// 把改动过的文件推回远程：先在远程备份原文件，再解到同 fs 的临时目录、逐个 `mv` 覆盖。
    fn push(
        &self,
        ssh: &dyn SshRunner,
        host: &str,
        server: &RemoteServer,
        rels: &[String],
    ) -> Result<()> {
        let app = self.dir.path().join("resources").join("app");
        let tar = tar_gz(&app, rels)?;
        let out = ssh.run(host, &push_script(server, rels), Some(&tar))?;
        if !out.ok() {
            return Err(AppError::new(
                ErrorCode::SandIntegrity,
                format!("推回远程失败：{}", out.stderr.trim()),
            )
            .with_hint(
                "远程可能只写了一部分。备份在远程 ~/.nexus-sand-backup/<commit>/，\
                 可以用卸载还原，或在 Cursor 里重连该远程后重试。",
            ));
        }
        Ok(())
    }
}

/// 本地打包。用系统 `tar`，理由和用系统 `ssh` 一样：不为这点事引一条压缩 / 归档依赖。
fn tar_gz(dir: &Path, rels: &[String]) -> Result<Vec<u8>> {
    use std::process::Command;
    let mut cmd = Command::new("tar");
    // macOS 的 bsdtar 默认把扩展属性（com.apple.provenance 之类）塞进包里，远程 GNU tar 解的时候
    // 每个文件都嚷一句 `Ignoring unknown extended header keyword`。那些属性对远程毫无意义，关掉。
    //
    // 模式用带横线的 `-c -z -f`，不用 `czf`：bsdtar 只在**第一个参数**位置认那种老式合并写法，
    // 前面一旦有别的选项就报 `Must specify one of -c, -r, -t, -u, -x`。
    cmd.env("COPYFILE_DISABLE", "1")
        .arg("-c")
        .arg("-z")
        .arg("-f")
        .arg("-")
        .arg("--no-xattrs")
        .arg("-C")
        .arg(dir);
    for r in rels {
        cmd.arg(r);
    }
    let out = cmd
        .output()
        .map_err(|e| AppError::new(ErrorCode::Io, format!("起不来 tar：{e}")))?;
    if !out.status.success() {
        return Err(AppError::new(
            ErrorCode::Io,
            format!("打包失败：{}", String::from_utf8_lossy(&out.stderr).trim()),
        ));
    }
    Ok(out.stdout)
}

fn existing_targets(ssh: &dyn SshRunner, host: &str, root: &str) -> Result<Vec<String>> {
    let out = ssh.run(host, &exists_script(root), None)?;
    if !out.ok() {
        return Err(ssh::ssh_error(host, &out));
    }
    Ok(out
        .stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect())
}

fn backup_key(host: &str, commit: &str) -> String {
    format!("ssh://{host}/{commit}")
}

// ---------------------------------------------------------------------------
// 远程脚本
// ---------------------------------------------------------------------------

/// 扫 `~/.cursor-server/bin/<arch>/<commit>/`，输出 `commit\tversion\troot`。
///
/// 版本号**必须正经解析 JSON**，不能 grep。`product.json` 里 `"version"` 出现好几次——
/// `builtInExtensions` 下 `js-debug-companion` 的 `"version": "1.1.3"` 排在第 146 行，顶层的
/// `"version": "3.19.7"` 在第 1380 行；第一版脚本 `grep | head -1` 抓到的是 1.1.3，界面于是说
/// 「远程是 1.1.3，补丁只适配本机那个版本」并把安装按钮灰掉，而旁边又写着「与本机同 commit」。
///
/// 解析器按可靠程度排：server 自带的 `node`（每份 server 根下都有，是 Cursor 自己跑 server 用的
/// 二进制，一定在）→ `python3` → 兜底 `tail -1`（VS Code 构建把顶层 version/commit 追加在文件末尾，
/// 最后一个匹配才是顶层的）。
const DISCOVER_SCRIPT: &str = r#"
ver() {
  if [ -x "$1/node" ]; then
    "$1/node" -p 'JSON.parse(require("fs").readFileSync(process.argv[1],"utf8")).version||""' "$1/product.json" 2>/dev/null && return
  fi
  if command -v python3 >/dev/null 2>&1; then
    python3 -c 'import json,sys;print(json.load(open(sys.argv[1])).get("version",""))' "$1/product.json" 2>/dev/null && return
  fi
  grep -o '"version"[[:space:]]*:[[:space:]]*"[^"]*"' "$1/product.json" | tail -1 | sed 's/.*"\([^"]*\)"$/\1/'
}
for d in "$HOME"/.cursor-server/bin/*/*/; do
  [ -f "$d/product.json" ] || continue
  d="${d%/}"
  c=$(basename "$d")
  v=$(ver "$d")
  printf '%s\t%s\t%s\n' "$c" "$v" "$d"
done
"#;

/// 杀掉运行中的 server，让 Cursor 客户端重连时用新 bundle 起来。
///
/// 3.19.x 的 server 是 `node …/out/server-main.js --start-server` 直接起的；3.18.x 及更早是
/// `bin/cursor-server --start-server` 壳脚本。第一版只认后者，在 3.19.7 上 pkill 一个都没匹配到，
/// 却因为 multiplex 那条匹配了而报「killed」——server 和抱着旧 bundle 的扩展宿主全活着，用户
/// Reload 也没用。现在两种启动形态都认，扩展宿主 / 文件监听（`bootstrap-fork`）一起清，
/// 杀完**数一遍**，还有 `--start-server` 活着就不算成功。
///
/// 「成功」= 主 server 那条 pkill 真的匹配到了东西；multiplex / 子进程匹不匹配不算数（第一版就是
/// 被 multiplex 匹配到误报的）。**不要**杀完再数存活数：Cursor 客户端会在一秒内重连并重新拉起
/// server，数出来的是新的那份，实测把一次成功的重启判成了失败。
///
/// 顺序是先子进程再主进程：反过来的话，客户端重新拉起的新 server 刚 fork 出的扩展宿主可能被
/// 第二条 pkill 误杀。不动 sshd，我们自己这条 ssh 会话不受影响。
const RESTART_SCRIPT: &str = r#"
pkill -f '\.cursor-server/bin/.*/bootstrap-fork' >/dev/null 2>&1
main=0
pkill -f '\.cursor-server/bin/.*--start-server' && main=1
pkill -f '\.cursor-server/bin/multiplex-server/' >/dev/null 2>&1
[ "$main" -eq 1 ] && echo killed
exit 0
"#;

fn exists_script(root: &str) -> String {
    let mut s = format!("root={}\n", shell_quote(root));
    s.push_str("[ -f \"$root/product.json\" ] && echo product.json\n");
    for (rel, _) in TARGET_SPECS {
        s.push_str(&format!(
            "[ -f \"$root/{rel}\" ] && echo {}\n",
            shell_quote(rel)
        ));
    }
    s.push_str("exit 0\n");
    s
}

/// 数 marker：一次 `grep -o -F -f` 扫完一个文件里的全部 marker，输出 `rel\tcount\tmarker`。
/// 用 `grep -o | uniq -c` 而不是 `grep -c`：bundle 是压缩过的单行 JS，按行数会永远得 1。
fn scan_script(root: &str) -> String {
    let markers: Vec<&str> = RuleId::ALL
        .iter()
        .flat_map(|id| id.markers().iter().copied())
        .collect();
    let mut s = format!(
        "root={}\npat=$(mktemp)\ncat > \"$pat\" <<'SANDPAT'\n",
        shell_quote(root)
    );
    for m in markers {
        s.push_str(m);
        s.push('\n');
    }
    s.push_str("SANDPAT\n");
    for (rel, _) in TARGET_SPECS {
        s.push_str(&format!(
            "f=\"$root/{rel}\"\nif [ -f \"$f\" ]; then grep -o -F -f \"$pat\" \"$f\" 2>/dev/null | sort | uniq -c | while read -r n m; do printf '%s\\t%s\\t%s\\n' {} \"$n\" \"$m\"; done; fi\n",
            shell_quote(rel)
        ));
    }
    s.push_str("rm -f \"$pat\"\n");
    // 端点是规则文本的一部分，单独抠出来给界面显示、也给卸载复原用。transport 装配处所在的 chunk
    // 每版都可能换文件（3.19.7 在 9909.js，3.19.13 并进了 agent-host 的 main.js），所以扫全部目标
    // 取第一处命中，而不是钉死某个 chunk 名。
    s.push_str("echo '---ENDPOINT---'\n");
    for (rel, _) in TARGET_SPECS {
        s.push_str(&format!(
            "f=\"$root/{rel}\"\nif [ -f \"$f\" ]; then grep -o 'sandInferenceTransport:this\\.transportFactory\\.createTransport({{baseUrl:\"[^\"]*\"' \"$f\" 2>/dev/null | head -1; fi\n"
        ));
    }
    s.push_str("exit 0\n");
    s
}

fn push_script(server: &RemoteServer, rels: &[String]) -> String {
    let root = shell_quote(&server.root);
    let bak = format!("\"$HOME\"/{REMOTE_BACKUP_ROOT}/{}", server.commit);
    let list = rels
        .iter()
        .map(|r| shell_quote(r))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        r#"set -e
root={root}
bak={bak}
mkdir -p "$bak"
for rel in {list}; do
  mkdir -p "$bak/$(dirname "$rel")"
  # 只备份第一次的原始字节：重复安装不能把备份覆盖成「已打补丁」的内容。
  [ -f "$bak/$rel" ] || cp -p "$root/$rel" "$bak/$rel"
done
# 解到同一个文件系统上的临时目录，这样下面的 mv 是 rename（原子），而不是跨设备复制。
tmp=$(mktemp -d "$root/.nexus-sand.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
tar xzf - -C "$tmp"
for rel in {list}; do
  mv -f "$tmp/$rel" "$root/$rel"
done
echo done
"#
    )
}

/// 探针的远程端。用 **server 自带的 node** 跑，不用 curl：`node` 每份 server 根下都一定在
/// （Cursor 自己就靠它跑 server），而 curl 在精简镜像里经常没有；而且要分辨
/// 「CONNECT 被拒」和「TLS 失败」，curl 的退出码远不如自己写来得准。
///
/// 只输出一行 `RESULT {json}`，别的都往 stderr 去 —— 远程的 shell profile 常常自己 echo 东西。
const PROBE_JS: &str = r#"
const net = require("net");
const tls = require("tls");
const port = Number(process.env.SAND_PORT);
const target = process.env.SAND_TARGET || "";
let done = false;
const say = (o, code) => {
  if (done) return;
  done = true;
  process.stdout.write("RESULT " + JSON.stringify(o) + "\n");
  process.exit(code);
};
const fail = (stage, detail) => say({ ok: false, stage, detail: String(detail || "").slice(0, 200) }, 7);
// 每一跳都可能悄悄挂住（代理不回话最常见），所以整体也压一个上限。
const guard = setTimeout(() => fail(target ? "proxy" : "tunnel", "15 秒没有回应"), 15000);
guard.unref && guard.unref();

/** 读到第一行 HTTP 状态行为止。 */
const readStatus = (sock, stage) => {
  let buf = "";
  sock.on("data", (chunk) => {
    buf += chunk.toString("latin1");
    const line = buf.split("\r\n")[0];
    if (buf.includes("\r\n")) {
      const m = /^HTTP\/1\.[01] (\d{3})/.exec(line);
      if (m) say({ ok: true, stage, status: Number(m[1]) }, 0);
      else fail(stage, "不是 HTTP 应答：" + line.slice(0, 80));
    }
  });
  sock.on("end", () => fail(stage, buf ? "应答不完整：" + buf.slice(0, 80) : "对方直接关了连接"));
  sock.on("error", (e) => fail(stage, e.message));
};

const sock = net.connect(port, "127.0.0.1");
sock.setTimeout(15000);
sock.on("timeout", () => fail(target ? "proxy" : "tunnel", "连上了但没有回应"));
sock.on("error", (e) => fail("tunnel", e.message));
sock.on("connect", () => {
  if (!target) {
    // 网关模式：能拿到任何 HTTP 应答就说明隧道那头真有人在听、而且说 HTTP。
    readStatus(sock, "http");
    sock.write("GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nUser-Agent: nexus-sand-probe\r\n\r\n");
    return;
  }
  // 代理模式：CONNECT → TLS → GET。
  let head = "";
  const onConnectReply = (chunk) => {
    head += chunk.toString("latin1");
    if (!head.includes("\r\n\r\n")) return;
    sock.removeListener("data", onConnectReply);
    const line = head.split("\r\n")[0];
    if (!/^HTTP\/1\.[01] 200/.test(line)) return fail("proxy", "代理拒绝 CONNECT：" + line.slice(0, 80));
    const secure = tls.connect({ socket: sock, servername: target }, () => {
      readStatus(secure, "http");
      secure.write("GET / HTTP/1.1\r\nHost: " + target + "\r\nConnection: close\r\nUser-Agent: nexus-sand-probe\r\n\r\n");
    });
    secure.on("error", (e) => fail("tls", e.message));
  };
  sock.on("data", onConnectReply);
  sock.write("CONNECT " + target + ":443 HTTP/1.1\r\nHost: " + target + ":443\r\n\r\n");
});
"#;

fn probe_script(root: &str, remote_port: u16, target: &ProbeTarget) -> String {
    let host = match target {
        ProbeTarget::Local => "",
        ProbeTarget::Proxy { host } => host.as_str(),
    };
    format!(
        r#"root={root}
node=""
for cand in "$root/node" "$root"/bin/node; do
  [ -x "$cand" ] && node="$cand" && break
done
if [ -z "$node" ]; then
  node=$(command -v node 2>/dev/null || true)
fi
if [ -z "$node" ]; then
  printf 'RESULT {{"ok":false,"stage":"tunnel","detail":"远程 server 里没找到可用的 node"}}\n'
  exit 0
fi
SAND_PORT={remote_port} SAND_TARGET={host} "$node" - <<'NEXUSPROBE'
{PROBE_JS}
NEXUSPROBE
exit 0
"#,
        root = shell_quote(root),
        host = shell_quote(host),
    )
}

/// 从远程的输出里挑出那一行 `RESULT {json}`。远程 profile 打的招呼、node 的告警都无视。
fn parse_probe(stdout: &str, remote_port: u16) -> Option<ProbeReport> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Raw {
        ok: bool,
        stage: String,
        #[serde(default)]
        status: Option<u16>,
        #[serde(default)]
        detail: Option<String>,
    }
    // 取**最后**一行：重试 / 多次输出时后面那次才是结论。
    let line = stdout
        .lines()
        .filter_map(|l| l.trim().strip_prefix("RESULT "))
        .next_back()?;
    let raw: Raw = serde_json::from_str(line.trim()).ok()?;
    let stage = match raw.stage.as_str() {
        "proxy" => ProbeStage::Proxy,
        "tls" => ProbeStage::Tls,
        "http" => ProbeStage::Http,
        _ => ProbeStage::Tunnel,
    };
    Some(ProbeReport {
        ok: raw.ok,
        stage,
        status: raw.status,
        detail: raw.detail.filter(|d| !d.trim().is_empty()),
        remote_port,
    })
}

fn restore_script(server: &RemoteServer) -> String {
    let root = shell_quote(&server.root);
    let bak = format!("\"$HOME\"/{REMOTE_BACKUP_ROOT}/{}", server.commit);
    format!(
        r#"set -e
root={root}
bak={bak}
[ -d "$bak" ] || exit 0
cd "$bak"
find . -type f | sed 's|^\./||' | while read -r rel; do
  cp -p "$bak/$rel" "$root/$rel"
  echo "$rel"
done
rm -rf "$bak"
"#
    )
}

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

fn parse_discover(stdout: &str) -> Vec<RemoteServer> {
    let mut out: Vec<RemoteServer> = stdout
        .lines()
        .filter_map(|line| {
            let mut it = line.trim().splitn(3, '\t');
            let commit = it.next()?.trim();
            let version = it.next()?.trim();
            let root = it.next()?.trim();
            (!commit.is_empty() && !root.is_empty()).then(|| RemoteServer {
                commit: commit.into(),
                version: version.into(),
                root: root.into(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.commit.cmp(&b.commit));
    out
}

#[derive(Default)]
struct MarkerScan {
    markers: MarkerCounts,
    patched_files: Vec<String>,
    inference_endpoint: Option<String>,
}

fn parse_scan(stdout: &str) -> MarkerScan {
    let mut scan = MarkerScan::default();
    let (counts, endpoint) = match stdout.split_once("---ENDPOINT---") {
        Some((a, b)) => (a, b.trim()),
        None => (stdout, ""),
    };
    for line in counts.lines() {
        let mut it = line.trim().splitn(3, '\t');
        let (Some(rel), Some(n), Some(marker)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let Ok(n) = n.trim().parse::<u32>() else {
            continue;
        };
        let Some(id) = RuleId::ALL
            .iter()
            .find(|id| id.markers().contains(&marker.trim()))
        else {
            continue;
        };
        *id.slot(&mut scan.markers) += n;
        let rel = rel.to_string();
        if !scan.patched_files.contains(&rel) {
            scan.patched_files.push(rel);
        }
    }
    scan.patched_files.sort();
    // 扫描脚本按 TARGET_SPECS 逐个 grep，命中所在的 chunk 每版都可能换，所以这里只认第一处、
    // 读到下一个引号为止，后面多出来的行原样忽略。
    if let Some((_, rest)) = endpoint.split_once("baseUrl:\"") {
        scan.inference_endpoint = rest
            .split('"')
            .next()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
    }
    scan
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(commit: &str, version: &str) -> RemoteServer {
        RemoteServer {
            commit: commit.into(),
            version: version.into(),
            root: format!("/home/u/.cursor-server/bin/linux-x64/{commit}"),
        }
    }

    #[test]
    fn discover_output_is_parsed_and_sorted() {
        let out = "b222\t3.18.25\t/home/u/.cursor-server/bin/linux-x64/b222\n\
                   a111\t3.18.9\t/home/u/.cursor-server/bin/linux-x64/a111\n\
                   \n";
        let got = parse_discover(out);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].commit, "a111");
        assert_eq!(got[0].version, "3.18.9");
        assert_eq!(got[1].root, "/home/u/.cursor-server/bin/linux-x64/b222");
    }

    #[test]
    fn discover_ignores_malformed_lines() {
        assert!(parse_discover("garbage\nonly\ttwo\n").is_empty());
    }

    /// 回归：真的跑一遍 `DISCOVER_SCRIPT`（`sh -s`），对着一份**按真机顺序**摆放的 product.json——
    /// `builtInExtensions` 里的 `"version": "1.1.3"` 在前、顶层 `"version"` 在最后。第一版脚本
    /// `grep | head -1` 读到 1.1.3，界面把安装按钮灰掉。这里没有 server 自带的 node，会走
    /// python3 或 grep-tail 兜底，两条路都必须给出顶层的那个。
    #[cfg(unix)]
    #[test]
    fn discover_script_reads_the_top_level_version_not_a_nested_one() {
        use std::io::Write;
        let home = tempfile::tempdir().unwrap();
        let root = home
            .path()
            .join(".cursor-server/bin/linux-x64/90de2327392570a5f5f625c656c6749d228e6430");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("product.json"),
            r#"{
  "nameShort": "Cursor",
  "builtInExtensions": [
    { "name": "ms-vscode.js-debug-companion", "version": "1.1.3" },
    { "name": "ms-vscode.js-debug", "version": "1.93.0" }
  ],
  "version": "3.19.7",
  "commit": "90de2327392570a5f5f625c656c6749d228e6430"
}
"#,
        )
        .unwrap();

        let mut child = std::process::Command::new("sh")
            .arg("-s")
            .env("HOME", home.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(DISCOVER_SCRIPT.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );

        let servers = parse_discover(&String::from_utf8_lossy(&out.stdout));
        assert_eq!(servers.len(), 1);
        assert_eq!(
            servers[0].commit,
            "90de2327392570a5f5f625c656c6749d228e6430"
        );
        assert_eq!(
            servers[0].version, "3.19.7",
            "读到的必须是顶层 version，不是内嵌扩展的"
        );
        assert_eq!(servers[0].root, root.to_string_lossy());
    }

    /// 远程常常堆着好几个 commit；必须挑与本机 Cursor 同 commit 的那份，
    /// 因为 server 版本是客户端决定的，其余都是历史残留。
    #[test]
    fn selection_prefers_the_commit_matching_local_cursor() {
        let servers = vec![
            server("old", "3.18.9"),
            server("cur", SUPPORTED_CURSOR_VERSION),
        ];
        let sand = RemoteSand::new("/tmp", Arc::new(FakeSsh), Some("old".into()));
        assert_eq!(sand.select(&servers).unwrap().commit, "old");
    }

    #[test]
    fn selection_falls_back_to_the_supported_version_then_to_the_last() {
        let servers = vec![server("a", "3.18.9"), server("b", SUPPORTED_CURSOR_VERSION)];
        let sand = RemoteSand::new("/tmp", Arc::new(FakeSsh), None);
        assert_eq!(sand.select(&servers).unwrap().commit, "b");

        let stale = vec![server("a", "3.18.9"), server("b", "3.18.10")];
        assert_eq!(sand.select(&stale).unwrap().commit, "b");
        assert!(sand.select(&[]).is_none());
    }

    #[test]
    fn marker_scan_sums_per_file_counts_and_lists_touched_files() {
        let client = rules::SAND_CLIENT_MARKER;
        let direct = rules::SAND_DIRECT_STREAM_MARKER;
        let out = format!(
            "out/vs/workbench/api/node/extensionHostProcess.js\t2\t{client}\n\
             extensions/cursor-agent-host/dist/675.js\t1\t{direct}\n\
             ---ENDPOINT---\n\
             sandInferenceTransport:this.transportFactory.createTransport({{baseUrl:\"http://127.0.0.1:8790\"\n"
        );
        let scan = parse_scan(&out);
        assert_eq!(scan.markers.client_type, 2);
        assert_eq!(scan.markers.inference_stream, 1);
        assert_eq!(scan.patched_files.len(), 2);
        assert_eq!(
            scan.inference_endpoint.as_deref(),
            Some("http://127.0.0.1:8790")
        );
    }

    #[test]
    fn marker_scan_without_endpoint_reports_none() {
        let out = format!(
            "extensions/cursor-agent-host/dist/675.js\t1\t{}\n---ENDPOINT---\n",
            rules::SAND_DIRECT_STREAM_MARKER
        );
        let scan = parse_scan(&out);
        assert!(scan.inference_endpoint.is_none());
        assert_eq!(scan.markers.inference_stream, 1);
    }

    /// 扫描脚本得把每个 marker 原样写进 here-doc，否则远程 grep 的模式表就是残的。
    #[test]
    fn scan_script_lists_every_marker_and_every_target() {
        let s = scan_script("/srv/root");
        for id in RuleId::ALL {
            for m in id.markers() {
                assert!(s.contains(m), "扫描脚本漏了 marker {m}");
            }
        }
        for (rel, _) in TARGET_SPECS {
            assert!(s.contains(rel), "扫描脚本漏了目标 {rel}");
        }
        assert!(s.contains("'/srv/root'"), "root 要被引号包住");
    }

    /// 回归：3.19.7 的 server 是 `node out/server-main.js --start-server`，不是 `bin/cursor-server`；
    /// 只认后者会一个都杀不到却报成功。两种形态都得匹配，扩展宿主也得清，杀完要数。
    #[test]
    fn restart_script_matches_both_server_launch_shapes_and_verifies() {
        let s = RESTART_SCRIPT;
        let re = regex::Regex::new(r"\.cursor-server/bin/.*--start-server").unwrap();
        for cmdline in [
            "/home/u/.cursor-server/bin/linux-x64/90de/node /home/u/.cursor-server/bin/linux-x64/90de/out/server-main.js --start-server --host 127.0.0.1 --port 0",
            "sh /home/u/.cursor-server/bin/linux-x64/280e/bin/cursor-server --start-server --host=127.0.0.1",
        ] {
            assert!(re.is_match(cmdline), "该匹配：{cmdline}");
        }
        assert!(!re.is_match("sshd: u@notty"), "不能碰 sshd");
        assert!(
            s.contains("bootstrap-fork"),
            "扩展宿主抱着旧 bundle，必须一起清"
        );
        assert!(
            s.find("bootstrap-fork").unwrap() < s.find("--start-server").unwrap(),
            "先杀子进程再杀主进程，免得误杀客户端刚拉起的新宿主"
        );
        // 只有主 server 那条匹配到才算成功；multiplex 单独匹配到不能报 killed。
        assert!(s.contains("&& main=1"));
        assert!(s.contains(r#"[ "$main" -eq 1 ] && echo killed"#));
        assert!(
            !s.contains("pgrep"),
            "别数存活数：客户端一秒内就会把新 server 拉起来"
        );
    }

    /// 推回去必须是「先备份、再解到同 fs 临时目录、最后 mv」，少一步都会留下半写状态。
    #[test]
    fn push_script_backs_up_before_overwriting_and_moves_within_the_same_fs() {
        let s = push_script(
            &server("c1", SUPPORTED_CURSOR_VERSION),
            &["out/main.js".into()],
        );
        assert!(s.contains("set -e"));
        assert!(s.contains(".nexus-sand-backup/c1"));
        assert!(
            s.find("cp -p").unwrap() < s.find("mv -f").unwrap(),
            "备份要发生在覆盖之前"
        );
        assert!(
            s.contains(r#"mktemp -d "$root/.nexus-sand.XXXXXX""#),
            "临时目录要建在 root 下，跨设备的 mv 不是原子的"
        );
        assert!(
            s.contains(r#"[ -f "$bak/$rel" ] ||"#),
            "重装不能覆盖首次备份"
        );
    }

    #[test]
    fn exists_script_probes_product_json_and_all_targets() {
        let s = exists_script("/srv/root");
        assert!(s.contains("product.json"));
        for (rel, _) in TARGET_SPECS {
            assert!(s.contains(rel), "存在性探测漏了 {rel}");
        }
    }

    /// 真跑一次本地 `tar`：打出来的包能被本机 tar 解回同样的字节，而且**不带扩展属性**。
    /// 回归：`--no-xattrs` 一度放在 `czf` 前面，bsdtar 直接报 `Must specify one of -c, -r, -t, -u, -x`，
    /// 端到端测试是 `#[ignore]` 的没拦住，用户点「安装到远程」才炸出来。
    #[cfg(unix)]
    #[test]
    fn tar_gz_round_trips_nested_paths_without_xattrs() {
        let src = tempfile::tempdir().unwrap();
        let nested = src.path().join("extensions/cursor-agent-host/dist");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("61.js"), b"patched bytes").unwrap();
        std::fs::write(src.path().join("product.json"), b"{}").unwrap();

        let tgz = tar_gz(
            src.path(),
            &[
                "extensions/cursor-agent-host/dist/61.js".into(),
                "product.json".into(),
            ],
        )
        .expect("本地 tar 打包不该失败");
        assert!(tgz.len() > 20, "gzip 流至少有个头");

        let dst = tempfile::tempdir().unwrap();
        let mut x = std::process::Command::new("tar")
            .arg("-x")
            .arg("-z")
            .arg("-f")
            .arg("-")
            .arg("-C")
            .arg(dst.path())
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        std::io::Write::write_all(x.stdin.as_mut().unwrap(), &tgz).unwrap();
        drop(x.stdin.take());
        assert!(x.wait().unwrap().success());
        assert_eq!(
            std::fs::read(dst.path().join("extensions/cursor-agent-host/dist/61.js")).unwrap(),
            b"patched bytes"
        );
        assert_eq!(
            std::fs::read(dst.path().join("product.json")).unwrap(),
            b"{}"
        );

        // 包里不该有 AppleDouble 伴生条目（`._61.js`），那是 xattr 没关掉的痕迹。
        let listing = std::process::Command::new("sh")
            .arg("-c")
            .arg("tar -t -z -f - ")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut t| {
                std::io::Write::write_all(t.stdin.as_mut().unwrap(), &tgz)?;
                drop(t.stdin.take());
                t.wait_with_output()
            })
            .unwrap();
        let names = String::from_utf8_lossy(&listing.stdout);
        assert!(
            !names.contains("._"),
            "包里混进了 AppleDouble 条目：\n{names}"
        );
    }

    #[test]
    fn backup_key_is_stable_per_host_and_commit() {
        assert_eq!(backup_key("box", "c1"), "ssh://box/c1");
        assert_ne!(backup_key("box", "c1"), backup_key("box", "c2"));
    }

    #[test]
    fn probe_result_line_is_picked_out_of_noisy_output() {
        // 远程的 shell profile 常常自己 echo 东西，node 也可能打告警——都要无视。
        let out = "Welcome to devbox-01!\n(node:12) Warning: something\nRESULT {\"ok\":true,\"stage\":\"http\",\"status\":404}\n";
        let r = parse_probe(out, 21890).expect("该认出结论行");
        assert!(r.ok);
        assert_eq!(r.stage, ProbeStage::Http);
        assert_eq!(r.status, Some(404));
        assert_eq!(r.remote_port, 21890);
        // 一行都没有时不能编一个「成功」出来。
        assert!(parse_probe("nothing here\n", 1).is_none());
    }

    #[test]
    fn probe_reports_the_hop_that_broke() {
        for (json, want) in [
            (
                r#"{"ok":false,"stage":"tunnel","detail":"connect ECONNREFUSED 127.0.0.1:21890"}"#,
                ProbeStage::Tunnel,
            ),
            (
                r#"{"ok":false,"stage":"proxy","detail":"代理拒绝 CONNECT：HTTP/1.1 403 Forbidden"}"#,
                ProbeStage::Proxy,
            ),
            (
                r#"{"ok":false,"stage":"tls","detail":"unable to verify the first certificate"}"#,
                ProbeStage::Tls,
            ),
        ] {
            let r = parse_probe(&format!("RESULT {json}\n"), 1).unwrap();
            assert!(!r.ok);
            assert_eq!(r.stage, want, "{json}");
            assert!(r.detail.is_some(), "失败必须带原因：{json}");
        }
    }

    /// 后一次的结论覆盖前一次：脚本里若有重试，最后那行才是答案。
    #[test]
    fn the_last_result_line_wins() {
        let out = "RESULT {\"ok\":false,\"stage\":\"tunnel\"}\nRESULT {\"ok\":true,\"stage\":\"http\",\"status\":200}\n";
        let r = parse_probe(out, 1).unwrap();
        assert!(r.ok);
        assert_eq!(r.status, Some(200));
    }

    #[test]
    fn probe_script_passes_the_port_and_target_by_env_not_argv() {
        // `node -` 的 argv 编号在各版本上不一致，用环境变量最稳。
        let s = probe_script(
            "/srv/root",
            21890,
            &ProbeTarget::Proxy {
                host: "api2.cursor.sh".into(),
            },
        );
        assert!(s.contains("SAND_PORT=21890"));
        assert!(s.contains("SAND_TARGET='api2.cursor.sh'"));
        assert!(s.contains("'/srv/root'"), "root 要被引号包住");
        assert!(s.contains("NEXUSPROBE"), "脚本靠 here-doc 送过去");
        // 网关模式没有 target：脚本里那一格是空串，node 侧据此走直连分支。
        let local = probe_script("/srv/root", 1, &ProbeTarget::Local);
        assert!(local.contains("SAND_TARGET=''"));
    }

    /// 探针的 JS 在**本机 node** 上真跑一遍：连一个我们自己起的假 HTTP 服务，
    /// 必须报 `stage=http` + 真实状态码。回归的是「脚本本身语法 / 逻辑没写错」——
    /// 这段代码只在远程执行，写错了在真机上才炸，最难查。
    #[cfg(unix)]
    #[test]
    fn the_probe_js_really_runs_and_reports_an_http_status() {
        use std::io::{BufRead, BufReader, Write as _};
        let Ok(node) = which_node() else {
            eprintln!("本机没有 node，跳过");
            return;
        };
        // 一个只回 418 的假服务，扮演「隧道那头的本机端口」。
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut sock, _)) = listener.accept() {
                let mut r = BufReader::new(sock.try_clone().unwrap());
                let mut line = String::new();
                let _ = r.read_line(&mut line);
                let _ = sock.write_all(b"HTTP/1.1 418 I'm a teapot\r\nContent-Length: 0\r\n\r\n");
            }
        });

        let out = std::process::Command::new(node)
            .arg("-")
            .env("SAND_PORT", port.to_string())
            .env("SAND_TARGET", "")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                c.stdin.as_mut().unwrap().write_all(PROBE_JS.as_bytes())?;
                drop(c.stdin.take());
                c.wait_with_output()
            })
            .unwrap();
        let _ = server.join();
        let stdout = String::from_utf8_lossy(&out.stdout);
        let r = parse_probe(&stdout, port).unwrap_or_else(|| {
            panic!(
                "探针脚本没给出结论：\nstdout={stdout}\nstderr={}",
                String::from_utf8_lossy(&out.stderr)
            )
        });
        assert!(r.ok, "{r:?}");
        assert_eq!(r.stage, ProbeStage::Http);
        assert_eq!(r.status, Some(418));
    }

    /// 没人监听时必须断在第一跳，而且带上 ECONNREFUSED 这种能直接搜的原文。
    #[cfg(unix)]
    #[test]
    fn the_probe_js_blames_the_tunnel_when_nobody_listens() {
        use std::io::Write as _;
        let Ok(node) = which_node() else { return };
        // 绑一下再放掉，拿一个几乎肯定没人在听的端口号。
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let out = std::process::Command::new(node)
            .arg("-")
            .env("SAND_PORT", port.to_string())
            .env("SAND_TARGET", "")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut c| {
                c.stdin.as_mut().unwrap().write_all(PROBE_JS.as_bytes())?;
                drop(c.stdin.take());
                c.wait_with_output()
            })
            .unwrap();
        let r = parse_probe(&String::from_utf8_lossy(&out.stdout), port).expect("该给出结论");
        assert!(!r.ok);
        assert_eq!(r.stage, ProbeStage::Tunnel);
        assert!(
            r.detail
                .as_deref()
                .unwrap_or_default()
                .contains("ECONNREFUSED"),
            "原因要能直接搜：{:?}",
            r.detail
        );
    }

    #[cfg(unix)]
    fn which_node() -> std::result::Result<std::path::PathBuf, ()> {
        std::process::Command::new("sh")
            .arg("-c")
            .arg("command -v node")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
            .filter(|p| p.exists())
            .ok_or(())
    }

    #[derive(Default)]
    struct FakeSsh;
    impl SshRunner for FakeSsh {
        fn run(&self, _: &str, _: &str, _: Option<&[u8]>) -> Result<SshOutput> {
            Ok(SshOutput {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            })
        }
        fn pull_tar(&self, _: &str, _: &str, _: &[String], _: &Path) -> Result<()> {
            Ok(())
        }
    }
}
