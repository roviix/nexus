//! 对外类型。这些结构会原样经 IPC 到前端，所以：全部 `camelCase`、不含任何路径之外的
//! 敏感信息（补丁不涉及凭证，这里没有秘密）、字段只增不改。

use serde::{Deserialize, Serialize};

/// 本工具适配的 Cursor 版本。锚点是压缩后的精确字符串，**版本不等就拒装**，不做模糊匹配
/// —— 锚点错配写坏 bundle 比不装糟糕得多（docs/SAND.md §5）。
pub const SUPPORTED_CURSOR_VERSION: &str = "3.21.13";

/// `downloads.cursor.com/production/<id>/...` 里的不可变发行 ID。
///
/// 不能在界面上链接 `/api/download?...releaseTrack=stable`：stable 会在 Cursor 发版后漂到新版本，
/// 而 Sand 仍只认 [`SUPPORTED_CURSOR_VERSION`]。升级补丁版本时必须从 Cursor 官方下载 API 重新取
/// 这一项，并对下面生成的每个链接做一次 HEAD 校验。
/// 官方下载直链用的 release id。**注意它不等于 `product.json` 的 `commit`**：两者只差最后一位
/// （3.21.13 下载 `…f014a2` / commit `…f014a0`；3.19.13 下载 `…648d5` / commit `…648d0`；
/// 3.19.7 下载 `…228e6437` / commit `…228e6430`）。差的那位没有规律，升级时把 commit 的末位
/// 换成 16 个十六进制字符逐个 HEAD 探一遍即可，命中的那个返回 200、其余 403。
/// 拿 commit 拼下载链会 403，别用 bundle 里的 sourcemap 路径来"订正"这个常量。
const SUPPORTED_CURSOR_RELEASE_ID: &str = "e44a49c17e334d442e58bbde931d791200f014a2";
const CURSOR_DOWNLOAD_BASE_URL: &str = "https://downloads.cursor.com/production";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorDownloadPlatform {
    Macos,
    Windows,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorArchitecture {
    Universal,
    X64,
    Arm64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorDownload {
    pub architecture: CursorArchitecture,
    /// Cursor 官方 CDN 的不可变直链，不经过会漂移的 `stable` 下载 API。
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CursorRelease {
    pub version: String,
    pub platform: CursorDownloadPlatform,
    pub downloads: Vec<CursorDownload>,
}

/// Sand 当前适配版本在指定桌面平台上的官方安装包。
///
/// macOS 给 Universal 包，避免前端猜 Apple Silicon / Intel；Windows 给默认的 per-user 安装器，
/// 不要求管理员权限；Linux 给不绑定发行版的两种架构 AppImage。
pub fn supported_cursor_release(platform: CursorDownloadPlatform) -> CursorRelease {
    let downloads = match platform {
        CursorDownloadPlatform::Macos => vec![cursor_download(
            CursorArchitecture::Universal,
            "darwin/universal/Cursor-darwin-universal.dmg".to_string(),
        )],
        CursorDownloadPlatform::Windows => vec![
            cursor_download(
                CursorArchitecture::X64,
                format!("win32/x64/user-setup/CursorUserSetup-x64-{SUPPORTED_CURSOR_VERSION}.exe"),
            ),
            cursor_download(
                CursorArchitecture::Arm64,
                format!(
                    "win32/arm64/user-setup/CursorUserSetup-arm64-{SUPPORTED_CURSOR_VERSION}.exe"
                ),
            ),
        ],
        CursorDownloadPlatform::Linux => vec![
            cursor_download(
                CursorArchitecture::X64,
                format!("linux/x64/Cursor-{SUPPORTED_CURSOR_VERSION}-x86_64.AppImage"),
            ),
            cursor_download(
                CursorArchitecture::Arm64,
                format!("linux/arm64/Cursor-{SUPPORTED_CURSOR_VERSION}-aarch64.AppImage"),
            ),
        ],
    };

    CursorRelease {
        version: SUPPORTED_CURSOR_VERSION.to_string(),
        platform,
        downloads,
    }
}

fn cursor_download(architecture: CursorArchitecture, relative_path: String) -> CursorDownload {
    CursorDownload {
        architecture,
        url: format!("{CURSOR_DOWNLOAD_BASE_URL}/{SUPPORTED_CURSOR_RELEASE_ID}/{relative_path}"),
    }
}

/// 模式放行档位（对应安装器的 SAND_ENABLE_PLAN_MODE / SAND_ENABLE_ALL_MODES）。
///
/// **默认是 `All`**，这和上游安装器不同，理由在补丁自身：我们把「本地循环处理不了就退回云端」
/// 那条路封了（`managed_local_route_patched` 里 `if(!1)return{runtime:"connect"}`），所以闸门拦下来的
/// turn 不是绕道云端，而是 `{runtime:"fail"}` 硬失败。档位收窄在原版里只是"这类 turn 走云端"，
/// 在补丁下却等于"这类 turn 直接报错"——`cursor-guide` 子代理就是这么挂的
/// （`Local loop cannot run this turn: mode-not-supported`）。
///
/// 收窄仍然留着，给愿意用"Agent 之外的模式宁可不工作、也不要走本地循环"换稳妥的人。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeGate {
    /// 只放 AGENT（与上游安装器逐字节一致）。别的模式会硬失败，不会退回云端。
    Agent,
    /// AGENT + PLAN。Plan 能走 sand、能出规划文字；落 .plan.md 不保证。
    AgentPlan,
    /// 不按模式拦截（含 Ask / Debug / Multitask）。已知边界：managed-local 主循环的 query 处理器
    /// 只执行 web_search / web_fetch / generate_image，`create_plan` 这类交互查询不被执行。
    #[default]
    All,
}

/// 安装选项。默认值：自动摘要开、只放 Agent、装完重启 Cursor
/// （与 Python 安装器一致；v1.2.6.7 起自动摘要默认开，此前默认关）。
///
/// 推理引擎只剩 Direct（劫持 `cursor-agent-host/dist/4883.js` 的 attempt 工厂直连
/// `InferenceService/Stream`，docs/SAND.md §10）。曾经的 Session 引擎（走官方 `RunInference`）
/// 被服务端对 sand 身份封掉（"Sand traffic is not supported on this endpoint"），2026-09-04 下线；
/// 它留在盘上的空 marker 由 install 原地迁成 Direct、uninstall 剥掉、status 计入 `legacy_markers`。
/// 旧前端传来的 `streamEngine` 字段会被 serde 忽略。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InstallOptions {
    /// Cursor 原生上下文自动摘要（注入的 supportsSelfSummary !0 / !1）。
    ///
    /// 默认开。Stream 模式下后台外部摘要因 managed-local 没配 `backgroundSummarizationProps`
    /// 阈值而永远不会主动触发，自摘要是**唯一**能在撞上限前压缩历史的机制：用量到 90% 后台
    /// 生成摘要，agentic 循环下一步落盘替换旧消息。关掉后长会话到上限只会一直
    /// `InputTokenLimitError`，撞墙后的阻塞摘要把同一份超限对话再发一遍也必然失败，只能手动
    /// Summarize 或开新会话。开着的下行风险小：sand 下摘要请求若失败，Background 模式丢弃结果、
    /// 会话照常继续。
    pub self_summary: bool,
    pub mode_gate: ModeGate,
    /// 写完后是否重新启动 Cursor。
    pub relaunch: bool,
    /// 把 `InferenceService` 改道到这个地址（`http://127.0.0.1:<port>`）。
    ///
    /// **产品里不再有这个开关。** 它曾经服务两件事：本机「推理经本机网关」（让网关透传口拦截面板
    /// 流量）和远程「经本机网关」出网；网关的透传口 2026-09 整条拆掉之后，两条路都没了。
    /// 字段留着有两个理由：① 老机器盘上还装着改道，`SandService::install` / `uninstall` 靠
    /// 同一条规则把它认出来并剥掉（见 `rules::installed_inference_endpoint`）；② 研究用的
    /// `examples/install_local` 仍可以把面板流量改道到一个本地抓包口。应用层永远传 `None`。
    pub inference_endpoint: Option<String>,
    /// `InferenceService/Stream` 用 Grok Bot 额度鉴权的方式（见 [`GrokBotAuthMode`]）。
    ///
    /// 旧前端传的布尔 `grokbotStreamAuth` 会被 serde 忽略（字段名不同），落到默认 `BoxRelay`。
    pub grokbot_auth: GrokBotAuthMode,
    /// Agent 面板选 grok-4.5 时改走 `sand-cua`。默认关：4.5 是 Bot 通道上各号都能直打的
    /// 稳模型；`sand-cua` 按账号分片，只有部分号落到 grok-4.7，其余仍是 luna。
    /// 开了需重新安装补丁（注入体变了）。Bot 通道关着时这项写不进补丁。
    pub grok45_via_cua: bool,
}

/// Agent 面板的 Stream 请求用 Grok Bot 额度，三种落法。三种都改同一处
/// （`TransportFactory.applyAuthorization` 的开头），互为变体，install 可原地切换。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GrokBotAuthMode {
    /// 不动鉴权：Stream 用 Cursor 自己登着的号。**这一档在今天的服务端上不能用**——`sand` 头配
    /// 会话 JWT 一律 401（Connect 16），它只在卸载 / 剥掉 Grok 鉴权块时作为「目标形态」出现，
    /// 界面上不再提供。
    Off,
    /// Stream 改道到 Grok Bot Box 内的 relay，token 留在 Box（v135 社区脚本同款）。
    /// 依赖 pod 在线 + Bot 端装过 relay。
    #[default]
    BoxRelay,
    /// 直连 api2：注入体读本机 `grokbot-stream-credential.json`，Bearer 换 grokBotToken，
    /// 快过期时自己拿 `sbi_*` 续期。不依赖 Bot 端改动；凭证由 Nexus（`nexus-grokbot`）生成。
    Direct,
}

