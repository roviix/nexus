/**
 * Rust 侧类型的 TypeScript 镜像。
 *
 * 手写而不是自动生成：这些类型是**契约**，手写逼着我们在改 Rust 结构时同步想一遍
 * 前端要怎么显示。字段名与 `#[serde(rename_all = "camelCase")]` 的产物一一对应。
 */

// ── 错误 ────────────────────────────────────────────────────────────────────

/** 与 `nexus_core::ErrorCode` 一一对应。前端按它分支。 */
export type ErrorCode =
  | "invalid_input"
  | "cursor_not_found"
  | "cursor_schema_drift"
  | "cursor_running"
  | "cursor_control"
  | "unsupported_platform"
  | "profile_not_found"
  | "profile_incomplete"
  | "backup_not_found"
  | "account_not_found"
  | "account_exists"
  | "secret_missing"
  | "database"
  | "io"
  | "network"
  | "upstream"
  | "unauthorized"
  | "forbidden"
  | "oauth_timeout"
  | "cancelled"
  | "busy"
  | "not_logged_in"
  | "internal";

export interface AppError {
  code: ErrorCode;
  /** 直接显示给用户。 */
  message: string;
  /** 下一步该做什么。有就一并显示。 */
  hint?: string;
}

// ── Cursor 自检 ─────────────────────────────────────────────────────────────

export interface SchemaCheck {
  dbPresent: boolean;
  tablePresent: boolean;
  presentKeys: string[];
  missingKeys: string[];
  cursorVersion?: string | null;
}

export interface AppStatus {
  version: string;
  cursor: SchemaCheck;
  cursorUserDir: string;
  /** Cursor 的安装目录。探不到就是 null —— 只影响启动 Cursor 和 Sand。 */
  cursorAppDir?: string | null;
  cursorVersion?: string | null;
  switchMachineIds: boolean;
  backupKeep: number;
}

export interface ActivityEntry {
  id: number;
  at: string;
  level: "info" | "warn" | "error";
  scope: string;
  email?: string | null;
  message: string;
}

// ── 切号 ────────────────────────────────────────────────────────────────────

export interface MachineProfile {
  "telemetry.machineId": string;
  "telemetry.macMachineId": string;
  "telemetry.devDeviceId": string;
  "telemetry.sqmId": string;
  machineidFile?: string;
}

export interface AuthSummary {
  email?: string | null;
  membership?: string | null;
  signupType?: string | null;
  subscriptionStatus?: string | null;
  hasAccessToken: boolean;
  hasRefreshToken: boolean;
  keyCount: number;
}

export interface SwitchProfile {
  id: string;
  email: string;
  membership?: string | null;
  signupType?: string | null;
  note?: string | null;
  machineIds: MachineProfile;
  createdAt: string;
  updatedAt: string;
  lastSwitchedAt?: string | null;
  /** 这一档的登录态还在不在。没有 = 切不进去。 */
  hasAuth: boolean;
  /**
   * `refreshToken` 那一格里其实是 access —— 旧版本给「仅会话」号收录时留下的毒档案。
   * 切过去 Cursor 续期会 401 掉登录，而这类号（没密码、接不了验证码）掉了找不回来，
   * 所以它也算切不进去。Rust 侧 `switch_to` 还有一道硬闸。
   */
  refreshIsPlaceholder: boolean;
  isCurrent: boolean;
}

export interface Overview {
  current?: AuthSummary | null;
  machineIdShort: string;
  machineIdOwner?: string | null;
  hasOriginalMachine: boolean;
  /** Cursor 此刻在不在跑。热切需要它在跑；界面据此决定确认文案。 */
  cursorRunning: boolean;
  check: SchemaCheck;
}

export type BackupReason = "pre-switch" | "pre-restore" | "manual";

export interface AuthBackup {
  id: string;
  email?: string | null;
  createdAt: string;
  reason: BackupReason;
}

/** 切号进度。与 `nexus_switcher::SwitchProgress` 的 tagged union 对齐。 */
export type SwitchProgress =
  | { step: "started"; email: string }
  | { step: "backedUp"; backupId: string; email?: string | null }
  | { step: "backupSkipped" }
  | { step: "hotLoginSent" }
  | { step: "hotLoginConfirmed" }
  /** 热切收尾：补写邮箱 / 档位 / 显示名缓存，否则 Cursor 菜单里还是上一个号的名字。 */
  | { step: "hotProfileWritten"; written: number; removed: number }
  | { step: "cursorQuit"; wasRunning: boolean; forced: boolean }
  | { step: "authWritten"; keys: number }
  | { step: "machineSwitched"; machineIdShort: string }
  | { step: "cursorLaunched" }
  | { step: "done"; email: string }
  | { step: "failed"; failedAt: string; message: string; backupId?: string | null };

export interface SwitchOutcome {
  email: string;
  backupId?: string | null;
  machineSwitched: boolean;
  cursorRelaunched: boolean;
  /** 是否走了热切（不退出 Cursor）。 */
  hot: boolean;
}

// ── 我的账号 ─────────────────────────────────────────────────────────────────

export type AccountSource = "local" | "purchased";
export type AccountStatus = "active" | "needs_login" | "dead";

