import { describe, expect, it } from "vitest";
import type { Account } from "../ipc/types";
import {
  accountPlanGroup,
  applyFilter,
  applyPlanFilter,
  matchesQuery,
  poolState,
  sortAccounts,
  summarize,
} from "./accounts";
import { resetInShort, worstMonthlyBucket } from "./usage";

const NOW = Date.parse("2026-09-02T12:00:00Z");
const DAY = 86_400_000;

let seq = 0;
function acct(over: Partial<Account> = {}): Account {
  seq += 1;
  return {
    id: `a${seq}`,
    email: `u${seq}@example.com`,
    source: "local",
    status: "active",
    tags: [],
    codeChannel: "auto",
    hasRefresh: true,
    hasAccess: false,
    hasPassword: false,
    hasEmailPassword: false,
    hasRecoveryEmail: false,
    hasApiKey: false,
    createdAt: "2026-09-01T00:00:00Z",
    updatedAt: "2026-09-01T00:00:00Z",
    ...over,
  };
}

const usage = (u: Record<string, unknown>) => ({ fetchedAt: "", ...u });

describe("poolState / filter / summarize", () => {
  it("grades an account by its overall quota, not by one exhausted bucket", () => {
    expect(poolState(acct({ usage: usage({ totalPercentUsed: 20 }) }))).toBe("ok");
    expect(poolState(acct({ usage: usage({ totalPercentUsed: 75 }) }))).toBe("warn");
    expect(poolState(acct({ usage: usage({ totalPercentUsed: 100 }) }))).toBe("full");
    // API 打满是这批号的常态，Auto 那条路还通着 —— 不该把整个号判成已满。
    expect(poolState(acct({ usage: usage({ totalPercentUsed: 20, apiPercentUsed: 100 }) }))).toBe("ok");
  });

  it("does not pass off an unchecked account as normal", () => {
    // 「没查过」和「查过、没问题」是两件事。
    expect(poolState(acct())).toBe("unknown");
  });

  it("folds credential problems into the same grades", () => {
    expect(poolState(acct({ status: "dead" }))).toBe("full");
    expect(poolState(acct({ status: "needs_login" }))).toBe("warn");
    expect(poolState(acct({ usage: usage({ bot: { hasAvailable: false } }) }))).toBe("full");
  });

  it("counts every account into exactly one grade so the bar adds up", () => {
    const list = [
      acct({ usage: usage({ totalPercentUsed: 10 }) }),
      acct({ usage: usage({ totalPercentUsed: 80 }) }),
      acct({ status: "dead" }),
      acct({ hasRefresh: false }),
    ];
    const s = summarize(list);
    expect(s.by).toEqual({ ok: 1, warn: 1, full: 1, unknown: 1 });
    expect(s.by.ok + s.by.warn + s.by.full + s.by.unknown).toBe(s.total);
    expect(s.refreshable).toBe(3);
    expect(applyFilter(list, "ok")).toHaveLength(1);
    expect(applyFilter(list, "unknown")).toHaveLength(1);
    expect(applyFilter(list, "all")).toHaveLength(4);
  });
});

describe("applyPlanFilter", () => {
  it("reads the plan from usage first, then the recorded membership — same as the card badge", () => {
    // 用量里的 plan 是刷新后 Cursor 回的最新档；membership 是入库时记下的旧档。
    const fresh = acct({ membership: "pro", usage: usage({ plan: "ultra" }) });
    const stale = acct({ membership: "pro" });
    expect(accountPlanGroup(fresh)).toBe("ultra");
    expect(accountPlanGroup(stale)).toBe("pro");
  });

  it("keeps unchecked accounts out of every real tier", () => {
    const ultra = acct({ usage: usage({ plan: "ultra" }) });
    const free = acct({ usage: usage({ plan: "free" }) });
    const neverChecked = acct();
    const list = [ultra, free, neverChecked];
    expect(applyPlanFilter(list, "ultra").map((a) => a.id)).toEqual([ultra.id]);
    expect(applyPlanFilter(list, "free").map((a) => a.id)).toEqual([free.id]);
    expect(applyPlanFilter(list, "unknown").map((a) => a.id)).toEqual([neverChecked.id]);
    expect(applyPlanFilter(list, "all")).toHaveLength(3);
  });
});

