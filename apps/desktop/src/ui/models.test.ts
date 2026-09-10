import { describe, expect, it } from "vitest";
import type { LocalModel } from "../ipc/models";
import { filterCards, groupLocal, modalityRail, vendorRail } from "./models";

const local = (over: Partial<LocalModel> & { id: string }): LocalModel => ({
  vendor: "anthropic",
  vendorLabel: "Anthropic",
  series: over.id,
  variant: "standard",
  aliases: [],
  note: null,
  ...over,
});

const LOCAL: LocalModel[] = [
  local({ id: "auto", vendor: "cursor", vendorLabel: "Cursor", aliases: ["haiku", "gpt-4o-mini"], note: "让 Cursor 挑" }),
  local({ id: "claude-sonnet-5", aliases: ["claude-3-5-sonnet", "claude-sonnet-4"] }),
  local({ id: "claude-opus-5" }),
  local({ id: "claude-opus-5-thinking-max-fast", series: "claude-opus-5", variant: "thinking-max-fast" }),
  local({ id: "gpt-5.6-sol", vendor: "openai", vendorLabel: "OpenAI", aliases: ["gpt-4o", "gpt-5"] }),
  local({ id: "gpt-image-1", vendor: "openai", vendorLabel: "OpenAI", modality: "image" }),
];

const ALL = { query: "", vendor: "all", modality: "all" } as const;

describe("groupLocal", () => {
  it("folds reasoning tiers into one card and titles single-tier cards by their real id", () => {
    const groups = groupLocal(LOCAL);
    const opus = groups.find((g) => g.series === "claude-opus-5")!;
    expect(opus.variants.map((v) => v.id)).toEqual(["claude-opus-5", "claude-opus-5-thinking-max-fast"]);
    expect(opus.title).toBe("claude-opus-5");
    const sonnet = groups.find((g) => g.series === "claude-sonnet-5")!;
    expect(sonnet.title).toBe("claude-sonnet-5");
  });

  it("orders vendors, puts chat before image, and cursor's auto last", () => {
    const groups = groupLocal(LOCAL);
    expect(groups.map((g) => g.vendor)).toEqual(["anthropic", "anthropic", "openai", "openai", "cursor"]);
    expect(groups.at(-1)?.series).toBe("auto");
    const openai = groups.filter((g) => g.vendor === "openai");
    expect(openai.map((g) => g.modality)).toEqual(["chat", "image"]);
  });
});

describe("filterCards", () => {
  const groups = groupLocal(LOCAL);

  it("searches aliases so a Claude Code name finds its mapping target", () => {
    expect(filterCards(groups, { ...ALL, query: "claude-3-5-sonnet" }).map((g) => g.series)).toEqual(["claude-sonnet-5"]);
    expect(filterCards(groups, { ...ALL, query: "GPT-4O" }).map((g) => g.series)).toEqual(["gpt-5.6-sol", "auto"]);
  });

  it("vendor and modality filters compose", () => {
    expect(filterCards(groups, { ...ALL, vendor: "openai", modality: "image" }).map((g) => g.title)).toEqual(["gpt-image-1"]);
    expect(filterCards(groups, { ...ALL, vendor: "google" })).toEqual([]);
  });

  it("rails count everything except their own dimension", () => {
    expect(vendorRail(groups, { ...ALL, vendor: "anthropic" })).toEqual([
      { vendor: "anthropic", label: "Anthropic", n: 2 },
      { vendor: "openai", label: "OpenAI", n: 2 },
      { vendor: "cursor", label: "Cursor", n: 1 },
    ]);
    expect(modalityRail(groups, { ...ALL, vendor: "openai" })).toEqual([
      { id: "chat", n: 1 },
      { id: "image", n: 1 },
    ]);
  });
});
