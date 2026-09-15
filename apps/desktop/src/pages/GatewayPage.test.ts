/**
 * 网关页的纯函数都在 `gateway/channels.ts`，这里只钉住页面对 `GatewayStatus` 形状的依赖：
 * 通道从 `status.channels` 来，不再有按平台写死的字段。
 */
import { describe, expect, it } from "vitest";
import { channelSummary } from "../gateway/channels";
import type { GatewayStatus } from "../ipc/types";

const base: GatewayStatus = {
  running: null,
  settings: { port: 8787, passthroughPort: 8788, clientType: "cli", autostart: false, forceModel: null, defaultChannel: "cursor" },
  restartNeeded: false,
  apiKeySet: true,
  channels: [
    {
      id: "chatgpt",
      label: "ChatGPT",
      vendor: "openai",
      ready: false,
      mediaReady: false,
      lane: { current: null, candidates: [], missing: [], available: [] },
      chatModels: ["gpt-5.6-sol"],
      imageModels: ["gpt-image-2"],
      videoModels: [],
      prefixes: ["chatgpt/"],
    },
  ],
  mediaJobs: [],
  entrances: [],
  intercept: { rule: { enabled: false, position: "tail", marker: "[nexus-mark]" }, calls: 0, rewritten: 0, errors: 0, recent: [] },
  grokbotStream: { enabled: false, credential: null },
  lane: { current: null, candidates: [], missing: [], available: [] },
};

describe("GatewayStatus.channels", () => {
  it("summarises the ChatGPT channel from the snapshot", () => {
    const s = channelSummary(base.channels[0]!);
    expect(s.tone).toBe("default");
    expect(s.text).toContain("chatgpt/");
  });
});
