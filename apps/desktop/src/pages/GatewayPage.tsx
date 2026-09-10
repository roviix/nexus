/**
 * 本地网关 —— 中转 API 的本地那台引擎：在本机开一个 OpenAI / Anthropic 兼容口，背后用号池里的号。
 *
 * 这一页只有两块：
 *  1. 开没开（一个开关）。地址、端口、口令这些是接线用的字面量，默认收在「地址与口令」后面 ——
 *     客户端配置在「接入」页一键写入，用户本不该进来先看见 `127.0.0.1:8787`；
 *  2. 通道 —— 网关背后的几队号，**并列**摆：Cursor（默认通道，不带前缀的模型都落它）、
 *     ChatGPT、Grok Build、Kiro。每条一行，一句网关视角的结论（几个能接、谁在用）。
 *     Cursor 那一行点进去是它的号池（`#gateway/pool`，见下）；其余通道的号在「账号」对应页签里
 *     授权了就自动在队里，这里只指过去。
 *
 * 设置不在页面上。端口、额度通道、强制模型这些是引擎室里的阀门，一年拧不到一次，摊在
 * 主页面上只会让人一进来就觉得「这么一大坨」；收进右上角一个齿轮后面的弹窗。端口更是
 * 连弹窗里都退到「高级」折叠区：被占了会自动换一个空闲的并记住，用户本不该为它操心。
 *
 * **Cursor 号池是一个下钻页**，不再摊在通道行底下。以前点 Cursor 那行展开一格账号卡：一张卡里
 * 套一列卡、抽屉又从卡里弹出来，三层嵌套；号一多，那一格比页面上其它所有东西加起来还长，
 * 「通道」这张卡就名不副实了。拆出去之后网关页只回答「开没开、几条通道什么光景」，号池页有整个
 * 宽度摆卡、有自己的动作行（添加 / 重置状态），地址栏还能直达。两页共用这一个组件的状态：
 * 切过去不重新拉数据、抽屉和弹窗在两页都能开。
 *
 * Cursor 号池和切号页同构：同一副行骨架、同样「正在用的置顶」。但它是号池的一个**子集**，
 * 不自动等于全部：网关背后是 Claude Code、Codex 这类会自己跑很久的客户端，一个号被它悄悄
 * 用光、回到 IDE 才发现，比多点一次「添加」贵得多。所以进队只有一条路 —— 用户点「添加」。
 *
 * **怎么把客户端接进来不在这里** —— 那是「接入」页的事。
 * 口令不常驻在界面上：点「显示」才取，取过记活动日志（和账号凭证同一套规矩）。
 */
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { AccountCard } from "../accounts/AccountCard";
import { AccountInspector } from "../accounts/AccountInspector";
import { createCursorAccountView, type AccountView } from "../accounts/model";
import { channelSummary, laneCount, localChannels, modelsOf, starved as isStarved, type LocalChannel } from "../gateway/channels";
import { accounts, gateway } from "../ipc/api";
import { models as modelsApi, type LocalModel } from "../ipc/models";
import type { Account, GatewayAvailable, GatewayCandidate, GatewaySettings, GatewayStatus, MediaJob } from "../ipc/types";
import { go, type AccountPlatform, type Route } from "../shell/nav";
import { ShellIcon } from "../shell/ShellIcon";
import { timeAgo } from "../ui/format";
import { Banner, CopyButton, Empty, ErrorNote, Health, Icon, Modal, Opt, Switch, Tag } from "../ui/primitives";
import { accountProblem, planLabel, planTone } from "../ui/usage";

/** 额度通道。这个词比 `client-type` 说得清它是干什么的：上游按它决定从哪个池子扣额度。 */
const CLIENT_TYPES: Array<{ id: string; label: string; hint: string }> = [
  { id: "cli", label: "CLI", hint: "稳定默认" },
  { id: "ide", label: "IDE", hint: "IDE 流量" },
  { id: "sand", label: "Sand", hint: "Bot 周额度 · 谨慎" },
];

