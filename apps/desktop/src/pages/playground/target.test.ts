import { describe, expect, it } from "vitest";
import type { LocalModel } from "../../ipc/models";
import type { ThreadSummary } from "../../ipc/playground";
import { aspectOf, DEFAULT_IMAGE_SIZES, defaultTarget, fixedSizeOf, fmtBytes, fmtLatency, fmtSize, kindOfModel, modelIds, plainPreview, retarget, sizeChip, sizeOptionsOf, threadTitle, tokensPerSecond } from "./target";

const local: LocalModel[] = [
  { id: "auto", vendor: "cursor", vendorLabel: "Cursor", series: "auto", variant: "standard", aliases: [], note: null },
  { id: "claude-sonnet-5", vendor: "anthropic", vendorLabel: "Anthropic", modality: "chat", series: "claude-sonnet-5", variant: "standard", aliases: [], note: null },
  { id: "nano-banana-2", vendor: "google", vendorLabel: "Google", modality: "image", series: "nano-banana-2", variant: "standard", aliases: ["gemini-3.1-flash-image"], note: null },
];
const cat = { local };

function thread(kind: "chat" | "image", model: string): ThreadSummary {
  return { id: `${kind}-${model}`, kind, title: "", model, createdAt: "", updatedAt: "", messageCount: 0, preview: null, coverImageId: null };
}

describe("modelIds", () => {
  it("splits by kind; unlabelled entries count as chat", () => {
    expect(modelIds(cat, "chat")).toEqual(["auto", "claude-sonnet-5"]);
    expect(modelIds(cat, "image")).toEqual(["nano-banana-2"]);
    // 老网关的目录没有 modality：全算对话，图片一侧为空。
    const old = { local: local.map(({ modality: _m, ...rest }) => rest) };
    expect(modelIds(old, "image")).toEqual([]);
    expect(modelIds(old, "chat")).toHaveLength(3);
    expect(modelIds({ local: null }, "chat")).toEqual([]);
  });

  it("knows which models are image models", () => {
    expect(kindOfModel(cat, "nano-banana-2")).toBe("image");
    expect(kindOfModel(cat, "claude-sonnet-5")).toBe("chat");
    expect(kindOfModel(cat, "unknown")).toBe("chat");
  });
});

describe("defaultTarget", () => {
  it("follows the most recent thread of that kind", () => {
    const recent = [thread("chat", "claude-sonnet-5"), thread("image", "nano-banana-2")];
    expect(defaultTarget("chat", recent, cat)).toEqual({ model: "claude-sonnet-5" });
    expect(defaultTarget("image", recent, cat)).toEqual({ model: "nano-banana-2" });
  });

  it("falls back to `auto` for chat and the first entry otherwise", () => {
    expect(defaultTarget("chat", [], cat)).toEqual({ model: "auto" });
    expect(defaultTarget("image", [], cat)).toEqual({ model: "nano-banana-2" });
    expect(defaultTarget("video", [], cat)).toEqual({ model: "" });
  });

  it("picks cursor/auto from a qualified local catalog", () => {
    const qualified = {
      local: [
        { id: "cursor/claude-sonnet-5", vendor: "anthropic", vendorLabel: "Anthropic", modality: "chat" as const, series: "cursor/claude-sonnet-5", variant: "standard", aliases: [], note: null },
        { id: "cursor/auto", vendor: "cursor", vendorLabel: "Cursor", series: "cursor/auto", variant: "standard", aliases: [], note: null },
      ],
    };
    expect(defaultTarget("chat", [], qualified)).toEqual({ model: "cursor/auto" });
    expect(retarget({ model: "gone" }, "chat", qualified)).toEqual({ model: "cursor/auto" });
  });

  it("honours a model handed over from the model plaza", () => {
    expect(defaultTarget("chat", [], cat, { model: "claude-sonnet-5" })).toEqual({ model: "claude-sonnet-5" });
  });
});

