//! 补丁**知识**：每一类补丁认什么锚点、改成什么、留什么 marker、期望命中几次。
//!
//! 这里只有数据和它的形状，没有「怎么改文件」——那在 `engine`。分开的理由写在
//! docs/SAND.md §3：Cursor 升级时要改的只是这一份表，引擎一行不动；将来若要把表挪到云端，
//! 引擎也不用重写。
//!
//! 一条规则 = 一个 `PatchRule`。同一个 `RuleId` 可以对应多条（client-type 有三条正则、
//! 一个共用计数器），引擎按 `RuleId` 汇总。
//!
//! **移植来源**：`gateway/scripts/sand-stream-installer.py`（v1.2.6-subagent-lifecycle-fixed.6，
//! 对应 Cursor 3.18.25）。每个 `RuleId` 的文档里标了对应的 Python 常量名，移植时逐条对照
//! **当前版本的 Python 文件**，不要凭记忆 —— 压缩后的符号名（`In` / `xre` / `gre` / `tre` /
//! `are` / `Bn`…）每个 Cursor 版本都会变，只有 marker 和期望计数是稳定的。

use nexus_core::{AppError, Result};
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};

use crate::model::{GrokBotAuthMode, InstallOptions, MarkerCounts, ModeGate};

// ---------------------------------------------------------------------------
// marker 常量（与 Python 版逐字一致；这些字符串会被写进用户的 bundle，改一个字就认不出旧安装）
// ---------------------------------------------------------------------------

pub const SAND_CLIENT_MARKER: &str = "/*SAND_CLIENT_MODE_V1*/";
pub const SAND_CLIENT_EXISTING_MARKER: &str = "/*SAND_CLIENT_EXISTING_V1*/";
pub const SAND_ELIGIBILITY_MARKER: &str = "/*SAND_ELIGIBILITY_MODE_V1*/";
pub const SAND_MANAGED_LOCAL_ROUTE_MARKER: &str = "/*SAND_MANAGED_LOCAL_ROUTE_V1*/";
pub const SAND_DIRECT_STREAM_MARKER: &str = "/*SAND_DIRECT_INFERENCE_STREAM_V1*/";
/// **已下线**的 Session 引擎留下的 marker：锚点后**只有这一个注释**，不带任何逻辑，让 Cursor
/// 自己的 `RunInference` 路径原样跑。2026-09-04 起服务端对 sand 身份直接拒掉该端点
/// （"Sand traffic is not supported on this endpoint"），所以它不再是可选项：install 见到就原地
/// 换成 Direct 注入体，uninstall 剥掉，inspect 计入 `legacy`。字符串与同类工具
/// （v1.2.7 "session-stream"）逐字一致——那类工具装过的机器要认成自己人、能精确迁移 / 卸载。
pub const LEGACY_SESSION_STREAM_MARKER: &str = "/*SAND_SESSION_INFERENCE_STREAM_V1*/";
pub const SAND_AGENT_HOST_ENABLEMENT_MARKER: &str = "/*SAND_AGENT_HOST_ENABLEMENT_V1*/";
pub const SAND_LOCAL_RUNTIME_LOAD_MARKER: &str = "/*SAND_LOCAL_RUNTIME_LOAD_V1*/";
pub const SAND_AGENT_HOST_IDENTITY_MARKER: &str = "/*SAND_AGENT_HOST_IDENTITY_V1*/";
pub const SAND_AGENT_HOST_MOVE_EXEC_MARKER: &str = "/*SAND_AGENT_HOST_MOVE_EXEC_V1*/";
pub const SAND_MANAGED_SUBAGENT_ROUTE_MARKER: &str = "/*SAND_MANAGED_SUBAGENT_ROUTE_V1*/";
pub const SAND_MANAGED_SUBAGENT_SESSION_MARKER: &str = "/*SAND_MANAGED_SUBAGENT_SESSION_V1*/";
pub const SAND_MANAGED_TASK_TOOL_MARKER: &str = "/*SAND_MANAGED_TASK_TOOL_V7*/";
pub const SAND_MANAGED_ACTION_ROUTE_MARKER: &str = "/*SAND_MANAGED_ACTION_ROUTE_V1*/";
pub const SAND_SUBAGENT_RESUME_MODE_MARKER: &str = "/*SAND_SUBAGENT_RESUME_AGENT_MODE_V1*/";
pub const SAND_SUBAGENT_COMPLETION_WAKE_MARKER: &str = "/*SAND_SUBAGENT_COMPLETION_WAKE_V1*/";
pub const SAND_SUBAGENT_INTERACTION_BUBBLE_MARKER: &str = "/*SAND_SUBAGENT_INTERACTION_BUBBLE_V1*/";
pub const SAND_SUBAGENT_MODEL_VARIANTS_MARKER: &str = "/*SAND_SUBAGENT_MODEL_VARIANTS_V1*/";
pub const SAND_CONTEXT_WINDOW_MARKER: &str = "/*SAND_CONTEXT_WINDOW_V1*/";

/// 旧版 marker。install 见到就迁移成当前版；inspect 计入 `legacy_markers`。
pub const LEGACY_TASK_TOOL_MARKERS: &[&str] = &[
    "/*SAND_MANAGED_TASK_TOOL_V1*/",
    "/*SAND_MANAGED_TASK_TOOL_V2*/",
    "/*SAND_MANAGED_TASK_TOOL_V3*/",
    "/*SAND_MANAGED_TASK_TOOL_V4*/",
    "/*SAND_MANAGED_TASK_TOOL_V5*/",
    "/*SAND_MANAGED_TASK_TOOL_V6*/",
];
/// 推理端点改道的两个 marker。**故意与 `gateway/scripts/sand-remote-server.py` 用同一串**：
/// 那个脚本装过的远程，这边要能认出来是自己人（而不是「外部工具 marker」）并正常卸载。
pub const SAND_INFERENCE_ENDPOINT_MARKER: &str = "/*SAND_INFERENCE_ENDPOINT_V1*/";
pub const SAND_REMOTE_INFERENCE_ROUTE_MARKER: &str = "/*SAND_REMOTE_INFERENCE_ROUTE_V1*/";
/// Grok Bot 鉴权的两种形态各一个 marker，都落在 `applyAuthorization` 开头。
pub const SAND_GROK_BOX_RELAY_AUTH_MARKER: &str = "/*SAND_GROK_BOX_RELAY_AUTH_V1*/";
pub const SAND_GROKBOT_DIRECT_AUTH_MARKER: &str = "/*SAND_GROKBOT_DIRECT_AUTH_V1*/";
/// 第一版直连（挂在 `originTransport` 的 `interceptors`，实测 `createTransport` 根本不读这个
/// 字段，从未生效）。install 见到就迁走，uninstall 认得。
pub const LEGACY_SAND_GROKBOT_STREAM_AUTH_MARKER: &str = "/*SAND_GROKBOT_STREAM_AUTH_V1*/";

/// 更早的另一个工具留下的前缀（`KC_`）。拆成两段拼是为了别让本文件自己被当成外部 marker。
pub const LEGACY_KC_CLIENT_MARKER: &str = "/*KC_SAND_CLIENT_V1*/";
pub const LEGACY_KC_ELIGIBILITY_MARKER: &str = "/*KC_SAND_ELIGIBILITY_V1*/";

/// 外部工具 marker 探测：凡是形如 `/*XXX_SAND_CLIENT…_V1*/` 又不是我们的，就算「外部」。
/// 见到外部 marker 一律拒绝接管（`ErrorCode::SandForeignMarkers`）。
pub const CLIENT_MARKER_GUARD_PATTERN: &str =
    r"/\*[A-Z0-9_]*SAND_CLIENT(?:_(?:MODE|EXISTING))?_V1\*/";
pub const ELIGIBILITY_MARKER_GUARD_PATTERN: &str = r"/\*[A-Z0-9_]*SAND_ELIGIBILITY(?:_MODE)?_V1\*/";

/// client-type 锚点的总命中数（isGlass 16 + 对象头 3 + header.set 4）。3.18.9 与 3.18.25 相同。
pub const EXPECTED_CLIENT_MARKERS: u32 = 23;
/// 子代理模型变体：workbench.desktop.main.js 与 workbench.glass.main.js 各一处 `rRf()`。
pub const EXPECTED_SUBAGENT_MODEL_VARIANTS_MARKERS: u32 = 2;

/// remote server 上 client-type 的命中数：只剩 `extensionHostProcess.js` 里那两处。
///
/// 这个 2 不是推的，是拿一份原版 remote bundle 用 `examples/profile_probe` 量出来的。它同时是
/// remote sand 能不能成立的关键——`x-cursor-client-type` 就出在这个文件里（`_??"ide"`），
/// `gateway/scripts/sand-remote-server.py` 从来不碰它，所以远程一直以 `ide` 身份发请求、
/// 记在账号自己的额度上。走统一规则表之后远程自己就是 sand 身份了。
pub const EXPECTED_SERVER_CLIENT_MARKERS: u32 = 2;

/// 打补丁的对象是哪一种 Cursor 安装。
///
/// 决定**期望命中数**：`layout::TARGET_SPECS` 里的 11 个文件，remote server 上只有 7 个
/// （少了 `out/main.js`、`workbench.desktop.main.js`、`workbench.glass.main.js`、
/// `extensionHostWorkerMain.js` —— 都是 Electron/UI 侧的，只存在于本机）。少了文件，
/// 若干类规则的命中数自然不同，硬校验不能用同一套数字，否则远程永远「锚点不齐」。
///
/// 期望值宁可写死也不「按实际命中算」：后者会让版本护栏退化成恒真。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LayoutProfile {
    /// 本机 Cursor.app / Windows 安装目录，11 个目标齐全。
    Desktop,
    /// 远程 `~/.cursor-server/bin/<arch>/<commit>`，只有 7 个目标。
    Server,
}

// ---------------------------------------------------------------------------
// 规则的形状
// ---------------------------------------------------------------------------

/// 补丁类别。一个类别 = `MarkerCounts` 里的一个字段 = 界面上的一行。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleId {
    /// `x-cursor-client-type: ide → sand`。Python：`CLIENT_RULES`（三条正则）+ legacy KC 迁移。
    ClientType,
    /// `adminSettingsService` 资格函数 `return!1`。Python：`ELIGIBILITY_PREFIXES`。近期版本命中 0，不进硬校验。
    Eligibility,
    /// 路由恒 `managed-local`。Python：`MANAGED_LOCAL_ROUTE_*`。
    ManagedLocalRoute,
    /// 强制加载本机 runtime。Python：`LOCAL_RUNTIME_LOAD_*`。
    LocalRuntimeLoad,
    /// 推理引擎（Direct），落在 attempt 工厂（3.19.7 叫 `ve`）的锚点上：注入体劫持工厂直连
    /// `InferenceService/Stream`（Python：`DIRECT_STREAM_ANCHOR` + `_direct_stream_injection`；
    /// 随 self_summary / context_window / 注入体形态 / premium 钉法 / resolved 日志通道 /
    /// GlmOnly 四代钉法（只钉 GLM / 再加 4.7→CUA / 再加 4.5→CUA / 4.5 带 remap 日志）共 108 种变体）。
    ///
    /// marker 期望 1；install 见到其余变体或已下线 Session 引擎的空 marker
    /// （[`LEGACY_SESSION_STREAM_MARKER`]）就原地换成当前形态，uninstall 全部都认。
    InferenceStream,
    /// `_agentHostEnabled=!0`。Python：`AGENT_HOST_ENABLEMENT_RE` / `_PATCH_RE`（每文件最多 1 处，总 2）。
    AgentHostEnablement,
    /// `clientIdentity:{clientType:"sand"}`。Python：`AGENT_HOST_IDENTITY_*`。
    AgentHostIdentity,
    /// `move_exec` gate 钉 true。Python：`AGENT_HOST_MOVE_EXEC_*`。
    AgentHostMoveExec,
    /// 子代理不再算 unsupported run option。Python：`MANAGED_SUBAGENT_ROUTE_*`。
    ManagedSubagentRoute,
    /// managed-local 的 featureFlags 对象（3.18.25 叫 `xre`）加 useClientSideSubagent + enableExploreSubagent。
    /// Python：`MANAGED_SUBAGENT_SESSION_*`（legacy：`_PATCHED_V1` 只有前一个 flag）。
    ManagedSubagentSession,
    /// Task 工具 props（模型目录 / 自定义子代理直通）。Python：`_managed_task_tool_patched*`
    /// （legacy：v124 / v125 / v2 / v3 四种）。
    ManagedTaskTool,
    /// action 白名单 + 模式放行档位。Python：`_managed_action_route_patched(level)`（三档互为变体）。
    ManagedActionRoute,
    /// resume 归一为 AGENT。Python：`SUBAGENT_RESUME_MODE_*`。
    SubagentResumeMode,
    /// 后台子代理完成唤醒。Python：`SUBAGENT_COMPLETION_WAKE_RE` / `_PATCH_RE`（总 2）。
    SubagentCompletionWake,
    /// 子代理交互策略默认 BUBBLE_TO_PARENT。Python：`SUBAGENT_INTERACTION_BUBBLE_*`。
    SubagentInteractionBubble,
    /// workbench 把每个模型的 legacySlugs（fast / max / thinking / effort 变体）也下发进
    /// selectedSubagentModels，子代理模型目录随之含全部变体。
    /// Python：`SUBAGENT_MODEL_VARIANTS_RE` / `_PATCH_RE`（desktop + glass 共 2）。
    SubagentModelVariants,
    /// `context` 参数覆盖 maxTokens。Python：`MAX_TOKENS_*`。
    ContextWindow,
    /// 把 `InferenceService` 改道到本地端点（remote 专用，两处改动）。
    /// Python 对应物在 `gateway/scripts/sand-remote-server.py`，不在 desktop 安装器里。
    InferenceEndpoint,
    /// `applyAuthorization` 里把 Stream 改道 Grok Bot Box relay（token 留在 Box 内）。
    GrokBotStreamAuth,
}

impl RuleId {
    pub const ALL: &'static [RuleId] = &[
        RuleId::ClientType,
        RuleId::Eligibility,
        RuleId::ManagedLocalRoute,
        RuleId::LocalRuntimeLoad,
        RuleId::InferenceStream,
        RuleId::AgentHostEnablement,
        RuleId::AgentHostIdentity,
        RuleId::AgentHostMoveExec,
        RuleId::ManagedSubagentRoute,
        RuleId::ManagedSubagentSession,
        RuleId::ManagedTaskTool,
        RuleId::ManagedActionRoute,
        RuleId::SubagentResumeMode,
        RuleId::SubagentCompletionWake,
        RuleId::SubagentInteractionBubble,
        RuleId::SubagentModelVariants,
        RuleId::ContextWindow,
        RuleId::InferenceEndpoint,
        RuleId::GrokBotStreamAuth,
    ];

    /// 界面 / 日志 / dry-run `missing` 列表里用的名字。
    pub fn name(self) -> &'static str {
        match self {
            RuleId::ClientType => "client-type",
            RuleId::Eligibility => "eligibility",
            RuleId::ManagedLocalRoute => "managed-local route",
            RuleId::LocalRuntimeLoad => "local runtime load",
            RuleId::InferenceStream => "inference stream",
            RuleId::AgentHostEnablement => "agent host enable",
            RuleId::AgentHostIdentity => "agent host identity",
            RuleId::AgentHostMoveExec => "move_exec",
            RuleId::ManagedSubagentRoute => "subagent route",
            RuleId::ManagedSubagentSession => "subagent session",
            RuleId::ManagedTaskTool => "task tool",
            RuleId::ManagedActionRoute => "action route",
            RuleId::SubagentResumeMode => "subagent resume mode",
            RuleId::SubagentCompletionWake => "completion wake",
            RuleId::SubagentInteractionBubble => "subagent web bubble",
            RuleId::SubagentModelVariants => "subagent model variants",
            RuleId::ContextWindow => "context window",
            RuleId::InferenceEndpoint => "inference endpoint",
            RuleId::GrokBotStreamAuth => "grokbot stream auth",
        }
    }

    /// 装完后 marker 该出现的总次数（跨全部目标文件）。`None` = 不校验（eligibility）。
    /// 与安装器 `install()` 里的硬校验和 `stream_mode_installed` 一一对应。
    pub fn expected(self) -> Option<u32> {
        self.expected_for(LayoutProfile::Desktop)
    }

    /// 按安装形态给期望命中数。`Server` 那几个 0 是实打实的约束：那些锚点所在的 workbench /
    /// Electron 文件在远程根本不存在，出现非 0 说明摸到了不该摸的东西。
    pub fn expected_for(self, profile: LayoutProfile) -> Option<u32> {
        match (self, profile) {
            (RuleId::Eligibility, _) => None,
            // 3.19.7 官方已把子代理从 unsupported run options 里拆出。那条规则留下只为卸旧装。
            // （子代理 featureFlags 那条不同：3.19.7 上它改挂 enableBrowserSubagent，照常校验。）
            (RuleId::ManagedSubagentRoute, _) => None,
            // 端点改道是**可选项**（远程网络好就不需要），装没装由 `installed_inference_endpoint`
            // 从盘上读，不在这里硬校验；真正要求它落地的是 remote 安装流程自己的显式检查。
            // 和 self_summary 一个路子：选项类的东西检测、不断言。
            (RuleId::InferenceEndpoint, _) => None,
            (RuleId::GrokBotStreamAuth, _) => None,
            (RuleId::ClientType, LayoutProfile::Desktop) => Some(EXPECTED_CLIENT_MARKERS),
            (RuleId::ClientType, LayoutProfile::Server) => Some(EXPECTED_SERVER_CLIENT_MARKERS),
            // 这三类的锚点全在 workbench / Electron 侧，远程没有那些文件。
            (
                RuleId::AgentHostEnablement
                | RuleId::SubagentCompletionWake
                | RuleId::SubagentModelVariants,
                LayoutProfile::Server,
            ) => Some(0),
            (RuleId::AgentHostEnablement | RuleId::SubagentCompletionWake, _) => Some(2),
            (RuleId::SubagentModelVariants, _) => Some(EXPECTED_SUBAGENT_MODEL_VARIANTS_MARKERS),
            _ => Some(1),
        }
    }

    /// 计数器写到 `MarkerCounts` 的哪个字段。
    pub fn slot(self, c: &mut MarkerCounts) -> &mut u32 {
        match self {
            RuleId::ClientType => &mut c.client_type,
            RuleId::Eligibility => &mut c.eligibility,
            RuleId::ManagedLocalRoute => &mut c.managed_local_route,
            RuleId::LocalRuntimeLoad => &mut c.local_runtime_load,
            RuleId::InferenceStream => &mut c.inference_stream,
            RuleId::AgentHostEnablement => &mut c.agent_host_enablement,
            RuleId::AgentHostIdentity => &mut c.agent_host_identity,
            RuleId::AgentHostMoveExec => &mut c.agent_host_move_exec,
            RuleId::ManagedSubagentRoute => &mut c.managed_subagent_route,
            RuleId::ManagedSubagentSession => &mut c.managed_subagent_session,
            RuleId::ManagedTaskTool => &mut c.managed_task_tool,
            RuleId::ManagedActionRoute => &mut c.managed_action_route,
            RuleId::SubagentResumeMode => &mut c.subagent_resume_mode,
            RuleId::SubagentCompletionWake => &mut c.subagent_completion_wake,
            RuleId::SubagentInteractionBubble => &mut c.subagent_interaction_bubble,
            RuleId::SubagentModelVariants => &mut c.subagent_model_variants,
            RuleId::ContextWindow => &mut c.context_window,
            RuleId::InferenceEndpoint => &mut c.inference_endpoint,
            RuleId::GrokBotStreamAuth => &mut c.grokbot_stream_auth,
        }
    }

    pub fn get(self, c: &MarkerCounts) -> u32 {
        let mut copy = *c;
        *self.slot(&mut copy)
    }

    /// inspect 时按哪些 marker 数这一类。client-type 有两个（新装 / 接管已有）。
    pub fn markers(self) -> &'static [&'static str] {
        match self {
            RuleId::ClientType => &[SAND_CLIENT_MARKER, SAND_CLIENT_EXISTING_MARKER],
            RuleId::Eligibility => &[SAND_ELIGIBILITY_MARKER],
            RuleId::ManagedLocalRoute => &[SAND_MANAGED_LOCAL_ROUTE_MARKER],
            RuleId::LocalRuntimeLoad => &[SAND_LOCAL_RUNTIME_LOAD_MARKER],
            // 已下线 Session 引擎的空 marker 不算「装好」，由 inspect 计入 legacy。
            RuleId::InferenceStream => &[SAND_DIRECT_STREAM_MARKER],
            RuleId::AgentHostEnablement => &[SAND_AGENT_HOST_ENABLEMENT_MARKER],
            RuleId::AgentHostIdentity => &[SAND_AGENT_HOST_IDENTITY_MARKER],
            RuleId::AgentHostMoveExec => &[SAND_AGENT_HOST_MOVE_EXEC_MARKER],
            RuleId::ManagedSubagentRoute => &[SAND_MANAGED_SUBAGENT_ROUTE_MARKER],
            RuleId::ManagedSubagentSession => &[SAND_MANAGED_SUBAGENT_SESSION_MARKER],
            RuleId::ManagedTaskTool => &[SAND_MANAGED_TASK_TOOL_MARKER],
            RuleId::ManagedActionRoute => &[SAND_MANAGED_ACTION_ROUTE_MARKER],
            RuleId::SubagentResumeMode => &[SAND_SUBAGENT_RESUME_MODE_MARKER],
            RuleId::SubagentCompletionWake => &[SAND_SUBAGENT_COMPLETION_WAKE_MARKER],
            RuleId::SubagentInteractionBubble => &[SAND_SUBAGENT_INTERACTION_BUBBLE_MARKER],
            RuleId::SubagentModelVariants => &[SAND_SUBAGENT_MODEL_VARIANTS_MARKER],
            RuleId::ContextWindow => &[SAND_CONTEXT_WINDOW_MARKER],
            RuleId::InferenceEndpoint => &[
                SAND_INFERENCE_ENDPOINT_MARKER,
                SAND_REMOTE_INFERENCE_ROUTE_MARKER,
            ],
            RuleId::GrokBotStreamAuth => &[
                SAND_GROK_BOX_RELAY_AUTH_MARKER,
                SAND_GROKBOT_DIRECT_AUTH_MARKER,
            ],
        }
    }
}

