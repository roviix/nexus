/**
 * 概览——打开应用第一眼看到的那页。
 *
 * 它回答两个问题，按顺序摆：**现在是什么状态**（正登着的号、网关开没开、能映射多少模型），
 * **最近用了多少**（本地网关的请求账本：今天几次、几天走势、花在哪个模型哪个号上）。
 *
 * 版式就是信息架构：正在用的号压顶；中间一整块是用量；下面一行三张小卡 —— 本地网关、
 * 模型目录、账号池的成色 —— 每张都是一扇门，点进去才是那一页。
 * 这里唯一能按的操作是网关开关：一个人人看得懂的开关，不值得为它跑一页。
 */
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { AccountCard } from "../accounts/AccountCard";
import { AccountInspector } from "../accounts/AccountInspector";
import { createCursorAccountView } from "../accounts/model";
import { accounts, app, gateway as gatewayApi, switcher } from "../ipc/api";
import type { Account, ActivityEntry, AppStatus, Overview } from "../ipc/types";
import { useRelay } from "../relay/useRelay";
import { go, type Route } from "../shell/nav";
import { ShellIcon } from "../shell/ShellIcon";
import { AVAIL_LABEL, AVAIL_ORDER, summarize } from "../ui/accounts";
import type { Availability } from "../ipc/types";
import { timeAgo } from "../ui/format";
import { Banner, Icon, Switch, Tag } from "../ui/primitives";
import { UsagePanel, type UsageRange } from "./overview/UsagePanel";

interface Snapshot {
  app: AppStatus | null;
  overview: Overview | null;
  accounts: Account[];
  activity: ActivityEntry[];
}

const EMPTY: Snapshot = { app: null, overview: null, accounts: [], activity: [] };

/** 页脚「最近」摆几条。取多少就摆多少 —— 多取一条再切掉只是让人以为这里能翻页。 */
const RECENT_ROWS = 5;

