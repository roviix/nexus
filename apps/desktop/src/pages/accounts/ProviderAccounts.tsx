/**
 * 账号页的「供应商」：网关 `provider/` 通道背后那几家。
 *
 * 现代化、克制、精致质感（完全对标 Cursor / Antigravity / Kiro 卡片 + 抽屉规范）：
 * - 卡片网格：服务商身份、协议徽章、端点地址、支持模型、时延与启停开关；
 * - 点击任意卡片滑出详情抽屉：完整模型清单、连通性探测、网关优先调度与配置编辑。
 */
import { useCallback, useEffect, useMemo, useState, type ReactNode } from "react";
import { errorText, gateway, keyProviders } from "../../ipc/api";
import type { ApiFormat, GatewayCandidateState, GatewayStatus, KeyProvider, ProviderPing } from "../../ipc/types";
import { go, type Route } from "../../shell/nav";
import { confirm } from "../../ui/confirm";
import { Banner, Empty, Icon, Spinner } from "../../ui/primitives";
import { GatewayCard } from "./GatewayCard";
import { KeyProviderModal } from "./KeyProviderModal";
import { formatLabel, primaryModel } from "./keyModel";
import { ProviderDrawer } from "./ProviderDrawer";

type FormatFilter = "all" | ApiFormat;

const FILTERS: { id: FormatFilter; label: string }[] = [
  { id: "all", label: "全部" },
  { id: "anthropic", label: "Anthropic" },
  { id: "openai_chat", label: "OpenAI Chat" },
  { id: "openai_responses", label: "Responses" },
];

type Ping = { ok: true; r: ProviderPing } | { ok: false; error: string };