/// 一条规则怎么改文件。
pub enum RuleKind {
    /// 精确字面量。apply：`original → patched`；remove：`patched → original`（也认 `legacy`
    /// 里的每个旧变体）。apply 时若 `original` 不在、但某个 `legacy` 在且 `patched` 不在 →
    /// 原地迁移成 `patched`（计入 migrated，不计入新命中）。
    Literal {
        original: String,
        patched: String,
        /// 旧版本 / 其它档位的完整 patched 字符串。
        legacy: Vec<String>,
    },
    /// 正则。`per_file_limit` 限制单文件替换数（agent_host_enablement 每文件只改第一处）。
    ///
    /// `skip_if_followed_by`：Python 版用负向前瞻 `(?!marker)` 跳过已打过的位置；Rust 的 `regex`
    /// 不支持前瞻，所以改成显式字段——引擎在每个匹配的**结束位置**试这个正则，命中就跳过。
    /// 这也是 inspect 数「还剩几处没打」的依据。
    ///
    /// `skip_file_if_marked`：Python 对 agent_host_enablement / completion_wake 是
    /// `if MARKER not in content:` **整文件**跳过——这两条的 `apply` 正则在打过之后仍能匹配
    /// 原文（注入体是前置的，原文原样保留），没有这层开关就会重复注入。client-type 则相反：
    /// 逐位置 guard，部分打过的文件要把剩下的补上，所以它是 `false`。
    Regex {
        apply: Regex,
        apply_repl: fn(&Captures<'_>) -> String,
        skip_if_followed_by: Option<Regex>,
        skip_file_if_marked: bool,
        remove: Regex,
        remove_repl: fn(&Captures<'_>) -> String,
        per_file_limit: Option<usize>,
    },
    /// 在 `anchor` 之后插入 `injection`（每文件只一次）。marker 已在但注入体是某个
    /// `legacy_injections`（另一档选项 / 早期版本）→ 原地换成 `injection`（计 migrated）。
    /// remove：把 `injection` 及每个 `legacy_injections` 原文删掉。
    AnchoredInsert {
        anchor: String,
        injection: String,
        legacy_injections: Vec<String>,
    },
}

pub struct PatchRule {
    pub id: RuleId,
    pub kind: RuleKind,
}

impl std::fmt::Debug for PatchRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match &self.kind {
            RuleKind::Literal { .. } => "literal",
            RuleKind::Regex { .. } => "regex",
            RuleKind::AnchoredInsert { .. } => "anchored_insert",
        };
        write!(f, "PatchRule({} / {kind})", self.id.name())
    }
}

/// install 前的存在性预检：这些锚点**不被修改**，但它们不在就说明版本对不上，装了也白装。
/// Python：`AGENT_HOST_MOVE_EXEC_READY_ANCHOR` / `MANAGED_TASK_TOOL_READY_ANCHOR` /
/// `MANAGED_TASK_RUN_READY_ANCHOR`。
#[derive(Debug, Clone)]
pub struct PreflightAnchor {
    pub name: &'static str,
    pub needle: &'static str,
    /// 期望命中数。
    pub expect: u32,
    /// `None`：跨全部目标文件总计。`Some(x)`：只在**含有 x 的那些文件**里数
    /// —— "Creating subagent and starting execution" 这句在多个 chunk 里都出现，Python 只在
    /// 含 task tool factory 的那个文件里数它。
    pub only_in_files_containing: Option<&'static str>,
}

const TASK_TOOL_FACTORY: &str = "function Ne(e){const{parentModelId:t,modelInfo:n}=e;return{";

pub fn preflight_anchors() -> Vec<PreflightAnchor> {
    vec![
        PreflightAnchor {
            name: "move_exec ready branch",
            needle: "using @anysphere/agent-host-exec for session resources",
            expect: 1,
            only_in_files_containing: None,
        },
        PreflightAnchor {
            name: "task tool factory",
            // 3.18.25 叫 `soe`，3.19.7 改成 `Ae`（官方已接上 taskToolProps，不再是 void 0），
            // 3.19.13 又改名 `Ne`；函数体逐字未变。
            needle: TASK_TOOL_FACTORY,
            expect: 1,
            only_in_files_containing: None,
        },
        PreflightAnchor {
            name: "task tool Ne body",
            needle: "managed local loop does not build in-process child AgentConfig",
            expect: 1,
            only_in_files_containing: Some(TASK_TOOL_FACTORY),
        },
    ]
}

impl PreflightAnchor {
    /// 按本锚点的作用域数命中。`contents` 是全部目标文件的内容。
    pub fn count<'a>(&self, contents: impl IntoIterator<Item = &'a str>) -> u32 {
        contents
            .into_iter()
            .filter(|c| {
                self.only_in_files_containing
                    .is_none_or(|scope| c.contains(scope))
            })
            .map(|c| c.matches(self.needle).count() as u32)
            .sum()
    }
}

// ---------------------------------------------------------------------------
// 锚点与注入体。逐字对照 sand-stream-installer.py（v1.2.6-subagent-lifecycle-fixed.4 ↔ 3.18.25）
// L355–L789；每段注释标出 Python 常量名。压缩后的符号名只对 3.18.25 有效。
// ---------------------------------------------------------------------------

/// 压缩后的 JS 标识符（Python 里反复出现的 `[A-Za-z_$][A-Za-z0-9_$]*`）。
const IDENT: &str = r"[A-Za-z_$][A-Za-z0-9_$]*";

// ---- 1. client-type ------------------------------------------------------------------
// Python：`CLIENT_RULES`（is_glass / object_header / set_header）+ `legacy_client_re`（KC 迁移）。
// Python 用 `([\"'])(ide|sand)\2` 让引号成对；Rust `regex` 没有反向引用，改成显式交替。

const CLIENT_QUOTED_VALUE: &str = r#"("(?:ide|sand)"|'(?:ide|sand)')"#;
const CLIENT_QUOTED_SAND: &str = r#"("sand"|'sand')"#;

/// 三条锚点的上下文（第 1 组）；第 2 组是带引号的值。顺序与 Python `CLIENT_RULES` 一致。
const CLIENT_CONTEXTS: [&str; 3] = [
    // is_glass：`isGlass?"glass":"ide"`（16 处）
    r#"(isGlass\s*\?\s*["']glass["']\s*:\s*)"#,
    // object_header：`"x-cursor-client-type":"ide"`（3 处）
    r#"(["']x-cursor-client-type["']\s*:\s*)"#,
    // set_header：`header.set("x-cursor-client-type",x??"ide")`（4 处）
    r#"(header\.set\(\s*["']x-cursor-client-type["']\s*,\s*[A-Za-z_$][A-Za-z0-9_$.]*\s*(?:\?\?|\|\|)\s*)"#,
];

/// 带引号的值拆成（引号，值）：`"ide"` → `("\"", "ide")`；`'sand'` → `("'", "sand")`。
fn split_quoted(lit: &str) -> (&str, &str) {
    (&lit[..1], &lit[1..lit.len() - 1])
}

/// 装：值改成 `sand` 并跟 marker。原值已是 `sand`（别的渠道改过）→ `EXISTING` marker，卸载时还回 `sand`。
fn client_to_sand(c: &Captures<'_>) -> String {
    let (quote, value) = split_quoted(&c[2]);
    let marker = if value == "sand" {
        SAND_CLIENT_EXISTING_MARKER
    } else {
        SAND_CLIENT_MARKER
    };
    format!("{}{quote}sand{quote}{marker}", &c[1])
}

/// 卸：`"sand"/*MODE*/` → `"ide"`；`"sand"/*EXISTING*/` → `"sand"`。与 Python 一样不看上下文。
fn client_restore(c: &Captures<'_>) -> String {
    let (quote, _) = split_quoted(&c[1]);
    let value = if &c[2] == SAND_CLIENT_EXISTING_MARKER {
        "sand"
    } else {
        "ide"
    };
    format!("{quote}{value}{quote}")
}

fn kc_client_to_sand(c: &Captures<'_>) -> String {
    let (quote, _) = split_quoted(&c[1]);
    format!("{quote}sand{quote}{SAND_CLIENT_MARKER}")
}

fn kc_client_to_ide(c: &Captures<'_>) -> String {
    let (quote, _) = split_quoted(&c[1]);
    format!("{quote}ide{quote}")
}

/// Python `legacy_client_re`：`"sand"/*KC_SAND_CLIENT_V1*/` → 装：换成我们的 marker；卸：还成 `"ide"`。
fn kc_client_rule() -> Result<PatchRule> {
    let kc = re(&format!(
        "{CLIENT_QUOTED_SAND}{}",
        regex::escape(LEGACY_KC_CLIENT_MARKER)
    ))?;
    Ok(PatchRule {
        id: RuleId::ClientType,
        kind: RuleKind::Regex {
            apply: kc.clone(),
            apply_repl: kc_client_to_sand,
            skip_if_followed_by: None,
            skip_file_if_marked: false,
            remove: kc,
            remove_repl: kc_client_to_ide,
            per_file_limit: None,
        },
    })
}

/// Python `CLIENT_RULES`。三条共用一个不看上下文的 remove（Python 的 `client_re` / `existing_re`
/// 就是这样），所以卸载时第一条就把全部 marker 收干净，后两条命中 0；计数按 `RuleId` 汇总不受影响。
fn client_rules() -> Result<Vec<PatchRule>> {
    let guard = re(CLIENT_MARKER_GUARD_PATTERN)?;
    let restore = re(&format!(
        "{CLIENT_QUOTED_SAND}({}|{})",
        regex::escape(SAND_CLIENT_MARKER),
        regex::escape(SAND_CLIENT_EXISTING_MARKER)
    ))?;
    CLIENT_CONTEXTS
        .iter()
        .map(|ctx| {
            Ok(PatchRule {
                id: RuleId::ClientType,
                kind: RuleKind::Regex {
                    apply: re(&format!("{ctx}{CLIENT_QUOTED_VALUE}"))?,
                    apply_repl: client_to_sand,
                    skip_if_followed_by: Some(guard.clone()),
                    skip_file_if_marked: false,
                    remove: restore.clone(),
                    remove_repl: client_restore,
                    per_file_limit: None,
                },
            })
        })
        .collect()
}

// ---- 2. eligibility ------------------------------------------------------------------
// Python：`ELIGIBILITY_PREFIXES` ×6 + `legacy_eligibility`（KC 迁移）。3.18.25 上这些函数已不在
// bundle 里（命中 0，`expected()` 为 None）；规则保留是为了能卸掉旧安装、能迁移 KC 标记。

const ELIGIBILITY_PREFIXES: [&str; 6] = [
    "function r4g(e){const{adminSettingsService:t",
    "function Vj_(t){const{adminSettingsService:e",
    "function inf(e){const{adminSettingsService:t",
    "function HSy(t){const{adminSettingsService:e",
    "function Q_f(e){const{adminSettingsService:t",
    "function BpS(t){const{adminSettingsService:e",
];
const ELIGIBILITY_SPLIT: &str = "{const{adminSettingsService:";

fn kc_eligibility_to_sand(_: &Captures<'_>) -> String {
    format!("return!1;{SAND_ELIGIBILITY_MARKER}")
}

fn drop_match(_: &Captures<'_>) -> String {
    String::new()
}

/// Python `legacy_eligibility`：`return!1;/*KC_SAND_ELIGIBILITY_V1*/` → 装：换 marker；卸：整段删掉。
fn kc_eligibility_rule() -> Result<PatchRule> {
    let kc = re(&format!(
        "return!1;{}",
        regex::escape(LEGACY_KC_ELIGIBILITY_MARKER)
    ))?;
    Ok(PatchRule {
        id: RuleId::Eligibility,
        kind: RuleKind::Regex {
            apply: kc.clone(),
            apply_repl: kc_eligibility_to_sand,
            skip_if_followed_by: None,
            skip_file_if_marked: false,
            remove: kc,
            remove_repl: drop_match,
            per_file_limit: None,
        },
    })
}

/// `function r4g(e){const{…` → `function r4g(e){return!1;/*marker*/const{…`。
fn eligibility_rules() -> Vec<PatchRule> {
    ELIGIBILITY_PREFIXES
        .iter()
        .map(|prefix| {
            let patched = prefix.replacen(
                ELIGIBILITY_SPLIT,
                &format!("{{return!1;{SAND_ELIGIBILITY_MARKER}const{{adminSettingsService:"),
                1,
            );
            literal(RuleId::Eligibility, *prefix, patched, vec![])
        })
        .collect()
}

// ---- 3–6. 简单字面量 ------------------------------------------------------------------

/// Python `MANAGED_LOCAL_ROUTE_ORIGINAL` / `_PATCHED`。
const MANAGED_LOCAL_ROUTE_ORIGINAL: &str = concat!(
    r#"if(!o)return{runtime:"connect",reason:"gate-off"};"#,
    "const s=g(t),i=A(s,e,r);",
    r#"return void 0!==i?f(i,s):{runtime:"managed-local",reason:"eligible"}"#,
);
fn managed_local_route_patched() -> String {
    [
        r#"if(!1)return{runtime:"connect",reason:"gate-off"};"#,
        "const s=g(t),i=A(s,e,r);",
        "return void 0!==i?f(i,s):",
        SAND_MANAGED_LOCAL_ROUTE_MARKER,
        r#"{runtime:"managed-local",reason:"sand-client"}"#,
    ]
    .concat()
}

/// Python `LOCAL_RUNTIME_LOAD_ORIGINAL` / `_PATCHED`。
const LOCAL_RUNTIME_LOAD_ORIGINAL: &str = "let t=!1;try{t=await r.cursor.checkFeatureGate(Ms)}";
fn local_runtime_load_patched() -> String {
    ["let t=!0;", SAND_LOCAL_RUNTIME_LOAD_MARKER, "try{t=!0}"].concat()
}

/// Python `AGENT_HOST_MOVE_EXEC_ORIGINAL` / `_PATCHED`。
/// 3.19.13：`Js="cursor_agent_host_move_exec"`，紧邻的 `A` 读 `Ms="agent_host_local_loop"`，
/// 二者以 `y=h||A` 汇合，所以把 `h` 钉成真就够了。
const AGENT_HOST_MOVE_EXEC_ORIGINAL: &str =
    "h=await Promise.resolve(r.cursor.checkFeatureGate(Js)).catch(()=>!1)";
fn agent_host_move_exec_patched() -> String {
    ["h=!0", SAND_AGENT_HOST_MOVE_EXEC_MARKER].concat()
}

/// Python `MANAGED_SUBAGENT_ROUTE_ORIGINAL` / `_PATCHED`。
const MANAGED_SUBAGENT_ROUTE_ORIGINAL: &str = concat!(
    "hasUnsupportedRunOptions:void 0!==e.runOptions.customSystemPrompt||",
    "void 0!==e.runOptions.harness||",
    "!0===e.runOptions.excludeWorkspaceContext||",
    "void 0!==e.runOptions.subagentTypeName||",
    "void 0!==e.runOptions.parentAgentToolCallId||",
    "!0===e.runOptions.directMetaParentChildSubagent",
);
fn managed_subagent_route_patched() -> String {
    [
        concat!(
            "hasUnsupportedRunOptions:void 0!==e.runOptions.customSystemPrompt||",
            "void 0!==e.runOptions.harness||",
            "!0===e.runOptions.excludeWorkspaceContext",
        ),
        SAND_MANAGED_SUBAGENT_ROUTE_MARKER,
        "||!0===e.runOptions.directMetaParentChildSubagent",
    ]
    .concat()
}

// ---- 7. action route（三档）------------------------------------------------------------

/// Python `MANAGED_ACTION_ROUTE_ORIGINAL`。
const MANAGED_ACTION_ROUTE_ORIGINAL: &str = concat!(
    r#""backgroundTaskCompletionAction"===e.actionCase?"#,
    r#"e.conversationMode!==o.xy.AGENT?"mode-not-supported":y(e,r):"#,
    r#""userMessageAction"!==e.actionCase?"action-not-supported":"#,
    "function(e){return e.requestedMode===o.xy.AGENT||",
    "e.isHostedSubagentChild&&e.requestedMode===o.xy.UNSPECIFIED}(e)?",
    r#"e.simulatedUserMessage?"simulated-message-not-supported":y(e,r):"#,
    r#""mode-not-supported""#,
);

/// Python `_managed_action_route_patched(level)`。3.19.7 官方已放行 background 完成；
/// 补丁补上 summarize / resume，模式档位改官方那个 IIFE。
fn managed_action_route_patched(gate: ModeGate) -> String {
    let mode_fn = match gate {
        ModeGate::All => "function(e){return!0}",
        ModeGate::AgentPlan => concat!(
            "function(e){return e.requestedMode===o.xy.AGENT||",
            "e.requestedMode===o.xy.PLAN||",
            "e.isHostedSubagentChild&&e.requestedMode===o.xy.UNSPECIFIED}",
        ),
        ModeGate::Agent => concat!(
            "function(e){return e.requestedMode===o.xy.AGENT||",
            "e.isHostedSubagentChild&&e.requestedMode===o.xy.UNSPECIFIED}",
        ),
    };
    [
        SAND_MANAGED_ACTION_ROUTE_MARKER,
        concat!(
            r#""backgroundTaskCompletionAction"===e.actionCase?"#,
            r#"e.conversationMode!==o.xy.AGENT?"mode-not-supported":y(e,r):"#,
            r#""summarizeAction"===e.actionCase||"resumeAction"===e.actionCase?y(e,r):"#,
            r#""userMessageAction"!==e.actionCase?"action-not-supported":"#,
        ),
        mode_fn,
        concat!(
            r#"(e)?e.simulatedUserMessage?"simulated-message-not-supported":y(e,r):"#,
            r#""mode-not-supported""#,
        ),
    ]
    .concat()
}

const MODE_GATES: [ModeGate; 3] = [ModeGate::Agent, ModeGate::AgentPlan, ModeGate::All];

/// 选中的档位是 `patched`，另两档是 `legacy`：引擎会把已装的其它档位原地归一到当前档
/// （Python：`elif want_action_route not in next_content: for level in …`）。
fn managed_action_route_rule(gate: ModeGate) -> PatchRule {
    let legacy = MODE_GATES
        .iter()
        .filter(|g| **g != gate)
        .map(|g| managed_action_route_patched(*g))
        .collect();
    literal(
        RuleId::ManagedActionRoute,
        MANAGED_ACTION_ROUTE_ORIGINAL,
        managed_action_route_patched(gate),
        legacy,
    )
}

// ---- 8–10. 简单字面量 ------------------------------------------------------------------

