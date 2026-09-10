/**
 * 依赖为零的 Markdown 渲染，照线上试用页那份搬（去掉了 Tailwind 类，换成 `.md-*`）。
 *
 * 不上 react-markdown：桌面端运行时依赖至今只有 react / tauri，为一处引入一棵解析树不值。
 * 更要紧的是流式——回复没写完时，未闭合的 ``` 围栏是常态，通用库会把半截代码当正文渲染、
 * 等围栏闭合再整段跳回代码块，画面来回蹦；自己写才能把「围栏还没收尾」也当代码块画。
 *
 * 认得的子集：围栏代码（带语法高亮与 HTML / SVG 沙箱预览）、行内代码（含多重反引号）、
 * 粗斜体、删除线、转义字符、链接、图片、两级列表与任务列表、h1–h4、可嵌套的引用、
 * 带对齐的表格、分隔线。全程构造 React 元素、不碰 innerHTML，模型输出再怪也翻不出注入。
 *
 * 明确不做：HTML 直通（正文里的 `<div>` 按字面显示——放开它等于把注入面还回去）、
 * 脚注、定义列表、下划线式的强调（`_x_` 会把 snake_case 撕碎）、setext 标题。
 */
import { openUrl } from "@tauri-apps/plugin-opener";
import { memo, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { Icon } from "../../ui/primitives";
import { useTheme } from "../../ui/theme";
import type { ArtifactRef } from "./artifacts";
import { highlight } from "./highlight";
import { PgIcon } from "./PgIcon";
import "./markdown.css";

export type Align = "left" | "center" | "right";

export interface ListItem {
  depth: number;
  text: string;
  /** `- [ ]` / `- [x]`：渲染成一个禁用的复选框。 */
  task?: boolean;
  done?: boolean;
}

export type Block =
  | { kind: "code"; lang: string; body: string; /** 围栏还没收尾。流式途中不给预览。 */ open?: boolean }
  | { kind: "heading"; level: number; text: string }
  | { kind: "list"; ordered: boolean; items: ListItem[] }
  | { kind: "quote"; blocks: Block[] }
  | { kind: "table"; head: string[]; align: (Align | null)[]; rows: string[][] }
  | { kind: "hr" }
  | { kind: "p"; text: string };

/* ── 预览 ───────────────────────────────────────────────────────────────── */

/** 可进沙箱预览的语言标签。xml 只在正文像 SVG 时才开，避免把任意 XML 当图画。 */
export function previewKind(lang: string, body: string): "html" | "svg" | null {
  const l = lang.trim().toLowerCase();
  if (l === "html" || l === "htm" || l === "xhtml") return "html";
  if (l === "svg") return "svg";
  if (l === "xml") return /^\s*<svg[\s>]/i.test(body) ? "svg" : null;
  if (l) return null;
  // 模型偶尔忘写语言标签，但正文就是整页 HTML / 一张 SVG。
  if (/^\s*(?:<!doctype\s+html|<html[\s>])/i.test(body)) return "html";
  if (/^\s*<svg[\s>]/i.test(body)) return "svg";
  return null;
}

/** 包裹文档要用到的几个颜色。iframe 是另一个文档，看不见宿主的变量表，只能把值抄过去。 */
export interface FrameTheme {
  paper: string;
  ink: string;
  accent: string;
}

const DARK_FRAME: FrameTheme = { paper: "#0a0c0d", ink: "#e9eef0", accent: "#2dd4a0" };

/**
 * 从宿主当前生效的 tokens 里读颜色：亮色主题下 iframe 若还是深底，就是一块黑纸贴在白卡上。
 * 读不到（测试环境、变量表没挂上）就退回暗色那套默认值。
 */
export function frameTheme(): FrameTheme {
  if (typeof window === "undefined" || typeof getComputedStyle !== "function") return DARK_FRAME;
  const cs = getComputedStyle(document.documentElement);
  const pick = (name: string, fallback: string) => cs.getPropertyValue(name).trim() || fallback;
  return { paper: pick("--color-paper", DARK_FRAME.paper), ink: pick("--color-ink", DARK_FRAME.ink), accent: pick("--accent", DARK_FRAME.accent) };
}

function frameCss(t: FrameTheme, svg: boolean): string {
  const base = `html,body{margin:0}
body{padding:14px;background:${t.paper};color:${t.ink};font:13px/1.7 -apple-system,BlinkMacSystemFont,"SF Pro Text","PingFang SC",system-ui,sans-serif}
a{color:${t.accent}}
img,svg{max-width:100%;height:auto}
::selection{background:${t.accent}44}`;
  // SVG 居中摆：一张图贴在左上角像没加载完。
  return svg ? `${base}\nbody{display:flex;align-items:center;justify-content:center;min-height:calc(100vh - 28px)}` : base;
}

/** 把片段包成能独立渲染的文档。已经是完整 HTML 的不再包——再包会把它自己的 head 吃掉。 */
export function previewDoc(kind: "html" | "svg", body: string, theme: FrameTheme = frameTheme()): string {
  const src = body.trim();
  if (kind === "html" && /^(?:<!doctype\s+html|<html[\s>])/i.test(src)) return src;
  return `<!DOCTYPE html><html lang="zh"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><style>${frameCss(theme, kind === "svg")}</style></head><body>${src}</body></html>`;
}

/**
 * 预览文档的 blob: 地址。代码块的内嵌预览与 artifact 预览区共用。
 *
 * 用 blob: 而不是 srcDoc：srcdoc 文档没有自己的地址，一切都算「继承自宿主」，在严格 CSP
 * 下能不能加载、里面的样式脚本按谁的策略算，各 webview 说法不一。blob: 是一个真地址，
 * 宿主只要放一条 `frame-src blob:` 就说得清；也省掉把整篇 HTML 塞进一个 DOM 属性里再让
 * webview 二次解析。
 *
 * 订阅主题：切了亮暗，已经打开的预览要跟着换纸色，而不是等下一次重渲染。theme 本身不进
 * previewDoc —— 颜色是从当时生效的 tokens 里读的，它只负责触发重算。
 */
export function usePreviewUrl(kind: "html" | "svg" | null, body: string, active = true): string | null {
  const { theme } = useTheme();
  const doc = useMemo(() => (active && kind ? previewDoc(kind, body) : null), [active, kind, body, theme]);
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!doc) {
      setUrl(null);
      return;
    }
    const next = URL.createObjectURL(new Blob([doc], { type: "text/html" }));
    setUrl(next);
    return () => URL.revokeObjectURL(next);
  }, [doc]);
  return url;
}