export function GatewayPage({ route, onGo }: { route: Route; onGo: (r: Route) => void }) {
  const inPool = route.sub === "pool";
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [key, setKey] = useState<string | null>(null);
  /** 「账号」里的号，只用来给候选号显示额度（与切号页同一种纯展示关联）。 */
  const [known, setKnown] = useState<Account[]>([]);
  /** 本地目录，只为在通道行上写「N 个模型」。 */
  const [local, setLocal] = useState<LocalModel[] | null>(null);
  const [adding, setAdding] = useState(false);
  const [configuring, setConfiguring] = useState(false);
  const [openLabel, setOpenLabel] = useState<string | null>(null);
  /** 地址 / 端口 / 口令那一块默认收着。 */
  const [wiringOpen, setWiringOpen] = useState(false);

  const reload = useCallback(async () => {
    try {
      setStatus(await gateway.status());
      setError(null);
    } catch (e) {
      setError(e);
    }
    const [k, l] = await Promise.allSettled([accounts.list(), modelsApi.local()]);
    setKnown(k.status === "fulfilled" ? k.value : []);
    setLocal(l.status === "fulfilled" ? l.value : null);
  }, []);

  const byEmail = useMemo(() => {
    const m = new Map<string, Account>();
    for (const a of known) m.set(a.email.toLowerCase(), a);
    return m;
  }, [known]);

  /** 正在用的置顶 —— 和切号页同一条规矩。其余保持网关给的接力顺序。 */
  const candidates = useMemo(() => {
    const list = status?.lane.candidates ?? [];
    return [...list].sort(
      (a, b) => Number(b.state.kind === "current") - Number(a.state.kind === "current"),
    );
  }, [status]);

  const missing = status?.lane.missing ?? [];
  const available = status?.lane.available ?? [];
  const enrolled = candidates.length + missing.length;
  const openCandidate = candidates.find((candidate) => candidate.label === openLabel) ?? null;
  const openMissing = openLabel != null && missing.includes(openLabel) ? openLabel : null;
  const openView = useMemo(() => {
    if (openCandidate) {
      return gatewayAccountView(
        openCandidate,
        byEmail.get(openCandidate.label.toLowerCase()) ?? null,
      );
    }
    if (openMissing) {
      return gatewayMissingView(openMissing, byEmail.get(openMissing) ?? null);
    }
    return null;
  }, [byEmail, openCandidate, openMissing]);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 开着的时候每 5 秒刷一次：接力 / 冷却状态会随请求变。
  useEffect(() => {
    if (!status?.running) return;
    const t = window.setInterval(() => void reload(), 5000);
    return () => window.clearInterval(t);
  }, [status?.running, reload]);

  async function run(action: () => Promise<GatewayStatus>) {
    setBusy(true);
    setError(null);
    try {
      setStatus(await action());
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  async function saveSettings(patch: {
    port?: number;
    passthroughPort?: number;
    clientType?: string;
    autostart?: boolean;
    forceModel?: string | null;
  }) {
    setError(null);
    try {
      await gateway.updateSettings(patch);
      await reload();
    } catch (e) {
      setError(e);
    }
  }

  /**
   * 移出名单。正在用的那个号要多问一句 —— 在途的对话会立刻换号，上游缓存跟着丢；
   * 其余的号移出没有副作用，不问。
   */
  async function unenroll(c: GatewayCandidate) {
    if (c.state.kind === "current") {
      const next = candidates.find((x) => x.state.kind === "ready");
      const then = next ? `下一个请求会换到 ${next.label}` : "名单里没有别的可用号了，网关会开始回 503";
      if (!window.confirm(`${c.label} 正在被网关使用。移出后${then}，正在进行的对话会丢上游缓存。继续？`)) return;
    }
    await run(() => gateway.unenroll(c.label));
  }

  async function revealKey() {
    try {
      setKey(await gateway.revealKey());
    } catch (e) {
      setError(e);
    }
  }

  async function rotateKey() {
    if (!window.confirm("换一把口令后，已经配了旧口令的客户端会立刻 401，要重新填。继续？")) return;
    try {
      setKey(await gateway.rotateKey());
      await reload();
    } catch (e) {
      setError(e);
    }
  }

  const running = !!status?.running;
  /** 开着，但所有通道里没有一个号能接请求（名单空，或号全都耗尽 / 到线 / 拿不到凭证）。 */
  const starved = Boolean(status && isStarved(status));

  /** 设置里有没有偏离默认的东西：有就在齿轮旁点一个点，让人知道「这台引擎调过」。 */
  const tuned = Boolean(status && (status.settings.clientType !== "cli" || status.settings.forceModel || status.settings.autostart));

  const channels = useMemo(() => localChannels(status, local), [status, local]);
  const currentLabel = candidates.find((c) => c.state.kind === "current")?.label ?? null;

  /** 号池页正文：候选 / 缺失的号，或者一个空态。 */
  const pool =
    enrolled === 0 ? (
      <Empty
        title="Cursor 号池为空"
        action={
          <button type="button" className="btn btn-primary btn-sm" onClick={() => setAdding(true)}>
            <Icon name="plus" size={13} />
            添加号
          </button>
        }
      >
        不带前缀的模型都走这里的号。加进来的号会被 Claude Code、Codex 这类客户端一直用到额度到线，再接力下一个。
      </Empty>
    ) : (
      <div className="accts">
        {candidates.map((c) => (
          <CandidateRow
            key={c.label}
            c={c}
            account={byEmail.get(c.label.toLowerCase()) ?? null}
            busy={busy}
            open={openLabel === c.label}
            onOpen={() => setOpenLabel(c.label)}
            onUse={() => void run(() => gateway.setCurrent(c.label))}
            onRemove={() => void unenroll(c)}
          />
        ))}
        {missing.map((email) => (
          <MissingRow
            key={email}
            email={email}
            account={byEmail.get(email) ?? null}
            busy={busy}
            open={openLabel === email}
            onOpen={() => setOpenLabel(email)}
            onRemove={() => void run(() => gateway.unenroll(email))}
            onFix={() => onGo(go("accounts"))}
          />
        ))}
      </div>
    );

  /** 号池页：面包屑回网关，右边是号池自己的动作。 */
  const poolHead = (
    <div className="page-head">
      <div className="page-title-line">
        <button type="button" className="btn btn-sm btn-quiet gw-crumb" onClick={() => onGo(go("gateway"))}>
          <Icon name="back" size={13} />
          本地网关
        </button>
        <span className="page-title-meta gw-crumb-here">
          Cursor 号池
          {status ? ` · ${enrolled} 个号` : ""}
          {currentLabel ? ` · 正在用 ${currentLabel}` : ""}
        </span>
      </div>
      <div className="row" style={{ gap: 8 }}>
        {enrolled > 0 ? (
          <button type="button" className="btn btn-sm btn-quiet" disabled={busy} onClick={() => void run(() => gateway.resetLane())} title="清掉耗尽 / 冷却记录，让所有号重新可选">
            重置状态
          </button>
        ) : null}
        <button type="button" className="btn btn-sm btn-icon btn-soft" onClick={() => void reload()} disabled={busy} title="刷新" aria-label="刷新">
          <Icon name="refresh" size={14} />
        </button>
        <button type="button" className="btn btn-sm btn-primary" disabled={busy || !status} onClick={() => setAdding(true)}>
          <Icon name="plus" size={13} />
          添加号
        </button>
      </div>
    </div>
  );

  const mainHead = (
    <div className="page-head">
      <div className="page-title-line">
        <h1>本地网关</h1>
        {status?.settings.autostart ? <span className="page-title-meta">随应用启动</span> : null}
      </div>
      <div className="row" style={{ gap: 8 }}>
        <button type="button" className="btn btn-sm" onClick={() => onGo(go("connect"))}>
          <ShellIcon name="plug" size={13} />
          接入配置
        </button>
        <button type="button" className="btn btn-sm btn-icon btn-soft gw-gear" onClick={() => setConfiguring(true)} disabled={!status} title="网关设置" aria-label="网关设置">
          <Icon name="settings" size={14} />
          {tuned ? <i className="gw-gear-dot" aria-hidden /> : null}
        </button>
        <button type="button" className="btn btn-sm btn-icon btn-soft" onClick={() => void reload()} disabled={busy} title="刷新" aria-label="刷新">
          <Icon name="refresh" size={14} />
        </button>
      </div>
    </div>
  );

  return (
    <div>
      {inPool ? poolHead : mainHead}

      <ErrorNote error={error} onRetry={() => void reload()} />

      {inPool ? (
        !status ? (
          <div className="accts">
            <div className="skeleton" style={{ height: 164 }} />
            <div className="skeleton" style={{ height: 164 }} />
          </div>
        ) : (
          <>
            {/* 网关没开时号池照样能整理，但得让人知道加了也还接不了请求。 */}
            {!running ? (
              <div style={{ marginBottom: 14 }}>
                <Banner
                  tone="default"
                  title="网关没开，这里的号暂时接不到请求"
                  action={
                    <button type="button" className="btn btn-sm" disabled={busy} onClick={() => void run(() => gateway.start())}>
                      开启网关
                    </button>
                  }
                />
              </div>
            ) : null}
            {pool}
          </>
        )
      ) : !status ? (
        <div className="skeleton" style={{ height: 96 }} />
      ) : (
        <div className="stack" style={{ gap: 12 }}>
          {/* 1. 开关 + 地址 + 口令 */}
          {/* 「进程在跑」和「能接请求」是两件事：开着但一个可用号都没有，绿灯就是在骗人。 */}
          <div className={running && !starved ? "card card-hot gw" : "card gw"}>
            <div className="gw-top">
              <span className={running ? (starved ? "current-orb is-warn" : "current-orb") : "current-orb is-off"} />
              <div className="grow" style={{ minWidth: 0 }}>
                <div className="row" style={{ alignItems: "baseline", gap: 10 }}>
                  <strong className="gw-state">{running ? "运行中" : "已关闭"}</strong>
                  {status.running ? (
                    <span className="muted tiny">自 {timeAgo(status.running.startedAt)}</span>
                  ) : (
                    <span className="muted tiny">开启后客户端就能把请求发到本机</span>
                  )}
                  {starved ? <Tag tone="warn">{enrolled === 0 ? "没有号，请求会被拒" : "号都不可用，请求会被拒"}</Tag> : null}
                </div>
              </div>
              <Switch checked={running} disabled={busy} label={running ? "关闭网关" : "开启网关"} onChange={(next) => void run(() => (next ? gateway.start() : gateway.stop()))} />
            </div>

            {/* 接线用的字面量默认收着。要接客户端去「接入」页一键写入；这里只给想核对或手抄的人留一个入口。 */}
            <div className="gw-wire">
              <span className="faint tiny">
                本机回环 · OpenAI / Anthropic 兼容 · 客户端配置在
                <button type="button" className="linkish" onClick={() => onGo(go("connect"))}>
                  接入
                </button>
                页一键写入
              </span>
              <button type="button" className="btn btn-sm btn-quiet gw-wire-toggle" aria-expanded={wiringOpen} onClick={() => setWiringOpen((v) => !v)}>
                地址与口令
                <Icon name="chevron" size={12} className={`opt-chev${wiringOpen ? " is-open" : ""}`} />
              </button>
            </div>

            {wiringOpen ? (
              <div className="gw-addrs">
                <AddrRow k="标准协议" v={status.running?.baseUrl ?? `http://127.0.0.1:${status.settings.port}`} hint="OpenAI / Anthropic · Claude Code、Codex、SDK" live={running} />
                <AddrRow k="Cursor 协议" v={status.running?.passthroughBaseUrl ?? `http://127.0.0.1:${status.settings.passthroughPort}`} hint="cursor-agent CLI" live={running} />
                <div className="gw-addr">
                  <span className="gw-addr-k">口令</span>
                  <span className="gw-addr-v">
                    {key ? (
                      <code className="mono selectable">{key}</code>
                    ) : (
                      <code className="mono muted">{status.apiKeySet ? "••••••••••••••••" : "首次开启时生成"}</code>
                    )}
                    <span className="gw-addr-hint">客户端的 API Key 填它 · 本机回环，只挡别的进程</span>
                  </span>
                  <span className="row" style={{ gap: 2 }}>
                    {key ? (
                      <>
                        <CopyButton value={key} icon />
                        <button type="button" className="btn btn-sm btn-icon btn-quiet" onClick={() => setKey(null)} title="隐藏" aria-label="隐藏">
                          <Icon name="eyeOff" size={13} />
                        </button>
                      </>
                    ) : (
                      <button type="button" className="btn btn-sm btn-icon btn-quiet" onClick={() => void revealKey()} disabled={!status.apiKeySet} title="显示" aria-label="显示">
                        <Icon name="eye" size={13} />
                      </button>
                    )}
                    <button type="button" className="btn btn-sm btn-quiet" onClick={() => void rotateKey()} disabled={!status.apiKeySet}>
                      更换
                    </button>
                  </span>
                </div>
              </div>
            ) : null}

            {status.restartNeeded ? (
              <Banner
                tone="warn"
                title="设置已改，重启网关后生效"
                action={
                  <button
                    type="button"
                    className="btn btn-sm"
                    disabled={busy}
                    onClick={() =>
                      void run(async () => {
                        await gateway.stop();
                        return gateway.start();
                      })
                    }
                  >
                    重启网关
                  </button>
                }
              />
            ) : null}
          </div>

          {/* 2. 通道：Cursor 和订阅通道并列。每行右边一个「去管号」的入口：Cursor 进它自己的号池页，
              其余通道进「账号」对应页签。行本身不展开 —— 通道这张卡只回答「几队号各什么光景」。 */}
          <div className="card card-flush gw-chans">
            <div className="gw-chans-head">
              <div className="row" style={{ gap: 8, alignItems: "baseline" }}>
                <strong>通道</strong>
                <span className="faint tiny">网关背后的几队号 · 模型名带前缀强制走该通道；不带前缀的走 Cursor</span>
              </div>
            </div>
            {channels.map((ch) =>
              ch.isDefault ? (
                <ChannelRow
                  key={ch.id}
                  ch={ch}
                  onOpen={() => onGo(go("gateway", { sub: "pool" }))}
                  actions={
                    <>
                      <button type="button" className="btn btn-sm" disabled={busy} onClick={() => setAdding(true)}>
                        <Icon name="plus" size={13} />
                        添加号
                      </button>
                      <button type="button" className="btn btn-sm btn-soft" onClick={() => onGo(go("gateway", { sub: "pool" }))}>
                        号池
                        <Icon name="chevron" size={12} className="gw-chan-go" />
                      </button>
                    </>
                  }
                />
              ) : (
                <ChannelRow
                  key={ch.id}
                  ch={ch}
                  onOpen={() => onGo(go("accounts", { platform: ch.id as AccountPlatform }))}
                  actions={
                    <button type="button" className="btn btn-sm btn-soft" onClick={() => onGo(go("accounts", { platform: ch.id as AccountPlatform }))}>
                      账号
                      <Icon name="chevron" size={12} className="gw-chan-go" />
                    </button>
                  }
                />
              ),
            )}
          </div>

          {status.mediaJobs.length > 0 ? <MediaJobsCard jobs={status.mediaJobs} /> : null}
        </div>
      )}

      {configuring && status ? (
        <SettingsModal
          settings={status.settings}
          running={running}
          onClose={() => setConfiguring(false)}
          onSave={async (patch) => {
            await saveSettings(patch);
          }}
        />
      ) : null}

      {openView ? (
        <AccountInspector
          view={openView}
          inCursor={Boolean(openCandidate?.pinned)}
          onClose={() => setOpenLabel(null)}
          onChanged={reload}
          onSwitch={() => onGo(go("switcher", { email: openView.label }))}
          placementActions={
            openCandidate ? (
              <>
                <button
                  type="button"
                  className="btn btn-sm btn-danger"
                  disabled={busy}
                  onClick={() => {
                    setOpenLabel(null);
                    void unenroll(openCandidate);
                  }}
                >
                  移出网关
                </button>
                <button
                  type="button"
                  className="btn btn-sm"
                  disabled={busy || openCandidate.state.kind === "current"}
                  onClick={() => void run(() => gateway.setCurrent(openCandidate.label))}
                >
                  {openCandidate.state.kind === "current" ? "使用中" : "用这个"}
                </button>
              </>
            ) : openMissing ? (
              <>
                <button
                  type="button"
                  className="btn btn-sm btn-danger"
                  disabled={busy}
                  onClick={() => {
                    setOpenLabel(null);
                    void run(() => gateway.unenroll(openMissing));
                  }}
                >
                  移出网关
                </button>
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() => onGo(go("accounts"))}
                >
                  去处理
                </button>
              </>
            ) : null
          }
          onOpenLibrary={() => onGo(go("accounts"))}
        />
      ) : null}

      {adding ? (
        <EnrollModal
          available={available}
          byEmail={byEmail}
          busy={busy}
          onClose={() => setAdding(false)}
          onEnroll={async (labels) => {
            await run(() => gateway.enroll(labels));
            setAdding(false);
          }}
          onGoAccounts={() => {
            setAdding(false);
            onGo(go("accounts"));
          }}
        />
      ) : null}
    </div>
  );
}

/**
 * 「添加号」：从号池里挑几个交给网关。
 *
 * 只列**此刻真能用**的号（网关自己算出来的 `available`）：待登录、没授权的号列出来也加不进队，
 * 只会让人加完发现没生效。它们要先去「账号」处理，弹窗底下留一句话指过去。
 * 默认一个都不勾 —— 这一步的意义就是让用户亲手挑。
 */
function EnrollModal({
  available,
  byEmail,
  busy,
  onClose,
  onEnroll,
  onGoAccounts,
}: {
  available: GatewayAvailable[];
  byEmail: Map<string, Account>;
  busy: boolean;
  onClose: () => void;
  onEnroll: (labels: string[]) => Promise<void>;
  onGoAccounts: () => void;
}) {
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const toggle = (label: string) =>
    setPicked((prev) => {
      const next = new Set(prev);
      if (next.has(label)) next.delete(label);
      else next.add(label);
      return next;
    });
  const all = picked.size === available.length && available.length > 0;

  return (
    <Modal
      title="添加号到网关"
      onClose={onClose}
      footer={
        <>
          <button type="button" className="btn" onClick={onClose}>
            取消
          </button>
          <button type="button" className="btn btn-primary" disabled={busy || picked.size === 0} onClick={() => void onEnroll([...picked])}>
            {picked.size > 0 ? `加入 ${picked.size} 个` : "加入"}
          </button>
        </>
      }
    >
      {available.length === 0 ? (
        <Empty
          title="没有能加的号"
          action={
            <button type="button" className="btn btn-sm" onClick={onGoAccounts}>
              去账号
            </button>
          }
        >
          网关只能用授权过（有 refresh_token）的号，或者 Cursor 里正登着的那个。号池里其余的要先去「账号」授权。
        </Empty>
      ) : (
        <div className="stack" style={{ gap: 10 }}>
          <div className="row-between">
            <span className="muted tiny">{available.length} 个可以加</span>
            <button
              type="button"
              className="linkish"
              onClick={() => setPicked(all ? new Set() : new Set(available.map((a) => a.label)))}
            >
              {all ? "全不选" : "全选"}
            </button>
          </div>
          <div className="list">
            {available.map((a) => {
              const acct = byEmail.get(a.label) ?? null;
              const u = acct?.usage;
              const on = picked.has(a.label);
              return (
                <label key={a.label} className={on ? "list-row enroll-row is-on" : "list-row enroll-row"}>
                  <input type="checkbox" className="tick" checked={on} onChange={() => toggle(a.label)} />
                  <span className="grow" style={{ minWidth: 0 }}>
                    <span className="row" style={{ gap: 8 }}>
                      <span className="mono selectable truncate" style={{ fontSize: 13 }}>
                        {a.label}
                      </span>
                      {u?.plan ? <span className={`plan ${planTone(u)}`}>{planLabel(u)}</span> : null}
                    </span>
                    <span className="faint tiny">
                      {a.pinned
                        ? "Cursor 当前登录"
                        : u?.totalPercentUsed != null
                          ? `总额度已用 ${Math.round(u.totalPercentUsed)}%`
                          : a.percentUsed != null
                            ? `已用 ${Math.round(a.percentUsed)}%`
                            : "未查用量"}
                    </span>
                  </span>
                </label>
              );
            })}
          </div>
        </div>
      )}
    </Modal>
  );
}

/**
 * 名单里有、此刻却没有任何来源给出凭证的号。
 *
 * 用户明明加过，列表却少一行，比一个「不可用」标签吓人 —— 所以它留在原位，说清为什么、
 * 去哪修，或者干脆移出。
 */
function gatewayMissingView(email: string, account: Account | null): AccountView {
  return createCursorAccountView({
    label: email,
    managed: account,
    placement: {
      kind: "gateway",
      label: "网关号池",
      detail: "接力时跳过 · 此刻拿不到凭证",
    },
    unavailableReason: account
      ? undefined
      : "这个名单成员已不在账号库，Cursor 也没有登录它",
  });
}

function MissingRow({
  email,
  account,
  busy,
  open,
  onOpen,
  onRemove,
  onFix,
}: {
  email: string;
  account: Account | null;
  busy: boolean;
  open: boolean;
  onOpen: () => void;
  onRemove: () => void;
  onFix: () => void;
}) {
  const why = !account
    ? "已不在号池里，Cursor 也没登着它"
    : accountProblem(account, account.usage)?.label ??
      (!account.hasRefresh
        ? account.hasAccess
          ? "session token 已过期，需要更新"
          : "没有 refresh_token，需要授权"
        : "此刻拿不到凭证");
  return (
    <AccountCard
      view={gatewayMissingView(email, account)}
      dimmed
      highlighted={open}
      onOpen={onOpen}
      badges={<Tag tone="bad">接力时跳过</Tag>}
      note={<span className="acct-problem">{why}</span>}
      actions={
        <>
          <button type="button" className="btn btn-sm btn-icon btn-soft btn-danger" disabled={busy} onClick={onRemove} aria-label="移出网关号池">
            <Icon name="trash" size={13} />
          </button>
          <button type="button" className="btn btn-sm" onClick={onFix}>
            去账号
          </button>
        </>
      }
    />
  );
}

/**
 * 三项常用设置直接保存；端口只留一个低干扰入口。这里不解释引擎原理，只保留做决定所需的信息。
 */
function SettingsModal({
  settings,
  running,
  onClose,
  onSave,
}: {
  settings: GatewaySettings;
  running: boolean;
  onClose: () => void;
  onSave: (patch: { port?: number; passthroughPort?: number; clientType?: string; autostart?: boolean; forceModel?: string | null }) => Promise<void>;
}) {
  const [saving, setSaving] = useState(false);
  const [portError, setPortError] = useState<string | null>(null);
  const [portsOpen, setPortsOpen] = useState(false);
  const current = CLIENT_TYPES.find((t) => t.id === settings.clientType);

  async function save(patch: Parameters<typeof onSave>[0]) {
    setSaving(true);
    try {
      await onSave(patch);
    } finally {
      setSaving(false);
    }
  }

  function commitPort(which: "port" | "passthroughPort", raw: string) {
    const p = Number(raw);
    if (!Number.isFinite(p) || p < 1024 || p > 65535) {
      setPortError("端口要在 1024–65535 之间。");
      return;
    }
    setPortError(null);
    if (p !== settings[which]) void save({ [which]: p });
  }

  return (
    <Modal title="网关设置" onClose={onClose} compact>
      <div className="opts is-flush">
        <Opt icon="power" title="自动启动" desc="随 Nexus 一起开" tone={settings.autostart ? "on" : undefined}>
          <Switch checked={settings.autostart} disabled={saving} label="随应用启动" onChange={(next) => void save({ autostart: next })} />
        </Opt>

        {/* 出图是 Sand 通道独有的能力（上游门禁），所以它不跟这里的选择走，否则选 CLI 的人永远出不了图。
            这句后果留在描述里，原理不留。 */}
        <Opt
          icon="sand"
          title="额度通道"
          desc="只管对话；出图固定走 Sand（Bot 额度）"
          tone={settings.clientType === "sand" ? "warn" : undefined}
          body={
            <div className="gwset-options" role="radiogroup" aria-label="额度通道">
              {CLIENT_TYPES.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  role="radio"
                  aria-checked={settings.clientType === t.id}
                  className={`gwset-option${settings.clientType === t.id ? " is-active" : ""}${t.id === "sand" ? " is-caution" : ""}`}
                  title={t.hint}
                  disabled={saving}
                  onClick={() => {
                    if (settings.clientType !== t.id) void save({ clientType: t.id });
                  }}
                >
                  {t.label}
                </button>
              ))}
            </div>
          }
        >
          {settings.clientType === "sand" ? (
            <Health tone="warn">{current?.hint}</Health>
          ) : (
            <span className="faint tiny">{current?.hint ?? "自定义"}</span>
          )}
        </Opt>

        <Opt
          icon="layers"
          title="上游模型"
          desc="留空跟随客户端请求"
          tone={settings.forceModel ? "on" : undefined}
          body={
            <input
              className="input mono"
              aria-label="上游模型"
              placeholder="跟随客户端请求"
              defaultValue={settings.forceModel ?? ""}
              disabled={saving}
              spellCheck={false}
              onBlur={(ev) => {
                const v = ev.currentTarget.value.trim();
                if (v !== (settings.forceModel ?? "")) void save({ forceModel: v || null });
              }}
              onKeyDown={(ev) => {
                if (ev.key === "Enter") ev.currentTarget.blur();
              }}
            />
          }
        />

        {/* 端口默认收着：冲突时会自动顺延，绝大多数人一辈子不用碰。 */}
        <Opt
          icon="gateway"
          title="端口"
          desc={portError ?? "1024–65535 · 冲突时自动顺延"}
          tone={portError ? "bad" : undefined}
          body={
            portsOpen ? (
              <div className="gwset-ports">
                <label>
                  <span>API</span>
                  <input className="input mono gwset-port" type="number" min={1024} max={65535} defaultValue={settings.port} disabled={saving} onBlur={(ev) => commitPort("port", ev.currentTarget.value)} />
                </label>
                <label>
                  <span>Cursor</span>
                  <input className="input mono gwset-port" type="number" min={1024} max={65535} defaultValue={settings.passthroughPort} disabled={saving} onBlur={(ev) => commitPort("passthroughPort", ev.currentTarget.value)} />
                </label>
              </div>
            ) : undefined
          }
        >
          <span className="faint tiny mono">
            {settings.port} / {settings.passthroughPort}
          </span>
          <button
            type="button"
            className="btn btn-sm btn-icon btn-quiet tip-end"
            data-tip={portsOpen ? "收起" : "修改端口"}
            aria-label={portsOpen ? "收起端口设置" : "修改端口"}
            aria-expanded={portsOpen}
            onClick={() => setPortsOpen((v) => !v)}
          >
            <Icon name="chevron" size={12} className={`opt-chev${portsOpen ? " is-open" : ""}`} />
          </button>
        </Opt>
      </div>

      {running ? (
        <div className="gwset-restart">
          <i aria-hidden />
          通道与端口将在重启后生效
        </div>
      ) : null}
    </Modal>
  );
}

