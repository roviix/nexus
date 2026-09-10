/**
 * 概览上的「本地用量」：本地网关最近处理了多少请求。
 *
 * 数据是网关自己记的账（`gateway_usage`），不联网。回答的问题按顺序摆：
 *   1. 今天跑了多少（四格数字）；
 *   2. 走势（一条折线：今天按小时，7 / 30 天按天；失败另画一条细红线）；
 *   3. 都花在哪个模型、哪个号上（两列排行）。
 *
 * 图不引库：一段 SVG 折线 + 淡淡一层面积，和线上控制台概览那张图是同一副样子。
 * 折线只连真的量到的点，桶与桶之间是直的 —— 平滑曲线会在两点之间画出并不存在的起伏。
 * 悬停给一条竖线和各点的圆点，浮层自己画：原生 title 要等半秒、只给一行字。
 * 「一次都没跑过」和「这段时间恰好为零」要分开 —— 前者给一扇去接入的门，后者照常画空图。
 */
import { useCallback, useEffect, useMemo, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { gateway } from "../../ipc/api";
import type { UsageNamed, UsageSummary } from "../../ipc/types";
import { ShellIcon } from "../../shell/ShellIcon";
import { timeAgo } from "../../ui/format";
import { busiest, compactNumber, errorRate, errorTone, fmtMs, hourLabel, isUnused, niceMax, trafficPoints, type TrafficPoint, type UsageRange } from "../../ui/traffic";

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
}: {
  range: UsageRange;
  onRange: (r: UsageRange) => void;
  gatewayRunning: boolean;
  onGoConnect: () => void;
  onGoGateway: () => void;
  /** 变了就重拉：父级按「刷新」时递增。 */
  reloadKey: number;
}) {
  const [data, setData] = useState<UsageSummary | null>(null);
  const [failed, setFailed] = useState(false);

  const load = useCallback(async () => {
    try {
      setData(await gateway.usage(range));
      setFailed(false);
    } catch {
      setFailed(true);
    }
  }, [range]);

  useEffect(() => {
    void load();
  }, [load, reloadKey]);

  // 开着的时候每 15 秒刷一次：正在用 Claude Code 的人回到概览，想看的就是刚刚那几次。
  useEffect(() => {
    if (!gatewayRunning) return;
    const t = window.setInterval(() => void load(), 15_000);
    return () => window.clearInterval(t);
  }, [gatewayRunning, load]);

  return (
    <section className="usage card card-flush">
      <header className="usage-head">
        <div className="usage-title">
          <span className="eyebrow">本地用量</span>
          <h3>网关请求</h3>
        </div>
        <div className="tabs" role="tablist" aria-label="统计范围">
          {RANGES.map((r) => (
            <button key={r.id} type="button" role="tab" className="tab" aria-selected={range === r.id} onClick={() => onRange(r.id)}>
              {r.label}
            </button>
          ))}
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
          <div className="skeleton" style={{ height: 150 }} />
        </div>
      ) : isUnused(data) ? (
        <Unused gatewayRunning={gatewayRunning} onGoConnect={onGoConnect} onGoGateway={onGoGateway} />
      ) : (
        <Body data={data} range={range} />
      )}
    </section>
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

