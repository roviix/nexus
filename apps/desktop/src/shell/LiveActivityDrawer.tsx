import { useEffect, useState } from "react";
import { gateway as gatewayApi } from "../ipc/api";
import type { UsageRecent } from "../ipc/types";
import { timeAgo } from "../ui/format";
import { Icon } from "../ui/primitives";

export function LiveActivityDrawer({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const [items, setItems] = useState<UsageRecent[]>([]);
  const [loading, setLoading] = useState(false);
  const [filter, setFilter] = useState<"all" | "ok" | "err">("all");

  useEffect(() => {
    if (!open) return;
    let isMounted = true;
    async function fetchRecent() {
      try {
        setLoading(true);
        const u = await gatewayApi.usage(1);
        if (isMounted) {
          setItems(u.recent ?? []);
        }
      } catch {} finally {
        if (isMounted) setLoading(false);
      }
    }
    void fetchRecent();
    const interval = setInterval(fetchRecent, 3000);
    return () => {
      isMounted = false;
      clearInterval(interval);
    };
  }, [open]);

  if (!open) return null;

  const filtered = items.filter((item) => {
    if (filter === "ok") return item.ok;
    if (filter === "err") return !item.ok;
    return true;
  });

  return (
    <div className="activity-drawer-scrim" onClick={onClose}>
      <aside
        className="activity-drawer"
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-label="实时请求流"
      >
        <div className="act-drawer-head">
          <div className="act-drawer-title-wrap">
            <span className="pod-dot is-live" />
            <h2 className="act-drawer-title">实时请求流 (Live Activity)</h2>
          </div>
          <button
            type="button"
            className="btn btn-sm btn-icon btn-quiet"
            onClick={onClose}
            aria-label="关闭抽屉"
          >
            <Icon name="close" size={14} />
          </button>
        </div>

        <div className="act-drawer-filter-bar">
          <div className="seg seg-sm">
            <button
              type="button"
              className={`seg-btn${filter === "all" ? " is-on" : ""}`}
              onClick={() => setFilter("all")}
            >
              全部 ({items.length})
            </button>
            <button
              type="button"
              className={`seg-btn${filter === "ok" ? " is-on" : ""}`}
              onClick={() => setFilter("ok")}
            >
              成功 ({items.filter((i) => i.ok).length})
            </button>
            <button
              type="button"
              className={`seg-btn${filter === "err" ? " is-on" : ""}`}
              onClick={() => setFilter("err")}
            >
              异常 ({items.filter((i) => !i.ok).length})
            </button>
          </div>
          {loading ? <span className="act-pulse-text">更新中…</span> : null}
        </div>

        <div className="act-drawer-body">
          {filtered.length === 0 ? (
            <div className="act-drawer-empty">
              <Icon name="list" size={24} style={{ opacity: 0.3, marginBottom: 8 }} />
              <div>暂无请求流记录</div>
              <div className="act-drawer-empty-hint">
                当客户端（Claude Code / Codex）或游乐场通过本地网关发出调用时，请求将在此实时滚动呈现。
              </div>
            </div>
          ) : (
            <div className="act-stream-list">
              {filtered.map((item, index) => {
                const time = item.at ? new Date(item.at).toLocaleTimeString() : "";
                const isSuccess = item.ok;
                const statusTone = isSuccess ? "ok" : "err";
                const client = item.kind || "Client";
                const channel = item.channel || "default";
                const acc = item.account ? item.account.replace(/^(.)(.*)(@.*)$/, "$1...$3") : "匿名凭证";

                return (
                  <div key={`${item.at}-${index}`} className="act-stream-card">
                    <div className="act-stream-top">
                      <span className="mono act-time">{time || timeAgo(item.at)}</span>
                      <span className={`pill tiny act-status-${statusTone}`}>
                        {item.status ? `${item.status} ${isSuccess ? "OK" : "ERR"}` : isSuccess ? "200 OK" : "FAILED"}
                      </span>
                      <span className="mono act-ms">{item.durationMs}ms</span>
                    </div>

                    <div className="act-stream-pipeline">
                      <span className="act-pipeline-client">{client}</span>
                      <span className="act-pipeline-arrow">→</span>
                      <span className="act-pipeline-channel">
                        通道: <b>{channel}</b>
                      </span>
                      <span className="act-pipeline-arrow">→</span>
                      <span className="act-pipeline-model mono">{item.model}</span>
                    </div>

                    <div className="act-stream-foot">
                      <span className="act-hit-acc">命中账号: {acc}</span>
                      {item.inputTokens || item.outputTokens ? (
                        <span className="act-tokens mono">
                          {item.inputTokens} in / {item.outputTokens} out
                        </span>
                      ) : null}
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </aside>
    </div>
  );
}
