/**
 * 网关页的「最近请求」：一行一个请求（换号 / 换一家重来的每一次尝试各一行），新的在上。
 *
 * 客户端里只看得到一句「出错了」；这里能看到它从哪个客户端进来、按路由换成了哪个模型、
 * 落到哪条通道哪个号、上游回的原话。失败的行点开看原文。明细只在内存里，重启就清空。
 */
import { useState } from "react";
import type { RequestLogEntry } from "../../ipc/types";
import { Empty, Icon } from "../../ui/primitives";

const CLIENT_LABEL: Record<string, string> = {
  claude: "Claude Code",
  codex: "Codex",
  opencode: "OpenCode",
  grok: "Grok CLI",
};

function clock(at: string): string {
  const d = new Date(at);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleTimeString("zh-CN", { hour12: false });
}

function tokens(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(n >= 10_000 ? 0 : 1)}K` : String(n);
}

export function RequestLog({
  entries,
  channelLabel,
  onClear,
}: {
  entries: RequestLogEntry[] | null;
  channelLabel: (id: string) => string;
  onClear: () => void;
}) {
  const [onlyErrors, setOnlyErrors] = useState(false);
  const [open, setOpen] = useState<number | null>(null);
  const shown = (entries ?? []).filter((e) => !onlyErrors || !e.ok);
  const failed = (entries ?? []).filter((e) => !e.ok).length;

  return (
    <div className="card card-flush gw-log">
      <div className="gw-chans-head gw-log-head">
        <div className="row" style={{ gap: 8, alignItems: "baseline" }}>
          <strong>最近请求</strong>
          <span className="faint tiny">每次尝试一行 · 只留在内存里，重启清空</span>
        </div>
        <div className="row" style={{ gap: 6 }}>
          {failed ? (
            <button type="button" className="chip" aria-pressed={onlyErrors} onClick={() => setOnlyErrors((v) => !v)}>
              只看失败 {failed}
            </button>
          ) : null}
          {entries?.length ? (
            <button type="button" className="btn btn-sm btn-quiet" onClick={onClear}>
              清空
            </button>
          ) : null}
        </div>
      </div>
      {entries == null ? (
        <div className="skeleton" style={{ height: 96, margin: 14 }} />
      ) : shown.length === 0 ? (
        <div style={{ padding: 14 }}>
          <Empty icon={null} title={onlyErrors ? "没有失败的请求" : "还没有请求"}>
            {onlyErrors ? "最近的都通了。" : "客户端发来的每一次请求都会在这里留一行：走了哪条通道、哪个号、花了多久、失败时上游说了什么。"}
          </Empty>
        </div>
      ) : (
        <div className="gw-log-list">
          {shown.slice(0, 60).map((e) => {
            const expanded = open === e.id;
            const model = e.target ?? e.requested;
            return (
              <div key={e.id} className={`gw-log-row${e.ok ? "" : " is-bad"}${expanded ? " is-open" : ""}`}>
                <button type="button" className="gw-log-main" onClick={() => setOpen(expanded ? null : e.id)} aria-expanded={expanded}>
                  <span className={`ctile-dot is-${e.ok ? "ok" : "bad"}`} aria-hidden />
                  <span className="gw-log-time mono">{clock(e.at)}</span>
                  <span className="gw-log-client">{e.client ? (CLIENT_LABEL[e.client] ?? e.client) : "/v1"}</span>
                  <span className="gw-log-model mono truncate" title={e.target ? `${e.requested} → ${e.target}` : e.requested}>
                    {e.target && e.target !== e.requested ? (
                      <>
                        <span className="faint">{e.requested} → </span>
                        {model}
                      </>
                    ) : (
                      model || "（空）"
                    )}
                  </span>
                  <span className="gw-log-where truncate">
                    {e.channel ? channelLabel(e.channel) : ""}
                    {e.account ? ` · ${e.account}` : ""}
                    {e.attempt > 1 ? ` · 第 ${e.attempt} 次` : ""}
                  </span>
                  <span className="gw-log-num mono">
                    {e.ok ? `${tokens(e.inputTokens)} / ${tokens(e.outputTokens)}` : e.status || ""}
                  </span>
                  <span className="gw-log-num mono">{e.durationMs ? `${(e.durationMs / 1000).toFixed(1)}s` : ""}</span>
                  {!e.ok ? <Icon name="chevron" size={11} className={`opt-chev${expanded ? " is-open" : ""}`} /> : <span style={{ width: 11 }} />}
                </button>
                {expanded ? (
                  <div className="gw-log-detail">
                    {e.error ? <p className="selectable">{e.error}</p> : null}
                    <span className="faint tiny">
                      {e.dialect} · {e.stream ? "流式" : "整段"}
                      {e.kind ? ` · ${e.kind}` : ""}
                      {e.routed ? ` · 上游自称 ${e.routed}` : ""}
                      {e.cacheReadTokens ? ` · 缓存命中 ${tokens(e.cacheReadTokens)}` : ""}
                      {e.ttftMs ? ` · 首字 ${(e.ttftMs / 1000).toFixed(1)}s` : ""}
                    </span>
                  </div>
                ) : null}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
