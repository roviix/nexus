/**
 * 到 Rust 的唯一通道。
 *
 * 组件不直接 `invoke` —— 全走这里。这样命令名只写一遍（拼错会在这一个文件里被发现），
 * 参数和返回值都有类型，错误也统一成 `AppError` 形状。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { startOfLocalDay } from "../ui/usage";
import type {
  Account,
  AccountBilling,
  AccountUsage,
  ActiveSession,
  ActivityEntry,
  AppError,
  AppStatus,
  AuthBackup,
  ChatGptAccount,
  ChatGptBilling,
  ChatGptImportOutcome,
  ChatGptLoginHandle,
  ChatGptLoginState,
  ChatGptManifestModel,
  ChatGptUsage,
  GatewayChannelId,
  ClientState,
  ConnectApplied,
  ConnectReverted,
  DeviceAccount,
  DeviceLoginHandle,
  DeviceLoginState,
  ExportOutcome,
  GatewaySettings,
  GatewayStatus,
  GrokManifestModel,
  ImportOutcome,
  ImportPreview,
  KickOutcome,
  LocalBackup,
  MediaJob,
  MintedApiKey,
  OauthStarted,
  OauthState,
  Overview,
  PermReport,
  ProbeReport,
  RemoteHost,
  RemoteOutcome,
  RemoteOverview,
  RemoteRoute,
  RemoteStatus,
  RestoreOutcome,
  RewriteRule,
  SandBackup,
  SandInstallOptions,
  GrokBotCuaProbe,
  GrokBotDirectMinted,
  GrokBotIdentity,
  GrokBotRelayInfo,
  GrokBotStatus,
  SandOutcome,
  SandProgress,
  CursorRelease,
  CrsrBackup,
  CrsrCredentialInfo,
  CrsrOutcome,
  CrsrProgress,
  CrsrStatus,
  SandStatus,
  SecretKind,
  SwitchOutcome,
  SwitchProfile,
  TunnelStatus,
  SwitchProgress,
  UsageSummary,
} from "./types";

/** Rust 侧返回的错误都是 `AppError`；这里把它认出来，别让界面显示 `[object Object]`。 */
export function isAppError(value: unknown): value is AppError {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as AppError).code === "string" &&
    typeof (value as AppError).message === "string"
  );
}

/** 任意抛出物 → 能显示给人看的一句话（含下一步）。 */
export function errorText(err: unknown): string {
  if (isAppError(err)) return err.hint ? `${err.message}（${err.hint}）` : err.message;
  if (err instanceof Error) return err.message;
  return String(err);
}

async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(cmd, args);
}

// ── app ─────────────────────────────────────────────────────────────────────

export const app = {
  status: () => call<AppStatus>("app_status"),
  activity: (limit?: number) => call<ActivityEntry[]>("app_activity", { limit }),
  updateSettings: (patch: {
    switchMachineIds?: boolean;
    backupKeep?: number;
    cursorUserDir?: string;
    cursorAppDir?: string;
  }) => call<AppStatus>("app_update_settings", patch),
};

// ── 切号 ────────────────────────────────────────────────────────────────────

export const switcher = {
  overview: () => call<Overview>("switcher_overview"),
  list: () => call<SwitchProfile[]>("switcher_list"),
  captureCurrent: (note?: string) => call<SwitchProfile>("switcher_capture_current", { note }),
  setNote: (id: string, note: string | null) => call<void>("switcher_set_note", { id, note }),
  remove: (id: string) => call<void>("switcher_remove", { id }),
  switchTo: (id: string, relaunch = true) =>
    call<SwitchOutcome>("switcher_switch_to", { id, relaunch }),
  backups: () => call<AuthBackup[]>("switcher_backups"),
  backupNow: () => call<AuthBackup | null>("switcher_backup_now"),
  removeBackup: (id: string) => call<void>("switcher_remove_backup", { id }),
  restoreBackup: (id: string, relaunch = true) =>
    call<SwitchOutcome>("switcher_restore_backup", { id, relaunch }),
  restoreMachine: (relaunch = true) => call<string>("switcher_restore_machine", { relaunch }),
};

