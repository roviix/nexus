/**
 * 「IDE 面板拦截」卡里两个纯函数的护栏：token 数怎么缩、会话 id 怎么截。
 * 表格一行要放得下几十万 token 的上下文和一个 UUID，全写出来一行就没了。
 */
import { describe, expect, it } from "vitest";
import { fmtTokens, shortConversation } from "./InterceptCard";

describe("fmtTokens", () => {
  it("keeps small numbers exact and shrinks big ones", () => {
    expect(fmtTokens(0)).toBe("0");
    expect(fmtTokens(9_999)).toBe("9,999");
    expect(fmtTokens(10_000)).toBe("10k");
    expect(fmtTokens(412_338)).toBe("412k");
    expect(fmtTokens(1_000_000)).toBe("1.0M");
    expect(fmtTokens(2_154_159)).toBe("2.2M");
  });
});

describe("shortConversation", () => {
  it("shows a dash when the request carried no conversation id", () => {
    expect(shortConversation(null)).toBe("—");
  });

  it("keeps the first eight characters of a uuid and short ids as they are", () => {
    expect(shortConversation("8f1c2a9e-4b7d-4e2a-9c1f-000000000001")).toBe("8f1c2a9e");
    expect(shortConversation("conv-42")).toBe("conv-42");
  });
});