function Body({ data, range }: { data: UsageSummary; range: UsageRange }) {
  const t = data.today;
  const w = data.window;
  const tone = errorTone(w.errors, w.calls);
  const last = data.recent[0];
  const points = useMemo(() => trafficPoints(data, range, new Date().getHours()), [data, range]);

  // 「今天」视图里窗口就是今天，再写「1 天共 N 次」是废话；换成最忙的那个小时。
  const peakHour = range === 1 ? busiest(data.hours) : null;
  const callsSub = range === 1 ? (peakHour ? `最忙 ${hourLabel(peakHour.hour)} · ${peakHour.calls} 次` : "还没有请求") : `${range} 天共 ${compactNumber(w.calls)} 次`;

  return (
    <>
      <div className="kpis">
        <Kpi k="今日请求" v={String(t.calls)} sub={callsSub} />
        <Kpi k="今日 Tokens" v={compactNumber(t.inputTokens + t.outputTokens)} sub={`输入 ${compactNumber(t.inputTokens)} · 输出 ${compactNumber(t.outputTokens)}`} />
        <Kpi k="首字中位" v={fmtMs(w.ttftP50Ms)} sub={w.durationP50Ms != null ? `整轮 ${fmtMs(w.durationP50Ms)}` : "还没有成功的请求"} />
        <Kpi k="失败率" v={errorRate(w.errors, w.calls)} tone={tone} sub={w.errors ? `${w.errors} 次失败` : w.calls ? "无失败" : "没有请求"} />
      </div>

      <Chart points={points} empty={range === 1 ? "今天还没有请求" : `这 ${range} 天没有请求`} />

      {data.byModel.length || data.byAccount.length ? (
        <div className="usage-split">
          <Ranking cap="按模型" rows={data.byModel} />
          <Ranking cap="按账号" rows={data.byAccount} mono />
        </div>
      ) : null}

      <footer className="usage-foot">
        {last ? (
          <span className="truncate">
            最近一次 {timeAgo(last.at)} · <span className="mono">{last.routed ?? last.model}</span>
            {last.ok ? (last.ttftMs != null ? ` · 首字 ${fmtMs(last.ttftMs)}` : "") : <span className="is-bad"> · 失败（{last.kind ?? last.status}）</span>}
          </span>
        ) : (
          <span>{range === 1 ? "今天没有请求" : "这几天没有请求"}</span>
        )}
        {data.since ? <span className="faint">记录自 {timeAgo(data.since)}</span> : null}
      </footer>
    </>
  );
}

function Kpi({ k, v, sub, tone }: { k: string; v: string; sub: string; tone?: "bad" | "warn" | null }) {
  return (
    <div className="kpi">
      <span className="kpi-k">{k}</span>
      <span className={`kpi-v num${tone ? ` is-${tone}` : ""}`}>{v}</span>
      <span className="kpi-sub truncate">{sub}</span>
    </div>
  );
}

/* ── 折线图 ───────────────────────────────────────────────────────────────── */

/** 逐点直连。只连真的量到的点，中间那段是直的，读者也就知道中间没有别的信息。 */
function linePath(pts: [number, number][]): string {
  return pts.map(([x, y], i) => `${i === 0 ? "M" : "L"}${x} ${y}`).join(" ");
}

/** 面积：沿折线走过去，再落到底边闭合。 */
function areaPath(pts: [number, number][]): string {
  if (pts.length < 2) return "";
  const first = pts[0]!;
  const last = pts[pts.length - 1]!;
  return `${linePath(pts)} L${last[0]} 100 L${first[0]} 100 Z`;
}

/**
 * 请求数一条主线（品牌色，带一层淡面积）。失败不另画一条线 —— 多数桶是零，那条线会整段
 * 贴着底轴发红；改成只在真有失败的桶上点一枚红点，高度就是失败次数，数字看浮层。
 * 纵轴上限取整到好看的数（`niceMax`），三档刻度对着 0 / 一半 / 顶三条网格线。
 *
 * SVG 用 `viewBox 0 0 100 100` + `preserveAspectRatio="none"` 铺满，横竖缩放比不同，
 * 所以线宽靠 `vectorEffect="non-scaling-stroke"` 钉住，圆点用 HTML 画（画进 SVG 会被拉成椭圆）。
 */