describe("matchesQuery", () => {
  it("searches email, note and tags case-insensitively", () => {
    const a = acct({ email: "Alice@Example.com", note: "给小王用", tags: ["Team-A"] });
    expect(matchesQuery(a, "alice")).toBe(true);
    expect(matchesQuery(a, "小王")).toBe(true);
    expect(matchesQuery(a, "team-a")).toBe(true);
    expect(matchesQuery(a, "bob")).toBe(false);
    expect(matchesQuery(a, "   ")).toBe(true);
  });
});

describe("sortAccounts · 添加时间（默认）", () => {
  it("orders by when the account was added, oldest first", () => {
    const late = acct({ createdAt: "2026-09-02T00:00:00Z" });
    const early = acct({ createdAt: "2026-08-01T00:00:00Z" });
    const mid = acct({ createdAt: "2026-08-20T00:00:00Z" });
    expect(sortAccounts([late, early, mid], "added").map((a) => a.id)).toEqual([
      early.id,
      mid.id,
      late.id,
    ]);
  });

  it("never moves an account because its usage or status changed", () => {
    // 这是这一档存在的理由：刷新用量 / 号失效都不该让列表重排。
    const a = acct({ createdAt: "2026-08-01T00:00:00Z" });
    const b = acct({ createdAt: "2026-08-02T00:00:00Z" });
    const c = acct({ createdAt: "2026-08-03T00:00:00Z" });
    const before = sortAccounts([a, b, c], "added").map((x) => x.id);

    const refreshed = [
      { ...a, usage: usage({ totalPercentUsed: 99 }), lastCheckedAt: "2026-09-02T12:00:00Z" },
      { ...b, status: "dead" as const },
      { ...c, usage: usage({ totalPercentUsed: 1 }) },
    ];
    expect(sortAccounts(refreshed, "added").map((x) => x.id)).toEqual(before);
  });

  it("does not depend on the order the backend happened to return", () => {
    const a = acct({ createdAt: "2026-08-01T00:00:00Z" });
    const b = acct({ createdAt: "2026-08-02T00:00:00Z" });
    expect(sortAccounts([a, b], "added").map((x) => x.id)).toEqual(
      sortAccounts([b, a], "added").map((x) => x.id),
    );
  });
});

