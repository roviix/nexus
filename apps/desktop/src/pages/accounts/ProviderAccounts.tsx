/**
 * 账号页的「供应商」：网关 `provider/` 通道背后那几家。
 *
 * 和别的页签一样，这里管的是「这条通道的号」：加一家、测一下、看它此刻在网关里是什么样子
 * （正在用 / 冷却 / 钥匙被拒停着）、临时停用。列表顺序就是同一个模型有几家都声明时的接力
 * 顺序。写进客户端在「接入」里做：客户端的通道选成供应商就走这里。
 */
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { errorText, gateway, keyProviders } from "../../ipc/api";
import type { ApiFormat, GatewayCandidateState, GatewayStatus, KeyProvider, ProviderPing } from "../../ipc/types";
import { go, type Route } from "../../shell/nav";
import { confirm } from "../../ui/confirm";
import { Banner, Empty, Icon, Spinner, Switch } from "../../ui/primitives";
import { KeyProviderModal } from "./KeyProviderModal";
import { formatLabel, primaryModel } from "./keyModel";

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
  const [modal, setModal] = useState<KeyProvider | null | "closed">("closed");
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
  const stateOf = (name: string): GatewayCandidateState | null => lane?.candidates.find((c) => c.label === name)?.state ?? null;
  const troubled = (lane?.candidates ?? []).some((c) => c.state.kind === "exhausted" || c.state.kind === "cooled");

  async function remove(p: KeyProvider) {
    const ok = await confirm(`网关里就没有「${p.name}」了；客户端的路由如果指着它的模型，会换到别家（没有别家声明就报错）。`, {
      title: `删除 ${p.name}？`,
      okLabel: "删除",
      danger: true,
    });
    if (!ok) return;
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
  const shown = filter === "all" ? providers : providers.filter((p) => p.apiFormat === filter);

  return (
    <>
      <div className="page-head acct-head">
        {tabs}
        <div className="row" style={{ gap: 8 }}>
          {troubled ? (
            <button type="button" className="btn btn-sm btn-quiet" onClick={() => void resetLane()} title="清掉冷却和停用记录，让每一家重新可选">
              清掉冷却
            </button>
          ) : null}
          <button type="button" className="btn btn-sm btn-icon btn-soft" onClick={() => void load()} title="刷新" aria-label="刷新">
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
          <p className="prov-intro">
            网关通道 <code className="mono">provider/</code>
            {status?.running ? null : " · 网关没开，接入客户端时会自动开启"}。同一个模型有几家声明时按下面的顺序接力：一家钥匙被拒、没余额、限流或连不上，下一条请求就换下一家。
          </p>
          {providers.length > 3 ? (
            <div className="prov-filters" role="tablist" aria-label="协议">
              {FILTERS.map((f) => (
                <button key={f.id} type="button" role="tab" aria-selected={filter === f.id} className={`chan-chip${filter === f.id ? " is-active" : ""}`} onClick={() => setFilter(f.id)}>
                  {f.label}
                </button>
              ))}
            </div>
          ) : null}
          <div className="prov-list">
            {shown.map((p) => (
              <ProviderCard
                key={p.id}
                p={p}
                state={stateOf(p.name)}
                ping={pings[p.id]}
                pinging={pinging === p.id}
                onPing={() => void ping(p)}
                onEdit={() => setModal(p)}
                onRemove={() => void remove(p)}
                onToggle={(v) => void setEnabled(p, v)}
                onPrefer={() => void prefer(p)}
                onConnect={() => onGo(go("connect", { channel: "provider", model: primaryModel(p) ? `provider/${primaryModel(p)}` : undefined }))}
              />
            ))}
            {!shown.length ? (
              <Empty icon={null} title="这一类还没有">
                换一个协议看看。
              </Empty>
            ) : null}
          </div>
        </>
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

/** 它此刻在网关里的样子。null = 网关还没见过它（或者网关没开）。 */
function healthOf(state: GatewayCandidateState | null): { tone: "ok" | "warn" | "bad" | "flat"; text: string } | null {
  if (!state) return null;
  switch (state.kind) {
    case "current":
      return { tone: "ok", text: "正在用" };
    case "ready":
      return null;
    case "cooled":
      return { tone: "warn", text: `${state.models.join("、")} 冷却中 · ${state.secsLeft} 秒后再试` };
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
  onPing,
  onEdit,
  onRemove,
  onToggle,
  onPrefer,
  onConnect,
}: {
  p: KeyProvider;
  state: GatewayCandidateState | null;
  ping: Ping | undefined;
  pinging: boolean;
  onPing: () => void;
  onEdit: () => void;
  onRemove: () => void;
  onToggle: (enabled: boolean) => void;
  onPrefer: () => void;
  onConnect: () => void;
}) {
  const health = p.enabled ? healthOf(state) : null;
  const shownModels = p.models.slice(0, 6);
  return (
    <article className={`card prov-card${p.enabled ? "" : " is-off"}`}>
      <div className="prov-card-main">
        <div className="prov-card-name">
          {p.name}
          <span className="role-meta">{formatLabel(p.apiFormat)}</span>
          {p.keyTail ? <span className="role-meta mono">····{p.keyTail}</span> : null}
          {health ? (
            <span className={`prov-health is-${health.tone}`}>
              <i aria-hidden />
              {health.tone === "ok" ? health.text : health.tone === "warn" ? "冷却中" : "停着"}
            </span>
          ) : null}
          {!p.enabled ? <span className="role-meta">已停用</span> : null}
        </div>
        <div className="prov-card-sub mono truncate" title={p.baseUrl}>
          {p.baseUrl}
        </div>
        <div className="prov-models">
          {shownModels.map((m) => (
            <span key={m} className="pchip mono is-static">
              {m}
            </span>
          ))}
          {p.models.length > shownModels.length ? <span className="faint tiny">+{p.models.length - shownModels.length}</span> : null}
        </div>
        {health && health.tone !== "ok" ? <p className={`prov-note is-${health.tone}`}>{health.text}</p> : null}
        {ping ? (
          ping.ok ? (
            <p className="prov-note is-ok">
              {ping.r.model} · {(ping.r.durationMs / 1000).toFixed(1)}s：{ping.r.text}
            </p>
          ) : (
            <p className="prov-note is-bad">{ping.error}</p>
          )
        ) : null}
      </div>
      <div className="prov-card-acts">
        <Switch checked={p.enabled} label={p.enabled ? "停用" : "启用"} onChange={onToggle} />
        <div className="row" style={{ gap: 4 }}>
          <button type="button" className="btn btn-sm btn-quiet" disabled={pinging} onClick={onPing} title={`直接打这家（不经过网关），用 ${p.models[0] ?? "第一个模型"} 说一句`}>
            {pinging ? <Spinner /> : null}
            测一下
          </button>
          {p.enabled && state && state.kind !== "current" ? (
            <button type="button" className="btn btn-sm btn-quiet" onClick={onPrefer} title="它声明的模型都先找它">
              优先用它
            </button>
          ) : null}
          <button type="button" className="btn btn-sm btn-quiet" onClick={onEdit}>
            编辑
          </button>
          <button type="button" className="btn btn-sm btn-icon btn-quiet" aria-label={`删除 ${p.name}`} onClick={onRemove}>
            <Icon name="trash" size={13} />
          </button>
          <button type="button" className="btn btn-sm" disabled={!p.enabled} onClick={onConnect}>
            接入
            <Icon name="chevron" size={12} />
          </button>
        </div>
      </div>
    </article>
  );
}
