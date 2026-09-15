import { describe, expect, it } from "vitest";
import type { Account, SwitchProfile } from "../ipc/types";
import { buildSwitchPool, canAddToSwitchPool, listAvailableSwitchAccounts } from "./switcher";

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

    expect(candidates.map((candidate) => candidate.email)).toEqual([
      available.email,
      sessionLive.email,
    ]);
  });
});

describe("canAddToSwitchPool", () => {
  it("lets a live session-token account join, but not an expired or dead one", () => {
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
        account("expired@example.com", {
          hasRefresh: false,
          hasAccess: true,
          accessExpiresAt: "2020-01-01T00:00:00Z",
        }),
      ),
    ).toBe(false);
    expect(
      canAddToSwitchPool(
        account("dead@example.com", {
          hasRefresh: false,
          hasAccess: true,
          accessExpiresAt: "2099-01-01T00:00:00Z",
          status: "dead",
        }),
      ),
    ).toBe(false);
  });
});
