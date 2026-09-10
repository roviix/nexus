/**
 * 输入框能带上去的两种东西，以及收不收它们的规矩。纯逻辑，不碰 DOM，好测。
 *
 * **注意：此刻界面上只开了图片这一条。** 文本文件那条入口撤掉了（`Composer` 的
 * `AttachButton` 只开图片选择器，拖放和粘贴也在 `addFiles` 里挡掉非图片），
 * 所以 `TextAttachment` / `inlineText` / `withInlinedText` 这一支现在没有调用方。
 * 留着是因为这套规矩本身没错、也有测试兜着，等真要支持文件时直接把入口接回来即可。
 *
 * **图片**真的发上去：字节 base64 交给 Rust，落盘、挂在 user 消息上，之后每一轮都作为
 * `image_url` 内联进历史。**文本文件**不发文件——它在发送前就被内联进提示词，变成一段
 * 围栏代码块。对上游而言那就是一句普通的话，任何模型都读得懂，不必挑多模态的。
 *
 * 上限有三道：单张图 8MB（再大多半是原图，不是要给模型看的东西）、单个文本 200KB
 * （超过它内联进提示词只会把上下文撑爆）、整条消息 20MB（Rust 那边也拦一道）。
 */
import type { ChatAttachment } from "../../ipc/playground";
import { fmtBytes } from "./target";

export interface ImageAttachment {
  kind: "image";
  id: string;
  name: string;
  /** 浏览器报的 MIME。Rust 落盘时会按字节再认一遍，以那次为准。 */
  mime: string;
  /** 不带 `data:` 前缀的载荷。 */
  dataBase64: string;
  bytes: number;
  /** 缩略图地址（object URL）。发出去之后归这一路 run 管，run 收掉时释放。 */
  url: string;
}

export interface TextAttachment {
  kind: "text";
  id: string;
  name: string;
  /** 小写后缀，给围栏当语言标注。 */
  ext: string;
  text: string;
  bytes: number;
}

export type Attachment = ImageAttachment | TextAttachment;

/** 只有名字、类型、大小 —— 校验不需要文件本身。 */
export interface FileLike {
  name: string;
  type: string;
  size: number;
}

export const MAX_IMAGES = 4;
export const MAX_IMAGE_BYTES = 8 * 1024 * 1024;
export const MAX_TEXT_BYTES = 200 * 1024;
export const MAX_TOTAL_BYTES = 20 * 1024 * 1024;

const IMAGE_MIMES = ["image/png", "image/jpeg", "image/webp", "image/gif"];
const IMAGE_EXTS = ["png", "jpg", "jpeg", "webp", "gif"];

/** 文本与常见代码后缀。认后缀不认 MIME：系统给 `.rs` / `.toml` 报的多半是空串。 */
const TEXT_EXTS = [
  "txt", "md", "markdown", "json", "jsonl", "csv", "tsv", "yaml", "yml", "toml", "ini", "conf", "env", "log", "diff", "patch",
  "ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "svelte", "css", "scss", "html", "xml", "svg",
  "rs", "go", "py", "rb", "php", "java", "kt", "swift", "c", "h", "cc", "cpp", "hpp", "cs", "m", "mm", "lua", "r", "scala", "dart",
  "sh", "bash", "zsh", "fish", "sql", "graphql", "proto", "dockerfile", "makefile", "gradle",
];

/** `<input type="file">` 的 accept。列后缀也列 MIME：两种系统的选择器各认一种。 */
export const IMAGE_ACCEPT = [...IMAGE_EXTS.map((e) => `.${e}`), ...IMAGE_MIMES].join(",");
export const TEXT_ACCEPT = [...TEXT_EXTS.map((e) => `.${e}`), "text/plain"].join(",");

let seq = 0;

export function newAttachmentId(): string {
  seq += 1;
  return `att-${Date.now().toString(36)}-${seq.toString(36)}`;
}

/** 文件名的后缀，小写、不含点。没有后缀就是空串。 */
export function extOf(name: string): string {
  const base = name.split(/[\\/]/).pop() ?? "";
  const i = base.lastIndexOf(".");
  return i > 0 ? base.slice(i + 1).toLowerCase() : "";
}

