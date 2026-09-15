import { describe, expect, it } from "vitest";
import type { GatewayStatus, SwitchProfile } from "../ipc/types";
import { inGatewayRoster, matchesPoolFilter, poolMembership } from "./pools";

const profile = (email: string): SwitchProfile => ({
  id: email,
  email,
  machineIds: {
    "telemetry.machineId": "",
    "telemetry.macMachineId": "",
    "telemetry.devDeviceId": "",
    "telemetry.sqmId": "",
  },
  createdAt: "2026-09-01T00:00:00Z",
  updatedAt: "2026-09-01T00:00:00Z",
  hasAuth: true,
  refreshIsPlaceholder: false,
  isCurrent: false,
});

const gateway: GatewayStatus = {
  running: null,
  settings: { port: 8787, autostart: false, forceModel: null, defaultChannel: "cursor" },
  restartNeeded: false,
  apiKeySet: true,
  channels: [],
  mediaJobs: [],
  lane: {
    current: "a@x.com",
    candidates: [
      { label: "a@x.com", source: "stored", pinned: false, storedId: "a", percentUsed: 10, state: { kind: "current" } },
      { label: "b@x.com", source: "stored", pinned: false, storedId: "b", percentUsed: null, state: { kind: "ready" } },
    ],
    missing: ["c@x.com"],
    available: [{ label: "d@x.com", source: "stored", pinned: false, percentUsed: null }],
  },
};

describe("poolMembership", () => {
  const profiles = [profile("A@x.com"), profile("d@x.com")];

  it("matches emails case-insensitively across both pools", () => {
    const m = poolMembership("a@X.com", profiles, gateway);
    expect(m.switcher?.email).toBe("A@x.com");
    expect(m.gateway).toBe("current");
  });

  it("tells the four gateway situations apart", () => {
    expect(poolMembership("b@x.com", profiles, gateway).gateway).toBe("enrolled");
    expect(poolMembership("c@x.com", profiles, gateway).gateway).toBe("skipped");
    expect(poolMembership("d@x.com", profiles, gateway).gateway).toBe("available");
    expect(poolMembership("zzz@x.com", profiles, gateway).gateway).toBe("none");
  });

  it("is quiet when the gateway status is unavailable", () => {
    const m = poolMembership("a@x.com", profiles, null);
    expect(m.gateway).toBe("none");
    expect(m.switcher).not.toBeNull();
  });

  it("knows which situations mean 'on the roster'", () => {
    expect(inGatewayRoster("current")).toBe(true);
    expect(inGatewayRoster("enrolled")).toBe(true);
    expect(inGatewayRoster("skipped")).toBe(true);
    expect(inGatewayRoster("available")).toBe(false);
    expect(inGatewayRoster("none")).toBe(false);
  });
});

describe("matchesPoolFilter", () => {
  const profiles = [profile("A@x.com")];
  const m = (email: string) => poolMembership(email, profiles, gateway);

  it("lets everything through on 'any'", () => {
    expect(matchesPoolFilter(m("zzz@x.com"), "any")).toBe(true);
  });

  it("picks out each pool", () => {
    expect(matchesPoolFilter(m("a@x.com"), "switcher")).toBe(true);
    expect(matchesPoolFilter(m("b@x.com"), "switcher")).toBe(false);
    expect(matchesPoolFilter(m("b@x.com"), "gateway")).toBe(true);
    // 「接力时跳过」仍然在名单上，筛网关时要算进去。
    expect(matchesPoolFilter(m("c@x.com"), "gateway")).toBe(true);
    // 「网关能用但没加进去」不算在名单上。
    expect(matchesPoolFilter(m("d@x.com"), "gateway")).toBe(false);
  });

  it("finds the accounts no pool is using", () => {
    expect(matchesPoolFilter(m("zzz@x.com"), "unpooled")).toBe(true);
    expect(matchesPoolFilter(m("d@x.com"), "unpooled")).toBe(true);
    expect(matchesPoolFilter(m("a@x.com"), "unpooled")).toBe(false);
    expect(matchesPoolFilter(m("c@x.com"), "unpooled")).toBe(false);
  });
});