// ── 我的账号 ─────────────────────────────────────────────────────────────────

export const accounts = {
  /** 连归档的一起给；页面自己按 `archivedAt` 分开。 */
  list: () => call<Account[]>("accounts_list"),
  /** 归档 / 取消归档一批号。只是收起来，凭证不动。 */
  setArchived: (ids: string[], archived: boolean) => call<Account[]>("accounts_set_archived", { ids, archived }),
  add: (input: {
    email: string;
    refreshToken?: string;
    /** session / access token：`user_xxx::<jwt>` 或裸 JWT。没 refresh 时靠它撑到过期。 */
    accessToken?: string;
    /** 长期 `crsr_…` User API Key。 */
    apiKey?: string;
    cursorPassword?: string;
    emailPassword?: string;
    recoveryEmail?: string;
    note?: string;
  }) => call<Account>("accounts_add", input),
  /** 解析一份粘进来的清单，只预览不写库。 */
  parseDump: (text: string) => call<ImportPreview>("accounts_parse_dump", { text }),
  importDump: (text: string) => call<ImportOutcome>("accounts_import_dump", { text }),
  /**
   * 把全部账号连凭证导出成清单文件（`~/.roviix/exports`），与 `importDump` 是同一种文件。
   * 只回路径和条数；文件里是明文凭证，Rust 侧会记一条 warn 级活动日志。
   */
  exportDump: () => call<ExportOutcome>("accounts_export_dump"),
  patch: (
    id: string,
    patch: { note?: string; tags?: string[]; codeChannel?: string; status?: string },
  ) => call<Account>("accounts_patch", { id, patch }),
  remove: (id: string) => call<void>("accounts_remove", { id }),
  // 本地零点由这边递过去：「今天」从几点算只有 WebView 知道得可靠，Rust 在多线程进程里
  // 拿不到本地时区。Rust 拿它多问两次（今天 / 近 7 天）。
  refreshUsage: (id: string) =>
    call<AccountUsage>("accounts_refresh_usage", { id, dayStartMs: startOfLocalDay() }),
  /** 读这个号的 Stripe 订阅账单（标价 / 折扣 / 发票）。门户密钥不回给前端。 */
  refreshBilling: (id: string) => call<AccountBilling>("accounts_refresh_billing", { id }),
  /** 改按需计费。`limitCents` 不传且开启 = 不封顶。成功后返回刚刷过的用量。 */
  setOnDemand: (id: string, enabled: boolean, limitCents?: number | null) =>
    call<AccountUsage>("accounts_set_on_demand", {
      id,
      enabled,
      limitCents: limitCents ?? null,
      dayStartMs: startOfLocalDay(),
    }),
  refreshAll: (ids?: string[]) =>
    call<number>("accounts_refresh_all", { ids, dayStartMs: startOfLocalDay() }),
  startOauth: (email: string) => call<OauthStarted>("accounts_start_oauth", { email }),
  cancelOauth: (uuid: string) => call<void>("accounts_cancel_oauth", { uuid }),
  listSessions: (id: string) => call<ActiveSession[]>("accounts_list_sessions", { id }),
  kickSessions: (id: string) => call<KickOutcome>("accounts_kick_sessions", { id }),
  /** 秘密唯一的 IPC 出口。会记活动日志。 */
  revealSecret: (id: string, kind: SecretKind) =>
    call<string>("accounts_reveal_secret", { id, kind }),
  /** 会话 token（`user_xxx::<jwt>`）。派生自 refresh_token，手上那把过期就当场换一把。 */
  revealSession: (id: string) => call<string>("accounts_reveal_session", { id }),
  /**
   * 用这个号手上那把 access 铸一把长期 `crsr_` API Key 并落库。
   *
   * 给只有 session token 的号保命用：那批号没有 refresh、接不了验证码，access 一过期就
   * 彻底拿不回来。铸 key 只认 access，不要密码、不要验证码。铸完额度仍能用（网关 / CRSR
   * 通道），但**换不回切号能力**——`crsr_` 兑出来的 JWT 登不进 Cursor。
   */
  mintApiKey: (id: string) => call<MintedApiKey>("accounts_mint_api_key", { id }),
  /** 改一条凭证（线下改过密码就从这里更新）。传空字符串 = 清除这一条。 */
  setSecret: (id: string, kind: SecretKind, value: string) =>
    call<Account>("accounts_set_secret", { id, kind, value }),
  /**
   * 把这个号的登录态拷进切号本 —— `nexus-accounts` 与 `nexus-switcher` 之间唯一的数据通路。
   * 只由切号池的显式「添加账号」动作调用。
   */
  addToSwitchBook: (id: string) => call<SwitchProfile>("accounts_add_to_switch_book", { id }),
};

