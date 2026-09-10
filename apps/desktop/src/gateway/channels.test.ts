import { describe, expect, it } from "vitest";
import type { ChannelSnapshot, GatewayCandidate, GatewayStatus } from "../ipc/types";
import { channelOf, channelSummary, laneOf, starved } from "./channels";

const candidate = (label: string, state: GatewayCandidate["state"]): GatewayCandidate =>
  ({ label, state, pinned: false, percentUsed: null, source: "grok", storedId: "id" }) as GatewayCandidate;

const channel = (over: Partial<ChannelSnapshot>): ChannelSnapshot => ({
  id: "grok",
  label: "Grok Build",
  vendor: "xai",
  ready: false,
  mediaReady: false,
  lane: { current: null, candidates: [], missing: [], available: [] },
  chatModels: ["grok-4.5"],
  imageModels: ["grok-imagine-image"],
  videoModels: ["grok-imagine-video-1.5"],
  prefixes: ["grok/", "xai/"],
  ...over,
});

const base: GatewayStatus = {
  running: null,
  settings: { port: 8787, passthroughPort: 8788, clientType: "cli", autostart: false, forceModel: null },
  restartNeeded: false,
  apiKeySet: true,
  channels: [],
  mediaJobs: [],
  entrances: [],
  intercept: { rule: { enabled: false, position: "tail", marker: "[nexus-mark]" }, calls: 0, rewritten: 0, errors: 0, recent: [] },
  grokbotStream: { enabled: false, credential: null },
  lane: { current: null, candidates: [], missing: [], available: [] },
};

describe("channelSummary", () => {
  it("says the model family falls back to Cursor when there is no account", () => {
    const s = channelSummary(channel({}));
    expect(s.tone).toBe("default");
    expect(s.text).toContain("grok-*");
    expect(s.text).toContain("Cursor");
  });

  it("counts usable accounts, names the one in use, and reports media readiness", () => {
    const s = channelSummary(
      channel({
        ready: true,
        mediaReady: true,
        lane: {
          current: "a@x.ai",
          candidates: [candidate("a@x.ai", { kind: "current" }), candidate("b@x.ai", { kind: "quota_line" })],
          missing: [],
          available: [],
        },
      }),
    );
    expect(s.tone).toBe("ok");
    expect(s.text).toContain("1 / 2");
    expect(s.text).toContain("a@x.ai");
    expect(s.text).toContain("出图");
  });

  it("warns when every account is out", () => {
    const s = channelSummary(
      channel({ lane: { current: null, candidates: [candidate("a@x.ai", { kind: "quota_line" })], missing: ["b@x.ai"], available: [] } }),
    );
    expect(s.tone).toBe("warn");
    expect(s.text).toContain("2 个");
  });

  it("does not say bare claude-* goes to Kiro", () => {
    const s = channelSummary(channel({ id: "kiro", label: "Kiro", chatModels: ["kiro-claude-sonnet-4.5"], imageModels: [], videoModels: [] }));
    expect(s.text).toContain("kiro-claude");
    expect(s.text).not.toMatch(/(?<!kiro-)claude-\*/);
  });
});

describe("laneOf / starved", () => {
  it("returns an empty lane for an unknown channel", () => {
    expect(laneOf(base, "grok").candidates).toEqual([]);
    expect(channelOf(null, "grok")).toBeNull();
  });

  it("is starved only when running and no channel has a usable account", () => {
    expect(starved(base)).toBe(false);
    const running = { ...base, running: { addr: "a", baseUrl: "b", passthroughAddr: "c", passthroughBaseUrl: "d", startedAt: "" } };
    expect(starved(running)).toBe(true);
    const withGrok = { ...running, channels: [channel({ lane: { current: null, candidates: [candidate("a", { kind: "ready" })], missing: [], available: [] } })] };
    expect(starved(withGrok)).toBe(false);
  });
});
