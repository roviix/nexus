/**
 * 一次试用的回字面板：模型 → 实际路由、思考（可折叠）、正文、首字 / 总耗时 / token。
 * 接入页的测试结果和模型广场的试用抽屉共用。
 */
import { useEffect, useRef } from "react";
import { Banner, Icon, Tag } from "../ui/primitives";
import type { Run, RunStats } from "./useTryRun";

export function TryResult({ run, stats, busy }: { run: Run; stats: RunStats | null; busy: boolean }) {
  const outRef = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    outRef.current?.scrollTo({ top: outRef.current.scrollHeight });
  }, [run.text, run.thinking]);

  const ok = Boolean(run.endedAt && !run.error && !run.failed);

  return (
    <div className={`tryout${ok ? " is-ok" : run.error || run.failed ? " is-bad" : ""}`} ref={outRef}>
      <div className="tryout-head">
        <span className={`tryout-dot${busy ? " is-busy" : ok ? " is-ok" : run.endedAt ? " is-bad" : ""}`} />
        <span className="tryout-title">{busy ? "正在生成…" : ok ? "通了" : run.endedAt ? "没通" : ""}</span>
        <Tag>本地网关</Tag>
        <span className="mono tryout-model">{run.model}</span>
        {run.routed && run.routed !== run.model ? (
          <span className="muted tryout-routed">
            → 实际 <span className="mono">{run.routed}</span>
          </span>
        ) : null}
        <span className="grow" />
        {stats ? (
          <span className="tryout-stats num">
            <Icon name="clock" size={12} /> 首字 {stats.ttft} · 总计 {stats.total} s
            {run.usage ? ` · ${run.usage.promptTokens} → ${run.usage.completionTokens} tokens` : ""}
          </span>
        ) : null}
      </div>

      {run.failed ? <Banner tone="bad" title="请求没有发出去" hint={run.failed} /> : null}
      {run.error ? <Banner tone="bad" title="上游出错，流已终结" hint={run.error} /> : null}

      {run.thinking ? (
        <details className="tryout-thinking" open={!run.text}>
          <summary>思考 · {run.thinking.length} 字</summary>
          <pre>{run.thinking}</pre>
        </details>
      ) : null}

      {run.text ? (
        <pre className="tryout-text">{run.text}</pre>
      ) : busy && !run.thinking ? (
        <div className="muted" style={{ fontSize: 12.5 }}>
          等首字…
        </div>
      ) : null}
    </div>
  );
}
