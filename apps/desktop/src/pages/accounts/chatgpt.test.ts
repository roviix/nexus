import { describe, expect, it } from "vitest";
import type { ChatGptAccount, GatewayCandidate } from "../../ipc/types";
import {
  chatgptGatewayMembership,
  chatgptHasCredits,
  chatgptLimitShort,
  chatgptOrgLabel,
  chatgptPlanLabel,
  chatgptProblem,
  chatgptRailTone,
  chatgptRenewText,
  chatgptSubscriptionExpired,
  chatgptSubscriptionText,
  chatgptTrafficText,
  chatgptUsable,
  chatgptWindowViews,
  importSummary,
  labelOf,
  laneBadge,
  planClass,
  windowIsFull,
  windowLabel,
} from "./chatgpt";

function candidate(state: GatewayCandidate["state"]): GatewayCandidate {
  return { label: "a@x.com", source: "chatgpt", pinned: false, storedId: "id", percentUsed: null, state };
}

describe("labelOf", () => {
  it("prefers the email and falls back to the account ref tail", () => {
    expect(labelOf({ email: "alice@example.com", accountRef: "acct_0123456789" })).toBe("alice@example.com");
    expect(labelOf({ email: "  ", accountRef: "acct_0123456789" })).toBe("chatgpt…456789");
    expect(labelOf({ email: null, accountRef: "acct_0123456789" })).toBe("chatgpt…456789");
  });
});

describe("windowLabel", () => {
  it("names windows by their real length instead of hardcoding 5h/7d", () => {
    expect(windowLabel(300, "5 小时")).toBe("5 小时");
    expect(windowLabel(10080, "7 天")).toBe("7 天");
    expect(windowLabel(90, "x")).toBe("90 分钟");
    expect(windowLabel(null, "5 小时")).toBe("5 小时");
    expect(windowLabel(0, "7 天")).toBe("7 天");
  });

  it("only calls a window full at 100%", () => {
    expect(windowIsFull({ usedPercent: 100, resetAtMs: 1, windowMinutes: 300 })).toBe(true);
    expect(windowIsFull({ usedPercent: 99.9, resetAtMs: 1, windowMinutes: 300 })).toBe(false);
    expect(windowIsFull(null)).toBe(false);
  });
});

describe("laneBadge", () => {
  it("only flags the states that are news", () => {
    expect(laneBadge(candidate({ kind: "ready" }), true)).toBeNull();
    expect(laneBadge(null, true)).toBeNull();
    expect(laneBadge(candidate({ kind: "current" }), true)).toEqual({ text: "正在用", tone: "ok" });
    expect(laneBadge(candidate({ kind: "exhausted", reason: "r", retryInSecs: 121 }), true)).toEqual({ text: "耗尽 · 3 分后重试", tone: "bad" });
    expect(laneBadge(candidate({ kind: "quota_line" }), true)).toEqual({ text: "额度到线", tone: "bad" });
    expect(laneBadge(candidate({ kind: "cooled", models: ["gpt-5.4"], secsLeft: 59 }), true)).toEqual({ text: "gpt-5.4 冷却 1 分", tone: "default" });
  });

  it("a disabled account is paused whatever the lane says", () => {
    expect(laneBadge(candidate({ kind: "current" }), false)).toEqual({ text: "已暂停", tone: "default" });
  });
});

describe("importSummary", () => {
  it("names created / updated / failed without inventing a total", () => {
    expect(importSummary({ created: 2, updated: 1, failed: 0 })).toBe("新建 2 个 · 更新 1 个");
    expect(importSummary({ created: 0, updated: 0, failed: 3 })).toBe("3 个没进去");
    expect(importSummary({ created: 0, updated: 0, failed: 0 })).toBe("没有导入任何账号");
  });
});

