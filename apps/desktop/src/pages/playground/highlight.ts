/**
 * 代码块的语法高亮。手写 tokenizer，零依赖。
 *
 * 不引 highlight.js / shiki：桌面端运行时依赖至今只有 react / tauri，为聊天气泡里的几个
 * 代码块背一棵几百 KB 的语法定义不值。真正的约束也不是「认得多全」，而是流式——回复没写完
 * 时，半截的字符串、没收尾的注释、只写了一半的标签是常态，于是有三条硬要求：
 *
 *   1. 未闭合的 `'` / `"` 停在行尾，不许把后面整块代码染成字符串（下一帧多来几个字，
 *      整块颜色就会来回翻；跨行的模板串 / 三引号 / 块注释另说，它们本来就能跨行）；
 *   2. 任何输入都不抛异常，也不死循环——主循环每一轮必定前进至少一个字符；
 *   3. `tokens.map(t => t.text).join("") === 输入`，一个字符不多不少。这条是渲染正确性的
 *      地基，测试对每种语言的每个前缀都验一遍。
 *
 * 单块 O(n)（全部用粘性正则，不切片重扫），调用方按块 memo。
 *
 * 刻意不做的：正则字面量（跟除号分不开，猜错会把半篇代码染成字符串）、模板串里 `${}` 的
 * 子高亮、HTML 里 `<script>` / `<style>` 的内嵌语言、JSX 表达式——这些都要一层上下文栈，
 * 在一个聊天气泡里收益抵不过复杂度。不认得的语言原样返回，交给调用方当纯文本画。
 */

export type TokenKind = "kw" | "str" | "num" | "cmt" | "type" | "fn" | "punct" | "plain";

export interface Token {
  kind: TokenKind;
  text: string;
}

/* ── 输出 ───────────────────────────────────────────────────────────────── */

/** 相邻同类合并。一个 300 行的文件不合并能出好几千个 span，合并后通常只剩几百。 */
class Sink {
  readonly out: Token[] = [];

  push(kind: TokenKind, text: string): void {
    if (!text) return;
    const last = this.out[this.out.length - 1];
    if (last && last.kind === kind) last.text += text;
    else this.out.push({ kind, text });
  }

  pushAll(tokens: Token[]): void {
    for (const t of tokens) this.push(t.kind, t.text);
  }
}

/* ── 扫描原语 ───────────────────────────────────────────────────────────── */

const LF = 10;
const TAB = 9;
const CR = 13;
const SPACE = 32;

function isSpace(code: number): boolean {
  return code === SPACE || code === LF || code === TAB || code === CR;
}

/** 粘性正则在 `i` 处匹配到的文本；没匹配上给 null。粘性是为了不切片——切片就成 O(n²) 了。 */
function at(re: RegExp, src: string, i: number): string | null {
  re.lastIndex = i;
  const m = re.exec(src);
  return m ? (m[0] ?? null) : null;
}

const RE_WS = /[ \t\r\n]+/y;
const RE_IDENT = /[A-Za-z_$][A-Za-z0-9_$]*/y;
const RE_NUM =
  /(?:0[xX][0-9a-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:[eE][+-]?\d+)?)(?:[A-Za-z_][A-Za-z0-9_]*)?/y;