/// Python `SUBAGENT_RESUME_MODE_ORIGINAL` / `_PATCHED`。
const SUBAGENT_RESUME_MODE_ORIGINAL: &str =
    "e.resumeAgentId&&e.mode===Gn.FL.UNSPECIFIED&&!e.readonly?Ee.xy.UNSPECIFIED:";
fn subagent_resume_mode_patched() -> String {
    [
        "e.resumeAgentId&&e.mode===Gn.FL.UNSPECIFIED&&!e.readonly?",
        SAND_SUBAGENT_RESUME_MODE_MARKER,
        "Ee.xy.AGENT:",
    ]
    .concat()
}

/// Python `SUBAGENT_INTERACTION_BUBBLE_ORIGINAL` / `_PATCHED`（61.js 的 `In`）。
const SUBAGENT_INTERACTION_BUBBLE_ORIGINAL: &str =
    "function Ar(e){return void 0===e||e===wt.w3.UNSPECIFIED?wt.w3.AUTO_REJECT:e}";
fn subagent_interaction_bubble_patched() -> String {
    [
        "function Ar(e){return void 0===e||e===wt.w3.UNSPECIFIED?wt.w3.BUBBLE_TO_PARENT:e}",
        SAND_SUBAGENT_INTERACTION_BUBBLE_MARKER,
    ]
    .concat()
}

// ---- 17. subagent model variants（workbench.desktop / workbench.glass 的 `rRf()`）--------------
// Python `SUBAGENT_MODEL_VARIANTS_RE` / `_PATCH_RE` + `_subagent_model_variants_patched` / `_original`。
//
// `rRf()` 把「用户勾选且支持 agent 的模型」组成 `selectedSubagentModels`（RequestedModel[]）随每轮
// 请求下发给 agent host；V5 任务工具目录按其 modelId 建 key。原文每个模型只发 canonical name，
// 于是目录里永远没有 fast / max / thinking / effort 变体。这里在同一个 map 里把该模型的 `legacySlugs`
// （服务端 AvailableModel.legacy_slugs，如 `claude-opus-5-thinking-max-fast`）也各造一条一并下发；
// 解析出的 legacy slug 原样进 InferenceRequestedModel.model_id，服务端自己解释变体（Cursor 子代理
// 默认模型 `composer-2.5-fast` 就是这样送的）。
//
// 两个 workbench bundle 的压缩标识符不同（`l/u/h/s/o/vA/p/AP` 与 `c/u/d/s/o/ZR/h/E3`），Python 用
// `\2` `\3` `\5` `\7` 回引；Rust 把每次出现都单独捕获，在闭包里比较，不等就原样返回。

/// 原文里各标识符的出现次数：模型变量 ×3、参数变量 ×2、maxMode ×2、参数项 ×3。
struct VariantIdents<'a> {
    models: &'a str,
    model: &'a str,
    params: &'a str,
    resolve: &'a str,
    max_mode: &'a str,
    requested_model_cls: &'a str,
    param: &'a str,
    param_cls: &'a str,
}

/// 前 14 组是原文（apply）与补丁（remove）共有的头部；不一致返回 `None`。
fn variant_idents<'a>(c: &'a Captures<'_>) -> Option<VariantIdents<'a>> {
    let (model, params, max_mode, param) = (&c[2], &c[3], &c[6], &c[11]);
    if c[5] != *model || c[8] != *model || c[10] != *params || c[9] != *max_mode {
        return None;
    }
    if c[13] != *param || c[14] != *param {
        return None;
    }
    Some(VariantIdents {
        models: c.get(1)?.as_str(),
        model: c.get(2)?.as_str(),
        params: c.get(3)?.as_str(),
        resolve: c.get(4)?.as_str(),
        max_mode: c.get(6)?.as_str(),
        requested_model_cls: c.get(7)?.as_str(),
        param: c.get(11)?.as_str(),
        param_cls: c.get(12)?.as_str(),
    })
}

fn subagent_model_variants_original_regex() -> String {
    format!(
        concat!(
            r"selectedSubagentModels:({id})\.map\(({id})=>\{{const ({id})=({id})\(({id})\.name,({id})\);",
            r"return new ({id})\(\{{modelId:({id})\.name,maxMode:({id}),",
            r"parameters:({id})\.map\(({id})=>new ({id})\(\{{id:({id})\.id,value:({id})\.value\}}\)\)\}}\)\}}\)",
        ),
        id = IDENT
    )
}

fn subagent_model_variants_patched_regex() -> String {
    format!(
        concat!(
            r"selectedSubagentModels:({id})\.flatMap\(({id})=>\{{const ({id})=({id})\(({id})\.name,({id})\);",
            r"return\[new ({id})\(\{{modelId:({id})\.name,maxMode:({id}),",
            r"parameters:({id})\.map\(({id})=>new ({id})\(\{{id:({id})\.id,value:({id})\.value\}}\)\)\}}\),",
            r"\.\.\.\(({id})\.legacySlugs\?\?\[\]\)\.filter\(g=>g&&g!==({id})\.name\)",
            r"\.map\(g=>new ({id})\(\{{modelId:g,maxMode:({id}),parameters:\[\]\}}\)\)\]{marker}\}}\)",
        ),
        id = IDENT,
        marker = regex::escape(SAND_SUBAGENT_MODEL_VARIANTS_MARKER)
    )
}

fn variants_enable(c: &Captures<'_>) -> String {
    let Some(v) = variant_idents(c) else {
        return c[0].to_string();
    };
    format!(
        concat!(
            "selectedSubagentModels:{models}.flatMap({model}=>{{const {params}={resolve}({model}.name,{max_mode});",
            "return[new {rm}({{modelId:{model}.name,maxMode:{max_mode},",
            "parameters:{params}.map({param}=>new {pc}({{id:{param}.id,value:{param}.value}}))}}),",
            "...({model}.legacySlugs??[]).filter(g=>g&&g!=={model}.name)",
            ".map(g=>new {rm}({{modelId:g,maxMode:{max_mode},parameters:[]}}))]{marker}}})",
        ),
        models = v.models,
        model = v.model,
        params = v.params,
        resolve = v.resolve,
        max_mode = v.max_mode,
        rm = v.requested_model_cls,
        param = v.param,
        pc = v.param_cls,
        marker = SAND_SUBAGENT_MODEL_VARIANTS_MARKER
    )
}

fn variants_disable(c: &Captures<'_>) -> String {
    let Some(v) = variant_idents(c) else {
        return c[0].to_string();
    };
    // 补丁尾巴里再出现的 model ×2、RequestedModel 类、maxMode 也要对上。
    if c[15] != *v.model
        || c[16] != *v.model
        || c[17] != *v.requested_model_cls
        || c[18] != *v.max_mode
    {
        return c[0].to_string();
    }
    format!(
        concat!(
            "selectedSubagentModels:{models}.map({model}=>{{const {params}={resolve}({model}.name,{max_mode});",
            "return new {rm}({{modelId:{model}.name,maxMode:{max_mode},",
            "parameters:{params}.map({param}=>new {pc}({{id:{param}.id,value:{param}.value}}))}})}})",
        ),
        models = v.models,
        model = v.model,
        params = v.params,
        resolve = v.resolve,
        max_mode = v.max_mode,
        rm = v.requested_model_cls,
        param = v.param,
        pc = v.param_cls,
    )
}

/// apply 正则匹配不到补丁后的文本（`.map(` 与 `.flatMap(` 不同），天然幂等，不需要整文件开关。
fn subagent_model_variants_rule() -> Result<PatchRule> {
    Ok(PatchRule {
        id: RuleId::SubagentModelVariants,
        kind: RuleKind::Regex {
            apply: re(&subagent_model_variants_original_regex())?,
            apply_repl: variants_enable,
            skip_if_followed_by: None,
            skip_file_if_marked: false,
            remove: re(&subagent_model_variants_patched_regex())?,
            remove_repl: variants_disable,
            per_file_limit: None,
        },
    })
}

/// Python `MAX_TOKENS_ORIGINAL` / `_PATCHED`。
const MAX_TOKENS_ORIGINAL: &str = concat!(
    "t.resolveExtendedUsage({inputTokens:n.inputTokens,",
    "outputTokens:n.outputTokens,cacheReadTokens:n.cacheReadTokens,",
    "cacheWriteTokens:n.cacheWriteTokens,maxTokens:n.maxTokens})",
);
fn max_tokens_patched() -> String {
    [
        concat!(
            "t.resolveExtendedUsage({inputTokens:n.inputTokens,",
            "outputTokens:n.outputTokens,cacheReadTokens:n.cacheReadTokens,",
            "cacheWriteTokens:n.cacheWriteTokens,maxTokens:(()=>{",
            r#"const c=this.requestedModel?.parameters?.find(p=>p.id==="context")?.value;"#,
            "if(void 0===c)return n.maxTokens;",
            "const s=String(c).trim().toLowerCase();const num=parseFloat(s);",
            "if(!Number.isFinite(num)||num<=0)return n.maxTokens;",
            r#"const mult=s.endsWith("k")?1e3:s.endsWith("m")?1e6:s.endsWith("b")?1e9:1;"#,
            "return num*mult})()})",
        ),
        SAND_CONTEXT_WINDOW_MARKER,
    ]
    .concat()
}

// ---- 11. completion wake ---------------------------------------------------------------
// Python `SUBAGENT_COMPLETION_WAKE_RE` / `_PATCH_RE`。`\1` 改成再捕获一次标识符、在闭包里比较；
// 不相等就原样返回（Python 里这种位置根本不会匹配）。Python 是 `if MARKER not in content:` 整文件
// 跳过 → `skip_file_if_marked`。

fn wake_enable(c: &Captures<'_>) -> String {
    if c[1] != c[2] {
        return c[0].to_string();
    }
    format!(
        r#"{v}.source==="subagent"{SAND_SUBAGENT_COMPLETION_WAKE_MARKER}||{}"#,
        &c[0],
        v = &c[1]
    )
}

fn wake_disable(c: &Captures<'_>) -> String {
    if c[1] != c[2] || c[1] != c[3] {
        return c[0].to_string();
    }
    format!(
        r#"{v}.source==="interactive-child"||{v}.payload.notificationContext==="user_driven_interactive_child""#,
        v = &c[1]
    )
}

fn subagent_completion_wake_rule() -> Result<PatchRule> {
    let original = format!(
        r#"({IDENT})\.source==="interactive-child"\|\|({IDENT})\.payload\.notificationContext==="user_driven_interactive_child""#
    );
    let patched = format!(
        r#"({IDENT})\.source==="subagent"{}\|\|{original}"#,
        regex::escape(SAND_SUBAGENT_COMPLETION_WAKE_MARKER)
    );
    Ok(PatchRule {
        id: RuleId::SubagentCompletionWake,
        kind: RuleKind::Regex {
            apply: re(&original)?,
            apply_repl: wake_enable,
            skip_if_followed_by: None,
            skip_file_if_marked: true,
            remove: re(&patched)?,
            remove_repl: wake_disable,
            per_file_limit: None,
        },
    })
}

// ---- 12. subagent session（675.js 的 featureFlags 对象 `xre`）-----------------------------

/// Python `MANAGED_SUBAGENT_SESSION_ORIGINAL` / `_PATCHED` / `_PATCHED_V1`。
/// `nalLoopDetection` 不是笔误，bundle 里就是这个键名。
const MANAGED_SUBAGENT_SESSION_HEAD: &str = concat!(
    "const xre={enableEmptyResponseRetry:!0,enableGrepBroadGlobGuard:!0,",
    "enableReadToolNegativeOffset:!0,enableSandboxSharedBuildCache:!0,",
    "nalLoopDetection:!0",
);
fn managed_subagent_session_original() -> String {
    [MANAGED_SUBAGENT_SESSION_HEAD, "};"].concat()
}
fn managed_subagent_session_patched() -> String {
    [
        MANAGED_SUBAGENT_SESSION_HEAD,
        ",useClientSideSubagent:!0,enableExploreSubagent:!0",
        SAND_MANAGED_SUBAGENT_SESSION_MARKER,
        "};",
    ]
    .concat()
}
/// 上一版（还没开 explore）。
fn managed_subagent_session_patched_v1() -> String {
    [
        MANAGED_SUBAGENT_SESSION_HEAD,
        ",useClientSideSubagent:!0",
        SAND_MANAGED_SUBAGENT_SESSION_MARKER,
        "};",
    ]
    .concat()
}

// ---- 9b. browser 子代理开关（3.19.7 的 featureFlags 对象）------------------------------
//
// 子代理目录是**按 feature flag 拼的**：`includeBrowserUseSubagent: t?.enableBrowserSubagent ?? false`。
// 目录里没有的类型会被本地循环拒成 `Local loop cannot resolve subagent type "browser-use"`。
// 原版 Cursor 里这不致命——路由会退回云端；但补丁把 `runtime:"connect"` 那条路封了
// （见 `managed_local_route_patched`），本地循环不认识就是硬失败。
//
// 3.19.7 的这个对象和 3.18.x 的 `const xre={…}` 已经不是一回事：上游自己开了
// `useClientSideSubagent` / `enableExploreSubagent` / `enableDebugSubagent`（所以上面那条规则在
// 3.19.7 上只剩「卸旧装」的用途、不进硬校验），但**没开** browser。这里单挂一条规则补上它，
// 与上面共用 `RuleId` 与 marker：同一个对象、同一件事，界面上也该是同一行。
//
// 浏览器机能本身在本地循环里是齐的：它加载的 chunk 清单含 9341 / 2337 / 5371，
// `browser_take_screenshot`、`browser_tools`、`browser_use_enabled` 都在那几块。
//
// `cursor-guide` 不需要 flag（`includeCursorGuideSubagent:!0` 硬编码开着），它走不通是模式闸的事，
// 见 `managed_action_route_patched`。
const SUBAGENT_BROWSER_FLAG_ORIGINAL: &str = ",useClientSideSubagent:!0};";
fn subagent_browser_flag_patched() -> String {
    [
        ",useClientSideSubagent:!0,enableBrowserSubagent:!0",
        SAND_MANAGED_SUBAGENT_SESSION_MARKER,
        "};",
    ]
    .concat()
}

// ---- 13. task tool ---------------------------------------------------------------------
// Python `MANAGED_TASK_TOOL_ORIGINAL` + `_managed_task_tool_patched*`（当前 / v4 / v3 / v2 / v125 / v124）
// + `SUBAGENT_MODEL_CATALOG_JS` / `SUBAGENT_MODEL_SLUGS_V4` / `_managed_task_tool_props`。

const MANAGED_TASK_TOOL_ORIGINAL: &str =
    "isGenerateImageModelRestricted:!1,taskToolProps:Ne({parentModelId:null!=p?p:n.modelName,modelInfo:n})},resolvers:";

/// V6：模型目录**动态**取注入点作用域里的 `e.runOptions.selectedSubagentModels` —— workbench 每轮
/// 随请求下发的、用户在模型选择器里勾选且支持 agent 的模型（`agent.v1.RequestedModel[]`；Cursor
/// 原生 connect 路径的服务端就是用这个字段组 taskToolProps）。于是目录永远与 Cursor 自身一致。
/// 剔掉 `"default"`（Auto，解析器会把它静默变成 composer）。
///
/// 末尾追加两把父模型键：客户端请求的 `e.requestedModel.modelId`，和 `i`
/// （`e.resolvedModel?.modelId ?? e.modelId`）。Session 引擎下 `resolvedModel` 是服务端解析过的，
/// 可能是 `claude-opus-5-thinking-high` 这类变体 slug；两把都在目录里，`Task` 的 `model` 参数写哪个
/// 都解析得到。Direct 引擎下二者相同，Map 重复键无害。Python `SUBAGENT_MODEL_CATALOG_JS`，逐字一致。
pub const SUBAGENT_MODEL_CATALOG_JS: &str = concat!(
    "new Map([...(e.runOptions.selectedSubagentModels??[])",
    ".map(m=>m.modelId).filter(m=>m&&\"default\"!==m).map(m=>[m,{slug:m}]),",
    "[e.requestedModel.modelId,{slug:e.requestedModel.modelId}],[i,{slug:i}]])"
);

/// V5 的目录：只追加 `i`。**只用于识别 / 迁移 / 卸载已装的 V5**。Python `SUBAGENT_MODEL_CATALOG_JS_V5`。
pub const SUBAGENT_MODEL_CATALOG_JS_V5: &str = concat!(
    "new Map([...(e.runOptions.selectedSubagentModels??[])",
    ".map(m=>m.modelId).filter(m=>m&&\"default\"!==m).map(m=>[m,{slug:m}]),",
    "[i,{slug:i}]])"
);

/// V4 曾硬编码的目录。14 个里 5 个是客户端别名或不存在的 id（`opus-5` / `fable-5` / `sonnet-4.5` /
/// `gpt-5.6` / `claude-4.5-opus`，服务端报 "AI Model Not Found"）。**只用于识别 / 迁移 / 卸载已装的
/// V3、V4**，不再用于新装。Python `SUBAGENT_MODEL_SLUGS_V4`。
pub const SUBAGENT_MODEL_SLUGS_V4: &[&str] = &[
    "claude-4.5-sonnet",
    "claude-4.5-opus",
    "claude-4.5-opus-high",
    "claude-4.5-haiku",
    "fable-5",
    "opus-5",
    "sonnet-4.5",
    "grok-4.6",
    "grok-4.5",
    "gemini-3.1-pro",
    "gemini-3-flash",
    "gpt-5.6",
    "gpt-5.5",
    "gpt-5.3-codex",
];

