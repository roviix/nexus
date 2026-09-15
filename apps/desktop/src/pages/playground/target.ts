/**
 * 游乐场的纯函数：某种会话能选哪些模型、默认落到哪个、规格档位、几个格式化。
 * 不碰 IPC，好测。
 */
import type { LocalModel } from "../../ipc/models";
import type { Kind, ThreadSummary } from "../../ipc/playground";

/** 一次发送的目标：模型。会话记着它，草稿也记着它。 */
export interface Target {
  model: string;
}

export interface Catalogs {
  local: LocalModel[] | null;
}

/**
 * 会话种类 → 能选的模型 id。
 * 按目录里的 modality 分；没标的条目按对话算（老网关的目录没有这个字段）。
 */
export function modelIds(cat: Catalogs, kind: Kind): string[] {
  return (cat.local ?? []).filter((m) => (m.modality ?? "chat") === kind).map((m) => m.id);
}

/** 目录里某个模型的种类；不认识的按对话算。 */
export function kindOfModel(cat: Catalogs, model: string): Kind {
  const modality = (cat.local ?? []).find((m) => m.id === model)?.modality;
  return modality === "image" ? "image" : modality === "video" ? "video" : "chat";
}

/** 本地网关的常用默认。目录主键是 `{通道}/{模型}`，也认老目录里的裸名 `auto`。 */
function localAutoId(ids: string[]): string | undefined {
  return ids.find((id) => id === "auto" || id.endsWith("/auto"));
}

/**
 * 新建会话时的目标：先看这一类最近一个会话用什么（用户上次的选择最可信），
 * 再落到目录里的默认（对话是 `{通道}/auto`，其它取第一个）。
 */
export function defaultTarget(kind: Kind, recent: ThreadSummary[], cat: Catalogs, hint?: { model?: string }): Target {
  if (hint?.model) return { model: hint.model };

  const last = recent.find((t) => t.kind === kind);
  if (last) return { model: last.model };

  const ids = modelIds(cat, kind);
  return { model: localAutoId(ids) ?? ids[0] ?? "" };
}

/** 目录刷新后把模型落回目录里：原来的模型不在就取默认。 */
export function retarget(cur: Target, kind: Kind, cat: Catalogs): Target {
  const ids = modelIds(cat, kind);
  if (ids.length === 0 || ids.includes(cur.model)) return cur;
  return { model: localAutoId(ids) ?? ids[0]! };
}

/** 没有别的依据时的初始规格，也是兜底那一排的第一档。 */
export const DEFAULT_IMAGE_SIZE = "1024x1024";

/**
 * 目录没说这个模型认哪些规格时摆的那一排。
 *
 * 是**兜底**不是白名单：能不能过由上游说了算，我们只是给个覆盖常见画幅的起点。
 */
export const DEFAULT_IMAGE_SIZES = [DEFAULT_IMAGE_SIZE, "1024x1536", "1536x1024", "2048x2048", "2304x1728", "1728x2304"];

/** 对话会话没有规格可选。共用一个常量，免得每次渲染都造一个新数组去触发下游的 effect。 */
const NO_SIZES: string[] = [];

/** Cursor 那条生图链路固定出这么大。协议里没有尺寸字段，写进 prompt 也不听。 */
export const CURSOR_IMAGE_SIZE = "1536x1024";

/**
 * 这个模型能选哪些规格。出图会话摆兜底的一排，对话没有。
 *
 * 不在这里按模型名猜：认不认某个尺寸取决于上游那一版怎么实现，不是名字能看出来的。
 */
export function sizeOptionsOf(kind: Kind): string[] {
  return kind === "image" ? DEFAULT_IMAGE_SIZES : NO_SIZES;
}

/**
 * `2304x1728` → `{ alias: "2K", ratio: "4:3" }`。芯片上写档位和画幅，像素留给 title。
 *
 * 算出来而不是跟着尺寸表写死：写死的标签配不上没预料到的尺寸。
 * 档位取短边（1K/2K/4K 说的是基准分辨率），画幅按最大公约数约分。
 */
export function sizeChip(size: string): { alias: string; ratio: string } {
  const m = /^(\d+)\s*[x×]\s*(\d+)$/i.exec(size);
  if (!m) return { alias: "", ratio: size };
  const w = Number(m[1]);
  const h = Number(m[2]);
  const g = gcd(w, h);
  return { alias: `${Math.max(1, Math.round(Math.min(w, h) / 1024))}K`, ratio: `${w / g}:${h / g}` };
}

function gcd(a: number, b: number): number {
  return b === 0 ? a : gcd(b, a % b);
}

/**
 * 这个目标出图固定多大；null = 认 `size`，该摆规格菜单。
 *
 * 事实由目录带下来（`LocalModel.fixedSize`），不在这里按模型名猜——名字看不出它派到哪个
 * 平台，猜错的后果是界面对参数支持情况说谎。目录没这个字段的老网关只有 Cursor 一条出图路，
 * 按固定算。
 */
export function fixedSizeOf(kind: Kind, target: Target, cat: Catalogs): string | null {
  if (kind !== "image") return null;
  const entry = (cat.local ?? []).find((m) => m.id === target.model);
  if (entry && "fixedSize" in entry) return entry.fixedSize ?? null;
  return CURSOR_IMAGE_SIZE;
}

/** `1536x1024` → `1536 × 1024`。数字之间要有呼吸，`x` 也不是乘号。 */
export function fmtSize(size: string): string {
  return size.replace(/\s*[x×]\s*/i, " × ");
}

/** `1024x1536` → 宽高比（CSS `aspect-ratio` 用）。认不出按 1。 */
export function aspectOf(size: string | null | undefined): number {
  const m = /^(\d+)\s*[x×]\s*(\d+)$/i.exec(size ?? "");
  if (!m) return 1;
  const w = Number(m[1]);
  const h = Number(m[2]);
  return w > 0 && h > 0 ? w / h : 1;
}

/** 时长：1 秒以下用 ms，否则一位小数的秒。试用比的是手感，别全挤成 0.0s。 */
export function fmtLatency(ms: number | null | undefined): string {
  if (ms == null || !Number.isFinite(ms)) return "—";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

export function fmtBytes(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return "—";
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) {
    const kb = n / 1024;
    return `${kb < 10 ? kb.toFixed(1) : Math.round(kb)} KB`;
  }
  const mb = n / (1024 * 1024);
  return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`;
}

/** 输出吞吐（tok/s）：按首字之后到收尾算；样本太短不报。 */
export function tokensPerSecond(completionTokens: number | null | undefined, durationMs: number | null | undefined, ttftMs: number | null | undefined): number | null {
  if (!completionTokens || durationMs == null) return null;
  const gen = durationMs - (ttftMs ?? 0);
  if (gen < 50) return null;
  return Math.round((completionTokens / (gen / 1000)) * 10) / 10;
}

/** 会话标题的展示：还没标题时按种类给一个占位。 */
export function threadTitle(title: string, kind: Kind): string {
  return title.trim() || (kind === "chat" ? "新对话" : kind === "video" ? "新视频" : "新图片");
}

/** 列表第二行：把 Markdown 的记号剥掉，只留字。一行灰字里的 `**` 和 `` ` `` 只是噪音。 */
export function plainPreview(text: string): string {
  return text
    .replace(/```[^\n]*/g, "")
    .replace(/[*_`~>#]+/g, "")
    .replace(/!?\[([^\]]*)\]\([^)]*\)/g, "$1")
    .replace(/\s+/g, " ")
    .trim();
}