/* ── artifact（可预览的代码块）──────────────────────────────────────────── */

/** 卡片与预览区共用的类别名。 */
export function artifactKindLabel(kind: "html" | "svg"): string {
  return kind === "svg" ? "SVG 图像" : "HTML 页面";
}

/** 模型写的标题里偶尔带几个实体，卡片上别把 `&amp;` 原样摆出来。 */
function decodeEntities(s: string): string {
  return s
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;|&apos;/g, "'")
    .replace(/&nbsp;/g, " ");
}

/**
 * 给一块可预览的代码起个名字：先挖 `<title>`，HTML 再试第一个 `<h1>`，都没有就用类别名。
 * 卡片和预览区的标题都靠它 —— 「未命名」读不出是哪一块。
 *
 * 兜底的就是类别名本身：卡片与面板的元信息行靠「标题 === 类别名」认出这种情形，
 * 把元信息里的类别名省掉，不然同一行上「SVG 图像 · SVG 图像 · 14 行」说两遍。
 */
export function artifactTitle(kind: "html" | "svg", body: string): string {
  const titled = /<title[^>]*>([\s\S]*?)<\/title>/i.exec(body)?.[1];
  const heading = kind === "html" ? /<h1[^>]*>([\s\S]*?)<\/h1>/i.exec(body)?.[1] : undefined;
  const text = decodeEntities((titled ?? heading ?? "").replace(/<[^>]+>/g, ""))
    .replace(/\s+/g, " ")
    .trim();
  if (text) return text.length > 60 ? `${text.slice(0, 60)}…` : text;
  return artifactKindLabel(kind);
}