export function ProviderAccounts({ tabs, onGo }: { tabs: ReactNode; onGo: (r: Route) => void }) {
  const [list, setList] = useState<KeyProvider[] | null>(null);
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<FormatFilter>("all");
  const [availFilter, setAvailFilter] = useState<"all" | "enabled" | "disabled">("all");
  const [modal, setModal] = useState<KeyProvider | null | "closed">("closed");
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [pinging, setPinging] = useState<string | null>(null);
  const [pings, setPings] = useState<Record<string, Ping>>({});

  const load = useCallback(async () => {
    const [p, s] = await Promise.allSettled([keyProviders.list(), gateway.status()]);
    if (p.status === "fulfilled") {
      setList(p.value);
      setError(null);
    } else {
      setError(errorText(p.reason));
      setList([]);
    }
    setStatus(s.status === "fulfilled" ? s.value : null);
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // 网关开着时状态会随请求变（冷却、接力）：隔一会儿刷一次。
  useEffect(() => {
    if (!status?.running) return;
    const t = window.setInterval(() => void gateway.status().then(setStatus).catch(() => {}), 5000);
    return () => window.clearInterval(t);
  }, [status?.running]);

  const lane = status?.channels.find((c) => c.id === "provider")?.lane;
  const stateOf = (name: string): GatewayCandidateState | null =>
    lane?.candidates.find((c) => c.label === name)?.state ?? null;
  const troubled = (lane?.candidates ?? []).some((c) => c.state.kind === "exhausted" || c.state.kind === "cooled");

  async function remove(p: KeyProvider) {
    const ok = await confirm(`网关里就没有「${p.name}」了；客户端的路由如果指着它的模型，会换到别家。`, {
      title: `删除 ${p.name}？`,
      okLabel: "删除",
      danger: true,
    });
    if (!ok) return;
    if (selectedId === p.id) setSelectedId(null);
    try {
      await keyProviders.remove(p.id);
      await load();
    } catch (e) {
      setError(errorText(e));
    }
  }

  async function setEnabled(p: KeyProvider, enabled: boolean) {
    try {
      await keyProviders.setEnabled(p.id, enabled);
      await load();
    } catch (e) {
      setError(errorText(e));
    }
  }

  async function ping(p: KeyProvider) {
    setPinging(p.id);
    try {
      const r = await keyProviders.ping(p.id);
      setPings((m) => ({ ...m, [p.id]: { ok: true, r } }));
    } catch (e) {
      setPings((m) => ({ ...m, [p.id]: { ok: false, error: errorText(e) } }));
    } finally {
      setPinging(null);
      void load();
    }
  }

  async function prefer(p: KeyProvider) {
    try {
      setStatus(await keyProviders.prefer(p.name));
    } catch (e) {
      setError(errorText(e));
    }
  }

  async function resetLane() {
    try {
      setStatus(await keyProviders.resetLane());
    } catch (e) {
      setError(errorText(e));
    }
  }

  const providers = list ?? [];
  const enabledCount = providers.filter((p) => p.enabled).length;
  const disabledCount = providers.length - enabledCount;
  const shown = providers.filter((p) => {
    if (availFilter === "enabled" && !p.enabled) return false;
    if (availFilter === "disabled" && p.enabled) return false;
    if (filter !== "all" && p.apiFormat !== filter) return false;
    return true;
  });
  const selectedProvider = useMemo(
    () => providers.find((p) => p.id === selectedId) ?? null,
    [providers, selectedId],
  );

  return (
    <>
      <div className="page-head acct-head">
        {tabs}
        <div className="row" style={{ gap: 8 }}>
          {troubled ? (
            <button
              type="button"
              className="btn btn-sm btn-quiet"
              onClick={() => void resetLane()}
              title="清掉冷却和停用记录，让每一家重新可选"
            >
              清掉冷却
            </button>
          ) : null}
          <button
            type="button"
            className="btn btn-sm btn-icon btn-soft"
            onClick={() => void load()}
            title="刷新"
            aria-label="刷新"
          >
            <Icon name="refresh" size={14} />
          </button>
          <button type="button" className="btn btn-primary" onClick={() => setModal(null)}>
            <Icon name="plus" size={14} />
            添加供应商
          </button>
        </div>
      </div>

      {error ? <Banner tone="bad" title="出了点问题" hint={error} /> : null}

      {list == null && !error ? (
        <div className="row" style={{ gap: 8, color: "var(--color-muted)", fontSize: 12.5 }}>
          <Spinner /> 读取供应商
        </div>
      ) : null}

      {list && providers.length === 0 && !error ? (
        <Empty
          icon="key"
          title="还没有供应商"
          action={
            <button type="button" className="btn btn-sm btn-primary" onClick={() => setModal(null)}>
              <Icon name="plus" size={13} />
              添加供应商
            </button>
          }
        >
          一把 API Key、一个 OpenAI / Anthropic 兼容地址、一张模型清单。加好之后它就是网关的一条通道（provider/模型），在「接入」里给哪个客户端选它都行；协议不一样网关会翻译。
        </Empty>
      ) : null}

      {providers.length > 0 ? (
        <>
          <div className="status-pills" role="tablist" aria-label="供应商状态" style={{ marginBottom: 12 }}>
            <button
              type="button"
              className={`status-pill${availFilter === "all" ? " is-active" : ""}`}
              onClick={() => setAvailFilter("all")}
            >
              <span>全部</span>
              <span className="pill-n">{providers.length}</span>
            </button>
            <button
              type="button"
              className={`status-pill${availFilter === "enabled" ? " is-active" : ""}`}
              onClick={() => setAvailFilter("enabled")}
            >
              <i className="status-dot is-long_lived" />
              <span>可用</span>
              <span className="pill-n">{enabledCount}</span>
            </button>
            {disabledCount > 0 ? (
              <button
                type="button"
                className={`status-pill${availFilter === "disabled" ? " is-active" : ""}`}
                onClick={() => setAvailFilter("disabled")}
              >
                <i className="status-dot is-dead" />
                <span>已停用</span>
                <span className="pill-n">{disabledCount}</span>
              </button>
            ) : null}
          </div>

          {providers.length > 3 ? (
            <div className="prov-filters" role="tablist" aria-label="协议" style={{ marginBottom: 12 }}>
              {FILTERS.map((f) => (
                <button
                  key={f.id}
                  type="button"
                  role="tab"
                  aria-selected={filter === f.id}
                  className={`chan-chip${filter === f.id ? " is-active" : ""}`}
                  onClick={() => setFilter(f.id)}
                >
                  {f.label}
                </button>
              ))}
            </div>
          ) : null}

          <div className="accts">
            {shown.map((p) => (
              <ProviderCard
                key={p.id}
                p={p}
                state={stateOf(p.name)}
                ping={pings[p.id]}
                pinging={pinging === p.id}
                onOpen={() => setSelectedId(p.id)}
                onPing={() => void ping(p)}
              />
            ))}
          </div>

          {!shown.length ? (
            <Empty icon={null} title="这一类还没有">
              换一个协议看看。
            </Empty>
          ) : null}
        </>
      ) : null}

      {/* 详情抽屉 */}
      {selectedProvider ? (
        <ProviderDrawer
          provider={selectedProvider}
          state={stateOf(selectedProvider.name)}
          ping={pings[selectedProvider.id]}
          pinging={pinging === selectedProvider.id}
          onClose={() => setSelectedId(null)}
          onPing={() => void ping(selectedProvider)}
          onEdit={() => {
            setModal(selectedProvider);
          }}
          onRemove={() => void remove(selectedProvider)}
          onToggle={(v) => void setEnabled(selectedProvider, v)}
          onPrefer={() => void prefer(selectedProvider)}
          onConnect={() =>
            onGo(
              go("connect", {
                channel: "provider",
                model: primaryModel(selectedProvider) ? `provider/${primaryModel(selectedProvider)}` : undefined,
              }),
            )
          }
        />
      ) : null}

      {modal !== "closed" ? (
        <KeyProviderModal
          initial={modal}
          onClose={() => setModal("closed")}
          onSaved={() => {
            setModal("closed");
            void load();
          }}
        />
      ) : null}
    </>
  );
}

function healthOf(state: GatewayCandidateState | null): { tone: "ok" | "warn" | "bad" | "default"; text: string } | null {
  if (!state) return null;
  switch (state.kind) {
    case "current":
      return { tone: "ok", text: "正在用" };
    case "ready":
      return null;
    case "cooled":
      return { tone: "warn", text: `${state.models.join("、")} 冷却中` };
    case "exhausted":
      return { tone: "bad", text: `停用 ${Math.ceil(state.retryInSecs / 60)} 分钟：${state.reason}` };
    case "quota_line":
      return { tone: "bad", text: "额度到线" };
  }
}

function ProviderCard({
  p,
  state,
  ping,
  pinging,
  onOpen,
  onPing,
}: {
  p: KeyProvider;
  state: GatewayCandidateState | null;
  ping: Ping | undefined;
  pinging: boolean;
  onOpen: () => void;
  onPing: () => void;
}) {
  const isCurrent = state?.kind === "current";
  const health = p.enabled ? healthOf(state) : null;
  const host = (() => {
    try {
      return new URL(p.baseUrl).host;
    } catch {
      return p.baseUrl;
    }
  })();

  const emptyState =
    ping && !ping.ok
      ? {
          title: "连通失败",
          hint: ping.error,
          tone: "bad" as const,
        }
      : undefined;

  const footSpend = (
    <span className="qf-meta">
      <span className="mono">{host}</span>
      {p.keyTail ? <i className="qf-sep">·</i> : null}
      {p.keyTail ? <span className="mono tiny">••••{p.keyTail}</span> : null}
    </span>
  );

  const kv = (
    <>
      <div>
        <span>响应延时</span>
        <b className={ping?.ok ? "is-ok" : ""}>
          {ping?.ok ? `${ping.r.durationMs}ms` : "就绪"}
        </b>
      </div>
      <div>
        <span>声明模型</span>
        <div className="chip-row">
          {p.models.slice(0, 3).map((m) => (
            <span key={m} className="chip" title={m}>
              {m.length > 18 ? `${m.slice(0, 16)}…` : m}
            </span>
          ))}
          {p.models.length > 3 ? (
            <span className="chip" title={p.models.slice(3).join(", ")}>
              +{p.models.length - 3}
            </span>
          ) : null}
        </div>
      </div>
    </>
  );

  return (
    <GatewayCard
      logo={<Icon name="key" size={14} />}
      title={p.name}
      plan={formatLabel(p.apiFormat)}
      planClass="plan pill-key"
      current={isCurrent}
      dimmed={!p.enabled}
      tone={!p.enabled ? "none" : health?.tone === "bad" ? "bad" : health?.tone === "warn" ? "warn" : "ok"}
      compact={true}
      kv={!emptyState ? kv : undefined}
      emptyState={emptyState}
      spend={footSpend}
      stamp={p.enabled ? "就绪可用" : "已停用"}
      problem={ping && !ping.ok ? ping.error : health && health.tone !== "ok" ? health.text : null}
      refreshing={pinging}
      refreshLabel="测速"
      onOpen={onOpen}
      onRefresh={onPing}
    />
  );
}