/**
 * 通道列表里的一行：状态灯、名字、一句网关视角的结论（几个能接、谁在用、能不能出媒体）、
 * 前缀与模型数，右侧动作。名字那一列整块可点（`onOpen`），去这条通道的号在哪管：
 * Cursor 去号池页，其余去账号页对应页签。加第五条通道这里不用改——它从 `status.channels` 里来。
 */
function ChannelRow({ ch, actions, onOpen }: { ch: LocalChannel; actions: ReactNode; onOpen?: () => void }) {
  const s = channelSummary(ch);
  const { total } = laneCount(ch.lane);
  const models = modelsOf(ch).length;
  return (
    <div className="gw-chan">
      <div className="gw-chan-row">
        <span className={`current-orb ${s.tone === "ok" ? "" : s.tone === "warn" ? "is-warn" : "is-off"}`} />
        {onOpen ? (
          <button type="button" className="gw-chan-main gw-chan-btn" onClick={onOpen}>
            <ChannelRowText ch={ch} text={s.text} total={total} models={models} />
          </button>
        ) : (
          <div className="gw-chan-main">
            <ChannelRowText ch={ch} text={s.text} total={total} models={models} />
          </div>
        )}
        <div className="row" style={{ gap: 6, flex: "none" }}>
          {actions}
        </div>
      </div>
    </div>
  );
}