/* ── 分块 ───────────────────────────────────────────────────────────────── */

const FENCE = /^\s*(```+|~~~+)\s*(\S*)\s*$/;
const LIST_ITEM = /^(\s*)([-*+]|\d+[.)])\s+(.*)$/;
const TASK = /^\[([ xX])\]\s+(.*)$/;
const HR = /^\s*(-{3,}|\*{3,}|_{3,})\s*$/;
/** 引用最多套这么深。`>>>>>>…` 这种输入不该把调用栈递归穿了。 */
const QUOTE_MAX = 3;

/**
 * 行尾空白一律剥掉。Markdown 用「行尾两个空格」表示硬换行，而 `.md-p` 是 `pre-wrap`，
 * 换行本来就照打——所以这里只需要把那两个空格擦干净，否则它们会挂在行尾影响折行位置。
 *
 * 反过来说，本渲染器不做 CommonMark 的「软换行合成一段」：中文正文按 80 列折行时，
 * 合并会在句子中间插进一堆可见的空格，比多换一行难看得多。
 */
const rstrip = (s: string) => s.replace(/[ \t]+$/, "");

function splitRow(line: string): string[] {
  return line
    .trim()
    .replace(/^\|/, "")
    .replace(/\|$/, "")
    .split("|")
    .map((c) => c.trim());
}

/** `:---` 左、`:---:` 居中、`---:` 右。没写冒号的给 null，用表格自己的默认。 */
function parseAlign(separator: string): (Align | null)[] {
  return splitRow(separator).map((c) => {
    const left = c.startsWith(":");
    const right = c.endsWith(":");
    if (left && right) return "center";
    if (right) return "right";
    if (left) return "left";
    return null;
  });
}

export function parseBlocks(src: string, depth = 0): Block[] {
  const lines = src.replace(/\r\n/g, "\n").split("\n");
  const blocks: Block[] = [];
  let i = 0;

  while (i < lines.length) {
    const line = lines[i] ?? "";

    const fence = line.match(FENCE);
    if (fence) {
      // 闭合围栏可以缺席：流式输出里代码块常常还没写完，照常渲染到当前末尾。
      const marker = fence[1] ?? "```";
      const close = marker.startsWith("`") ? /^\s*```+\s*$/ : /^\s*~~~+\s*$/;
      const body: string[] = [];
      i += 1;
      let closed = false;
      while (i < lines.length) {
        const bodyLine = lines[i] ?? "";
        if (close.test(bodyLine)) {
          closed = true;
          i += 1;
          break;
        }
        body.push(bodyLine);
        i += 1;
      }
      blocks.push({ kind: "code", lang: fence[2] ?? "", body: body.join("\n"), open: !closed });
      continue;
    }

    if (!line.trim()) {
      i += 1;
      continue;
    }

    const h = line.match(/^(#{1,4})\s+(.*)$/);
    if (h) {
      const marker = h[1] ?? "#";
      blocks.push({ kind: "heading", level: marker.length, text: rstrip(h[2] ?? "") });
      i += 1;
      continue;
    }

    if (HR.test(line)) {
      blocks.push({ kind: "hr" });
      i += 1;
      continue;
    }

    if (/^\s*>/.test(line)) {
      const q: string[] = [];
      while (i < lines.length) {
        const quoteLine = lines[i] ?? "";
        if (!/^\s*>/.test(quoteLine)) break;
        q.push(quoteLine.replace(/^\s*>\s?/, ""));
        i += 1;
      }
      // 剥掉一层 `>` 再整段重解析：嵌套引用、引用里的列表和代码块就都白来了。
      const inner = q.join("\n");
      blocks.push({
        kind: "quote",
        blocks: depth >= QUOTE_MAX ? [{ kind: "p", text: inner }] : parseBlocks(inner, depth + 1),
      });
      continue;
    }

    const separatorLine = lines[i + 1] ?? "";
    if (
      /^\s*\|.*\|\s*$/.test(line) &&
      /^\s*\|?[\s:|-]+\|?\s*$/.test(separatorLine) &&
      separatorLine.includes("-")
    ) {
      const head = splitRow(line);
      const align = parseAlign(separatorLine);
      i += 2;
      const rows: string[][] = [];
      while (i < lines.length) {
        const rowLine = lines[i] ?? "";
        if (!rowLine.includes("|") || !/^\s*\|/.test(rowLine)) break;
        rows.push(splitRow(rowLine));
        i += 1;
      }
      blocks.push({ kind: "table", head, align, rows });
      continue;
    }

    if (LIST_ITEM.test(line)) {
      const ordered = /^\s*\d/.test(line);
      const items: ListItem[] = [];
      while (i < lines.length) {
        const itemLine = lines[i] ?? "";
        const m = itemLine.match(LIST_ITEM);
        if (m) {
          const itemDepth = (m[1] ?? "").length >= 2 ? 1 : 0;
          const raw = rstrip(m[3] ?? "");
          const task = raw.match(TASK);
          items.push(
            task
              ? { depth: itemDepth, text: task[2] ?? "", task: true, done: (task[1] ?? " ").toLowerCase() === "x" }
              : { depth: itemDepth, text: raw },
          );
          i += 1;
        } else {
          const previousItem = items.at(-1);
          if (!previousItem || !itemLine.trim() || !/^\s{2,}/.test(itemLine)) break;
          // 缩进的续行并进上一项，避免模型换行习惯把一个条目撕成两条。
          previousItem.text += ` ${itemLine.trim()}`;
          i += 1;
        }
      }
      blocks.push({ kind: "list", ordered, items });
      continue;
    }

    const para: string[] = [rstrip(line)];
    i += 1;
    while (i < lines.length) {
      const paragraphLine = lines[i] ?? "";
      if (
        !paragraphLine.trim() ||
        /^\s*(```|~~~|#{1,4}\s|>|\|)/.test(paragraphLine) ||
        HR.test(paragraphLine) ||
        LIST_ITEM.test(paragraphLine)
      ) {
        break;
      }
      para.push(rstrip(paragraphLine));
      i += 1;
    }
    blocks.push({ kind: "p", text: para.join("\n") });
  }

  return blocks;
}