export interface BotQuota {
  percentUsed?: number;
  periodStart?: number;
  /** 下次重置，epoch ms。Bot 是**周**额，与月账期不是一回事。 */
  resetAt?: number;
  hasAvailable?: boolean;
  access?: "granted" | "blocked";
  blockReason?: string;
  planLabel?: string;
}

export interface ModelUsage {
  model: string;
  tier?: number;
  cents: number;
  input: number;
  output: number;
  cacheRead: number;
  cacheWrite: number;
}

/**
 * 一段时间窗（今天 / 近 7 天）内的花费。`cents` 是按模型明细汇总的，与本账期那个
 * 结算过的 `spendCents` 不是同一口径，差几美分正常。
 */
export interface UsageWindow {
  /** 窗口起止，epoch ms。 */
  start: number;
  end: number;
  cents: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  cacheWriteTokens: number;
  byModel: ModelUsage[];
}

/** 一笔 Cursor 赠送的积分（credit grant）。这是「积分」；账期里的 bonus 是厂商补贴的免费加量，不是它。 */
export interface CreditGrant {
  displayName?: string | null;
  totalCents: number;
  remainingCents: number;
  /** epoch ms */
  expiresAt?: number | null;
}

export interface AccountUsage {
  fetchedAt: string;
  email?: string;
  accountCreatedAt?: string;
  plan?: string;
  subscriptionStatus?: string;
  isYearlyPlan?: boolean;
  isTeamMember?: boolean;
  pendingCancellationDate?: string;
  /** 月账期起止，epoch ms。 */
  cycleStart?: number;
  cycleEnd?: number;
  bot?: BotQuota;
  totalPercentUsed?: number;
  autoPercentUsed?: number;
  apiPercentUsed?: number;
  includedCents?: number;
  bonusCents?: number;
  /** Cursor 赠送的 credit grant，单位美分。1 积分 = $1。没有赠送时缺席。 */
  creditGrantTotalCents?: number;
  creditGrantUsedCents?: number;
  creditGrantRemainingCents?: number;
  /** 每一笔赠送的明细：叫什么、还剩多少、什么时候过期。没有赠送时缺席。 */
  creditGrants?: CreditGrant[];
  spendCents?: number;
  planLimitCents?: number;
  onDemandEnabled?: boolean;
  onDemandUsedCents?: number;
  onDemandLimitCents?: number | null;
  inputTokens?: number;
  outputTokens?: number;
  cacheReadTokens?: number;
  cacheWriteTokens?: number;
  byModel?: ModelUsage[];
  /** 今天（本地零点起）/ 近 7 天。老快照、或那两问没回来时缺席，界面退回只看本账期。 */
  today?: UsageWindow;
  week?: UsageWindow;
  /** `apiKey`：这次快照来自 crsr_ 兑票后的逐条事件，没有额度百分比。 */
  via?: string;
}

/**
 * Stripe 门户读到的订阅账单。金额是 Stripe 最小货币单位（美元是美分，日元是日元）。
 * 门户 URL / ephemeral key **不在这个结构里**。
 */
export type DiscountState = "unknown" | "none" | "active" | "expired";

export interface BillingDiscount {
  state?: DiscountState;
  name?: string;
  percentOff?: number;
  amountOff?: number;
  currency?: string;
  /** `once` / `repeating` / `forever` */
  duration?: string;
  durationInMonths?: number;
  /** 绑到订阅上的起止，epoch ms。 */
  startsAt?: number;
  endsAt?: number;
}

export interface BillingItem {
  name?: string;
  interval?: string;
  unitAmount?: number;
  quantity?: number;
  currency?: string;
}

export interface BillingInvoiceLine {
  description?: string;
  amount?: number;
  quantity?: number;
}

export interface BillingInvoice {
  number?: string;
  created?: number;
  status?: string;
  description?: string;
  subtotal?: number;
  total?: number;
  amountDue?: number;
  amountPaid?: number;
  amountRemaining?: number;
  currency?: string;
  periodStart?: number;
  periodEnd?: number;
  discounts?: BillingDiscount[];
  lines?: BillingInvoiceLine[];
}

export interface AccountBilling {
  fetchedAt: string;
  currency?: string;
  collectionMethod?: string;
  subscriptionStatus?: string;
  interval?: string;
  currentPeriodStart?: number;
  currentPeriodEnd?: number;
  cancelAtPeriodEnd?: boolean;
  canceledAt?: number;
  items?: BillingItem[];
  listPrice?: number;
  currentAmount?: number;
  discountState: DiscountState;
  discount?: BillingDiscount | null;
  invoices?: BillingInvoice[];
}

export interface Account {
  id: string;
  email: string;
  source: AccountSource;
  status: AccountStatus;
  note?: string | null;
  tags: string[];
  membership?: string | null;
  signupType?: string | null;
  workosUserId?: string | null;
  usage?: AccountUsage | null;
  billing?: AccountBilling | null;
  lastCheckedAt?: string | null;
  lastError?: string | null;
  codeChannel: string;
  codeChannelResolved?: string | null;
  lastCodeAt?: string | null;
  hasRefresh: boolean;
  /** 存着一把 access JWT。没 refresh 的号全靠它（「仅会话」），到期退回待登录。 */
  hasAccess: boolean;
  /** 那把 access 的到期时刻（ISO）。判「仅会话的号还活着没」用它，不用解密。 */
  accessExpiresAt?: string | null;
  hasPassword: boolean;
  hasEmailPassword: boolean;
  hasRecoveryEmail: boolean;
  /** 长期 `crsr_…` User API Key。不能切号，session 过期后仍能查基础用量。 */
  hasApiKey: boolean;
  createdAt: string;
  updatedAt: string;
  /** 入库序号（rowid）。同一秒导入的一批号靠它保持导入顺序。 */
  seq: number;
  /** 归档时刻；缺席 = 没归档。归档的号默认不列、不刷、不进网关候选，凭证原样留着。 */
  archivedAt?: string | null;
  /**
   * 「此刻能不能用」的唯一答案，Rust 侧算好带过来（`Account::availability`）。
   * 卡片、分布条、筛子、抽屉都只认它——别在前端再用 status / hasRefresh 拼一套。
   */
  availability: Availability;
}