function ChannelRowText({ ch, text, total, models }: { ch: LocalChannel; text: string; total: number; models: number }) {
  return (
    <span className="grow stack" style={{ gap: 3, minWidth: 0 }}>
      <span className="row" style={{ gap: 8, alignItems: "baseline", minWidth: 0 }}>
        <strong>{ch.label}</strong>
        {ch.isDefault ? <Tag>默认</Tag> : null}
        <span className="muted tiny truncate">{text}</span>
      </span>
      <span className="row" style={{ gap: 6, flexWrap: "wrap" }}>
        {ch.prefixes.map((p) => (
          <code key={p} className="mono tiny">
            {p}
          </code>
        ))}
        <span className="faint tiny">
          {total > 0 ? `${total} 个号` : ch.isDefault ? "还没有号" : "没有号"}
          {models > 0 ? ` · ${ch.chatModels.length} 个对话模型` : ""}
          {ch.imageModels.length > 0 ? ` · ${ch.imageModels.length} 个生图` : ""}
          {ch.videoModels.length > 0 ? ` · ${ch.videoModels.length} 个生视频` : ""}
        </span>
      </span>
    </span>
  );
}

/** 最近的生视频任务：状态、归属的号、成片链接。任务粘在创建它的号上，换号查不到。 */
function MediaJobsCard({ jobs }: { jobs: MediaJob[] }) {
  return (
    <div className="card">
      <div className="row" style={{ gap: 8, alignItems: "baseline", marginBottom: 10 }}>
        <strong>视频任务</strong>
        <span className="faint tiny">异步：客户端拿到 request_id 后轮询 /v1/videos/{"{id}"}；成片链接是上游的临时地址</span>
      </div>
      <div className="list">
        {jobs.slice(0, 8).map((j) => (
          <div key={j.requestId} className="list-row" style={{ alignItems: "center" }}>
            <Tag tone={j.status === "done" ? "ok" : j.status === "failed" || j.status === "expired" ? "bad" : "default"}>{j.status}</Tag>
            <div className="grow stack" style={{ gap: 2, minWidth: 0 }}>
              <div className="row" style={{ gap: 8, minWidth: 0 }}>
                <code className="mono tiny selectable truncate">{j.requestId}</code>
                <span className="faint tiny">{j.model}</span>
                {j.durationSecs ? <span className="faint tiny">{j.durationSecs}s</span> : null}
                {j.resolution ? <span className="faint tiny">{j.resolution}</span> : null}
              </div>
              <span className="faint tiny">
                {j.channel} · {j.account} · {timeAgo(new Date(j.createdMs).toISOString())}
              </span>
            </div>
            {j.videoUrl ? <CopyButton value={j.videoUrl} label="复制链接" /> : null}
          </div>
        ))}
      </div>
    </div>
  );
}

