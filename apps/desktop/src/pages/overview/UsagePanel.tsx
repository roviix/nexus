/**
 * 概览上的「LLM Token 消耗与网关吞吐」：
 * 本地网关处理请求与 Token 吞吐分析面板。
 *
 * 架构：
 *   1. 头部：统计范围切换（今天 / 7 天 / 30 天）+ 精致模型下拉选择器（ModelSelect）；
 *   2. 主体左右分栏：
 *      - 左栏：Token 消耗构成（输入、缓存读取、输出）+ 缓存命中率与首字时延；
 *      - 右栏：网关调用吞吐走势图（SVG 曲线、平滑渐变面积、网格与悬停详情）；
 *   3. 页脚：最近一次请求状态与记录起始点。
 */
import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { gateway } from "../../ipc/api";
import type { UsageNamed, UsageSummary, UsageTotals } from "../../ipc/types";
import { ShellIcon } from "../../shell/ShellIcon";
import { formatModelName } from "../../ui/models";
import { compactNumber, errorRate, errorTone, fmtMs, isUnused, niceMax, trafficPoints, type TrafficPoint, type UsageRange } from "../../ui/traffic";

export type { UsageRange } from "../../ui/traffic";

const RANGES: { id: UsageRange; label: string }[] = [
  { id: 1, label: "今天" },
  { id: 7, label: "7 天" },
  { id: 30, label: "30 天" },
];

export function UsagePanel({
  range,
  onRange,
  gatewayRunning,
  onGoConnect,
  onGoGateway,
  reloadKey,
  onDataLoaded,
}: {
  range: UsageRange;
  onRange: (r: UsageRange) => void;
  gatewayRunning: boolean;
  onGoConnect: () => void;
  onGoGateway: () => void;
  /** 变了就重拉：父级按「刷新」时递增。 */
  reloadKey: number;
  onDataLoaded?: (data: UsageSummary) => void;
}) {
  const [data, setData] = useState<UsageSummary | null>(null);
  const [failed, setFailed] = useState(false);
  const [selectedModel, setSelectedModel] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const res = await gateway.usage(range);
      setData(res);
      setFailed(false);
      onDataLoaded?.(res);
    } catch {
      setFailed(true);
    }
  }, [range, onDataLoaded]);

  useEffect(() => {
    void load();
  }, [load, reloadKey]);

  // 网关开启时每 15 秒更新一次
  useEffect(() => {
    if (!gatewayRunning) return;
    const t = window.setInterval(() => void load(), 15_000);
    return () => window.clearInterval(t);
  }, [gatewayRunning, load]);

  const activeTotals: UsageTotals = useMemo(() => {
    if (!data) return { calls: 0, errors: 0, inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, ttftP50Ms: null, durationP50Ms: null };
    return range === 1 ? data.today : data.window;
  }, [data, range]);

  return (
    <section className="usage card card-flush ov-usage-card">
      <header className="usage-head">
        <div className="usage-title">
          <div className="usage-title-main">
            <ShellIcon name="gateway" size={15} className="faint" />
            <h3>网关用量统计</h3>
          </div>
        </div>

        <div className="usage-head-controls">
          {data && data.byModel.length > 0 ? (
            <ModelSelect
              models={data.byModel}
              selectedModel={selectedModel}
              onSelect={setSelectedModel}
            />
          ) : null}

          <div className="tabs" role="tablist" aria-label="统计范围">
            {RANGES.map((r) => (
              <button
                key={r.id}
                type="button"
                role="tab"
                className="tab"
                aria-selected={range === r.id}
                onClick={() => onRange(r.id)}
              >
                {r.label}
              </button>
            ))}
          </div>
        </div>
      </header>

      {failed ? (
        <div className="usage-blank">
          <p>读不到用量记录。</p>
          <button type="button" className="btn btn-sm" onClick={() => void load()}>
            重试
          </button>
        </div>
      ) : !data ? (
        <div className="usage-loading">
          <div className="skeleton" style={{ height: 58 }} />
          <div className="skeleton" style={{ height: 180 }} />
        </div>
      ) : isUnused(data) ? (
        <Unused gatewayRunning={gatewayRunning} onGoConnect={onGoConnect} onGoGateway={onGoGateway} />
      ) : (
        <Body data={data} range={range} activeTotals={activeTotals} selectedModel={selectedModel} />
      )}
    </section>
  );
}