function Chart({ points, empty }: { points: TrafficPoint[]; empty: string }) {
  const [at, setAt] = useState<number | null>(null);
  const plot = useRef<HTMLDivElement>(null);
  const n = points.length;
  const max = useMemo(() => niceMax(points.map((p) => p.calls)), [points]);
  const hasAny = points.some((p) => p.calls > 0);

  // 只有一个点时给它一点位置，否则落在 x=0 贴着左边。
  const xAt = (i: number) => (n <= 1 ? 50 : (i / (n - 1)) * 100);
  const yAt = (v: number) => 100 - (v / max) * 100;
  const okPts: [number, number][] = points.map((p, i) => [xAt(i), yAt(p.calls)]);

  // 按鼠标横坐标找最近的点，而不是切成 n 列 —— 点在列的边界上，切列会差半格。
  const track = (e: ReactPointerEvent<HTMLDivElement>) => {
    const box = plot.current?.getBoundingClientRect();
    if (!box || box.width <= 0 || n === 0) return;
    const rel = Math.min(1, Math.max(0, (e.clientX - box.left) / box.width));
    setAt(n <= 1 ? 0 : Math.round(rel * (n - 1)));
  };

  const hovered = at != null ? points[at] : undefined;
  const flip = at != null && n > 1 && at / (n - 1) > 0.5;

  return (
    <div className="chart">
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
                <stop offset="0%" className="chart-stop-ok" style={{ stopOpacity: 0.22 }} />
                <stop offset="100%" className="chart-stop-ok" style={{ stopOpacity: 0.02 }} />
              </linearGradient>
            </defs>
            <path d={areaPath(okPts)} fill="url(#usage-fill-ok)" />
            <path d={linePath(okPts)} className="chart-line chart-line-ok" vectorEffect="non-scaling-stroke" />
          </svg>
        ) : (
          <p className="chart-empty">{empty}</p>
        )}

        {/* 有失败的桶点一枚红点，高度就是失败次数。 */}
        {hasAny ? points.map((p, i) => (p.errors > 0 ? <span key={p.key} className="chart-mark" style={{ left: `${xAt(i)}%`, top: `${yAt(p.errors)}%` }} aria-hidden /> : null)) : null}

        {/* 最后一个点是「现在」：一枚小点钉住线的终点，只有一个点时它也是唯一能看见的东西。 */}
        {hasAny && n > 0 && at == null ? <span className="chart-end" style={{ left: `${xAt(n - 1)}%`, top: `${okPts[n - 1]![1]}%` }} aria-hidden /> : null}

        {hasAny && hovered && at != null ? (
          <>
            <span className="chart-cursor" style={{ left: `${xAt(at)}%` }} aria-hidden />
            <span className="chart-dot is-ok" style={{ left: `${xAt(at)}%`, top: `${okPts[at]![1]}%` }} aria-hidden />
            {hovered.errors > 0 ? <span className="chart-dot is-err" style={{ left: `${xAt(at)}%`, top: `${yAt(hovered.errors)}%` }} aria-hidden /> : null}
            {/* 浮层固定挂在图的上沿、只左右翻转：跟着线的高低走，鼠标横扫一遍它就上下乱跳，
                而且在高峰处必然压住峰顶 —— 那儿正是要看的地方。 */}
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

/** 排行：名字、条子、次数。条子按第一名的长度算比例，一眼看出「主力是哪个」。 */
function Ranking({ cap, rows, mono }: { cap: string; rows: UsageNamed[]; mono?: boolean }) {
  const top = rows[0]?.calls ?? 0;
  return (
    <div className="rank">
      <div className="rank-cap">
        <span>{cap}</span>
        <span className="faint">请求</span>
      </div>
      {rows.length === 0 ? (
        <p className="rank-none">—</p>
      ) : (
        rows.slice(0, 5).map((r) => {
          const tone = errorTone(r.errors, r.calls);
          return (
            <div key={r.name} className="rank-row" title={`${r.name} · ${r.calls} 次 · ${compactNumber(r.tokens)} tokens${r.errors ? ` · ${r.errors} 次失败` : ""}`}>
              <span className={`rank-name truncate${mono ? " mono" : ""}`}>{r.name}</span>
              <span className="rank-bar">
                <i style={{ width: `${top ? Math.max(2, (r.calls / top) * 100) : 0}%` }} />
              </span>
              <span className="rank-n num">
                {r.calls}
                {tone ? <i className={`rank-dot is-${tone}`} aria-label={`${r.errors} 次失败`} /> : null}
              </span>
            </div>
          );
        })
      )}
    </div>
  );
}
