import { describe, expect, it } from "vitest";
import {
  accountProblem,
  allBuckets,
  blockReasonText,
  cardBuckets,
  cycleProgress,
  daysText,
  isEndingSoon,
  isFresh,
  railTone,
  meterColor,
  meterWidth,
  money,
  moneyShort,
  onDemandParts,
  onDemandText,
  overallPercent,
  pctText,
  planBudget,
  planGroup,
  planLabel,
  planTone,
  shortDate,
  spendPace,
  startOfLocalDay,
  untilText,
  worstBucket,
} from "./usage";
import type { AccountUsage } from "../ipc/types";

const NOW = Date.parse("2026-09-02T12:00:00Z");
const DAY = 86_400_000;

const usage = (u: Partial<AccountUsage> = {}): AccountUsage => ({ fetchedAt: "", ...u });

describe("planLabel", () => {
  it("normalises Cursor's raw plan names", () => {
    expect(planLabel(usage({ plan: "ultra" }))).toBe("Ultra");
    expect(planLabel(usage({ plan: "pro_plus" }))).toBe("Pro+");
    // Team 套餐在接口里叫 enterprise：徽章和筛选都叫它 Team，别让人以为是两档。
    expect(planLabel(usage({ plan: "enterprise" }))).toBe("Team");
    expect(planLabel(usage({ plan: "business" }))).toBe("Team");
  });

  it("marks a trialing subscription because that plan can drop at any time", () => {
    expect(planLabel(usage({ plan: "pro", subscriptionStatus: "trialing" }))).toBe("Pro Trial");
    // 已经带 trial 字样的不要叠两遍。
    expect(planLabel(usage({ plan: "free_trial", subscriptionStatus: "trialing" }))).toBe(
      "Pro Trial",
    );
  });

  it("falls back to a readable form for plans it has never seen", () => {
    expect(planLabel(usage({ plan: "super_duper" }))).toBe("Super Duper");
    expect(planLabel(usage())).toBe("未知");
    expect(planLabel(null)).toBe("未知");
  });

  it("gives each tier a visually distinct tone", () => {
    const tones = ["ultra", "pro_plus", "pro", "team", "free"].map((p) =>
      planTone(usage({ plan: p })),
    );
    expect(new Set(tones).size).toBe(5);
  });
});

describe("spendPace / planBudget", () => {
  const cycle = { cycleStart: NOW - 12 * DAY, cycleEnd: NOW + 18 * DAY };

  it("takes plan.limit as the budget, never the consumed 'included' amount", () => {
    // breakdown.included 是花掉的那部分，不是额度：拿它当额度，用满的号永远「刚好 100%」。
    expect(planBudget(usage({ planLimitCents: 2000, includedCents: 1800 }))).toBe(2000);
    // 老档没有 limit：用花费 ÷ 已用比例倒推。
    expect(planBudget(usage({ spendCents: 500, totalPercentUsed: 25 }))).toBe(2000);
    expect(planBudget(usage({ includedCents: 1800 }))).toBeNull();
    expect(planBudget(null)).toBeNull();
  });

  it("projects the cycle from the daily average and says how much runway is left", () => {
    const p = spendPace(usage({ ...cycle, spendCents: 840, planLimitCents: 2000 }), NOW)!;
    expect(p.totalDays).toBe(30);
    expect(p.elapsedDays).toBe(12);
    expect(p.perDay).toBe(70);
    expect(p.projected).toBe(2100);
    expect(p.remaining).toBe(1160);
    // 花了 42%，时间过了 40%：快 2 个点。
    expect(p.aheadPct).toBeCloseTo(2, 5);
    // 1160 / 70 ≈ 16.6 天，不够撑到 18 天后的重置。
    expect(p.runwayDays).toBeCloseTo(16.57, 1);
  });

  it("keeps the daily average finite on the first day and marks an exhausted budget as zero runway", () => {
    const fresh = spendPace(usage({ cycleStart: NOW - 3_600_000, cycleEnd: NOW + 29 * DAY, spendCents: 300, planLimitCents: 2000 }), NOW)!;
    expect(fresh.elapsedDays).toBe(1);
    expect(fresh.perDay).toBe(300);

    const broke = spendPace(usage({ ...cycle, spendCents: 2250, planLimitCents: 2000 }), NOW)!;
    expect(broke.remaining).toBe(-250);
    expect(broke.runwayDays).toBe(0);
  });

  it("gives up without a cycle or a spend figure, and degrades without a budget", () => {
    expect(spendPace(usage({ spendCents: 100 }), NOW)).toBeNull();
    expect(spendPace(usage({ ...cycle }), NOW)).toBeNull();
    const p = spendPace(usage({ ...cycle, spendCents: 840 }), NOW)!;
    expect(p.budget).toBeNull();
    expect(p.remaining).toBeNull();
    expect(p.perDay).toBe(70);
  });
});