/* ── 行内 ───────────────────────────────────────────────────────────────── */

const URL_OK = /^https?:\/\//;
/** 图片只收 data:。桌面端 CSP 不放外域图，画一个破图不如把地址原样给人看。 */
const IMG_OK = /^data:image\/[a-z0-9+.-]+[;,]/i;

/**
 * 行内标记。一个正则扫完，靠命中文本的开头分辨是哪种——比八个正则轮流跑少七遍全文扫描，
 * 流式下每个增量帧都要重渲染，这点开销值得省。顺序即优先级：
 *
 *   · 转义排第一，`\*` 才不会先被斜体吃掉；
 *   · 多重反引号排在单反引号前，`` `a` `` 这种「代码里含反引号」才拆得对；
 *   · 图片排在链接前，`![alt](url)` 的后半截本身就是个合法链接，链接分支先命中会把图片
 *     渲染成一个前面挂着感叹号的链接。
 *
 * 斜体两端禁空格，否则「2 * 3 * 4」这种算式会被吃成斜体。
 */
const INLINE_SRC = [
  /\\[\\`*_{}[\]()#+\-.!>~|]/,
  /``+[^\n]*?``+/,
  /`[^`\n]+`/,
  /\*\*[^*\n]+\*\*/,
  /\*(?!\s)[^*\n]*[^*\s]\*/,
  /~~[^~\n]+~~/,
  /!\[[^\]\n]*\]\([^\s)]+\)/,
  /\[[^\]\n]+\]\([^\s)]+\)/,
  /https?:\/\/[^\s<>"')\]，。；]+/,
]
  .map((re) => `(?:${re.source})`)
  .join("|");

