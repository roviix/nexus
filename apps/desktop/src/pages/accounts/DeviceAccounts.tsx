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
import { channelOf, laneOf } from "../../gateway/channels";
import { gateway, grok, kiro, onGrokLogin, onKiroLogin, zcode } from "../../ipc/api";
import type {
  GatewayChannelId,
  DeviceAccount,
  DeviceLoginHandle,
  DeviceLoginState,
  GatewayCandidate,
  GatewayStatus,
} from "../../ipc/types";
import { go, type Route } from "../../shell/nav";
import { confirm } from "../../ui/confirm";
import { timeAgo, timeUntil } from "../../ui/format";
import { resetInShort } from "../../ui/usage";
import { Banner, CopyButton, Empty, ErrorNote, Gauge, Icon, Modal, Switch, Tag } from "../../ui/primitives";
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
  importLocal: () => Promise<unknown>;
  importText: (text: string, note?: string) => Promise<unknown>;
  remove: (id: string) => Promise<void>;
  setEnabled: (id: string, enabled: boolean) => Promise<DeviceAccount>;
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
  /** 「API Key」页签的提示；没有就不出这个页签。 */
  apiKeyHint?: ReactNode;
  labelPrefix: string;
  api: DeviceApi;
  /** 设备码授权的进度事件。和 `api.loginStart` 同进同退。 */
  listen?: (cb: (s: DeviceLoginState) => void) => Promise<() => void>;
  /**
   * 本机那个客户端在不在。给了就在「从本机导入」页签上先说清楚，
   * 而不是让用户点一下再吃一个「读不到文件」。
   */
  probeLocal?: () => Promise<{ present: boolean; path: string }>;
}

const GROK: Spec = {
  id: "grok",
  title: "Grok Build",
  emptyTitle: "还没有 Grok 账号",
  emptyBody:
    "xAI 的 Grok Build 订阅号，或一把 xAI API Key。加进来后，客户端把 grok-* / grok-imagine-* 指到本机网关就会走这条通道：对话、生图、生视频都在。凭证只在这台电脑上。这不是 Grok Bot。",
  bannerHint: "Grok 的号只有一个用途：给本机网关跑 grok-* 与 Imagine 媒体。开了网关，OpenCode / Grok CLI 指到它就能用。",
  usableHint: "个可接 · grok-* 与 grok-imagine-* 走这里",
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
    "AWS Builder ID / 社交登录的 Kiro 订阅。加进来后，kiro-claude-* 走这条通道。裸的 claude-* 不会进这里——那是 Cursor 的号。凭证只在这台电脑上。",
  bannerHint: "Kiro 的号只有一个用途：给本机网关跑 kiro-claude-*。开了网关再指过来。",
  usableHint: "个可接 · kiro-claude-* 走这里",
  addSubtitle: "AWS Builder ID 设备码。凭证只存在这台电脑上。",
  oauthHint: "浏览器会打开 AWS 的设备码页。把下面那串码输进去。只走 us-east-1；没有客户端对时走社交刷新。",
  waitingHint: "在打开的页面里输入这串码，用 AWS Builder ID 登录并同意。",
  cliTab: "从本机 Kiro IDE",
  cliHint: (
    <>
      读取 <code className="mono">~/.aws/sso/cache/kiro-auth-token.json</code>（Kiro IDE 留下的态）。文件不动，只把凭证收进 Nexus。
    </>
  ),
  pastePlaceholder:
    "三种写法都认：\n· kiro-auth-token.json 的原文\n· CLIProxyAPI 的 JSON\n· access_token----refresh_token",
  labelPrefix: "kiro",
  api: kiro,
  listen: onKiroLogin,
};