describe("startOfLocalDay / daysText / moneyShort", () => {
  it("snaps to local midnight", () => {
    const at = startOfLocalDay(NOW);
    const d = new Date(at);
    expect([d.getHours(), d.getMinutes(), d.getSeconds(), d.getMilliseconds()]).toEqual([0, 0, 0, 0]);
    expect(NOW - at).toBeLessThan(DAY);
    expect(NOW - at).toBeGreaterThanOrEqual(0);
  });

  it("rounds days to what a glance needs", () => {
    expect(daysText(0.4)).toBe("不到 1 天");
    expect(daysText(1.26)).toBe("1.5 天");
    expect(daysText(3)).toBe("3 天");
    expect(daysText(16.57)).toBe("17 天");
  });

  it("shortens big amounts and keeps cents on small ones", () => {
    expect(moneyShort(840)).toBe("$8.40");
    expect(moneyShort(12_345)).toBe("$123");
    expect(moneyShort(123_456)).toBe("$1.2k");
    expect(moneyShort(null)).toBe("—");
  });
});

describe("planGroup", () => {
  it("groups Cursor's raw plan names into the filter's buckets", () => {
    expect(planGroup(usage({ plan: "ultra" }))).toBe("ultra");
    expect(planGroup(usage({ plan: "pro_plus" }))).toBe("proplus");
    expect(planGroup(usage({ plan: "pro" }))).toBe("pro");
    expect(planGroup(usage({ plan: "free" }))).toBe("free");
    expect(planGroup(usage({ plan: "free_trial" }))).toBe("free");
    expect(planGroup(usage({ plan: "enterprise" }))).toBe("team");
  });

  it("checks plus before pro — pro_plus contains both", () => {
    expect(planGroup("pro_plus")).toBe("proplus");
    expect(planGroup("ProPlus")).toBe("proplus");
  });

  it("never passes an unchecked or unrecognised plan off as Free", () => {
    // 同 poolState 那条规矩：「没查过」不该混进「正常」。
    expect(planGroup(usage())).toBe("unknown");
    expect(planGroup(null)).toBe("unknown");
    expect(planGroup("")).toBe("unknown");
    expect(planGroup(usage({ plan: "super_duper" }))).toBe("unknown");
  });

  it("keeps planTone's palette unchanged — unknown wears Free's grey", () => {
    expect(planTone(usage())).toBe("plan-free");
    expect(planTone(usage({ plan: "super_duper" }))).toBe("plan-free");
  });
});

describe("pctText", () => {
  it("never renders a real usage as 0%", () => {
    // 0% 会被读成「一次没用过」。
    expect(pctText(0.3)).toBe("<1%");
    expect(pctText(0)).toBe("0%");
  });

  it("rounds and marks unknown", () => {
    expect(pctText(42.4)).toBe("42%");
    expect(pctText(99.6)).toBe("100%");
    expect(pctText(undefined)).toBe("—");
    expect(pctText(null)).toBe("—");
  });
});