/** 剥掉两端等长（或不等长）的反引号；CommonMark 还允许各去掉一个空格，`` ` `` 才写得出。 */
function codeText(span: string): string {
  const open = /^`+/.exec(span)?.[0].length ?? 1;
  const close = /`+$/.exec(span)?.[0].length ?? 1;
  const body = span.slice(open, Math.max(open, span.length - close));
  if (body.length > 2 && body.startsWith(" ") && body.endsWith(" ") && body.trim()) {
    return body.slice(1, -1);
  }
  return body;
}

/**
 * 外链。桌面端不让前端自己跳：主窗口一旦导航到外站，用户就再也回不到应用里了。
 * 走 opener 插件交给系统浏览器（capabilities 只放了 http(s)/mailto 的默认范围），
 * 跟「网页控制台」那些按钮同一条路。`href` 留着，纯粹为了 hover 时能看清要去哪。
 */
function Link({ href, bare, children }: { href: string; bare?: boolean; children: ReactNode }) {
  return (
    <a
      href={href}
      className={bare ? "md-link md-link-bare" : "md-link"}
      onClick={(e) => {
        e.preventDefault();
        void openUrl(href).catch(() => {});
      }}
    >
      {children}
    </a>
  );
}

function inline(text: string, keyBase: string): ReactNode[] {
  const out: ReactNode[] = [];
  let last = 0;
  let n = 0;
  // 每次调用各建一个实例：粗体分支会递归进本函数，共享 /g 实例的话递归会把 lastIndex 清零。
  const INLINE = new RegExp(INLINE_SRC, "g");
  for (let m = INLINE.exec(text); m; m = INLINE.exec(text)) {
    if (m.index > last) out.push(text.slice(last, m.index));
    const s = m[0];
    const key = `${keyBase}-${n++}`;
    if (s.startsWith("\\")) {
      out.push(s.slice(1));
    } else if (s.startsWith("`")) {
      out.push(
        <code key={key} className="md-code">
          {codeText(s)}
        </code>,
      );
    } else if (s.startsWith("**")) {
      out.push(<strong key={key}>{inline(s.slice(2, -2), key)}</strong>);
    } else if (s.startsWith("~~")) {
      out.push(<del key={key}>{s.slice(2, -2)}</del>);
    } else if (s.startsWith("*")) {
      out.push(<em key={key}>{s.slice(1, -1)}</em>);
    } else if (s.startsWith("![")) {
      const img = s.match(/^!\[([^\]]*)\]\(([^\s)]+)\)$/);
      const src = img?.[2]?.trim();
      if (src && IMG_OK.test(src)) {
        out.push(<img key={key} src={src} alt={img?.[1] ?? ""} className="md-img" />);
      } else {
        // 地址不合规就退回原文，别悄悄吞掉——链接错在哪得让人看见。
        out.push(s);
      }
    } else if (s.startsWith("[")) {
      const link = s.match(/^\[([^\]]+)\]\(([^\s)]+)\)$/);
      const label = link?.[1];
      const rawUrl = link?.[2]?.trim();
      const url = rawUrl && URL_OK.test(rawUrl) ? rawUrl : null;
      if (url && label) {
        out.push(
          <Link key={key} href={url}>
            {label}
          </Link>,
        );
      } else {
        out.push(s);
      }
    } else {
      out.push(
        <Link key={key} href={s} bare>
          {s}
        </Link>,
      );
    }
    last = m.index + s.length;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