// ── 本地备份（~/.roviix/backups）────────────────────────────────────────────

export const backup = {
  list: () => call<LocalBackup[]>("backup_list"),
  /** 现在快照一份整库（账号、凭证、切号本、设置…）。 */
  create: () => call<LocalBackup>("backup_create"),
  /**
   * 用某份备份覆盖当前库。Rust 侧会先把当前状态另存为 `pre-restore`。
   * 还原后要 `relaunch()`：有些模块在启动时读过库就攥在内存里。
   */
  restore: (fileName: string) => call<RestoreOutcome>("backup_restore", { fileName }),
  remove: (fileName: string) => call<void>("backup_remove", { fileName }),
  /** 在 Finder / 资源管理器里显示一个文件；不传就打开备份目录。只认 `~/.roviix` 下的路径。 */
  reveal: (path?: string) => call<void>("backup_reveal", { path }),
};

// ── Sand 补丁 ────────────────────────────────────────────────────────────────

export const sand = {
  release: () => call<CursorRelease>("sand_release"),
  status: () => call<SandStatus>("sand_status"),
  install: (options?: SandInstallOptions) => call<SandOutcome>("sand_install", { options }),
  uninstall: (relaunch = true) => call<SandOutcome>("sand_uninstall", { relaunch }),
  backups: () => call<SandBackup[]>("sand_backups"),
  removeBackup: (id: string) => call<void>("sand_remove_backup", { id }),
  /** 紧急刹车：按字节写回某份备份。 */
  restoreBackup: (id: string, relaunch = true) =>
    call<SandOutcome>("sand_restore_backup", { id, relaunch }),
};

// ── CRSR 补丁（原生 Agent 面板走 crsr_；和 Sand 互斥）────────────────────────

export const crsr = {
  status: () => call<CrsrStatus>("crsr_status"),
  install: (relaunch = true) => call<CrsrOutcome>("crsr_install", { relaunch }),
  uninstall: (relaunch = true) => call<CrsrOutcome>("crsr_uninstall", { relaunch }),
  backups: () => call<CrsrBackup[]>("crsr_backups"),
  removeBackup: (id: string) => call<void>("crsr_remove_backup", { id }),
  restoreBackup: (id: string, relaunch = true) =>
    call<CrsrOutcome>("crsr_restore_backup", { id, relaunch }),
  /** 用这个号的 crsr_ 兑票写成凭证。不必重装补丁。 */
  mintForAccount: (id: string) => call<CrsrCredentialInfo>("crsr_mint_for_account", { id }),
  clearCredential: () => call<void>("crsr_clear_credential"),
};

// ── Grok Bot 桥（不是账号系统；Sand / 网关按需借额度）──────────────────────────