/**
 * long_lived：有 refresh，能自己续期 · session：只靠一把还活着的 session token · api_key：只有 crsr_ ·
 * logged_out：掉登录（上游拒了 / 过期 / 只有密码）· dead：refresh 被拒又没密码，救不回来。
 */
export type Availability = "long_lived" | "session" | "api_key" | "logged_out" | "dead";

export type SecretKind = "refresh" | "access" | "cursorPassword" | "emailPassword" | "recoveryEmail" | "apiKey";

/** 刚铸出来的一把 `crsr_`。不含完整钥匙——要看完整的去凭证页点「显示」。 */
export interface MintedApiKey {
  name: string;
  /** `crsr_…1a2b`，只够确认确实铸出来了。 */
  masked: string;
  expiresAt?: string | null;
}

/** 批量导入的预览：每一条会不会被收下、为什么。 */
export interface ImportRow {
  email: string;
  accepted: boolean;
  reason: string;
  hasRefresh: boolean;
  hasPassword: boolean;
  hasEmailPassword: boolean;
  hasAccess?: boolean;
  hasApiKey?: boolean;
}

export interface ImportPreview {
  rows: ImportRow[];
  /** 完全认不出的行，原样带回来让人自己看。 */
  skipped: string[];
  acceptedCount: number;
  rejectedCount: number;
}

export interface ImportOutcome {
  imported: number;
  skipped: number;
  failures: string[];
}

/** 导出账号清单的结果。只有路径和条数 —— 文件内容（明文凭证）没经过 IPC。 */
export interface ExportOutcome {
  path: string;
  count: number;
}

/** `~/.roviix/backups` 里的一份整库快照。只是文件的元信息，内容不经过 IPC。 */
export interface LocalBackup {
  fileName: string;
  path: string;
  /** `manual`，或 `pre-restore`（还原前自动留的那份）。 */
  reason: "manual" | "pre-restore";
  createdAt: string;
  sizeBytes: number;
}

/** 一次还原：还原了哪份，以及还原前的状态被另存成了哪份。 */
export interface RestoreOutcome {
  restored: string;
  safety: LocalBackup;
}

export type OauthState =
  | { state: "waiting"; uuid: string; elapsedSecs: number }
  | { state: "succeeded"; uuid: string; email?: string | null }
  | { state: "failed"; uuid: string; message: string }
  | { state: "cancelled"; uuid: string };

export interface OauthStarted {
  uuid: string;
  loginUrl: string;
}

/** Cursor 侧一条活跃会话（IDE / 网页 / …）。 */
export interface ActiveSession {
  sessionId: string;
  sessionType: string | number;
  createdAt?: string;
  expiresAt?: string;
  isCurrent: boolean;
}

/** 一键踢会话的结果。 */
export interface KickOutcome {
  listed: number;
  revoked: number;
  kept: number;
  failed: number;
  keptCurrent: boolean;
  refreshAlive: boolean;
}

// ── Sand 补丁 ────────────────────────────────────────────────────────────────
// 与 `nexus_sand::model` 逐字段对齐（camelCase）。

export type ModeGate = "agent" | "agent_plan" | "all";

export type CursorDownloadPlatform = "macos" | "windows" | "linux";
export type CursorArchitecture = "universal" | "x64" | "arm64";

export interface CursorDownload {
  architecture: CursorArchitecture;
  /** Cursor 官方 CDN 的不可变直链，不经过会漂移的 stable 下载 API。 */
  url: string;
}

export interface CursorRelease {
  version: string;
  platform: CursorDownloadPlatform;
  downloads: CursorDownload[];
}

/**
 * 安装选项。推理引擎只有一种：注入体劫持 attempt 工厂直连 `InferenceService/Stream`。
 * 曾经的「原生通道」（Session，走官方 `RunInference`）被服务端对 sand 身份封掉后已下线；
 * 老机器上它留下的空 marker 在 `SandStatus.legacyMarkers` 里计数，重新安装会原地迁走。
 */
export interface SandInstallOptions {
  /**
   * Cursor 原生上下文自动摘要。默认开：Stream 模式下它是唯一会在撞上限前压缩历史的机制，
   * 关掉后长会话到上限只会一直失败（详见 Rust `InstallOptions::self_summary`）。
   */
  selfSummary?: boolean;
  modeGate?: ModeGate;
  relaunch?: boolean;
  /**
   * 把 Agent 面板的推理改道到这个地址（本机网关的透传口，`http://127.0.0.1:<port>`）。
   * 这是网关拦截 IDE 流量（记用量、改写上下文）的唯一入口；缺省 / null = 直连 api2。
   */
  inferenceEndpoint?: string | null;
  /** Agent 面板 Stream 用 Grok Bot 额度的方式（默认 `box_relay`）。 */
  grokbotAuth?: GrokBotAuthMode;
  /**
   * Agent 面板选 grok-4.5 时改走 `sand-cua`。默认关。
   * 只有部分号会落到 grok-4.7，其余仍是 luna。改完需重新安装补丁。
   */
  grok45ViaCua?: boolean;
}