/* ── 代码块 ─────────────────────────────────────────────────────────────── */

const PREVIEW_H = { svg: 280, html: 360 } as const;
/** 「展开」之后的高度。再高就该是另一个窗口了，不是气泡里的一格预览。 */
const PREVIEW_H_TALL = 620;
/** 超过这个宽度才给「换行」开关——短代码摆一个用不上的按钮只是噪音。 */
const LONG_LINE = /[^\n]{73,}/;

/** 带语法高亮的代码本体。代码块与 artifact 预览区的「代码」页共用同一副。 */
export function CodeBody({ lang, body, wrap }: { lang: string; body: string; wrap?: boolean }) {
  const tokens = useMemo(() => highlight(lang, body), [lang, body]);
  return (
    <pre className={wrap ? "md-pre-body is-wrap" : "md-pre-body"}>
      <code className="mono selectable">
        {tokens.map((t, ti) =>
          t.kind === "plain" ? (
            t.text
          ) : (
            <span key={ti} className={`hl-${t.kind}`}>
              {t.text}
            </span>
          ),
        )}
      </code>
    </pre>
  );
}

const CodeBlock = memo(function CodeBlock({
  lang,
  body,
  open,
}: {
  lang: string;
  body: string;
  open?: boolean;
}) {
  const [copied, setCopied] = useState(false);
  const [tab, setTab] = useState<"code" | "preview">("code");
  const [wrap, setWrap] = useState(false);
  const [tall, setTall] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const kind = previewKind(lang, body);
  // 围栏没收尾时预览没有意义（半截标签），流式途中也不默认切过去——
  // 画面会在「代码 ↔ 破版预览」之间来回跳。
  const canPreview = kind !== null && !open;
  const view = canPreview && tab === "preview" ? "preview" : "code";

  const long = useMemo(() => LONG_LINE.test(body), [body]);
  const url = usePreviewUrl(kind, body, view === "preview");

  function copy() {
    void navigator.clipboard.writeText(body).catch(() => {});
    setCopied(true);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(false), 1400);
  }

  return (
    <div className="md-pre">
      <div className="md-pre-head">
        <span className="mono md-pre-lang">{lang || "text"}</span>
        {canPreview ? (
          <div className="md-tabs" role="tablist" aria-label="代码或预览">
            <button
              type="button"
              role="tab"
              aria-selected={view === "code"}
              className="md-tab"
              onClick={() => setTab("code")}
            >
              代码
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={view === "preview"}
              className="md-tab"
              onClick={() => setTab("preview")}
            >
              预览
            </button>
          </div>
        ) : null}
        <span className="grow" />
        {view === "preview" ? (
          <button type="button" className="btn btn-sm btn-quiet" onClick={() => setTall((v) => !v)}>
            {tall ? "收起" : "展开"}
          </button>
        ) : long ? (
          <button
            type="button"
            className={wrap ? "btn btn-sm btn-quiet is-on" : "btn btn-sm btn-quiet"}
            onClick={() => setWrap((v) => !v)}
            title={wrap ? "长行改成横向滚动" : "长行折到下一行"}
          >
            换行
          </button>
        ) : null}
        <button type="button" className="btn btn-sm btn-quiet" onClick={copy}>
          <Icon name={copied ? "check" : "copy"} size={12} />
          {copied ? "已复制" : "复制"}
        </button>
      </div>
      {view === "preview" && kind && url ? (
        <iframe
          title="预览"
          src={url}
          // 给脚本，但不给 same-origin：预览页拿到的是一个不透明源，够不着父页面的
          // DOM，也发不出 Tauri 的 IPC。模型写的东西只能在这个格子里折腾。
          sandbox="allow-scripts"
          className="md-frame"
          style={{ height: tall ? PREVIEW_H_TALL : PREVIEW_H[kind] }}
        />
      ) : (
        <CodeBody lang={lang} body={body} wrap={wrap} />
      )}
    </div>
  );
});

