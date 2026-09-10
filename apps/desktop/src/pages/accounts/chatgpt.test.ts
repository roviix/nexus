import { describe, expect, it } from "vitest";
import type { GatewayCandidate } from "../../ipc/types";
import { labelOf, laneBadge, planClass, windowIsFull, windowLabel } from "./chatgpt";

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

describe("planClass", () => {
  it("maps ChatGPT plan names onto the existing badge tones", () => {
    expect(planClass("plus")).toBe("plan");
    expect(planClass("pro")).toBe("plan plan-pro");
    expect(planClass("team")).toBe("plan plan-team");
    expect(planClass("free")).toBe("plan plan-free");
    expect(planClass(null)).toBeNull();
  });
});