describe("sortAccounts", () => {
  it("puts dead accounts last regardless of the chosen order", () => {
    const dead = acct({ status: "dead", usage: usage({ cycleEnd: NOW + DAY }) });
    const live = acct({ usage: usage({ cycleEnd: NOW + 20 * DAY }) });
    expect(sortAccounts([dead, live], "reset", NOW).map((a) => a.id)).toEqual([live.id, dead.id]);
  });

  it("orders by the monthly cycle end only, never mixing in the Bot week", () => {
    // 这正是以前排错的那种局面：monthly 的 Bot 桶最紧、3 天后重置，但它的月账期 20 天后才到；
    // 按「最紧的桶」排它会跑到 5 天后重置的 later 前面，而卡上写的却是「20 天后重置」。
    const monthly = acct({
      usage: usage({ totalPercentUsed: 10, cycleEnd: NOW + 20 * DAY, bot: { percentUsed: 90, resetAt: NOW + 3 * DAY } }),
    });
    const later = acct({ usage: usage({ totalPercentUsed: 50, cycleEnd: NOW + 5 * DAY }) });
    const unknown = acct({ usage: usage({ totalPercentUsed: 5 }) });
    expect(sortAccounts([monthly, unknown, later], "reset", NOW).map((x) => x.id)).toEqual([
      later.id,
      monthly.id,
      unknown.id,
    ]);
  });

  it("treats an already-passed cycle end as 'reset now' and puts it first", () => {
    const stale = acct({ usage: usage({ cycleEnd: NOW - DAY }) });
    const soon = acct({ usage: usage({ cycleEnd: NOW + DAY }) });
    expect(sortAccounts([soon, stale], "reset", NOW).map((x) => x.id)).toEqual([stale.id, soon.id]);
  });

  it("orders by the Bot channel's own reset, ignoring the other buckets", () => {
    // 这一档存在的理由：按「最紧的桶」排时，Bot 常被一个打满的 API 挡在后面，
    // 「哪个号的 Bot 快回来了」就永远问不出来。
    const botSoon = acct({
      usage: usage({ totalPercentUsed: 10, apiPercentUsed: 100, cycleEnd: NOW + 30 * DAY, bot: { percentUsed: 100, resetAt: NOW + DAY } }),
    });
    const botLater = acct({
      usage: usage({ totalPercentUsed: 90, cycleEnd: NOW + DAY, bot: { percentUsed: 20, resetAt: NOW + 6 * DAY } }),
    });
    // 没有 Bot 通道的沉底：它回答不了这个问题。
    const noBot = acct({ usage: usage({ totalPercentUsed: 5, cycleEnd: NOW + 2 * DAY }) });
    expect(sortAccounts([botLater, noBot, botSoon], "botReset", NOW).map((x) => x.id)).toEqual([
      botSoon.id,
      botLater.id,
      noBot.id,
    ]);
  });

  it("is stable within a rank so the list does not shuffle on every reload", () => {
    // 四个号都没查过用量，重置时刻全是「不知道」：同分时按 id，不按后端给的顺序。
    const a = acct();
    const b = acct();
    const c = acct();
    expect(sortAccounts([c, a, b], "reset").map((x) => x.id)).toEqual([a.id, b.id, c.id]);
  });

  it("puts the most recently checked first", () => {
    const old = acct({ lastCheckedAt: "2026-09-01T00:00:00Z" });
    const fresh = acct({ lastCheckedAt: "2026-09-02T00:00:00Z" });
    const never = acct();
    expect(sortAccounts([never, old, fresh], "checked").map((a) => a.id)).toEqual([
      fresh.id,
      old.id,
      never.id,
    ]);
  });
});

describe("worstMonthlyBucket", () => {
  it("ignores the Bot channel", () => {
    const b = worstMonthlyBucket(
      usage({ totalPercentUsed: 10, autoPercentUsed: 30, bot: { percentUsed: 99 } }),
    );
    expect(b?.key).toBe("auto");
  });

  it("is null when no monthly bucket is known", () => {
    expect(worstMonthlyBucket(usage({ bot: { percentUsed: 50 } }))).toBeNull();
  });
});

describe("resetInShort", () => {
  it("phrases a countdown as a reset but leaves the sentinel strings alone", () => {
    expect(resetInShort(NOW + 3 * DAY, NOW)).toBe("3 天后重置");
    expect(resetInShort(NOW - 1000, NOW)).toBe("已重置");
    expect(resetInShort(undefined, NOW)).toBe("—");
  });

  // 一天以内换钟点：倒计时到了当天只剩「3 小时」这种粗粒度，钟点才答得了「几点回来」。
  it("switches to a clock time inside the last day", () => {
    const at = NOW + 3 * 3_600_000;
    const hhmm = new Date(at).toTimeString().slice(0, 5);
    expect(resetInShort(at, NOW)).toBe(`${hhmm} 重置`);
  });

  it("marks tomorrow when the reset is past midnight", () => {
    // 本地时间的当天最后一刻起算，19 小时后必然落到第二天。
    const midnight = new Date(NOW);
    midnight.setHours(23, 0, 0, 0);
    const now = midnight.getTime();
    expect(resetInShort(now + 5 * 3_600_000, now)).toBe("明天 04:00 重置");
  });
});
