/**
 * 账号 · Grok Build / Kiro / ZCode —— 「号只喂本机网关」的平台共用这一页。
 *
 * 和 ChatGPT 页签同一套心智（号只喂本机网关、凭证不进前端、接力状态来自网关），
 * 差别在怎么把号弄进来：
 *
 * - Grok / Kiro 走 OIDC **设备码**：浏览器里输入 user_code，没有 Codex 那种
 *   localhost:1455 回调可贴，所以弹窗里最显眼的是那串码而不是「贴地址」。
 * - ZCode **没有授权登录**：官方客户端登录后把凭证写在本机一个加密 JSON 里，
 *   用户在那边登录一次，这里直接读。`spec.api.loginStart` 缺席就是这个意思，
 *   「授权登录」页签会整个消失。
 *
 * Grok Bot（钥匙串里的 grok.com 额度）不是 Grok Build，别混进这一页。
 */
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { laneOf } from "../../gateway/channels";
import {
  antigravity,
  claude,
  gateway,
  grok,
  kiro,
  onAntigravityLogin,
  onClaudeLogin,
  onGrokLogin,
  onKiroLogin,
  qoder,
  zcode,
} from "../../ipc/api";
import type {
  GatewayChannelId,
  DeviceAccount,
  DeviceLoginHandle,
  DeviceLoginState,
  GatewayCandidate,
  GatewayStatus,
} from "../../ipc/types";
import type { Route } from "../../shell/nav";
import { confirm } from "../../ui/confirm";
import { timeAgo, timeUntil } from "../../ui/format";
import { resetInShort } from "../../ui/usage";
import { Banner, CopyButton, Empty, ErrorNote, Gauge, Icon, Modal, Spinner, Switch, Tag } from "../../ui/primitives";
import { AntigravityAccountCard } from "./AntigravityAccountCard";
import { ClaudeAccountCard } from "./ClaudeAccountCard";
import { ClaudeDrawer } from "./ClaudeDrawer";
import { GrokAccountCard } from "./GrokAccountCard";
import { GrokDrawer } from "./GrokDrawer";
import { KiroAccountCard } from "./KiroAccountCard";
import { KiroDrawer } from "./KiroDrawer";
import { QoderAccountCard } from "./QoderAccountCard";
import { QoderDrawer } from "./QoderDrawer";
import { ZcodeAccountCard } from "./ZcodeAccountCard";
import { ZcodeDrawer } from "./ZcodeDrawer";
import { openUrl } from "@tauri-apps/plugin-opener";
import { laneBadge } from "./chatgpt";

type DeviceApi = {
  list: () => Promise<DeviceAccount[]>;
  /** 走设备码授权的平台才有。缺席时「授权登录」页签不出现（ZCode）。 */
  loginStart?: (note?: string) => Promise<DeviceLoginHandle>;
  loginCancel?: (sessionId: string) => Promise<void>;
  /**
   * 从本机装着的那个客户端导入（Grok CLI / Kiro IDE / ZCode 桌面端）。
   *
   * 回值没人用：导入完一律重拉列表。ZCode 一次能带回好几个号，所以不限定成单个。
   */
  importLocal?: () => Promise<unknown>;
  importText: (text: string, note?: string, backend?: string) => Promise<unknown>;
  remove: (id: string) => Promise<void>;
  setEnabled: (id: string, enabled: boolean) => Promise<DeviceAccount>;
  setNote?: (id: string, note: string | null) => Promise<DeviceAccount>;
  setCurrent: (label: string) => Promise<GatewayStatus>;
  resetLane: () => Promise<GatewayStatus>;
  /** 只有 Grok 有：额度探测、媒体资格覆盖、API Key 号。 */
  refreshQuota?: (id: string) => Promise<DeviceAccount>;
  setMediaOverride?: (id: string, value: boolean | null) => Promise<DeviceAccount>;
  addApiKey?: (apiKey: string, note?: string) => Promise<DeviceAccount>;
};

interface Spec {
  id: GatewayChannelId;
  title: string;
  emptyTitle: string;
  emptyBody: string;
  bannerHint: string;
  usableHint: string;
  addSubtitle: string;
  oauthHint: string;
  waitingHint: string;
  cliTab: string;
  cliHint: ReactNode;
  pastePlaceholder: string;
  /** 粘贴时要先选一边（Qoder 国际 / 国内）。没有就不出。 */
  pasteBackends?: { id: string; label: string }[];
  /** 「API Key」页签的提示；没有就不出这个页签。 */
  apiKeyHint?: ReactNode;
  labelPrefix: string;
  api: DeviceApi;
  /** 设备码授权的进度事件。和 `api.loginStart` 同进同退。 */
  listen?: (cb: (s: DeviceLoginState) => void) => Promise<() => void>;
  /**
   * PKCE 授权：先打开授权页。本机回调口在听时同意后自动完成；
   * 口被占了再把地址栏贴回来。和设备码二选一。
   * 返回的地址由弹窗自己打开；取消时调 `api.loginCancel`。
   */
  prepareOauth?: (note?: string) => Promise<{
    authorizeUrl: string;
    loginId?: string;
    callbackListening?: boolean;
  }>;
  /** PKCE 授权进度事件（Claude / Antigravity）。 */
  listenPkce?: (cb: (st: any) => void) => Promise<() => void>;
  /** 本机那个客户端在不在。给了就在「从本机导入」页签上先说清楚，
   * 而不是让用户点一下再吃一个「读不到文件」。
   */
  probeLocal?: () => Promise<{ present: boolean; path: string; also?: string | null }>;
  /** 授权渠道列表（例如 Kiro 支持 Google、GitHub、AWS Builder ID）。 */
  oauthProviders?: { id: string; label: string; desc?: string }[];
}

const GROK: Spec = {
  id: "grok",
  title: "Grok Build",
  emptyTitle: "还没有 Grok 账号",
  emptyBody:
    "xAI 的 Grok Build 订阅号，或一把 xAI API Key。加进来后，客户端把 grok-* / grok-imagine-* 指到本机网关就会走这条通道：对话、生图、生视频都在。凭证只在这台电脑上。这不是 Grok Bot。",
  bannerHint: "Grok 的号只有一个用途：给本机网关跑 grok-* 与 Imagine 媒体。开了网关，OpenCode / Grok CLI 指到它就能用。",
  usableHint: "个可用",
  addSubtitle: "订阅号走 xAI 设备码授权；也可以直接贴一把 xAI API Key。凭证只存在这台电脑上。",
  oauthHint: "浏览器会打开 xAI 的设备码页。把下面那串码输进去，用 X 账号同意即可——没有回调地址可贴。",
  waitingHint: "在打开的页面里输入这串码，用 X 账号登录并同意。",
  cliTab: "从本机 Grok CLI",
  cliHint: (
    <>
      读取本机 <code className="mono">~/.grok/auth.json</code>（Grok CLI 登录留下的态）。文件不动，只把凭证收进 Nexus。
    </>
  ),
  pastePlaceholder:
    "四种写法都认：\n· ~/.grok/auth.json 的原文\n· CLIProxyAPI type=xai 的 JSON\n· access_token----refresh_token\n· 一把 xai-… API Key",
  apiKeyHint: (
    <>
      xAI 开发者平台（console.x.ai）的 API Key，按 token 计费，走 <code className="mono">api.x.ai</code>。
      和订阅号一样进这条通道；生图 / 生视频对它一律放行（按你的余额扣）。
    </>
  ),
  labelPrefix: "grok",
  api: grok,
  listen: onGrokLogin,
};