export const grokbot = {
  /** 只读、不碰钥匙串。 */
  status: () => call<GrokBotStatus>("grokbot_status"),
  launch: () => call<void>("grokbot_launch"),
  /** Grok Bot 登着谁（首次会弹钥匙串授权）。 */
  identify: () => call<GrokBotIdentity>("grokbot_identify"),
  /** 把 Grok Bot 当前账号（email + refresh token）收进「我的账号」；已在库里就补 token。 */
  importActiveAccount: () => call<Account>("grokbot_import_active_account"),
  /** 从 Grok Bot 读 gateway descriptor → grok-box-relay.json（box_relay 的前提）。首次会弹钥匙串授权。 */
  relayRefresh: () => call<GrokBotRelayInfo>("grokbot_relay_refresh"),
  relayClear: () => call<void>("grokbot_relay_clear"),
  /** 生成直连凭证：descriptor → pod 读 sbi → 续期 → 落盘（direct / 网关 Grok 开关的前提）。 */
  mintDirect: () => call<GrokBotDirectMinted>("grokbot_mint_direct"),
  /**
   * 不经 Grok Bot 客户端：用账号库里这个号直接换到 grokBotToken 并写成直连凭证——Agent 面板下一发就花它的额度。
   * 会给这个号建（或唤醒）一个 Box pod。
   */
  mintForAccount: (id: string) => call<GrokBotDirectMinted>("grokbot_mint_for_account", { id }),
  /** 手动续一次 token（正常不需要，补丁 / 网关会自己续）。 */
  renewDirect: () => call<GrokBotDirectMinted>("grokbot_renew_direct"),
  clearDirect: () => call<void>("grokbot_clear_direct"),
  /** Grok Bot 换了账号 / 重装后：忘掉缓存的口令与秘密。 */
  forget: () => call<void>("grokbot_forget"),
  /** 上次探过这个号的 sand-cua 落点（磁盘缓存）。 */
  cuaProbeGet: (id: string) => call<GrokBotCuaProbe | null>("grokbot_cua_probe_get", { id }),
  /** 打一发极短 sand-cua，看这个号有没有 grok 4.7 灰度。不覆盖本机正在用的凭证。 */
  cuaProbe: (id: string) => call<GrokBotCuaProbe>("grokbot_cua_probe", { id }),
};

// ── 远程 Sand（remote SSH）─────────────────────────────────────────────────────

export const sandRemote = {
  /** 每台主机各走一次 ssh；连不上的主机在结果里是 `{ Err }`，不拖累别的。 */
  overview: () => call<RemoteOverview>("sand_remote_overview"),
  hosts: () => call<RemoteHost[]>("sand_remote_hosts"),
  /** 先探一次（能连上、能找到 server）再收进列表；返回探到的状态。 */
  addHost: (host: RemoteHost) => call<RemoteStatus>("sand_remote_add_host", { host }),
  removeHost: (host: string) => call<RemoteHost[]>("sand_remote_remove_host", { host }),
  updateHost: (host: RemoteHost) => call<RemoteHost[]>("sand_remote_update_host", { host }),
  /** 装完只要这条路要隧道就顺手拉起来。 */
  install: (host: string, route: RemoteRoute = "gateway") =>
    call<RemoteOutcome>("sand_remote_install", { host, route }),
  uninstall: (host: string) => call<RemoteOutcome>("sand_remote_uninstall", { host }),
  /**
   * 从远程实地走一遍出网链路，报出断在哪一跳。打的是常驻隧道的远程口（远程 Agent 用的正是它）。
   * 要走一条 ssh，几秒，所以是显式动作而不是随 overview 一起跑。
   */
  probe: (host: string) => call<ProbeReport>("sand_remote_probe", { host }),
  tunnelStart: (host: string) => call<TunnelStatus>("sand_remote_tunnel_start", { host }),
  tunnelStop: (host: string) => call<TunnelStatus>("sand_remote_tunnel_stop", { host }),
  tunnelStatus: (host: string) => call<TunnelStatus>("sand_remote_tunnel_status", { host }),
};

// ── 本地网关 ─────────────────────────────────────────────────────────────────