/**
 * Grok Bot 鉴权的三种落法，都改 `applyAuthorization` 同一处、互为变体、重装原地切换。
 * - `off`：不动，Stream 用 Cursor 登着的号（或交给「推理经本机网关」）。
 * - `box_relay`：改道到 Grok Bot Box 内的 relay，token 留在 Box（v135 同款；依赖 pod 在线 + Bot 端装过 relay）。
 * - `direct`：直连 api2，补丁读本机凭证、自己续期（不依赖 Bot 端改动；凭证由 Nexus 从 Grok Bot 生成）。
 */
export type GrokBotAuthMode = "off" | "box_relay" | "direct";

// ── Grok Bot 桥（不是账号系统；Sand / 网关按需借额度） ─────────────────────────

export interface GrokBotAppStatus {
  installed: boolean;
  /** `desktop-status.json` 的 signedIn；从没启动过为 null。 */
  signedIn: boolean | null;
  appVersion: string | null;
  running: boolean;
}

export interface GrokBotRelayInfo {
  baseUrl: string;
  relayPath: string;
  accountFingerprint: string | null;
}

export interface GrokBotDirectInfo {
  expiresAtMs: number | null;
  expired: boolean;
  /** 有 `sbi_*` 种子，过期了也能自己续。 */
  canRenew: boolean;
  accountEmail: string | null;
  /** 凭证来自哪条路：读 Grok Bot 客户端 / 账号库里的号自己换的。老文件为 null。 */
  source: "grokbot_app" | "library" | null;
  mintedAtMs: number | null;
  renewedAtMs: number | null;
}

export interface GrokBotStatus {
  app: GrokBotAppStatus;
  /** 活跃账号 email；没解过钥匙串、凭证又不是当前槽生成的时为 null（不算错）。 */
  activeEmail: string | null;
  /** 活跃账号槽 id（明文键）。变了 = Grok Bot 换过号。 */
  activeSlot: string | null;
  relay: GrokBotRelayInfo | null;
  direct: GrokBotDirectInfo | null;
  /** 直连凭证是上一个号生成的：还能用，但花的是旧号额度；安装 / 网关会自动重生成。 */
  directStale: boolean;
}

export interface GrokBotDirectMinted {
  accountEmail: string | null;
  expiresAtMs: number | null;
}

/** 某个号打 `sand-cua` 实际落到哪个模型（不含 token）。 */
export interface GrokBotCuaProbe {
  email: string;
  requestedModel: string;
  resolvedModel: string | null;
  hasGrok47: boolean;
  probedAtMs: number;
  error?: string | null;
}

/** Grok Bot 客户端此刻登着谁（不含秘密）。要解钥匙串，所以是显式动作。 */
export interface GrokBotIdentity {
  email: string | null;
  name: string | null;
  subject: string | null;
  /** 有 refresh token 才能收进「我的账号」。 */
  hasRefresh: boolean;
}

/** 各类补丁 marker 的计数。字段名与 Rust `MarkerCounts` 一一对应。 */
export interface MarkerCounts {
  clientType: number;
  eligibility: number;
  managedLocalRoute: number;
  localRuntimeLoad: number;
  /** 推理引擎（直连注入体）的 marker，期望 1。已下线原生通道的空 marker 计在 legacyMarkers。 */
  inferenceStream: number;
  agentHostEnablement: number;
  agentHostIdentity: number;
  agentHostMoveExec: number;
  managedSubagentRoute: number;
  managedSubagentSession: number;
  managedTaskTool: number;
  managedActionRoute: number;
  subagentResumeMode: number;
  subagentCompletionWake: number;
  subagentInteractionBubble: number;
  subagentModelVariants: number;
  contextWindow: number;
  /** 推理端点改道（remote 专用，两处：建 transport + 挂路由）。本机永远是 0。 */
  inferenceEndpoint: number;
  /** Grok Bot Box Relay Stream 鉴权（agent-host + always-local，各 1）。 */
  grokbotStreamAuth: number;
}

export interface SandDryRun {
  wouldHit: MarkerCounts;
  filesToChange: number;
  anchorsComplete: boolean;
  /** 未达标的锚点，形如 `client-type（0 / 需 23）`。 */
  missing: string[];
}