/// Python `_subagent_model_catalog_js_v4`：`new Map([["slug",{slug:"slug"}],…,[i,{slug:i}]])`。
fn subagent_model_catalog_js_v4() -> String {
    let mut s = String::from("new Map([");
    for slug in SUBAGENT_MODEL_SLUGS_V4 {
        s.push_str(&format!(r#"["{slug}",{{slug:"{slug}"}}],"#));
    }
    s.push_str("[i,{slug:i}]])");
    s
}

struct TaskToolProps<'a> {
    marker: &'a str,
    /// 子代理「与父模型相同」时用的名字。V6 起取客户端请求的 `e.requestedModel.modelId`：
    /// Session 引擎下 `i` 是服务端解析后的变体 slug，拿它当父模型再配上 `parentModelParameters`
    /// （thinking / max 等参数）会把变体编码两遍。V5 及更早都是 `i`。
    parent_model_name: &'a str,
    model_catalog: &'a str,
    is_model_valid: &'a str,
    custom_subagent_normalizer: &'a str,
}

/// Python `_managed_task_tool_props`。
fn managed_task_tool_props(p: &TaskToolProps<'_>) -> String {
    [
        "{",
        p.marker,
        "parentRequestedModelName:",
        p.parent_model_name,
        ",",
        "parentModelParameters:e.requestedModel.parameters,",
        "parentMaxMode:l,",
        "isModelBlocked:()=>!1,",
        "isModelValid:",
        p.is_model_valid,
        ",",
        "requiresMaxMode:()=>!1,",
        "compareModelCosts:()=>0,",
        r#"subagentModelForcePolicy:"none","#,
        "requireServerSideSubagent:!1,",
        "subagentModels:{modelsBySlug:",
        p.model_catalog,
        "},",
        "normalizeCustomSubagents:",
        p.custom_subagent_normalizer,
        ",",
        "getTaskToolConfig:async()=>({})",
        "}",
    ]
    .concat()
}

const TASK_TOOL_HEAD: &str = "isGenerateImageModelRestricted:!1,taskToolProps:";
const TASK_TOOL_TYPED_GUARD: &str = "void 0!==e.runOptions.subagentTypeName?void 0:";
const TASK_TOOL_TAIL: &str = "},resolvers:";

/// `taskToolProps:` 后面接 props（带不带 `subagentTypeName` 判断），再接 `},resolvers:`。
fn managed_task_tool_variant(typed_guard: bool, props: &TaskToolProps<'_>) -> String {
    let props = managed_task_tool_props(props);
    [
        TASK_TOOL_HEAD,
        if typed_guard {
            TASK_TOOL_TYPED_GUARD
        } else {
            ""
        },
        props.as_str(),
        TASK_TOOL_TAIL,
    ]
    .concat()
}

/// V7 目录：第三把父模型键是官方 `parentModelId`（`null!=p?p:n.modelName`），不再用 `i`。
const SUBAGENT_MODEL_CATALOG_JS_V7: &str = concat!(
    "new Map([...(e.runOptions.selectedSubagentModels??[])",
    ".map(m=>m.modelId).filter(m=>m&&\"default\"!==m).map(m=>[m,{slug:m}]),",
    "[e.requestedModel.modelId,{slug:e.requestedModel.modelId}],",
    "[null!=p?p:n.modelName,{slug:null!=p?p:n.modelName}]])"
);

/// 当前版（V7）：替换 3.19.7 官方 Ae() 调用点，打开目录、保留官方已开的子代理类型旗标。
fn managed_task_tool_patched() -> String {
    [
        "isGenerateImageModelRestricted:!1,taskToolProps:{",
        SAND_MANAGED_TASK_TOOL_MARKER,
        "parentRequestedModelName:e.requestedModel.modelId,",
        "parentModelParameters:e.requestedModel.parameters,",
        "parentMaxMode:v,",
        "isModelBlocked:()=>!1,",
        "isModelValid:()=>!0,",
        "requiresMaxMode:()=>!1,",
        "compareModelCosts:()=>0,",
        r#"subagentModelForcePolicy:"none","#,
        "requireServerSideSubagent:!1,",
        "enableExecuteHookExec:!0,",
        "enableExploreSubagent:!0,",
        "enableShellSubagent:!0,",
        "enableDebugSubagent:!0,",
        "enableBrowserSubagent:!0,",
        "enableGrindSwarmSubagent:!0,",
        "subagentModels:{modelsBySlug:",
        SUBAGENT_MODEL_CATALOG_JS_V7,
        "},",
        "normalizeCustomSubagents:e=>e,",
        "getTaskToolConfig:async()=>({})",
        "}},resolvers:",
    ]
    .concat()
}

/// V6：旧 original（taskToolProps:void 0）+ 动态目录两把父模型键。只用于迁移 / 卸载。
fn managed_task_tool_patched_v6() -> String {
    managed_task_tool_variant(
        true,
        &TaskToolProps {
            marker: LEGACY_TASK_TOOL_MARKERS[5],
            parent_model_name: "e.requestedModel.modelId",
            model_catalog: SUBAGENT_MODEL_CATALOG_JS,
            is_model_valid: "()=>!0",
            custom_subagent_normalizer: "e=>e",
        },
    )
}

/// V5：动态模型目录（只追加 `i`）、父模型名 `i`、自定义子代理直通。
fn managed_task_tool_patched_v5() -> String {
    managed_task_tool_variant(
        true,
        &TaskToolProps {
            marker: LEGACY_TASK_TOOL_MARKERS[4],
            parent_model_name: "i",
            model_catalog: SUBAGENT_MODEL_CATALOG_JS_V5,
            is_model_valid: "()=>!0",
            custom_subagent_normalizer: "e=>e",
        },
    )
}

/// V4：硬编码模型目录（含 5 个假 slug）、自定义子代理直通。
fn managed_task_tool_patched_v4() -> String {
    managed_task_tool_variant(
        true,
        &TaskToolProps {
            marker: LEGACY_TASK_TOOL_MARKERS[3],
            parent_model_name: "i",
            model_catalog: &subagent_model_catalog_js_v4(),
            is_model_valid: "()=>!0",
            custom_subagent_normalizer: "e=>e",
        },
    )
}

/// V3：硬编码模型目录，但仍丢弃自定义子代理。
fn managed_task_tool_patched_v3() -> String {
    managed_task_tool_variant(
        true,
        &TaskToolProps {
            marker: LEGACY_TASK_TOOL_MARKERS[2],
            parent_model_name: "i",
            model_catalog: &subagent_model_catalog_js_v4(),
            is_model_valid: "()=>!0",
            custom_subagent_normalizer: "()=>[]",
        },
    )
}

/// V2：只塞父模型、isModelValid 限父模型、丢弃自定义子代理。
fn managed_task_tool_patched_v2() -> String {
    managed_task_tool_variant(
        true,
        &TaskToolProps {
            marker: LEGACY_TASK_TOOL_MARKERS[1],
            parent_model_name: "i",
            model_catalog: "new Map([[i,{slug:i}]])",
            is_model_valid: "e=>e===i",
            custom_subagent_normalizer: "()=>[]",
        },
    )
}

/// 上游 1.2.5（V1 marker）：空 Map、限父模型、丢弃自定义子代理。
fn managed_task_tool_patched_v125() -> String {
    managed_task_tool_variant(
        true,
        &TaskToolProps {
            marker: LEGACY_TASK_TOOL_MARKERS[0],
            parent_model_name: "i",
            model_catalog: "new Map",
            is_model_valid: "e=>e===i",
            custom_subagent_normalizer: "()=>[]",
        },
    )
}

/// 上游 1.2.4（V1 marker）：没有 subagentTypeName 的判断，自定义子代理直通。
fn managed_task_tool_patched_v124() -> String {
    managed_task_tool_variant(
        false,
        &TaskToolProps {
            marker: LEGACY_TASK_TOOL_MARKERS[0],
            parent_model_name: "i",
            model_catalog: "new Map",
            is_model_valid: "e=>e===i",
            custom_subagent_normalizer: "e=>e",
        },
    )
}

/// legacy 顺序 = Python `apply_patch_to_content` 里迁移循环的顺序（v5 / v4 / v3 / v2 / v125 / v124）。
fn managed_task_tool_rule() -> PatchRule {
    literal(
        RuleId::ManagedTaskTool,
        MANAGED_TASK_TOOL_ORIGINAL,
        managed_task_tool_patched(),
        vec![
            managed_task_tool_patched_v6(),
            managed_task_tool_patched_v5(),
            managed_task_tool_patched_v4(),
            managed_task_tool_patched_v3(),
            managed_task_tool_patched_v2(),
            managed_task_tool_patched_v125(),
            managed_task_tool_patched_v124(),
        ],
    )
}

// ---- 14. agent host identity -----------------------------------------------------------

/// Python `AGENT_HOST_IDENTITY_ORIGINAL` / `_PATCHED`。
const AGENT_HOST_IDENTITY_ORIGINAL: &str = r#"clientIdentity:{clientType:"ide"}"#;
fn agent_host_identity_patched() -> String {
    [
        r#"clientIdentity:{clientType:"sand""#,
        SAND_AGENT_HOST_IDENTITY_MARKER,
        "}",
    ]
    .concat()
}

// ---- 15. direct stream -----------------------------------------------------------------
// Python `DIRECT_STREAM_ANCHOR` + `_direct_stream_injection(self_summary, context_window)`
// + `CONTEXT_TOKENS_EXPR`。3.18.25 的 attempt 工厂叫 `gre`，会话类叫 `tre`，元数据解析叫 `are`。

const DIRECT_STREAM_ANCHOR: &str =
    "function me(e){return t=>{return n=this,r=void 0,s=function*(){";

/// 模型参数 `context`（"200k"/"1m"/"1b" 或纯数字）→ token 数；不设则 undefined。
const CONTEXT_TOKENS_EXPR: &str = concat!(
    r#"(function(){const v=r.get("context");if(void 0===v)return void 0;"#,
    r#"const s=String(v).trim().toLowerCase();const n=parseFloat(s);"#,
    r#"if(!Number.isFinite(n)||n<=0)return void 0;"#,
    r#"const m=s.endsWith("k")?1e3:s.endsWith("m")?1e6:s.endsWith("b")?1e9:1;"#,
    r#"return n*m})()"#,
);

/// 官方 `ve` 在服务端没下发 promptConfig 时给 `getSession` 挂的执行器中间件链（3.19.7）：
/// `o.got` 组装 → 图片缩放（超限截图缩到供应商能收的尺寸，缩不动的换成占位文本）+ 请求指标；
/// `o.sXH` 把它们 compose。loopNudge / progressReminder / effortLevel 这几段都要 runReady 的
/// 元数据才会启用，Direct 没有那一步，所以和官方一样不挂。
const DIRECT_SESSION_MIDDLEWARE: &str = concat!(
    r#"(0,o.sXH)((0,o.got)({imageResizing:{webpWithoutCodec:"passthrough"},"#,
    r#"configureDsv3Thinking:!1,isSlopPromptSession:!1,usesComposerFacade:!1,"#,
    r#"safeTokenizationEnabled:!1,supportsAssistantMessagePrefill:!0},{}))"#,
);

/// Direct 注入体的三代形态。只有 `Current` 会被装；另外两种只用于识别 / 迁移 / 卸载。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DirectShape {
    /// 3.19.7：`resolvedModelMetadata` 包进 `promptModelInfo`，`getSession` 挂官方中间件链。
    Current,
    /// 3.19.7 首版：元数据已包装，但 `getSession()` 没挂中间件（没有图片缩放）。
    WrappedBare,
    /// 3.19.7 之前：扁平 `resolvedModelMetadata:oe(a,d)`，agentTokenLimit 写在 `a` 里。
    Flat,
}

const DIRECT_SHAPES: [DirectShape; 3] = [
    DirectShape::Current,
    DirectShape::WrappedBare,
    DirectShape::Flat,
];

/// Bot 通道怎么改 `requestedModel.modelId`。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PremiumPin {
    /// 不改，面板选什么送什么。
    Off,
    /// 最早一版：一律钉 `premium`。只给迁移 / 卸载认盘上已装的体。
    Always,
    /// 上一版：Grok / Composer / Auto 原样，其余钉 `premium`。
    ExceptNative,
    /// 现行：只有 GLM 5.2 钉 `premium`（面板入口）；其余原样。
    GlmOnly,
}

const PREMIUM_PINS: [PremiumPin; 4] = [
    PremiumPin::GlmOnly,
    PremiumPin::ExceptNative,
    PremiumPin::Always,
    PremiumPin::Off,
];

/// Current + 钉 premium 时，executor 回包 `modelId` 打到哪。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ResolvedLog {
    /// 不包 stream；`Off` / 旧形态用。
    None,
    /// 上一版：`console.info`。扩展宿主 DevTools 才看得到，Agent Host.log 没有。
    Console,
    /// 现行：跟官方同一条 `Cursor Agent Host` LogOutputChannel。
    AgentHost,
}

/// Bot 通道请求的 routed tier。服务端再解析成实际模型（本号目前是 `gpt-5.3-codex`）。
#[cfg_attr(not(test), allow(dead_code))]
const GROKBOT_FORCED_MODEL_ID: &str = "premium";
/// 给本地 prompt 装配用的 slug：`premium` 本身对不上任何 vendor 分支，按当前落点用 Codex。
#[cfg_attr(not(test), allow(dead_code))]
const GROKBOT_FORCED_PROMPT_SLUG: &str = "gpt-5.3-codex";
/// grok 4.7 在 Bot 通道只能走这个 CUA 别名。
#[cfg_attr(not(test), allow(dead_code))]
const GROKBOT_CUA_MODEL_ID: &str = "sand-cua";

/// GlmOnly 注入体的四代钉法。现行只装后两种；前两代必须留着，否则卸载认不出
/// 2026-09-13 当天装的盘（只钉 GLM、或 4.5→CUA 但还没有 remap 日志），写后校验
/// 会剩 1 处推理引擎 marker 并回滚。
#[derive(Clone, Copy, PartialEq, Eq)]
enum GlmOnlyGen {
    /// 只钉 GLM 5.2 → premium。
    Legacy,
    /// 再加 grok 4.7 → sand-cua。
    Grok47,
    /// 再加 grok 4.5 → sand-cua，还没有 `[nexus-sand] remap` 日志。
    /// 2026-09-13 下午之前装的「4.5 走 CUA」盘是这一代。
    Grok47And45Plain,
    /// 现行：4.5→CUA，并打 remap console.info。
    Grok47And45,
}

fn glm_only_pin_js(gen: GlmOnlyGen) -> &'static str {
    match gen {
        GlmOnlyGen::Legacy => concat!(
            r#"const k=String(n.modelId||"").toLowerCase(),"#,
            r#"pin=k.includes("glm-5.2")||k.includes("glm5.2")||k.includes("glm_5.2");"#,
            r#"if(pin){n.modelId="premium";n.maxMode=!1;n.parameters=[];}"#,
            r#"const d=String(n.modelId||""),i="premium"===d.toLowerCase()?"gpt-5.3-codex":d.toLowerCase(),"#,
        ),
        GlmOnlyGen::Grok47And45Plain => concat!(
            r#"const k=String(n.modelId||"").toLowerCase(),"#,
            r#"pin=k.includes("glm-5.2")||k.includes("glm5.2")||k.includes("glm_5.2"),"#,
            r#"g47=k.includes("grok-4.7")||k.includes("grok-4-7")||k.includes("4-7-0910"),"#,
            r#"g45=k.includes("grok-4.5")||k.includes("grok-4-5");"#,
            r#"if(pin){n.modelId="premium";n.maxMode=!1;n.parameters=[];}"#,
            r#"if(g47||g45){n.modelId="sand-cua";n.maxMode=!1;n.parameters=[];}"#,
            r#"const d=g47?"grok-4.7":g45?"grok-4.5":String(n.modelId||""),i="premium"===d.toLowerCase()?"gpt-5.3-codex":g47||g45?"grok-4.6":d.toLowerCase(),"#,
        ),
        GlmOnlyGen::Grok47And45 => concat!(
            r#"const k=String(n.modelId||"").toLowerCase(),"#,
            r#"pin=k.includes("glm-5.2")||k.includes("glm5.2")||k.includes("glm_5.2"),"#,
            r#"g47=k.includes("grok-4.7")||k.includes("grok-4-7")||k.includes("4-7-0910"),"#,
            r#"g45=k.includes("grok-4.5")||k.includes("grok-4-5");"#,
            r#"if(pin){n.modelId="premium";n.maxMode=!1;n.parameters=[];}"#,
            r#"if(g47||g45){n.modelId="sand-cua";n.maxMode=!1;n.parameters=[];try{console.info("[nexus-sand] remap",k,"→ sand-cua")}catch(e){}}"#,
            r#"const d=g47?"grok-4.7":g45?"grok-4.5":String(n.modelId||""),i="premium"===d.toLowerCase()?"gpt-5.3-codex":g47||g45?"grok-4.6":d.toLowerCase(),"#,
        ),
        GlmOnlyGen::Grok47 => concat!(
            r#"const k=String(n.modelId||"").toLowerCase(),"#,
            r#"pin=k.includes("glm-5.2")||k.includes("glm5.2")||k.includes("glm_5.2"),"#,
            r#"g47=k.includes("grok-4.7")||k.includes("grok-4-7")||k.includes("4-7-0910");"#,
            r#"if(pin){n.modelId="premium";n.maxMode=!1;n.parameters=[];}"#,
            r#"if(g47){n.modelId="sand-cua";n.maxMode=!1;n.parameters=[];}"#,
            r#"const d=g47?"grok-4.7":String(n.modelId||""),i="premium"===d.toLowerCase()?"gpt-5.3-codex":g47?"grok-4.6":d.toLowerCase(),"#,
        ),
    }
}

/// 现行形态；`context_window=false` 是更早的注入（还没有 agentTokenLimit）。
///
/// 产线走 [`direct_stream_injection_for_gen`]（它按 gen 分派形态），这个薄包装只留给
/// 逐字节比对 Python 参考实现的那几个测试。
#[cfg(test)]
fn direct_stream_injection(self_summary: bool, context_window: bool) -> String {
    direct_stream_injection_impl(
        self_summary,
        context_window,
        DirectShape::Current,
        PremiumPin::GlmOnly,
    )
}

fn default_resolved_log(shape: DirectShape, pin: PremiumPin) -> ResolvedLog {
    if pin != PremiumPin::Off && shape == DirectShape::Current {
        ResolvedLog::AgentHost
    } else {
        ResolvedLog::None
    }
}

fn direct_stream_injection_impl(
    self_summary: bool,
    context_window: bool,
    shape: DirectShape,
    pin: PremiumPin,
) -> String {
    direct_stream_injection_with_log(
        self_summary,
        context_window,
        shape,
        pin,
        default_resolved_log(shape, pin),
        false,
    )
}

fn direct_stream_injection_with_log(
    self_summary: bool,
    context_window: bool,
    shape: DirectShape,
    pin: PremiumPin,
    log: ResolvedLog,
    grok45_via_cua: bool,
) -> String {
    let wrap_metadata = shape != DirectShape::Flat;
    // 扁平旧版把 agentTokenLimit 写在 `a` 里；3.19.7 的 Fe() 只认外层包装上的那一份。
    let inner_limit = if context_window && !wrap_metadata {
        format!("agentTokenLimit:{CONTEXT_TOKENS_EXPR},")
    } else {
        String::new()
    };
    let grok46 = if wrap_metadata {
        r#"isGrok46ProductPrompt:i.includes("grok-4.6")||i.includes("grok46"),"#
    } else {
        ""
    };
    let metadata = if wrap_metadata {
        if context_window {
            format!("{{promptModelInfo:oe(a,d),agentTokenLimit:{CONTEXT_TOKENS_EXPR}}}")
        } else {
            "{promptModelInfo:oe(a,d)}".to_string()
        }
    } else {
        "oe(a,d)".to_string()
    };
    let middleware = if shape == DirectShape::Current {
        DIRECT_SESSION_MIDDLEWARE
    } else {
        ""
    };
    let pin_js = match pin {
        PremiumPin::GlmOnly => glm_only_pin_js(if grok45_via_cua {
            GlmOnlyGen::Grok47And45
        } else {
            GlmOnlyGen::Grok47
        }),
        PremiumPin::ExceptNative => concat!(
            r#"const k=String(n.modelId||"").toLowerCase(),"#,
            r#"q=k.includes("grok")||k.includes("composer")||"default"===k||"sand-default"===k||"sand-cua"===k||"auto"===k||k.startsWith("auto-")||"premium"===k;"#,
            r#"if(!q){n.modelId="premium";n.maxMode=!1;n.parameters=[];}"#,
            r#"const d=String(n.modelId||""),i=q&&"premium"!==k?d.toLowerCase():"gpt-5.3-codex","#,
        ),
        PremiumPin::Always => concat!(
            r#"n.modelId="premium";"#,
            r#"n.maxMode=!1;n.parameters=[];"#,
            r#"const d="premium",i="gpt-5.3-codex","#,
        ),
        PremiumPin::Off => r#"const d=String(n.modelId||""),i=d.toLowerCase(),"#,
    };
    [
        "{",
        SAND_DIRECT_STREAM_MARKER,
        concat!(
            r#"const n=t.requestedModel;"#,
            r#"if(void 0===n)throw new Error("Sand direct Stream requires requestedModel");"#,
        ),
        pin_js,
        concat!(
            r#"r=new Map(n.parameters.map(e=>[e.id,e.value])),"#,
            r#"s=new J(e,n,void 0,void 0).getSession("#,
        ),
        middleware,
        ")",
        match log {
            ResolvedLog::AgentHost => concat!(
                r#",p={getExecutor:e=>{const x=new o.Ycw(s.getExecutor(e)),f=x.stream.bind(x);"#,
                r#"return x.stream=function(){const r=f.apply(this,arguments);"#,
                r#"return r&&r.response&&r.response.then(v=>{const m=v&&v.modelId;"#,
                r#"m&&function(){try{require("vscode").window.createOutputChannel("Cursor Agent Host",{log:!0}).info("[nexus-sand] resolved "+JSON.stringify({requested:d,actual:String(m)}))}catch(e){}}()}).catch(()=>{}),r},x;}},"#,
            ),
            ResolvedLog::Console => concat!(
                r#",p={getExecutor:e=>{const x=new o.Ycw(s.getExecutor(e)),f=x.stream.bind(x);"#,
                r#"return x.stream=function(){const r=f.apply(this,arguments);"#,
                r#"return r&&r.response&&r.response.then(v=>{const m=v&&v.modelId;"#,
                r#"m&&console.info("[nexus-sand] premium resolved",String(m))}).catch(()=>{}),r},x;}},"#,
            ),
            ResolvedLog::None => r#",p={getExecutor:e=>new o.Ycw(s.getExecutor(e))},"#,
        },
        concat!(
            r#"a={vendor:i.includes("grok")?"xai":i.includes("gemini")?"gemini":"#,
            r#"i.includes("claude")||i.includes("opus")||i.includes("sonnet")||i.includes("fable")?"#,
            r#""anthropic":i.includes("gpt")||i.includes("codex")?"openai":"unknown","#,
            r#"promptVersion:"latest",reasoningEffort:r.get("effort"),"#,
        ),
        inner_limit.as_str(),
        r#"isGrok45ProductPrompt:i.includes("grok"),"#,
        grok46,
        concat!(
            r#"isClaude4x:i.includes("claude")||i.includes("opus")||i.includes("sonnet")||i.includes("fable"),"#,
            r#"isFable5:i.includes("fable-5"),"#,
            r#"isOpus5:i.includes("opus-5")||i.includes("opus5"),"#,
            r#"isOpus48:i.includes("opus-4.8")||i.includes("opus48"),"#,
            r#"isOpus46:i.includes("opus-4.6")||i.includes("opus46"),"#,
            r#"isOpus45:i.includes("opus-4.5")||i.includes("opus45"),"#,
            r#"isSonnet45:i.includes("sonnet-4.5")||i.includes("sonnet45"),"#,
            r#"isSonnet4:i.includes("sonnet-4")||i.includes("sonnet4"),"#,
            r#"isGemini3:i.includes("gemini-3")||i.includes("gemini3"),"#,
            r#"isGpt56:i.includes("gpt-5.6")||i.includes("gpt5.6"),"#,
            r#"isGpt55:i.includes("gpt-5.5")||i.includes("gpt5.5"),"#,
            r#"isGpt54:i.includes("gpt-5.4")||i.includes("gpt5.4"),"#,
            r#"isGpt53CodexSpark:i.includes("codex-spark"),"#,
            r#"isGpt53Codex:i.includes("gpt-5.3-codex"),"#,
            r#"isGpt52Codex:i.includes("gpt-5.2-codex"),"#,
            r#"isGpt51:i.includes("gpt-5.1")||i.includes("gpt51"),"#,
            r#"isGpt52:i.includes("gpt-5.2")||i.includes("gpt52"),"#,
            r#"isGpt5:i.includes("gpt-5"),"#,
            r#"isCodexFamily:i.includes("codex"),isGpt5Family:i.includes("gpt-5"),"#,
            r#"isFruitcake:i.includes("fruitcake"),"#,
            r#"isComposer1:i.includes("composer-1"),"#,
            r#"isComposer15:i.includes("composer-1.5")||i.includes("composer-15")||i.includes("composer15"),"#,
            r#"isComposer2:i.includes("composer-2"),"#,
            r#"isComposerMatterhorn:i.includes("matterhorn"),"#,
            r#"isRawTrainingSlug:!1};"#,
            r#"return{promptSession:s,promptToolSession:p,attempt:{resolvedModel:n,"#,
            r#"supportsSelfSummary:"#,
        ),
        if self_summary { "!0" } else { "!1" },
        ",routedModelDisplayName:d,resolvedModelMetadata:",
        metadata.as_str(),
        ",finish:()=>Promise.resolve()}}}",
    ]
    .concat()
}