describe("retarget", () => {
  it("keeps the model when the catalog has it, else falls back to the default", () => {
    expect(retarget({ model: "claude-sonnet-5" }, "chat", cat)).toEqual({ model: "claude-sonnet-5" });
    expect(retarget({ model: "gone" }, "chat", cat)).toEqual({ model: "auto" });
    expect(retarget({ model: "gone" }, "image", cat)).toEqual({ model: "nano-banana-2" });
    // 目录还没到：不动。
    expect(retarget({ model: "gone" }, "chat", { local: null })).toEqual({ model: "gone" });
  });
});

describe("sizes and formatting", () => {
  it("offers the generic size row for images only", () => {
    expect(sizeOptionsOf("image")).toEqual(DEFAULT_IMAGE_SIZES);
    expect(sizeOptionsOf("chat")).toEqual([]);
  });

  it("derives the tier and aspect chip from the pixels", () => {
    expect(sizeChip("1024x1024")).toEqual({ alias: "1K", ratio: "1:1" });
    expect(sizeChip("1024x1536")).toEqual({ alias: "1K", ratio: "2:3" });
    expect(sizeChip("2304x1728")).toEqual({ alias: "2K", ratio: "4:3" });
    expect(sizeChip("1792x1024")).toEqual({ alias: "1K", ratio: "7:4" });
    expect(sizeChip("4096x4096")).toEqual({ alias: "4K", ratio: "1:1" });
    expect(sizeChip("auto")).toEqual({ alias: "", ratio: "auto" });
  });

  it("aspect ratios come from the size string", () => {
    expect(aspectOf("1024x1536")).toBeCloseTo(2 / 3);
    expect(aspectOf("2304×1728")).toBeCloseTo(4 / 3);
    expect(aspectOf(null)).toBe(1);
    expect(aspectOf("nope")).toBe(1);
    expect(fmtSize("1536x1024")).toBe("1536 × 1024");
  });

  it("knows which targets ignore the size parameter", () => {
    // 目录没带 fixedSize 的老网关：出图只有 Cursor 一条路，按固定算。
    expect(fixedSizeOf("image", { model: "nano-banana-2" }, cat)).toBe("1536x1024");
    // 新网关的目录自己说：Cursor 那条固定，ChatGPT 的 gpt-image 认规格。
    const newLocal: LocalModel[] = [
      { ...local[2]!, fixedSize: "1536x1024" },
      { id: "gpt-image-2", vendor: "openai", vendorLabel: "OpenAI", modality: "image", series: "gpt-image-2", variant: "standard", aliases: [], note: null, fixedSize: null },
    ];
    expect(fixedSizeOf("image", { model: "nano-banana-2" }, { local: newLocal })).toBe("1536x1024");
    expect(fixedSizeOf("image", { model: "gpt-image-2" }, { local: newLocal })).toBeNull();
    expect(fixedSizeOf("chat", { model: "auto" }, cat)).toBeNull();
  });

  it("formats latency, bytes and throughput", () => {
    expect(fmtLatency(420)).toBe("420 ms");
    expect(fmtLatency(12_340)).toBe("12.3 s");
    expect(fmtLatency(null)).toBe("—");
    expect(fmtBytes(900)).toBe("900 B");
    expect(fmtBytes(1536)).toBe("1.5 KB");
    expect(fmtBytes(3 * 1024 * 1024)).toBe("3.0 MB");
    expect(tokensPerSecond(100, 2_400, 400)).toBe(50);
    expect(tokensPerSecond(0, 2_400, 400)).toBeNull();
    expect(tokensPerSecond(10, 420, 400)).toBeNull();
  });

  it("gives untitled threads a placeholder by kind", () => {
    expect(threadTitle("  ", "chat")).toBe("新对话");
    expect(threadTitle("", "image")).toBe("新图片");
    expect(threadTitle("画只猫", "image")).toBe("画只猫");
  });

  it("strips markdown noise out of list previews", () => {
    expect(plainPreview("这是 **claude** 在 `预览` 里的回答。\n\n- 第一点")).toBe("这是 claude 在 预览 里的回答。 - 第一点");
    expect(plainPreview("```ts\nconst a = 1;")).toBe("const a = 1;");
    expect(plainPreview("看 [文档](https://x.y) 和 ![图](https://z)")).toBe("看 文档 和 图");
  });
});
