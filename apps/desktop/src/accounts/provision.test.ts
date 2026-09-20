import { describe, expect, it } from "vitest";
import type { Account, ProvisionReport, ProvisionStepReport } from "../ipc/types";
import {
  DEFAULT_PROVISION_PLAN,
  batchSummary,
  estimateTargets,
  loadAutoProvision,
  loadProvisionPlan,
  normalizePlan,
  planIsEmpty,
  reportLine,
  saveAutoProvision,
  saveProvisionPlan,
  togglePlanStep,
} from "./provision";

const LIVE = "2099-01-01T00:00:00Z";
const EXPIRED = "2020-01-01T00:00:00Z";

function account(email: string, over: Partial<Account> = {}): Account {
  return {
    id: email,
    email,
    source: "local",
    status: "active",
    tags: [],
    codeChannel: "auto",
    hasRefresh: false,
    hasAccess: false,
    hasPassword: false,
    hasEmailPassword: false,
    hasRecoveryEmail: false,
    hasApiKey: false,
    createdAt: "2026-09-19T00:00:00Z",
    updatedAt: "2026-09-19T00:00:00Z",
    seq: 1,
    availability: "logged_out",
    ...over,
  } as Account;
}

function steps(...pairs: Array<[ProvisionStepReport["step"], ProvisionStepReport["state"]]>): ProvisionReport {
  return {
    id: "a1",
    email: "a@example.com",
    steps: pairs.map(([step, state]) => ({ step, state })),
  };
}

describe("estimateTargets", () => {
  it("只把要换 session 的号算进那一步：web 型、活着、没 refresh", () => {
    const list = [
      account("web@x.com", { hasAccess: true, accessExpiresAt: LIVE, accessTokenType: "web" }),
      // 已经是桌面 session，不用换。
      account("sess@x.com", { hasAccess: true, accessExpiresAt: LIVE, accessTokenType: "session" }),
      // 有 refresh 的号随时能换一把新鲜 session。
      account("long@x.com", { hasRefresh: true, accessTokenType: "web" }),
      // 过期的 web token 换不了——网站会话也没了。
      account("dead@x.com", { hasAccess: true, accessExpiresAt: EXPIRED, accessTokenType: "web" }),
    ];
    expect(estimateTargets(list).convertSession).toBe(1);
  });

  it("铸 key 那一步：要一把拿得出的会话，已经有 key 的不算", () => {
    const list = [
      account("web@x.com", { hasAccess: true, accessExpiresAt: LIVE, accessTokenType: "web" }),
      account("long@x.com", { hasRefresh: true }),
      account("has@x.com", { hasRefresh: true, hasApiKey: true }),
      account("pw@x.com", { hasPassword: true }),
    ];
    expect(estimateTargets(list).mintApiKey).toBe(2);
  });

  it("按需估不出来就说「看情况」，不硬写一个数", () => {
    // 要不要写取决于上游此刻的按需状态，没查过用量的号根本不知道。
    expect(estimateTargets([account("a@x.com", { hasRefresh: true })]).onDemand).toBeNull();
  });

  it("数据保留那一步要一把能打 dashboard 的会话：crsr_ 不算，只有密码也不算", () => {
    const list = [
      account("web@x.com", { hasAccess: true, accessExpiresAt: LIVE, accessTokenType: "web" }),
      account("long@x.com", { hasRefresh: true }),
      // crsr_ 走不通 dashboard cookie 面。
      account("key@x.com", { hasApiKey: true }),
      account("pw@x.com", { hasPassword: true }),
    ];
    expect(estimateTargets(list).dataRetention).toBe(2);
  });

  it("刷用量那一步连只有 crsr_ 的号一起算：它还能拉逐条花费", () => {
    const list = [
      account("key@x.com", { hasApiKey: true }),
      account("pw@x.com", { hasPassword: true }),
    ];
    expect(estimateTargets(list).refreshUsage).toBe(1);
  });
});

describe("plan", () => {
  it("默认全做、按需不封顶", () => {
    expect(DEFAULT_PROVISION_PLAN).toEqual({
      mintApiKey: true,
      convertSession: true,
      onDemand: true,
      onDemandLimitCents: null,
      dataRetention: true,
      refreshUsage: true,
    });
    expect(planIsEmpty(DEFAULT_PROVISION_PLAN)).toBe(false);
  });

  it("每一项全关才算空计划——那时按钮该禁掉", () => {
    let plan = DEFAULT_PROVISION_PLAN;
    for (const step of [
      "mintApiKey",
      "convertSession",
      "onDemand",
      "dataRetention",
      "refreshUsage",
    ] as const) {
      expect(planIsEmpty(plan)).toBe(false);
      plan = togglePlanStep(plan, step);
    }
    expect(planIsEmpty(plan)).toBe(true);
  });

  it("认不出来的值回默认；上限只收正数，0 和负数当不封顶", () => {
    expect(normalizePlan({ convertSession: "yes", onDemandLimitCents: 5000 })).toEqual({
      ...DEFAULT_PROVISION_PLAN,
      onDemandLimitCents: 5000,
    });
    expect(normalizePlan({ onDemandLimitCents: 0 }).onDemandLimitCents).toBeNull();
    expect(normalizePlan({ onDemandLimitCents: -1 }).onDemandLimitCents).toBeNull();
    expect(normalizePlan(null)).toEqual(DEFAULT_PROVISION_PLAN);
  });

  it("存了再读回来是同一套；存坏了回默认", () => {
    const store = new Map<string, string>();
    const storage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
    };
    const plan = { ...DEFAULT_PROVISION_PLAN, convertSession: false, onDemandLimitCents: 2000 };
    saveProvisionPlan(plan, storage);
    expect(loadProvisionPlan(storage)).toEqual(plan);
    expect(loadProvisionPlan({ getItem: () => "{not json" })).toEqual(DEFAULT_PROVISION_PLAN);
  });

  it("导入时的「顺手配置」默认不勾——这几步会动对方账号上的东西", () => {
    const store = new Map<string, string>();
    const storage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
    };
    expect(loadAutoProvision(storage)).toBe(false);
    saveAutoProvision(true, storage);
    expect(loadAutoProvision(storage)).toBe(true);
    saveAutoProvision(false, storage);
    expect(loadAutoProvision(storage)).toBe(false);
  });
});

describe("reportLine", () => {
  it("只讲真做了的那几步，跳过的不占地方", () => {
    const r = steps(["convertSession", "done"], ["mintApiKey", "skipped"], ["onDemand", "failed"]);
    expect(reportLine(r)).toBe("换 session✓ · 开按需✗");
  });

  it("全跳过说一句「本来就配好了」，而不是留一片空白", () => {
    expect(reportLine(steps(["convertSession", "skipped"], ["mintApiKey", "skipped"]))).toBe("本来就配好了");
  });
});

describe("batchSummary", () => {
  it("报有步骤失败的号数，不报失败的步骤数", () => {
    const ok = steps(["mintApiKey", "done"]);
    const bad = steps(["mintApiKey", "done"], ["onDemand", "failed"]);
    expect(batchSummary([ok, ok])).toBe("已配置 2 个账号。");
    expect(batchSummary([ok, bad, bad])).toBe("已配置 3 个账号，其中 2 个有步骤没成功。");
    expect(batchSummary([])).toBe("没有需要配置的账号。");
  });
});
