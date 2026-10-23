import { useMemo, useState } from "react";
import type { UsageRecent } from "../../ipc/types";
import { formatModelName } from "../../ui/models";
import { timeAgo } from "../../ui/format";
import { compactNumber, fmtMs } from "../../ui/traffic";
import { Icon, Modal } from "../../ui/primitives";
import { ShellIcon } from "../../shell/ShellIcon";

export function RequestStreamTable({
  requests,
}: {
  requests: UsageRecent[];
}) {
  const [statusFilter, setStatusFilter] = useState<"all" | "ok" | "err">("all");
  const [search, setSearch] = useState("");
  const [inspecting, setInspecting] = useState<UsageRecent | null>(null);

  const okCount = useMemo(() => requests.filter((r) => r.ok).length, [requests]);
  const errCount = useMemo(() => requests.filter((r) => !r.ok).length, [requests]);

  const filtered = useMemo(() => {
    return requests.filter((r) => {
      if (statusFilter === "ok" && !r.ok) return false;
      if (statusFilter === "err" && r.ok) return false;
      if (search.trim()) {
        const q = search.trim().toLowerCase();
        const m = (r.routed ?? r.model).toLowerCase();
        const ch = (r.channel ?? "").toLowerCase();
        const acc = (r.account ?? "").toLowerCase();
        if (!m.includes(q) && !ch.includes(q) && !acc.includes(q)) return false;
      }
      return true;
    });
  }, [requests, statusFilter, search]);

  return (
    <section className="card card-flush rst-card" aria-label="最近请求记录">
      {/* 头部：标题、实时状态、筛选器 */}
      <header className="rst-header">
        <div className="rst-title-group">
          <div className="rst-icon-box">
            <span className="rst-bolt">⚡</span>
          </div>
          <div className="rst-title-row">
            <h3 className="rst-title">最近请求记录</h3>
            <span className="rst-live-tag">
              <span className="pod-dot is-live" />
              <span>实时捕获</span>
            </span>
            <span className="pill tiny mono rst-count-pill">{requests.length} 笔记录</span>
          </div>
        </div>

        <div className="rst-controls">
          <div className="seg seg-sm rst-seg">
            <button
              type="button"
              className={`seg-btn${statusFilter === "all" ? " is-on" : ""}`}
              onClick={() => setStatusFilter("all")}
            >
              全部 ({requests.length})
            </button>
            <button
              type="button"
              className={`seg-btn${statusFilter === "ok" ? " is-on" : ""}`}
              onClick={() => setStatusFilter("ok")}
            >
              成功 ({okCount})
            </button>
            <button
              type="button"
              className={`seg-btn${statusFilter === "err" ? " is-on" : ""}`}
              onClick={() => setStatusFilter("err")}
            >
              异常 ({errCount})
            </button>
          </div>

          <label className="search rst-search">
            <Icon name="search" size={12} />
            <input
              value={search}
              placeholder="过滤模型、通道或账号…"
              onChange={(e) => setSearch(e.target.value)}
            />
            {search ? (
              <button
                type="button"
                className="search-clear"
                aria-label="清空"
                onClick={() => setSearch("")}
              >
                <Icon name="close" size={11} />
              </button>
            ) : null}
          </label>
        </div>
      </header>

      {/* 表格主体 */}
      <div className="rst-table-wrap">
        {filtered.length === 0 ? (
          <div className="rst-empty">
            <div className="rst-empty-icon">
              <ShellIcon name="gateway" size={24} />
            </div>
            <p className="rst-empty-title">
              {requests.length === 0 ? "暂无网关请求记录" : "未找到匹配的请求记录"}
            </p>
            <p className="rst-empty-sub">
              {requests.length === 0
                ? "启动本地网关并使用 Claude Code、Codex 或游乐场发起推理对话，每一笔请求明细均将实时在此审计呈现。"
                : "尝试调整状态筛选或搜索关键词。"}
            </p>
          </div>
        ) : (
          <div className="rst-table">
            <div className="rst-thead">
              <div className="rst-th th-time">时刻</div>
              <div className="rst-th th-status">状态</div>
              <div className="rst-th th-model">请求模型与路由</div>
              <div className="rst-th th-tokens">Token 吞吐</div>
              <div className="rst-th th-latency">链路延迟</div>
              <div className="rst-th th-account">承载账号</div>
              <div className="rst-th th-action"></div>
            </div>

            <div className="rst-tbody">
              {filtered.map((r, idx) => {
                const totalTokens = r.inputTokens + r.outputTokens;
                const validRouted = r.routed && r.routed.toLowerCase() !== r.channel?.toLowerCase() ? r.routed : undefined;
                const modelDisplay = formatModelName(validRouted ?? r.model);
                const statusTone = r.ok ? "ok" : r.status === 429 ? "warn" : "err";

                return (
                  <div
                    key={`${r.at}-${idx}`}
                    className="rst-tr"
                    onClick={() => setInspecting(r)}
                    role="button"
                    tabIndex={0}
                    title="点击查看请求详情"
                  >
                    {/* 1. 时刻 */}
                    <div className="rst-td td-time" title={r.at}>
                      <span className="mono rst-time-text">{timeAgo(r.at)}</span>
                    </div>

                    {/* 2. 状态 */}
                    <div className="rst-td td-status">
                      <span className={`rst-status-badge is-${statusTone}`}>
                        <span className={`rst-status-dot is-${statusTone}`} />
                        <span className="mono">{r.status || (r.ok ? "200" : "ERR")}</span>
                      </span>
                    </div>

                    {/* 3. 模型与路由 */}
                    <div className="rst-td td-model truncate">
                      <div className="rst-model-row truncate">
                        <span className="rst-model-name truncate" title={validRouted ? `${r.model} → ${validRouted}` : r.model}>
                          {modelDisplay}
                        </span>
                        {r.channel ? (
                          <span className="rst-channel-pill mono">
                            {r.channel}/*
                          </span>
                        ) : null}
                        {r.kind ? (
                          <span className="rst-kind-tag mono">{r.kind}</span>
                        ) : null}
                      </div>
                    </div>

                    {/* 4. Token 吞吐 */}
                    <div className="rst-td td-tokens">
                      <div className="rst-token-row">
                        <span className="mono rst-token-total">{compactNumber(totalTokens)} tok</span>
                        <span className="rst-token-detail faint mono">
                          入 {compactNumber(r.inputTokens)} · 出 {compactNumber(r.outputTokens)}
                        </span>
                      </div>
                    </div>

                    {/* 5. 链路耗时 */}
                    <div className="rst-td td-latency">
                      <div className="rst-latency-row">
                        <span className="mono rst-dur-text">{fmtMs(r.durationMs)}</span>
                        {r.ttftMs != null ? (
                          <span className="rst-ttft-text faint mono" title="首字时延 (Time to First Token)">
                            首字 {fmtMs(r.ttftMs)}
                          </span>
                        ) : null}
                      </div>
                    </div>

                    {/* 6. 承载账号 */}
                    <div className="rst-td td-account truncate">
                      <span className="rst-acc-name truncate faint mono" title={r.account || "号池自动指派"}>
                        {r.account || "通用号池"}
                      </span>
                    </div>

                    {/* 7. 查看图标 */}
                    <div className="rst-td td-action">
                      <span className="rst-row-inspect-btn">
                        <Icon name="search" size={12} />
                      </span>
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>

      {/* 精致克制的弹窗展示请求详情 */}
      {inspecting ? (() => {
        const modalRouted = inspecting.routed && inspecting.routed.toLowerCase() !== inspecting.channel?.toLowerCase() ? inspecting.routed : undefined;
        const modalTitle = formatModelName(modalRouted ?? inspecting.model);
        return (
        <Modal onClose={() => setInspecting(null)} title="请求审计详情">
          <div className="rst-modal-content">
            <div className="rst-modal-status-bar">
              <span className={`rst-status-badge is-${inspecting.ok ? "ok" : inspecting.status === 429 ? "warn" : "err"}`}>
                <span className={`rst-status-dot is-${inspecting.ok ? "ok" : inspecting.status === 429 ? "warn" : "err"}`} />
                <span className="mono">{inspecting.status || (inspecting.ok ? "200" : "ERR")}</span>
              </span>
              <span className="rst-modal-model-title mono">{modalTitle}</span>
              <span className="rst-modal-time mono faint">{timeAgo(inspecting.at)}</span>
            </div>

            <div className="rst-modal-grid">
              <div className="rst-modal-field">
                <span className="diag-k">请求模型</span>
                <span className="diag-v mono">{inspecting.model}</span>
              </div>
              {modalRouted && modalRouted !== inspecting.model ? (
                <div className="rst-modal-field">
                  <span className="diag-k">实际路由</span>
                  <span className="diag-v mono">{modalRouted}</span>
                </div>
              ) : null}
              <div className="rst-modal-field">
                <span className="diag-k">路由通道</span>
                <span className="diag-v mono">{inspecting.channel || "默认通道"}</span>
              </div>
              <div className="rst-modal-field">
                <span className="diag-k">承载凭证</span>
                <span className="diag-v mono">{inspecting.account || "通用号池"}</span>
              </div>
              <div className="rst-modal-field">
                <span className="diag-k">输入 Tokens</span>
                <span className="diag-v mono">{inspecting.inputTokens.toLocaleString()}</span>
              </div>
              <div className="rst-modal-field">
                <span className="diag-k">输出 Tokens</span>
                <span className="diag-v mono">{inspecting.outputTokens.toLocaleString()}</span>
              </div>
              <div className="rst-modal-field">
                <span className="diag-k">首字时延 (TTFT)</span>
                <span className="diag-v mono">{inspecting.ttftMs != null ? `${inspecting.ttftMs}ms` : "—"}</span>
              </div>
              <div className="rst-modal-field">
                <span className="diag-k">总耗时</span>
                <span className="diag-v mono">{inspecting.durationMs}ms</span>
              </div>
              <div className="rst-modal-field full-row">
                <span className="diag-k">请求模式</span>
                <span className="diag-v mono">{inspecting.kind || "chat completions"}</span>
              </div>
              <div className="rst-modal-field full-row">
                <span className="diag-k">精确发起时刻 (ISO)</span>
                <span className="diag-v mono faint">{inspecting.at}</span>
              </div>
            </div>

            <div className="rst-modal-footer">
              <button
                type="button"
                className="btn btn-sm btn-primary"
                onClick={() => setInspecting(null)}
              >
                关闭
              </button>
            </div>
          </div>
        </Modal>
        );
      })() : null}
    </section>
  );
}