/** 这个文件按哪种附件收；两种都不是就是 null。 */
export function classify(f: FileLike): Attachment["kind"] | null {
  const ext = extOf(f.name);
  if (IMAGE_MIMES.includes(f.type) || IMAGE_EXTS.includes(ext)) return "image";
  if (TEXT_EXTS.includes(ext) || TEXT_EXTS.includes(f.name.toLowerCase())) return "text";
  // 没有后缀但系统说是文本（README 一类）也收。
  if (f.type.startsWith("text/") || f.type === "application/json") return "text";
  return null;
}

export function isImage(a: Attachment): a is ImageAttachment {
  return a.kind === "image";
}

export function isText(a: Attachment): a is TextAttachment {
  return a.kind === "text";
}

/** 附件加起来占多少。 */
export function totalBytes(list: Attachment[]): number {
  return list.reduce((n, a) => n + a.bytes, 0);
}

/**
 * 收不收这个文件。不收就回一句话——说清是哪个文件、超了什么，用户才知道下一步怎么改。
 * `current` 是已经挂着的那些：张数与总量都得算上它们。
 */
export function reject(f: FileLike, current: Attachment[]): string | null {
  const kind = classify(f);
  if (!kind) return `「${f.name}」这种文件带不了。图片支持 png / jpg / webp / gif，文本支持 txt / md / json / csv 与常见代码文件。`;
  if (kind === "image") {
    if (current.filter(isImage).length >= MAX_IMAGES) return `一条消息最多带 ${MAX_IMAGES} 张图。`;
    if (f.size > MAX_IMAGE_BYTES) return `「${f.name}」有 ${fmtBytes(f.size)}，单张图不能超过 ${fmtBytes(MAX_IMAGE_BYTES)}。`;
  } else if (f.size > MAX_TEXT_BYTES) {
    return `「${f.name}」有 ${fmtBytes(f.size)}，文本文件不能超过 ${fmtBytes(MAX_TEXT_BYTES)}。`;
  }
  if (totalBytes(current) + f.size > MAX_TOTAL_BYTES) return `附件加起来不能超过 ${fmtBytes(MAX_TOTAL_BYTES)}。`;
  return null;
}

/** 剪贴板里的图常常没有名字。 */
export function nameOf(f: FileLike, at = new Date()): string {
  if (f.name.trim()) return f.name;
  const ext = f.type.split("/")[1]?.replace("jpeg", "jpg") || "png";
  const stamp = `${at.getHours()}`.padStart(2, "0") + `${at.getMinutes()}`.padStart(2, "0") + `${at.getSeconds()}`.padStart(2, "0");
  return `粘贴的图片-${stamp}.${ext}`;
}

/** `FileReader.readAsDataURL` 的结果 → 纯 base64 载荷。 */
export function payloadOf(dataUrl: string): string {
  const i = dataUrl.indexOf(",");
  return i >= 0 ? dataUrl.slice(i + 1) : dataUrl;
}

/** 围栏要比正文里最长的一串反引号还长，否则文件里有代码块就会把围栏截断。 */
function fenceFor(text: string): string {
  const longest = (text.match(/`+/g) ?? []).reduce((n, s) => Math.max(n, s.length), 0);
  return "`".repeat(Math.max(3, longest + 1));
}

/** 一个文本文件内联成的那一段：围栏上标语言与文件名，模型才知道这是「一份文件」。 */
export function inlineText(a: TextAttachment): string {
  const fence = fenceFor(a.text);
  return `${fence}${a.ext} ${a.name}\n${a.text.replace(/\s+$/, "")}\n${fence}`;
}

/** 发送前把文本附件拼进提示词。图片不在这里——它们走 IPC 的 attachments。 */
export function withInlinedText(prompt: string, list: Attachment[]): string {
  const files = list.filter(isText);
  if (!files.length) return prompt.trim();
  return [prompt.trim(), ...files.map(inlineText)].filter(Boolean).join("\n\n");
}

/** 交给 IPC 的形状。 */
export function toChatAttachments(list: Attachment[]): ChatAttachment[] {
  return list.filter(isImage).map((a) => ({ name: a.name, mime: a.mime, dataBase64: a.dataBase64 }));
}