export interface SandStatus {
  cursorVersion: string | null;
  supportedVersion: string;
  versionSupported: boolean;
  installed: boolean;
  /** 全部必需 marker 到位。 */
  complete: boolean;
  markers: MarkerCounts;
  remainingIde: number;
  foreignMarkers: number;
  legacyMarkers: number;
  patchedFiles: string[];
  dryRun: SandDryRun | null;
  backups: number;
  /**
   * 盘上注入体里自动摘要开关的实际取值；没装推理引擎注入体时为 null。
   * 与界面上的开关（要装什么）不同时，安装会原地切换，不必先卸载。
   */
  selfSummary: boolean | null;
  /** 盘上注入体是否把 grok-4.5 改走 sand-cua；没装推理引擎时为 null。 */
  grok45ViaCua: boolean | null;
  /** 盘上装着的推理端点改道地址；没改道为 null。与界面开关不同时安装会原地换 / 剥掉。 */
  inferenceEndpoint: string | null;
  /** 盘上装着的 Grok 鉴权形态；没装为 `off`。与界面选的不同时安装会原地切换。 */
  grokbotAuth: GrokBotAuthMode;
  /** 本地 grok-box-relay.json 是否就绪（box_relay 的前提）。 */
  grokbotRelayConfigured: boolean;
  /** 本地直连凭证是否就绪（未过期或可续期；direct 的前提）。 */
  grokbotDirectConfigured: boolean;
}

export type SandStep =
  | "preflight"
  | "backup"
  | "quit_cursor"
  | "write"
  | "verify"
  | "launch"
  | "done";

export interface SandProgress {
  step: SandStep;
  detail: string;
}

export type SandOperation = "install" | "uninstall" | "restore";

export interface SandOutcome {
  operation: SandOperation;
  /** 是否真的改了文件。为假 = 已是目标状态，Cursor 没被碰。 */
  wrote: boolean;
  filesWritten: number;
  backupId: string | null;
  cursorRelaunched: boolean;
  status: SandStatus;
}

export interface SandBackup {
  id: string;
  createdAt: string;
  operation: SandOperation;
  cursorVersion: string;
  files: number;
  /** prepared | committed | rolled_back */
  state: string;
  error: string | null;
}

// ── CRSR 补丁（原生 Agent 面板走 crsr_ API Key；和 Sand 互斥）────────────────

/** 给界面看的、不含秘密的凭证摘要。 */
export interface CrsrCredentialInfo {
  accountEmail: string | null;
  accountId: string | null;
  expiresAtMs: number | null;
  expired: boolean;
  canRenew: boolean;
}

export interface CrsrStatus {
  cursorVersion: string | null;
  supportedVersion: string;
  versionSupported: boolean;
  installed: boolean;
  complete: boolean;
  hits: number;
  expectedHits: number;
  anchors: number;
  patchedFiles: string[];
  sandConflict: string | null;
  backups: number;
  credential: CrsrCredentialInfo | null;
}

export interface CrsrOutcome {
  operation: SandOperation;
  wrote: boolean;
  filesWritten: number;
  backupId: string | null;
  cursorRelaunched: boolean;
  status: CrsrStatus;
}

export type CrsrBackup = SandBackup;
export type CrsrProgress = SandProgress;

// ── 远程 Sand（remote SSH）─────────────────────────────────────────────────────
// 与 `nexus_sand::remote` 和 `commands::sand_remote` 逐字段对齐。

/**
 * 远程怎么出网。
 *
 * - `gateway`：改推理端点 → 隧道 → 本机网关。号池接力、记账、面板拦截都在这条路上。
 * - `proxy`：**不改端点**，远程照旧打官方 api2，只是经隧道走本机的 HTTP 代理。链路短，
 *   但用的就是远程当前登录的那个号，没有轮换。
 * - `direct`：远程自己出得去网，不改也不起隧道。
 */
export type RemoteRoute = "gateway" | "proxy" | "direct";

/**
 * 一台已保存的远程主机。`host` 是 ssh 认的名字（config 里的别名或 user@hostname）。
 * 不保存任何凭证——认证交给用户自己的 ssh 配置。
 */
export interface RemoteHost {
  host: string;
  label: string;
  /** 出网方式。老配置里是布尔 `routeViaLocal`，Rust 侧读的时候已经归一了。 */
  route: RemoteRoute;
  /**
   * 隧道在远程那头监听的端口。网关模式下写进远程 bundle 的端点就是它，代理模式下 Cursor
   * 设置里的 HTTP_PROXY 就是它。远程上 7890 / 7897 常常有别的东西在听，所以默认是一个高位口。
   */
  remotePort: number;
  /** 代理模式：本机代理端口；null = 用探测到的。 */
  proxyPort: number | null;
}

/** 远程上的一份 Cursor server。同一台机常常堆着好几个 commit。 */
export interface RemoteServer {
  commit: string;
  version: string;
  root: string;
}

export interface RemoteStatus {
  host: string;
  servers: RemoteServer[];
  /** 选中的那份（与本机 Cursor 同 commit 优先）；null = 没有可用的。 */
  selected: RemoteServer | null;
  versionSupported: boolean;
  commitMatchesLocal: boolean;
  markers: MarkerCounts;
  patchedFiles: string[];
  /** 盘上装着的推理端点；null = 没改道（远程直连 api2）。 */
  inferenceEndpoint: string | null;
  /** marker 数与 Server profile 期望一致。只是指示，硬校验在安装时做。 */
  complete: boolean;
}

export type TunnelPhase = "connecting" | "connected" | "reconnecting" | "stopped";

export interface TunnelStatus {
  spec: { host: string; remotePort: number; localPort: number } | null;
  phase: TunnelPhase;
  reconnects: number;
  lastError: string | null;
  /** 此刻经隧道活着的连接数。有数就是真在用。 */
  streams: number;
}