export function OverviewPage({ onGo }: { onGo: (r: Route) => void }) {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [busy, setBusy] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [inspectCurrent, setInspectCurrent] = useState(false);
  const [range, setRange] = useState<UsageRange>(7);
  const [tick, setTick] = useState(0);
  const relay = useRelay({ catalogs: true, pollMs: 8000 });

  const reload = useCallback(async () => {
    const [a, o, l, act] = await Promise.allSettled([app.status(), switcher.overview(), accounts.list(), app.activity(RECENT_ROWS)]);
    setSnap({
      app: a.status === "fulfilled" ? a.value : null,
      overview: o.status === "fulfilled" ? o.value : null,
      accounts: l.status === "fulfilled" ? l.value : [],
      activity: act.status === "fulfilled" ? act.value : [],
    });
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const s = snap ?? EMPTY;
  /**
   * 「账号池」那一格数的是没归档的号。归档的不进网关候选，默认列表也看不见它们，算进来这一格
   * 就比账号页默认看到的那一列多出几个 —— 点过去发现数不对，这一格就白摆了。
   * `byEmail` 仍照全量建：Cursor 此刻登着的那个号归了档，也得认出来。
   */
  const live = useMemo(() => s.accounts.filter((a) => !a.archivedAt), [s.accounts]);
  const summary = useMemo(() => summarize(live), [live]);
  const byEmail = useMemo(() => new Map(s.accounts.map((a) => [a.email.toLowerCase(), a])), [s.accounts]);

  const current = s.overview?.current ?? null;
  const currentAccount = current?.email ? byEmail.get(current.email.toLowerCase()) ?? null : null;
  const currentView = useMemo(
    () =>
      current?.email
        ? createCursorAccountView({
            label: current.email,
            managed: currentAccount,
            placement: {
              kind: "overview",
              label: "Cursor 当前登录",
              detail: s.overview?.machineIdOwner ? `机器码属于 ${s.overview.machineIdOwner}` : "使用本机原始机器码",
            },
            unavailableReason: currentAccount ? undefined : "这个登录只存在于 Cursor，未在账号库托管，无法查询完整用量",
          })
        : null,
    [current, currentAccount, s.overview?.machineIdOwner],
  );
  const cursorMissing = s.app != null && !s.app.cursor.dbPresent;
  const needsLogin = live.filter((a) => a.status === "needs_login").length;

  /** 标题栏那枚刷新键：账本、网关、用量三处一起拉，转到都回来为止。 */
  async function refreshAll() {
    setRefreshing(true);
    try {
      await Promise.all([reload(), relay.reload()]);
      setTick((n) => n + 1);
    } finally {
      setRefreshing(false);
    }
  }

  async function toggleGateway(next: boolean) {
    setBusy(true);
    try {
      await (next ? gatewayApi.start() : gatewayApi.stop());
    } finally {
      setBusy(false);
      await relay.reload();
    }
  }

  const g = relay.gateway;
  const running = Boolean(g?.running);
  // 目录按 modality 分一下：一眼看出这台网关此刻能聊、能画、能出视频里的哪几样。
  const catalog = useMemo(() => {
    const acc = { total: 0, image: 0, video: 0 };
    for (const m of relay.local ?? []) {
      acc.total += 1;
      if (m.modality === "image") acc.image += 1;
      else if (m.modality === "video") acc.video += 1;
    }
    return acc;
  }, [relay.local]);

  return (
    <div>
      <div className="page-head">
        <h1>概览</h1>
        <button
          type="button"
          className="btn btn-icon btn-soft tip-end"
          data-tip="刷新"
          aria-label="刷新"
          onClick={() => void refreshAll()}
          disabled={!snap || refreshing}
        >
          <Icon name="refresh" size={15} className={refreshing ? "is-spinning" : undefined} />
        </button>
      </div>

      {cursorMissing ? (
        <div style={{ marginBottom: 14 }}>
          <Banner
            tone="bad"
            title="没找到本机的 Cursor"
            action={
              <button type="button" className="btn btn-sm" onClick={() => onGo(go("settings", { tab: "advanced" }))}>
                设置路径
              </button>
            }
          />
        </div>
      ) : null}

      {/* 状态带：左边「我是谁」，右边「这三件事现在什么状态」。
          并排而不是上下摞，两个理由都很实在 —— 当前账号卡有 420px 的宽度上限（额度条再长
          就成发丝线了），它独占一行会在右边空掉大半屏；而「网关开没开 / 还剩多少钱 /
          号池什么成色」正是这一页要回答的问题，摆到用量图下面就掉出了首屏。 */}
      <div className="ov-band">
        {!snap ? (
          <div className="skeleton ov-band-skel" />
        ) : currentView ? (
          <AccountCard
            view={currentView}
            highlighted
            onOpen={() => setInspectCurrent(true)}
            badges={
              <>
                <Tag tone="ok">当前登录</Tag>
                {current?.subscriptionStatus && current.subscriptionStatus !== "active" ? <Tag tone="warn">{current.subscriptionStatus}</Tag> : null}
              </>
            }
            note={s.overview?.machineIdShort ? <span>机器码 {s.overview.machineIdShort}</span> : null}
            actions={
              // 只留图标：⇄ 已经把「换一个」说清楚了，文案交给悬停时的气泡。
              <button type="button" className="btn btn-sm btn-icon tip-end" data-tip="切号" aria-label="切号" onClick={() => onGo(go("switcher"))}>
                <Icon name="switcher" size={14} />
              </button>
            }
          />
        ) : (
          <div className="card current ov-band-none">
            <div className="current-top">
              <span className="current-orb is-off" />
              <strong>Cursor 未登录</strong>
            </div>
            {/* 并排之后这一格有了整张卡的高度，只摆一行「未登录」就是一个空盒子。
                补的这句是空态该有的那一句：说清写进去之后这里会变成什么。 */}
            <p className="ov-band-hint">从切号池挑一个号写进 Cursor，这里就会显示它的额度和重置时间。</p>
            <div>
              <button type="button" className="btn btn-sm" onClick={() => onGo(go("switcher"))}>
                去切号池
              </button>
            </div>
          </div>
        )}

        <div className="ov-rail">
          <StatusRow
            title="本地网关"
            onOpen={() => onGo(go("gateway"))}
            control={g ? <Switch checked={running} disabled={busy} label={running ? "关闭网关" : "开启网关"} onChange={(next) => void toggleGateway(next)} /> : null}
            value={
              g ? (
                <>
                  <span className={running ? (g.lane.candidates.length ? "current-orb" : "current-orb is-warn") : "current-orb is-off"} />
                  <strong>{running ? "运行中" : "已关闭"}</strong>
                </>
              ) : null
            }
            sub={
              g ? (
                <>
                  <span className="mono">{running ? g.running!.addr : `:${g.settings.port}`}</span>
                  {` · ${g.lane.candidates.length ? `${g.lane.candidates.length} 个号在接力` : "号池为空"}`}
                  {g.lane.current ? ` · 当前 ${g.lane.current}` : ""}
                </>
              ) : null
            }
          />

          <StatusRow
            title="模型目录"
            onOpen={() => onGo(go("models"))}
            value={
              relay.local ? (
                <>
                  <strong className="num">{catalog.total}</strong>
                  <span className="faint">个模型</span>
                </>
              ) : null
            }
            sub={
              relay.local
                ? `OpenAI / Anthropic 兼容接口${catalog.image ? ` · ${catalog.image} 个能出图` : ""}${catalog.video ? ` · ${catalog.video} 个能出视频` : ""}`
                : null
            }
          />

          <StatusRow
            title="账号池"
            onOpen={() => onGo(go("accounts"))}
            value={
              !snap ? null : summary.total === 0 ? (
                <strong className="muted">还没有账号</strong>
              ) : (
                <>
                  <strong className="num">{summary.total}</strong>
                  <span className="faint">个号</span>
                  {needsLogin ? <Tag tone="warn">{needsLogin} 待登录</Tag> : null}
                </>
              )
            }
            sub={!snap ? null : summary.total === 0 ? "添加或导入你的 Cursor 号" : <PoolLegend by={summary.by} />}
          />
        </div>
      </div>

      <UsagePanel range={range} onRange={setRange} gatewayRunning={running} reloadKey={tick} onGoConnect={() => onGo(go("connect"))} onGoGateway={() => onGo(go("gateway"))} />

      {s.activity.length > 0 ? (
        <section className="ov-recent">
          <div className="fw-sect-cap">
            <strong>最近</strong>
            <span className="grow" />
            <button type="button" className="linkish" onClick={() => onGo(go("settings", { tab: "log" }))}>
              全部记录 →
            </button>
          </div>
          <div className="ov-log">
            {s.activity.map((e) => (
              <div key={e.id} className={`ov-log-row is-${e.level}`}>
                <span className="ov-log-time mono">{timeAgo(e.at)}</span>
                <span className="truncate">{e.message}</span>
                {e.email ? <span className="ov-log-who mono truncate">{e.email}</span> : null}
              </div>
            ))}
          </div>
        </section>
      ) : null}

      {inspectCurrent && currentView ? (
        <AccountInspector
          view={currentView}
          inCursor
          onClose={() => setInspectCurrent(false)}
          onChanged={reload}
          onSwitch={() => onGo(go("switcher", { email: currentView.label }))}
          onOpenLibrary={() => onGo(go("accounts"))}
        />
      ) : null}
    </div>
  );
}

/**
 * 状态带右侧的一行：标题在左、结论右对齐、细节压在下面一行，整行可点进对应的页。
 *
 * 结论右对齐是这一带成立的关键 —— 三行的值落在同一条竖线上，眼睛顺着右边缘往下扫一遍
 * 就读完了「开没开 / 还剩多少钱 / 号池什么成色」。为此右边留一条固定的槽给箭头或开关，
 * 有没有开关的行都对得齐。
 */
function StatusRow({ title, value, sub, onOpen, control }: { title: string; value: ReactNode; sub: ReactNode; onOpen: () => void; control?: ReactNode }) {
  return (
    <div
      className="card srow"
      role="button"
      tabIndex={0}
      aria-label={`打开${title}`}
      onClick={onOpen}
      onKeyDown={(e) => {
        if (e.target !== e.currentTarget) return;
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen();
        }
      }}
    >
      <div className="srow-top">
        <span className="srow-k">{title}</span>
        {/* 数据还没到时留一条占位，不要让标题独自晃在那儿。 */}
        <span className="srow-v">{value ?? <i className="srow-wait" />}</span>
      </div>
      <div className="srow-sub">{sub ?? <i className="srow-wait is-wide" />}</div>
      <ShellIcon name="arrow" size={12} className="srow-arrow" />
      {/* 开关是这一行里唯一的真按钮：它的点击不能顺带把页面也翻过去。 */}
      {control ? (
        <span className="srow-ctl" onClick={(e) => e.stopPropagation()} role="presentation">
          {control}
        </span>
      ) : null}
    </div>
  );
}

/**
 * 账号按**可用性**分档的图例（可用 / 仅会话 / 掉登录 / 已失效）。为零的档不写 —— 一行
 * 「已失效 0」占的位置和真有 3 个已失效时一样宽，扫一眼分不出哪个是要处理的。
 *
 * 这里不画那条分布条：一行 52 高的状态行只有一条副行，而带数字的彩点比一条 3px 的杠
 * 说得更清楚；要看比例去账号页，那儿本来就有一条通宽的。
 */
function PoolLegend({ by }: { by: Record<Availability, number> }) {
  return (
    <span className="srow-legend">
      {AVAIL_ORDER.filter((k) => by[k] > 0).map((k) => (
        <span key={k} className="srow-chip">
          <i className={`pool-dot is-${k}`} />
          {AVAIL_LABEL[k]} <b className="num">{by[k]}</b>
        </span>
      ))}
    </span>
  );
}
