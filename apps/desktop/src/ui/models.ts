/**
 * 模型广场的聚合与筛选。纯函数，页面只负责把结果画出来。
 *
 * 目录说的是「此刻能接的 `{通道}/{模型}`」。用户来这一页要回答的问题是
 * 「这个模型我该用哪个名字调、它有哪些档位」。
 */
import type { LocalModel, Modality, ModelVendor } from "../ipc/models";

/** 主流厂商在前，Cursor 自家的 `auto` 压尾。 */
export const VENDOR_ORDER: ModelVendor[] = [
  "anthropic",
  "openai",
  "google",
  "xai",
  "aws",
  "kiro",
  "deepseek",
  "moonshot",
  "zhipu",
  "qoder",
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
  aws: "Amazon",
  kiro: "Amazon",
  qoder: "Qoder",
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

/** 友好格式化模型显示名（将 technical slug 转换为精致产品名）。 */
export function formatModelName(raw: string): string {
  if (!raw) return "未知模型";
  // 去除通道前缀，如 "chatgpt/gpt-4o" -> "gpt-4o"
  let clean = raw.includes("/") ? raw.split("/").pop()! : raw;
  // 去除前置通道修饰，如 "kiro-claude-sonnet-4.6" -> "claude-sonnet-4.6"
  clean = clean.replace(/^kiro[-_]/i, "");
  const lower = clean.toLowerCase();
  const normalized = lower.replace(/\./g, "-");
  const map: Record<string, string> = {
    "claude-opus-5-5": "Claude Opus 5.5",
    "claude-opus-5-5-thinking": "Claude Opus 5.5 (Thinking)",
    "claude-opus-5": "Claude Opus 5",
    "claude-opus-5-thinking": "Claude Opus 5 (Thinking)",
    "claude-opus-4-8": "Claude Opus 4.8",
    "claude-opus-4-8-thinking": "Claude Opus 4.8 (Thinking)",
    "claude-opus-4-7": "Claude Opus 4.7",
    "claude-opus-4-7-thinking": "Claude Opus 4.7 (Thinking)",
    "claude-opus-4-6": "Claude Opus 4.6",
    "claude-opus-4-6-thinking": "Claude Opus 4.6 (Thinking)",
    "claude-opus-4-5": "Claude Opus 4.5",
    "claude-opus-4-5-thinking": "Claude Opus 4.5 (Thinking)",
    "claude-sonnet-5": "Claude Sonnet 5",
    "claude-sonnet-5-thinking": "Claude Sonnet 5 (Thinking)",
    "claude-sonnet-4-6": "Claude Sonnet 4.6",
    "claude-sonnet-4-6-thinking": "Claude Sonnet 4.6 (Thinking)",
    "claude-sonnet-4-5": "Claude Sonnet 4.5",
    "claude-sonnet-4-5-thinking": "Claude Sonnet 4.5 (Thinking)",
    "claude-sonnet-4": "Claude Sonnet 4",
    "claude-haiku-4-5": "Claude Haiku 4.5",
    "claude-haiku-4-5-thinking": "Claude Haiku 4.5 (Thinking)",
    "claude-fable-5-1": "Claude Fable 5.1",
    "claude-fable-5": "Claude Fable 5",
    "claude-3-7-sonnet": "Claude 3.7 Sonnet",
    "claude-3-7-sonnet-thought": "Claude 3.7 Sonnet (Thinking)",
    "claude-3-5-sonnet": "Claude 3.5 Sonnet",
    "claude-3-5-sonnet-20241022": "Claude 3.5 Sonnet",
    "claude-3-5-sonnet-v2": "Claude 3.5 Sonnet v2",
    "claude-3-5-haiku": "Claude 3.5 Haiku",
    "claude-3-opus": "Claude 3 Opus",
    "claude-3-sonnet": "Claude 3 Sonnet",
    "claude-3-haiku": "Claude 3 Haiku",
    "gpt-6-astra": "GPT-6 Astra",
    "gpt-6-0-astra": "GPT-6 Astra",
    "gpt-6-sol": "GPT-6 Sol",
    "gpt-6-luna": "GPT-6 Luna",
    "gpt-6": "GPT-6",
    "gpt-5.6-sol": "GPT-5.6 Sol",
    "gpt-5-6-sol": "GPT-5.6 Sol",
    "gpt-5.6-sol-thinking": "GPT-5.6 Sol (Thinking)",
    "gpt-5-6-sol-thinking": "GPT-5.6 Sol (Thinking)",
    "gpt-5.6-terra": "GPT-5.6 Terra",
    "gpt-5.6-luna": "GPT-5.6 Luna",
    "gpt-5-6-luna": "GPT-5.6 Luna",
    "gpt-5.6": "GPT-5.6",
    "gpt-5.4": "GPT-5.4",
    "gpt-5.4-mini": "GPT-5.4 mini",
    "gpt-4o": "GPT-4o",
    "gpt-4o-mini": "GPT-4o mini",
    "gpt-4-turbo": "GPT-4 Turbo",
    "gpt-4": "GPT-4",
    "gpt-3.5-turbo": "GPT-3.5 Turbo",
    "gpt-oss-120b-medium": "GPT-OSS 120B (Medium)",
    "o1": "OpenAI o1",
    "o1-preview": "OpenAI o1-preview",
    "o1-mini": "OpenAI o1-mini",
    "o3-mini": "OpenAI o3-mini",
    "o3": "OpenAI o3",
    "deepseek-chat": "DeepSeek V3",
    "deepseek-reasoner": "DeepSeek R1",
    "deepseek-v3": "DeepSeek V3",
    "deepseek-r1": "DeepSeek R1",
    "gemini-4-argon-thinking": "Gemini 4 Argon (Thinking)",
    "gemini-4-argon": "Gemini 4 Argon",
    "gemini-3.8-flash-high": "Gemini 3.8 Flash (High)",
    "gemini-3.8-flash-medium": "Gemini 3.8 Flash (Medium)",
    "gemini-3.8-flash-low": "Gemini 3.8 Flash (Low)",
    "gemini-3.8-flash-tiered": "Gemini 3.8 Flash (Tiered)",
    "gemini-3.8-flash": "Gemini 3.8 Flash",
    "gemini-3.1-flash-lite": "Gemini 3.1 Flash Lite",
    "gemini-3.1-flash-image": "Gemini 3.1 Flash Image",
    "gemini-2.5-pro": "Gemini 2.5 Pro",
    "gemini-2.5-flash": "Gemini 2.5 Flash",
    "gemini-2.5-flash-lite": "Gemini 2.5 Flash Lite",
    "gemini-2.0-flash": "Gemini 2.0 Flash",
    "gemini-1.5-pro": "Gemini 1.5 Pro",
    "gemini-1.5-flash": "Gemini 1.5 Flash",
    "gpt-4.5": "GPT-4.5 Preview",
    "gpt-4.5-preview": "GPT-4.5 Preview",
    "chatgpt-4o-latest": "ChatGPT 4o (Latest)",
    "grok-4.7": "Grok 4.7",
    "grok-4.6": "Grok 4.6",
    "grok-4.5": "Grok 4.5",
    "grok-4": "Grok 4",
    "grok-4-fast": "Grok 4 Fast",
    "grok-code": "Grok Code",
    "grok-3": "Grok 3",
    "grok-3-mini": "Grok 3 mini",
    "grok-2": "Grok 2",
    "grok-2-1212": "Grok 2 (1212)",
    "grok-2-vision-1212": "Grok 2 Vision",
    "grok-beta": "Grok Beta",
    "grok-vision-beta": "Grok Vision Beta",
    "cursor-small": "Cursor Small",
    auto: "Cursor Auto",
  };

  if (map[lower]) return map[lower];
  if (map[normalized]) return map[normalized];

  // 规范化 fallback
  return clean
    .replace(/^gpt[ -]?/i, "GPT-")
    .replace(/^claude[ -]?/i, "Claude-")
    .replace(/^deepseek[ -]?/i, "DeepSeek-")
    .replace(/^gemini[ -]?/i, "Gemini-")
    .replace(/^grok[ -]?/i, "Grok-")
    .replace(/[-_]/g, " ")
    .replace(/\bgpt\b/gi, "GPT")
    .replace(/\bapi\b/gi, "API")
    .replace(/\b\w/g, (c) => c.toUpperCase());
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
 * 模型卡片在厂商内部的层级与版本权重打分（分数越高排在越前面）。
 * 旗舰推理 (Opus/o1/o3/R1) > 主力通用 (Sonnet/GPT-4o/V3) > 极速低延迟 (Haiku/Flash/mini)，
 * 同层级内版本号越新越高（5.5 > 5 > 4.8 > 4.6 > 4.5 > 4）。
 */
function modelSeriesScore(rawSeries: string): number {
  const s = rawSeries.toLowerCase();

  // 1. Cursor 自带 auto 压底
  if (s === "auto" || s.endsWith("/auto")) return -999999;

  let baseTier = 0;
  // ── Anthropic Claude ──
  if (s.includes("opus")) baseTier = 50000;
  else if (s.includes("fable") || s.includes("mythos")) baseTier = 45000;
  else if (s.includes("sonnet")) baseTier = 30000;
  else if (s.includes("haiku")) baseTier = 15000;

  // ── OpenAI ──
  else if (s.includes("gpt-6")) baseTier = 60000;
  else if (s.includes("gpt-5.6") || s.includes("gpt-5-6")) {
    baseTier = 55000;
    if (s.includes("sol")) baseTier += 300;
    else if (s.includes("terra")) baseTier += 200;
    else if (s.includes("luna")) baseTier += 100;
  }
  else if (s.includes("gpt-5")) baseTier = 50000;
  else if (s.includes("o3")) baseTier = 48000;
  else if (s.includes("o1")) baseTier = 45000;
  else if (s.includes("gpt-4.5")) baseTier = 40000;
  else if (s.includes("gpt-4o") && !s.includes("mini")) baseTier = 35000;
  else if (s.includes("gpt-4o-mini") || s.includes("gpt-4-turbo")) baseTier = 25000;
  else if (s.includes("gpt-4")) baseTier = 20000;
  else if (s.includes("gpt-3.5")) baseTier = 10000;

  // ── Google Gemini ──
  else if (s.includes("gemini-4")) baseTier = 40000;
  else if (s.includes("gemini-3")) baseTier = 35000;
  else if (s.includes("gemini-2.5") || s.includes("gemini-2-5")) baseTier = 30000;
  else if (s.includes("gemini-2")) baseTier = 25000;
  else if (s.includes("gemini-1.5")) baseTier = 20000;

  // ── xAI Grok ──
  else if (s.includes("grok-4.7") || s.includes("grok-4-7")) baseTier = 47000;
  else if (s.includes("grok-4.6") || s.includes("grok-4-6")) baseTier = 46000;
  else if (s.includes("grok-4.5") || s.includes("grok-4-5")) baseTier = 45000;
  else if (s.includes("grok-4") || s.includes("grok-code")) baseTier = 40000;
  else if (s.includes("grok-3")) baseTier = 30000;
  else if (s.includes("grok-2")) baseTier = 20000;

  // ── DeepSeek ──
  else if (s.includes("r1") || s.includes("reasoner")) baseTier = 40000;
  else if (s.includes("v3") || s.includes("chat")) baseTier = 30000;

  // 提取主版本数字进行加权 (例如 5.5 -> 5500, 4.8 -> 4800, 4.6 -> 4600)
  const match = s.match(/(\d+)(?:[.-](\d+))?/);
  let verScore = 0;
  if (match) {
    const major = parseInt(match[1]!, 10);
    const minor = match[2] ? parseInt(match[2]!, 10) : 0;
    verScore = major * 1000 + minor * 100;
  }

  return baseTier + verScore;
}

/**
 * 厂商优先，模态其次（对话和出图混排会得到一个纯属巧合的顺序）；
 * 厂商内部按模型梯队与版本倒序（旗舰推理优先，新版本优先）；
 * 权重相同时看档位数（档位齐的优先），最后按名称倒序。
 */
function sortGroups(groups: ModelCardGroup[]): ModelCardGroup[] {
  return groups.sort(
    (a, b) =>
      vendorRank(a.vendor) - vendorRank(b.vendor) ||
      MODALITY_ORDER.indexOf(a.modality) - MODALITY_ORDER.indexOf(b.modality) ||
      modelSeriesScore(b.series) - modelSeriesScore(a.series) ||
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

/** 类别清单：反映当前通道下各模态的模型数，不随子级厂商下拉变动而跳变。 */
export function modalityRail(groups: ModelCardGroup[], f: CardFilter): Array<{ id: Modality; n: number }> {
  const hit = groups.filter((g) => cardMatches(g, f.query));
  return MODALITY_ORDER.filter((k) => hit.some((g) => g.modality === k)).map((k) => ({ id: k, n: hit.filter((g) => g.modality === k).length }));
}