function AddrRow({ k, v, hint, live }: { k: string; v: string; hint: string; live: boolean }) {
  return (
    <div className="gw-addr">
      <span className="gw-addr-k">{k}</span>
      <span className="gw-addr-v">
        <code className={live ? "mono selectable" : "mono muted selectable"}>{v}</code>
        <span className="gw-addr-hint">{hint}</span>
      </span>
      <CopyButton value={v} icon />
    </div>
  );
}

/** 候选号此刻在接力里处于什么位置。只有「耗尽 / 到线」是坏消息。 */
function stateLabel(c: GatewayCandidate): { text: string; tone: "ok" | "bad" | "default" } | null {
  switch (c.state.kind) {
    case "current":
      return { text: "正在用", tone: "ok" };
    // 「待接力」是常态，一列里每个都挂一个灰标签等于没标。
    case "ready":
      return null;
    case "exhausted":
      return { text: `耗尽 · ${Math.ceil(c.state.retryInSecs / 60)} 分后重试`, tone: "bad" };
    case "quota_line":
      return { text: "额度到线", tone: "bad" };
    case "cooled":
      return { text: `${c.state.models.join(", ")} 冷却 ${Math.ceil(c.state.secsLeft / 60)} 分`, tone: "default" };
  }
}

/**
 * 网关候选号的一行。骨架与「账号」「切号」共用（`AccountLine`）。
 *
 * 网关自己只知道一个总用量百分比，所以能按邮箱对上「账号」时就摆完整的四个桶 ——
 * 决定「要不要手动切到这个号」看的正是这些数字，和另外两页是同一个判断。
 */
