import { describe, expect, it } from "vitest";
import {
  classify,
  extOf,
  inlineText,
  MAX_IMAGE_BYTES,
  MAX_TEXT_BYTES,
  nameOf,
  payloadOf,
  reject,
  toChatAttachments,
  totalBytes,
  withInlinedText,
  type Attachment,
  type FileLike,
  type ImageAttachment,
  type TextAttachment,
} from "./attachments";

const file = (name: string, type = "", size = 1024): FileLike => ({ name, type, size });

function image(name: string, bytes: number): ImageAttachment {
  return { kind: "image", id: name, name, mime: "image/png", dataBase64: "AAAA", bytes, url: `blob:${name}` };
}

function text(name: string, ext: string, content: string): TextAttachment {
  return { kind: "text", id: name, name, ext, text: content, bytes: content.length };
}

describe("classify", () => {
  it("takes images by MIME or by extension", () => {
    expect(classify(file("猫.PNG"))).toBe("image");
    expect(classify(file("screenshot", "image/jpeg"))).toBe("image");
    expect(classify(file("a.webp"))).toBe("image");
    expect(classify(file("a.gif"))).toBe("image");
  });

  it("takes text and code files by extension, plus extension-less text/*", () => {
    expect(classify(file("notes.md"))).toBe("text");
    expect(classify(file("main.rs"))).toBe("text");
    expect(classify(file("Cargo.toml"))).toBe("text");
    expect(classify(file("Dockerfile"))).toBe("text");
    expect(classify(file("README", "text/plain"))).toBe("text");
    expect(classify(file("data.json", "application/json"))).toBe("text");
  });

  it("turns everything else down", () => {
    expect(classify(file("清单.pdf", "application/pdf"))).toBeNull();
    expect(classify(file("片子.mp4", "video/mp4"))).toBeNull();
    expect(classify(file("包.zip"))).toBeNull();
  });
});

describe("extOf", () => {
  it("is lowercase, dotless, and empty when there is none", () => {
    expect(extOf("a/b/猫.JPG")).toBe("jpg");
    expect(extOf("archive.tar.gz")).toBe("gz");
    expect(extOf("Makefile")).toBe("");
    expect(extOf(".gitignore")).toBe("");
  });
});

describe("reject", () => {
  it("lets a normal image and a normal text file through", () => {
    expect(reject(file("猫.png", "image/png", 900_000), [])).toBeNull();
    expect(reject(file("notes.md", "", 4_000), [])).toBeNull();
  });

  it("caps images at four per message", () => {
    const four = [image("1", 10), image("2", 10), image("3", 10), image("4", 10)];
    expect(reject(file("5.png"), four)).toMatch(/最多带 4 张图/);
    // 文本不占图片的名额。
    expect(reject(file("notes.md", "", 10), four)).toBeNull();
  });

  it("names the file and the limit it broke", () => {
    const big = reject(file("原图.png", "image/png", MAX_IMAGE_BYTES + 1), []);
    expect(big).toContain("原图.png");
    expect(big).toContain("8.0 MB");
    expect(reject(file("dump.json", "", MAX_TEXT_BYTES + 1), [])).toContain("200 KB");
    expect(reject(file("片子.mp4", "video/mp4"), [])).toContain("带不了");
  });

  it("counts what is already attached toward the 20MB budget", () => {
    const heavy = [image("a", 7 * 1024 * 1024), image("b", 7 * 1024 * 1024), image("c", 7 * 1024 * 1024)];
    expect(totalBytes(heavy)).toBe(21 * 1024 * 1024);
    expect(reject(file("d.png", "image/png", 1024), heavy)).toMatch(/20 MB/);
  });
});

describe("nameOf", () => {
  it("gives clipboard images a name with a timestamp", () => {
    const at = new Date(2026, 8, 3, 9, 5, 7);
    expect(nameOf(file("", "image/png"), at)).toBe("粘贴的图片-090507.png");
    expect(nameOf(file("", "image/jpeg"), at)).toBe("粘贴的图片-090507.jpg");
    expect(nameOf(file("有名字.png", "image/png"), at)).toBe("有名字.png");
  });
});

describe("payloadOf", () => {
  it("drops the data URL prefix and leaves a bare payload alone", () => {
    expect(payloadOf("data:image/png;base64,AAAB")).toBe("AAAB");
    expect(payloadOf("AAAB")).toBe("AAAB");
  });
});

describe("inlining text files", () => {
  it("fences the content with its extension and file name", () => {
    expect(inlineText(text("main.rs", "rs", "fn main() {}\n\n"))).toBe("```rs main.rs\nfn main() {}\n```");
  });

  it("grows the fence past the longest run of backticks inside", () => {
    const inner = "见下：\n```js\nok()\n```";
    expect(inlineText(text("notes.md", "md", inner))).toBe("````md notes.md\n见下：\n```js\nok()\n```\n````");
  });

  it("appends every text file after the prompt, images excluded", () => {
    const list: Attachment[] = [text("a.txt", "txt", "一"), image("猫.png", 10), text("b.csv", "csv", "x,y")];
    expect(withInlinedText("  看看这两个文件  ", list)).toBe("看看这两个文件\n\n```txt a.txt\n一\n```\n\n```csv b.csv\nx,y\n```");
    expect(withInlinedText(" 只说话 ", [image("猫.png", 10)])).toBe("只说话");
  });
});

describe("toChatAttachments", () => {
  it("hands the IPC only the images, in order", () => {
    const list: Attachment[] = [text("a.txt", "txt", "一"), image("猫.png", 10), image("狗.png", 20)];
    expect(toChatAttachments(list)).toEqual([
      { name: "猫.png", mime: "image/png", dataBase64: "AAAA" },
      { name: "狗.png", mime: "image/png", dataBase64: "AAAA" },
    ]);
  });
});