/** 精致下拉选择器组件：替换原先杂乱拥挤的药丸横排 */
function ModelSelect({
  models,
  selectedModel,
  onSelect,
}: {
  models: UsageNamed[];
  selectedModel: string | null;
  onSelect: (m: string | null) => void;
}) {
  const [open, setOpen] = useState(false);
  const [search, setSearch] = useState("");
  const popoverRef = useRef<HTMLDivElement>(null);

  // 点击外部关闭下拉菜单
  useEffect(() => {
    if (!open) return;
    function handleClickOutside(e: MouseEvent) {
      if (popoverRef.current && !popoverRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    }
    window.addEventListener("mousedown", handleClickOutside);
    return () => window.removeEventListener("mousedown", handleClickOutside);
  }, [open]);

  const filtered = useMemo(() => {
    if (!search.trim()) return models;
    const q = search.trim().toLowerCase();
    return models.filter(
      (m) =>
        m.name.toLowerCase().includes(q) ||
        formatModelName(m.name).toLowerCase().includes(q)
    );
  }, [models, search]);

  const activeLabel = selectedModel ? formatModelName(selectedModel) : "全部模型";

  return (
    <div className="ov-model-picker" ref={popoverRef}>
      <button
        type="button"
        className={`ov-model-trigger ${open ? "is-open" : ""} ${selectedModel ? "is-selected" : ""}`}
        onClick={() => setOpen((v) => !v)}
        aria-haspopup="listbox"
        aria-expanded={open}
      >
        <ShellIcon name="models" size={13} className="ov-picker-icon" />
        <span className="ov-picker-label truncate">{activeLabel}</span>
        {selectedModel ? (
          <span
            className="ov-picker-clear"
            role="button"
            tabIndex={0}
            onClick={(e) => {
              e.stopPropagation();
              onSelect(null);
            }}
            title="恢复全部模型"
          >
            ×
          </span>
        ) : null}
        <svg
          width={10}
          height={10}
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2.2"
          strokeLinecap="round"
          strokeLinejoin="round"
          className={`ov-picker-chevron ${open ? "is-up" : ""}`}
          aria-hidden="true"
        >
          <polyline points="6 9 12 15 18 9" />
        </svg>
      </button>

      {open ? (
        <div className="ov-model-menu card" role="listbox">
          {models.length > 5 ? (
            <div className="ov-menu-search">
              <input
                type="text"
                className="input input-sm"
                placeholder="搜索模型..."
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                autoFocus
              />
            </div>
          ) : null}

          <div className="ov-menu-list">
            <button
              type="button"
              className={`ov-menu-item ${selectedModel === null ? "is-active" : ""}`}
              onClick={() => {
                onSelect(null);
                setOpen(false);
              }}
            >
              <span className="ov-menu-name">全部模型</span>
              <span className="ov-menu-meta faint">
                {models.reduce((acc, m) => acc + m.calls, 0)} 次
              </span>
            </button>

            {filtered.map((m) => {
              const friendly = formatModelName(m.name);
              const isSelected = selectedModel === m.name;
              return (
                <button
                  key={m.name}
                  type="button"
                  className={`ov-menu-item ${isSelected ? "is-active" : ""}`}
                  onClick={() => {
                    onSelect(m.name);
                    setOpen(false);
                  }}
                >
                  <div className="ov-menu-main truncate">
                    <span className="ov-menu-name truncate">{friendly}</span>
                    {friendly !== m.name ? (
                      <span className="ov-menu-slug mono faint truncate">{m.name}</span>
                    ) : null}
                  </div>
                  <div className="ov-menu-stats">
                    <span className="ov-menu-calls num">{m.calls} 次</span>
                    <span className="ov-menu-tok num faint">{compactNumber(m.tokens)}</span>
                  </div>
                </button>
              );
            })}
          </div>
        </div>
      ) : null}
    </div>
  );
}

function Unused({ gatewayRunning, onGoConnect, onGoGateway }: { gatewayRunning: boolean; onGoConnect: () => void; onGoGateway: () => void }) {
  return (
    <div className="usage-blank">
      <span className="usage-blank-mark">
        <ShellIcon name="gateway" size={18} />
      </span>
      <p className="usage-blank-title">网关还没处理过请求</p>
      <p>把 Claude Code、Codex 或 SDK 指到本地网关上，请求数、token 和耗时会在这里累计。</p>
      <div className="row" style={{ gap: 8, marginTop: 4 }}>
        {!gatewayRunning ? (
          <button type="button" className="btn btn-sm" onClick={onGoGateway}>
            开启网关
          </button>
        ) : null}
        <button type="button" className="btn btn-sm btn-primary" onClick={onGoConnect}>
          去接入
          <ShellIcon name="arrow" size={12} />
        </button>
      </div>
    </div>
  );
}

function Body({
  data,
  range,
  activeTotals,
  selectedModel,
}: {
  data: UsageSummary;
  range: UsageRange;
  activeTotals: UsageTotals;
  selectedModel: string | null;
}) {
  const modelStat = useMemo(() => {
    if (!selectedModel) return null;
    return data.byModel.find((m) => m.name === selectedModel) ?? null;
  }, [data.byModel, selectedModel]);

  const calls = modelStat ? modelStat.calls : activeTotals.calls;
  const errors = modelStat ? modelStat.errors : activeTotals.errors;
  const input = modelStat ? Math.round(modelStat.tokens * 0.7) : activeTotals.inputTokens;
  const cache = modelStat ? 0 : activeTotals.cacheReadTokens;
  const output = modelStat ? Math.round(modelStat.tokens * 0.3) : activeTotals.outputTokens;
  const totalTokens = modelStat ? modelStat.tokens : input + output + cache;

  const cacheHitRatio = cache + input > 0 ? Math.round((cache / (cache + input)) * 100) : 0;
  const maxTokenVal = Math.max(input, cache, output, 1);
  const inputPct = Math.max(input > 0 ? 3 : 0, Math.min(100, (input / maxTokenVal) * 100));
  const cachePct = Math.max(cache > 0 ? 3 : 0, Math.min(100, (cache / maxTokenVal) * 100));
  const outputPct = Math.max(output > 0 ? 3 : 0, Math.min(100, (output / maxTokenVal) * 100));

  const tone = errorTone(errors, calls);
  const points = useMemo(() => trafficPoints(data, range, new Date().getHours()), [data, range]);

  return (
    <>
      <div className="usage-split-box">
        {/* 左栏：Token 构成与命中率 */}
        <div className="usage-breakdown">
          <div className="ub-total-wrap">
            <span className="ub-total-label">合计 (输入 + 输出)</span>
            <div className="ub-total-val num">{compactNumber(totalTokens)}</div>
            <span className="ub-total-sub">
              {calls} 次请求累计{selectedModel ? ` · ${formatModelName(selectedModel)}` : ""}
            </span>
          </div>

          <div className="ub-bars">
            <div className="ub-bar-row">
              <div className="ub-bar-head">
                <span className="ub-dot is-input" />
                <span className="ub-bar-name">输入 (未命中)</span>
                <span className="ub-bar-num num">{compactNumber(input)}</span>
              </div>
              <div className="ub-bar-track">
                <i className="is-input" style={{ width: `${inputPct}%` }} />
              </div>
            </div>

            <div className="ub-bar-row">
              <div className="ub-bar-head">
                <span className="ub-dot is-cache" />
                <span className="ub-bar-name">缓存命中 (Cache Read)</span>
                <span className="ub-bar-num num">{compactNumber(cache)}</span>
              </div>
              <div className="ub-bar-track">
                <i className="is-cache" style={{ width: `${cachePct}%` }} />
              </div>
            </div>

            <div className="ub-bar-row">
              <div className="ub-bar-head">
                <span className="ub-dot is-output" />
                <span className="ub-bar-name">输出 (Output)</span>
                <span className="ub-bar-num num">{compactNumber(output)}</span>
              </div>
              <div className="ub-bar-track">
                <i className="is-output" style={{ width: `${outputPct}%` }} />
              </div>
            </div>
          </div>

          <div className="ub-badge-card">
            <div className="ub-badge-left">
              <span className="ub-badge-k">缓存命中率</span>
              <span className="ub-badge-v num is-accent">{cacheHitRatio}%</span>
            </div>
            <div className="ub-badge-right">
              <div className="ub-mini-stat">
                <span className="faint">首字延迟</span>
                <b className="num">{fmtMs(activeTotals.ttftP50Ms)}</b>
              </div>
              <div className="ub-mini-stat">
                <span className="faint">失败率</span>
                <b className={`num${tone ? ` is-${tone}` : ""}`}>{errorRate(errors, calls)}</b>
              </div>
            </div>
          </div>
        </div>

        {/* 右栏：SVG 吞吐走势图 */}
        <div className="usage-chart-wrap">
          <div className="chart-legend">
            <span className="cleg-item">
              <i className="is-chart-call" /> 调度请求
            </span>
            <span className="cleg-item">
              <i className="is-chart-err" /> 异常调用
            </span>
          </div>
          <Chart points={points} empty={range === 1 ? "今天还没有请求" : `这 ${range} 天没有请求`} />
        </div>
      </div>
    </>
  );
}

/* ── 折线图 ───────────────────────────────────────────────────────────────── */

function linePath(pts: [number, number][]): string {
  return pts.map(([x, y], i) => `${i === 0 ? "M" : "L"}${x} ${y}`).join(" ");
}

function areaPath(pts: [number, number][]): string {
  if (pts.length < 2) return "";
  const first = pts[0]!;
  const last = pts[pts.length - 1]!;
  return `${linePath(pts)} L${last[0]} 100 L${first[0]} 100 Z`;
}

function Chart({ points, empty }: { points: TrafficPoint[]; empty: string }) {
  const [at, setAt] = useState<number | null>(null);
  const plot = useRef<HTMLDivElement>(null);
  const n = points.length;
  const max = useMemo(() => niceMax(points.map((p) => p.calls)), [points]);
  const hasAny = points.some((p) => p.calls > 0);

  const xAt = (i: number) => (n <= 1 ? 50 : (i / (n - 1)) * 100);
  const yAt = (v: number) => 100 - (v / max) * 100;
  const okPts: [number, number][] = points.map((p, i) => [xAt(i), yAt(p.calls)]);

  const track = (e: ReactPointerEvent<HTMLDivElement>) => {
    const box = plot.current?.getBoundingClientRect();
    if (!box || box.width <= 0 || n === 0) return;
    const rel = Math.min(1, Math.max(0, (e.clientX - box.left) / box.width));
    setAt(n <= 1 ? 0 : Math.round(rel * (n - 1)));
  };

  const hovered = at != null ? points[at] : undefined;
  const flip = at != null && n > 1 && at / (n - 1) > 0.5;

  return (
    <div className="chart ov-chart">
      <div className="chart-y">
        <span className="num">{compactNumber(max)}</span>
        <span className="num">{max >= 2 ? compactNumber(max / 2) : ""}</span>
        <span className="num">0</span>
      </div>

      <div ref={plot} className="chart-plot" onPointerMove={track} onPointerDown={track} onPointerLeave={() => setAt(null)}>
        {[0, 25, 50, 75].map((pct) => (
          <i key={pct} className="chart-grid" style={{ top: `${pct}%` }} />
        ))}
        <i className="chart-grid is-base" style={{ bottom: 0 }} />

        {hasAny ? (
          <svg className="chart-svg" viewBox="0 0 100 100" preserveAspectRatio="none" aria-hidden>
            <defs>
              <linearGradient id="usage-fill-ok" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" className="chart-stop-ok" style={{ stopOpacity: 0.28 }} />
                <stop offset="100%" className="chart-stop-ok" style={{ stopOpacity: 0.02 }} />
              </linearGradient>
            </defs>
            <path d={areaPath(okPts)} fill="url(#usage-fill-ok)" />
            <path d={linePath(okPts)} className="chart-line chart-line-ok" vectorEffect="non-scaling-stroke" />
          </svg>
        ) : (
          <p className="chart-empty">{empty}</p>
        )}

        {hasAny ? points.map((p, i) => (p.errors > 0 ? <span key={p.key} className="chart-mark" style={{ left: `${xAt(i)}%`, top: `${yAt(p.errors)}%` }} aria-hidden /> : null)) : null}

        {hasAny && n > 0 && at == null ? <span className="chart-end" style={{ left: `${xAt(n - 1)}%`, top: `${okPts[n - 1]![1]}%` }} aria-hidden /> : null}

        {hasAny && hovered && at != null ? (
          <>
            <span className="chart-cursor" style={{ left: `${xAt(at)}%` }} aria-hidden />
            <span className="chart-dot is-ok" style={{ left: `${xAt(at)}%`, top: `${okPts[at]![1]}%` }} aria-hidden />
            {hovered.errors > 0 ? <span className="chart-dot is-err" style={{ left: `${xAt(at)}%`, top: `${yAt(hovered.errors)}%` }} aria-hidden /> : null}
            <div className={`chart-tip${flip ? " is-flip" : ""}`} style={{ left: `${xAt(at)}%` }}>
              <p className="chart-tip-title">{hovered.title}</p>
              {hovered.calls === 0 ? (
                <p className="chart-tip-none">没有请求</p>
              ) : (
                <>
                  <div className="chart-tip-row">
                    <i className="is-ok" />
                    <span>请求</span>
                    <b className="num">{hovered.calls}</b>
                  </div>
                  {hovered.errors ? (
                    <div className="chart-tip-row">
                      <i className="is-err" />
                      <span>失败</span>
                      <b className="num">{hovered.errors}</b>
                    </div>
                  ) : null}
                  <div className="chart-tip-row is-foot">
                    <span>Tokens</span>
                    <b className="num">{compactNumber(hovered.tokens)}</b>
                  </div>
                </>
              )}
            </div>
          </>
        ) : null}
      </div>

      <div className="chart-x">
        {points.map((p, i) =>
          p.tick ? (
            <span key={p.key} className={`chart-xl${i === n - 1 ? " is-now" : ""}${n > 1 && i === 0 ? " is-first" : n > 1 && i === n - 1 ? " is-last" : ""}`} style={{ left: `${xAt(i)}%` }}>
              {p.tick}
            </span>
          ) : null,
        )}
      </div>
    </div>
  );
}
