/**
 * 供应商详情抽屉。
 *
 * 现代化、克制、精致质感（完全对标 Cursor 与 Antigravity 抽屉）：
 * - 供应商身份、协议格式与健康状态；
 * - 网关调度控制（优先使用、启停开关）；
 * - 连通性测试（直接测试指定模型、时延与返回样本）；
 * - 端点地址与密钥管理；
 * - 声明模型全量清单（搜索筛选、一键复制）；
 * - 页脚删除供应商（安全二次确认）。
 */
import { useMemo, useState } from "react";
import type { GatewayCandidateState, KeyProvider, ProviderPing } from "../../ipc/types";
import { CopyButton, Drawer, Health, Icon, Spinner } from "../../ui/primitives";
import { formatLabel } from "./keyModel";
import { DeleteAccount } from "./AccountDrawer";
import { GatewayLane } from "./GatewayCard";

type Ping = { ok: true; r: ProviderPing } | { ok: false; error: string };

export function ProviderDrawer({
  provider: p,
  state,
  ping,
  pinging,
  onClose,
  onPing,
  onEdit,
  onRemove,
  onToggle,
  onPrefer: _onPrefer,
  onConnect,
}: {
  provider: KeyProvider;
  state: GatewayCandidateState | null;
  ping: Ping | undefined;
  pinging: boolean;
  onClose: () => void;
  onPing: () => void;
  onEdit: () => void;
  onRemove: () => void;
  onToggle: (enabled: boolean) => void;
  onPrefer: () => void;
  onConnect?: () => void;
}) {
  const [query, setQuery] = useState("");

  const isCurrent = state?.kind === "current";
  const health = p.enabled ? healthOf(state) : null;

  const filteredModels = useMemo(() => {
    if (!query.trim()) return p.models;
    const q = query.trim().toLowerCase();
    return p.models.filter((m) => m.toLowerCase().includes(q));
  }, [p.models, query]);

  const head = (
    <div className="dr-id">
      <div className="dr-title">
        <Icon name="key" size={18} />
        <span className="dr-email selectable truncate" title={p.name}>
          {p.name}
        </span>
        <CopyButton value={p.name} icon label="复制名称" />
      </div>

      <div className="dr-badges">
        <span className="plan pill-key">{formatLabel(p.apiFormat)}</span>
        {p.keyTail ? <span className="pill pill-quiet mono">••••{p.keyTail}</span> : null}
        <span className="dr-meta">
          {!p.enabled ? (
            <Health tone="warn">已停用</Health>
          ) : health?.tone === "warn" ? (
            <Health tone="warn">{health.text}</Health>
          ) : health?.tone === "bad" ? (
            <Health tone="bad">{health.text}</Health>
          ) : (
            <Health tone="ok">就绪可用</Health>
          )}
          <span>{p.models.length} 个模型</span>
        </span>
      </div>

      <div className="dr-actions">
        <button
          type="button"
          className="btn btn-sm btn-icon btn-soft"
          onClick={onPing}
          disabled={pinging}
          title="发起连通性测试"
          aria-label="测速"
        >
          {pinging ? <Spinner /> : <Icon name="refresh" size={13} />}
        </button>

        <button type="button" className="btn btn-sm btn-quiet" onClick={onEdit}>
          <Icon name="pencil" size={12} />
          编辑
        </button>
        {onConnect ? (
          <button type="button" className="btn btn-sm btn-quiet" onClick={onConnect}>
            接入
          </button>
        ) : null}
      </div>
    </div>
  );

  const foot = (
    <footer className="dr-foot">
      <DeleteAccount
        onConfirm={async () => {
          onRemove();
          onClose();
        }}
        warning="将从网关配置中彻底删除该供应商及其全部模型路由。"
      />
      <span className="faint tiny">
        {p.models.length} 个已声明模型
      </span>
    </footer>
  );

  return (
    <Drawer label={`供应商详情 ${p.name}`} onClose={onClose} head={head} footer={foot}>
      <div className="dr-body">
        {/* 网关调度状态 */}
        <GatewayLane
          enabled={p.enabled}
          current={isCurrent}
          k="网关路由"
          idle="已停用"
          active="已启用"
          join="启用"
          leave="停用"
          onToggle={onToggle}
        />

        {/* 连通性测试 */}
        <div className="dr-panel">
          <div className="row" style={{ justifyContent: "space-between", alignItems: "center" }}>
            <span className="strong small">连通性测试</span>
            <button
              type="button"
              className="btn btn-sm btn-quiet"
              disabled={pinging}
              onClick={onPing}
            >
              {pinging ? <Spinner /> : <Icon name="refresh" size={12} />}
              立即测速
            </button>
          </div>

          {ping ? (
            ping.ok ? (
              <div className="stack" style={{ gap: 6, padding: "8px 10px", borderRadius: 8, background: "var(--color-canvas)" }}>
                <div className="row" style={{ justifyContent: "space-between", fontSize: 12 }}>
                  <span className="strong" style={{ color: "var(--color-ok)" }}>
                    ✓ 连通正常 ({(ping.r.durationMs / 1000).toFixed(2)}s)
                  </span>
                  <span className="faint tiny mono">{ping.r.model}</span>
                </div>
                <div className="faint tiny truncate" title={ping.r.text}>
                  “{ping.r.text}”
                </div>
              </div>
            ) : (
              <div className="acct-problem" style={{ margin: 0 }}>
                <Icon name="alert" size={12} />
                <span className="tiny">{ping.error}</span>
              </div>
            )
          ) : (
            <p className="faint tiny" style={{ margin: 0 }}>
              点击测试可直接向该供应商发起一次轻量对话测试，测量端到端网络时延与响应样本。
            </p>
          )}
        </div>

        {/* 接口端点与凭证 */}
        <div className="dr-panel">
          <div className="strong small">接口端点与凭证</div>

          <div className="dr-row">
            <span className="k">接口地址</span>
            <span className="v row" style={{ gap: 6, alignItems: "center", maxWidth: "70%" }}>
              <code className="tiny mono truncate" title={p.baseUrl}>{p.baseUrl}</code>
              <CopyButton value={p.baseUrl} label="复制地址" icon />
            </span>
          </div>

          <div className="dr-row">
            <span className="k">协议格式</span>
            <span className="v">{formatLabel(p.apiFormat)}</span>
          </div>

          <div className="dr-row">
            <span className="k">API Key</span>
            <code className="v tiny mono">{p.keyTail ? `••••${p.keyTail}` : "未配置"}</code>
          </div>
        </div>

        {/* 声明模型清单 */}
        <div className="dr-panel">
          <div className="row" style={{ justifyContent: "space-between", alignItems: "center" }}>
            <span className="strong small">声明支持模型 ({p.models.length})</span>
          </div>

          {p.models.length > 5 ? (
            <label className="search" style={{ height: 28 }}>
              <Icon name="search" size={13} />
              <input
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="筛选模型..."
                style={{ fontSize: 12 }}
              />
              {query ? (
                <button
                  type="button"
                  className="search-clear"
                  onClick={() => setQuery("")}
                  aria-label="清空"
                >
                  <Icon name="close" size={11} />
                </button>
              ) : null}
            </label>
          ) : null}

          <div className="row" style={{ flexWrap: "wrap", gap: 6, maxHeight: 220, overflowY: "auto" }}>
            {filteredModels.map((m) => (
              <span
                key={m}
                className="pchip mono"
                style={{ cursor: "pointer", fontSize: 11.5 }}
                title="点击复制模型名"
                onClick={() => {
                  navigator.clipboard.writeText(m).catch(() => {});
                }}
              >
                {m}
              </span>
            ))}
            {filteredModels.length === 0 ? (
              <span className="faint tiny" style={{ padding: "8px 0" }}>没有匹配的模型</span>
            ) : null}
          </div>
        </div>
      </div>
    </Drawer>
  );
}

function healthOf(state: GatewayCandidateState | null): { tone: "ok" | "warn" | "bad" | "flat"; text: string } | null {
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
