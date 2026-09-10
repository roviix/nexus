import { describe, expect, it } from "vitest";
import { canHighlight, highlight, type Token, type TokenKind } from "./highlight";

/** 拼回原文。所有断言的前提：高亮只给字符分类，不增删任何一个字符。 */
const text = (tokens: Token[]) => tokens.map((t) => t.text).join("");

/**
 * 恰好自成一个 token 的那段文本被判成了什么。用来断言**切分**：`format!` 有没有在
 * 感叹号处断开、字符串有没有在转义引号处提前收尾。
 */
const kindOf = (tokens: Token[], needle: string): TokenKind | undefined =>
  tokens.find((t) => t.text === needle)?.kind;

/**
 * 某段文本被涂成了什么颜色。相邻的同类会合并（`plain` 常常跟前后的空白连成一片），
 * 所以断言颜色时按位置找覆盖它的那个 token，而不是要求它自己就是一个 token。
 */
function colorOf(tokens: Token[], needle: string): TokenKind | undefined {
  const from = text(tokens).indexOf(needle);
  if (from < 0) return undefined;
  let pos = 0;
  for (const t of tokens) {
    if (from >= pos && from + needle.length <= pos + t.text.length) return t.kind;
    pos += t.text.length;
  }
  return undefined;
}

const kinds = (tokens: Token[]) => tokens.map((t) => t.kind);

describe("highlight 的不变量", () => {
  const samples: [string, string][] = [
    ["ts", `import { a } from "b";\nexport const f = async (x: number): Promise<Foo> => {\n  // 注释\n  return \`v=\${x}\`; /* 块 */\n};`],
    ["json", `{ "a": 1, "b": [true, null, "x\\"y"], "c": { "d": 1.5e3 } }`],
    ["py", `@dec\ndef f(x: int = 3) -> str:\n    """文档\n    串"""\n    # 注释\n    return f'{x}'`],
    ["rust", `pub fn main() {\n    let mut v: Vec<u32> = Vec::new();\n    let s: &'static str = "hi";\n    println!("{v:?}"); // 注释\n}`],
    ["go", "package main\n\nimport \"fmt\"\n\nfunc main() {\n\ts := `raw\nstring`\n\tfmt.Println(s, 0x1F)\n}"],
    ["bash", `#!/usr/bin/env bash\nset -euo pipefail\nfor f in *.ts; do\n  echo "\${f%.ts}" # 去掉后缀\ndone`],
    ["sql", `-- 注释\nSELECT id, count(*) AS n FROM users WHERE name LIKE 'a%' GROUP BY id;`],
    ["html", `<!doctype html>\n<html><body>\n  <!-- 注释 -->\n  <button class="x" disabled>点我</button>\n</body></html>`],
    ["svg", `<svg viewBox="0 0 10 10"><circle cx="5" cy="5" r="4" fill="#2dd4a0"/></svg>`],
    ["css", `/* 注释 */\n.a > .b:hover {\n  --x: 1px;\n  color: #2dd4a0 !important;\n  margin: 0 auto;\n  background: url("a.png");\n}`],
    ["yaml", `# 注释\nname: nexus\nlist:\n  - one\n  - key: 2\nflag: true`],
    ["toml", `# 注释\n[package]\nname = "nexus"\nversion = "0.1.0"\nedition = 2021\n\n[deps]\nserde = { version = "1", features = ["derive"] }`],
    ["md", "# 标题\n\n- 一项 `code`\n- **粗** 和 [链接](https://a.b)\n\n> 引用\n\n```ts\nlet a = 1;\n```"],
  ];

  it.each(samples)("%s：拼回来跟原文一字不差", (lang, src) => {
    expect(text(highlight(lang, src))).toBe(src);
  });

  it.each(samples)("%s：任何一个前缀都不崩、也不丢字（流式）", (lang, src) => {
    for (let k = 0; k <= src.length; k += 1) {
      const head = src.slice(0, k);
      expect(text(highlight(lang, head))).toBe(head);
    }
  });

  it("不认得的语言原样返回一个 plain", () => {
    expect(highlight("brainfuck", "+[-]")).toEqual([{ kind: "plain", text: "+[-]" }]);
    expect(highlight("", "hi")).toEqual([{ kind: "plain", text: "hi" }]);
    expect(canHighlight("tsx")).toBe(true);
    expect(canHighlight("brainfuck")).toBe(false);
  });

  it("空输入给空数组", () => {
    expect(highlight("ts", "")).toEqual([]);
  });

  it("超长的块不高亮——流式里每帧重扫会掉帧", () => {
    const huge = "const a = 1;\n".repeat(4000);
    expect(highlight("ts", huge)).toEqual([{ kind: "plain", text: huge }]);
  });
});