impl GrokBotAuthMode {
    pub fn as_str(self) -> &'static str {
        match self {
            GrokBotAuthMode::Off => "off",
            GrokBotAuthMode::BoxRelay => "box_relay",
            GrokBotAuthMode::Direct => "direct",
        }
    }

    pub fn is_on(self) -> bool {
        self != GrokBotAuthMode::Off
    }
}

impl Default for InstallOptions {
    fn default() -> Self {
        Self {
            self_summary: true,
            mode_gate: ModeGate::Agent,
            relaunch: true,
            inference_endpoint: None,
            grokbot_auth: GrokBotAuthMode::default(),
            grok45_via_cua: false,
        }
    }
}

/// 各类补丁 marker 的计数。既用于「已装了什么」（inspect），也用于「装了会命中什么」（dry-run）。
/// 字段名与安装器 `PatchStatus` 一一对应，方便和 Python 版对账。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkerCounts {
    pub client_type: u32,
    pub eligibility: u32,
    pub managed_local_route: u32,
    pub local_runtime_load: u32,
    /// 推理引擎（Direct 注入体）的 marker，应为 1。已下线的 Session 空 marker 不计在这里，
    /// 计入 `FileInspection.legacy`。
    pub inference_stream: u32,
    pub agent_host_enablement: u32,
    pub agent_host_identity: u32,
    pub agent_host_move_exec: u32,
    pub managed_subagent_route: u32,
    pub managed_subagent_session: u32,
    pub managed_task_tool: u32,
    pub managed_action_route: u32,
    pub subagent_resume_mode: u32,
    pub subagent_completion_wake: u32,
    pub subagent_interaction_bubble: u32,
    pub subagent_model_variants: u32,
    pub context_window: u32,
    /// 推理端点改道（可选项，两处：建 transport + 挂路由）。
    pub inference_endpoint: u32,
    /// grokBot Stream 鉴权 interceptor（`originTransport` 一处）。
    pub grokbot_stream_auth: u32,
}

