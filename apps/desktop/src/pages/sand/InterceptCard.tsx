/**
 * IDE 面板拦截 —— Sand 页的一张卡。
 *
 * 它长在 Sand 页而不是网关页，因为它拦的是**Sand 补丁改道过来的 Agent 面板流量**：打了补丁、开了
 * 「推理经本机网关」的 Cursor，每一次模型调用先到透传口，才有东西可拦。网关只是它借的那条管子。
 * 数据来源是透传口上唯一「看懂内容」的那条路径（Rust `nexus_gateway::intercept`）。这张卡回答三件事：
 *
 *  1. 有没有流量 —— 本次进程内拦了几次、改写了几次、失败几次；没有就说清楚该去哪开。
 *  2. 改写规则 —— 现阶段只有一个「哨兵」：往目标 user 消息塞一段固定文本，用来证明改写到达了模型
 *     （在 Agent 面板让模型复述它；复述得出 + 下面某一行标着「已改写」，两个证据对上才算通）。
 *  3. 最近的请求 —— 会话、模型 → 实际路由、消息条数、token、耗时、有没有改写。**没有对话内容**。
 *
 * 用量这块和「本地用量」分开：那边是标准 API 客户端的请求，这边是 Cursor 自己每一轮的模型调用，
 * 一轮上下文几十万 token 是常态，混在一起两边都看不懂。
 */
import { useEffect, useState } from "react";
import { gateway } from "../../ipc/api";
import type {
  GatewayStatus,
  InterceptRecord,
  MarkerPosition,
  RewriteRule,
  UsageSummary,
} from "../../ipc/types";
import { go, type Route } from "../../shell/nav";
import { timeAgo, timeUntil } from "../../ui/format";
import { Banner, Empty, ErrorNote, Icon, Opt, Switch, Tag } from "../../ui/primitives";

const POSITION_LABEL: Record<MarkerPosition, string> = {
  tail: "最后一条 user 消息末尾",
  head: "第一条 user 消息开头",
};

/** 表里最多摆几行；再多用「展开」。 */
const RECENT_FOLD = 8;

/** token 数：几十万一行是常态，全写出来一行放不下。 */
export function fmtTokens(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 10_000) return `${Math.round(n / 1000)}k`;
  return n.toLocaleString();
}

/** 会话 id 只露前 8 位：够在列表里认出「同一个会话」，不占一整行。 */
export function shortConversation(id: string | null): string {
  if (!id) return "—";
  return id.length > 8 ? id.slice(0, 8) : id;
}