describe("meter", () => {
  it("colours by severity at the same thresholds as the ops panel", () => {
    expect(meterColor(70)).toBe("var(--warn)");
    expect(meterColor(90)).toBe("var(--warn)");
    expect(meterColor(91)).toBe("var(--bad)");
  });

  it("leaves the healthy range colourless", () => {
    // 一屏十几二十张卡、每张三条，健康的占绝大多数。全染成品牌绿之后
    // 「有颜色 = 要看的」这条规矩就废了，红的还得在一片绿里抢注意力。
    expect(meterColor(10)).toBe("var(--track-fill)");
    expect(meterColor(69)).toBe("var(--track-fill)");
  });

  it("keeps a sliver visible for tiny usage", () => {
    // 「用了一点」和「完全没用」在视觉上必须不一样。
    expect(meterWidth(0.4)).toBe(2);
    expect(meterWidth(0)).toBe(0);
    expect(meterWidth(undefined)).toBe(0);
    expect(meterWidth(55)).toBe(55);
    expect(meterWidth(140)).toBe(100);
  });
});

describe("untilText", () => {
  it("drops the hours once the span is a week out", () => {
    expect(untilText(NOW + 9 * DAY, NOW)).toBe("还剩 9 天");
    expect(untilText(NOW + 3 * DAY + 5 * 3_600_000, NOW)).toBe("还剩 3 天 5 小时");
    expect(untilText(NOW + 4 * 3_600_000, NOW)).toBe("还剩 4 小时");
    expect(untilText(NOW + 60_000, NOW)).toBe("不到 1 小时");
  });

  it("says expired rather than a negative duration", () => {
    expect(untilText(NOW - DAY, NOW)).toBe("已到期");
    expect(untilText(undefined, NOW)).toBe("—");
  });
});

describe("cycleProgress", () => {
  it("reports how far through the billing period we are", () => {
    const u = usage({ cycleStart: NOW - 3 * DAY, cycleEnd: NOW + DAY });
    expect(cycleProgress(u, NOW)).toBeCloseTo(0.75, 5);
  });

  it("is null when the period is unknown or nonsensical", () => {
    expect(cycleProgress(usage(), NOW)).toBeNull();
    expect(cycleProgress(usage({ cycleStart: NOW, cycleEnd: NOW - DAY }), NOW)).toBeNull();
  });

  it("clamps outside the period instead of overflowing the bar", () => {
    const past = usage({ cycleStart: NOW - 10 * DAY, cycleEnd: NOW - DAY });
    expect(cycleProgress(past, NOW)).toBe(1);
  });
});

describe("freshness", () => {
  it("flags a just-reset period and an imminent one", () => {
    expect(isFresh(NOW - 3_600_000, NOW)).toBe(true);
    expect(isFresh(NOW - 5 * DAY, NOW)).toBe(false);
    expect(isEndingSoon(NOW + 3_600_000, NOW)).toBe(true);
    expect(isEndingSoon(NOW + 5 * DAY, NOW)).toBe(false);
    // 已经过去的不算「即将」。
    expect(isEndingSoon(NOW - 1000, NOW)).toBe(false);
  });
});

describe("money and on-demand", () => {
  it("renders cents as dollars", () => {
    expect(money(2000)).toBe("$20.00");
    expect(money(undefined)).toBe("—");
  });

  it("distinguishes uncapped from unknown", () => {
    // null + enabled = 不封顶，不是「没有数据」。
    expect(onDemandText(usage({ onDemandEnabled: true, onDemandUsedCents: 315 }))).toEqual({
      value: "$3.15",
      sub: "不封顶",
    });
    expect(
      onDemandText(usage({ onDemandEnabled: true, onDemandUsedCents: 315, onDemandLimitCents: 5000 })),
    ).toEqual({ value: "$3.15", sub: "上限 $50.00" });
    expect(onDemandText(usage())).toEqual({ value: "$0.00", sub: "未开启" });
  });

  it("packs the same three cases into one line for the card", () => {
    // 卡片末行只有一行的宽度，所以上限省掉分位；但「没开」仍然要说出来。
    expect(onDemandParts(usage({ onDemandEnabled: true, onDemandUsedCents: 315 }))).toEqual({
      k: "按需",
      v: "$3.15",
      sub: "不封顶",
    });
    expect(
      onDemandParts(usage({ onDemandEnabled: true, onDemandUsedCents: 315, onDemandLimitCents: 5000 })),
    ).toEqual({ k: "按需", v: "$3.15 / $50" });
    expect(onDemandParts(usage())).toEqual({ k: "按需未开启" });
  });
});

