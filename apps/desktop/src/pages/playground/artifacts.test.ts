import { describe, expect, it } from "vitest";
import type { Message } from "../../ipc/playground";
import { artifactTitle } from "./Markdown";
import { artifactsInText, collectArtifacts, LIVE_ARTIFACT, sameArtifact } from "./artifacts";

let seq = 0;
function msg(role: "user" | "assistant", content: string): Message {
  seq += 1;
  return {
    id: `m${seq}`,
    threadId: "t1",
    seq,
    role,
    content,
    thinking: null,
    model: null,
    routed: null,
    usage: null,
    error: null,
    durationMs: null,
    ttftMs: null,
    createdAt: "2026-09-01T00:00:00Z",
    images: [],
  };
}

describe("artifactTitle", () => {
  it("digs the <title> out of a page", () => {
    const body = `<!DOCTYPE html><html><head><title>  待办清单  </title></head><body></body></html>`;
    expect(artifactTitle("html", body)).toBe("待办清单");
  });

  it("falls back to the first <h1>, then to the kind name — so the meta line can spot the fallback", () => {
    expect(artifactTitle("html", `<div><h1>季度报表</h1></div>`)).toBe("季度报表");
    expect(artifactTitle("html", `<!DOCTYPE html><html><body>hi</body></html>`)).toBe("HTML 页面");
    // 片段也一样：兜底就是类别名本身，卡片靠「标题 === 类别名」认出它。
    expect(artifactTitle("html", `<div>hi</div>`)).toBe("HTML 页面");
    expect(artifactTitle("svg", `<svg viewBox="0 0 1 1"></svg>`)).toBe("SVG 图像");
  });

  it("strips tags and decodes the common entities", () => {
    expect(artifactTitle("html", `<title>Tom &amp; Jerry <b>笔记</b></title>`)).toBe("Tom & Jerry 笔记");
  });

  it("caps absurdly long titles", () => {
    const long = `<title>${"长".repeat(80)}</title>`;
    expect(artifactTitle("html", long)).toHaveLength(61);
  });
});

describe("artifactsInText", () => {
  it("collects previewable blocks and skips the rest", () => {
    const text = [
      "看这个：",
      "```python",
      "print(1)",
      "```",
      "```html",
      "<!DOCTYPE html><title>页</title>",
      "```",
      "```svg",
      "<svg></svg>",
      "```",
    ].join("\n");
    const arts = artifactsInText("m1", text);
    expect(arts.map((a) => a.kind)).toEqual(["html", "svg"]);
    // 序号把不可预览的 python 块也算上 —— 坐标才和消息流里的卡片对得上。
    expect(arts.map((a) => a.ref.codeIndex)).toEqual([1, 2]);
    expect(arts[0]?.ref.messageId).toBe("m1");
    expect(arts[0]?.title).toBe("页");
    expect(arts[0]?.open).toBe(false);
    expect(arts[0]?.lines).toBe(1);
  });

  it("marks an unclosed fence as still being written", () => {
    const arts = artifactsInText("m1", "```html\n<!DOCTYPE html><title>半篇</title>");
    expect(arts).toHaveLength(1);
    expect(arts[0]?.open).toBe(true);
  });
});

describe("collectArtifacts", () => {
  it("only reads assistant replies and appends the live run last", () => {
    const messages = [
      msg("user", "```html\n<title>用户贴的</title>\n```"),
      msg("assistant", "```html\n<title>回复的</title>\n```"),
    ];
    const arts = collectArtifacts(messages, "```svg\n<svg></svg>");
    expect(arts.map((a) => [a.ref.messageId, a.kind])).toEqual([
      [messages[1]!.id, "html"],
      [LIVE_ARTIFACT, "svg"],
    ]);
  });
});

describe("sameArtifact", () => {
  it("compares both coordinates", () => {
    expect(sameArtifact({ messageId: "m1", codeIndex: 0 }, { messageId: "m1", codeIndex: 0 })).toBe(true);
    expect(sameArtifact({ messageId: "m1", codeIndex: 0 }, { messageId: "m1", codeIndex: 1 })).toBe(false);
    expect(sameArtifact(null, { messageId: "m1", codeIndex: 0 })).toBe(false);
  });
});
