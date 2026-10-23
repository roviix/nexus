import { describe, expect, it } from "vitest";
import type { LocalModel } from "../ipc/models";
import type { ChannelSnapshot, GatewayCandidate, GatewayStatus } from "../ipc/types";
import { channelOf, channelOfModel, channelSummary, defaultChannelId, laneOf, localChannels, splitModelId, starved } from "./channels";

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
  settings: { port: 8787, autostart: false, forceModel: null, defaultChannel: "cursor" },
  restartNeeded: false,
  apiKeySet: true,
  channels: [],
  mediaJobs: [],
  routes: {},
  lane: { current: null, candidates: [], missing: [], available: [] },
};

describe("channelSummary", () => {
  it("says a prefixed id is required when the channel has no account", () => {
    const s = channelSummary(channel({}));
    expect(s.tone).toBe("default");
    expect(s.text).toContain("grok/");
    expect(s.text).not.toContain("会走 Cursor");
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
    expect(s.text).toContain("kiro/");
    expect(s.text).not.toMatch(/(?<!kiro-)claude-\*/);
  });
});

describe("channelOfModel / defaultChannelId", () => {
  it("reads the prefix, and bare names go to the user default", () => {
    const status = {
      ...base,
      settings: { ...base.settings, defaultChannel: "chatgpt" },
      channels: [channel({ id: "chatgpt", label: "ChatGPT", vendor: "openai", prefixes: ["chatgpt/", "codex/"] }), channel({})],
    };
    const channels = localChannels(status, []);
    expect(defaultChannelId(status)).toBe("chatgpt");
    expect(channelOfModel(channels, "cursor/claude-opus-5")).toBe("cursor");
    expect(channelOfModel(channels, "chatgpt/gpt-5.4")).toBe("chatgpt");
    expect(channelOfModel(channels, "codex/gpt-5.4")).toBe("chatgpt");
    expect(channelOfModel(channels, "xai/grok-4.5")).toBe("grok");
    expect(channelOfModel(channels, "gpt-5.4")).toBe("chatgpt");
  });

  it("routes both zcode/ and glm/ to the ZCode channel", () => {
    const status = {
      ...base,
      settings: { ...base.settings, defaultChannel: "zcode" },
      channels: [channel({ id: "zcode", label: "ZCode", vendor: "zhipu", prefixes: ["zcode/", "glm/"] })],
    };
    const channels = localChannels(status, []);
    expect(defaultChannelId(status)).toBe("zcode");
    expect(channelOfModel(channels, "zcode/glm-4.7")).toBe("zcode");
    expect(channelOfModel(channels, "glm/glm-4.7")).toBe("zcode");
    // 裸名走用户设的默认通道，和别的平台一个规矩。
    expect(channelOfModel(channels, "glm-4.7")).toBe("zcode");
  });

  it("routes qoder/ and provider/ to their own channels", () => {
    const status: GatewayStatus = {
      ...base,
      settings: { ...base.settings, defaultChannel: "provider" },
      channels: [
        channel({ id: "qoder", label: "Qoder", vendor: "qoder", prefixes: ["qoder/", "qoder-cn/"], chatModels: [], imageModels: [], videoModels: [] }),
        channel({ id: "provider", label: "供应商", vendor: "provider", prefixes: ["provider/"], chatModels: ["provider/deepseek-v4-pro"], imageModels: [], videoModels: [] }),
      ],
    };
    const channels = localChannels(status);
    expect(defaultChannelId(status)).toBe("provider");
    expect(channelOfModel(channels, "qoder/Qwen3.8-Max")).toBe("qoder");
    expect(channelOfModel(channels, "provider/deepseek-v4-pro")).toBe("provider");
    expect(splitModelId("qoder-cn/Auto").channel).toBe("qoder");
    expect(splitModelId("claude/claude-opus-4-6").channel).toBe("claude");
    expect(splitModelId("antigravity/gemini-3.1-pro").channel).toBe("antigravity");
    expect(splitModelId("agy/claude-3-7-sonnet").channel).toBe("antigravity");
    expect(splitModelId("google/gemini-2.5-pro").channel).toBe("antigravity");
    // 供应商的模型 id 自己带斜杠：不是已知前缀就当裸名。
    expect(splitModelId("anthropic/claude-sonnet").channel).toBeNull();
    expect(channelSummary({ ...channels[2]!, isDefault: false }).text).toContain("还没有供应商");
  });

  it("does not dump bare catalog names onto Cursor", () => {
    const local: LocalModel[] = [
      { id: "cursor/claude-sonnet-5", vendor: "anthropic", vendorLabel: "Anthropic", series: "cursor/claude-sonnet-5", variant: "standard", aliases: [], note: null },
      { id: "chatgpt/gpt-5.4", vendor: "openai", vendorLabel: "OpenAI", series: "chatgpt/gpt-5.4", variant: "standard", aliases: [], note: null },
      { id: "bare-old-name", vendor: "cursor", vendorLabel: "Cursor", series: "bare-old-name", variant: "standard", aliases: [], note: null },
    ];
    expect(localChannels(base, local)[0]!.chatModels).toEqual(["cursor/claude-sonnet-5"]);
  });
});