describe("planClass", () => {
  it("maps ChatGPT plan names onto the existing badge tones", () => {
    expect(planClass("plus")).toBe("plan");
    expect(planClass("pro")).toBe("plan plan-pro");
    expect(planClass("team")).toBe("plan plan-team");
    expect(planClass("free")).toBe("plan plan-free");
    expect(planClass(null)).toBeNull();
  });
});

function account(overrides: Partial<ChatGptAccount> = {}): ChatGptAccount {
  return {
    id: "id",
    accountRef: "acct_x",
    email: "a@x.com",
    planType: "plus",
    userId: null,
    organizationId: null,
    organizationTitle: null,
    status: "active",
    enabled: true,
    note: null,
    usage: null,
    billing: null,
    lastCheckedAt: null,
    lastError: null,
    hasRefresh: true,
    accessExpiresAt: null,
    createdAt: "2026-09-01T00:00:00Z",
    updatedAt: "2026-09-01T00:00:00Z",
    ...overrides,
  };
}

describe("chatgptProblem / membership / tone", () => {
  it("names the states a card should surface", () => {
    expect(chatgptProblem(account({ status: "dead" }))).toEqual({ label: "已停用", tone: "bad" });
    expect(chatgptProblem(account({ hasRefresh: false }))).toEqual({ label: "需要重新授权", tone: "warn" });
    expect(
      chatgptProblem(
        account({
          usage: {
            primary: { usedPercent: 100, resetAtMs: 1, windowMinutes: 300 },
            secondary: { usedPercent: 100, resetAtMs: 2, windowMinutes: 10080 },
            planType: "plus",
            checkedAt: "t",
            source: "wham/usage",
          },
        }),
      ),
    ).toEqual({ label: "额度用尽", tone: "bad" });
    expect(chatgptProblem(account())).toBeNull();
  });

  it("maps enabled + lane onto gateway membership without inventing a switcher pool", () => {
    expect(chatgptGatewayMembership(account({ enabled: false }), null)).toBe("available");
    expect(chatgptGatewayMembership(account({ enabled: false, status: "dead" }), null)).toBe("none");
    expect(chatgptGatewayMembership(account(), null)).toBe("enrolled");
    expect(chatgptGatewayMembership(account(), candidate({ kind: "current" }))).toBe("current");
    expect(chatgptGatewayMembership(account(), candidate({ kind: "exhausted", reason: "r", retryInSecs: 10 }))).toBe("skipped");
    expect(chatgptGatewayMembership(account(), candidate({ kind: "quota_line" }))).toBe("skipped");
    expect(chatgptGatewayMembership(account(), candidate({ kind: "cooled", models: ["gpt-5.4"], secsLeft: 30 }))).toBe("enrolled");
  });

  it("paints the rail from the worse of the two windows", () => {
    expect(chatgptRailTone(account({ status: "dead" }))).toBe("bad");
    expect(chatgptRailTone(account({ hasRefresh: false }))).toBe("warn");
    expect(chatgptRailTone(account())).toBe("none");
    expect(
      chatgptRailTone(
        account({
          usage: {
            primary: { usedPercent: 20, resetAtMs: 1, windowMinutes: 300 },
            secondary: { usedPercent: 92, resetAtMs: 2, windowMinutes: 10080 },
            planType: "plus",
            checkedAt: "t",
            source: "wham/usage",
          },
        }),
      ),
    ).toBe("bad");
  });

  it("does not call Plus Pro+", () => {
    expect(chatgptPlanLabel("plus")).toBe("Plus");
    expect(chatgptPlanLabel("pro")).toBe("Pro");
    expect(chatgptPlanLabel("prolite")).toBe("Pro Lite");
    expect(chatgptUsable(account({ status: "needs_login" }))).toBe(false);
  });

  it("keeps both Codex windows even when one is missing", () => {
    const rows = chatgptWindowViews({
      primary: { usedPercent: 8, resetAtMs: 1, windowMinutes: 300 },
      secondary: null,
      planType: "plus",
      checkedAt: "t",
      source: "wham/usage",
    });
    expect(rows.map((r) => r.label)).toEqual(["5 小时", "7 天"]);
    expect(rows[1]?.percent).toBeNull();
  });

  it("names Spark buckets without turning a missing extra window into 0%", () => {
    const rows = chatgptWindowViews({
      primary: { usedPercent: 34, resetAtMs: 1, windowMinutes: 300 },
      secondary: { usedPercent: 37, resetAtMs: 2, windowMinutes: 10080 },
      planType: "pro",
      checkedAt: "t",
      source: "wham/usage",
      additional: [
        {
          name: "GPT-5.3-Codex-Spark",
          feature: "codex_bengalfox",
          allowed: true,
          limitReached: false,
          primary: { usedPercent: 100, resetAtMs: 3, windowMinutes: 300 },
          secondary: { usedPercent: 12, resetAtMs: 4, windowMinutes: 10080 },
        },
      ],
    });
    expect(rows.map((r) => r.label)).toEqual(["Codex 5 小时", "Codex 7 天", "Spark 5 小时", "Spark 7 天"]);
    expect(rows[2]?.percent).toBe(100);
  });
});