const ZCODE: Spec = {
  id: "zcode",
  title: "ZCode",
  emptyTitle: "还没有 ZCode 账号",
  emptyBody:
    "智谱 GLM 的编码套餐。在官方 ZCode 客户端里登录一次，这里就能把凭证收进来——加进来后，glm-* 走这条通道（zcode/glm-4.7 也认）。凭证只在这台电脑上。",
  bannerHint: "ZCode 的号只有一个用途：给本机网关跑 glm-*。开了网关，Claude Code / OpenCode 指到它就能用。",
  usableHint: "个可接 · glm-* 走这里",
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

/**
 * 这个号还能不能自己拿出凭证。
 *
 * OAuth 号看 refresh token，API Key 号看 key，ZCode 看永久 key / JWT——它没有
 * `hasRefresh` 那一位，因为压根没有续期这回事。
 */
function deviceCanServe(a: DeviceAccount): boolean {
  if (a.hasRefresh !== undefined) return a.hasRefresh;
  return (a.hasApiKey ?? false) || (a.hasJwt ?? false);
}

function DeviceAccounts({ spec, tabs, onGo }: { spec: Spec; tabs: ReactNode; onGo: (r: Route) => void }) {
  const [accounts, setAccounts] = useState<DeviceAccount[] | null>(null);
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [working, setWorking] = useState(false);
  const [adding, setAdding] = useState(false);

  const reload = useCallback(async () => {
    try {
      setAccounts(await spec.api.list());
      setError(null);
    } catch (e) {
      setError(e);
    }
    try {
      setStatus(await gateway.status());
    } catch {
      setStatus(null);
    }
  }, [spec.api]);

  useEffect(() => {
    void reload();
  }, [reload]);

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
  const channel = channelOf(status, spec.id);

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
    await run(() => spec.api.remove(a.id));
  }

  const list = accounts ?? [];
  const active = list.filter((a) => a.enabled && a.status === "active" && deviceCanServe(a)).length;
  const disabled = working;

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

      {list.length > 0 && status && !status.running ? (
        <div style={{ marginBottom: 12 }}>
          <Banner
            tone="default"
            title="本地网关没在跑，这些号此刻没人用。"
            hint={spec.bannerHint}
            action={
              <button type="button" className="btn btn-sm" onClick={() => onGo(go("gateway"))}>
                去本地网关
              </button>
            }
          />
        </div>
      ) : null}

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
      ) : (
        <div className="card">
          <div className="row" style={{ gap: 8, alignItems: "baseline", marginBottom: 12 }}>
            <strong>{list.length} 个号</strong>
            <span className="faint tiny">
              {active} {spec.usableHint}
            </span>
            {channel && channel.imageModels.length + channel.videoModels.length > 0 ? (
              <span className="faint tiny">· {channel.mediaReady ? "可出图 / 出视频" : "此刻没有号能出媒体"}</span>
            ) : null}
          </div>
          <div className="list">
            {list.map((a) => (
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
  if (m.includes("social") || m.includes("google") || m.includes("github")) return <Tag>社交登录</Tag>;
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
                label={quota.periodType === "monthly" ? "本月额度" : "本周额度"}
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

function AddModal({ spec, onClose, onAdded }: { spec: Spec; onClose: () => void; onAdded: () => Promise<void> }) {
  const canOauth = !!spec.api.loginStart && !!spec.listen;
  const [mode, setMode] = useState<Mode>(canOauth ? "oauth" : "cli");
  const [error, setError] = useState<unknown>(null);
  const [working, setWorking] = useState(false);
  const [handle, setHandle] = useState<DeviceLoginHandle | null>(null);
  const [waitSecs, setWaitSecs] = useState(0);
  const [paste, setPaste] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [note, setNote] = useState("");
  const [probe, setProbe] = useState<{ present: boolean; path: string } | null>(null);
  const handleRef = useRef<DeviceLoginHandle | null>(null);
  handleRef.current = handle;

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
    const listen = spec.listen;
    if (!listen) return;
    let unlisten: (() => void) | null = null;
    void listen((st: DeviceLoginState) => {
      const h = handleRef.current;
      if (!h || st.sessionId !== h.sessionId) return;
      if (st.state === "waiting") setWaitSecs(st.elapsedSecs);
      if (st.state === "succeeded") void onAdded();
      if (st.state === "failed") {
        setError({ code: "upstream", message: st.message, hint: st.hint ?? undefined });
        setHandle(null);
      }
      if (st.state === "cancelled") setHandle(null);
    }).then((u) => {
      unlisten = u;
    });
    return () => {
      unlisten?.();
      const h = handleRef.current;
      if (h) void spec.api.loginCancel?.(h.sessionId);
    };
  }, [onAdded, spec]);

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
            {handle ? "关闭" : "取消"}
          </button>
          {mode === "oauth" && !handle && spec.api.loginStart ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={working}
              onClick={() => void go(async () => setHandle(await spec.api.loginStart!(note.trim() || undefined)))}
            >
              <Icon name="external" size={13} />
              打开浏览器授权
            </button>
          ) : null}
          {mode === "cli" ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={working || probe?.present === false}
              onClick={() =>
                void go(async () => {
                  await spec.api.importLocal();
                  await onAdded();
                })
              }
            >
              <Icon name="download" size={13} />
              {spec.cliTab.replace("从本机 ", "从本机导入")}
            </button>
          ) : null}
          {mode === "paste" ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={working || !paste.trim()}
              onClick={() =>
                void go(async () => {
                  await spec.api.importText(paste, note.trim() || undefined);
                  await onAdded();
                })
              }
            >
              导入
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
              ["cli", spec.cliTab],
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
              disabled={working || !!handle}
              onClick={() => setMode(id)}
            >
              {label}
            </button>
          ))}
        </div>

        <ErrorNote error={error} />

        {mode === "oauth" ? (
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
          ) : (
            <div className="stack" style={{ gap: 10 }}>
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
