import { describe, expect, it } from "vitest";
import type { UsageDay, UsageHour, UsageSummary } from "../ipc/types";
import {
  busiest,
  compactNumber,
  dayShort,
  errorRate,
  errorTone,
  fmtMs,
  hourLabel,
  isUnused,
  niceMax,
  trafficPoints,
  weekdayOf,
} from "./traffic";

describe("compactNumber", () => {
  it("keeps small numbers whole and compresses big ones", () => {
    expect(compactNumber(0)).toBe("0");
    expect(compactNumber(999)).toBe("999");
    expect(compactNumber(1234)).toBe("1.2K");
    expect(compactNumber(12_000)).toBe("12K");
    expect(compactNumber(34_567)).toBe("35K");
    expect(compactNumber(1_234_567)).toBe("1.2M");
    expect(compactNumber(2_000_000)).toBe("2M");
    expect(compactNumber(3.4e9)).toBe("3.4B");
  });

  it("says nothing for missing values", () => {
    expect(compactNumber(null)).toBe("—");
    expect(compactNumber(Number.NaN)).toBe("—");
  });
});

describe("fmtMs", () => {
  it("switches unit at a second and a minute", () => {
    expect(fmtMs(620)).toBe("620ms");
    expect(fmtMs(6800)).toBe("6.8s");
    expect(fmtMs(72_000)).toBe("1m 12s");
    expect(fmtMs(null)).toBe("—");
  });
});

describe("errorRate", () => {
  it("distinguishes no data from no errors", () => {
    expect(errorRate(0, 0)).toBe("—");
    expect(errorRate(0, 40)).toBe("0%");
  });

  it("never rounds a real failure down to zero", () => {
    expect(errorRate(1, 500)).toBe("<1%");
    expect(errorRate(3, 100)).toBe("3.0%");
    expect(errorRate(15, 100)).toBe("15%");
  });

  it("tones by threshold", () => {
    expect(errorTone(0, 0)).toBeNull();
    expect(errorTone(0, 10)).toBeNull();
    expect(errorTone(1, 200)).toBeNull();
    expect(errorTone(3, 100)).toBe("warn");
    expect(errorTone(10, 100)).toBe("bad");
  });
});

describe("dates", () => {
  it("shortens a day and names its weekday", () => {
    expect(dayShort("2026-09-03")).toBe("9/3");
    expect(dayShort("garbage")).toBe("garbage");
    expect(weekdayOf("2026-09-03")).toBe("周四");
    expect(weekdayOf("nope")).toBe("");
  });
});

describe("niceMax", () => {
  it("rounds the axis up to a clean step", () => {
    expect(niceMax([])).toBe(1);
    expect(niceMax([0, 0])).toBe(1);
    expect(niceMax([7])).toBe(10);
    expect(niceMax([13, 4])).toBe(20);
    expect(niceMax([41])).toBe(50);
    expect(niceMax([96])).toBe(100);
    expect(niceMax([100])).toBe(100);
    expect(niceMax([230])).toBe(500);
  });
});

describe("summary helpers", () => {
  const day = (d: string, calls: number): UsageDay => ({ day: d, calls, errors: 0, inputTokens: 0, outputTokens: 0 });
  const hour = (h: number, calls: number, errors = 0): UsageHour => ({ hour: h, calls, errors, inputTokens: calls * 10, outputTokens: calls });
  const base: UsageSummary = {
    days: [day("2026-09-01", 3), day("2026-09-02", 9), day("2026-09-03", 4)],
    hours: Array.from({ length: 24 }, (_, h) => hour(h, h === 9 ? 5 : h === 14 ? 2 : 0, h === 14 ? 1 : 0)),
    today: { calls: 4, errors: 0, inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, ttftP50Ms: null, durationP50Ms: null },
    window: { calls: 16, errors: 0, inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, ttftP50Ms: null, durationP50Ms: null },
    byModel: [],
    byAccount: [],
    byChannel: [],
    recent: [],
    since: "2026-08-01T00:00:00Z",
  };

  it("tells an empty ledger apart from a quiet week", () => {
    expect(isUnused(null)).toBe(false);
    expect(isUnused(base)).toBe(false);
    expect(isUnused({ ...base, since: null })).toBe(true);
  });

  it("finds the busiest bucket, day or hour", () => {
    expect(busiest(base.days)?.day).toBe("2026-09-02");
    expect(busiest(base.hours)?.hour).toBe(9);
    expect(busiest([day("2026-09-01", 0)])).toBeNull();
  });

  it("labels an hour as a clock time", () => {
    expect(hourLabel(0)).toBe("0:00");
    expect(hourLabel(14)).toBe("14:00");
  });
});

describe("trafficPoints", () => {
  const day = (d: string, calls: number, errors = 0): UsageDay => ({ day: d, calls, errors, inputTokens: 100, outputTokens: 20 });
  const hour = (h: number, calls: number): UsageHour => ({ hour: h, calls, errors: 0, inputTokens: 0, outputTokens: 0 });
  const summary = (days: UsageDay[]): UsageSummary => ({
    days,
    hours: Array.from({ length: 24 }, (_, h) => hour(h, h)),
    today: { calls: 0, errors: 0, inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, ttftP50Ms: null, durationP50Ms: null },
    window: { calls: 0, errors: 0, inputTokens: 0, outputTokens: 0, cacheReadTokens: 0, ttftP50Ms: null, durationP50Ms: null },
    byModel: [],
    byAccount: [],
    byChannel: [],
    recent: [],
    since: null,
  });
  const week = ["2026-08-28", "2026-08-29", "2026-08-30", "2026-08-31", "2026-09-01", "2026-09-02", "2026-09-03"].map((d, i) => day(d, i));

  it("cuts today's hours at the current hour and marks the last one as now", () => {
    const pts = trafficPoints(summary(week), 1, 14);
    expect(pts).toHaveLength(15);
    expect(pts[0]?.tick).toBe("0:00");
    expect(pts[4]?.tick).toBe("4:00");
    expect(pts[5]?.tick).toBe("");
    expect(pts[14]?.tick).toBe("现在");
    expect(pts[14]?.title).toBe("今天 14:00–15:00");
    expect(pts[14]?.calls).toBe(14);
    // 「现在」压过整点标：12 点整时最后一格也叫现在。
    expect(trafficPoints(summary(week), 1, 12).at(-1)?.tick).toBe("现在");
    // 刚过午夜只有一格。
    expect(trafficPoints(summary(week), 1, 0)).toHaveLength(1);
  });

  it("labels a week by weekday and a month every five days, today last", () => {
    const w = trafficPoints(summary(week), 7, 10);
    expect(w).toHaveLength(7);
    expect(w[0]?.tick).toBe(weekdayOf("2026-08-28"));
    expect(w[6]?.tick).toBe("今天");
    expect(w[6]?.title).toBe("今天 · 9/3");
    expect(w[1]?.title).toBe(`8/29 ${weekdayOf("2026-08-29")}`);
    expect(w[3]?.tokens).toBe(120);

    const month = Array.from({ length: 30 }, (_, i) => day(`2026-08-${String(i + 1).padStart(2, "0")}`, 1));
    const m = trafficPoints(summary(month), 30, 10);
    expect(m.filter((p) => p.tick !== "").map((p) => p.tick)).toEqual(["8/5", "8/10", "8/15", "8/20", "8/25", "今天"]);
  });
});
