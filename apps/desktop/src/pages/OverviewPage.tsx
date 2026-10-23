/**
 * 概览——打开应用第一眼看到的综合调度看板。
 *
 * 摒弃单一 Cursor 账号的狭隘视角，提升为多平台、多协议 Local-First 网关全景中心：
 *   1. 顶栏 5 联关键指标矩阵（网关核心、路由通道、托管账号、可用模型、Token 吞吐）；
 *   2. 中间 LLM Token 消耗与网关吞吐分析面板（Token 构成、缓存命中率、实时曲线、模型选择器）；
 *   3. 下方双栏对称布局：
 *      - 左栏：网关实时活动流（请求账本与系统审计事件，支持微筛）；
 *      - 右栏：通道就绪矩阵（Cursor, ChatGPT, Claude, Kiro, Antigravity, Grok 等健康态）。
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { accounts, app, gateway as gatewayApi, meshGetStatus, switcher } from "../ipc/api";
import type { Account, ActivityEntry, AppStatus, MeshStatus, Overview, UsageSummary } from "../ipc/types";
import { useRelay } from "../relay/useRelay";
import { go, type Route } from "../shell/nav";
import { ShellIcon } from "../shell/ShellIcon";
import { summarize } from "../ui/accounts";
import { Banner, Icon, Switch, Tag } from "../ui/primitives";
import { compactNumber } from "../ui/traffic";
import { UsagePanel, type UsageRange } from "./overview/UsagePanel";
import { RequestStreamTable } from "./overview/RequestStreamTable";
import { GatewayInspectorModal } from "../shell/GatewayPod";

interface Snapshot {
  app: AppStatus | null;
  overview: Overview | null;
  accounts: Account[];
  activity: ActivityEntry[];
  mesh: MeshStatus | null;
}

const EMPTY: Snapshot = { app: null, overview: null, accounts: [], activity: [], mesh: null };

const RECENT_ROWS = 8;

export function OverviewPage({ onGo }: { onGo: (r: Route) => void }) {
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [busy, setBusy] = useState(false);
  const [gatewayModalOpen, setGatewayModalOpen] = useState(false);
  const [range, setRange] = useState<UsageRange>(7);
  const [tick, setTick] = useState(0);
  const [usageData, setUsageData] = useState<UsageSummary | null>(null);
  const relay = useRelay({ catalogs: true, pollMs: 8000 });

  const reload = useCallback(async () => {
    const [a, o, l, act, m] = await Promise.allSettled([
      app.status(),
      switcher.overview(),
      accounts.list(),
      app.activity(RECENT_ROWS),
      meshGetStatus(),
    ]);
    setSnap({
      app: a.status === "fulfilled" ? a.value : null,
      overview: o.status === "fulfilled" ? o.value : null,
      accounts: l.status === "fulfilled" ? l.value : [],
      activity: act.status === "fulfilled" ? act.value : [],
      mesh: m.status === "fulfilled" ? m.value : null,
    });
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const s = snap ?? EMPTY;
  const live = useMemo(() => s.accounts.filter((a) => !a.archivedAt), [s.accounts]);
  const summary = useMemo(() => summarize(live), [live]);
  const needsLogin = live.filter((a) => a.status === "needs_login").length;
  const cursorMissing = s.app != null && !s.app.cursor.dbPresent;

  async function toggleGateway(next: boolean) {
    setBusy(true);
    try {
      await (next ? gatewayApi.start() : gatewayApi.stop());
    } finally {
      setBusy(false);
      await relay.reload();
      setTick((n) => n + 1);
    }
  }

  const g = relay.gateway;
  const running = Boolean(g?.running);

  // 目录统计
  const catalog = useMemo(() => {
    const acc = { total: 0, image: 0, video: 0 };
    for (const m of relay.local ?? []) {
      acc.total += 1;
      if (m.modality === "image") acc.image += 1;
      else if (m.modality === "video") acc.video += 1;
    }
    return acc;
  }, [relay.local]);

  // 通道就绪数与活跃度统计
  const channelsList = useMemo(() => {
    return g?.channels ?? [];
  }, [g?.channels]);

  const readyChannelsCount = useMemo(() => {
    let count = channelsList.filter((c) => c.ready).length;
    if (g?.lane?.candidates?.length) count += 1;
    if (s.mesh?.peers?.length || s.mesh?.host_channels?.some((h) => h.enabled)) count += 1;
    return count;
  }, [channelsList, g?.lane?.candidates?.length, s.mesh]);

  const totalChannelsCount = useMemo(() => {
    return Math.max(channelsList.length + 2, 9);
  }, [channelsList.length]);

  const meshPeersCount = s.mesh?.peers?.length ?? 0;

  // Token 用量
  const tokenTotals = useMemo(() => {
    if (!usageData) {
      return { total: 0, input: 0, output: 0, cache: 0 };
    }
    const cur = range === 1 ? usageData.today : usageData.window;
    const inp = cur.inputTokens;
    const out = cur.outputTokens;
    const cch = cur.cacheReadTokens;
    return {
      total: inp + out + cch,
      input: inp,
      output: out,
      cache: cch,
    };
  }, [usageData, range]);

  return (
    <div className="overview-container">

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

      {/* 顶层 5 联关键指标矩阵 */}
      <section className="ov-kpis-grid" aria-label="核心指标">
        {/* 卡 1: 本地网关 */}
        <div className="card ov-kpi-card ov-kpi-gateway">
          <div className="ov-kpi-top">
            <span className="ov-kpi-k">本地网关</span>
            <div className="ov-kpi-ctl" role="presentation">
              {g ? (
                <Switch
                  checked={running}
                  disabled={busy}
                  label={running ? "关闭网关" : "开启网关"}
                  onChange={(next) => void toggleGateway(next)}
                />
              ) : null}
            </div>
          </div>
          <div className="ov-kpi-v-row">
            <span className={`ov-status-dot ${running ? "is-live" : "is-off"}`} />
            <strong className="ov-kpi-v">{running ? "运行中" : "待命"}</strong>
          </div>
          <div className="ov-kpi-bottom">
            <div className="ov-kpi-sub truncate">
              {g ? (running ? `http://${g.running?.addr ?? "127.0.0.1"}` : `端口 :${g.settings?.port ?? 8694}`) : "加载中..."}
              {g?.lane?.candidates?.length ? ` · ${g.lane.candidates.length} 号就绪` : ""}
            </div>
            <button
              type="button"
              className="ov-gw-settings-btn"
              onClick={() => setGatewayModalOpen(true)}
              title="配置网关监听端口、访问口令与守护进程"
            >
              <Icon name="settings" size={12} />
              <span>设置</span>
            </button>
          </div>
        </div>

        {/* 卡 2: 路由通道 */}
        <div
          className="card ov-kpi-card is-interactive"
          onClick={() => onGo(go("channels"))}
          role="button"
          tabIndex={0}
        >
          <div className="ov-kpi-top">
            <span className="ov-kpi-k">路由通道</span>
            <ShellIcon name="channels" size={14} className="ov-kpi-icon" />
          </div>
          <div className="ov-kpi-v-row">
            <strong className="ov-kpi-v num">{readyChannelsCount}</strong>
            <span className="ov-kpi-unit">/ {totalChannelsCount}</span>
          </div>
          <div className="ov-kpi-sub truncate">
            {meshPeersCount > 0 ? `${meshPeersCount} 个 Mesh 互联节点` : "8 本地 · 1 Mesh 共享"}
          </div>
        </div>

        {/* 卡 3: 托管账号 */}
        <div
          className="card ov-kpi-card is-interactive"
          onClick={() => onGo(go("accounts"))}
          role="button"
          tabIndex={0}
        >
          <div className="ov-kpi-top">
            <span className="ov-kpi-k">托管账号</span>
            <ShellIcon name="accounts" size={14} className="ov-kpi-icon" />
          </div>
          <div className="ov-kpi-v-row">
            <strong className="ov-kpi-v num">{summary.total}</strong>
            <span className="ov-kpi-unit">个号</span>
            {needsLogin > 0 ? <Tag tone="warn">{needsLogin} 待登录</Tag> : null}
          </div>
          <div className="ov-kpi-sub truncate">
            {summary.total === 0 ? "未配置号池" : `${(summary.by.long_lived ?? 0) + (summary.by.session ?? 0)} 可用 · ${needsLogin > 0 ? `${needsLogin} 待登录` : "号池健康"}`}
          </div>
        </div>

        {/* 卡 4: 可用模型 */}
        <div
          className="card ov-kpi-card is-interactive"
          onClick={() => onGo(go("models"))}
          role="button"
          tabIndex={0}
        >
          <div className="ov-kpi-top">
            <span className="ov-kpi-k">可用模型</span>
            <ShellIcon name="models" size={14} className="ov-kpi-icon" />
          </div>
          <div className="ov-kpi-v-row">
            <strong className="ov-kpi-v num">{catalog.total}</strong>
            <span className="ov-kpi-unit">款</span>
          </div>
          <div className="ov-kpi-sub truncate">
            多厂商聚合 · 全协议兼容
          </div>
        </div>

        {/* 卡 5: Token 吞吐 */}
        <div className="card ov-kpi-card">
          <div className="ov-kpi-top">
            <span className="ov-kpi-k">Token 用量</span>
            <ShellIcon name="gateway" size={14} className="ov-kpi-icon" />
          </div>
          <div className="ov-kpi-v-row">
            <strong className="ov-kpi-v num">{compactNumber(tokenTotals.total)}</strong>
            <span className="ov-kpi-unit">tokens</span>
          </div>
          <div className="ov-kpi-sub truncate">
            入 {compactNumber(tokenTotals.input)} · 缓存 {compactNumber(tokenTotals.cache)} · 出 {compactNumber(tokenTotals.output)}
          </div>
        </div>
      </section>

      {/* 中部：LLM Token 消耗与网关吞吐面板 */}
      <UsagePanel
        range={range}
        onRange={setRange}
        gatewayRunning={running}
        reloadKey={tick}
        onGoConnect={() => onGo(go("connect"))}
        onGoGateway={() => onGo(go("channels"))}
        onDataLoaded={setUsageData}
      />

      {/* 下部：全宽最近请求记录 */}
      <div className="ov-stream-section">
        <RequestStreamTable requests={usageData?.recent ?? []} />
      </div>

      {gatewayModalOpen ? (
        <GatewayInspectorModal
          status={relay.gateway}
          usage={usageData}
          onClose={() => setGatewayModalOpen(false)}
          onReload={async () => {
            await relay.reload();
            setTick((n) => n + 1);
          }}
          onGo={onGo}
        />
      ) : null}
    </div>
  );
}
