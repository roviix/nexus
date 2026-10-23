import { describe, expect, it } from "vitest";
import type { QoderUsage } from "../../ipc/types";
import { primaryBucket, qoderPlan, qoderRegion, qoderTitle, usedPercent } from "./qoder";

const usage = (patch: Partial<QoderUsage>): QoderUsage => ({
  plan: "Pro",
  userType: "personal",
  percent: 42,
  exceeded: false,
  unlimited: false,
  resetAt: null,
  unit: "Credits",
  buckets: [
    { id: "plan", label: "套餐额度", used: 1260, total: 3000, remaining: 1740, percent: 42, unit: "Credits" },
    { id: "addon", label: "加购额度", used: 10, total: 500, remaining: 490, percent: 2, unit: "Credits" },
  ],
  checkedAt: "2026-10-11T00:00:00Z",
  ...patch,
});

describe("qoder account display", () => {
  it("prefers the email, then the plan bucket's used percent", () => {
    expect(qoderTitle({ email: "a@b.c", displayName: "Ada", label: "label", accountRef: "global:1" })).toBe("a@b.c");
    expect(qoderRegion("cn")).toBe("国内版");
    expect(qoderRegion("global")).toBe("国际版");
    expect(primaryBucket(usage({}))?.id).toBe("plan");
    expect(usedPercent(usage({}))).toBe(42);
  });

  it("shows exhaustion as 100 and leaves a missing snapshot blank", () => {
    expect(usedPercent(usage({ exceeded: true, percent: 10 }))).toBe(100);
    expect(usedPercent(null)).toBeNull();
    expect(qoderPlan({ qoderUsage: null } as never)).toBe("未同步套餐");
  });
});