describe("laneOf / starved", () => {
  it("returns an empty lane for an unknown channel", () => {
    expect(laneOf(base, "grok").candidates).toEqual([]);
    expect(channelOf(null, "grok")).toBeNull();
  });

  it("is starved only when running and no channel has a usable account", () => {
    expect(starved(base)).toBe(false);
    const running = { ...base, running: { addr: "a", baseUrl: "b", startedAt: "" } };
    expect(starved(running)).toBe(true);
    const withGrok = { ...running, channels: [channel({ lane: { current: null, candidates: [candidate("a", { kind: "ready" })], missing: [], available: [] } })] };
    expect(starved(withGrok)).toBe(false);
  });

  it("supports Nexus 2.0 custom logical channels and mesh cloud channels", () => {
    const claudeBase = channel({
      id: "claude",
      label: "Claude",
      vendor: "anthropic",
      ready: true,
      chatModels: ["claude-sonnet-4-6", "claude-opus-4-6"],
      prefixes: ["claude/"],
      lane: {
        current: "ada@work.com",
        candidates: [
          candidate("ada@work.com", { kind: "ready" }),
          candidate("backup@me.com", { kind: "ready" }),
        ],
        missing: [],
        available: [],
      },
    });

    const customChannels = [
      {
        id: "claude-work",
        label: "Claude-主力工作组",
        platform: "claude" as const,
        prefix: "claude-work",
        accountIds: ["ada@work.com"],
        strategy: "quota_first" as const,
        createdAt: Date.now(),
      },
    ];

    const meshPeers = [
      {
        node_id: "node-123",
        device_name: "home-pc",
        remote_models: ["gpt-5-codex", "grok-4-vision"],
        latency_ms: 22,
      },
    ];

    const statusWithClaude: GatewayStatus = {
      ...base,
      channels: [claudeBase],
    };

    const channels = localChannels(statusWithClaude, null, meshPeers, customChannels);

    // Should include Cursor, Claude, custom channel claude-work, and mesh channel home-pc
    const workCh = channels.find((c) => c.id === "claude-work");
    expect(workCh).toBeDefined();
    expect(workCh?.kind).toBe("custom");
    expect(workCh?.label).toBe("Claude-主力工作组");
    expect(workCh?.chatModels).toEqual(["claude-work/claude-sonnet-4-6", "claude-work/claude-opus-4-6"]);
    expect(workCh?.lane.candidates.length).toBe(1);
    expect(workCh?.lane.candidates[0]?.label).toBe("ada@work.com");

    const meshCh = channels.find((c) => c.id === "home-pc");
    expect(meshCh).toBeDefined();
    expect(meshCh?.kind).toBe("mesh");
    expect(meshCh?.latencyMs).toBe(22);
    expect(meshCh?.chatModels).toContain("home-pc/gpt-5-codex");
  });
});