const KIRO: Spec = {
  id: "kiro",
  title: "Kiro",
  emptyTitle: "还没有 Kiro 账号",
  emptyBody:
    "Google / GitHub / AWS Builder ID 的 Kiro 订阅。加进来后，kiro-claude-* 走这条通道。裸的 claude-* 不会进这里——那是 Cursor 的号。凭证只在这台电脑上。",
  bannerHint: "Kiro 的号用于网关 kiro-claude-* 访问。",
  usableHint: "个可用",
  addSubtitle: "支持 Google / GitHub 授权登录（Pro+ / Pro / Power 个人订阅）及 AWS Builder ID 设备码。",
  oauthHint: "推荐使用 Google / GitHub 登录，获取 Kiro Pro+ / Pro 等付费订阅套餐；AWS Builder ID 仅提供 Free 免费额度。",
  waitingHint: "在打开的页面里输入这串码，用 AWS Builder ID 登录并同意。",
  cliTab: "从本机 Kiro IDE",
  cliHint: (
    <>
      读取 <code className="mono">~/.aws/sso/cache/kiro-auth-token.json</code>（Kiro IDE 留下的态）。文件不动，只把凭证收进 Nexus。
    </>
  ),
  pastePlaceholder:
    "五种写法都认：\n· kiro://kiro.kiroAgent/authenticate-success?code=… 重定向链接\n· kiro-auth-token.json 的原文\n· sub2api 导出的 accounts JSON\n· CLIProxyAPI 的 JSON\n· access_token----refresh_token",
  labelPrefix: "kiro",
  api: kiro,
  listen: onKiroLogin,
  oauthProviders: [
    { id: "Google", label: "Google 授权（推荐）", desc: "适用于 Kiro 官网的 Pro+、Pro、Power 等个人付费订阅（无需本地安装 Kiro）" },
    { id: "Github", label: "GitHub 授权", desc: "适用于通过 GitHub 绑定的 Kiro 个人付费订阅（无需本地安装 Kiro）" },
    { id: "builder-id", label: "AWS Builder ID", desc: "通过 AWS 设备码登录，仅提供 Free 免费额度（50次请求）" },
  ],
};

const ZCODE: Spec = {
  id: "zcode",
  title: "ZCode",
  emptyTitle: "还没有 ZCode 账号",
  emptyBody:
    "智谱 GLM 的编码套餐。在官方 ZCode 客户端里登录一次，这里就能把凭证收进来——加进来后，glm-* 走这条通道（zcode/glm-4.7 也认）。凭证只在这台电脑上。",
  bannerHint: "ZCode 的号用于网关 glm-* 访问。",
  usableHint: "个可用",
  addSubtitle:
    "ZCode 没有单独的授权流程：在官方客户端里登录一次，这里直接读它留下的凭证。也可以自己贴一把 API key。",
  // 没有 loginStart，这两条用不上，但 Spec 要求非空。
  oauthHint: "",
  waitingHint: "",
  cliTab: "从本机 ZCode 客户端",
  cliHint: (
    <>
      读取 <code className="mono">~/.zcode/v2/credentials.json</code>（官方客户端登录后留下的态）。文件不动，只把凭证收进
      Nexus。一次会把里面所有套餐都收进来——个人版、团队版各算一个号，额度是分开的。
      <br />
      这个文件的加密密钥绑了本机用户名与主目录，<strong>不能从别的电脑拷过来</strong>。
    </>
  ),
  pastePlaceholder:
    "三种写法都认：\n· 一行 {apiKeyId}.{apiKeySecret}\n· 一整份 credentials.json 的原文\n· 一个 JWT（体验套餐）",
  labelPrefix: "zcode",
  api: zcode,
  probeLocal: zcode.probeClient,
};

const QODER: Spec = {
  id: "qoder",
  title: "Qoder",
  emptyTitle: "还没有 Qoder 账号",
  emptyBody:
    "Qoder 的号。贴一把 Personal Access Token，这里换成能签名的短票，qoder/Qwen3.8-Max 这类模型走这条通道。国际版和国内版是两条号，目录不一样。凭证只在这台电脑上。",
  bannerHint: "Qoder 的号只有一个用途：给本机网关跑 qoder/…。在「接入」里把客户端的通道选成 Qoder 就走这里。不经过 qodercli。",
  usableHint: "个可接 · qoder/… 走这里",
  addSubtitle: "在 Qoder 的集成页创建一把 Personal Access Token。国际版和国内版要分开贴，各算一个号。",
  oauthHint: "",
  waitingHint: "",
  cliTab: "",
  cliHint: null,
  pastePlaceholder: "一行一把 Personal Access Token。\n某一行想换边，写成 cn pt-… 或 global pt-…",
  pasteBackends: [
    { id: "global", label: "国际版" },
    { id: "cn", label: "国内版" },
  ],
  labelPrefix: "qoder",
  api: qoder,
};

const CLAUDE: Spec = {
  id: "claude",
  title: "Claude",
  emptyTitle: "还没有 Claude 账号",
  emptyBody:
    "Claude Pro / Max / Team 的订阅，或一把 Console API Key。加进来后，claude/claude-opus-4-6 这类模型走这条通道，直连 api.anthropic.com。凭证只在这台电脑上。",
  bannerHint:
    "Claude 的号只有一个用途：给本机网关跑 claude/…。在「接入」里把客户端指到本机网关，模型写成 claude/claude-sonnet-4-6。OAuth / setup-token 出站按 Claude Code 的指纹发，用量记在套餐额度里，不进 extra usage。",
  usableHint: "个可接 · claude/… 走这里",
  addSubtitle: "Pro / Max 走 Claude 的授权页；也可以导入本机 Claude Code 的凭证、setup-token，或贴一把 Console API Key。",
  oauthHint:
    "和 claude 登录走同一条路：系统浏览器里用 Claude 账号同意，授权码会回到本机 54545。54545 被占时，把跳转后的地址栏整条贴回来。",
  waitingHint: "浏览器已打开。同意之后会自动回到这里。",
  cliTab: "从本机 Claude Code",
  cliHint: (
    <>
      读取 <code className="mono">~/.claude/.credentials.json</code>。macOS 上如果 token 只在钥匙串
      「Claude Code-credentials」里，导入时会再读那里（可能弹出一次钥匙串确认）。文件和钥匙串都不动，只把凭证收进 Nexus。
    </>
  ),
  pastePlaceholder:
    "这几种都认：\n· 授权完成后地址栏里的整段地址\n· ~/.claude/.credentials.json 的原文\n· claude setup-token 打出来的 sk-ant-oat…\n· Console 的 sk-ant-api…",
  labelPrefix: "claude",
  api: {
    list: claude.list,
    loginCancel: claude.loginCancel,
    importLocal: claude.importLocal,
    importText: claude.importText,
    refreshQuota: claude.refreshQuota,
    remove: claude.remove,
    setEnabled: claude.setEnabled,
    setCurrent: claude.setCurrent,
    resetLane: claude.resetLane,
  },
  prepareOauth: (note) => claude.loginStart(note),
  listenPkce: onClaudeLogin,
  probeLocal: claude.probeLocal,
};

