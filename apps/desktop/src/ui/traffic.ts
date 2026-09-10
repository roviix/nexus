/**
 * 本地用量（网关请求账本）的展示语汇。纯函数，好测。
 *
 * 数字的取舍：
 *   - token 动辄百万，全写出来一行放不下，压成 `1.2M` / `34K`；千以下原样。
 *   - 耗时过了一秒就说秒，`940ms` 和 `6.8s` 各用各的单位，同一格里量级一眼分得开。
 *   - 失败率：一次都没跑过是「—」不是「0%」——「没数据」和「没失败」是两件事。
 */
import type { UsageSummary } from "../ipc/types";

/** 概览上能选的统计范围：今天（按小时）、7 天、30 天（按天）。 */
export type UsageRange = 1 | 7 | 30;

/** `1234567` → `1.2M`，`34567` → `35K`，`999` → `999`。 */
export function compactNumber(n?: number | null): string {
  if (n == null || !Number.isFinite(n)) return "—";
  const abs = Math.abs(n);
  if (abs >= 1e9) return `${trim(n / 1e9)}B`;
  if (abs >= 1e6) return `${trim(n / 1e6)}M`;
  if (abs >= 1e4) return `${Math.round(n / 1e3)}K`;
  if (abs >= 1e3) return `${trim(n / 1e3)}K`;
  return String(Math.round(n));
}

/** 一位小数，但 `12.0` 写成 `12`。 */
function trim(v: number): string {
  const s = v.toFixed(1);
  return s.endsWith(".0") ? s.slice(0, -2) : s;
}

/** 毫秒 → `620ms` / `6.8s` / `1m 12s`。 */
export function fmtMs(ms?: number | null): string {
  if (ms == null || !Number.isFinite(ms)) return "—";
  if (ms < 1000) return `${Math.round(ms)}ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
  const m = Math.floor(ms / 60_000);
  const s = Math.round((ms % 60_000) / 1000);
  return `${m}m ${s}s`;
}

/** 失败率。没有请求是「—」；有失败但不到 1% 是 `<1%`，不能四舍五入成 0%。 */
export function errorRate(errors: number, calls: number): string {
  if (!calls) return "—";
  if (!errors) return "0%";
  const pct = (errors / calls) * 100;
  if (pct < 1) return "<1%";
  return `${pct >= 10 ? Math.round(pct) : pct.toFixed(1)}%`;
}

/** 失败率的色调：>5% 红、>1% 琥珀，其余不染色。 */
export function errorTone(errors: number, calls: number): "bad" | "warn" | null {
  if (!calls || !errors) return null;
  const pct = (errors / calls) * 100;
  if (pct > 5) return "bad";
  if (pct > 1) return "warn";
  return null;
}

/** `2026-09-03` → `9/3`。 */
export function dayShort(day: string): string {
  const [, m, d] = day.split("-");
  if (!m || !d) return day;
  return `${Number(m)}/${Number(d)}`;
}

/** `2026-09-03` → `周三`。柱子少的时候（7 天）标星期比标日期直观。 */
export function weekdayOf(day: string): string {
  const t = Date.parse(`${day}T00:00:00`);
  if (!Number.isFinite(t)) return "";
  return ["周日", "周一", "周二", "周三", "周四", "周五", "周六"][new Date(t).getDay()] ?? "";
}

/** 柱状图的纵轴上限：最高的那根再往上留一点，数字取整到「好看」的刻度。 */
export function niceMax(values: number[]): number {
  const max = Math.max(0, ...values);
  if (max <= 0) return 1;
  const mag = 10 ** Math.floor(Math.log10(max));
  const unit = max / mag;
  const step = unit <= 1 ? 1 : unit <= 2 ? 2 : unit <= 5 ? 5 : 10;
  return step * mag;
}

/** 账本里一条都没有。「今天零」和「从没用过」要区分：前者画空图，后者画空态。 */
export function isUnused(s: UsageSummary | null): boolean {
  return s != null && s.since == null;
}

/** 请求最多的那一格（天或小时）。一格都没请求时为 null。 */
export function busiest<T extends { calls: number }>(rows: T[]): T | null {
  let best: T | null = null;
  for (const r of rows) if (r.calls > 0 && (!best || r.calls > best.calls)) best = r;
  return best;
}

/** `14` → `14:00`。 */
export function hourLabel(h: number): string {
  return `${h}:00`;
}

/** 折线图上的一个点。 */
export interface TrafficPoint {
  key: string;
  /** 横轴上的短标；空串就不画。 */
  tick: string;
  /** 悬停浮层的标题。 */
  title: string;
  calls: number;
  errors: number;
  tokens: number;
}

/**
 * 把账本变成图上的点。今天按小时（0 点到当前小时，之后的没发生，不画），其余按天。
 *
 * 横轴标签在这里定：7 天逐天标星期，30 天每 5 天标一个日期，今天每 4 小时标一个整点 ——
 * 密了标不下，全标就成一排糊字。最后一个点永远是「今天」/「现在」：它是这条线的锚。
 */
export function trafficPoints(s: UsageSummary, range: UsageRange, nowHour: number): TrafficPoint[] {
  if (range === 1) {
    const upto = Math.max(0, Math.min(23, Math.floor(nowHour)));
    return s.hours
      .filter((h) => h.hour <= upto)
      .map((h) => ({
        key: `h${h.hour}`,
        tick: h.hour === upto ? "现在" : h.hour % 4 === 0 ? hourLabel(h.hour) : "",
        title: `今天 ${hourLabel(h.hour)}–${hourLabel(h.hour + 1)}`,
        calls: h.calls,
        errors: h.errors,
        tokens: h.inputTokens + h.outputTokens,
      }));
  }
  const last = s.days.length - 1;
  return s.days.map((d, i) => {
    const today = i === last;
    const spaced = range === 7 || (last - i) % 5 === 0;
    return {
      key: d.day,
      tick: today ? "今天" : spaced ? (range === 7 ? weekdayOf(d.day) : dayShort(d.day)) : "",
      title: today ? `今天 · ${dayShort(d.day)}` : `${dayShort(d.day)} ${weekdayOf(d.day)}`,
      calls: d.calls,
      errors: d.errors,
      tokens: d.inputTokens + d.outputTokens,
    };
  });
}