/**
 * 一块 artifact 在对话里的样子：一张卡片，不是内嵌的预览。
 *
 * 一整篇 HTML 直接摊在消息流里，对话就被切成了几截 —— 读回复的人要的是先读完话，
 * 想看效果再点开。卡片只答三个问题：这是什么（图标 + 类别）、它叫什么（标题）、
 * 写完没有（生成中）。点一下，右侧预览区打开它；再点一下，收回去。
 */
function ArtifactCard({
  kind,
  lang,
  body,
  open,
  active,
  onOpen,
}: {
  kind: "html" | "svg";
  lang: string;
  body: string;
  open?: boolean;
  active: boolean;
  onOpen: () => void;
}) {
  const title = artifactTitle(kind, body);
  const lines = body ? body.split("\n").length : 0;
  // 标题就是类别名（没挖到 <title>）时，元信息里不再重复它；语言标签和类别
  // 不一样才摆（`html` 对「HTML 页面」是废话）。
  const kindLabel = artifactKindLabel(kind);
  const meta = [
    title !== kindLabel ? kindLabel : "",
    lang && lang.toLowerCase() !== kind ? lang : "",
    `${lines} 行`,
  ]
    .filter(Boolean)
    .join(" · ");
  return (
    <button
      type="button"
      className={`md-art${active ? " is-on" : ""}`}
      onClick={onOpen}
      aria-label={`${active ? "收起" : "在右侧预览"} ${title}`}
      aria-expanded={active}
    >
      <span className={`md-art-ico is-${kind}`} aria-hidden>
        <PgIcon name={kind === "svg" ? "image" : "window"} size={15} />
      </span>
      <span className="md-art-text">
        <span className="md-art-title truncate">{title}</span>
        <span className="md-art-meta num">
          {meta}
          {open ? (
            <>
              {" · "}
              <em className="md-art-live">生成中</em>
            </>
          ) : null}
        </span>
      </span>
      <Icon name="chevron" size={13} className="md-art-chev" />
    </button>
  );
}

/* ── 块渲染 ─────────────────────────────────────────────────────────────── */

function alignClass(a: Align | null | undefined): string | undefined {
  if (a === "center") return "md-al-c";
  if (a === "right") return "md-al-r";
  return undefined;
}

/**
 * 一块代码在消息流里的「坐标」：第几个代码块 + 这条消息的 artifact 通道。
 * 引用块里的代码不带坐标 —— 那里不画卡片，保持内嵌。
 */
interface BlockCtx {
  codeIndex: number;
  artifactLink?: ArtifactLink;
}