export const ANTIGRAVITY: Spec = {
  id: "antigravity",
  title: "Antigravity",
  emptyTitle: "还没有 Google 反重力 (Antigravity) 账号",
  emptyBody:
    "Google Antigravity 官方 OAuth 授权或 Refresh Token 凭证。支持 Gemini 3 / 2.5 系列与 Claude 3/4 模型家族。凭证保存在本地加密存储中，直连 Google 官方端点。",
  bannerHint:
    "Google Antigravity 统一网关：支持 gemini-3.1-pro、gemini-3.8-flash 以及 claude-3-7-sonnet 等模型。内置确定性 Session ID 保持，保护上游 KV-Cache 命中率，额度耗尽自动接力。",
  usableHint: "个可接 · antigravity/… 走这里",
  addSubtitle:
    "点击「登录授权」在浏览器中授权，或直接粘贴 Google 凭证（以 1// 开头的 Refresh Token、跳转 URL、或配置 JSON）。",
  oauthHint:
    "使用 Google OAuth 2.0 PKCE 授权：在系统浏览器中同意，授权码自动回传到本机 51121 端口。若端口受限，也可直接复制授权跳转后的浏览器地址栏 URL 贴回。",
  waitingHint: "浏览器已打开。在 Google 账号授权页点击同意后将自动完成登录。",
  cliTab: "从本机 Antigravity",
  cliHint: (
    <>
      探测系统钥匙串（macOS Keychain 中的 Google 凭证）或本地 <code className="mono">~/.gemini</code> 配置，直接导入已登录的 Google 反重力凭证。
    </>
  ),
  pastePlaceholder:
    "支持以下任意格式：\n· 授权完成后的地址栏跳转完整 URL (http://localhost:51121/oauth-callback?code=...)\n· 以 1// 开头的 Google Refresh Token 原文\n· 包含 refresh_token / access_token 的 JSON 配置",
  labelPrefix: "agy",
  api: {
    list: antigravity.list,
    loginCancel: antigravity.loginCancel,
    importLocal: antigravity.importLocal,
    importText: antigravity.importText,
    refreshQuota: antigravity.refreshQuota,
    remove: antigravity.remove,
    setEnabled: antigravity.setEnabled,
    setNote: antigravity.setNote,
    setCurrent: antigravity.setCurrent,
    resetLane: antigravity.resetLane,
  },
  prepareOauth: (note) => antigravity.loginStart(note),
  listenPkce: onAntigravityLogin,
  probeLocal: antigravity.probeLocal,
};

export function AntigravityAccounts({ tabs, onGo }: { tabs: ReactNode; onGo: (r: Route) => void }) {
  return <DeviceAccounts spec={ANTIGRAVITY} tabs={tabs} onGo={onGo} />;
}

export function ClaudeAccounts({ tabs, onGo }: { tabs: ReactNode; onGo: (r: Route) => void }) {
  return <DeviceAccounts spec={CLAUDE} tabs={tabs} onGo={onGo} />;
}

export function QoderAccounts({ tabs, onGo }: { tabs: ReactNode; onGo: (r: Route) => void }) {
  return <DeviceAccounts spec={QODER} tabs={tabs} onGo={onGo} />;
}

export function GrokAccounts({ tabs, onGo }: { tabs: ReactNode; onGo: (r: Route) => void }) {
  return <DeviceAccounts spec={GROK} tabs={tabs} onGo={onGo} />;
}

export function ZcodeAccounts({ tabs, onGo }: { tabs: ReactNode; onGo: (r: Route) => void }) {
  return <DeviceAccounts spec={ZCODE} tabs={tabs} onGo={onGo} />;
}

export function KiroAccounts({ tabs, onGo }: { tabs: ReactNode; onGo: (r: Route) => void }) {
  return <DeviceAccounts spec={KIRO} tabs={tabs} onGo={onGo} />;
}

/**
 * 号的显示名，**同时是网关接力队里的键**。
 *
 * 后端给了 `label` 就用它——ZCode 一个邮箱下有个人版 / 团队版 / 体验套餐三条号，
 * 光看邮箱三条长得一模一样，键就撞了。所以那边的名字由 Rust 拼好送来，前端不再推导。
 */
function labelOf(a: Pick<DeviceAccount, "email" | "accountRef" | "label">, prefix: string): string {
  const given = a.label?.trim();
  if (given) return given;
  const email = a.email?.trim();
  return email ? email : `${prefix}…${a.accountRef.slice(-6)}`;
}

/** 打开页面时要不要再问一次 Qoder。没有快照，或快照超过一刻钟。 */
function qoderUsageStale(a: DeviceAccount, now = Date.now()): boolean {
  const checked = a.qoderUsage?.checkedAt;
  if (!checked) return true;
  const t = Date.parse(checked);
  return !Number.isFinite(t) || now - t > 15 * 60_000;
}

/**
 * 这个号还能不能自己拿出凭证。
 *
 * OAuth 号看 refresh token，API Key 号看 key，ZCode 看永久 key / JWT——它没有
 * `hasRefresh` 那一位，因为压根没有续期这回事。
 */
function deviceCanServe(a: DeviceAccount): boolean {
  if (a.hasToken) return true;
  if (a.hasRefresh !== undefined) return a.hasRefresh;
  return (a.hasApiKey ?? false) || (a.hasJwt ?? false);
}

const deviceAccountsCache = new Map<string, DeviceAccount[]>();
let lastDeviceGatewayStatus: GatewayStatus | null = null;

