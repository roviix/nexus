import { describe, expect, it } from "vitest";
import {
  accountSourceLabel,
  accountStatusLabel,
  bytes,
  maskEmail,
  membershipLabel,
  money,
  orderStatusLabel,
  timeAgo,
  timeUntil,
  yuan,
} from "./format";

const NOW = Date.parse("2026-09-02T12:00:00Z");

describe("timeAgo", () => {
  it("scales the unit to the distance", () => {
    expect(timeAgo("2026-09-02T11:59:30Z", NOW)).toBe("30 秒前");
    expect(timeAgo("2026-09-02T11:45:00Z", NOW)).toBe("15 分钟前");
    expect(timeAgo("2026-09-02T09:00:00Z", NOW)).toBe("3 小时前");
    expect(timeAgo("2026-08-30T12:00:00Z", NOW)).toBe("3 天前");
  });

  it("falls back to a date once it is far in the past", () => {
    expect(timeAgo("2025-01-01T00:00:00Z", NOW)).toMatch(/2025/);
  });

  it("never renders a negative duration", () => {
    // 机器时钟偏一点是常事，显示「-3 秒前」会让人以为程序坏了。
    expect(timeAgo("2026-09-02T12:00:05Z", NOW)).toBe("刚刚");
  });

  it("degrades to a dash instead of NaN", () => {
    expect(timeAgo(null, NOW)).toBe("—");
    expect(timeAgo(undefined, NOW)).toBe("—");
    expect(timeAgo("昨天", NOW)).toBe("—");
  });
});

describe("timeUntil", () => {
  it("counts forward and reports a lapsed reset", () => {
    expect(timeUntil(NOW + 30 * 60_000, NOW)).toBe("30 分钟后");
    expect(timeUntil(NOW + 5 * 3600_000, NOW)).toBe("5 小时后");
    expect(timeUntil(NOW + 3 * 86400_000, NOW)).toBe("3 天后");
    expect(timeUntil(NOW - 1000, NOW)).toBe("已重置");
  });

  it("rounds sub-minute waits up so it never shows 0 分钟后", () => {
    expect(timeUntil(NOW + 20_000, NOW)).toBe("1 分钟后");
  });

  it("handles a missing reset time", () => {
    expect(timeUntil(undefined, NOW)).toBe("—");
    expect(timeUntil(null, NOW)).toBe("—");
  });
});

describe("money", () => {
  it("renders cents as dollars", () => {
    expect(money(2000)).toBe("$20.00");
    expect(money(1234)).toBe("$12.34");
    expect(money(0)).toBe("$0.00");
  });

  it("distinguishes absent from zero", () => {
    expect(money(undefined)).toBe("—");
    expect(money(null)).toBe("—");
  });
});

describe("yuan", () => {
  it("renders shop prices", () => {
    expect(yuan(128)).toBe("¥128.00");
    expect(yuan(0)).toBe("¥0.00");
    expect(yuan(undefined)).toBe("—");
  });
});

describe("bytes", () => {
  it("picks the unit a person would", () => {
    expect(bytes(512)).toBe("512 B");
    expect(bytes(4_300)).toBe("4.2 KB");
    expect(bytes(412 * 1024)).toBe("412 KB");
    expect(bytes(3 * 1024 * 1024 + 200 * 1024)).toBe("3.2 MB");
  });

  it("degrades to a dash", () => {
    expect(bytes(undefined)).toBe("—");
    expect(bytes(-1)).toBe("—");
  });
});

describe("maskEmail", () => {
  it("keeps enough to recognise, not enough to read", () => {
    expect(maskEmail("alexander@example.com")).toBe("al****er@example.com");
    expect(maskEmail("ab@example.com")).toBe("a****@example.com");
  });

  it("leaves a malformed address alone rather than mangling it", () => {
    expect(maskEmail("not-an-email")).toBe("not-an-email");
  });
});

describe("labels", () => {
  it("translates the states the user sees", () => {
    expect(accountStatusLabel("active")).toBe("可用");
    expect(accountStatusLabel("needs_login")).toBe("待登录");
    expect(accountStatusLabel("dead")).toBe("已失效");
    expect(accountSourceLabel("purchased")).toBe("已购");
    expect(orderStatusLabel("ship_failed")).toBe("发货失败");
  });

  it("passes unknown values through instead of blanking them", () => {
    // 服务端加了新状态时，宁可显示原文也不要显示空白。
    expect(accountStatusLabel("quarantined")).toBe("quarantined");
    expect(orderStatusLabel("chargeback")).toBe("chargeback");
    expect(accountSourceLabel("gifted")).toBe("gifted");
  });

  it("makes Cursor's plan names readable", () => {
    expect(membershipLabel("pro_plus")).toBe("pro plus");
    expect(membershipLabel("ultra")).toBe("ultra");
    expect(membershipLabel(null)).toBe("—");
  });
});
