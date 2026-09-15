import { describe, expect, it } from "vitest";
import {
  collectionLabel,
  discountOffText,
  discountStateLabel,
  durationText,
  intervalLabel,
  invoiceStatusLabel,
} from "./billing";
import { moneyFx } from "./usage";

describe("billing copy", () => {
  it("keeps unknown and none as different words", () => {
    expect(discountStateLabel("unknown")).toBe("未知");
    expect(discountStateLabel("none")).toBe("无折扣");
    expect(discountStateLabel("active")).toBe("折扣生效");
    expect(discountStateLabel("expired")).toBe("折扣已结束");
  });

  it("says how a coupon lasts without inventing a calendar", () => {
    expect(durationText({ duration: "once" })).toBe("一次性");
    expect(durationText({ duration: "forever" })).toBe("长期有效");
    expect(durationText({ duration: "repeating", durationInMonths: 6 })).toBe("重复 6 个月");
  });

  it("formats amount-off and percent-off", () => {
    expect(discountOffText({ amountOff: 20000, currency: "usd" })).toMatch(/−.*200/);
    expect(discountOffText({ percentOff: 50 })).toBe("−50%");
  });

  it("names the common billing intervals and invoice states", () => {
    expect(intervalLabel("month")).toBe("月付");
    expect(intervalLabel("year")).toBe("年付");
    expect(invoiceStatusLabel("paid")).toBe("已付");
    expect(invoiceStatusLabel("uncollectible")).toBe("无法收取");
    expect(collectionLabel("charge_automatically")).toBe("自动续费");
  });
});

describe("moneyFx", () => {
  it("treats yen as a face-value amount", () => {
    expect(moneyFx(2000, "jpy")).toMatch(/2,000|2000/);
    expect(moneyFx(2000, "usd")).toMatch(/20\.00/);
  });
});