function DeviceAccounts({ spec, tabs, onGo: _onGo }: { spec: Spec; tabs: ReactNode; onGo: (r: Route) => void }) {
  const [accounts, setAccounts] = useState<DeviceAccount[] | null>(() => deviceAccountsCache.get(spec.id) ?? null);
  const [status, setStatus] = useState<GatewayStatus | null>(() => lastDeviceGatewayStatus);
  const [kiroActiveId, setKiroActiveId] = useState<string | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [working, setWorking] = useState(false);
  const [adding, setAdding] = useState(false);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  /** 打开 Qoder 页时，缺快照或超过一刻钟的号静默问一次。每个挂载只问一轮。 */
  const usageWarmed = useRef(false);
  const alive = useRef(true);
  useEffect(() => () => {
    alive.current = false;
  }, []);

  const selectedAccount = useMemo(
    () => (accounts ?? []).find((a) => a.id === selectedId) ?? null,
    [accounts, selectedId],
  );

  const reload = useCallback(async () => {
    try {
      const list = await spec.api.list();
      deviceAccountsCache.set(spec.id, list);
      setAccounts(list);
      setError(null);
    } catch (e) {
      setError(e);
    }
    if (spec.id === "kiro") {
      try {
        const active = await kiro.activeClient();
        setKiroActiveId(active?.accountId ?? null);
      } catch {}
    }
    try {
      const st = await gateway.status();
      lastDeviceGatewayStatus = st;
      setStatus(st);
    } catch {
      setStatus(null);
    }
  }, [spec.api, spec.id]);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    if (spec.id !== "qoder" || usageWarmed.current || accounts === null || !spec.api.refreshQuota) return;
    const due = accounts.filter((a) => a.status === "active" && a.hasToken && qoderUsageStale(a));
    if (accounts.length === 0) return;
    usageWarmed.current = true;
    if (due.length === 0) return;
    const refresh = spec.api.refreshQuota;
    void (async () => {
      for (const a of due) {
        if (!alive.current) return;
        try {
          await refresh(a.id);
        } catch {
          /* 失败记在账号的 lastError 上，列表照常能看。 */
        }
      }
      if (alive.current) await reload();
    })();
  }, [accounts, spec.id, spec.api, reload]);

  useEffect(() => {
    if (!status?.running) return;
    const t = window.setInterval(() => void reload(), 5000);
    return () => window.clearInterval(t);
  }, [status?.running, reload]);

  const laneByLabel = useMemo(() => {
    const m = new Map<string, GatewayCandidate>();
    for (const c of laneOf(status, spec.id).candidates) m.set(c.label.toLowerCase(), c);
    return m;
  }, [status, spec.id]);

  async function run(action: () => Promise<unknown>) {
    setWorking(true);
    setError(null);
    try {
      await action();
      await reload();
    } catch (e) {
      setError(e);
    } finally {
      setWorking(false);
    }
  }

  async function remove(a: DeviceAccount) {
    const current = laneByLabel.get(labelOf(a, spec.labelPrefix).toLowerCase())?.state.kind === "current";
    const warn = current ? "它正在被网关使用，进行中的对话会换号并丢上游缓存。" : "";
    const ok = await confirm(`本机保存的凭证一起删除。${warn}`, {
      title: `删除 ${labelOf(a, spec.labelPrefix)}？`,
      okLabel: "删除",
      danger: true,
    });
    if (!ok) return;
    if (selectedId === a.id) setSelectedId(null);
    await run(() => spec.api.remove(a.id));
  }

  const list = accounts ?? [];
  const [filter, setFilter] = useState<"all" | "active" | "disabled">("all");
  const activeCount = list.filter((a) => a.enabled && a.status === "active" && deviceCanServe(a)).length;
  const disabledCount = list.length - activeCount;

  const filteredList = useMemo(() => {
    if (filter === "active") {
      return list.filter((a) => a.enabled && a.status === "active" && deviceCanServe(a));
    }
    if (filter === "disabled") {
      return list.filter((a) => !(a.enabled && a.status === "active" && deviceCanServe(a)));
    }
    return list;
  }, [list, filter]);

  const disabled = working;

  const filterBar = (
    <div className="status-pills" role="tablist" aria-label="账号状态视图" style={{ marginBottom: 12 }}>
      <button
        type="button"
        className={`status-pill${filter === "all" ? " is-active" : ""}`}
        onClick={() => setFilter("all")}
      >
        <span>全部</span>
        <span className="pill-n">{list.length}</span>
      </button>
      <button
        type="button"
        className={`status-pill${filter === "active" ? " is-active" : ""}`}
        onClick={() => setFilter("active")}
      >
        <i className="status-dot is-long_lived" />
        <span>可用</span>
        <span className="pill-n">{activeCount}</span>
      </button>
      {disabledCount > 0 ? (
        <button
          type="button"
          className={`status-pill${filter === "disabled" ? " is-active" : ""}`}
          onClick={() => setFilter("disabled")}
        >
          <i className="status-dot is-dead" />
          <span>已停用</span>
          <span className="pill-n">{disabledCount}</span>
        </button>
      ) : null}
    </div>
  );

  return (
    <>
      <div className="page-head acct-head">
        {tabs}
        <div className="row">
          {list.length > 0 ? (
            <button
              type="button"
              className="btn btn-icon"
              disabled={disabled}
              onClick={() => void run(() => spec.api.resetLane())}
              data-tip="重置接力状态：清掉耗尽 / 冷却记录"
              aria-label="重置接力状态"
            >
              <Icon name="refresh" size={15} className={working ? "is-spinning" : undefined} />
            </button>
          ) : null}
          <button type="button" className="btn btn-primary" disabled={disabled} onClick={() => setAdding(true)}>
            <Icon name="plus" size={14} />
            添加账号
          </button>
        </div>
      </div>

      <ErrorNote error={error} onRetry={() => void reload()} />

      {accounts === null ? (
        <div className="stack" style={{ gap: 10 }}>
          <div className="skeleton" style={{ height: 96 }} />
          <div className="skeleton" style={{ height: 96 }} />
        </div>
      ) : list.length === 0 ? (
        <Empty
          title={spec.emptyTitle}
          action={
            <button type="button" className="btn btn-primary" onClick={() => setAdding(true)}>
              <Icon name="plus" size={14} />
              添加 {spec.title} 账号
            </button>
          }
        >
          {spec.emptyBody}
        </Empty>
      ) : spec.id === "antigravity" ? (
        <>
          {filterBar}
          <div className="accts">
            {filteredList.map((a) => (
              <AntigravityAccountCard
                key={a.id}
                account={a}
                prefix={spec.labelPrefix}
                lane={laneByLabel.get(labelOf(a, spec.labelPrefix).toLowerCase()) ?? null}
                disabled={disabled}
                refreshing={working}
                onToggle={(on) => void run(() => spec.api.setEnabled(a.id, on))}
                onUse={() => void run(() => spec.api.setCurrent(labelOf(a, spec.labelPrefix)))}
                onRelogin={() => setAdding(true)}
                onRemove={() => void remove(a)}
                onRefresh={
                  spec.api.refreshQuota ? () => run(() => spec.api.refreshQuota!(a.id)) : undefined
                }
                onNote={
                  spec.api.setNote
                    ? (note) => void run(() => spec.api.setNote!(a.id, note))
                    : undefined
                }
              />
            ))}
          </div>
        </>
      ) : spec.id === "kiro" ? (
        <>
          {filterBar}
          <div className="accts">
            {filteredList.map((a) => (
              <KiroAccountCard
                key={a.id}
                account={a}
                prefix={spec.labelPrefix}
                lane={laneByLabel.get(labelOf(a, spec.labelPrefix).toLowerCase()) ?? null}
                disabled={disabled}
                isClientActive={kiroActiveId === a.id}
                refreshing={working}
                onOpen={() => setSelectedId(a.id)}
                onToggle={(on) => void run(() => spec.api.setEnabled(a.id, on))}
                onUse={() => void run(() => spec.api.setCurrent(labelOf(a, spec.labelPrefix)))}
                onRelogin={() => setAdding(true)}
                onRemove={() => void remove(a)}
                onRefresh={
                  spec.api.refreshQuota ? () => run(() => spec.api.refreshQuota!(a.id)) : undefined
                }
                onNote={
                  spec.api.setNote
                    ? (note) => void run(() => spec.api.setNote!(a.id, note))
                    : undefined
                }
              />
            ))}
          </div>
        </>
      ) : spec.id === "zcode" ? (
        <>
          {filterBar}
          <div className="accts">
            {filteredList.map((a) => (
              <ZcodeAccountCard
                key={a.id}
                account={a}
                prefix={spec.labelPrefix}
                lane={laneByLabel.get(labelOf(a, spec.labelPrefix).toLowerCase()) ?? null}
                disabled={disabled}
                onOpen={() => setSelectedId(a.id)}
                onToggle={(on) => void run(() => spec.api.setEnabled(a.id, on))}
                onUse={() => void run(() => spec.api.setCurrent(labelOf(a, spec.labelPrefix)))}
                onRemove={() => void remove(a)}
                onNote={
                  spec.api.setNote
                    ? (note) => void run(() => spec.api.setNote!(a.id, note))
                    : undefined
                }
              />
            ))}
          </div>
        </>
      ) : spec.id === "qoder" ? (
        <>
          {filterBar}
          <div className="accts">
            {filteredList.map((a) => (
              <QoderAccountCard
                key={a.id}
                account={a}
                lane={laneByLabel.get(labelOf(a, spec.labelPrefix).toLowerCase()) ?? null}
                disabled={disabled}
                refreshing={working}
                onOpen={() => setSelectedId(a.id)}
                onToggle={(on) => void run(() => spec.api.setEnabled(a.id, on))}
                onRelogin={() => setAdding(true)}
                onRemove={() => void remove(a)}
                onRefresh={
                  spec.api.refreshQuota ? () => run(() => spec.api.refreshQuota!(a.id)) : undefined
                }
                onNote={
                  spec.api.setNote ? (note) => void run(() => spec.api.setNote!(a.id, note)) : undefined
                }
              />
            ))}
          </div>
        </>
      ) : spec.id === "grok" || spec.id === "claude" ? (
        <>
          {filterBar}
          <div className="accts">
            {filteredList.map((a) =>
              spec.id === "grok" ? (
                <GrokAccountCard
                  key={a.id}
                  account={a}
                  prefix={spec.labelPrefix}
                  lane={laneByLabel.get(labelOf(a, spec.labelPrefix).toLowerCase()) ?? null}
                  refreshing={working}
                  onOpen={() => setSelectedId(a.id)}
                  onRelogin={() => setAdding(true)}
                  onRefresh={spec.api.refreshQuota ? () => run(() => spec.api.refreshQuota!(a.id)) : undefined}
                />
              ) : (
                <ClaudeAccountCard
                  key={a.id}
                  account={a}
                  prefix={spec.labelPrefix}
                  lane={laneByLabel.get(labelOf(a, spec.labelPrefix).toLowerCase()) ?? null}
                  refreshing={working}
                  onOpen={() => setSelectedId(a.id)}
                  onRelogin={() => setAdding(true)}
                  onRefresh={spec.api.refreshQuota ? () => run(() => spec.api.refreshQuota!(a.id)) : undefined}
                />
              ),
            )}
          </div>
        </>
      ) : (
        <div className="card">
          {filterBar}
          <div className="list">
            {filteredList.map((a) => (
              <AccountRow
                key={a.id}
                account={a}
                prefix={spec.labelPrefix}
                lane={laneByLabel.get(labelOf(a, spec.labelPrefix).toLowerCase()) ?? null}
                disabled={disabled}
                onToggle={(on) => void run(() => spec.api.setEnabled(a.id, on))}
                onUse={() => void run(() => spec.api.setCurrent(labelOf(a, spec.labelPrefix)))}
                onRelogin={() => setAdding(true)}
                onRemove={() => void remove(a)}
                onRefreshQuota={spec.api.refreshQuota ? () => void run(() => spec.api.refreshQuota!(a.id)) : undefined}
                onMediaOverride={
                  spec.api.setMediaOverride ? (v) => void run(() => spec.api.setMediaOverride!(a.id, v)) : undefined
                }
              />
            ))}
          </div>
        </div>
      )}

      {selectedAccount && spec.id === "kiro" ? (
        <KiroDrawer
          account={selectedAccount}
          lane={laneByLabel.get(labelOf(selectedAccount, spec.labelPrefix).toLowerCase()) ?? null}
          isClientActive={kiroActiveId === selectedAccount.id}
          onSwitchedClient={() => void reload()}
          onClose={() => setSelectedId(null)}
          onRefresh={async () => {
            if (spec.api.refreshQuota) {
              await run(() => spec.api.refreshQuota!(selectedAccount.id));
            }
          }}
          onRelogin={() => {
            setSelectedId(null);
            setAdding(true);
          }}
          onRemove={async () => {
            await remove(selectedAccount);
          }}
          onToggle={async (on) => {
            await run(() => spec.api.setEnabled(selectedAccount.id, on));
          }}
          onUse={async () => {
            await run(() => spec.api.setCurrent(labelOf(selectedAccount, spec.labelPrefix)));
          }}
          onNote={async (note) => {
            if (spec.api.setNote) {
              await run(() => spec.api.setNote!(selectedAccount.id, note));
            }
          }}
        />
      ) : null}

      {selectedAccount && spec.id === "qoder" ? (
        <QoderDrawer
          account={selectedAccount}
          lane={laneByLabel.get(labelOf(selectedAccount, spec.labelPrefix).toLowerCase()) ?? null}
          onClose={() => setSelectedId(null)}
          onRefresh={async () => {
            if (spec.api.refreshQuota) {
              await run(() => spec.api.refreshQuota!(selectedAccount.id));
            }
          }}
          onRelogin={() => {
            setSelectedId(null);
            setAdding(true);
          }}
          onRemove={async () => {
            await remove(selectedAccount);
          }}
          onToggle={async (on) => {
            await run(() => spec.api.setEnabled(selectedAccount.id, on));
          }}
          onUse={async () => {
            await run(() => spec.api.setCurrent(labelOf(selectedAccount, spec.labelPrefix)));
          }}
          onNote={async (note) => {
            if (spec.api.setNote) {
              await run(() => spec.api.setNote!(selectedAccount.id, note));
            }
          }}
        />
      ) : null}

      {selectedAccount && (spec.id === "grok" || spec.id === "claude") ? (
        spec.id === "grok" ? (
          <GrokDrawer
            account={selectedAccount}
            lane={laneByLabel.get(labelOf(selectedAccount, spec.labelPrefix).toLowerCase()) ?? null}
            onClose={() => setSelectedId(null)}
            onRefresh={async () => {
              if (spec.api.refreshQuota) await run(() => spec.api.refreshQuota!(selectedAccount.id));
            }}
            onRelogin={() => {
              setSelectedId(null);
              setAdding(true);
            }}
            onRemove={async () => {
              await remove(selectedAccount);
            }}
            onToggle={async (on) => {
              await run(() => spec.api.setEnabled(selectedAccount.id, on));
            }}
            onUse={async () => {
              await run(() => spec.api.setCurrent(labelOf(selectedAccount, spec.labelPrefix)));
            }}
            onNote={async (note) => {
              if (spec.api.setNote) await run(() => spec.api.setNote!(selectedAccount.id, note));
            }}
          />
        ) : (
          <ClaudeDrawer
            account={selectedAccount}
            lane={laneByLabel.get(labelOf(selectedAccount, spec.labelPrefix).toLowerCase()) ?? null}
            onClose={() => setSelectedId(null)}
            onRefresh={async () => {
              if (spec.api.refreshQuota) await run(() => spec.api.refreshQuota!(selectedAccount.id));
            }}
            onRelogin={() => {
              setSelectedId(null);
              setAdding(true);
            }}
            onRemove={async () => {
              await remove(selectedAccount);
            }}
            onToggle={async (on) => {
              await run(() => spec.api.setEnabled(selectedAccount.id, on));
            }}
            onUse={async () => {
              await run(() => spec.api.setCurrent(labelOf(selectedAccount, spec.labelPrefix)));
            }}
            onNote={
              spec.api.setNote
                ? async (note) => {
                    await run(() => spec.api.setNote!(selectedAccount.id, note));
                  }
                : undefined
            }
          />
        )
      ) : null}

      {selectedAccount && spec.id === "zcode" ? (
        <ZcodeDrawer
          account={selectedAccount}
          lane={laneByLabel.get(labelOf(selectedAccount, spec.labelPrefix).toLowerCase()) ?? null}
          onClose={() => setSelectedId(null)}
          onRemove={async () => {
            await remove(selectedAccount);
          }}
          onToggle={async (on) => {
            await run(() => spec.api.setEnabled(selectedAccount.id, on));
          }}
          onUse={async () => {
            await run(() => spec.api.setCurrent(labelOf(selectedAccount, spec.labelPrefix)));
          }}
          onNote={async (note) => {
            if (spec.api.setNote) {
              await run(() => spec.api.setNote!(selectedAccount.id, note));
            }
          }}
        />
      ) : null}

      {adding ? (
        <AddModal
          spec={spec}
          onClose={() => setAdding(false)}
          onAdded={async () => {
            setAdding(false);
            await reload();
          }}
        />
      ) : null}
    </>
  );
}

