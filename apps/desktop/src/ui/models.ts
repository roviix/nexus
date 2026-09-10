/**
 * 模型广场的聚合与筛选。纯函数，页面只负责把结果画出来。
 *
 * 目录说的是「网关能把哪些名字映射到哪个上游模型」。用户来这一页要回答的问题是
 * 「这个模型我该用哪个名字调、它有哪些档位」。
 */
import type { LocalModel, Modality, ModelVendor } from "../ipc/models";

/** 主流厂商在前，Cursor 自家的 `auto` 压尾。 */
export const VENDOR_ORDER: ModelVendor[] = [
  "anthropic",
  "openai",
  "google",
  "xai",
  "deepseek",
  "moonshot",
  "zhipu",
  "alibaba",
  "bytedance",
  "minimax",
  "other",
  "cursor",
];

export const VENDOR_LABEL: Record<string, string> = {
  anthropic: "Anthropic",
  openai: "OpenAI",
  google: "Google",
  xai: "xAI",
  cursor: "Cursor",
  moonshot: "Moonshot",
  zhipu: "智谱",
  bytedance: "字节",
  alibaba: "阿里",
  deepseek: "DeepSeek",
  minimax: "MiniMax",
  other: "其他",
};

export function vendorLabel(v: ModelVendor, fallback?: string): string {
  return VENDOR_LABEL[v] ?? fallback ?? v;
}

function vendorRank(v: ModelVendor): number {
  const i = VENDOR_ORDER.indexOf(v);
  return i === -1 ? VENDOR_ORDER.length : i;
}

/** 档位排序：standard 最前，其余按名字长度——短的一般是「少一个修饰」的那档。 */
function variantRank(v: string): number {
  return v === "standard" ? -1 : v.length;
}

// ── 广场卡片 ──────────────────────────────────────────────────────────────────

/** 一张卡里的一个档位。别名只用于搜索，不进入卡片展示。 */
export interface CardVariant {
  id: string;
  variant: string;
  aliases: string[];
  note?: string;
}

/**
 * 一张卡。claude-opus-5 和它的 -thinking-max / -thinking-max-fast 是同一个模型的推理档位，
 * 摊成三张就是把同一组信息抄三遍。
 */
export interface ModelCardGroup {
  key: string;
  series: string;
  /** 只有一个档位时 series 可能是个调不通的词根，标题直接用真实 id。 */
  title: string;
  vendor: ModelVendor;
  vendorLabel: string;
  modality: Modality;
  note?: string;
  variants: CardVariant[];
}

const MODALITY_ORDER: Modality[] = ["chat", "image", "video"];

/**
 * 目录 → 卡。对话模型按系列合并；出图 / 视频模型没有「档位」这回事，一个 id 一张卡。
 * 别名只保留给搜索。老网关的目录没有 `modality`，缺省按对话算。
 */
export function groupLocal(models: LocalModel[]): ModelCardGroup[] {
  const map = new Map<string, ModelCardGroup>();
  for (const m of models) {
    const modality: Modality = m.modality ?? "chat";
    const key = modality === "chat" ? `${m.vendor}|${m.series}` : `${modality}|${m.id}`;
    const v: CardVariant = { id: m.id, variant: m.variant || "standard", aliases: [...m.aliases], note: m.note ?? undefined };
    const g = map.get(key);
    if (!g) {
      map.set(key, { key, series: m.series, title: m.id, vendor: m.vendor, vendorLabel: vendorLabel(m.vendor, m.vendorLabel), modality, note: m.note ?? undefined, variants: [v] });
      continue;
    }
    g.variants.push(v);
    g.note = g.note || m.note || undefined;
  }
  for (const g of map.values()) {
    g.variants.sort((a, b) => variantRank(a.variant) - variantRank(b.variant) || a.id.length - b.id.length);
    g.title = g.variants.length === 1 ? g.variants[0]!.id : g.series;
  }
  return sortGroups([...map.values()]);
}

/**
 * 厂商优先，模态其次（对话和出图混排会得到一个纯属巧合的顺序），再看档位数：
 * 档位齐的通常就是那家的主力。最后按名称倒序，让 4.6 排在 4.5 前面。
 */
function sortGroups(groups: ModelCardGroup[]): ModelCardGroup[] {
  return groups.sort(
    (a, b) =>
      vendorRank(a.vendor) - vendorRank(b.vendor) ||
      MODALITY_ORDER.indexOf(a.modality) - MODALITY_ORDER.indexOf(b.modality) ||
      b.variants.length - a.variants.length ||
      b.series.localeCompare(a.series),
  );
}

export interface CardFilter {
  query: string;
  vendor: ModelVendor | "all";
  modality: Modality | "all";
}

/** 搜索匹配 id、系列、厂商名和**别名** —— 用户手里拿着的往往是 Claude Code 里看到的那个名字。 */
export function cardMatches(g: ModelCardGroup, q: string): boolean {
  const kw = q.trim().toLowerCase();
  if (!kw) return true;
  if (g.series.toLowerCase().includes(kw) || g.vendorLabel.toLowerCase().includes(kw)) return true;
  return g.variants.some((v) => v.id.toLowerCase().includes(kw) || v.aliases.some((a) => a.toLowerCase().includes(kw)));
}

export function filterCards(groups: ModelCardGroup[], f: CardFilter): ModelCardGroup[] {
  return groups.filter((g) => (f.vendor === "all" || g.vendor === f.vendor) && (f.modality === "all" || g.modality === f.modality) && cardMatches(g, f.query));
}

/**
 * 右栏的厂商清单，带各家的卡片数。计数走搜索和类别之后、厂商之前的集合：
 * 把厂商也算进去，选中 Anthropic 后其余各家就全成了 0，那份清单也就没法再当导航用了。
 */
export function vendorRail(groups: ModelCardGroup[], f: CardFilter): Array<{ vendor: ModelVendor; label: string; n: number }> {
  const hit = groups.filter((g) => (f.modality === "all" || g.modality === f.modality) && cardMatches(g, f.query));
  return VENDOR_ORDER.filter((v) => hit.some((g) => g.vendor === v)).map((v) => ({
    vendor: v,
    label: hit.find((g) => g.vendor === v)!.vendorLabel,
    n: hit.filter((g) => g.vendor === v).length,
  }));
}

/** 类别清单，口径与厂商那份对称：算上搜索和厂商，唯独不算自己。 */
export function modalityRail(groups: ModelCardGroup[], f: CardFilter): Array<{ id: Modality; n: number }> {
  const hit = groups.filter((g) => (f.vendor === "all" || g.vendor === f.vendor) && cardMatches(g, f.query));
  return MODALITY_ORDER.filter((k) => hit.some((g) => g.modality === k)).map((k) => ({ id: k, n: hit.filter((g) => g.modality === k).length }));
}