describe("blockReasonText", () => {
  it("strips Cursor's enum prefix so it fits in a card", () => {
    expect(blockReasonText("SAND_ACCESS_BLOCK_REASON_ABUSE")).toBe("abuse");
    expect(blockReasonText("SAND_ACCESS_BLOCK_REASON_TRIAL_USER")).toBe("trial user");
    expect(blockReasonText(null)).toBe("");
  });
});

describe("accountProblem", () => {
  const active = { status: "active" as const };

  it("reports credential problems", () => {
    expect(accountProblem({ status: "dead" }, usage({ totalPercentUsed: 1 }))).toEqual({
      tone: "bad",
      label: "已失效",
    });
    expect(accountProblem({ status: "needs_login" })).toEqual({ tone: "warn", label: "待登录" });
  });

  it("surfaces a blocked or exhausted Bot channel", () => {
    // 月额度满格也救不了一个没权限的 Bot 通道。
    expect(accountProblem(active, usage({ totalPercentUsed: 2, bot: { access: "blocked" } }))).toEqual({
      tone: "bad",
      label: "Bot 无权限",
    });
    expect(
      accountProblem(active, usage({ totalPercentUsed: 2, bot: { hasAvailable: false } })),
    ).toEqual({ tone: "bad", label: "Bot 已耗尽" });
  });

  it("says nothing when the account is merely busy or fine", () => {
    // 「额度将尽」不再是一个结论 —— 四个桶的数字自己会说话（见 allBuckets）。
    expect(accountProblem(active, usage({ totalPercentUsed: 15, apiPercentUsed: 96 }))).toBeNull();
    // 「可用」也不说：一列里绝大多数都可用，标了等于没标。
    expect(accountProblem(active, usage({ totalPercentUsed: 15 }))).toBeNull();
    expect(accountProblem(active)).toBeNull();
  });
});

describe("railTone", () => {
  const active = { status: "active" as const };

  it("follows the overall quota, not a single exhausted bucket", () => {
    // API 常年 100% 是这批号的常态：Auto 那条路还通着，号还能用，不该判红。
    expect(railTone(active, usage({ totalPercentUsed: 15, apiPercentUsed: 100 }))).toBe("ok");
    expect(railTone(active, usage({ totalPercentUsed: 75, apiPercentUsed: 100 }))).toBe("warn");
    expect(railTone(active, usage({ totalPercentUsed: 100 }))).toBe("bad");
  });

  it("falls back to the tightest bucket when the total is missing", () => {
    expect(railTone(active, usage({ apiPercentUsed: 96 }))).toBe("bad");
    expect(overallPercent(usage({ apiPercentUsed: 96 }))).toBe(96);
    expect(overallPercent(usage({ totalPercentUsed: 15, apiPercentUsed: 96 }))).toBe(15);
  });

  it("is neutral without usage and takes problems first", () => {
    expect(railTone(active)).toBe("none");
    expect(overallPercent(null)).toBeNull();
    expect(railTone({ status: "dead" }, usage({ totalPercentUsed: 1 }))).toBe("bad");
    expect(railTone({ status: "needs_login" })).toBe("warn");
  });
});