function renderBlock(b: Block, key: string, ctx?: BlockCtx): ReactNode {
  switch (b.kind) {
    case "code": {
      // 可预览的代码块（HTML / SVG）收成一张卡片，预览交给右侧的预览区；
      // 没有 artifact 通道时（别的场景复用 Markdown）退回内嵌预览的代码块。
      const kind = ctx?.artifactLink ? previewKind(b.lang, b.body) : null;
      if (ctx?.artifactLink && kind) {
        const link = ctx.artifactLink;
        const ref: ArtifactRef = { messageId: link.messageId, codeIndex: ctx.codeIndex };
        const active =
          link.active !== null &&
          link.active.messageId === ref.messageId &&
          link.active.codeIndex === ref.codeIndex;
        return (
          <ArtifactCard
            key={key}
            kind={kind}
            lang={b.lang}
            body={b.body}
            open={b.open}
            active={active}
            onOpen={() => link.onOpen(ref)}
          />
        );
      }
      return <CodeBlock key={key} lang={b.lang} body={b.body} open={b.open} />;
    }
    case "heading":
      // 用 `<p>` 不用 `<h1>`：一屏聊天里几十条回复各带一堆标题，真标题会把无障碍
      // 大纲搅成一团。字阶也压得很平——气泡窄、上下文短，标题稍大就把一句回答切成几块。
      return (
        <p key={key} className={`md-h md-h${Math.min(b.level, 4)}`}>
          {inline(b.text, key)}
        </p>
      );
    case "hr":
      return <hr key={key} className="md-hr" />;
    case "quote":
      return (
        <blockquote key={key} className="md-quote">
          {b.blocks.map((child, ci) => renderBlock(child, `${key}-${ci}`))}
        </blockquote>
      );
    case "list": {
      // 只认两级。模型输出偶尔更深，压平成第二级也比整段退回纯文本强。
      const roots: { item: ListItem; children: ListItem[] }[] = [];
      for (const it of b.items) {
        const parent = roots.at(-1);
        if (it.depth === 0 || !parent) roots.push({ item: it, children: [] });
        else parent.children.push(it);
      }
      const Tag = b.ordered ? "ol" : "ul";
      return (
        <Tag key={key} className="md-list">
          {roots.map((node, idx) => (
            <li key={idx} className={node.item.task ? "md-task" : undefined}>
              {node.item.task ? <Check done={node.item.done} /> : null}
              {inline(node.item.text, `${key}-${idx}`)}
              {node.children.length ? (
                <ul className="md-list md-list-sub">
                  {node.children.map((c, ci) => (
                    <li key={ci} className={c.task ? "md-task" : undefined}>
                      {c.task ? <Check done={c.done} /> : null}
                      {inline(c.text, `${key}-${idx}-${ci}`)}
                    </li>
                  ))}
                </ul>
              ) : null}
            </li>
          ))}
        </Tag>
      );
    }
    case "table":
      return (
        <div key={key} className="md-table-wrap">
          <table className="md-table">
            <thead>
              <tr>
                {b.head.map((c, ci) => (
                  <th key={ci} className={alignClass(b.align[ci])}>
                    {inline(c, `${key}-h${ci}`)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {b.rows.map((row, ri) => (
                <tr key={ri}>
                  {row.map((c, ci) => (
                    <td key={ci} className={alignClass(b.align[ci])}>
                      {inline(c, `${key}-${ri}-${ci}`)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    default:
      return (
        <p key={key} className="md-p">
          {inline(b.text, key)}
        </p>
      );
  }
}

/** 任务列表的勾。永远禁用：这是一条已经生成完的回复，勾掉它不该改变任何东西。 */
function Check({ done }: { done?: boolean }) {
  return <input type="checkbox" className="md-check" checked={!!done} disabled readOnly />;
}

/**
 * 一条消息的 artifact 通道。给了它，可预览的代码块就画成卡片而不是内嵌预览；
 * 卡片点开的预览区由上层（工作台）持有，这里只报坐标。
 */
export interface ArtifactLink {
  /** 这条消息的 id；流式中的那一轮换 artifacts.ts 的 LIVE_ARTIFACT。 */
  messageId: string;
  /** 预览区此刻打开的那一块 —— 对应的卡片要亮起来。 */
  active: ArtifactRef | null;
  onOpen: (ref: ArtifactRef) => void;
}

export const Markdown = memo(function Markdown({
  text,
  artifactLink,
}: {
  text: string;
  artifactLink?: ArtifactLink;
}) {
  const blocks = useMemo(() => parseBlocks(text), [text]);
  // 代码块的序号把不可预览的也算上：预览区靠 (messageId, codeIndex) 找回同一块，
  // 跳过一个不可预览的块，后面的序号就全对不上了。
  let codeCount = 0;
  return (
    <div className="md selectable">
      {blocks.map((b, i) => {
        const ctx: BlockCtx | undefined =
          b.kind === "code" ? { codeIndex: codeCount++, artifactLink } : undefined;
        return renderBlock(b, String(i), ctx);
      })}
    </div>
  );
});