function laneTag(c: GatewayCandidate | null, enabled: boolean) {
  const b = laneBadge(c, enabled);
  if (!b) return null;
  return b.tone === "default" ? <Tag>{b.text}</Tag> : <Tag tone={b.tone}>{b.text}</Tag>;
}

function authMethodTag(method: string | null | undefined) {
  if (!method) return null;
  const m = method.toLowerCase();
  if (m.includes("google")) return <Tag>Google</Tag>;
  if (m.includes("github")) return <Tag>GitHub</Tag>;
  if (m.includes("social")) return <Tag>社交登录</Tag>;
  if (m.includes("builder") || m.includes("idc") || m.includes("sso")) return <Tag>Builder ID</Tag>;
  return <Tag>{method}</Tag>;
}

/** 媒体资格的徽章：三态（能 / 不能 / 没探过）；手动覆盖过的标出来。 */
function mediaTag(a: DeviceAccount) {
  if (a.mediaEligible === undefined) return null;
  const manual = a.mediaOverride != null ? "（手动）" : "";
  if (a.mediaEligible === true) return <Tag tone="ok">可出媒体{manual}</Tag>;
  if (a.mediaEligible === false) return <Tag tone="warn">不出媒体{manual}</Tag>;
  return <Tag>媒体资格未知</Tag>;
}