impl MarkerCounts {
    pub fn total(&self) -> u32 {
        self.client_type
            + self.eligibility
            + self.managed_local_route
            + self.local_runtime_load
            + self.inference_stream
            + self.agent_host_enablement
            + self.agent_host_identity
            + self.agent_host_move_exec
            + self.managed_subagent_route
            + self.managed_subagent_session
            + self.managed_task_tool
            + self.managed_action_route
            + self.subagent_resume_mode
            + self.subagent_completion_wake
            + self.subagent_interaction_bubble
            + self.subagent_model_variants
            + self.context_window
            + self.inference_endpoint
            + self.grokbot_stream_auth
    }

    /// 逐项相加。用于「已装 marker + 待打锚点」合计判断（避免在已装机器上误报锚点不齐）。
    pub fn plus(&self, other: &MarkerCounts) -> MarkerCounts {
        MarkerCounts {
            client_type: self.client_type + other.client_type,
            eligibility: self.eligibility + other.eligibility,
            managed_local_route: self.managed_local_route + other.managed_local_route,
            local_runtime_load: self.local_runtime_load + other.local_runtime_load,
            inference_stream: self.inference_stream + other.inference_stream,
            agent_host_enablement: self.agent_host_enablement + other.agent_host_enablement,
            agent_host_identity: self.agent_host_identity + other.agent_host_identity,
            agent_host_move_exec: self.agent_host_move_exec + other.agent_host_move_exec,
            managed_subagent_route: self.managed_subagent_route + other.managed_subagent_route,
            managed_subagent_session: self.managed_subagent_session
                + other.managed_subagent_session,
            managed_task_tool: self.managed_task_tool + other.managed_task_tool,
            managed_action_route: self.managed_action_route + other.managed_action_route,
            subagent_resume_mode: self.subagent_resume_mode + other.subagent_resume_mode,
            subagent_completion_wake: self.subagent_completion_wake
                + other.subagent_completion_wake,
            subagent_interaction_bubble: self.subagent_interaction_bubble
                + other.subagent_interaction_bubble,
            subagent_model_variants: self.subagent_model_variants + other.subagent_model_variants,
            inference_endpoint: self.inference_endpoint + other.inference_endpoint,
            grokbot_stream_auth: self.grokbot_stream_auth + other.grokbot_stream_auth,
            context_window: self.context_window + other.context_window,
        }
    }
}