export const gateway = {
  status: () => call<GatewayStatus>("gateway_status"),
  /**
   * 最近 `days` 天的本地用量。时区偏移在这里算好带过去：「今天」得按用户墙上的钟，
   * Rust 侧不知道 WebView 在哪个时区。
   */
  usage: (days: number) =>
    call<UsageSummary>("gateway_usage", { days, tzOffsetMin: -new Date().getTimezoneOffset() }),
  /** IDE Agent 面板经本机网关的用量，与 `usage` 的账分开（口径不同）。 */
  ideUsage: (days: number) =>
    call<UsageSummary>("gateway_ide_usage", { days, tzOffsetMin: -new Date().getTimezoneOffset() }),
  /** 改 IDE 拦截的改写规则；落库并立刻生效。 */
  setIntercept: (rule: RewriteRule) => call<GatewayStatus>("gateway_set_intercept", { rule }),
  /** 透传口的 Grok Bot 额度开关（热生效；开时没凭证会当场生成，可能弹钥匙串授权）。 */
  setGrokbotStream: (on: boolean) => call<GatewayStatus>("gateway_set_grokbot_stream", { on }),
  start: () => call<GatewayStatus>("gateway_start"),
  stop: () => call<GatewayStatus>("gateway_stop"),
  updateSettings: (patch: {
    port?: number;
    passthroughPort?: number;
    clientType?: string;
    autostart?: boolean;
    forceModel?: string | null;
    defaultChannel?: string;
  }) => call<GatewaySettings>("gateway_update_settings", { patch }),
  /** 把号放进网关的接力队。名单外的号网关一概不碰，所以只有用户点了才进来。 */
  enroll: (labels: string[]) => call<GatewayStatus>("gateway_enroll", { labels }),
  unenroll: (label: string) => call<GatewayStatus>("gateway_unenroll", { label }),
  setCurrent: (label: string) => call<GatewayStatus>("gateway_set_current", { label }),
  resetLane: () => call<GatewayStatus>("gateway_reset_lane"),
  /** 订阅通道（chatgpt / grok / kiro）的接力队：指定当前号 / 清掉耗尽与冷却。 */
  channelSetCurrent: (channel: GatewayChannelId, label: string) =>
    call<GatewayStatus>("gateway_channel_set_current", { channel, label }),
  channelResetLane: (channel: GatewayChannelId) => call<GatewayStatus>("gateway_channel_reset_lane", { channel }),
  /** 最近的异步媒体任务（生视频）。 */
  mediaJobs: (limit = 20) => call<MediaJob[]>("gateway_media_jobs", { limit }),
  /** 口令的唯一出口。用户显式点「显示 / 复制」才调。 */
  revealKey: () => call<string>("gateway_reveal_key"),
  rotateKey: () => call<string>("gateway_rotate_key"),
};

// ── 订阅通道的账号（ChatGPT / Grok / Kiro）──────────────────────────────────────

export const chatgpt = {
  list: () => call<ChatGptAccount[]>("chatgpt_list"),
  /** 上次从上游拉到的模型目录；空 = 还没拉过，网关用静态清单。 */
  models: () => call<ChatGptManifestModel[]>("chatgpt_models"),
  /** 现在拉一次目录（加号和开网关时也会自动拉）。 */
  refreshModels: () => call<ChatGptManifestModel[]>("chatgpt_refresh_models"),
  /**
   * 发起授权：Rust 侧拼好链接、在本机 1455 上把回调监听绑起来、打开系统浏览器。
   * `callbackListening` 为 true 时后续状态经 `onChatGptLogin` 推回；否则要 `loginComplete`。
   */
  loginStart: (note?: string) => call<ChatGptLoginHandle>("chatgpt_login_start", { note }),
  /** 手贴路径：把浏览器地址栏的回调地址（或 `code#state`）贴回来。 */
  loginComplete: (sessionId: string, callback: string) =>
    call<ChatGptAccount>("chatgpt_login_complete", { sessionId, callback }),
  loginCancel: (sessionId: string) => call<void>("chatgpt_login_cancel", { sessionId }),
  /** 读本机 `~/.codex/auth.json`（`codex login` 留下的）。 */
  importCodexCli: () => call<ChatGptAccount>("chatgpt_import_codex_cli"),
  /** `auth.json` / sub2api Codex session JSON / `access----refresh` / 单个 refresh。可一次多个。明文只进 Rust。 */
  importText: (text: string, note?: string) =>
    call<ChatGptImportOutcome>("chatgpt_import_text", { text, note }),
  remove: (id: string) => call<void>("chatgpt_remove", { id }),
  setEnabled: (id: string, enabled: boolean) =>
    call<ChatGptAccount>("chatgpt_set_enabled", { id, enabled }),
  setNote: (id: string, note: string | null) =>
    call<ChatGptAccount>("chatgpt_set_note", { id, note }),
  refreshUsage: (id: string) => call<ChatGptUsage>("chatgpt_refresh_usage", { id }),
  refreshBilling: (id: string) => call<ChatGptBilling>("chatgpt_refresh_billing", { id }),
  /** ChatGPT 通道的接力队操作走网关的通道命令。 */
  setCurrent: (label: string) => gateway.channelSetCurrent("chatgpt", label),
  resetLane: () => gateway.channelResetLane("chatgpt"),
};

