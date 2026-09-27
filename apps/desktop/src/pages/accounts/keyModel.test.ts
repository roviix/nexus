import { describe, expect, it } from "vitest";
import { cleanModels, formatLabel, primaryModel, splitModels } from "./keyModel";

describe("provider model lists", () => {
  it("dedupes case-insensitively, drops [1m] and blanks, keeps order", () => {
    expect(cleanModels([" deepseek-v4-pro[1m] ", "deepseek-v4-flash", "DeepSeek-V4-Pro", "", "has space"])).toEqual([
      "deepseek-v4-pro",
      "deepseek-v4-flash",
    ]);
  });

  it("splits pasted lists on commas, spaces and newlines", () => {
    expect(splitModels("a, b\nc；a")).toEqual(["a", "b", "c"]);
  });

  it("uses the first model as the one to connect with", () => {
    expect(primaryModel({ models: ["x", "y"] })).toBe("x");
    expect(primaryModel({ models: [] })).toBe("");
  });

  it("labels formats", () => {
    expect(formatLabel("openai_chat")).toBe("OpenAI Chat");
  });
});