function gatewayAccountView(
  candidate: GatewayCandidate,
  account: Account | null,
): AccountView {
  const state = stateLabel(candidate);
  return createCursorAccountView({
    label: candidate.label,
    managed: account,
    fallbackPercentUsed: candidate.percentUsed,
    placement: {
      kind: "gateway",
      label: "网关号池",
      detail:
        candidate.state.kind === "current"
          ? "正在接力"
          : state?.text ?? "等待接力",
    },
    unavailableReason: "网关只拿到了身份，没有完整用量数据",
  });
}

function CandidateRow({
  c,
  account,
  busy,
  open,
  onOpen,
  onUse,
  onRemove,
}: {
  c: GatewayCandidate;
  account: Account | null;
  busy: boolean;
  open: boolean;
  onOpen: () => void;
  onUse: () => void;
  onRemove: () => void;
}) {
  const s = stateLabel(c);
  const isCurrent = c.state.kind === "current";

  return (
    <AccountCard
      view={gatewayAccountView(c, account)}
      highlighted={isCurrent || open}
      onOpen={onOpen}
      badges={s ? <Tag tone={s.tone}>{s.text}</Tag> : null}
      // 「哪来的号」在这一页是要紧事（真机码那个不能随便换），所以留着；耗尽原因也留。
      note={
        c.state.kind === "exhausted" ? (
          <span className="acct-problem" title={c.state.reason}>
            {c.state.reason.slice(0, 90)}
          </span>
        ) : c.pinned ? (
          <span>Cursor 正登着 · 真机码</span>
        ) : null
      }
      // 正在用的那一行也留一个（禁用的）按钮：右列对齐了，一列扫下来才看得出这些行是同一种东西。
      // 移出藏在 hover 后面，和切号页删快照的键一个位置 —— 它不是每行都要按的。
      actions={
        <>
          <span className="acct-hover">
            <button type="button" className="btn btn-sm btn-icon btn-soft btn-danger" disabled={busy} onClick={onRemove} aria-label="移出网关号池">
              <Icon name="trash" size={13} />
            </button>
          </span>
          <button type="button" className="btn btn-sm" disabled={busy || isCurrent} onClick={onUse}>
            {isCurrent ? "使用中" : "用这个"}
          </button>
        </>
      }
    />
  );
}