// ---------------------------------------------------------------------------
// 推理端点改道（remote 专用）
// ---------------------------------------------------------------------------
//
// agent host 的 MultiProxyTransport 按 service.typeName 选 transport；`InferenceService` 没有
// override，走默认 `backendTransport`，也就是直连 `cursorCreds.backendUrl`（api2）。远程要么到不了
// api2，要么出口地区拿不到 claude / gpt，所以给它单建一条指向本地端点的 transport 再挂进路由表。
//
// 为什么不复用 Cursor 现成的本地转发：实测两条都不通。`agentBidiTransport` 的出口是 agent.v1/api5，
// InferenceService 打过去被拒；`backgroundComposerProxyTransport` 虽然同为 aiserver/api2 后端，但
// 本地端 demux 不路由 InferenceService，会话建起来后两分钟 idle 关闭。那两条是 service 专线。
//
// 两条规则必须同进同出：查表用的是 `e in map`，只挂路由不建 transport 会命中一个 `undefined`，
// 而不是回退到 backendTransport。所以它们共用一个 `RuleId`。

// 这两条是「在锚点后插入」，但**没有**用 `AnchoredInsert`：那个 kind 拿 `markers()[0]` 当整条
// 规则的幂等闸，而这里是同一个 `RuleId` 下的两处插入，共用一个闸会让第二处被第一处挡掉。
// 改成 `Literal`，并把锚点**后面**紧邻的那一段也纳入 `original`：这样 patched 里不再包含
// 连续的 original，apply 天然幂等，remove 也是精确反向替换。

/// 建 transport 的插入点：`originTransport` 那一条的结尾。同作用域里有 `this.transportFactory`。
const INFERENCE_TRANSPORT_ANCHOR: &str = "originTransport:this.transportFactory.createTransport({baseUrl:d,useHttp2:!1,maybeUseCppSpoofToken:!1,pingConfig:yield(0,R.getHttp2PingConfig)(this.host,R.Http2TransportCallSite.ORIGIN),getHttp1KeepaliveDisabled:t,http1KeepaliveInitialDelayMs:n}),";
/// 紧跟在它后面的下一个 transport，用来把 `original` 撑成「非前缀」。
const INFERENCE_TRANSPORT_NEXT: &str =
    "remoteAgentHostPresenceTransport:this.transportFactory.createTransport({baseUrl:o,";

/// 挂路由的插入点：`_updateTransportMap` 里 OriginService 那一行。
const INFERENCE_ROUTE_ANCHOR: &str = "this._overrideServiceNameToTransportMapLowerPriorityThanMethodOverrides[E.OriginService.typeName]=e.originTransport,";
/// 紧跟其后的那条：3.19.7 起在 Origin 和 AgentService.run 之间插了 Presence。
const INFERENCE_ROUTE_NEXT: &str =
    "this._overrideServiceNameToTransportMapLowerPriorityThanMethodOverrides[y.RemoteAgentHostPresenceService.typeName]=e.remoteAgentHostPresenceTransport,";

fn inference_transport_original() -> String {
    format!("{INFERENCE_TRANSPORT_ANCHOR}{INFERENCE_TRANSPORT_NEXT}")
}

fn inference_transport_patched(endpoint: &str) -> String {
    format!(
        "{INFERENCE_TRANSPORT_ANCHOR}sandInferenceTransport:this.transportFactory.createTransport({{baseUrl:\"{endpoint}\",useHttp2:!1,maybeUseCppSpoofToken:!1}}),{SAND_INFERENCE_ENDPOINT_MARKER}{INFERENCE_TRANSPORT_NEXT}"
    )
}

fn inference_route_original() -> String {
    format!("{INFERENCE_ROUTE_ANCHOR}{INFERENCE_ROUTE_NEXT}")
}

fn inference_route_patched() -> String {
    format!(
        "{INFERENCE_ROUTE_ANCHOR}this._overrideServiceNameToTransportMapLowerPriorityThanMethodOverrides[\"aiserver.v1.InferenceService\"]=e.sandInferenceTransport,{SAND_REMOTE_INFERENCE_ROUTE_MARKER}{INFERENCE_ROUTE_NEXT}"
    )
}

/// 端点得能安全地嵌进 JS 字符串字面量，而且不能踩中 bundle 自己的 localhost 判断。
///
/// `isDebug()` 认 `localhost` 和 `lclhst.build` 两个子串，措辞不当会把整个 agent host 切进
/// debug 分支；`127.0.0.1` 不在那两个串里，只影响 analytics 落点，所以用它。
pub fn validate_inference_endpoint(raw: &str) -> Result<String> {
    let endpoint = raw.trim().trim_end_matches('/');
    if !(endpoint.starts_with("http://") || endpoint.starts_with("https://")) {
        return Err(AppError::invalid(
            "推理端点要带 scheme，例如 http://127.0.0.1:8790。",
        ));
    }
    if endpoint
        .chars()
        .any(|c| c == '"' || c == '\\' || c.is_whitespace())
    {
        return Err(AppError::invalid("推理端点里不能有引号、反斜杠或空格。"));
    }
    if endpoint.contains("localhost") || endpoint.contains("lclhst.build") {
        return Err(AppError::invalid(
            "别用 localhost / lclhst.build（会触发 agent host 的 debug 分支），用 127.0.0.1。",
        ));
    }
    Ok(endpoint.to_string())
}