export const grok = {
  list: () => call<DeviceAccount[]>("grok_list"),
  loginStart: (note?: string) => call<DeviceLoginHandle>("grok_login_start", { note }),
  loginCancel: (sessionId: string) => call<void>("grok_login_cancel", { sessionId }),
  importCli: () => call<DeviceAccount>("grok_import_cli"),
  /** `~/.grok/auth.json` 原文 / `access----refresh` / 一把 `xai-…` API Key 都认。 */
  importText: (text: string, note?: string) => call<DeviceAccount>("grok_import_text", { text, note }),
  /** 加一个 xAI API Key 号（按 token 计费，走 api.x.ai）。 */
  addApiKey: (apiKey: string, note?: string) => call<DeviceAccount>("grok_add_api_key", { apiKey, note }),
  remove: (id: string) => call<void>("grok_remove", { id }),
  setEnabled: (id: string, enabled: boolean) => call<DeviceAccount>("grok_set_enabled", { id, enabled }),
  setNote: (id: string, note: string | null) => call<DeviceAccount>("grok_set_note", { id, note }),
  /** 现在拉一次额度 / 档位。 */
  refreshQuota: (id: string) => call<DeviceAccount>("grok_refresh_quota", { id }),
  /** 媒体资格手动覆盖：true 强开 / false 强关 / null 回到自动。 */
  setMediaOverride: (id: string, value: boolean | null) =>
    call<DeviceAccount>("grok_set_media_override", { id, value }),
  models: () => call<GrokManifestModel[]>("grok_models"),
  refreshModels: () => call<GrokManifestModel[]>("grok_refresh_models"),
  setCurrent: (label: string) => gateway.channelSetCurrent("grok", label),
  resetLane: () => gateway.channelResetLane("grok"),
};

export const kiro = {
  list: () => call<DeviceAccount[]>("kiro_list"),
  loginStart: (note?: string) => call<DeviceLoginHandle>("kiro_login_start", { note }),
  loginCancel: (sessionId: string) => call<void>("kiro_login_cancel", { sessionId }),
  importCli: () => call<DeviceAccount>("kiro_import_cli"),
  importText: (text: string, note?: string) => call<DeviceAccount>("kiro_import_text", { text, note }),
  remove: (id: string) => call<void>("kiro_remove", { id }),
  setEnabled: (id: string, enabled: boolean) => call<DeviceAccount>("kiro_set_enabled", { id, enabled }),
  setNote: (id: string, note: string | null) => call<DeviceAccount>("kiro_set_note", { id, note }),
  setCurrent: (label: string) => gateway.channelSetCurrent("kiro", label),
  resetLane: () => gateway.channelResetLane("kiro"),
};

// ── 一键接入 ─────────────────────────────────────────────────────────────────