function AccountRow({
  account: a,
  prefix,
  lane,
  disabled,
  onToggle,
  onUse,
  onRelogin,
  onRemove,
  onRefreshQuota,
  onMediaOverride,
}: {
  account: DeviceAccount;
  prefix: string;
  lane: GatewayCandidate | null;
  disabled: boolean;
  onToggle: (on: boolean) => void;
  onUse: () => void;
  onRelogin: () => void;
  onRemove: () => void;
  onRefreshQuota?: () => void;
  onMediaOverride?: (value: boolean | null) => void;
}) {
  const isCurrent = lane?.state.kind === "current";
  const needsLogin = a.status !== "active" || !deviceCanServe(a);
  const isApiKey = a.authKind === "api_key";
  const quota = a.usage ?? null;
  const tier = a.subscriptionTier ?? quota?.subscriptionTier ?? a.planType;
  return (
    <div className={isCurrent ? "list-row is-current" : "list-row"} style={{ alignItems: "flex-start" }}>
      <div className="grow stack" style={{ gap: 8, minWidth: 0 }}>
        <div className="row" style={{ gap: 8, minWidth: 0, flexWrap: "wrap" }}>
          <span className="mono selectable truncate" style={{ fontSize: 13 }}>
            {labelOf(a, prefix)}
          </span>
          {isApiKey ? <Tag>API Key</Tag> : tier ? <span className="plan">{tier}</span> : null}
          {authMethodTag(a.authMethod)}
          {mediaTag(a)}
          {laneTag(lane, a.enabled)}
          {a.status === "dead" ? (
            <Tag tone="bad">已停用</Tag>
          ) : needsLogin ? (
            <Tag tone="warn">{isApiKey ? "Key 不在了" : "需要重新授权"}</Tag>
          ) : null}
        </div>

        {quota && !isApiKey ? (
          <div className="row" style={{ gap: 14, alignItems: "flex-start" }}>
            <div style={{ flex: 1, minWidth: 0 }}>
              <Gauge
                label={quota.periodType === "monthly" ? "本月额度" : quota.periodType === "five_hour" ? "5 小时额度" : "本周额度"}
                percent={quota.creditUsagePercent}
                compact
                note={
                  quota.periodEnd && quota.creditUsagePercent != null && quota.creditUsagePercent >= 99.5 ? (
                    <span className="acct-problem">{resetInShort(Date.parse(quota.periodEnd))}</span>
                  ) : quota.periodEnd ? (
                    <span className="faint tiny">{resetInShort(Date.parse(quota.periodEnd))}</span>
                  ) : undefined
                }
              />
            </div>
            {quota.remainingRequests != null || quota.remainingTokens != null ? (
              <span className="faint tiny" style={{ alignSelf: "center" }}>
                限流窗口剩 {quota.remainingRequests ?? "—"} 次 / {quota.remainingTokens ?? "—"} token
              </span>
            ) : null}
          </div>
        ) : null}

        <div className="row" style={{ gap: 10, flexWrap: "wrap" }}>
          {a.lastError ? (
            <span className="acct-problem" title={a.lastError}>
              {a.lastError.slice(0, 90)}
            </span>
          ) : null}
          {a.accessExpiresAt && !needsLogin && !isApiKey ? (
            <span className="faint tiny">凭证 {timeUntil(new Date(a.accessExpiresAt).getTime())} 后自动续期</span>
          ) : null}
          {quota ? <span className="faint tiny">额度 {timeAgo(quota.checkedAt)} 更新</span> : null}
          {a.note ? <span className="faint tiny">{a.note}</span> : null}
        </div>
      </div>
      <div className="row" style={{ gap: 6, flexShrink: 0 }}>
        <span className="acct-hover row" style={{ gap: 2 }}>
          {onRefreshQuota && !isApiKey ? (
            <button type="button" className="btn btn-sm btn-icon btn-quiet" disabled={disabled || needsLogin} onClick={onRefreshQuota} title="现在查一次额度 / 档位" aria-label="查额度">
              <Icon name="refresh" size={13} />
            </button>
          ) : null}
          {onMediaOverride ? (
            <button
              type="button"
              className="btn btn-sm btn-icon btn-quiet"
              disabled={disabled}
              onClick={() => onMediaOverride(a.mediaOverride == null ? a.mediaEligible !== true : null)}
              title={
                a.mediaOverride == null
                  ? a.mediaEligible === true
                    ? "手动关掉这个号的媒体（生图 / 生视频）"
                    : "手动放行这个号的媒体（生图 / 生视频）"
                  : "取消手动覆盖，回到自动探测"
              }
              aria-label="媒体资格"
            >
              <Icon name="image" size={13} />
            </button>
          ) : null}
          <button type="button" className="btn btn-sm btn-icon btn-soft btn-danger" disabled={disabled} onClick={onRemove} title="删除账号（连凭证）" aria-label="删除">
            <Icon name="trash" size={13} />
          </button>
        </span>
        {needsLogin ? (
          <button type="button" className="btn btn-sm" disabled={disabled} onClick={onRelogin}>
            重新授权
          </button>
        ) : (
          <button type="button" className="btn btn-sm" disabled={disabled || isCurrent || !a.enabled} onClick={onUse}>
            {isCurrent ? "使用中" : "用这个"}
          </button>
        )}
        <Switch checked={a.enabled} disabled={disabled} label={a.enabled ? "暂停这个号" : "加入网关"} onChange={onToggle} />
      </div>
    </div>
  );
}

type Mode = "oauth" | "cli" | "paste" | "apikey";

