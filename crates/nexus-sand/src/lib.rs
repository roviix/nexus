//! `nexus-sand` —— 给本机 Cursor 打「Sand Stream」补丁，让 IDE Agent 面板走 sand/bot 额度通道。
//!
//! **这是对 ARCHITECTURE §1.2「不做 sand 补丁」的有意反转**，理由与边界写在 `docs/SAND.md`。
//! 一句话：补丁改的是 Cursor 的**代码**（bundle），追着 Cursor 版本跑；切号写的是 Cursor 的
//! **数据**（登录态），升级不失效。两者性质不同，所以是两个 crate、互不依赖，只共享
//! `nexus-cursor` 那层「定位 / 退出 / 启动」。
//!
//! 结构（依赖只向下）：
//!
//! ```text
//! service   编排：预检 → 备份 → 退出 Cursor → 写 → 校验 → 启动；单飞闸
//! remote    同一份规则打远程 ~/.cursor-server：ssh 拉 → 暂存镜像 → 复用下面全部 → 推回；受监管的隧道
//!           （ssh 会话里的多路复用中继，remote/relay.js + remote/tunnel.rs——不是 ssh -R）
//!   ├─ rules      知识：锚点 / 注入体 / marker / 期望命中数（Cursor 升级只改这一层）
//!   │             期望值按 LayoutProfile::{Desktop, Server} 两套走——远程少 4 个 Electron/UI 文件
//!   ├─ engine     引擎：对字符串 apply / remove / inspect（纯函数，不认识具体锚点）
//!   ├─ integrity  Cursor 自己的完整性：扩展内嵌 hash、product.json checksums
//!   ├─ commit     原子写 + 写后校验 + 失败回滚
//!   ├─ backup     改动前字节的快照与清单
//!   └─ layout     目标文件在哪
//! ```
//!
//! remote 那条路的理由与边界在 `docs/SAND.md §9`。一句话：`gateway/scripts/sand-remote-server.py` 曾是另一套
//! 只有 5 类的规则，漏掉的正是 `extensionHostProcess.js` 上的 client-type——第二套规则本身就是那个 bug，
//! 所以远程复用同一份 `rules::catalog`，而不是把脚本集成进来。
//!
//! 移植自 `gateway/scripts/sand-stream-installer.py`（v1.2.6-subagent-lifecycle-fixed.4）。
//! 版本硬绑 Cursor 3.19.13（`model::SUPPORTED_CURSOR_VERSION`）：锚点是压缩后的精确字符串，
//! 版本不等一律拒装，不做模糊匹配。升级 Cursor 时只改 `rules.rs` / `layout::TARGET_SPECS` /
//! 版本与下载 release ID，其它模块不动 —— 这正是 docs/SAND.md §1.2 里「军备竞赛变运营节拍」的落点。

pub mod backup;
pub mod commit;
pub mod engine;
pub mod grokbot;
pub mod integrity;
pub mod layout;
pub mod model;
pub mod remote;
pub mod rules;
pub mod service;

pub use grokbot::{
    BoxRelayDescriptor, DirectCredentialInfo, GrokBotService, GrokBotStatus, RelayInfo,
    StreamCredential,
};
pub use layout::SandLayout;
pub use model::GrokBotAuthMode;
pub use model::{
    supported_cursor_release, CursorArchitecture, CursorDownload, CursorDownloadPlatform,
    CursorRelease, DryRun, InstallOptions, MarkerCounts, ModeGate, Operation, SandBackup,
    SandOutcome, SandProgress, SandStatus, SandStep, SUPPORTED_CURSOR_VERSION,
};
pub use remote::{
    ProbeReport, ProbeStage, ProbeTarget, RemoteOutcome, RemoteSand, RemoteServer, RemoteStatus,
    Tunnel, TunnelPhase, TunnelSpec, TunnelStatus,
};
pub use rules::LayoutProfile;
pub use service::{with_inference_endpoint, SandService};
