import { describe, expect, it } from "vitest";
import { formatPrice, resolveModelPrice, stripChannelPrefix } from "./pricing";

describe("pricing resolver", () => {
  it("strips channel prefix correctly", () => {
    expect(stripChannelPrefix("cursor/claude-sonnet-5")).toBe("claude-sonnet-5");
    expect(stripChannelPrefix("chatgpt/gpt-5-codex")).toBe("gpt-5-codex");
    expect(stripChannelPrefix("test01/claude-3.7-sonnet")).toBe("claude-3.7-sonnet");
    expect(stripChannelPrefix("gpt-4o")).toBe("gpt-4o");
  });

  it("resolves curated models with accurate prices including Cache Write", () => {
    const sonnet = resolveModelPrice("claude-sonnet-5");
    expect(sonnet.inputUsdPerMtok).toBe(3);
    expect(sonnet.outputUsdPerMtok).toBe(15);
    expect(sonnet.cacheReadUsdPerMtok).toBe(0.3);
    expect(sonnet.cacheWriteUsdPerMtok).toBe(3.75);

    const opus = resolveModelPrice("claude-opus-5-thinking-max-fast");
    expect(opus.inputUsdPerMtok).toBe(15);
    expect(opus.outputUsdPerMtok).toBe(75);
    expect(opus.cacheReadUsdPerMtok).toBe(1.5);
    expect(opus.cacheWriteUsdPerMtok).toBe(18.75);

    const gpt4o = resolveModelPrice("gpt-4o");
    expect(gpt4o.inputUsdPerMtok).toBe(2.5);
    expect(gpt4o.outputUsdPerMtok).toBe(10);
    expect(gpt4o.cacheReadUsdPerMtok).toBe(1.25);
    expect(gpt4o.cacheWriteUsdPerMtok).toBe(2.5);

    const dsR1 = resolveModelPrice("deepseek-r1");
    expect(dsR1.inputUsdPerMtok).toBe(0.55);
    expect(dsR1.outputUsdPerMtok).toBe(2.19);
    expect(dsR1.cacheReadUsdPerMtok).toBe(0.14);
    expect(dsR1.cacheWriteUsdPerMtok).toBe(0.55);

    const dsChat = resolveModelPrice("deepseek-chat");
    expect(dsChat.inputUsdPerMtok).toBe(0.14);
    expect(dsChat.outputUsdPerMtok).toBe(0.28);
    expect(dsChat.cacheReadUsdPerMtok).toBe(0.014);
    expect(dsChat.cacheWriteUsdPerMtok).toBe(0.14);

    const flash = resolveModelPrice("gemini-3.7-flash");
    expect(flash.inputUsdPerMtok).toBe(0.1);
    expect(flash.outputUsdPerMtok).toBe(0.4);
    expect(flash.cacheReadUsdPerMtok).toBe(0.025);
    expect(flash.cacheWriteUsdPerMtok).toBe(0.1);

    const img = resolveModelPrice("nano-banana-2");
    expect(img.mode).toBe("image");
    expect(img.usdPerImage?.default).toBe(0.04);
  });

  it("resolves remote mesh prefixed models", () => {
    const meshSonnet = resolveModelPrice("node-a/claude-3-7-sonnet");
    expect(meshSonnet.inputUsdPerMtok).toBe(3);
    expect(meshSonnet.outputUsdPerMtok).toBe(15);
    expect(meshSonnet.cacheReadUsdPerMtok).toBe(0.3);
    expect(meshSonnet.cacheWriteUsdPerMtok).toBe(3.75);

    const meshDs = resolveModelPrice("test01/deepseek-r1");
    expect(meshDs.inputUsdPerMtok).toBe(0.55);
    expect(meshDs.outputUsdPerMtok).toBe(2.19);
  });

  it("formats prices cleanly", () => {
    expect(formatPrice(3)).toBe("3.00");
    expect(formatPrice(15)).toBe("15.00");
    expect(formatPrice(1.25)).toBe("1.25");
    expect(formatPrice(0.14)).toBe("0.14");
    expect(formatPrice(0.028)).toBe("0.028");
    expect(formatPrice(0.002)).toBe("0.002");
    expect(formatPrice(undefined)).toBe("0.00");
  });
});