/// 只读检查结果。前端「Sand」页顶部那张卡全靠它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandStatus {
    /// 本机 Cursor 版本（product.json）。找不到时为 `None`。
    pub cursor_version: Option<String>,
    pub supported_version: String,
    /// 版本精确匹配才为真。为假时 install 一定被拒。
    pub version_supported: bool,
    /// 任意 marker > 0。
    pub installed: bool,
    /// 全部必需 marker 都到位（等价于安装器 `stream_mode_installed`）。
    pub complete: bool,
    /// 已装 marker 计数。
    pub markers: MarkerCounts,
    /// 残留的原版 `"ide"` 命中数（装完应为 0）。
    pub remaining_ide: u32,
    /// 其它同类工具留下的 marker。>0 时拒绝接管，提示先用原工具卸载。
    pub foreign_markers: u32,
    /// 旧版本 marker（V1–V6 task tool、已下线的 Session 引擎空 marker 等），install 会自动迁移。
    pub legacy_markers: u32,
    /// 已被改过的目标文件（相对 app 根）。
    pub patched_files: Vec<String>,
    /// 未完整安装时给的预演：若 install 会命中什么。已完整安装时为 `None`。
    pub dry_run: Option<DryRun>,
    pub backups: u32,
    /// 盘上 Direct 注入体里自动摘要开关的实际取值；没装 Direct 注入体时为 `None`。
    /// 这是「盘上是什么」，界面上选的是「要装什么」——两者不同时 install 会原地切换。
    pub self_summary: Option<bool>,
    /// 盘上装着的推理端点改道地址；没改道为 `None`。产品里已经没有这个开关，所以 `Some` 只剩
    /// 一种含义：早期版本留下的改道还在，指着一个已经不存在的本机端口——界面要把它当问题报出来，
    /// 重新安装会把那两处剥掉。
    pub inference_endpoint: Option<String>,
    /// 盘上装着的 Grok Bot 鉴权形态；没装为 `Off`。同样是「盘上是什么」，界面选的与它不同时
    /// install 原地切换。
    pub grokbot_auth: GrokBotAuthMode,
    /// 盘上 Direct 注入体是否把 grok-4.5 改走 `sand-cua`；没装 Direct 时为 `None`。
    pub grok45_via_cua: Option<bool>,
    /// 本地 `grok-box-relay.json` 是否已就绪（Box Relay 模式的前提）。
    pub grokbot_relay_configured: bool,
    /// 本地 `grokbot-stream-credential.json` 是否存在且未过期 / 可续期（Direct 模式的前提）。
    pub grokbot_direct_configured: bool,
}