describe("identity helpers", () => {
  it("folds Personal into 个人账户 and shortens Spark names", () => {
    expect(chatgptOrgLabel(null)).toBe("个人账户");
    expect(chatgptOrgLabel("Personal")).toBe("个人账户");
    expect(chatgptOrgLabel("Acme Labs")).toBe("Acme Labs");
    expect(chatgptLimitShort("GPT-5.3-Codex-Spark")).toBe("Spark");
    expect(chatgptLimitShort("GPT Reserve")).toBe("Reserve");
    expect(chatgptHasCredits({ hasCredits: false, unlimited: false, overageLimitReached: false, balance: "0", resetAvailable: 0 })).toBe(false);
    expect(chatgptHasCredits({ hasCredits: true, unlimited: false, overageLimitReached: false, balance: "12", resetAvailable: 1 })).toBe(true);
    expect(chatgptTrafficText({ requests: 1600, tokens: 12_000_000, errors: 2, days: 90 })).toBe("1.6K 次 · 12M token");
    expect(chatgptTrafficText({ requests: 0, tokens: 0, errors: 0, days: 90 })).toBeNull();
  });
});

describe("chatgpt subscription copy", () => {
  const NOW = Date.parse("2026-09-13T00:00:00Z");
  const billing = {
    planType: "plus",
    subscriptionPlan: "chatgptplusplan",
    hasActiveSubscription: true as boolean | null,
    expiresAt: "2026-09-25T00:00:00Z",
    willRenew: null as boolean | null,
    billingPeriod: "monthly",
    checkedAt: "t",
    source: "accounts/check",
  };

  it("names remaining days and refuses to invent expiry", () => {
    expect(chatgptSubscriptionText(billing, NOW)).toBe("订阅还剩 12 天");
    expect(chatgptSubscriptionText({ ...billing, expiresAt: "2026-09-13T12:00:00Z" }, NOW)).toBe("订阅今天到期");
    expect(chatgptSubscriptionText({ ...billing, expiresAt: null }, NOW)).toBeNull();
    expect(chatgptRenewText(null)).toBe("会不会续费未知");
    expect(chatgptRenewText(false)).toBe("不会自动续费");
  });

  it("does not call an active renewing plan expired just because the period end passed", () => {
    const ended = { ...billing, expiresAt: "2026-09-01T00:00:00Z", willRenew: true };
    expect(chatgptSubscriptionExpired(ended, NOW)).toBe(false);
    expect(chatgptSubscriptionText(ended, NOW)).toBe("账期已过 · 将续费");
    expect(chatgptSubscriptionExpired({ ...billing, hasActiveSubscription: false, expiresAt: null }, NOW)).toBe(true);
    expect(chatgptProblem(account({ billing: { ...billing, hasActiveSubscription: false, expiresAt: null } }), NOW)).toEqual({
      label: "订阅已过期",
      tone: "bad",
    });
  });
});
