import { describe, expect, it } from "vitest";
import { parseBlocks, previewDoc, previewKind } from "./Markdown";

describe("parseBlocks", () => {
  it("keeps an unclosed fence as a code block (streaming)", () => {
    const blocks = parseBlocks("看这个：\n```python\nprint(1)\nprint(2)");
    expect(blocks).toEqual([
      { kind: "p", text: "看这个：" },
      { kind: "code", lang: "python", body: "print(1)\nprint(2)", open: true },
    ]);
  });

  it("closes fences and reads the language tag", () => {
    const blocks = parseBlocks("```ts\nconst a = 1;\n```\n后面");
    expect(blocks[0]).toEqual({ kind: "code", lang: "ts", body: "const a = 1;", open: false });
    expect(blocks[1]).toEqual({ kind: "p", text: "后面" });
  });

  it("recognises headings, quotes, rules, lists and tables", () => {
    const src = ["## 标题", "> 引用一", "> 引用二", "---", "- 一", "  - 一点一", "- 二", "", "| a | b |", "|---|---|", "| 1 | 2 |"].join("\n");
    const kinds = parseBlocks(src).map((b) => b.kind);
    expect(kinds).toEqual(["heading", "quote", "hr", "list", "table"]);
    const list = parseBlocks("- 一\n  - 一点一\n- 二")[0];
    expect(list).toEqual({ kind: "list", ordered: false, items: [{ depth: 0, text: "一" }, { depth: 1, text: "一点一" }, { depth: 0, text: "二" }] });
    const table = parseBlocks("| a | b |\n|---|---|\n| 1 | 2 |")[0];
    expect(table).toEqual({ kind: "table", head: ["a", "b"], align: [null, null], rows: [["1", "2"]] });
  });

  it("reads h1–h4 levels", () => {
    expect(parseBlocks("# 一\n## 二\n### 三\n#### 四").map((b) => (b.kind === "heading" ? b.level : 0))).toEqual([1, 2, 3, 4]);
    // 五个井号不是标题，就是一行普通字
    expect(parseBlocks("##### 五")[0]?.kind).toBe("p");
  });

  it("joins consecutive lines into one paragraph and splits on blank lines", () => {
    expect(parseBlocks("一行\n二行\n\n三行")).toEqual([
      { kind: "p", text: "一行\n二行" },
      { kind: "p", text: "三行" },
    ]);
  });

  it("ends a paragraph at a horizontal rule instead of swallowing it", () => {
    expect(parseBlocks("上面\n---\n下面").map((b) => b.kind)).toEqual(["p", "hr", "p"]);
  });

  it("strips the hard-break marker (trailing spaces) — pre-wrap already breaks the line", () => {
    expect(parseBlocks("一行  \n二行   ")).toEqual([{ kind: "p", text: "一行\n二行" }]);
    expect(parseBlocks("## 标题  ")[0]).toEqual({ kind: "heading", level: 2, text: "标题" });
  });

  it("reads task lists", () => {
    expect(parseBlocks("- [ ] 没做\n- [x] 做了\n- [X] 也做了\n- 普通")[0]).toEqual({
      kind: "list",
      ordered: false,
      items: [
        { depth: 0, text: "没做", task: true, done: false },
        { depth: 0, text: "做了", task: true, done: true },
        { depth: 0, text: "也做了", task: true, done: true },
        { depth: 0, text: "普通" },
      ],
    });
    // 中括号后面没有空格的不算任务项
    expect(parseBlocks("- [x]紧挨着")[0]).toEqual({ kind: "list", ordered: false, items: [{ depth: 0, text: "[x]紧挨着" }] });
  });

  it("reads table alignment from the separator row", () => {
    const table = parseBlocks("| a | b | c | d |\n|:--|:-:|--:|---|\n| 1 | 2 | 3 | 4 |")[0];
    expect(table).toEqual({
      kind: "table",
      head: ["a", "b", "c", "d"],
      align: ["left", "center", "right", null],
      rows: [["1", "2", "3", "4"]],
    });
  });

  it("nests quotes and re-parses their contents as blocks", () => {
    expect(parseBlocks("> 引用一\n> 引用二")[0]).toEqual({ kind: "quote", blocks: [{ kind: "p", text: "引用一\n引用二" }] });
    expect(parseBlocks("> 外\n> > 里")[0]).toEqual({
      kind: "quote",
      blocks: [{ kind: "p", text: "外" }, { kind: "quote", blocks: [{ kind: "p", text: "里" }] }],
    });
    // 引用里的列表和代码块照样认
    const inner = parseBlocks("> - 一\n> - 二\n> ```ts\n> let a = 1;\n> ```")[0];
    if (inner?.kind !== "quote") throw new Error("这一段该解析成引用");
    expect(inner.blocks.map((b) => b.kind)).toEqual(["list", "code"]);
  });

  it("stops recursing on absurdly deep quotes instead of blowing the stack", () => {
    const deep = parseBlocks(`${">".repeat(40)} 底`);
    let node = deep[0];
    let levels = 0;
    while (node?.kind === "quote") {
      levels += 1;
      node = node.blocks[0];
    }
    expect(levels).toBe(4);
    expect(node).toEqual({ kind: "p", text: `${">".repeat(36)} 底` });
  });
});

describe("previewKind", () => {
  it("takes html / svg by language tag", () => {
    expect(previewKind("html", "<b>x</b>")).toBe("html");
    expect(previewKind("HTM", "<b>x</b>")).toBe("html");
    expect(previewKind("xhtml", "<b>x</b>")).toBe("html");
    expect(previewKind("svg", "<svg></svg>")).toBe("svg");
  });

  it("only takes xml when the body actually looks like an svg", () => {
    expect(previewKind("xml", '<svg viewBox="0 0 1 1"></svg>')).toBe("svg");
    expect(previewKind("xml", "<config><a/></config>")).toBeNull();
  });

  it("sniffs an untagged fence that is a whole html page or an svg", () => {
    expect(previewKind("", "<!DOCTYPE html><html></html>")).toBe("html");
    expect(previewKind("", "\n  <html lang=\"zh\">")).toBe("html");
    expect(previewKind("", "<svg xmlns='...'>")).toBe("svg");
    // 只是一段片段（没有 <html>）时不猜：模型讲解 HTML 语法时也会这么写
    expect(previewKind("", "<div>hi</div>")).toBeNull();
  });

  it("never offers a preview for other languages", () => {
    expect(previewKind("ts", "<div/>")).toBeNull();
    expect(previewKind("markdown", "<svg>")).toBeNull();
    expect(previewKind("", "普通文字")).toBeNull();
  });
});

describe("previewDoc", () => {
  it("wraps a fragment in a dark document", () => {
    const doc = previewDoc("html", "<button>点</button>");
    expect(doc).toContain("<!DOCTYPE html>");
    expect(doc).toContain("background:#0a0c0d");
    expect(doc).toContain("<button>点</button>");
  });

  it("leaves an already-complete document alone", () => {
    const src = "<!doctype html><html><head><title>x</title></head><body>y</body></html>";
    expect(previewDoc("html", src)).toBe(src);
  });

  it("centres a bare svg", () => {
    expect(previewDoc("svg", "<svg></svg>")).toContain("justify-content:center");
  });
});