/// dry-run：仅内存计算，不写盘。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DryRun {
    pub would_hit: MarkerCounts,
    pub files_to_change: u32,
    /// 「已装 + 待打」合计是否达到全部必需锚点数。为假则 install 会中止。
    pub anchors_complete: bool,
    /// 未达标的锚点名（给用户看「差哪个」）。
    pub missing: Vec<String>,
}

/// 安装 / 卸载的步骤。顺序是硬约束：预检 → 备份 → 退出 Cursor → 写入 → 校验 → 启动。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandStep {
    Preflight,
    Backup,
    QuitCursor,
    Write,
    Verify,
    Launch,
    Done,
}

/// 进度事件载荷（`sand://progress`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandProgress {
    pub step: SandStep,
    pub detail: String,
}

impl SandProgress {
    pub fn new(step: SandStep, detail: impl Into<String>) -> Self {
        Self {
            step,
            detail: detail.into(),
        }
    }
}

/// 安装 / 卸载的结果。
///
/// `wrote` 决定前端怎么措辞：写盘之前失败可以说「Cursor 没有被改动」；写盘之后就不能
/// （那时 bundle 已经是新的了，即便随后回滚）。这和切号的 `wrote_auth` 是同一条规矩。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandOutcome {
    pub operation: Operation,
    /// 是否真的改了文件。「已经是目标状态、无需改动」时为假。
    pub wrote: bool,
    pub files_written: u32,
    /// 本次操作前创建的备份。无改动时为 `None`。
    pub backup_id: Option<String>,
    pub cursor_relaunched: bool,
    /// 装完后的状态快照。
    pub status: SandStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Install,
    Uninstall,
    /// 从备份把原始字节写回。
    Restore,
}

impl Operation {
    pub fn as_str(self) -> &'static str {
        match self {
            Operation::Install => "install",
            Operation::Uninstall => "uninstall",
            Operation::Restore => "restore",
        }
    }
}