describe("字符串与注释", () => {
  it("认得反斜杠转义，不会被里面的引号骗到", () => {
    const t = highlight("ts", 'const s = "a\\"b";');
    expect(kindOf(t, '"a\\"b"')).toBe("str");
  });

  it("没收尾的单引号停在行尾，不吃掉后面整块", () => {
    const t = highlight("py", "a = 'abc\nb = 1\nc = 2");
    expect(kindOf(t, "'abc")).toBe("str");
    // 关键：下一行照常认得出来，没被染成字符串的一部分
    expect(t.filter((x) => x.kind === "str")).toHaveLength(1);
    expect(kindOf(t, "1")).toBe("num");
  });

  it("模板串跨行，`${}` 不拆开", () => {
    const t = highlight("ts", "const t = `a\n${b}\nc`;");
    expect(kindOf(t, "`a\n${b}\nc`")).toBe("str");
  });

  it("没收尾的模板串吃到当前末尾（它本来就跨行）", () => {
    const t = highlight("ts", "const t = `a\nb");
    expect(kindOf(t, "`a\nb")).toBe("str");
  });

  it("Python 三引号跨行，没收尾也不崩", () => {
    expect(kindOf(highlight("py", 's = """a\nb"""'), '"""a\nb"""')).toBe("str");
    expect(text(highlight("py", 's = """a\nb'))).toBe('s = """a\nb');
  });

  it("行注释到行尾为止", () => {
    const t = highlight("ts", "a; // 说明\nb;");
    expect(kindOf(t, "// 说明")).toBe("cmt");
    expect(colorOf(t, "b")).toBe("plain");
  });

  it("没收尾的块注释吃到末尾，不抛异常", () => {
    const t = highlight("ts", "a;\n/* 还没写完");
    expect(kindOf(t, "/* 还没写完")).toBe("cmt");
  });

  it("字符串里的 `//` 不是注释", () => {
    const t = highlight("ts", 'const u = "https://a.b"; // 真注释');
    expect(kindOf(t, '"https://a.b"')).toBe("str");
    expect(kindOf(t, "// 真注释")).toBe("cmt");
  });

  it("shell 里 `a#b` 中间的井号不是注释", () => {
    const t = highlight("bash", "echo a#b # 这才是");
    expect(kindOf(t, "# 这才是")).toBe("cmt");
    expect(t.filter((x) => x.kind === "cmt")).toHaveLength(1);
  });
});

describe("数字与关键字边界", () => {
  it("认得各种进制、分隔符、指数和后缀", () => {
    const t = highlight("ts", "0xFF 0b1010 0o17 1_000 1.5e3 42");
    expect(kinds(t).filter((k) => k === "num")).toHaveLength(6);
    expect(kindOf(highlight("rust", "let x = 42u32;"), "42u32")).toBe("num");
  });

  it("关键字要整词命中：`let_x` 不是 `let`", () => {
    const t = highlight("rust", "let let_x = 1;");
    expect(kindOf(t, "let")).toBe("kw");
    expect(colorOf(t, "let_x")).toBe("plain");
  });

  it("`format!` 整个是一个宏，不会在感叹号处断开", () => {
    const t = highlight("rust", 'let s = format!("{}", x);');
    expect(kindOf(t, "format!")).toBe("fn");
    expect(kindOf(t, "format")).toBeUndefined();
  });

  it("`a != b` 的感叹号不算宏", () => {
    const t = highlight("rust", "if a != b {}");
    expect(kindOf(t, "a!")).toBeUndefined();
    expect(colorOf(t, "a")).toBe("plain");
  });

  it("Rust 的生命周期不是没收尾的字符字面量", () => {
    const t = highlight("rust", "fn f<'a>(s: &'a str) -> char { 'x' }");
    expect(kindOf(t, "'a")).toBe("type");
    expect(kindOf(t, "'x'")).toBe("str");
  });
});

describe("按语言分派", () => {
  it("TS：内建类型、大写标识符与调用分得开", () => {
    const t = highlight("ts", "const m = new Map<string, Foo>(); bar();");
    expect(kindOf(t, "const")).toBe("kw");
    expect(kindOf(t, "Map")).toBe("type");
    expect(kindOf(t, "Foo")).toBe("type");
    expect(kindOf(t, "bar")).toBe("fn");
  });

  it("JSON：键跟值不同色", () => {
    const t = highlight("json", '{"a": "b", "c": true}');
    expect(kindOf(t, '"a"')).toBe("type");
    expect(kindOf(t, '"b"')).toBe("str");
    expect(kindOf(t, "true")).toBe("kw");
  });

  it("SQL：关键字不分大小写", () => {
    const t = highlight("sql", "select * From t");
    expect(kindOf(t, "select")).toBe("kw");
    expect(kindOf(t, "From")).toBe("kw");
  });

  it("HTML：标签名、属性名、属性值分得开", () => {
    const t = highlight("html", '<a href="https://a.b" data-x>点</a>');
    expect(kindOf(t, "a")).toBe("kw");
    expect(kindOf(t, "href")).toBe("type");
    expect(kindOf(t, '"https://a.b"')).toBe("str");
    expect(kindOf(t, "点")).toBe("plain");
  });

  it("HTML：半个标签不崩", () => {
    expect(text(highlight("html", '<div class="a'))).toBe('<div class="a');
    expect(text(highlight("html", "<!-- 没收尾"))).toBe("<!-- 没收尾");
  });

  it("CSS：块里的属性名是属性，选择器不是", () => {
    const t = highlight("css", ".a:hover { color: red; }");
    expect(kindOf(t, "color")).toBe("type");
    expect(colorOf(t, "hover")).toBe("plain");
    expect(kindOf(highlight("css", "a { color: #2dd4a0; }"), "#2dd4a0")).toBe("num");
  });

  it("YAML：键、注释、布尔各归各位", () => {
    const t = highlight("yaml", "name: nexus # 注释\nok: true");
    expect(kindOf(t, "name")).toBe("type");
    expect(kindOf(t, "# 注释")).toBe("cmt");
    expect(kindOf(t, "true")).toBe("kw");
  });

  it("TOML：段名与键", () => {
    const t = highlight("toml", '[package]\nname = "nexus"');
    expect(kindOf(t, "[package]")).toBe("type");
    expect(kindOf(t, "name")).toBe("type");
    expect(kindOf(t, '"nexus"')).toBe("str");
  });
});