export interface RemoteHostView {
  host: RemoteHost;
  /** Rust 的 `Result<RemoteStatus, String>`：连不上时是 `{ Err: string }`。 */
  status: { Ok: RemoteStatus } | { Err: string };
  tunnel: TunnelStatus;
  /** 隧道本机这头接到哪个端口（本机代理口）；null = 这条路不用隧道，或代理端口猜不到。 */
  localPort: number | null;
  /** 代理模式：Cursor 那三个设置此刻给这台主机配的地址；null = 没配。 */
  proxyConfigured: string | null;
  /** 本机那头（代理口）现在有没有人听。 */
  localListening: boolean;
}

export interface RemoteOverview {
  hosts: RemoteHostView[];
  localCommit: string | null;
  /** 猜出来的本机代理端口（代理模式的默认值）；null = 常见端口都没人听。 */
  detectedProxyPort: number | null;
}

/**
 * 探针走到了哪一跳。失败时它就是断点 —— 「隧道不通」「代理拒绝 CONNECT」「TLS 失败」
 * 的下一步动作完全不同。
 */
export type ProbeStage = "tunnel" | "proxy" | "tls" | "http";

export interface ProbeReport {
  ok: boolean;
  stage: ProbeStage;
  /** HTTP 状态码。几百都算链路通——我们验的是链路，不是鉴权。 */
  status: number | null;
  detail: string | null;
  /** 打的是哪个远程端口（就是常驻隧道那个）。 */
  remotePort: number;
}

export type RemoteOperation = "install" | "uninstall";

export interface RemoteOutcome {
  operation: RemoteOperation;
  host: string;
  commit: string;
  wrote: boolean;
  filesWritten: number;
  backupId: string | null;
  /** 已把远程 cursor-server 进程杀掉；还需要在 Cursor 里对该远程 Reload Window。 */
  serverRestarted: boolean;
  status: RemoteStatus;
}

// ── 本地网关 ─────────────────────────────────────────────────────────────────

export interface GatewaySettings {
  port: number;
  autostart: boolean;
  /** 强制上游模型；null/缺省 = 不强制。 */
  forceModel: string | null;
  /** 裸名 / 空模型走哪条通道。出厂 cursor。 */
  defaultChannel: string;
}

export interface GatewayRunning {
  addr: string;
  baseUrl: string;
  startedAt: string;
}

export type GatewayCandidateState =
  | { kind: "ready" }
  | { kind: "current" }
  | { kind: "exhausted"; reason: string; retryInSecs: number }
  | { kind: "quota_line" }
  | { kind: "cooled"; models: string[]; secsLeft: number };

export interface GatewayCandidate {
  label: string;
  /** cursor_login | stored */
  source: string;
  /** 用的是真机码（Cursor 正登着的号）。 */
  pinned: boolean;
  storedId: string | null;
  percentUsed: number | null;
  state: GatewayCandidateState;
}

/** 名单外、但此刻有来源能给出凭证的号：「添加」弹窗列的就是它们。 */
export interface GatewayAvailable {
  label: string;
  /** cursor_login | stored */
  source: string;
  pinned: boolean;
  percentUsed: number | null;
}

export interface GatewayLane {
  current: string | null;
  /** 名单里、此刻有来源给出凭证的号，按接力顺序。 */
  candidates: GatewayCandidate[];
  /** 名单里、但此刻没有任何来源给出凭证的号（小写邮箱）。 */
  missing: string[];
  available: GatewayAvailable[];
}

/** 网关里一条订阅通道的 id。与 Rust `channel::ChannelId` 的取值一致；也是账号页签的平台 id。 */
export type GatewayChannelId = "chatgpt" | "grok" | "kiro";

/** 一条订阅通道的快照。与 Rust `service::ChannelSnapshot` 对齐。 */
export interface ChannelSnapshot {
  id: GatewayChannelId;
  label: string;
  /** `/v1/models` 的 owned_by：openai / xai / aws。 */
  vendor: string;
  /** 有没有号能接聊天。 */
  ready: boolean;
  /** 有没有号能接媒体（生图 / 生视频）。没有媒体能力的通道恒为 false。 */
  mediaReady: boolean;
  lane: GatewayLane;
  chatModels: string[];
  imageModels: string[];
  videoModels: string[];
  /** 显式路由前缀（`grok/` 这类）。 */
  prefixes: string[];
}

/** 一个异步媒体任务（生视频）。与 Rust `media::MediaJob` 对齐。 */
export interface MediaJob {
  requestId: string;
  channel: string;
  account: string;
  model: string;
  /** generate | edit | extend */
  op: string;
  /** pending | done | failed | expired */
  status: string;
  videoUrl: string | null;
  durationSecs: number | null;
  resolution: string | null;
  createdMs: number;
  updatedMs: number;
}

export interface GatewayStatus {
  running: GatewayRunning | null;
  settings: GatewaySettings;
  restartNeeded: boolean;
  apiKeySet: boolean;
  lane: GatewayLane;
  /** 订阅通道（ChatGPT / Grok Build / Kiro …），按选路顺序。号本身在各自的账号页签管。 */
  channels: ChannelSnapshot[];
  /** 最近的异步媒体任务（生视频），新的在前。 */
  mediaJobs: MediaJob[];
}

// ── ChatGPT 订阅号（本地网关的第二种号源）──────────────────────────────────────