/// 备份快照。备份的是被改动文件**改动前**的完整字节，放应用数据目录下（不进库——
/// 这里没有秘密，而且单个 bundle 有几 MB）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandBackup {
    pub id: String,
    pub created_at: String,
    pub operation: Operation,
    pub cursor_version: String,
    pub files: u32,
    /// `prepared` / `committed` / `rolled_back`。
    pub state: String,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_options_default_has_self_summary_on() {
        let o = InstallOptions::default();
        assert!(
            o.self_summary,
            "自摘要是 Direct 模式唯一的撞墙前压缩机制，默认必须开"
        );
        assert_eq!(o.mode_gate, ModeGate::Agent);
        assert!(o.relaunch);
        assert_eq!(o.inference_endpoint, None);
        assert!(
            !o.grok45_via_cua,
            "4.5 走 CUA 会让多数号丢掉稳的 grok-4.5，默认必须关"
        );
    }

    #[test]
    fn install_options_deserialize_with_partial_fields() {
        // 前端可能只传一个字段；其余取默认。
        let o: InstallOptions = serde_json::from_str(r#"{"modeGate":"all"}"#).unwrap();
        assert_eq!(o.mode_gate, ModeGate::All);
        assert!(o.self_summary);
        assert!(o.relaunch);
        // 显式关掉要能关。
        let o: InstallOptions = serde_json::from_str(r#"{"selfSummary":false}"#).unwrap();
        assert!(!o.self_summary);
        // 老前端还会传已下线的 streamEngine：忽略，不报错。
        let o: InstallOptions =
            serde_json::from_str(r#"{"streamEngine":"session","selfSummary":false}"#).unwrap();
        assert!(!o.self_summary);
        assert!(!o.grok45_via_cua);
        let o: InstallOptions = serde_json::from_str(r#"{"grok45ViaCua":true}"#).unwrap();
        assert!(o.grok45_via_cua);
        assert!(o.self_summary);
    }

    #[test]
    fn supported_cursor_release_uses_immutable_official_downloads() {
        let releases = [
            supported_cursor_release(CursorDownloadPlatform::Macos),
            supported_cursor_release(CursorDownloadPlatform::Windows),
            supported_cursor_release(CursorDownloadPlatform::Linux),
        ];

        for release in &releases {
            assert_eq!(release.version, SUPPORTED_CURSOR_VERSION);
            assert!(!release.downloads.is_empty());
            for download in &release.downloads {
                assert!(download.url.starts_with(CURSOR_DOWNLOAD_BASE_URL));
                assert!(download.url.contains(SUPPORTED_CURSOR_RELEASE_ID));
                assert!(!download.url.contains("releaseTrack=stable"));
            }
        }

        assert_eq!(
            releases[0]
                .downloads
                .iter()
                .map(|download| download.architecture)
                .collect::<Vec<_>>(),
            vec![CursorArchitecture::Universal]
        );
        for release in &releases[1..] {
            assert_eq!(
                release
                    .downloads
                    .iter()
                    .map(|download| download.architecture)
                    .collect::<Vec<_>>(),
                vec![CursorArchitecture::X64, CursorArchitecture::Arm64]
            );
            assert!(release
                .downloads
                .iter()
                .all(|download| download.url.contains(SUPPORTED_CURSOR_VERSION)));
        }
    }

    #[test]
    fn serializes_camel_case_for_the_frontend() {
        let p = SandProgress::new(SandStep::QuitCursor, "正在退出 Cursor");
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["step"], "quit_cursor");
        assert_eq!(v["detail"], "正在退出 Cursor");
        let c = MarkerCounts {
            managed_local_route: 1,
            ..Default::default()
        };
        let v = serde_json::to_value(c).unwrap();
        assert_eq!(v["managedLocalRoute"], 1);
    }

    #[test]
    fn marker_counts_add_field_by_field() {
        let a = MarkerCounts {
            client_type: 20,
            inference_stream: 1,
            ..Default::default()
        };
        let b = MarkerCounts {
            client_type: 3,
            context_window: 1,
            ..Default::default()
        };
        let s = a.plus(&b);
        assert_eq!(s.client_type, 23);
        assert_eq!(s.inference_stream, 1);
        assert_eq!(s.context_window, 1);
        assert_eq!(s.total(), 25);
    }
}