describe("allBuckets", () => {
  it("returns every bucket so Auto and API are never hidden", () => {
    const rows = allBuckets(usage({ totalPercentUsed: 15, autoPercentUsed: 3, apiPercentUsed: 96 }));
    expect(rows.map((b) => b.key)).toEqual(["bot", "total", "auto", "api"]);
    expect(rows.find((b) => b.key === "auto")?.percent).toBe(3);
  });

  it("keeps a missing bucket null rather than calling it 0%", () => {
    const rows = allBuckets(usage({ totalPercentUsed: 15 }));
    expect(rows.find((b) => b.key === "auto")?.percent).toBeNull();
  });

  it("describes the Bot channel's special states in words", () => {
    expect(allBuckets(usage({}))[0]).toMatchObject({ note: "无", percent: null });
    expect(allBuckets(usage({ bot: { access: "blocked" } }))[0]).toMatchObject({ note: "无权限" });
    // 耗尽就是满格，条子也该是满的。
    expect(allBuckets(usage({ bot: { hasAvailable: false } }))[0]).toMatchObject({
      note: "已耗尽",
      percent: 100,
    });
  });

  it("gives the Bot bucket its own weekly reset, the rest the billing cycle", () => {
    const rows = allBuckets(
      usage({ cycleEnd: NOW + 20 * DAY, bot: { percentUsed: 5, resetAt: NOW + 2 * DAY } }),
    );
    expect(rows[0]?.resetAt).toBe(NOW + 2 * DAY);
    expect(rows[1]?.resetAt).toBe(NOW + 20 * DAY);
  });
});

describe("cardBuckets", () => {
  it("drops 总额度 —— 那是结论，由卡上那颗状态点在说", () => {
    const rows = cardBuckets(usage({ totalPercentUsed: 15, autoPercentUsed: 3, apiPercentUsed: 96 }));
    expect(rows.map((b) => b.key)).toEqual(["bot", "auto", "api"]);
  });

  it("其余三个桶原样保留，包括 Bot 的特殊状态", () => {
    const rows = cardBuckets(usage({ autoPercentUsed: 3, bot: { access: "blocked" } }));
    expect(rows[0]).toMatchObject({ key: "bot", note: "无权限" });
    expect(rows.find((b) => b.key === "auto")?.percent).toBe(3);
    // 缺数仍然是 null，不能画成 0%。
    expect(rows.find((b) => b.key === "api")?.percent).toBeNull();
  });
});

describe("worstBucket", () => {
  it("挑最紧的那个桶，不是总量", () => {
    // 总量才 15%，但 API 已经 97% —— 这个号接不了任何 named model。
    const b = worstBucket(usage({ totalPercentUsed: 15, autoPercentUsed: 20, apiPercentUsed: 97 }));
    expect(b?.key).toBe("api");
    expect(b?.percent).toBe(97);
  });

  it("Bot 周额也参与比较，并带自己的重置时刻", () => {
    const b = worstBucket(
      usage({
        totalPercentUsed: 10,
        cycleEnd: NOW + 20 * DAY,
        bot: { percentUsed: 88, resetAt: NOW + 2 * DAY },
      }),
    );
    expect(b?.key).toBe("bot");
    // Bot 按周、其余按月，重置时刻必须跟着桶走。
    expect(b?.resetAt).toBe(NOW + 2 * DAY);
  });

  it("月账期的桶用 cycleEnd 作为重置时刻", () => {
    const b = worstBucket(usage({ apiPercentUsed: 60, cycleEnd: NOW + 9 * DAY }));
    expect(b?.key).toBe("api");
    expect(b?.resetAt).toBe(NOW + 9 * DAY);
  });

  it("缺席的桶不参与比较，不会被当成 0", () => {
    // 只有 Auto 有数：不能因为其余是 undefined 就挑出一个 0% 的桶。
    const b = worstBucket(usage({ autoPercentUsed: 5 }));
    expect(b?.key).toBe("auto");
  });

  it("完全没有用量时什么都不给", () => {
    expect(worstBucket(usage())).toBeNull();
    expect(worstBucket(null)).toBeNull();
  });
});

describe("shortDate", () => {
  it("pads to a fixed width so columns line up", () => {
    expect(shortDate(Date.parse("2026-09-05T00:00:00Z"))).toMatch(/^\d{2}\/\d{2}$/);
    expect(shortDate(undefined)).toBe("—");
  });
});