/** 只有 Claude Code / Codex 有能直接写的配置文件。 */
export type ConnectTool = "claude" | "codex" | "opencode" | "grok";

export const connect = {
  /** 这个工具的配置现在指向哪。不改任何东西。 */
  inspect: (tool: ConnectTool) => call<ClientState>("connect_inspect", { tool }),
  /**
   * 把工具接到本地网关上：Rust 侧自己解出地址与钥匙、备份原文件、合并写入。
   * 钥匙不经过前端。返回写了哪些文件。
   */
  apply: (tool: ConnectTool, model: string) => call<ConnectApplied>("connect_apply", { tool, model }),
  /** 撤销：按清单还原备份 / 删掉我们建的文件；没清单就只剔我们的键。 */
  revert: (tool: ConnectTool) => call<ConnectReverted>("connect_revert", { tool }),
};

// ── 权限预检 ────────────────────────────────────────────────────────────────

export const perms = {
  /** 现状。不弹任何系统窗。 */
  check: () => call<PermReport>("perms_check"),
  /** 申请：会弹系统窗的探针真跑一遍。不传 id = 全部。 */
  request: (id?: string) => call<PermReport>("perms_request", { id: id ?? null }),
  /** 首次启动的准备工作走完了（不管结果），以后不再主动弹。 */
  markPreflight: () => call<void>("perms_mark_preflight"),
  /** 打开系统设置里对应的那一页（macOS）。 */
  openSettings: (id: string) => call<void>("perms_open_settings", { id }),
};

// ── 事件 ────────────────────────────────────────────────────────────────────

/** 事件名。与 `commands::events` 一一对应。 */
export const EVENTS = {
  switchProgress: "switcher://progress",
  oauthState: "oauth://state",
  chatgptLogin: "chatgpt://login",
  grokLogin: "grok://login",
  kiroLogin: "kiro://login",
  accountRefreshed: "accounts://refreshed",
  sandProgress: "sand://progress",
  crsrProgress: "crsr://progress",
  sandRemoteProgress: "sand://remote-progress",
} as const;

export function onSandProgress(cb: (p: SandProgress) => void): Promise<UnlistenFn> {
  return listen<SandProgress>(EVENTS.sandProgress, (e) => cb(e.payload));
}

export function onCrsrProgress(cb: (p: CrsrProgress) => void): Promise<UnlistenFn> {
  return listen<CrsrProgress>(EVENTS.crsrProgress, (e) => cb(e.payload));
}

export function onSandRemoteProgress(cb: (p: SandProgress) => void): Promise<UnlistenFn> {
  return listen<SandProgress>(EVENTS.sandRemoteProgress, (e) => cb(e.payload));
}

export function onSwitchProgress(cb: (p: SwitchProgress) => void): Promise<UnlistenFn> {
  return listen<SwitchProgress>(EVENTS.switchProgress, (e) => cb(e.payload));
}

export function onOauthState(cb: (s: OauthState) => void): Promise<UnlistenFn> {
  return listen<OauthState>(EVENTS.oauthState, (e) => cb(e.payload));
}

export function onChatGptLogin(cb: (s: ChatGptLoginState) => void): Promise<UnlistenFn> {
  return listen<ChatGptLoginState>(EVENTS.chatgptLogin, (e) => cb(e.payload));
}

export function onGrokLogin(cb: (s: DeviceLoginState) => void): Promise<UnlistenFn> {
  return listen<DeviceLoginState>(EVENTS.grokLogin, (e) => cb(e.payload));
}

export function onKiroLogin(cb: (s: DeviceLoginState) => void): Promise<UnlistenFn> {
  return listen<DeviceLoginState>(EVENTS.kiroLogin, (e) => cb(e.payload));
}

export interface AccountRefreshed {
  id: string;
  ok: boolean;
  message?: string | null;
}

export function onAccountRefreshed(cb: (r: AccountRefreshed) => void): Promise<UnlistenFn> {
  return listen<AccountRefreshed>(EVENTS.accountRefreshed, (e) => cb(e.payload));
}