/** 非 ASCII 连片算正文：注释和字符串外的中文（JSX 文案、Go 的中文标识符）不该被涂成标点灰。 */
const RE_WIDE = /[^\x00-\x7F]+/y;
/** Rust 的生命周期。必须在字符字面量之前判，否则 `'a` 会被当成一个没收尾的 `'`。 */
const RE_LIFETIME = /'[A-Za-z_][A-Za-z0-9_]*(?!')/y;
/** Shell 变量。`${` 允许不闭合——流式里它很可能只写到一半。 */
const RE_SHVAR = /\$(?:\{[^}\n]*\}?|[A-Za-z_][A-Za-z0-9_]*|[0-9@*#?$!_-])/y;

interface StrRule {
  open: string;
  /** 能跨行（模板串、三引号、Go 的裸串）。不跨行的碰到换行就收工。 */
  multi?: boolean;
  /** 反斜杠转义。 */
  esc?: boolean;
}

/**
 * 字符串的结束位置（含定界符）。没收尾时：单行的停在换行处，跨行的吃到末尾——
 * 后者是对的，那几个字符本来就还在字符串里，下一帧补上引号即可。
 */
function endOfString(src: string, i: number, rule: StrRule): number {
  const n = src.length;
  let j = i + rule.open.length;
  while (j < n) {
    const code = src.charCodeAt(j);
    if (rule.esc && code === 92 /* \ */) {
      j += 2;
      continue;
    }
    if (!rule.multi && code === LF) return j;
    if (src.startsWith(rule.open, j)) return j + rule.open.length;
    j += 1;
  }
  return n;
}

/* ── 通用扫描器 ─────────────────────────────────────────────────────────── */

interface Grammar {
  kw: Set<string>;
  types?: Set<string>;
  /** 行注释前缀。 */
  line?: string[];
  /** 块注释的起止。 */
  block?: [string, string];
  /** 行注释前必须是行首或空白：`a#b` 在 shell / yaml 里不是注释。 */
  lineNeedsBoundary?: boolean;
  strings?: StrRule[];
  /** 关键字大小写不敏感（SQL）。 */
  ci?: boolean;
  /** 大写开头的标识符当类型（TS / Rust / Go 的命名约定）。 */
  capsAreTypes?: boolean;
  /** `format!(…)` 这种宏。 */
  macros?: boolean;
  lifetimes?: boolean;
  dollarVars?: boolean;
  /** 后面跟冒号的字符串是键（JSON）。 */
  keyBeforeColon?: boolean;
}

/**
 * 标识符归类。顺序有讲究：先查关键字与内建类型表，再看后面是不是 `(`，最后才轮到
 * 「大写开头 = 类型」这条约定——不然 Go 的 `fmt.Println(` 会被涂成类型而不是调用。
 */
function classify(word: string, src: string, end: number, g: Grammar): TokenKind {
  const key = g.ci ? word.toLowerCase() : word;
  if (g.kw.has(key)) return "kw";
  if (g.types?.has(key)) return "type";
  if (src.charCodeAt(end) === 40 /* ( */) return "fn";
  const first = word.charCodeAt(0);
  if (g.capsAreTypes && first >= 65 && first <= 90) return "type";
  return "plain";
}

function scan(src: string, g: Grammar, out: Sink): void {
  const n = src.length;
  let i = 0;

  while (i < n) {
    const code = src.charCodeAt(i);

    if (isSpace(code)) {
      const ws = at(RE_WS, src, i) ?? src[i] ?? "";
      out.push("plain", ws);
      i += ws.length;
      continue;
    }

    if (g.block && src.startsWith(g.block[0], i)) {
      const hit = src.indexOf(g.block[1], i + g.block[0].length);
      const end = hit < 0 ? n : hit + g.block[1].length;
      out.push("cmt", src.slice(i, end));
      i = end;
      continue;
    }

    if (g.line) {
      let prefix: string | null = null;
      for (const p of g.line) {
        if (src.startsWith(p, i)) {
          prefix = p;
          break;
        }
      }
      const boundary = !g.lineNeedsBoundary || i === 0 || isSpace(src.charCodeAt(i - 1));
      if (prefix && boundary) {
        const hit = src.indexOf("\n", i);
        const end = hit < 0 ? n : hit;
        out.push("cmt", src.slice(i, end));
        i = end;
        continue;
      }
    }

    if (g.lifetimes && code === 39 /* ' */) {
      const life = at(RE_LIFETIME, src, i);
      if (life) {
        out.push("type", life);
        i += life.length;
        continue;
      }
    }

    if (g.dollarVars && code === 36 /* $ */) {
      const v = at(RE_SHVAR, src, i);
      if (v) {
        out.push("type", v);
        i += v.length;
        continue;
      }
    }

    if (g.strings) {
      let rule: StrRule | null = null;
      for (const r of g.strings) {
        if (src.startsWith(r.open, i)) {
          rule = r;
          break;
        }
      }
      if (rule) {
        const end = endOfString(src, i, rule);
        let kind: TokenKind = "str";
        if (g.keyBeforeColon) {
          let k = end;
          while (k < n && (src.charCodeAt(k) === SPACE || src.charCodeAt(k) === TAB)) k += 1;
          if (src.charCodeAt(k) === 58 /* : */) kind = "type";
        }
        out.push(kind, src.slice(i, end));
        i = end;
        continue;
      }
    }

    if (code >= 48 && code <= 57 /* 0-9 */) {
      const num = at(RE_NUM, src, i);
      if (num) {
        out.push("num", num);
        i += num.length;
        continue;
      }
    }

    const word = at(RE_IDENT, src, i);
    if (word) {
      const end = i + word.length;
      if (g.macros && src.charCodeAt(end) === 33 /* ! */ && "([{".includes(src[end + 1] ?? "")) {
        out.push("fn", src.slice(i, end + 1));
        i = end + 1;
        continue;
      }
      out.push(classify(word, src, end, g), word);
      i = end;
      continue;
    }

    if (code > 127) {
      const wide = at(RE_WIDE, src, i) ?? src[i] ?? "";
      out.push("plain", wide);
      i += wide.length;
      continue;
    }

    out.push("punct", src[i] ?? "");
    i += 1;
  }
}

/* ── 各语言的表 ─────────────────────────────────────────────────────────── */

const words = (s: string) => new Set(s.trim().split(/\s+/));

const TS: Grammar = {
  kw: words(`
    const let var function return new delete typeof instanceof void this
    if else for while do break continue switch case default
    class extends super implements interface type enum
    import export from as await async yield of in
    try catch finally throw debugger
    null undefined true false
    static get set public private protected readonly abstract declare namespace module
    satisfies keyof infer is out override accessor using
  `),
  types: words(`
    string number boolean object symbol bigint any unknown never void
    Array Promise Record Map Set WeakMap WeakSet Date RegExp Error JSON Math
    Partial Required Readonly Pick Omit ReturnType Awaited
  `),
  line: ["//"],
  block: ["/*", "*/"],
  strings: [
    { open: "`", multi: true, esc: true },
    { open: '"', esc: true },
    { open: "'", esc: true },
  ],
  capsAreTypes: true,
};

const JSON_G: Grammar = {
  kw: words("true false null"),
  line: ["//"],
  block: ["/*", "*/"],
  strings: [{ open: '"', esc: true }],
  keyBeforeColon: true,
};

const PY: Grammar = {
  kw: words(`
    def class lambda return yield if elif else for while break continue pass
    import from as with try except finally raise assert del global nonlocal
    in is not and or None True False async await match case self cls
  `),
  types: words("int float str bool bytes bytearray list dict set frozenset tuple object type complex range"),
  line: ["#"],
  strings: [
    { open: '"""', multi: true, esc: true },
    { open: "'''", multi: true, esc: true },
    { open: '"', esc: true },
    { open: "'", esc: true },
  ],
  capsAreTypes: true,
};

const RUST: Grammar = {
  kw: words(`
    fn let mut const static struct enum union impl trait type where
    for while loop if else match return break continue
    use pub mod crate self Self super as in ref move box dyn
    unsafe async await extern true false
  `),
  types: words(`
    i8 i16 i32 i64 i128 isize u8 u16 u32 u64 u128 usize f32 f64 bool char str
    String Vec Option Result Box Rc Arc Cell RefCell HashMap HashSet BTreeMap Mutex RwLock
  `),
  line: ["//"],
  block: ["/*", "*/"],
  strings: [
    { open: '"', esc: true },
    { open: "'", esc: true },
  ],
  capsAreTypes: true,
  macros: true,
  lifetimes: true,
};

const GO: Grammar = {
  kw: words(`
    package import func var const type struct interface map chan
    return if else for range switch case default break continue fallthrough
    go defer select goto nil true false iota
    make new len cap append copy delete panic recover
  `),
  types: words(`
    int int8 int16 int32 int64 uint uint8 uint16 uint32 uint64 uintptr
    float32 float64 complex64 complex128 string bool byte rune error any
  `),
  line: ["//"],
  block: ["/*", "*/"],
  strings: [
    { open: "`", multi: true },
    { open: '"', esc: true },
    { open: "'", esc: true },
  ],
  capsAreTypes: true,
};

const SH: Grammar = {
  kw: words(`
    if then elif else fi for while until do done case esac in function select
    return exit break continue local export readonly declare unset source alias
    set trap shift eval exec time
    echo printf cd pwd ls cat sed awk grep find curl git npm cargo sudo mkdir rm cp mv
  `),
  line: ["#"],
  lineNeedsBoundary: true,
  strings: [{ open: '"', esc: true }, { open: "'" }],
  dollarVars: true,
};

const SQL: Grammar = {
  kw: words(`
    select from where group by order having insert into values update set delete
    create table drop alter add column primary key foreign references index view
    join left right full inner outer cross on using as and or not null is
    in between like ilike exists any all limit offset distinct returning
    case when then else end union intersect except with recursive
    default constraint unique check cascade begin commit rollback transaction
    asc desc count sum avg min max coalesce cast
  `),
  types: words(`
    int integer bigint smallint tinyint text varchar char boolean bool
    date time timestamp timestamptz interval numeric decimal real double float
    serial bigserial uuid json jsonb bytea blob array
  `),
  line: ["--"],
  block: ["/*", "*/"],
  strings: [{ open: "'" }, { open: '"' }],
  ci: true,
};

/** YAML / TOML 的值部分共用：只要认得字符串、数字、布尔和行尾注释。 */
const YAML_VALUE: Grammar = {
  kw: words("true false null yes no on off True False Null None ~"),
  line: ["#"],
  lineNeedsBoundary: true,
  strings: [{ open: '"', esc: true }, { open: "'" }],
};

const TOML_VALUE: Grammar = {
  kw: words("true false inf nan"),
  line: ["#"],
  strings: [
    { open: '"""', multi: true, esc: true },
    { open: "'''", multi: true },
    { open: '"', esc: true },
    { open: "'" },
  ],
};

/* ── 标记语言 ───────────────────────────────────────────────────────────── */

const RE_TAG = /<\/?[A-Za-z][A-Za-z0-9:._-]*/y;
const RE_ATTR = /[^\s=<>/"'`]+/y;

/** HTML / XML / SVG：标签名当关键字、属性名当类型、属性值当字符串，正文原样。 */
function scanMarkup(src: string, out: Sink): void {
  const n = src.length;
  let i = 0;

  while (i < n) {
    const lt = src.indexOf("<", i);
    if (lt < 0) {
      out.push("plain", src.slice(i));
      return;
    }
    if (lt > i) out.push("plain", src.slice(i, lt));
    i = lt;

    if (src.startsWith("<!--", i)) {
      const hit = src.indexOf("-->", i + 4);
      const end = hit < 0 ? n : hit + 3;
      out.push("cmt", src.slice(i, end));
      i = end;
      continue;
    }

    // <!DOCTYPE …> / <?xml …?>：整条当一个记号，没人需要读它的内部结构。
    if (src.startsWith("<!", i) || src.startsWith("<?", i)) {
      const hit = src.indexOf(">", i);
      const end = hit < 0 ? n : hit + 1;
      out.push("kw", src.slice(i, end));
      i = end;
      continue;
    }

    const tag = at(RE_TAG, src, i);
    if (!tag) {
      // 孤零零一个 `<`（JS 里的小于号，或者流式刚吐出来的半个标签）。
      out.push("punct", "<");
      i += 1;
      continue;
    }
    const slash = tag.startsWith("</") ? 2 : 1;
    out.push("punct", tag.slice(0, slash));
    out.push("kw", tag.slice(slash));
    i += tag.length;

    while (i < n) {
      const code = src.charCodeAt(i);
      if (code === 62 /* > */) {
        out.push("punct", ">");
        i += 1;
        break;
      }
      if (code === 47 /* / */ && src.charCodeAt(i + 1) === 62) {
        out.push("punct", "/>");
        i += 2;
        break;
      }
      if (isSpace(code)) {
        const ws = at(RE_WS, src, i) ?? src[i] ?? "";
        out.push("plain", ws);
        i += ws.length;
        continue;
      }
      if (code === 61 /* = */) {
        out.push("punct", "=");
        i += 1;
        continue;
      }
      if (code === 34 /* " */ || code === 39 /* ' */) {
        // 属性值不跨行：流式里半写完的 `class="` 否则会把后面整块染成字符串。
        const end = endOfString(src, i, { open: src[i] ?? '"' });
        out.push("str", src.slice(i, end));
        i = end;
        continue;
      }
      const attr = at(RE_ATTR, src, i);
      if (attr) {
        out.push("type", attr);
        i += attr.length;
        continue;
      }
      out.push("punct", src[i] ?? "");
      i += 1;
    }
  }
}

/* ── CSS ────────────────────────────────────────────────────────────────── */

const RE_CSS_VAR = /--[A-Za-z0-9_-]+/y;
const RE_CSS_HEX = /#(?:[0-9a-fA-F]{8}|[0-9a-fA-F]{6}|[0-9a-fA-F]{4}|[0-9a-fA-F]{3})(?![0-9a-fA-F])/y;
const RE_CSS_AT = /@[A-Za-z-]+/y;
const RE_CSS_NUM = /(?:\d+\.?\d*|\.\d+)(?:%|[A-Za-z]+)?/y;
const RE_CSS_IDENT = /-?[A-Za-z_][A-Za-z0-9_-]*/y;
const RE_CSS_BANG = /![ \t]*[A-Za-z-]+/y;

function scanCss(src: string, out: Sink): void {
  const n = src.length;
  let i = 0;
  // 属性名靠花括号深度认：`color:` 在块里是属性，`a:hover` 在块外是选择器。
  let depth = 0;

  while (i < n) {
    const code = src.charCodeAt(i);

    if (isSpace(code)) {
      const ws = at(RE_WS, src, i) ?? src[i] ?? "";
      out.push("plain", ws);
      i += ws.length;
      continue;
    }
    if (src.startsWith("/*", i)) {
      const hit = src.indexOf("*/", i + 2);
      const end = hit < 0 ? n : hit + 2;
      out.push("cmt", src.slice(i, end));
      i = end;
      continue;
    }
    if (code === 34 || code === 39) {
      const end = endOfString(src, i, { open: src[i] ?? '"', esc: true });
      out.push("str", src.slice(i, end));
      i = end;
      continue;
    }
    if (code === 45 && src.charCodeAt(i + 1) === 45) {
      const v = at(RE_CSS_VAR, src, i);
      if (v) {
        out.push("type", v);
        i += v.length;
        continue;
      }
    }
    if (code === 35 /* # */) {
      const hex = at(RE_CSS_HEX, src, i);
      if (hex) {
        out.push("num", hex);
        i += hex.length;
        continue;
      }
    }
    if (code === 64 /* @ */) {
      const rule = at(RE_CSS_AT, src, i);
      if (rule) {
        out.push("kw", rule);
        i += rule.length;
        continue;
      }
    }
    if (code === 33 /* ! */) {
      const bang = at(RE_CSS_BANG, src, i);
      if (bang) {
        out.push("kw", bang);
        i += bang.length;
        continue;
      }
    }
    if ((code >= 48 && code <= 57) || (code === 46 && src.charCodeAt(i + 1) >= 48 && src.charCodeAt(i + 1) <= 57)) {
      const num = at(RE_CSS_NUM, src, i);
      if (num) {
        out.push("num", num);
        i += num.length;
        continue;
      }
    }
    if (code === 123 /* { */) {
      depth += 1;
      out.push("punct", "{");
      i += 1;
      continue;
    }
    if (code === 125 /* } */) {
      depth = Math.max(0, depth - 1);
      out.push("punct", "}");
      i += 1;
      continue;
    }
    const ident = at(RE_CSS_IDENT, src, i);
    if (ident) {
      const end = i + ident.length;
      let kind: TokenKind = "plain";
      if (src.charCodeAt(end) === 40 /* ( */) kind = "fn";
      else if (depth > 0) {
        let k = end;
        while (k < n && (src.charCodeAt(k) === SPACE || src.charCodeAt(k) === TAB)) k += 1;
        if (src.charCodeAt(k) === 58 /* : */) kind = "type";
      }
      out.push(kind, ident);
      i = end;
      continue;
    }
    if (code > 127) {
      const wide = at(RE_WIDE, src, i) ?? src[i] ?? "";
      out.push("plain", wide);
      i += wide.length;
      continue;
    }
    out.push("punct", src[i] ?? "");
    i += 1;
  }
}

/* ── 行导向的三种：YAML / TOML / Markdown ───────────────────────────────── */

/** 逐行遍历，行尾的换行留在行里——拼回去必须一字不差。 */
function eachLine(src: string, fn: (line: string) => void): void {
  const n = src.length;
  let start = 0;
  while (start < n) {
    const hit = src.indexOf("\n", start);
    const end = hit < 0 ? n : hit + 1;
    fn(src.slice(start, end));
    start = end;
  }
}

const RE_YAML_KEY = /^[^\s#][^:]*?:(?=\s|$)/;
const RE_YAML_LEAD = /^\s*(?:-\s+)*/;

function scanYaml(src: string, out: Sink): void {
  eachLine(src, (line) => {
    const lead = RE_YAML_LEAD.exec(line)?.[0] ?? "";
    const rest = line.slice(lead.length);
    // 缩进原样，`- ` 当标点：一列对齐的短横比一列灰字好认层级。
    if (lead) {
      const spaces = lead.length - lead.trimStart().length;
      out.push("plain", lead.slice(0, spaces));
      out.push("punct", lead.slice(spaces));
    }
    if (!rest) return;
    if (rest.startsWith("#")) {
      out.push("cmt", rest);
      return;
    }
    if (rest.startsWith("---") || rest.startsWith("...")) {
      out.push("punct", rest);
      return;
    }
    const key = RE_YAML_KEY.exec(rest)?.[0];
    if (key) {
      out.push("type", key.slice(0, -1));
      out.push("punct", ":");
      scan(rest.slice(key.length), YAML_VALUE, out);
      return;
    }
    scan(rest, YAML_VALUE, out);
  });
}

const RE_TOML_SECTION = /^\[\[?[^\]\n]*\]?\]?/;
const RE_TOML_KEY = /^[A-Za-z0-9_.\-"']+(?=[ \t]*=)/;

function scanToml(src: string, out: Sink): void {
  eachLine(src, (line) => {
    const indent = line.length - line.trimStart().length;
    if (indent) out.push("plain", line.slice(0, indent));
    const rest = line.slice(indent);
    if (!rest) return;
    if (rest.startsWith("#")) {
      out.push("cmt", rest);
      return;
    }
    const section = RE_TOML_SECTION.exec(rest)?.[0];
    if (section) {
      out.push("type", section);
      scan(rest.slice(section.length), TOML_VALUE, out);
      return;
    }
    const key = RE_TOML_KEY.exec(rest)?.[0];
    if (key) {
      out.push("type", key);
      scan(rest.slice(key.length), TOML_VALUE, out);
      return;
    }
    scan(rest, TOML_VALUE, out);
  });
}

const RE_MD_FENCE = /^\s*(?:```|~~~)/;
const RE_MD_HEAD = /^(\s*)(#{1,6}\s.*)$/;
const RE_MD_MARK = /^(\s*)([-*+]|\d+[.)])(\s)/;
const RE_MD_INLINE = /(`+[^\n]*?`+)|(\*\*[^*\n]+\*\*)|(!?\[[^\]\n]*\]\([^)\n]*\))|(https?:\/\/[^\s)]+)/g;

function scanMd(src: string, out: Sink): void {
  eachLine(src, (line) => {
    if (RE_MD_FENCE.test(line)) {
      out.push("punct", line);
      return;
    }
    const head = RE_MD_HEAD.exec(line.replace(/\n$/, ""));
    if (head) {
      out.push("plain", head[1] ?? "");
      out.push("kw", head[2] ?? "");
      if (line.endsWith("\n")) out.push("plain", "\n");
      return;
    }
    if (/^\s*>/.test(line)) {
      out.push("cmt", line);
      return;
    }
    let rest = line;
    const mark = RE_MD_MARK.exec(line);
    if (mark) {
      out.push("plain", mark[1] ?? "");
      out.push("punct", mark[2] ?? "");
      out.push("plain", mark[3] ?? "");
      rest = line.slice((mark[0] ?? "").length);
    }
    let last = 0;
    RE_MD_INLINE.lastIndex = 0;
    for (let m = RE_MD_INLINE.exec(rest); m; m = RE_MD_INLINE.exec(rest)) {
      if (m.index > last) out.push("plain", rest.slice(last, m.index));
      // 粗体给「函数名」那档亮度：它本来就是「这里请加重」的意思。
      out.push(m[0].startsWith("**") ? "fn" : "str", m[0]);
      last = m.index + m[0].length;
    }
    if (last < rest.length) out.push("plain", rest.slice(last));
  });
}

/* ── 入口 ───────────────────────────────────────────────────────────────── */

const ALIAS: Record<string, string> = {
  ts: "ts", tsx: "ts", typescript: "ts", mts: "ts", cts: "ts",
  js: "ts", jsx: "ts", javascript: "ts", mjs: "ts", cjs: "ts", node: "ts",
  json: "json", jsonc: "json", json5: "json",
  py: "py", python: "py", python3: "py",
  rs: "rust", rust: "rust",
  go: "go", golang: "go",
  sh: "sh", bash: "sh", zsh: "sh", shell: "sh", console: "sh",
  sql: "sql", psql: "sql", postgres: "sql", postgresql: "sql", mysql: "sql", sqlite: "sql",
  html: "markup", htm: "markup", xhtml: "markup", xml: "markup", svg: "markup", vue: "markup",
  css: "css", scss: "css", less: "css",
  yaml: "yaml", yml: "yaml",
  toml: "toml",
  md: "md", markdown: "md",
};

/**
 * 超过这个长度就不高亮了。流式里每来一帧都要把整块重扫一遍，几万字符的块开始掉帧；
 * 何况那么长的代码没人会在一个聊天气泡里读，滚出去看才对。
 */
const MAX_LEN = 40_000;

/** 语言标签能不能高亮。给 UI 判断要不要挂 `.hl` 类。 */
export function canHighlight(lang: string): boolean {
  return ALIAS[lang.trim().toLowerCase()] !== undefined;
}

export function highlight(lang: string, src: string): Token[] {
  if (!src) return [];
  const id = ALIAS[lang.trim().toLowerCase()];
  if (!id || src.length > MAX_LEN) return [{ kind: "plain", text: src }];

  const out = new Sink();
  switch (id) {
    case "ts": scan(src, TS, out); break;
    case "json": scan(src, JSON_G, out); break;
    case "py": scan(src, PY, out); break;
    case "rust": scan(src, RUST, out); break;
    case "go": scan(src, GO, out); break;
    case "sh": scan(src, SH, out); break;
    case "sql": scan(src, SQL, out); break;
    case "markup": scanMarkup(src, out); break;
    case "css": scanCss(src, out); break;
    case "yaml": scanYaml(src, out); break;
    case "toml": scanToml(src, out); break;
    case "md": scanMd(src, out); break;
    default: return [{ kind: "plain", text: src }];
  }
  return out.out;
}
