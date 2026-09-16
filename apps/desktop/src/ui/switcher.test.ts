import { describe, expect, it } from "vitest";
import type { Account, SwitchProfile } from "../ipc/types";
import { buildSwitchPool, canAddToSwitchPool, canMintApiKey, listAvailableSwitchAccounts, switchNeedsWebConversion } from "./switcher";

function account(
  email: string,
  overrides: Partial<Account> = {},
): Account {
  return {
    id: email,
    email,
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
    seq: 1,
    availability: "long_lived",
    ...overrides,
  };
}

function profile(
  email: string,
  overrides: Partial<SwitchProfile> = {},
): SwitchProfile {
  return {
    id: `profile-${email}`,
    email,
    machineIds: {
      "telemetry.machineId": "machine",
      "telemetry.macMachineId": "mac-machine",
      "telemetry.devDeviceId": "device",
      "telemetry.sqmId": "sqm",
    },
    createdAt: "2026-09-01T00:00:00Z",
    updatedAt: "2026-09-01T00:00:00Z",
    hasAuth: true,
    isCurrent: false,
    ...overrides,
  };
}

describe("buildSwitchPool", () => {
  it("only includes explicitly enrolled profiles", () => {
    const enrolled = account("enrolled@example.com");
    const accountOnly = account("account-only@example.com");

    const pool = buildSwitchPool(
      [profile(enrolled.email)],
      [enrolled, accountOnly],
      accountOnly.email,
    );

    expect(pool.map((entry) => entry.email)).toEqual([enrolled.email]);
    expect(pool[0]?.account).toBe(enrolled);
    expect(pool[0]?.isCurrent).toBe(false);
  });

  it("puts the current profile first without duplicating it", () => {
    const current = profile("current@example.com");
    const recent = profile("recent@example.com", {
      lastSwitchedAt: "2026-09-03T00:00:00Z",
    });

    const pool = buildSwitchPool(
      [recent, current],
      [],
      "CURRENT@example.com",
    );

    expect(pool.map((entry) => entry.email)).toEqual([
      current.email,
      recent.email,
    ]);
  });
});

describe("listAvailableSwitchAccounts", () => {
  it("only offers usable accounts that are not already enrolled", () => {
    const enrolled = account("enrolled@example.com");
    const available = account("available@example.com");
    const sessionLive = account("session@example.com", {
      hasRefresh: false,
      hasAccess: true,
      accessExpiresAt: "2099-01-01T00:00:00Z",
    });
    const missingRefresh = account("missing@example.com", {
      hasRefresh: false,
    });
    const dead = account("dead@example.com", { status: "dead" });

    const candidates = listAvailableSwitchAccounts(
      [enrolled, sessionLive, missingRefresh, dead, available],
      [profile(enrolled.email)],
    );

    // 活着的 session token 够格：同一把 JWT 写两格就是 Cursor 自己续期后的稳态。
    // 没有任何凭证的、已失效的、已在池里的不列。
    expect(candidates.map((candidate) => candidate.email)).toEqual([
      available.email,
      sessionLive.email,
    ]);
  });
});

describe("canAddToSwitchPool", () => {
  it("admits a session-only account while its JWT is alive, and refuses it once expired", () => {
    expect(
      canAddToSwitchPool(
        account("live@example.com", {
          hasRefresh: false,
          hasAccess: true,
          accessExpiresAt: "2099-01-01T00:00:00Z",
        }),
      ),
    ).toBe(true);
    expect(
      canAddToSwitchPool(
        account("stale@example.com", {
          hasRefresh: false,
          hasAccess: true,
          accessExpiresAt: "2020-01-01T00:00:00Z",
        }),
      ),
    ).toBe(false);
    expect(
      canAddToSwitchPool(account("refresh@example.com", { hasRefresh: true })),
    ).toBe(true);
    expect(
      canAddToSwitchPool(
        account("dead@example.com", { hasRefresh: true, status: "dead" }),
      ),
    ).toBe(false);
    // web-only 也放行——切号时后台自动转成 session（不用碰 crsr）。
    expect(
      canAddToSwitchPool(
        account("web@example.com", {
          hasRefresh: false,
          hasAccess: true,
          accessExpiresAt: "2099-01-01T00:00:00Z",
          accessTokenType: "web",
        }),
      ),
    ).toBe(true);
  });
});

describe("switchNeedsWebConversion", () => {
  it("flags only the live web-only accounts (they convert on first switch)", () => {
    const live = { hasRefresh: false, hasAccess: true, accessExpiresAt: "2099-01-01T00:00:00Z" as const };
    expect(switchNeedsWebConversion(account("w@x.com", { ...live, accessTokenType: "web" }))).toBe(true);
    // session 型直接切，不用转换。
    expect(switchNeedsWebConversion(account("s@x.com", { ...live, accessTokenType: "session" }))).toBe(false);
    // 有 refresh 的号用 refresh 换，不走 web 转换。
    expect(switchNeedsWebConversion(account("r@x.com", { hasRefresh: true, accessTokenType: "web" }))).toBe(false);
    // 过期的 web token 不能转（网站会话也没了）。
    expect(
      switchNeedsWebConversion(account("e@x.com", { hasRefresh: false, hasAccess: true, accessExpiresAt: "2020-01-01T00:00:00Z", accessTokenType: "web" })),
    ).toBe(false);
  });
});

describe("canMintApiKey", () => {
  it("offers the escape hatch exactly to the accounts that need it", () => {
    // 仅会话、access 还活着：这正是唯一能保命的窗口。
    const sessionOnly = account("session@example.com", {
      hasRefresh: false,
      hasAccess: true,
      accessExpiresAt: "2099-01-01T00:00:00Z",
    });
    expect(canMintApiKey(sessionOnly)).toBe(true);

    // access 过期了就铸不出来了——这条路只在 token 还活着时存在。
    expect(
      canMintApiKey({ ...sessionOnly, accessExpiresAt: "2020-01-01T00:00:00Z" }),
    ).toBe(false);
    // 已经有 key 的不用再铸。
    expect(canMintApiKey({ ...sessionOnly, hasApiKey: true })).toBe(false);
    expect(canMintApiKey({ ...sessionOnly, status: "dead" })).toBe(false);
    // 有 refresh 的号也能铸一把当备份。
    expect(
      canMintApiKey(account("refresh@example.com", { hasRefresh: true })),
    ).toBe(true);
  });
});