/** 一个滚动窗口。primary 通常是 5 小时、secondary 是 7 天，以 `windowMinutes` 为准。 */
export interface ChatGptUsageWindow {
  usedPercent: number | null;
  /** 窗口重置时刻（Unix 毫秒）。 */
  resetAtMs: number | null;
  windowMinutes: number | null;
}

/** `/wham/usage` 里按模型单独计的一桶（Spark 等）。与 Rust `RateLimitBucket` 对齐。 */
export interface ChatGptRateLimitBucket {
  name: string | null;
  feature: string | null;
  allowed: boolean | null;
  limitReached: boolean | null;
  primary: ChatGptUsageWindow | null;
  secondary: ChatGptUsageWindow | null;
}

/** Codex 点数。没点数的号 `hasCredits=false`，别把 balance "0" 画成「有 0 点」。 */
export interface ChatGptCredits {
  hasCredits: boolean | null;
  unlimited: boolean | null;
  overageLimitReached: boolean | null;
  balance: string | null;
  resetAvailable: number | null;
}

/** 与 Rust `nexus_chatgpt::CodexUsage` 对齐。 */
export interface ChatGptUsage {
  primary: ChatGptUsageWindow | null;
  secondary: ChatGptUsageWindow | null;
  planType: string | null;
  checkedAt: string;
  /** response-headers | wham/usage */
  source: string;
  additional?: ChatGptRateLimitBucket[];
  credits?: ChatGptCredits | null;
  allowed?: boolean | null;
  limitReached?: boolean | null;
  userId?: string | null;
}

/** 本地网关走这个号的合计。不是 ChatGPT 网页上的终身用量。 */
export interface ChatGptTraffic {
  requests: number;
  tokens: number;
  errors: number;
  days: number;
}

/**
 * ChatGPT 订阅快照。与 Rust `nexus_chatgpt::ChatGptBilling` 对齐。
 * 没有标价 / 券 / 发票——Codex OAuth 打不开网页 Stripe 门户。
 * `null` 是「没读到」，不是「没有订阅 / 不会续费」。
 */
export interface ChatGptBilling {
  planType: string | null;
  subscriptionPlan: string | null;
  hasActiveSubscription: boolean | null;
  expiresAt: string | null;
  willRenew: boolean | null;
  billingPeriod: string | null;
  checkedAt: string;
  /** accounts/check | wham/accounts/check | subscriptions */
  source: string;
}

export type ChatGptStatus = "active" | "needs_login" | "dead";

/** 与 Rust `nexus_chatgpt::ChatGptAccount` 对齐。**没有任何凭证字段。** */
export interface ChatGptAccount {
  id: string;
  /** chatgpt_account_id：上游侧的账号身份。 */
  accountRef: string;
  email: string | null;
  planType: string | null;
  userId: string | null;
  organizationId: string | null;
  organizationTitle: string | null;
  status: ChatGptStatus;
  /** 进不进网关接力队。 */
  enabled: boolean;
  note: string | null;
  usage: ChatGptUsage | null;
  billing: ChatGptBilling | null;
  lastCheckedAt: string | null;
  lastError: string | null;
  hasRefresh: boolean;
  accessExpiresAt: string | null;
  createdAt: string;
  updatedAt: string;
  traffic?: ChatGptTraffic | null;
}

/** 上游目录里的一条（已按套餐与客户端版本筛过）。与 Rust `nexus_chatgpt::ManifestModel` 对齐。 */
export interface ChatGptManifestModel {
  slug: string;
  reasoningLevels: string[];
  preferWebsockets: boolean;
}

export interface ChatGptLoginHandle {
  sessionId: string;
  authorizeUrl: string;
  redirectUri: string;
  /** 本机 1455 在听：点完同意会自动完成。false = 要把回调地址贴回来。 */
  callbackListening: boolean;
}

export type ChatGptLoginState =
  | { state: "waiting"; sessionId: string; elapsedSecs: number }
  | { state: "succeeded"; sessionId: string; account: ChatGptAccount; created: boolean }
  | { state: "failed"; sessionId: string; message: string; hint: string | null }
  | { state: "cancelled"; sessionId: string };

/** 与 Rust `nexus_chatgpt::ImportOutcome` 对齐。凭证不回传。 */
export interface ChatGptImportOutcome {
  created: number;
  updated: number;
  failed: number;
  errors: string[];
}

export type DeviceStatus = "active" | "needs_login" | "dead";

/** Grok 的额度快照。与 Rust `nexus_grok::GrokQuota` 对齐。 */
export interface GrokQuota {
  /** 当前周期已用百分比（0–100）。 */
  creditUsagePercent: number | null;
  /** weekly / monthly。 */
  periodType: string | null;
  periodStart: string | null;
  periodEnd: string | null;
  products: Array<{ product: string; usagePercent: number | null }>;
  subscriptionTier: string | null;
  prepaidBalanceCents: number | null;
  remainingRequests: number | null;
  remainingTokens: number | null;
  retryAfterMs: number | null;
  checkedAt: string;
  /** billing / headers。 */
  source: string;
}

/**
 * 订阅平台（Grok Build / Kiro）的一个账号。两边共用一个形状：Kiro 多 `authMethod`，
 * Grok 多账号类型 / 档位 / 额度 / 媒体资格——没有的字段就是 undefined。
 */