/// 盘上装的是哪个端点。`None` = 没改道。给 status 显示，也给 uninstall 复原用——
/// 端点 URL 是规则文本的一部分，卸载时必须拿装的时候那一个才能精确反向替换。
pub fn installed_inference_endpoint(content: &str) -> Option<String> {
    let head = "sandInferenceTransport:this.transportFactory.createTransport({baseUrl:\"";
    let start = content.find(head)? + head.len();
    let rest = content.get(start..)?;
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// 盘上 Direct 注入体里 `supportsSelfSummary` 的实际取值；该内容没装 Direct 引擎时 `None`。
/// 给 status 用：「盘上是什么」和「选项要装什么」是两回事，界面两个都要说
/// （Python：`_installed_direct_stream_self_summary`）。
pub fn installed_self_summary(content: &str) -> Option<bool> {
    let mut variants = direct_stream_variants();
    variants.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for v in variants {
        if content.contains(v.as_str()) {
            return Some(v.contains("supportsSelfSummary:!0,"));
        }
    }
    None
}

/// 盘上 Direct 注入体是否把 grok-4.5 改走 `sand-cua`。没装 Direct 时 `None`。
///
/// 只认这句针，不按「第一段匹配的完整注入体」猜：完整体互相是子串时会误报开/关，
/// 安装器再按误报去 no-op，Agent 就一直跑着旧的 4884.js。
pub fn installed_grok45_via_cua(content: &str) -> Option<bool> {
    if !content.contains(SAND_DIRECT_STREAM_MARKER) {
        return None;
    }
    Some(content.contains(r#"g45=k.includes("grok-4.5")"#))
}

fn glm_only_generations(pin: PremiumPin) -> &'static [GlmOnlyGen] {
    match pin {
        PremiumPin::GlmOnly => &[
            GlmOnlyGen::Legacy,
            GlmOnlyGen::Grok47,
            GlmOnlyGen::Grok47And45Plain,
            GlmOnlyGen::Grok47And45,
        ],
        _ => &[GlmOnlyGen::Grok47],
    }
}

/// Direct 引擎全部注入体：三代形态 × 自摘要 × context × premium 钉法 + Current 钉 premium 的
/// console / Agent Host 两套 wrap + GlmOnly 四代钉法 = 108 种。
fn direct_stream_variants() -> Vec<String> {
    let mut out = Vec::with_capacity(108);
    for pin in PREMIUM_PINS {
        for &gen in glm_only_generations(pin) {
            for self_summary in [true, false] {
                for context_window in [true, false] {
                    for shape in DIRECT_SHAPES {
                        if pin != PremiumPin::Off && shape == DirectShape::Current {
                            out.push(direct_stream_injection_for_gen(
                                self_summary,
                                context_window,
                                shape,
                                pin,
                                ResolvedLog::Console,
                                gen,
                            ));
                            out.push(direct_stream_injection_for_gen(
                                self_summary,
                                context_window,
                                shape,
                                pin,
                                ResolvedLog::AgentHost,
                                gen,
                            ));
                        } else {
                            out.push(direct_stream_injection_for_gen(
                                self_summary,
                                context_window,
                                shape,
                                pin,
                                ResolvedLog::None,
                                gen,
                            ));
                        }
                    }
                }
            }
        }
    }
    out
}

fn direct_stream_injection_for_gen(
    self_summary: bool,
    context_window: bool,
    shape: DirectShape,
    pin: PremiumPin,
    log: ResolvedLog,
    gen: GlmOnlyGen,
) -> String {
    match gen {
        GlmOnlyGen::Legacy if pin == PremiumPin::GlmOnly => {
            direct_stream_injection_legacy_glm_only(self_summary, context_window, shape, log)
        }
        GlmOnlyGen::Grok47And45Plain if pin == PremiumPin::GlmOnly => {
            direct_stream_injection_g45_plain(self_summary, context_window, shape, log)
        }
        _ => direct_stream_injection_with_log(
            self_summary,
            context_window,
            shape,
            pin,
            log,
            matches!(gen, GlmOnlyGen::Grok47And45 | GlmOnlyGen::Grok47And45Plain),
        ),
    }
}

/// 2026-09-13 下午之前的「4.5 走 CUA」：钉法和现行一样，只是还没有 remap console.info。
fn direct_stream_injection_g45_plain(
    self_summary: bool,
    context_window: bool,
    shape: DirectShape,
    log: ResolvedLog,
) -> String {
    let mut body = direct_stream_injection_with_log(
        self_summary,
        context_window,
        shape,
        PremiumPin::GlmOnly,
        log,
        true,
    );
    let current = glm_only_pin_js(GlmOnlyGen::Grok47And45);
    let plain = glm_only_pin_js(GlmOnlyGen::Grok47And45Plain);
    debug_assert!(body.contains(current));
    body = body.replacen(current, plain, 1);
    body
}

fn direct_stream_injection_legacy_glm_only(
    self_summary: bool,
    context_window: bool,
    shape: DirectShape,
    log: ResolvedLog,
) -> String {
    let mut body = direct_stream_injection_with_log(
        self_summary,
        context_window,
        shape,
        PremiumPin::GlmOnly,
        log,
        false,
    );
    let current = glm_only_pin_js(GlmOnlyGen::Grok47);
    let legacy = glm_only_pin_js(GlmOnlyGen::Legacy);
    debug_assert!(body.contains(current));
    body = body.replacen(current, legacy, 1);
    body
}

/// 推理引擎规则。`injection` 是当前选项的注入体（Python `_direct_stream_injection(self_summary)`），
/// 其余 Direct 变体 + 已下线 Session 引擎的空 marker 全是 `legacy_injections`：
/// install 见到任一种就原地换成当前形态（计 migrated 不计 hits），uninstall 全部都认。
fn inference_stream_rule(
    self_summary: bool,
    force_premium: bool,
    grok45_via_cua: bool,
) -> PatchRule {
    let injection = if force_premium {
        direct_stream_injection_with_log(
            self_summary,
            true,
            DirectShape::Current,
            PremiumPin::GlmOnly,
            ResolvedLog::AgentHost,
            grok45_via_cua,
        )
    } else {
        direct_stream_injection_impl(self_summary, true, DirectShape::Current, PremiumPin::Off)
    };
    let mut legacy_injections: Vec<String> = direct_stream_variants()
        .into_iter()
        .filter(|v| v != &injection)
        .collect();
    legacy_injections.push(LEGACY_SESSION_STREAM_MARKER.to_string());
    PatchRule {
        id: RuleId::InferenceStream,
        kind: RuleKind::AnchoredInsert {
            anchor: DIRECT_STREAM_ANCHOR.into(),
            injection,
            legacy_injections,
        },
    }
}

// ---- 16. agent host enablement --------------------------------------------------------
// Python `AGENT_HOST_ENABLEMENT_RE` / `_PATCH_RE`：`this._agentHostEnabled=n,` →
// `n=!0;/*marker*/this._agentHostEnabled=n,`。Python 是整文件 `if MARKER not in` + `count=1`。

fn enablement_enable(c: &Captures<'_>) -> String {
    format!(
        "{v}=!0;{SAND_AGENT_HOST_ENABLEMENT_MARKER}{}{v}{}",
        &c[1],
        &c[3],
        v = &c[2]
    )
}

fn enablement_disable(c: &Captures<'_>) -> String {
    if c[1] != c[3] {
        return c[0].to_string();
    }
    format!("{}{}{}", &c[2], &c[1], &c[4])
}

fn agent_host_enablement_rule() -> Result<PatchRule> {
    Ok(PatchRule {
        id: RuleId::AgentHostEnablement,
        kind: RuleKind::Regex {
            apply: re(&format!(r"(this\._agentHostEnabled=)({IDENT})(,)"))?,
            apply_repl: enablement_enable,
            skip_if_followed_by: None,
            skip_file_if_marked: true,
            remove: re(&format!(
                r"({IDENT})=!0;{}(this\._agentHostEnabled=)({IDENT})(,)",
                regex::escape(SAND_AGENT_HOST_ENABLEMENT_MARKER)
            ))?,
            remove_repl: enablement_disable,
            per_file_limit: Some(1),
        },
    })
}

// ---------------------------------------------------------------------------
// 组装
// ---------------------------------------------------------------------------

fn literal(
    id: RuleId,
    original: impl Into<String>,
    patched: impl Into<String>,
    legacy: Vec<String>,
) -> PatchRule {
    PatchRule {
        id,
        kind: RuleKind::Literal {
            original: original.into(),
            patched: patched.into(),
            legacy,
        },
    }
}

fn re(pattern: &str) -> Result<Regex> {
    Regex::new(pattern).map_err(|e| AppError::internal(format!("Sand 规则表里的正则无效：{e}")))
}

/// 全部规则，按选项实例化（选项只影响 `InferenceStream` 的形态 / 注入体和 `ManagedActionRoute` 档位）。
///
/// **顺序与 Python `apply_patch_to_content` 逐条一致**：KC 迁移在最前；`ManagedTaskTool` 的旧变体
/// 迁移先于对 `original` 的匹配（引擎 `Literal` 语义）；`InferenceStream` 与 `AgentHostEnablement`
/// 收尾。引擎按返回顺序执行 apply 与 remove（各类补丁改的文本互不重叠，remove 顺序不影响结果）。
///
/// 端点改道只按 `options.inference_endpoint` 生成；盘上已装着别的端点时用
/// [`catalog_with_installed`]，否则卸载 / 换端点都对不上。
pub fn catalog(options: &InstallOptions) -> Result<Vec<PatchRule>> {
    catalog_with_installed(options, None)
}

/// 盘上装着哪种 Grok Bot 鉴权。旧版 originTransport interceptor 算 `BoxRelay` 之外的历史形态，
/// 这里报成 `Direct`（它的语义就是直连），install 会迁走。
pub fn installed_grokbot_auth(content: &str) -> GrokBotAuthMode {
    if content.contains(SAND_GROK_BOX_RELAY_AUTH_MARKER) {
        GrokBotAuthMode::BoxRelay
    } else if content.contains(SAND_GROKBOT_DIRECT_AUTH_MARKER)
        || content.contains(LEGACY_SAND_GROKBOT_STREAM_AUTH_MARKER)
    {
        GrokBotAuthMode::Direct
    } else {
        GrokBotAuthMode::Off
    }
}

/// 盘上是否挂了任一种 Grok Bot 鉴权。
pub fn installed_grokbot_stream_auth(content: &str) -> bool {
    installed_grokbot_auth(content).is_on()
}

const GROK_RUNTIME_AUTH_METHOD_PREFIX: &str =
    "applyAuthorization(e,t){return a(this,void 0,void 0,function*(){";
const GROK_RUNTIME_AUTH_SUFFIX: &str = "if(t.overrideAuthToken){";
/// 3.19.13 里两个 `applyAuthorization`（agent-host / always-local）只差压缩后的变量声明顺序。
/// 两处都打：注入块按 `InferenceService/Stream` 守卫，别的请求原样落回官方逻辑。
const GROK_RUNTIME_AUTH_VAR_DECLS: &[&str] = &[
    "var n,r,o,s,i,a,l,c,u,d,m,p;", // cursor-agent-host/dist/main.js
    "var n,r,s,o,i,a,l,u,m,c,d,p;", // cursor-always-local/dist/main.js
];

fn grok_runtime_auth_original(vars: &str) -> String {
    format!("{GROK_RUNTIME_AUTH_METHOD_PREFIX}{vars}{GROK_RUNTIME_AUTH_SUFFIX}")
}

fn grok_runtime_auth_patched(vars: &str, block: &str) -> String {
    format!("{GROK_RUNTIME_AUTH_METHOD_PREFIX}{vars}{block}{GROK_RUNTIME_AUTH_SUFFIX}")
}

fn grokbot_block(mode: GrokBotAuthMode) -> Option<&'static str> {
    match mode {
        GrokBotAuthMode::Off => None,
        GrokBotAuthMode::BoxRelay => Some(crate::grokbot::grok_box_relay_auth_block()),
        GrokBotAuthMode::Direct => Some(crate::grokbot::grokbot_direct_auth_block()),
    }
}

/// 旧版 originTransport interceptor（迁移：apply 时卸掉）。
fn legacy_origin_transport_grokbot_patched() -> String {
    let iife = crate::grokbot::legacy_direct_stream_interceptor();
    let suffix = format!("interceptors:[{iife}],");
    let repl = format!("maybeUseCppSpoofToken:!1,{suffix}pingConfig:");
    let inner = INFERENCE_TRANSPORT_ANCHOR.replace("maybeUseCppSpoofToken:!1,pingConfig:", &repl);
    format!("{inner}{LEGACY_SAND_GROKBOT_STREAM_AUTH_MARKER}")
}

/// 某一形态的 Grok Bot 鉴权规则（两处 `applyAuthorization`）。另一形态作 `legacy`：盘上装着
/// Box Relay、界面改选 Direct（或反过来），install 原地换块，不必先卸。`Off` 没有规则——
/// 剥掉盘上的块走 `service` 的 strip 前置步骤（Apply 语义下 Literal 不会删）。
pub fn grokbot_auth_rules(mode: GrokBotAuthMode) -> Vec<PatchRule> {
    let Some(block) = grokbot_block(mode) else {
        return Vec::new();
    };
    let other = match mode {
        GrokBotAuthMode::BoxRelay => GrokBotAuthMode::Direct,
        _ => GrokBotAuthMode::BoxRelay,
    };
    let other_block = grokbot_block(other).expect("non-off");
    GROK_RUNTIME_AUTH_VAR_DECLS
        .iter()
        .map(|vars| {
            literal(
                RuleId::GrokBotStreamAuth,
                grok_runtime_auth_original(vars),
                grok_runtime_auth_patched(vars, block),
                vec![grok_runtime_auth_patched(vars, other_block)],
            )
        })
        .collect()
}

/// 兼容旧调用点：默认形态（Box Relay）。
pub fn grokbot_stream_auth_rules() -> Vec<PatchRule> {
    grokbot_auth_rules(GrokBotAuthMode::BoxRelay)
}

/// 旧 Nexus 直连 api2 interceptor → stock anchor（install 预迁移，命中 0 时 no-op）。
pub fn grokbot_legacy_migration_rules() -> Vec<PatchRule> {
    vec![literal(
        RuleId::GrokBotStreamAuth,
        legacy_origin_transport_grokbot_patched(),
        INFERENCE_TRANSPORT_ANCHOR.to_string(),
        vec![],
    )]
}

/// 卸载时剥掉旧 interceptor（只进 strip，不进 catalog）。
pub fn grokbot_legacy_interceptor_strip_rules() -> Vec<PatchRule> {
    vec![literal(
        RuleId::GrokBotStreamAuth,
        INFERENCE_TRANSPORT_ANCHOR.to_string(),
        legacy_origin_transport_grokbot_patched(),
        vec![],
    )]
}

/// 兼容旧调用点（uninstall strip 等）。
pub fn grokbot_stream_auth_rule() -> PatchRule {
    grokbot_stream_auth_rules()
        .into_iter()
        .next()
        .expect("grokbot rules")
}

/// 端点改道的两条规则：建 transport（URL 是规则文本的一部分）+ 挂路由（与 URL 无关）。
///
/// `legacy_endpoint` 是盘上**已经装着**的另一个 URL：给 transport 那条当 `legacy`，引擎会把它原地
/// 换成新 URL（计 migrated），「改了端口点重新安装」才不是静默 no-op；卸载时两种 URL 都认。
pub fn endpoint_rules(endpoint: &str, legacy_endpoint: Option<&str>) -> Result<Vec<PatchRule>> {
    let endpoint = validate_inference_endpoint(endpoint)?;
    let legacy = legacy_endpoint
        .map(validate_inference_endpoint)
        .transpose()?
        .filter(|old| *old != endpoint)
        .map(|old| vec![inference_transport_patched(&old)])
        .unwrap_or_default();
    Ok(vec![
        literal(
            RuleId::InferenceEndpoint,
            inference_transport_original(),
            inference_transport_patched(&endpoint),
            legacy,
        ),
        literal(
            RuleId::InferenceEndpoint,
            inference_route_original(),
            inference_route_patched(),
            vec![],
        ),
    ])
}

/// [`catalog`] 的完整形态：`installed_endpoint` 是盘上现在装着的端点（`installed_inference_endpoint`
/// 读出来的），`None` = 盘上没改道。
///
/// - 选项要装端点 B、盘上是 A：B 的规则带 A 作 legacy，install 原地迁移。
/// - 选项不装端点、盘上是 A：仍生成 A 的规则——status 的 dry-run 和 uninstall 的 Remove 计划都要
///   认得它；install 想把它**去掉**要走 `service` 里的 strip 前置步骤（Apply 语义下 Literal 不会删）。
pub fn catalog_with_installed(
    options: &InstallOptions,
    installed_endpoint: Option<&str>,
) -> Result<Vec<PatchRule>> {
    let mut rules = Vec::with_capacity(24);
    rules.push(kc_client_rule()?);
    rules.push(kc_eligibility_rule()?);
    rules.extend(client_rules()?);
    rules.extend(eligibility_rules());
    rules.push(literal(
        RuleId::ManagedLocalRoute,
        MANAGED_LOCAL_ROUTE_ORIGINAL,
        managed_local_route_patched(),
        vec![],
    ));
    rules.push(literal(
        RuleId::LocalRuntimeLoad,
        LOCAL_RUNTIME_LOAD_ORIGINAL,
        local_runtime_load_patched(),
        vec![],
    ));
    rules.push(literal(
        RuleId::AgentHostMoveExec,
        AGENT_HOST_MOVE_EXEC_ORIGINAL,
        agent_host_move_exec_patched(),
        vec![],
    ));
    rules.push(literal(
        RuleId::ManagedSubagentRoute,
        MANAGED_SUBAGENT_ROUTE_ORIGINAL,
        managed_subagent_route_patched(),
        vec![],
    ));
    rules.push(managed_action_route_rule(options.mode_gate));
    rules.push(literal(
        RuleId::SubagentResumeMode,
        SUBAGENT_RESUME_MODE_ORIGINAL,
        subagent_resume_mode_patched(),
        vec![],
    ));
    rules.push(literal(
        RuleId::SubagentInteractionBubble,
        SUBAGENT_INTERACTION_BUBBLE_ORIGINAL,
        subagent_interaction_bubble_patched(),
        vec![],
    ));
    rules.push(subagent_model_variants_rule()?);
    rules.push(literal(
        RuleId::ContextWindow,
        MAX_TOKENS_ORIGINAL,
        max_tokens_patched(),
        vec![],
    ));
    rules.push(subagent_completion_wake_rule()?);
    rules.push(literal(
        RuleId::ManagedSubagentSession,
        managed_subagent_session_original(),
        managed_subagent_session_patched(),
        vec![managed_subagent_session_patched_v1()],
    ));
    // 同 RuleId 的第二条：3.19.7 的 featureFlags 对象上补 enableBrowserSubagent。
    rules.push(literal(
        RuleId::ManagedSubagentSession,
        SUBAGENT_BROWSER_FLAG_ORIGINAL,
        subagent_browser_flag_patched(),
        vec![],
    ));
    rules.push(managed_task_tool_rule());
    rules.push(literal(
        RuleId::AgentHostIdentity,
        AGENT_HOST_IDENTITY_ORIGINAL,
        agent_host_identity_patched(),
        vec![],
    ));
    match (&options.inference_endpoint, installed_endpoint) {
        (Some(want), installed) => rules.extend(endpoint_rules(want, installed)?),
        (None, Some(installed)) => rules.extend(endpoint_rules(installed, None)?),
        (None, None) => {}
    }
    rules.extend(grokbot_auth_rules(options.grokbot_auth));
    rules.push(inference_stream_rule(
        options.self_summary,
        options.grokbot_auth.is_on(),
        options.grok45_via_cua,
    ));
    rules.push(agent_host_enablement_rule()?);
    Ok(rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_rule_id_has_a_slot_a_name_and_markers() {
        let mut c = MarkerCounts::default();
        for id in RuleId::ALL {
            *id.slot(&mut c) += 1;
            assert!(!id.name().is_empty());
            assert!(!id.markers().is_empty());
        }
        // 18 个字段各加了 1（GrokBotStreamAuth 默认开）。
        assert_eq!(c.total(), RuleId::ALL.len() as u32);
    }

    #[test]
    fn expectations_match_the_installer_hard_checks() {
        assert_eq!(RuleId::ClientType.expected(), Some(23));
        assert_eq!(RuleId::Eligibility.expected(), None);
        assert_eq!(RuleId::ManagedSubagentRoute.expected(), None);
        // 3.19.7 上这条改挂 enableBrowserSubagent（browser-use 子代理靠它），照常校验。
        assert_eq!(RuleId::ManagedSubagentSession.expected(), Some(1));
        assert_eq!(RuleId::AgentHostEnablement.expected(), Some(2));
        assert_eq!(RuleId::SubagentCompletionWake.expected(), Some(2));
        assert_eq!(RuleId::SubagentModelVariants.expected(), Some(2));
        assert_eq!(RuleId::InferenceStream.expected(), Some(1));
        assert_eq!(RuleId::ContextWindow.expected(), Some(1));
    }

    #[test]
    fn our_markers_are_not_flagged_as_foreign_by_the_guard() {
        // guard 模式必须能匹配到我们自己的 marker（这样「总数 − 我们的 = 外部」才成立）。
        let guard = Regex::new(CLIENT_MARKER_GUARD_PATTERN).unwrap();
        assert!(guard.is_match(SAND_CLIENT_MARKER));
        assert!(guard.is_match(SAND_CLIENT_EXISTING_MARKER));
        assert!(guard.is_match(LEGACY_KC_CLIENT_MARKER));
        let eg = Regex::new(ELIGIBILITY_MARKER_GUARD_PATTERN).unwrap();
        assert!(eg.is_match(SAND_ELIGIBILITY_MARKER));
    }

    #[test]
    fn scoped_preflight_anchor_only_counts_inside_files_holding_the_scope() {
        let runner = preflight_anchors()
            .into_iter()
            .find(|a| a.name == "task tool Ne body")
            .unwrap();
        let with_factory = format!(
            "{TASK_TOOL_FACTORY} … managed local loop does not build in-process child AgentConfig"
        );
        let without =
            "managed local loop does not build in-process child AgentConfig ×2 managed local loop does not build in-process child AgentConfig";
        // 全局 3 处，但只有含 factory 的文件里那 1 处算数。
        assert_eq!(runner.count([with_factory.as_str(), without]), 1);
        // 不带作用域的锚点照常全局计数。
        let factory = preflight_anchors()
            .into_iter()
            .find(|a| a.name == "task tool factory")
            .unwrap();
        assert_eq!(factory.count([with_factory.as_str(), without]), 1);
    }

    fn plain_install_options() -> InstallOptions {
        InstallOptions {
            grokbot_auth: GrokBotAuthMode::Off,
            ..InstallOptions::default()
        }
    }

    /// 带端点改道的选项：remote 的默认形态。
    fn options_with_endpoint() -> InstallOptions {
        InstallOptions {
            inference_endpoint: Some("http://127.0.0.1:8790".into()),
            grokbot_auth: GrokBotAuthMode::Off,
            ..InstallOptions::default()
        }
    }

    #[test]
    fn catalog_covers_all_nineteen_rule_ids_in_python_order() {
        // 端点改道是可选项，默认（本机安装）不该出现。
        let default_rules = catalog(&InstallOptions::default()).unwrap();
        assert!(!default_rules
            .iter()
            .any(|r| r.id == RuleId::InferenceEndpoint));
        assert!(default_rules
            .iter()
            .any(|r| r.id == RuleId::GrokBotStreamAuth));

        let rules = catalog(&options_with_endpoint()).unwrap();
        assert!(!rules.is_empty());
        for id in RuleId::ALL {
            if *id == RuleId::GrokBotStreamAuth {
                continue;
            }
            assert!(rules.iter().any(|r| r.id == *id), "{} 没有规则", id.name());
        }
        // 与 Python `apply_patch_to_content` 的顺序一致：KC 迁移在最前，inference stream 与
        // agent host enablement 收尾。
        let order: Vec<RuleId> = rules.iter().map(|r| r.id).collect();
        assert_eq!(order[0], RuleId::ClientType);
        assert_eq!(order[1], RuleId::Eligibility);
        assert_eq!(order[order.len() - 2], RuleId::InferenceStream);
        assert_eq!(order[order.len() - 1], RuleId::AgentHostEnablement);
        // client-type：KC 迁移 + 三条锚点 = 4 条正则。
        assert_eq!(
            rules.iter().filter(|r| r.id == RuleId::ClientType).count(),
            4
        );
        assert_eq!(
            rules.iter().filter(|r| r.id == RuleId::Eligibility).count(),
            7
        );
    }

    const ENDPOINT_A: &str = "http://127.0.0.1:8790";
    const ENDPOINT_B: &str = "http://127.0.0.1:8688";

    fn options_with(endpoint: &str) -> InstallOptions {
        InstallOptions {
            inference_endpoint: Some(endpoint.into()),
            grokbot_auth: GrokBotAuthMode::Off,
            ..InstallOptions::default()
        }
    }

    /// 端点 URL 是规则文本的一部分：换端口点「重新安装」必须原地把旧 URL 换成新的，
    /// 而不是留着旧的、新的又装不上（`original` 已不在）。
    #[test]
    fn endpoint_url_change_migrates_in_place() {
        let src = synthetic_bundle();
        let (with_a, rep_a) =
            crate::engine::apply(&src, &catalog(&options_with(ENDPOINT_A)).unwrap());
        assert_eq!(rep_a.hits.inference_endpoint, 2);
        assert_eq!(
            installed_inference_endpoint(&with_a).as_deref(),
            Some(ENDPOINT_A)
        );

        // 不知道盘上是 A 的规则表：B 的 original 已经不在，transport 那处装不上。
        let (stuck, rep) =
            crate::engine::apply(&with_a, &catalog(&options_with(ENDPOINT_B)).unwrap());
        assert_eq!(
            rep.hits.inference_endpoint + rep.migrated.inference_endpoint,
            0
        );
        assert_eq!(
            installed_inference_endpoint(&stuck).as_deref(),
            Some(ENDPOINT_A)
        );

        // 带上盘上的 A：transport 原地迁到 B（计 migrated），路由那处本就与 URL 无关。
        let rules_b = catalog_with_installed(&options_with(ENDPOINT_B), Some(ENDPOINT_A)).unwrap();
        let (with_b, rep_b) = crate::engine::apply(&with_a, &rules_b);
        assert_eq!(rep_b.hits.inference_endpoint, 0);
        assert_eq!(rep_b.migrated.inference_endpoint, 1);
        assert_eq!(
            installed_inference_endpoint(&with_b).as_deref(),
            Some(ENDPOINT_B)
        );
        assert!(!with_b.contains(ENDPOINT_A));
        assert_eq!(
            crate::engine::inspect(&with_b, &rules_b)
                .markers
                .inference_endpoint,
            2
        );
        // 与一开始就装 B 逐字节一致。
        let (direct_b, _) =
            crate::engine::apply(&src, &catalog(&options_with(ENDPOINT_B)).unwrap());
        assert_eq!(with_b, direct_b);
        // 幂等。
        assert_eq!(crate::engine::apply(&with_b, &rules_b).0, with_b);
    }

    /// 卸载 / status 拿的是默认选项（端点 None）；盘上装着端点时规则表必须从盘上把它带回来，
    /// 否则 Remove 计划反向不了那两处，写后校验又按常量数出 2 处残留。
    #[test]
    fn catalog_from_disk_lets_uninstall_reverse_an_installed_endpoint() {
        let src = synthetic_bundle();
        let (with_a, _) = crate::engine::apply(&src, &catalog(&options_with(ENDPOINT_A)).unwrap());

        let blind = catalog(&InstallOptions::default()).unwrap();
        let (left, _) = crate::engine::remove(&with_a, &blind);
        assert_eq!(
            crate::engine::inspect(&left, &blind)
                .markers
                .inference_endpoint,
            2,
            "不知道端点的规则表卸不掉它——这正是要从盘上读的原因"
        );

        let informed = catalog_with_installed(&plain_install_options(), Some(ENDPOINT_A)).unwrap();
        assert!(informed.iter().any(|r| r.id == RuleId::InferenceEndpoint));
        let (back, removed) = crate::engine::remove(&with_a, &informed);
        assert_eq!(removed.inference_endpoint, 2);
        assert_eq!(back, src);
        assert_eq!(installed_inference_endpoint(&back), None);
        // 选项不要端点、盘上有：Apply 不动它（Literal 不反向），这是 service 走 strip 的理由。
        let (kept, _) = crate::engine::apply(&with_a, &informed);
        assert_eq!(
            installed_inference_endpoint(&kept).as_deref(),
            Some(ENDPOINT_A)
        );
    }

    /// 界面把「经本机网关」关掉再点安装：先剥掉旧端点那两处，再打其余补丁，结果与一开始就
    /// 没装端点逐字节一致（`service::build_plan_with_strip` 的语义）。
    #[test]
    fn stripping_the_installed_endpoint_then_applying_equals_a_plain_install() {
        let src = synthetic_bundle();
        let plain = catalog(&plain_install_options()).unwrap();
        let (without, _) = crate::engine::apply(&src, &plain);
        let (with_a, _) = crate::engine::apply(
            &src,
            &catalog(&InstallOptions {
                inference_endpoint: Some(ENDPOINT_A.into()),
                grokbot_auth: GrokBotAuthMode::Off,
                ..InstallOptions::default()
            })
            .unwrap(),
        );

        let strip = endpoint_rules(ENDPOINT_A, None).unwrap();
        let (stripped, n) = crate::engine::remove(&with_a, &strip);
        assert_eq!(n.inference_endpoint, 2);
        let (after, _) = crate::engine::apply(&stripped, &plain);
        assert_eq!(after, without);
        assert_eq!(
            crate::engine::inspect(&after, &plain)
                .markers
                .inference_endpoint,
            0
        );
    }

    /// 真机 3.19.7 `workbench.desktop.main.js` 里组 selectedSubagentModels 的尾巴。glass 版标识符不同。
    const SUBAGENT_MODEL_VARIANTS_DESKTOP: &str = concat!(
        "return l.length===0?{}:{selectedSubagentModels:l.map(u=>{const h=s(u.name,o);",
        "return new Bx({modelId:u.name,maxMode:o,parameters:h.map(p=>new XP({id:p.id,value:p.value}))})})}}",
    );
    const SUBAGENT_MODEL_VARIANTS_GLASS: &str = concat!(
        "return c.length===0?{}:{selectedSubagentModels:c.map(u=>{const d=s(u.name,o);",
        "return new xP({modelId:u.name,maxMode:o,parameters:d.map(h=>new cO({id:h.id,value:h.value}))})})}}",
    );

    /// 一份把每类锚点各放一处（client-type 三种上下文各一处、端点改道两处）的迷你 bundle。
    fn synthetic_bundle() -> String {
        [
            r#"const t=isGlass?"glass":"ide";"#,
            r#"h={"x-cursor-client-type":"ide"};"#,
            r#"header.set("x-cursor-client-type",e.clientType??'ide');"#,
            ELIGIBILITY_PREFIXES[0],
            "){}",
            MANAGED_LOCAL_ROUTE_ORIGINAL,
            LOCAL_RUNTIME_LOAD_ORIGINAL,
            AGENT_HOST_MOVE_EXEC_ORIGINAL,
            MANAGED_SUBAGENT_ROUTE_ORIGINAL,
            MANAGED_ACTION_ROUTE_ORIGINAL,
            SUBAGENT_RESUME_MODE_ORIGINAL,
            SUBAGENT_INTERACTION_BUBBLE_ORIGINAL,
            SUBAGENT_MODEL_VARIANTS_DESKTOP,
            MAX_TOKENS_ORIGINAL,
            r#"if(g.source==="interactive-child"||g.payload.notificationContext==="user_driven_interactive_child"){}"#,
            &managed_subagent_session_original(),
            MANAGED_TASK_TOOL_ORIGINAL,
            AGENT_HOST_IDENTITY_ORIGINAL,
            &inference_transport_original(),
            &inference_route_original(),
            &grok_runtime_auth_original(GROK_RUNTIME_AUTH_VAR_DECLS[0]),
            &grok_runtime_auth_original(GROK_RUNTIME_AUTH_VAR_DECLS[1]),
            DIRECT_STREAM_ANCHOR,
            "yield 1}}}",
            "constructor(e,t,n){this._agentHostEnabled=n,this.x=1}",
        ]
        .join("\n")
    }

    #[test]
    fn grokbot_auth_rule_patches_apply_authorization() {
        let src = synthetic_bundle();
        let rules = catalog(&InstallOptions::default()).unwrap();
        let (patched, report) = crate::engine::apply(&src, &rules);
        assert_eq!(report.hits.grokbot_stream_auth, 2);
        assert!(patched.contains(SAND_GROK_BOX_RELAY_AUTH_MARKER));
        assert_eq!(installed_grokbot_auth(&patched), GrokBotAuthMode::BoxRelay);
        let (back, _) = crate::engine::remove(&patched, &rules);
        assert_eq!(installed_grokbot_auth(&back), GrokBotAuthMode::Off);
        assert_eq!(back, src);
    }

    /// Box Relay ↔ 直连互为 legacy：盘上装着一种、选另一种重装，原地换块（计 migrated），
    /// 两种形态的 uninstall 都回到原文。
    #[test]
    fn grokbot_auth_modes_migrate_in_place_and_both_uninstall() {
        let src = synthetic_bundle();
        let relay_rules = catalog(&InstallOptions::default()).unwrap();
        let direct_rules = catalog(&InstallOptions {
            grokbot_auth: GrokBotAuthMode::Direct,
            ..InstallOptions::default()
        })
        .unwrap();

        let (relay, _) = crate::engine::apply(&src, &relay_rules);
        let (direct, rep) = crate::engine::apply(&relay, &direct_rules);
        assert_eq!(
            rep.migrated.grokbot_stream_auth, 2,
            "两处 applyAuthorization 各迁一次"
        );
        assert_eq!(rep.hits.grokbot_stream_auth, 0);
        assert_eq!(installed_grokbot_auth(&direct), GrokBotAuthMode::Direct);
        assert!(!direct.contains(SAND_GROK_BOX_RELAY_AUTH_MARKER));
        assert!(direct.contains(SAND_GROKBOT_DIRECT_AUTH_MARKER));

        let (back_again, rep2) = crate::engine::apply(&direct, &relay_rules);
        assert_eq!(rep2.migrated.grokbot_stream_auth, 2);
        assert_eq!(
            installed_grokbot_auth(&back_again),
            GrokBotAuthMode::BoxRelay
        );

        // 用另一形态的规则表卸载也行（legacy 认得）。
        let (stock, removed) = crate::engine::remove(&direct, &relay_rules);
        assert_eq!(stock, src);
        assert_eq!(removed.grokbot_stream_auth, 2);
        let (stock2, _) = crate::engine::remove(&relay, &direct_rules);
        assert_eq!(stock2, src);
    }

    /// 第一版直连 interceptor（挂 originTransport，从未生效）：install 表里的迁移规则把它还成 stock
    /// 锚点，端点改道那条规则随即能命中；strip 表在「关」时也能剥掉它。
    #[test]
    fn legacy_origin_interceptor_is_migrated_away_before_endpoint_rules() {
        let src = synthetic_bundle();
        let legacy_on_disk = src.replace(
            &inference_transport_original(),
            &format!(
                "{}{INFERENCE_TRANSPORT_NEXT}",
                legacy_origin_transport_grokbot_patched()
            ),
        );
        assert_ne!(legacy_on_disk, src);
        assert_eq!(
            installed_grokbot_auth(&legacy_on_disk),
            GrokBotAuthMode::Direct
        );

        let mut rules = grokbot_legacy_migration_rules();
        rules.extend(catalog(&options_with_endpoint()).unwrap());
        let (patched, rep) = crate::engine::apply(&legacy_on_disk, &rules);
        assert!(!patched.contains(LEGACY_SAND_GROKBOT_STREAM_AUTH_MARKER));
        assert_eq!(rep.hits.inference_endpoint, 2, "迁移之后端点改道照常命中");

        let (stripped, _) =
            crate::engine::remove(&legacy_on_disk, &grokbot_legacy_interceptor_strip_rules());
        assert_eq!(stripped, src);
    }

    #[test]
    fn synthetic_bundle_round_trips_and_hits_every_rule_once() {
        let opts = options_with_endpoint();
        let rules = catalog(&opts).unwrap();
        let src = synthetic_bundle();
        let (patched, report) = crate::engine::apply(&src, &rules);
        let ins = crate::engine::inspect(&patched, &rules);
        for id in RuleId::ALL {
            let want = match *id {
                RuleId::ClientType => 3,
                // 建 transport + 挂路由，两处
                RuleId::InferenceEndpoint => 2,
                RuleId::GrokBotStreamAuth => u32::from(opts.grokbot_auth.is_on()) * 2,
                _ => 1,
            };
            assert_eq!(id.get(&report.hits), want, "{} hits", id.name());
            assert_eq!(id.get(&ins.markers), want, "{} markers", id.name());
        }
        assert_eq!(report.migrated.total(), 0);
        assert_eq!(ins.remaining_ide, 0);
        assert_eq!(ins.foreign, 0);
        assert_eq!(ins.legacy, 0);
        // 引号风格保留：单引号的 set_header 位置换成 'sand'。
        assert!(patched.contains(r#"??'sand'/*SAND_CLIENT_MODE_V1*/"#));

        let (again, rep2) = crate::engine::apply(&patched, &rules);
        assert_eq!(again, patched, "apply 必须幂等");
        assert_eq!(rep2.hits.total() + rep2.migrated.total(), 0);

        let (back, removed) = crate::engine::remove(&patched, &rules);
        assert_eq!(back, src, "remove(apply(x)) 必须逐字节等于 x");
        assert_eq!(removed.client_type, 3);
        let expected_removed: u32 = RuleId::ALL
            .iter()
            .map(|id| match id {
                RuleId::ClientType => 3,
                RuleId::InferenceEndpoint => 2,
                RuleId::GrokBotStreamAuth => u32::from(opts.grokbot_auth.is_on()) * 2,
                _ => 1,
            })
            .sum();
        assert_eq!(removed.total(), expected_removed);
        assert!(!crate::engine::inspect(&back, &rules).touched());
    }

    #[test]
    fn subagent_model_variants_rewrite_both_workbench_flavours_and_round_trip() {
        let rules = catalog(&InstallOptions::default()).unwrap();
        for (src, requested_model_cls, model_param) in [
            (SUBAGENT_MODEL_VARIANTS_DESKTOP, "Bx", "p"),
            (SUBAGENT_MODEL_VARIANTS_GLASS, "xP", "h"),
        ] {
            let (patched, rep) = crate::engine::apply(src, &rules);
            assert_eq!(rep.hits.subagent_model_variants, 1, "{src}");
            // 原条目保留在前，legacySlugs 各造一条，参数留空、maxMode 跟父会话。
            assert!(patched.contains(&format!(
                "return[new {requested_model_cls}({{modelId:u.name,maxMode:o,parameters:"
            )));
            assert!(patched.contains(&format!("{model_param}=>new ")));
            assert!(patched.contains(&format!(
                r#",...(u.legacySlugs??[]).filter(g=>g&&g!==u.name).map(g=>new {requested_model_cls}({{modelId:g,maxMode:o,parameters:[]}}))]{SAND_SUBAGENT_MODEL_VARIANTS_MARKER}}})}}}}"#
            )));
            assert!(patched.contains(".flatMap(u=>{"));
            let (again, rep2) = crate::engine::apply(&patched, &rules);
            assert_eq!(again, patched, "apply 必须幂等");
            assert_eq!(rep2.hits.subagent_model_variants, 0);
            let (back, removed) = crate::engine::remove(&patched, &rules);
            assert_eq!(back, src, "remove(apply(x)) 必须逐字节等于 x");
            assert_eq!(removed.subagent_model_variants, 1);
        }
        // 标识符对不上（Python 的 `\2` 不会匹配的位置）：原样保留，不计命中。
        let mismatch = "selectedSubagentModels:l.map(u=>{const h=s(x.name,o);return new Bx({modelId:u.name,maxMode:o,parameters:h.map(p=>new XP({id:p.id,value:p.value}))})})";
        let (same, rep) = crate::engine::apply(mismatch, &rules);
        assert_eq!(same, mismatch);
        assert_eq!(rep.hits.subagent_model_variants, 0);
    }

    #[test]
    fn client_type_adopts_existing_sand_and_migrates_kc_markers() {
        let rules = catalog(&InstallOptions::default()).unwrap();
        let src = concat!(
            r#"isGlass?"glass":"sand" "#, // 别的渠道已改成 sand → 接管
            r#"isGlass?"glass":"sand"/*KC_SAND_CLIENT_V1*/ "#, // KC 旧标记 → 迁移
            r#"function r4g(e){return!1;/*KC_SAND_ELIGIBILITY_V1*/const{adminSettingsService:t"#,
        );
        let (patched, report) = crate::engine::apply(src, &rules);
        assert_eq!(report.hits.client_type, 2);
        assert!(patched.contains(r#""sand"/*SAND_CLIENT_EXISTING_V1*/"#));
        assert!(patched.contains(r#""sand"/*SAND_CLIENT_MODE_V1*/ "#));
        assert!(!patched.contains("KC_SAND"));
        assert!(patched.contains(&format!("return!1;{SAND_ELIGIBILITY_MARKER}")));
        let ins = crate::engine::inspect(&patched, &rules);
        assert_eq!(ins.markers.client_type, 2);
        assert_eq!(ins.markers.eligibility, 1);
        assert_eq!((ins.legacy, ins.foreign, ins.remaining_ide), (0, 0, 0));

        // 卸载：EXISTING 还回 sand；KC 迁移过来的还成 ide；KC 资格标记整段删掉。
        let (back, _) = crate::engine::remove(&patched, &rules);
        assert_eq!(
            back,
            concat!(
                r#"isGlass?"glass":"sand" "#,
                r#"isGlass?"glass":"ide" "#,
                r#"function r4g(e){const{adminSettingsService:t"#,
            )
        );
        // 直接卸 KC 标记（不先 install）也走同一条路。
        let (direct, removed) = crate::engine::remove(src, &rules);
        assert_eq!(direct, back);
        assert_eq!(removed.client_type, 1);
        assert_eq!(removed.eligibility, 1);
    }

    #[test]
    fn action_route_tier_is_selected_by_options_and_other_tiers_migrate() {
        let agent = catalog(&InstallOptions::default()).unwrap();
        let plan = catalog(&InstallOptions {
            mode_gate: ModeGate::AgentPlan,
            ..Default::default()
        })
        .unwrap();
        let (with_agent, _) = crate::engine::apply(MANAGED_ACTION_ROUTE_ORIGINAL, &agent);
        assert!(with_agent.contains(
            "function(e){return e.requestedMode===o.xy.AGENT||e.isHostedSubagentChild&&e.requestedMode===o.xy.UNSPECIFIED}"
        ));
        let (with_plan, rep) = crate::engine::apply(&with_agent, &plan);
        assert_eq!(rep.migrated.managed_action_route, 1);
        assert!(with_plan.contains(
            "function(e){return e.requestedMode===o.xy.AGENT||e.requestedMode===o.xy.PLAN||e.isHostedSubagentChild&&e.requestedMode===o.xy.UNSPECIFIED}"
        ));
        let all = catalog(&InstallOptions {
            mode_gate: ModeGate::All,
            ..Default::default()
        })
        .unwrap();
        let (with_all, _) = crate::engine::apply(&with_plan, &all);
        assert!(with_all.contains("function(e){return!0}(e)?"));
        // 任何档位的规则表都能卸任何档位。
        for rules in [&agent, &plan, &all] {
            for content in [&with_agent, &with_plan, &with_all] {
                let (back, removed) = crate::engine::remove(content, rules);
                assert_eq!(back, MANAGED_ACTION_ROUTE_ORIGINAL);
                assert_eq!(removed.managed_action_route, 1);
            }
        }
    }

    #[test]
    fn legacy_task_tool_and_session_variants_migrate_in_place() {
        let rules = catalog(&InstallOptions::default()).unwrap();
        for old in [
            managed_task_tool_patched_v124(),
            managed_task_tool_patched_v125(),
            managed_task_tool_patched_v2(),
            managed_task_tool_patched_v3(),
            managed_task_tool_patched_v4(),
            managed_task_tool_patched_v5(),
            managed_task_tool_patched_v6(),
        ] {
            let ins = crate::engine::inspect(&old, &rules);
            assert_eq!((ins.markers.managed_task_tool, ins.legacy), (0, 1));
            let (migrated, rep) = crate::engine::apply(&old, &rules);
            assert_eq!(migrated, managed_task_tool_patched());
            assert_eq!(rep.migrated.managed_task_tool, 1);
            let (back, _) = crate::engine::remove(&old, &rules);
            assert_eq!(back, MANAGED_TASK_TOOL_ORIGINAL);
        }
        let (migrated, rep) = crate::engine::apply(&managed_subagent_session_patched_v1(), &rules);
        assert_eq!(migrated, managed_subagent_session_patched());
        assert_eq!(rep.migrated.managed_subagent_session, 1);
        let (back, _) = crate::engine::remove(&managed_subagent_session_patched_v1(), &rules);
        assert_eq!(back, managed_subagent_session_original());
    }

    /// 子代理目录由 feature flag 决定，缺一个就是 `cannot resolve subagent type "..."`。
    /// browser 这个 flag 是 2026-09-04 补的：在此之前 `browser-use` 子代理一直解析不出来。
    /// 3.19.7 的 featureFlags 对象和 3.18.x 的 `const xre={…}` 不是同一串，所以它是独立一条规则。
    #[test]
    fn browser_flag_is_appended_to_the_3197_feature_flags_object() {
        let patched = subagent_browser_flag_patched();
        assert!(patched.contains("enableBrowserSubagent:!0"));
        assert!(patched.contains(SAND_MANAGED_SUBAGENT_SESSION_MARKER));
        // 只在对象收尾处追加，上游那串键一个都不动。
        assert!(patched.starts_with(SUBAGENT_BROWSER_FLAG_ORIGINAL.trim_end_matches("};")));
        assert!(patched.ends_with("};"));
        // 打完不再含原文（否则 apply 不幂等、remove 会二次匹配）。
        assert!(!patched.contains(SUBAGENT_BROWSER_FLAG_ORIGINAL));

        // 和 3.18.x 那条规则共用 RuleId / marker，但锚点必须互不重叠：一个 bundle 上只该有一条命中。
        let rules = catalog(&InstallOptions::default()).unwrap();
        let n = rules
            .iter()
            .filter(|r| r.id == RuleId::ManagedSubagentSession)
            .count();
        assert_eq!(n, 2, "3.18.x 卸旧装那条 + 3.19.7 加 browser 那条");
        assert!(!managed_subagent_session_original().contains(SUBAGENT_BROWSER_FLAG_ORIGINAL));

        // 幂等 + 可逆。
        let src = format!("const q={{a:!0{SUBAGENT_BROWSER_FLAG_ORIGINAL}");
        let (p1, rep) = crate::engine::apply(&src, &rules);
        assert_eq!(rep.hits.managed_subagent_session, 1);
        assert!(p1.contains("enableBrowserSubagent:!0"));
        let (p2, rep2) = crate::engine::apply(&p1, &rules);
        assert_eq!(p2, p1, "apply 必须幂等");
        assert_eq!(rep2.hits.managed_subagent_session, 0);
        let (back, removed) = crate::engine::remove(&p1, &rules);
        assert_eq!(back, src, "remove(apply(x)) 必须逐字节等于 x");
        assert_eq!(removed.managed_subagent_session, 1);
    }

    #[test]
    fn task_tool_variants_match_the_python_builders_shape() {
        let current = managed_task_tool_patched();
        // V7：替换官方 Ae() 调用点，父模型名取客户端 id，第三把键是官方 parentModelId。
        assert!(current.starts_with(
            "isGenerateImageModelRestricted:!1,taskToolProps:{/*SAND_MANAGED_TASK_TOOL_V7*/parentRequestedModelName:e.requestedModel.modelId,"
        ));
        assert!(current.contains(
            r#"subagentModels:{modelsBySlug:new Map([...(e.runOptions.selectedSubagentModels??[]).map(m=>m.modelId).filter(m=>m&&"default"!==m).map(m=>[m,{slug:m}]),[e.requestedModel.modelId,{slug:e.requestedModel.modelId}],[null!=p?p:n.modelName,{slug:null!=p?p:n.modelName}]])},"#
        ));
        assert!(!current.contains("opus-5"), "V7 不该再含任何硬编码 slug");
        assert!(current.contains("enableGrindSwarmSubagent:!0,"));
        assert!(current.contains("isModelValid:()=>!0,"));
        let v6 = managed_task_tool_patched_v6();
        assert!(v6.contains(
            "/*SAND_MANAGED_TASK_TOOL_V6*/parentRequestedModelName:e.requestedModel.modelId,"
        ));
        assert!(v6.contains(r#",[i,{slug:i}]])},normalizeCustomSubagents:e=>e,"#));
        let v5 = managed_task_tool_patched_v5();
        assert!(v5.contains("/*SAND_MANAGED_TASK_TOOL_V5*/parentRequestedModelName:i,"));
        assert!(
            v5.contains(r#".map(m=>[m,{slug:m}]),[i,{slug:i}]])},normalizeCustomSubagents:e=>e,"#)
        );
        assert!(!v5.contains("e.requestedModel.modelId,{slug:"));
        let v4 = managed_task_tool_patched_v4();
        assert!(v4.contains("/*SAND_MANAGED_TASK_TOOL_V4*/"));
        assert!(v4.contains(r#"new Map([["claude-4.5-sonnet",{slug:"claude-4.5-sonnet"}],"#));
        assert!(v4.contains(r#"["gpt-5.3-codex",{slug:"gpt-5.3-codex"}],[i,{slug:i}]])"#));
        assert!(v4.contains("normalizeCustomSubagents:e=>e,"));
        let v3 = managed_task_tool_patched_v3();
        assert!(
            v3.contains("/*SAND_MANAGED_TASK_TOOL_V3*/")
                && v3.contains("normalizeCustomSubagents:()=>[],")
        );
        let v2 = managed_task_tool_patched_v2();
        assert!(v2.contains("/*SAND_MANAGED_TASK_TOOL_V2*/"));
        assert!(v2.contains("subagentModels:{modelsBySlug:new Map([[i,{slug:i}]])},"));
        assert!(v2.contains("isModelValid:e=>e===i,"));
        let v125 = managed_task_tool_patched_v125();
        assert!(v125.contains("/*SAND_MANAGED_TASK_TOOL_V1*/"));
        assert!(v125.contains("subagentModels:{modelsBySlug:new Map},"));
        assert!(v125.contains("subagentTypeName?void 0:{"));
        let v124 = managed_task_tool_patched_v124();
        assert!(v124.starts_with(
            "isGenerateImageModelRestricted:!1,taskToolProps:{/*SAND_MANAGED_TASK_TOOL_V1*/"
        ));
        assert!(v124.contains("normalizeCustomSubagents:e=>e,"));
        // 八个变体两两不同。
        let all = [current, v6, v5, v4, v3, v2, v125, v124];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn direct_stream_injection_variants() {
        let off_ctx = direct_stream_injection(false, true);
        let on_ctx = direct_stream_injection(true, true);
        let off_noctx = direct_stream_injection(false, false);
        assert!(
            off_ctx.starts_with("{/*SAND_DIRECT_INFERENCE_STREAM_V1*/const n=t.requestedModel;")
        );
        assert!(off_ctx
            .contains(r#"pin=k.includes("glm-5.2")||k.includes("glm5.2")||k.includes("glm_5.2")"#));
        assert!(off_ctx.contains(&format!(
            r#"if(pin){{n.modelId="{GROKBOT_FORCED_MODEL_ID}";n.maxMode=!1;n.parameters=[];}}"#
        )));
        assert!(off_ctx.contains(&format!(
            r#"if(g47){{n.modelId="{GROKBOT_CUA_MODEL_ID}";n.maxMode=!1;n.parameters=[];}}"#
        )));
        assert!(off_ctx.contains(
            r#"g47=k.includes("grok-4.7")||k.includes("grok-4-7")||k.includes("4-7-0910")"#
        ));
        assert!(off_ctx.contains(&format!(
            r#"i="premium"===d.toLowerCase()?"{GROKBOT_FORCED_PROMPT_SLUG}":g47?"grok-4.6":d.toLowerCase()"#
        )));
        assert!(off_ctx.contains(
            r#"require("vscode").window.createOutputChannel("Cursor Agent Host",{log:!0})"#
        ));
        assert!(off_ctx.contains(
            r#".info("[nexus-sand] resolved "+JSON.stringify({requested:d,actual:String(m)}))"#
        ));
        assert!(!off_ctx.contains(r#"console.info("[nexus-sand] premium resolved""#));
        assert!(off_ctx.contains("supportsSelfSummary:!1,routedModelDisplayName:d,"));
        assert!(on_ctx.contains("supportsSelfSummary:!0,routedModelDisplayName:d,"));
        assert!(off_ctx.contains(r#"reasoningEffort:r.get("effort"),isGrok45ProductPrompt:"#));
        assert!(off_ctx.contains(&format!(
            r#"resolvedModelMetadata:{{promptModelInfo:oe(a,d),agentTokenLimit:{CONTEXT_TOKENS_EXPR}}}"#
        )));
        assert!(off_noctx.contains(r#"resolvedModelMetadata:{promptModelInfo:oe(a,d)},"#));
        assert!(off_ctx
            .contains(r#"isGrok46ProductPrompt:i.includes("grok-4.6")||i.includes("grok46"),"#));
        let flat = direct_stream_injection_impl(false, true, DirectShape::Flat, PremiumPin::Off);
        assert!(flat.contains(&format!(
            r#"reasoningEffort:r.get("effort"),agentTokenLimit:{CONTEXT_TOKENS_EXPR},isGrok45ProductPrompt:"#
        )));
        assert!(flat.ends_with(
            r#"attempt:{resolvedModel:n,supportsSelfSummary:!1,routedModelDisplayName:d,resolvedModelMetadata:oe(a,d),finish:()=>Promise.resolve()}}}"#
        ));
        // 3.19.7 的符号名；getSession 挂的是官方 ve 无 promptConfig 时的那条中间件链。
        for needle in [
            &format!("s=new J(e,n,void 0,void 0).getSession({DIRECT_SESSION_MIDDLEWARE}),"),
            r#"(0,o.sXH)((0,o.got)({imageResizing:{webpWithoutCodec:"passthrough"},"#,
            "supportsAssistantMessagePrefill:!0},{}))",
            "p={getExecutor:e=>{const x=new o.Ycw(s.getExecutor(e)),f=x.stream.bind(x);",
            r#"isGpt53CodexSpark:i.includes("codex-spark"),"#,
            r#"isGpt51:i.includes("gpt-5.1")||i.includes("gpt51"),"#,
            r#"isGpt52:i.includes("gpt-5.2")||i.includes("gpt52"),"#,
            r#"isGpt5:i.includes("gpt-5"),"#,
            r#"isFruitcake:i.includes("fruitcake"),"#,
            r#"isComposerMatterhorn:i.includes("matterhorn"),"#,
            "isRawTrainingSlug:!1};",
        ] {
            assert!(off_ctx.contains(needle), "缺 {needle}");
        }
        // 3.19.7 首版没挂中间件；扁平旧版更没有。两种都得和现行体不同，才能被识别成 legacy。
        let bare =
            direct_stream_injection_impl(false, true, DirectShape::WrappedBare, PremiumPin::Off);
        assert!(bare.contains("s=new J(e,n,void 0,void 0).getSession(),"));
        assert!(!bare.contains("o.sXH"));
        assert!(bare.contains(r#"resolvedModelMetadata:{promptModelInfo:oe(a,d),"#));
        assert!(flat.contains("s=new J(e,n,void 0,void 0).getSession(),"));
        assert_ne!(bare, off_ctx);
        assert_ne!(bare, flat);
        assert_eq!(direct_stream_variants().len(), 108);
        assert!(!off_ctx.contains(r#"g45=k.includes("grok-4.5")"#));
        assert!(direct_stream_variants().iter().any(|v| {
            v.contains(r#"console.info("[nexus-sand] premium resolved""#)
                && v.contains(r#"pin=k.includes("glm-5.2")"#)
        }));

        // 选项决定装哪种：自摘要开装 !0，显式关掉装 !1；全部变体都能卸。
        let anchor_src = format!("{DIRECT_STREAM_ANCHOR}body}}");
        let direct_rules = catalog(&InstallOptions::default()).unwrap();
        let no_summary_rules = catalog(&InstallOptions {
            self_summary: false,
            ..Default::default()
        })
        .unwrap();
        let (p_default, rep) = crate::engine::apply(&anchor_src, &direct_rules);
        assert!(p_default.contains("supportsSelfSummary:!0,"));
        assert!(p_default.contains("Sand direct Stream requires requestedModel"));
        assert_eq!(rep.hits.inference_stream, 1);
        assert_eq!(installed_self_summary(&p_default), Some(true));
        let (p_off, _) = crate::engine::apply(&anchor_src, &no_summary_rules);
        assert!(p_off.contains("supportsSelfSummary:!1,"));
        assert_eq!(installed_self_summary(&p_off), Some(false));
        assert_eq!(installed_self_summary(&anchor_src), None);
        for variant in [
            &p_default,
            &p_off,
            &format!("{DIRECT_STREAM_ANCHOR}{off_noctx}body}}"),
            &format!(
                "{DIRECT_STREAM_ANCHOR}{}body}}",
                direct_stream_injection(true, false)
            ),
            &format!("{DIRECT_STREAM_ANCHOR}{flat}body}}"),
            &format!("{DIRECT_STREAM_ANCHOR}{bare}body}}"),
        ] {
            let (back, removed) = crate::engine::remove(variant, &direct_rules);
            assert_eq!(back, anchor_src);
            assert_eq!(removed.inference_stream, 1);
        }

        // 盘上装着 !1（老默认）→ 用当前默认 install：原地切到 !0，不必先卸；反向同理。
        // 早期无 agentTokenLimit 的注入也走同一条迁移路。
        let (flipped_on, rep) = crate::engine::apply(&p_off, &direct_rules);
        assert_eq!(flipped_on, p_default);
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );
        let (flipped_off, rep) = crate::engine::apply(&p_default, &no_summary_rules);
        assert_eq!(flipped_off, p_off);
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );
        let early = format!("{DIRECT_STREAM_ANCHOR}{off_noctx}body}}");
        let (upgraded, rep) = crate::engine::apply(&early, &direct_rules);
        assert_eq!(upgraded, p_default);
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );
        // 盘上是 3.19.7 之前的扁平 oe()：原地包进 promptModelInfo。
        let flat_on_disk = format!(
            "{DIRECT_STREAM_ANCHOR}{}body}}",
            direct_stream_injection_impl(true, true, DirectShape::Flat, PremiumPin::Off)
        );
        let (from_flat, rep) = crate::engine::apply(&flat_on_disk, &direct_rules);
        assert_eq!(from_flat, p_default);
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );
        // 盘上是 3.19.7 首版（已包装、没中间件）：原地补上中间件链。
        let bare_on_disk = format!(
            "{DIRECT_STREAM_ANCHOR}{}body}}",
            direct_stream_injection_impl(true, true, DirectShape::WrappedBare, PremiumPin::Off)
        );
        let (from_bare, rep) = crate::engine::apply(&bare_on_disk, &direct_rules);
        assert_eq!(from_bare, p_default);
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );
        // 盘上是钉 premium 之前的现行体：原地改 modelId，不叠加。
        let pre_premium = format!(
            "{DIRECT_STREAM_ANCHOR}{}body}}",
            direct_stream_injection_impl(true, true, DirectShape::Current, PremiumPin::Off)
        );
        assert!(!pre_premium.contains(r#"n.modelId="premium""#));
        let (from_old, rep) = crate::engine::apply(&pre_premium, &direct_rules);
        assert_eq!(from_old, p_default);
        assert!(from_old.contains(r#"if(pin){n.modelId="premium""#));
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );

        // Bot 关着：不钉 premium，沿用面板选的模型；再打开 Bot 会原地迁过去。
        let cursor_rules = catalog(&InstallOptions {
            grokbot_auth: GrokBotAuthMode::Off,
            ..Default::default()
        })
        .unwrap();
        let (p_cursor, _) = crate::engine::apply(&anchor_src, &cursor_rules);
        assert!(p_cursor.contains(r#"const d=String(n.modelId||""),i=d.toLowerCase(),"#));
        assert!(!p_cursor.contains(r#"n.modelId="premium""#));
        let (to_bot, rep) = crate::engine::apply(&p_cursor, &direct_rules);
        assert_eq!(to_bot, p_default);
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );

        // 最早一律钉 premium：原地改成只钉 GLM 5.2。
        let always = format!(
            "{DIRECT_STREAM_ANCHOR}{}body}}",
            direct_stream_injection_impl(true, true, DirectShape::Current, PremiumPin::Always)
        );
        assert!(always.contains(r#"n.modelId="premium";n.maxMode=!1"#));
        assert!(!always.contains(r#"k.includes("glm-5.2")"#));
        let (from_always, rep) = crate::engine::apply(&always, &direct_rules);
        assert_eq!(from_always, p_default);
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );

        // 上一版 console.info wrap：原地改走 Agent Host logger。
        let console_glm = format!(
            "{DIRECT_STREAM_ANCHOR}{}body}}",
            direct_stream_injection_with_log(
                true,
                true,
                DirectShape::Current,
                PremiumPin::GlmOnly,
                ResolvedLog::Console,
                false,
            )
        );
        assert!(console_glm.contains(r#"console.info("[nexus-sand] premium resolved""#));
        let (from_console, rep) = crate::engine::apply(&console_glm, &direct_rules);
        assert_eq!(from_console, p_default);
        assert!(from_console.contains(r#"createOutputChannel("Cursor Agent Host""#));
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );

        // 上一版「非 native → premium」：原地收成只钉 GLM 5.2。
        let except = format!(
            "{DIRECT_STREAM_ANCHOR}{}body}}",
            direct_stream_injection_impl(
                true,
                true,
                DirectShape::Current,
                PremiumPin::ExceptNative
            )
        );
        assert!(except.contains(r#"k.includes("grok")||k.includes("composer")"#));
        assert!(except.contains(r#"if(!q){n.modelId="premium""#));
        let (from_except, rep) = crate::engine::apply(&except, &direct_rules);
        assert_eq!(from_except, p_default);
        assert!(from_except.contains(r#"if(pin){n.modelId="premium""#));
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );
    }

    #[test]
    fn grok45_via_cua_is_off_by_default_and_migrates_in_place() {
        let off = direct_stream_injection_with_log(
            true,
            true,
            DirectShape::Current,
            PremiumPin::GlmOnly,
            ResolvedLog::AgentHost,
            false,
        );
        let on = direct_stream_injection_with_log(
            true,
            true,
            DirectShape::Current,
            PremiumPin::GlmOnly,
            ResolvedLog::AgentHost,
            true,
        );
        assert!(!off.contains(r#"g45=k.includes("grok-4.5")"#));
        assert!(on.contains(r#"g45=k.includes("grok-4.5")||k.includes("grok-4-5")"#));
        assert!(on.contains(r#"if(g47||g45){n.modelId="sand-cua";n.maxMode=!1;n.parameters=[];try{console.info("[nexus-sand] remap""#));
        assert_ne!(off, on);

        let anchor = format!("{DIRECT_STREAM_ANCHOR}body}}");
        let rules_off = catalog(&InstallOptions::default()).unwrap();
        let rules_on = catalog(&InstallOptions {
            grok45_via_cua: true,
            ..InstallOptions::default()
        })
        .unwrap();
        let (p_off, _) = crate::engine::apply(&anchor, &rules_off);
        assert_eq!(installed_grok45_via_cua(&p_off), Some(false));
        let (p_on, rep) = crate::engine::apply(&p_off, &rules_on);
        assert_eq!(installed_grok45_via_cua(&p_on), Some(true));
        assert!(p_on.contains(r#"g45=k.includes("grok-4.5")"#));
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );
        let (back, rep) = crate::engine::apply(&p_on, &rules_off);
        assert_eq!(back, p_off);
        assert_eq!(installed_grok45_via_cua(&back), Some(false));
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );
    }

    /// 2026-09-13 下午之前装的「4.5 走 CUA」（有 g45、没有 remap console.info）必须仍能卸 /
    /// 原地迁。漏掉这一代时，卸载改了 4884.js 其它补丁却剥不掉推理引擎，写后校验
    /// 「仍有 1 处 Sand 标记（inference stream 1 处）」并回滚。
    #[test]
    fn pre_remap_g45_injection_is_a_legacy_variant_and_uninstalls() {
        let old = direct_stream_injection_g45_plain(
            true,
            true,
            DirectShape::Current,
            ResolvedLog::AgentHost,
        );
        assert!(old.contains(r#"g45=k.includes("grok-4.5")||k.includes("grok-4-5")"#));
        assert!(old.contains(
            r#"if(g47||g45){n.modelId="sand-cua";n.maxMode=!1;n.parameters=[];}const d="#
        ));
        assert!(!old.contains(r#"console.info("[nexus-sand] remap""#));
        assert!(
            direct_stream_variants().iter().any(|v| v == &old),
            "无 remap 的 4.5→CUA 必须在变体表里，uninstall 才能精确剥"
        );

        let anchor = format!("{DIRECT_STREAM_ANCHOR}body}}");
        let installed = format!("{DIRECT_STREAM_ANCHOR}{old}body}}");
        let rules = catalog(&InstallOptions::default()).unwrap();
        let (back, removed) = crate::engine::remove(&installed, &rules);
        assert_eq!(back, anchor);
        assert_eq!(removed.inference_stream, 1);
        assert!(!back.contains(SAND_DIRECT_STREAM_MARKER));

        let rules_on = catalog(&InstallOptions {
            grok45_via_cua: true,
            ..InstallOptions::default()
        })
        .unwrap();
        let (migrated, rep) = crate::engine::apply(&installed, &rules_on);
        assert_eq!(rep.migrated.inference_stream, 1);
        assert!(migrated.contains(r#"console.info("[nexus-sand] remap""#));
        assert_eq!(installed_grok45_via_cua(&migrated), Some(true));
    }

    /// 2026-09-13 之前装的 GlmOnly（只钉 GLM，没有 4.7→CUA）必须仍能卸 / 原地迁。
    /// 漏掉这一代的话，卸载写后校验会剩 1 处 `SAND_DIRECT_INFERENCE_STREAM_V1` 并回滚。
    #[test]
    fn pre_g47_glm_only_injection_is_a_legacy_variant_and_uninstalls() {
        let old = direct_stream_injection_legacy_glm_only(
            true,
            true,
            DirectShape::Current,
            ResolvedLog::AgentHost,
        );
        assert!(old.contains(
            r#"pin=k.includes("glm-5.2")||k.includes("glm5.2")||k.includes("glm_5.2");if(pin)"#
        ));
        assert!(!old.contains("g47="));
        assert!(!old.contains("sand-cua"));
        assert!(
            direct_stream_variants().iter().any(|v| v == &old),
            "旧 GlmOnly 必须在变体表里，uninstall 才能剥"
        );

        let anchor = format!("{DIRECT_STREAM_ANCHOR}body}}");
        let installed = format!("{DIRECT_STREAM_ANCHOR}{old}body}}");
        let rules = catalog(&InstallOptions::default()).unwrap();
        let (back, removed) = crate::engine::remove(&installed, &rules);
        assert_eq!(back, anchor);
        assert_eq!(removed.inference_stream, 1);
        let (migrated, rep) = crate::engine::apply(&installed, &rules);
        assert_eq!(rep.migrated.inference_stream, 1);
        assert!(migrated.contains("g47="));
        assert!(!migrated.contains(
            r#"pin=k.includes("glm-5.2")||k.includes("glm5.2")||k.includes("glm_5.2");if(pin)"#
        ));
    }

    /// 已下线的 Session 引擎（v1.2.7 "session-stream" / v1.2.8 `SAND_STREAM_ENGINE=session`）
    /// 在盘上留的是锚点后一个空 marker。它不再算「装好」：status 计 legacy、install 原地换成
    /// Direct 注入体（不叠加、不重复计 hit）、uninstall 能剥干净。
    #[test]
    fn legacy_session_marker_is_migrated_to_direct_in_place() {
        let anchor_src =
            format!("{DIRECT_STREAM_ANCHOR}const n=yield officialRunInference();body}}");
        let rules = catalog(&InstallOptions::default()).unwrap();
        let session = format!(
            "{DIRECT_STREAM_ANCHOR}{LEGACY_SESSION_STREAM_MARKER}const n=yield officialRunInference();body}}"
        );

        // 盘上是 Session：推理引擎那一行算 0，legacy 算 1，自摘要读不出来。
        let ins = crate::engine::inspect(&session, &rules);
        assert_eq!(ins.markers.inference_stream, 0);
        assert_eq!(ins.legacy, 1);
        assert!(ins.touched());
        assert_eq!(installed_self_summary(&session), None);

        // install：原地换成注入体，与全新装 Direct 逐字节一致，只计 migrated。
        let (to_direct, rep) = crate::engine::apply(&session, &rules);
        let (fresh_direct, _) = crate::engine::apply(&anchor_src, &rules);
        assert_eq!(to_direct, fresh_direct, "迁移要与全新装逐字节一致");
        assert!(!to_direct.contains(LEGACY_SESSION_STREAM_MARKER));
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );
        let ins = crate::engine::inspect(&to_direct, &rules);
        assert_eq!((ins.markers.inference_stream, ins.legacy), (1, 0));

        // uninstall：两种形态都卸回基线。
        for content in [&session, &to_direct] {
            let (back, removed) = crate::engine::remove(content, &rules);
            assert_eq!(back, anchor_src);
            assert_eq!(removed.inference_stream, 1);
        }

        // 已迁移后再 install 幂等。
        let (again, rep2) = crate::engine::apply(&to_direct, &rules);
        assert_eq!(again, to_direct);
        assert_eq!(rep2.hits.total() + rep2.migrated.total(), 0);
    }

    #[test]
    fn wake_and_enablement_only_rewrite_when_the_variable_names_agree() {
        let rules = catalog(&InstallOptions::default()).unwrap();
        // 变量名不同：Python 的 `\1` 根本不匹配；这里原样保留。
        let mismatch = r#"a.source==="interactive-child"||b.payload.notificationContext==="user_driven_interactive_child""#;
        let (same, _) = crate::engine::apply(mismatch, &rules);
        assert_eq!(same, mismatch);
        let ok = r#"x.source==="interactive-child"||x.payload.notificationContext==="user_driven_interactive_child""#;
        let (patched, rep) = crate::engine::apply(ok, &rules);
        assert_eq!(rep.hits.subagent_completion_wake, 1);
        assert_eq!(
            patched,
            format!(r#"x.source==="subagent"{SAND_SUBAGENT_COMPLETION_WAKE_MARKER}||{ok}"#)
        );
        // 已打过的文件整条跳过（原文还在，不带开关会重复注入）。
        let (again, rep2) = crate::engine::apply(&patched, &rules);
        assert_eq!(again, patched);
        assert_eq!(rep2.hits.subagent_completion_wake, 0);
        let (back, _) = crate::engine::remove(&patched, &rules);
        assert_eq!(back, ok);

        let src = "{this._agentHostEnabled=n,this.y=t}";
        let (p, rep) = crate::engine::apply(src, &rules);
        assert_eq!(
            p,
            format!(
                "{{n=!0;{SAND_AGENT_HOST_ENABLEMENT_MARKER}this._agentHostEnabled=n,this.y=t}}"
            )
        );
        assert_eq!(rep.hits.agent_host_enablement, 1);
        let (again, rep2) = crate::engine::apply(&p, &rules);
        assert_eq!(again, p);
        assert_eq!(rep2.hits.agent_host_enablement, 0);
        let (back, _) = crate::engine::remove(&p, &rules);
        assert_eq!(back, src);
    }

    /// remote server profile 的期望值是拿一份原版远程 bundle 量出来的（`examples/profile_probe`
    /// 对 `~/.cursor-server/bin/linux-x64/<commit>` 的镜像，Cursor 3.18.25）。这里把那次测量钉住：
    /// 有人改动 profile 表时，得先重新量过再改这个测试，而不是反过来迁就代码。
    #[test]
    fn server_profile_matches_the_measured_remote_bundle() {
        // 只有这四类和 desktop 不同；三个 0 是因为它们的锚点在 workbench / Electron 文件里，
        // 那几个文件远程根本不存在。
        assert_eq!(
            RuleId::ClientType.expected_for(LayoutProfile::Server),
            Some(EXPECTED_SERVER_CLIENT_MARKERS)
        );
        assert_eq!(
            RuleId::AgentHostEnablement.expected_for(LayoutProfile::Server),
            Some(0)
        );
        assert_eq!(
            RuleId::SubagentCompletionWake.expected_for(LayoutProfile::Server),
            Some(0)
        );
        assert_eq!(
            RuleId::SubagentModelVariants.expected_for(LayoutProfile::Server),
            Some(0)
        );

        let differing: Vec<&str> = RuleId::ALL
            .iter()
            .filter(|id| {
                id.expected_for(LayoutProfile::Server) != id.expected_for(LayoutProfile::Desktop)
            })
            .map(|id| id.name())
            .collect();
        assert_eq!(
            differing,
            vec![
                "client-type",
                "agent host enable",
                "completion wake",
                "subagent model variants"
            ]
        );

        // `expected()` 是 desktop 的别名，别让它悄悄改语义。
        for id in RuleId::ALL {
            assert_eq!(id.expected(), id.expected_for(LayoutProfile::Desktop));
        }
    }
}