export function AddModal({ spec, onClose, onAdded }: { spec: Spec; onClose: () => void; onAdded: () => Promise<void> }) {
  const canOauth = (!!spec.api.loginStart && !!spec.listen) || !!spec.prepareOauth;
  const canCli = !!spec.api.importLocal;
  const [mode, setMode] = useState<Mode>(canOauth ? "oauth" : canCli ? "cli" : "paste");
  const [error, setError] = useState<unknown>(null);
  const [working, setWorking] = useState(false);
  const [handle, setHandle] = useState<DeviceLoginHandle | null>(null);
  const [waitSecs, setWaitSecs] = useState(0);
  const [paste, setPaste] = useState("");
  const [callback, setCallback] = useState("");
  const [pkceUrl, setPkceUrl] = useState<string | null>(null);
  const [pkceListening, setPkceListening] = useState(false);
  const [pkceId, setPkceId] = useState<string | null>(null);
  const [oauthProvider, setOauthProvider] = useState<string>(spec.oauthProviders?.[0]?.id ?? "Google");
  const [backend, setBackend] = useState(spec.pasteBackends?.[0]?.id ?? "global");
  const [apiKey, setApiKey] = useState("");
  const [note, setNote] = useState("");
  const [probe, setProbe] = useState<{ present: boolean; path: string; also?: string | null } | null>(null);
  const handleRef = useRef<DeviceLoginHandle | null>(null);
  handleRef.current = handle;
  const pkceRef = useRef<string | null>(null);
  pkceRef.current = pkceUrl;
  const pkceIdRef = useRef<string | null>(null);
  pkceIdRef.current = pkceId;
  const onAddedRef = useRef(onAdded);
  onAddedRef.current = onAdded;

  useEffect(() => {
    return () => {
      if (spec.id === "kiro" && pkceIdRef.current) {
        void kiro.socialCancel(pkceIdRef.current).catch(() => {});
      } else if (pkceRef.current) {
        void spec.api.loginCancel?.("");
      }
      const h = handleRef.current;
      if (h) void spec.api.loginCancel?.(h.sessionId);
    };
  }, [spec.api, spec.id]);

  useEffect(() => {
    const p = spec.probeLocal;
    if (!p) return;
    let alive = true;
    void p()
      .then((r) => {
        if (alive) setProbe(r);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [spec]);

  useEffect(() => {
    if (!pkceListening || !pkceId) return;
    let unlisten: (() => void) | null = null;
    const listenPkce = spec.listenPkce ?? onClaudeLogin;
    void listenPkce((st: any) => {
      if (st.sessionId !== pkceId) return;
      if (st.state === "waiting") setWaitSecs(st.elapsedSecs);
      if (st.state === "succeeded") {
        pkceRef.current = null;
        setWorking(true);
        void onAddedRef.current();
      }
      if (st.state === "failed") {
        setError({ code: "upstream", message: st.message, hint: st.hint ?? undefined });
        setPkceUrl(null);
        setPkceListening(false);
      }
      if (st.state === "cancelled") {
        setPkceUrl(null);
        setPkceListening(false);
      }
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      unlisten?.();
    };
  }, [pkceId, pkceListening, spec.listenPkce]);

  useEffect(() => {
    const listen = spec.listen;
    if (!listen) return;
    let unlisten: (() => void) | null = null;
    void listen((st: DeviceLoginState) => {
      const h = handleRef.current;
      if (h && st.sessionId === h.sessionId) {
        if (st.state === "waiting") setWaitSecs(st.elapsedSecs);
        if (st.state === "succeeded") {
          handleRef.current = null;
          void onAddedRef.current();
        }
        if (st.state === "failed") {
          setError({ code: "upstream", message: st.message, hint: st.hint ?? undefined });
          setHandle(null);
        }
        if (st.state === "cancelled") setHandle(null);
      } else if (pkceRef.current && (st.sessionId === pkceIdRef.current || st.sessionId === "")) {
        if (st.state === "succeeded") {
          pkceRef.current = null;
          setWorking(true);
          void onAddedRef.current();
        }
        if (st.state === "failed") {
          setError({ code: "upstream", message: st.message, hint: st.hint ?? undefined });
          setWorking(false);
        }
      }
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      unlisten?.();
    };
  }, [spec.listen]);

  async function go(action: () => Promise<unknown>) {
    setWorking(true);
    setError(null);
    try {
      await action();
    } catch (e) {
      setError(e);
    } finally {
      setWorking(false);
    }
  }

  const code = handle?.userCode ?? "";

  return (
    <Modal
      title={`添加 ${spec.title} 账号`}
      subtitle={spec.addSubtitle}
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn" onClick={onClose}>
            {handle || pkceUrl ? "关闭" : "取消"}
          </button>
          {mode === "oauth" && !handle && spec.id === "kiro" && oauthProvider !== "builder-id" ? (
            pkceUrl ? (
              <button
                type="button"
                className="btn btn-primary"
                disabled={working || !callback.trim()}
                onClick={() =>
                  void go(async () => {
                    await kiro.socialComplete(pkceId ?? "", callback);
                    pkceRef.current = null;
                    await onAdded();
                  })
                }
              >
                {working ? <Spinner /> : null}
                {working ? "正在完成授权…" : "完成授权"}
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-primary"
                disabled={working}
                onClick={() =>
                  void go(async () => {
                    const started = await kiro.socialStart(oauthProvider, note.trim() || undefined);
                    setPkceUrl(started.authorizeUrl);
                    setPkceId(started.sessionId);
                    setPkceListening(false);
                  })
                }
              >
                {working ? <Spinner /> : <Icon name="external" size={13} />}
                {working ? "正在启动授权…" : "打开浏览器授权"}
              </button>
            )
          ) : null}
          {mode === "oauth" && !handle && spec.prepareOauth ? (
            pkceUrl && !pkceListening ? (
              <button
                type="button"
                className="btn btn-primary"
                disabled={working || !callback.trim()}
                onClick={() =>
                  void go(async () => {
                    await spec.api.importText(callback, note.trim() || undefined);
                    pkceRef.current = null;
                    await onAdded();
                  })
                }
              >
                {working ? <Spinner /> : null}
                {working ? "正在完成授权…" : "完成授权"}
              </button>
            ) : pkceUrl ? null : (
              <button
                type="button"
                className="btn btn-primary"
                disabled={working}
                onClick={() =>
                  void go(async () => {
                    const started = await spec.prepareOauth!(note.trim() || undefined);
                    setPkceUrl(started.authorizeUrl);
                    setPkceId(started.loginId ?? null);
                    setPkceListening(!!started.callbackListening);
                    await openUrl(started.authorizeUrl);
                  })
                }
              >
                {working ? <Spinner /> : <Icon name="external" size={13} />}
                {working ? "正在启动授权…" : "打开浏览器授权"}
              </button>
            )
          ) : null}
          {mode === "oauth" && !handle && !spec.prepareOauth && (spec.id !== "kiro" || oauthProvider === "builder-id") && spec.api.loginStart ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={working}
              onClick={() => void go(async () => setHandle(await spec.api.loginStart!(note.trim() || undefined)))}
            >
              {working ? <Spinner /> : <Icon name="external" size={13} />}
              {working ? "正在启动授权…" : "打开浏览器授权"}
            </button>
          ) : null}
          {mode === "cli" && spec.api.importLocal ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={working || (probe != null && !probe.present && !probe.also)}
              onClick={() =>
                void go(async () => {
                  await spec.api.importLocal!();
                  await onAdded();
                })
              }
            >
              {working ? <Spinner /> : <Icon name="download" size={13} />}
              {working ? "正在导入并同步用量…" : spec.cliTab.replace("从本机 ", "从本机导入")}
            </button>
          ) : null}
          {mode === "paste" ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={working || !paste.trim()}
              onClick={() =>
                void go(async () => {
                  await spec.api.importText(paste, note.trim() || undefined, spec.pasteBackends ? backend : undefined);
                  await onAdded();
                })
              }
            >
              {working ? <Spinner /> : null}
              {working ? "正在校验导入…" : "导入"}
            </button>
          ) : null}
          {mode === "apikey" && spec.api.addApiKey ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={working || !apiKey.trim().startsWith("xai-")}
              onClick={() =>
                void go(async () => {
                  await spec.api.addApiKey!(apiKey, note.trim() || undefined);
                  await onAdded();
                })
              }
            >
              添加 API Key
            </button>
          ) : null}
        </>
      }
    >
      <div className="stack" style={{ gap: 12 }}>
        <div className="gwset-options" role="radiogroup" aria-label="添加方式">
          {(
            [
              ...(canOauth ? [["oauth", "授权登录"] as [Mode, string]] : []),
              ...(canCli ? [["cli", spec.cliTab] as [Mode, string]] : []),
              ["paste", "粘贴 token"],
              ...(spec.apiKeyHint && spec.api.addApiKey ? [["apikey", "API Key"] as [Mode, string]] : []),
            ] as Array<[Mode, string]>
          ).map(([id, label]) => (
            <button
              key={id}
              type="button"
              role="radio"
              aria-checked={mode === id}
              className={`gwset-option${mode === id ? " is-active" : ""}`}
              disabled={working || !!handle || !!pkceUrl}
              onClick={() => setMode(id)}
            >
              {label}
            </button>
          ))}
        </div>

        <ErrorNote error={error} />

        {mode === "oauth" && spec.prepareOauth ? (
          <div className="stack" style={{ gap: 10 }}>
            {!pkceUrl ? (
              <p className="muted" style={{ margin: 0 }}>
                {spec.oauthHint}
              </p>
            ) : null}
            {pkceUrl ? (
              pkceListening ? (
                <>
                  <div className="row" style={{ gap: 8, alignItems: "center" }}>
                    <Spinner />
                    <p className="muted" style={{ margin: 0 }}>
                      {spec.waitingHint}
                      {waitSecs > 0 ? <span className="faint tiny">（已等待 {waitSecs} 秒）</span> : null}
                    </p>
                  </div>
                  <div className="row" style={{ gap: 6 }}>
                    <span className="faint tiny">没自动打开？</span>
                    <CopyButton value={pkceUrl} label="复制授权链接" />
                  </div>
                </>
              ) : (
              <>
                <p className="muted" style={{ margin: 0 }}>
                  本机 54545 端口被占着，没法自动收回调。浏览器跳到打不开的 localhost:54545 之后，把地址栏整条复制过来。
                </p>
                <textarea
                  className="textarea mono"
                  rows={3}
                  placeholder="http://localhost:54545/callback?code=…&state=…"
                  value={callback}
                  onChange={(e) => setCallback(e.target.value)}
                  spellCheck={false}
                />
                <div className="row" style={{ gap: 6 }}>
                  <span className="faint tiny">没自动打开？</span>
                  <CopyButton value={pkceUrl} label="复制授权链接" />
                </div>
              </>
              )
            ) : null}
            {pkceUrl ? null : (
              <input className="input" placeholder="备注（可选）" value={note} onChange={(e) => setNote(e.target.value)} />
            )}
          </div>
        ) : mode === "oauth" ? (
          handle ? (
            <div className="stack" style={{ gap: 12 }}>
              <p className="muted" style={{ margin: 0 }}>
                {spec.waitingHint}
                {waitSecs > 0 ? <span className="faint tiny">（已等待 {waitSecs} 秒）</span> : null}
              </p>
              <div className="row" style={{ gap: 10, alignItems: "center", flexWrap: "wrap" }}>
                <code className="mono selectable" style={{ fontSize: 22, letterSpacing: "0.18em", fontWeight: 600 }}>
                  {code}
                </code>
                <CopyButton value={code} label="复制验证码" />
              </div>
              <div className="row" style={{ gap: 6 }}>
                <span className="faint tiny">没自动打开？</span>
                <CopyButton value={handle.authorizeUrl} label="复制授权链接" />
              </div>
            </div>
          ) : pkceUrl ? (
            <div className="stack" style={{ gap: 12 }}>
              <div className="row" style={{ gap: 10, alignItems: "center" }}>
                <Spinner />
                <div>
                  <p style={{ margin: 0, fontWeight: 550, fontSize: 13 }}>
                    正在等待浏览器完成 {oauthProvider === "Github" ? "GitHub" : "Google"} 授权…
                  </p>
                  <p className="faint tiny" style={{ margin: "2px 0 0" }}>
                    授权成功后浏览器提示“打开 Nexus”时点击确认，系统将全自动完成绑定。
                  </p>
                </div>
              </div>
              <div
                style={{
                  padding: "9px 12px",
                  background: "var(--color-canvas)",
                  border: "1px solid var(--color-line)",
                  borderRadius: 6,
                }}
              >
                <p className="muted tiny" style={{ margin: 0, lineHeight: 1.5 }}>
                  <strong>自动授权：</strong>浏览器登录后若弹出“要打开 Nexus 吗？”，请点击<strong>「打开 Nexus」</strong>。<br />
                  <strong>备用方式：</strong>若未自动打开，可复制浏览器跳转后地址栏中的链接粘贴到下方：
                </p>
              </div>
              <textarea
                className="textarea mono"
                rows={2}
                placeholder="kiro://kiro.kiroAgent/authenticate-success?code=…&state=…"
                value={callback}
                onChange={(e) => setCallback(e.target.value)}
                spellCheck={false}
              />
              <div className="row" style={{ gap: 10, alignItems: "center", justifyContent: "space-between" }}>
                <div className="row" style={{ gap: 6 }}>
                  <span className="faint tiny">没自动打开浏览器？</span>
                  <CopyButton value={pkceUrl} label="复制授权链接" />
                </div>
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={async () => {
                    try {
                      const text = await navigator.clipboard.readText();
                      if (text) setCallback(text.trim());
                    } catch {}
                  }}
                >
                  <Icon name="copy" size={12} />
                  从剪贴板粘贴
                </button>
              </div>
            </div>
          ) : (
            <div className="stack" style={{ gap: 10 }}>
              {spec.oauthProviders ? (
                <div className="stack" style={{ gap: 6 }}>
                  <label className="muted tiny" style={{ fontWeight: 500 }}>
                    授权渠道
                  </label>
                  <div className="gwset-options" role="radiogroup" aria-label="授权渠道">
                    {spec.oauthProviders.map((p) => (
                      <button
                        key={p.id}
                        type="button"
                        role="radio"
                        aria-checked={oauthProvider === p.id}
                        className={`gwset-option${oauthProvider === p.id ? " is-active" : ""}`}
                        disabled={working}
                        onClick={() => setOauthProvider(p.id)}
                      >
                        {p.label}
                      </button>
                    ))}
                  </div>
                  {spec.oauthProviders.find((p) => p.id === oauthProvider)?.desc ? (
                    <span className="faint tiny">
                      {spec.oauthProviders.find((p) => p.id === oauthProvider)?.desc}
                    </span>
                  ) : null}
                </div>
              ) : null}
              <p className="muted" style={{ margin: 0 }}>
                {spec.oauthHint}
              </p>
              <input className="input" placeholder="备注（可选）" value={note} onChange={(e) => setNote(e.target.value)} />
            </div>
          )
        ) : null}

        {mode === "cli" ? (
          <div className="stack" style={{ gap: 10 }}>
            <p className="muted" style={{ margin: 0 }}>
              {spec.cliHint}
            </p>
            {probe ? (
              probe.present ? (
                <Banner tone="ok" title="找到本机的凭证了，可以直接导入。" hint={probe.path} />
              ) : probe.also ? (
                <Banner
                  tone="warn"
                  title="凭证文件不在，导入时会再读钥匙串。"
                  hint={`${probe.path}；${probe.also}`}
                />
              ) : (
                <Banner
                  tone="warn"
                  title="这台电脑上没找到凭证文件。"
                  hint={`先装官方客户端并登录一次，或者改用「粘贴」。找的位置：${probe.path}`}
                />
              )
            ) : null}
          </div>
        ) : null}

        {mode === "paste" ? (
          <div className="stack" style={{ gap: 10 }}>
            {spec.pasteBackends ? (
              <div className="gwset-options" role="radiogroup" aria-label={`${spec.title} 版本`}>
                {spec.pasteBackends.map((b) => (
                  <button
                    key={b.id}
                    type="button"
                    role="radio"
                    aria-checked={backend === b.id}
                    className={`gwset-option${backend === b.id ? " is-active" : ""}`}
                    disabled={working}
                    onClick={() => setBackend(b.id)}
                  >
                    {b.label}
                  </button>
                ))}
              </div>
            ) : null}
            <textarea
              className="textarea mono"
              rows={5}
              placeholder={spec.pastePlaceholder}
              value={paste}
              onChange={(e) => setPaste(e.target.value)}
              spellCheck={false}
            />
            <input className="input" placeholder="备注（可选）" value={note} onChange={(e) => setNote(e.target.value)} />
          </div>
        ) : null}

        {mode === "apikey" ? (
          <div className="stack" style={{ gap: 10 }}>
            <p className="muted" style={{ margin: 0 }}>
              {spec.apiKeyHint}
            </p>
            <input
              className="input mono"
              type="password"
              placeholder="xai-…"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              spellCheck={false}
              autoComplete="off"
            />
            <input className="input" placeholder="备注（可选）" value={note} onChange={(e) => setNote(e.target.value)} />
          </div>
        ) : null}
      </div>
    </Modal>
  );
}
