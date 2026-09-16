import { describe, expect, it } from "vitest";
import type { AccountUsage } from "../ipc/types";
import { copyInfoLine, copyInfoMap, dateMinute, loadCopyChoice, normalizeChoice, saveCopyChoice } from "./copy";

// 本地时区构造，断言里的钟点才不随跑测试的机器变。
const CYCLE_END = new Date(2026, 8, 30, 20, 5).getTime();
const BOT_RESET = new Date(2026, 8, 18, 9, 30).getTime();

function usage(over: Partial<AccountUsage> = {}): AccountUsage {
  return {
    fetchedAt: "2026-09-16T00:00:00Z",
    apiPercentUsed: 46,
    totalPercentUsed: 40,
    onDemandEnabled: true,
    onDemandUsedCents: 120,
    onDemandLimitCents: 2000,
    creditGrantRemainingCents: 10000,
    cycleEnd: CYCLE_END,
    bot: { resetAt: BOT_RESET },
    ...over,
  } as AccountUsage;
}

describe("dateMinute", () => {
  it("月/日 时:分，各两位，本地时区", () => {
    expect(dateMinute(new Date(2026, 0, 5, 7, 3).getTime())).toBe("01/05 07:03");
    expect(dateMinute(CYCLE_END)).toBe("09/30 20:05");
  });
});

describe("copyInfoLine", () => {
  it("没选附加项就是空串，Rust 侧当没有", () => {
    expect(copyInfoLine({ usage: usage() }, [])).toBe("");
  });

  it("按固定顺序拼四段，中间用点隔开", () => {
    const line = copyInfoLine({ usage: usage() }, ["resets", "api", "credits", "on_demand"]);
    expect(line).toBe("API 余 54% · 按需 $1.20 / $20 · 积分 100 · 月额 09/30 20:05 重置 · Bot 09/18 09:30 重置");
  });

  it("只选一项就只有一段", () => {
    expect(copyInfoLine({ usage: usage() }, ["api"])).toBe("API 余 54%");
  });

  it("没查过用量只说一句，不铺四个未知", () => {
    expect(copyInfoLine({ usage: null }, ["api", "on_demand", "credits", "resets"])).toBe("未查用量");
  });

  it("按需没开、没积分、没重置时间各有一句人话", () => {
    const u = usage({
      onDemandEnabled: false,
      creditGrantRemainingCents: 0,
      cycleEnd: undefined,
      bot: undefined,
    });
    expect(copyInfoLine({ usage: u }, ["on_demand", "credits", "resets"])).toBe("按需未开启 · 无积分 · 重置时间未知");
  });

  it("按需不封顶时把「不封顶」接在后面", () => {
    expect(copyInfoLine({ usage: usage({ onDemandLimitCents: null }) }, ["on_demand"])).toBe("按需 $1.20 · 不封顶");
  });

  it("API 桶没单独计量时退到总额度", () => {
    expect(copyInfoLine({ usage: usage({ apiPercentUsed: undefined, totalPercentUsed: 30 }) }, ["api"])).toBe(
      "API 余 70%",
    );
    expect(copyInfoLine({ usage: usage({ apiPercentUsed: undefined, totalPercentUsed: undefined }) }, ["api"])).toBe(
      "API 未知",
    );
    // 超过 100 的已用不会算出负余量。
    expect(copyInfoLine({ usage: usage({ apiPercentUsed: 130 }) }, ["api"])).toBe("API 余 0%");
  });
});

describe("copyInfoMap", () => {
  it("每个号一行，按 id 归位；没选附加项就是空表", () => {
    const list = [
      { id: "a", usage: usage() },
      { id: "b", usage: null },
    ];
    expect(copyInfoMap(list, [])).toEqual({});
    expect(copyInfoMap(list, ["api"])).toEqual({ a: "API 余 54%", b: "未查用量" });
  });
});

describe("copy choice persistence", () => {
  it("不认识的格式回默认，附加项去重并按固定顺序", () => {
    expect(normalizeChoice({ format: "csv", extras: ["resets", "api", "resets", "nope"] })).toEqual({
      format: "email",
      extras: ["api", "resets"],
    });
    expect(normalizeChoice(null)).toEqual({ format: "email", extras: [] });
  });

  it("存了再读回来是同一套", () => {
    const store = new Map<string, string>();
    const storage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
    };
    saveCopyChoice({ format: "email_password", extras: ["on_demand", "api"] }, storage);
    expect(loadCopyChoice(storage)).toEqual({ format: "email_password", extras: ["api", "on_demand"] });
    expect(loadCopyChoice({ getItem: () => "{not json" })).toEqual({ format: "email", extras: [] });
  });
});