export function InterceptCard({
  status,
  onChanged,
  onGo,
}: {
  status: GatewayStatus;
  /** 规则改完之后让父级重新拉一次网关状态。 */
  onChanged: () => Promise<void> | void;
  onGo: (r: Route) => void;
}) {
  const snap = status.intercept;
  const saved = snap.rule;
  const [draft, setDraft] = useState<RewriteRule>(saved);
  const [usage, setUsage] = useState<UsageSummary | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  // 保存值变了（别处改的、或刚保存完）草稿跟上；用户正改着的那份不会被 5 秒一次的刷新冲掉。
  useEffect(() => {
    setDraft(saved);
  }, [saved.enabled, saved.position, saved.marker]); // eslint-disable-line react-hooks/exhaustive-deps

  // 每次状态刷新顺带刷一次账：一次 Stream 结束才落一行，这个频率够了。
  useEffect(() => {
    let alive = true;
    gateway
      .ideUsage(7)
      .then((u) => alive && setUsage(u))
      .catch(() => alive && setUsage(null));
    return () => {
      alive = false;
    };
  }, [snap.calls]);

  async function onApply(rule: RewriteRule) {
    setBusy(true);
    setError(null);
    try {
      await gateway.setIntercept(rule);
      await onChanged();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  async function onGrokbot(on: boolean) {
    setBusy(true);
    setError(null);
    try {
      await gateway.setGrokbotStream(on);
      await onChanged();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const grok = status.grokbotStream;
  const grokCred = grok.credential;
  const grokCredOk = !!grokCred && (!grokCred.expired || grokCred.canRenew);

  const dirty = draft.position !== saved.position || draft.marker !== saved.marker;
  const markerEmpty = draft.marker.trim().length === 0;
  const hasTraffic = snap.calls > 0 || (usage?.window.calls ?? 0) > 0;
  const recent = expanded ? snap.recent : snap.recent.slice(0, RECENT_FOLD);

  return (
    <div className="card">
      <div className="section-head" style={{ margin: "0 0 12px" }}>
        <div className="row" style={{ gap: 8, alignItems: "baseline" }}>
          <strong>IDE 面板拦截</strong>
          <span className="faint tiny">
            {snap.calls > 0
              ? `本次 ${snap.calls} 次${snap.rewritten > 0 ? ` · 改写 ${snap.rewritten}` : ""}${snap.errors > 0 ? ` · 失败 ${snap.errors}` : ""}`
              : "Agent 面板经本机网关的模型调用"}
          </span>
        </div>
        <button type="button" className="btn btn-sm btn-quiet" onClick={() => onGo(go("gateway"))}>
          <Icon name="gateway" size={13} />
          本地网关
        </button>
      </div>

      <ErrorNote error={error} />

      {saved.enabled ? (
        <div style={{ marginBottom: 12 }}>
          <Banner tone="warn" title="上下文改写开着：Agent 面板的每一轮都会插入哨兵。" hint="测试完记得关。" />
        </div>
      ) : null}

      <div className="opts">
        <Opt
          icon="shield"
          title="Bot 通道"
          desc="经网关的 Agent 面板请求用 Grok Bot 的额度；选 GLM 5.2 会改走 premium，其它模型原样，下表「实际」是服务端落到的模型"
          hint={grok.enabled && !grokCredOk ? "还没选用哪个号：到账号页打开该账号的「Grok Bot」页选一个，或关掉。" : undefined}
          tone={grok.enabled ? (grokCredOk ? "on" : "warn") : undefined}
        >
          {grok.enabled ? (
            grokCredOk ? (
              <Tag tone="ok">{grokCred?.accountEmail ?? "已就绪"} · {timeUntil(grokCred?.expiresAtMs)}</Tag>
            ) : (
              <Tag tone="bad">未选号</Tag>
            )
          ) : null}
          <Switch checked={grok.enabled} disabled={busy} onChange={(next) => void onGrokbot(next)} label="Bot 通道" />
        </Opt>
        <Opt icon="shield" title="上下文改写（测试）" desc="往目标 user 消息里插入下面这段文本。默认关" tone={saved.enabled ? "warn" : undefined}>
          <Switch
            checked={saved.enabled}
            disabled={busy || (!saved.enabled && markerEmpty)}
            onChange={(next) => void onApply({ ...draft, enabled: next })}
            label="上下文改写"
          />
        </Opt>
        <Opt icon="list" title="插入位置">
          <select
            className="input"
            style={{ width: "auto" }}
            aria-label="哨兵位置"
            value={draft.position}
            disabled={busy}
            onChange={(e) => setDraft({ ...draft, position: e.target.value as MarkerPosition })}
          >
            {(Object.keys(POSITION_LABEL) as MarkerPosition[]).map((p) => (
              <option key={p} value={p}>
                {POSITION_LABEL[p]}
              </option>
            ))}
          </select>
        </Opt>
        <Opt icon="info" title="哨兵文本" desc="原样插入，不加分隔符">
          <input
            className="input mono"
            style={{ width: 220 }}
            aria-label="哨兵文本"
            value={draft.marker}
            disabled={busy}
            maxLength={2000}
            placeholder="[nexus-mark]"
            onChange={(e) => setDraft({ ...draft, marker: e.target.value })}
          />
          {dirty ? (
            <button
              type="button"
              className="btn btn-sm btn-primary"
              disabled={busy || (saved.enabled && markerEmpty)}
              onClick={() => void onApply({ ...draft, enabled: saved.enabled })}
            >
              应用
            </button>
          ) : null}
        </Opt>
      </div>

      {usage && usage.window.calls > 0 ? (
        <div className="row faint tiny" style={{ marginTop: 12, gap: 14, flexWrap: "wrap" }}>
          <span>
            今天 <b className="mono">{usage.today.calls}</b> 次 · 输入 {fmtTokens(usage.today.inputTokens)} · 输出 {fmtTokens(usage.today.outputTokens)} · 缓存读 {fmtTokens(usage.today.cacheReadTokens)}
          </span>
          <span>
            7 天 <b className="mono">{usage.window.calls}</b> 次 · 输入 {fmtTokens(usage.window.inputTokens)} · 输出 {fmtTokens(usage.window.outputTokens)}
            {usage.window.errors > 0 ? ` · 失败 ${usage.window.errors}` : ""}
          </span>
        </div>
      ) : null}

      <div style={{ marginTop: 12 }}>
        {!hasTraffic ? (
          <Empty title="还没有 Agent 面板的流量经过">
            {status.running
              ? "打开上面的「推理经本机网关」并重新安装补丁后，Agent 面板的模型调用会从这里经过。"
              : "先到「本地网关」开启网关，再打开「推理经本机网关」。"}
          </Empty>
        ) : snap.recent.length === 0 ? (
          <p className="faint tiny" style={{ margin: 0 }}>
            本次启动后还没有新请求。
          </p>
        ) : (
          <>
            <table className="table">
              <thead>
                <tr>
                  <th>时间</th>
                  <th>会话</th>
                  <th>模型 → 实际</th>
                  <th className="n">消息</th>
                  <th className="n">输入 / 输出</th>
                  <th className="n">耗时</th>
                  <th />
                </tr>
              </thead>
              <tbody>
                {recent.map((r, i) => (
                  <RecentRow key={`${r.at}-${i}`} r={r} />
                ))}
              </tbody>
            </table>
            {snap.recent.length > RECENT_FOLD ? (
              <div className="row" style={{ justifyContent: "flex-end", marginTop: 6 }}>
                <button type="button" className="btn btn-sm btn-quiet" onClick={() => setExpanded((v) => !v)}>
                  {expanded ? "收起" : `展开全部 ${snap.recent.length} 条`}
                </button>
              </div>
            ) : null}
          </>
        )}
      </div>
    </div>
  );
}

function RecentRow({ r }: { r: InterceptRecord }) {
  const routedDiffers = !!r.routed && r.routed !== r.model;
  return (
    <tr>
      <td className="dim" title={r.at}>
        {timeAgo(r.at)}
      </td>
      <td className="mono" title={r.conversationId ?? undefined}>
        {shortConversation(r.conversationId)}
      </td>
      <td title={routedDiffers ? `请求 ${r.model} → 实际 ${r.routed}` : r.model}>
        <span className="mono">{r.model}</span>
        {routedDiffers ? <span className="dim"> → {r.routed}</span> : null}
      </td>
      <td className="n">{r.messageCount}</td>
      <td className="n" title={r.measured ? `缓存读 ${r.cacheReadTokens.toLocaleString()} · 缓存写 ${r.cacheWriteTokens.toLocaleString()}` : "上游没报用量"}>
        {r.measured ? (
          <>
            {fmtTokens(r.inputTokens)} / {fmtTokens(r.outputTokens)}
          </>
        ) : (
          <span className="dim">—</span>
        )}
      </td>
      <td className="n">{r.durationMs >= 1000 ? `${(r.durationMs / 1000).toFixed(1)}s` : `${r.durationMs}ms`}</td>
      <td>
        <span className="row" style={{ gap: 4, justifyContent: "flex-end" }}>
          {r.rewritten ? <Tag tone="warn">已改写</Tag> : null}
          {!r.ok ? (
            <span title={r.error ?? undefined}>
              <Tag tone="bad">{r.kind ?? `HTTP ${r.status}`}</Tag>
            </span>
          ) : null}
        </span>
      </td>
    </tr>
  );
}