export interface DeviceAccount {
  id: string;
  accountRef: string;
  email: string | null;
  planType: string | null;
  status: DeviceStatus;
  enabled: boolean;
  note: string | null;
  /** Kiro：builder-id / social。 */
  authMethod?: string | null;
  /** Grok：oauth（订阅号）/ api_key。 */
  authKind?: "oauth" | "api_key";
  /** Grok：订阅档位（SuperGrok / Free …）。 */
  subscriptionTier?: string | null;
  lastCheckedAt: string | null;
  lastError: string | null;
  /** OAuth 号：有 refresh token；API Key 号：有 key。都是「还能自己拿到凭证」。 */
  hasRefresh: boolean;
  accessExpiresAt: string | null;
  /** Grok：额度快照。 */
  usage?: GrokQuota | null;
  /** Grok：自动探测出的媒体资格；null = 还没探过。 */
  mediaProbe?: boolean | null;
  /** Grok：手动覆盖。 */
  mediaOverride?: boolean | null;
  /** Grok：生效的媒体资格（覆盖 > 探测）。null = 未知，让它去撞一次。 */
  mediaEligible?: boolean | null;
  createdAt: string;
  updatedAt: string;
}

/** Grok 上游拉到的一条模型。 */
export interface GrokManifestModel {
  id: string;
  displayName: string | null;
  /** chat / image / video。 */
  modality: string;
}

export interface DeviceLoginHandle {
  sessionId: string;
  authorizeUrl: string;
  userCode: string;
  expiresInSecs: number;
}

export type DeviceLoginState =
  | { state: "waiting"; sessionId: string; userCode: string; elapsedSecs: number }
  | { state: "succeeded"; sessionId: string; account: DeviceAccount; created: boolean }
  | { state: "failed"; sessionId: string; message: string; hint: string | null }
  | { state: "cancelled"; sessionId: string };

// ── 一键接入（改客户端配置文件）──────────────────────────────────────────────
// 与 `nexus_connect` + `commands::connect` 对齐。

/** 配置文件现在指向哪里。 */
export type PointsTo = "local" | "other" | "none";

export interface ClientState {
  /** 主配置文件（Claude 的 settings.json / Codex 的 config.toml）。 */
  path: string;
  exists: boolean;
  baseUrl: string | null;
  model: string | null;
  /** 有我们留下的接入清单 —— 也就是「可以撤销」。 */
  revertible: boolean;
  appliedAt: string | null;
  pointsTo: PointsTo;
}

export interface AppliedFile {
  path: string;
  /** 之前没有这个文件，是我们建的。 */
  created: boolean;
  /** 改之前拷的那份。 */
  backup: string | null;
}

export interface ConnectApplied {
  files: AppliedFile[];
}

export interface ConnectReverted {
  restored: string[];
  removed: string[];
  /** 没有清单也没有备份，只剔掉了我们的键。 */
  stripped: string[];
}

// ── 权限预检 ────────────────────────────────────────────────────────────────
// 与 `commands::perms` 对齐。

export type PermStatus = "ok" | "denied" | "unknown" | "not_applicable";

export interface PermItem {
  id: string;
  title: string;
  usedBy: string;
  status: PermStatus;
  detail: string | null;
  canRequest: boolean;
  /** 系统设置里对应的那一页；只有 macOS 的两种系统弹窗类权限有。 */
  settingsUrl: string | null;
  required: boolean;
}

export interface PermReport {
  platform: "macos" | "windows" | "linux";
  preflightDone: boolean;
  items: PermItem[];
}

// ── 本地用量（网关请求账本）────────────────────────────────────────────────
// 与 `nexus_gateway::ledger` 逐字段对齐。只有数字与名字，没有对话内容。

export interface UsageDay {
  /** `2026-09-03`，按用户本地时区分桶。 */
  day: string;
  calls: number;
  errors: number;
  inputTokens: number;
  outputTokens: number;
}

/** 今天的某一个小时，`hour` 是本地时区的 0–23。 */
export interface UsageHour {
  hour: number;
  calls: number;
  errors: number;
  inputTokens: number;
  outputTokens: number;
}

export interface UsageTotals {
  calls: number;
  errors: number;
  inputTokens: number;
  outputTokens: number;
  cacheReadTokens: number;
  /** 成功请求的首字中位数；一条成功的都没有时为 null。 */
  ttftP50Ms: number | null;
  durationP50Ms: number | null;
}

/** 按模型 / 按账号的一行。 */
export interface UsageNamed {
  name: string;
  calls: number;
  errors: number;
  tokens: number;
}

export interface UsageRecent {
  at: string;
  account: string;
  model: string;
  routed: string | null;
  ok: boolean;
  status: number;
  kind: string | null;
  inputTokens: number;
  outputTokens: number;
  ttftMs: number | null;
  durationMs: number;
  channel: string;
}

export interface UsageSummary {
  /** 从旧到新，恰好 N 天；没请求的天也在，数字为零。 */
  days: UsageDay[];
  /** 今天的 24 个小时，0 点在前；还没到的小时也在，数字为零。 */
  hours: UsageHour[];
  today: UsageTotals;
  window: UsageTotals;
  byModel: UsageNamed[];
  byAccount: UsageNamed[];
  /** 按通道（cursor / chatgpt / grok / kiro）。 */
  byChannel: UsageNamed[];
  recent: UsageRecent[];
  /** 账本里最早一条的时刻；一条都没有是 null。 */
  since: string | null;
}
