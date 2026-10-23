/**
 * 全量官方模型定价库与解析器。
 *
 * 维护主流及前沿模型的最新官方定价（$/1M tokens，含 Input、Output、Cache Read、Cache Write；生图模型按张计费）。
 * 用于模型广场展示及后续 Mesh 算力出借共享时收益与成本的精细核算。
 */
import catalogData from "./pricing-catalog.json";

export type BillingMode = "token" | "per_request" | "image" | "video";

export interface PriceCard {
  id: string;
  mode: BillingMode;
  inputUsdPerMtok?: number;
  outputUsdPerMtok?: number;
  cacheReadUsdPerMtok?: number;
  cacheWriteUsdPerMtok?: number;
  usdPerImage?: Record<string, number>;
  usdPerSecond?: Record<string, number>;
  usdPerRequest?: number;
}

export const FALLBACK_PRICE: PriceCard = {
  id: "*",
  mode: "token",
  inputUsdPerMtok: 1.25,
  outputUsdPerMtok: 10,
  cacheReadUsdPerMtok: 0.3,
  cacheWriteUsdPerMtok: 1.25,
};

/**
 * 官方基准模型核心定价（$/1M tokens）。
 * 优先匹配最长前缀，覆盖常见的主力旗舰、推理模型及特殊通道别名。
 *
 * Anthropic 官方计费规则：
 * - Cache Write = 1.25 × Input (创建缓存费率)
 * - Cache Read  = 0.1 × Input (命中期读缓存费率)
 *
 * OpenAI 官方计费规则：
 * - Cache Write = 1.0 × Input (未命中 / 首次写入无额外加价)
 * - Cache Read  = 0.5 × Input (缓存命中有 50% 折扣)
 *
 * DeepSeek 官方计费规则：
 * - V3: Input $0.14, Output $0.28, Cache Read $0.014, Cache Write $0.14
 * - R1: Input $0.55, Output $2.19, Cache Read $0.14, Cache Write $0.55
 */
export const CURATED_PRICES: PriceCard[] = [
  // ── Anthropic Claude ──
  { id: "claude-opus-5-5", mode: "token", inputUsdPerMtok: 15, outputUsdPerMtok: 75, cacheReadUsdPerMtok: 1.5, cacheWriteUsdPerMtok: 18.75 },
  { id: "claude-opus-5", mode: "token", inputUsdPerMtok: 15, outputUsdPerMtok: 75, cacheReadUsdPerMtok: 1.5, cacheWriteUsdPerMtok: 18.75 },
  { id: "claude-fable-5-1", mode: "token", inputUsdPerMtok: 15, outputUsdPerMtok: 75, cacheReadUsdPerMtok: 1.5, cacheWriteUsdPerMtok: 18.75 },
  { id: "claude-fable-5", mode: "token", inputUsdPerMtok: 15, outputUsdPerMtok: 75, cacheReadUsdPerMtok: 1.5, cacheWriteUsdPerMtok: 18.75 },
  { id: "claude-sonnet-5", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.3, cacheWriteUsdPerMtok: 3.75 },
  { id: "claude-sonnet-4-6", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.3, cacheWriteUsdPerMtok: 3.75 },
  { id: "claude-haiku-4-5", mode: "token", inputUsdPerMtok: 0.8, outputUsdPerMtok: 4, cacheReadUsdPerMtok: 0.08, cacheWriteUsdPerMtok: 1.0 },
  { id: "claude-3-7-sonnet", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.3, cacheWriteUsdPerMtok: 3.75 },
  { id: "claude-3.7-sonnet", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.3, cacheWriteUsdPerMtok: 3.75 },
  { id: "claude-3-5-sonnet", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.3, cacheWriteUsdPerMtok: 3.75 },
  { id: "claude-3.5-sonnet", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.3, cacheWriteUsdPerMtok: 3.75 },
  { id: "claude-4.6-sonnet", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.3, cacheWriteUsdPerMtok: 3.75 },
  { id: "claude-4.5-sonnet", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.3, cacheWriteUsdPerMtok: 3.75 },
  { id: "claude-4-sonnet", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.3, cacheWriteUsdPerMtok: 3.75 },
  { id: "claude-3-5-haiku", mode: "token", inputUsdPerMtok: 0.8, outputUsdPerMtok: 4, cacheReadUsdPerMtok: 0.08, cacheWriteUsdPerMtok: 1.0 },
  { id: "claude-3.5-haiku", mode: "token", inputUsdPerMtok: 0.8, outputUsdPerMtok: 4, cacheReadUsdPerMtok: 0.08, cacheWriteUsdPerMtok: 1.0 },
  { id: "claude-3-haiku", mode: "token", inputUsdPerMtok: 0.25, outputUsdPerMtok: 1.25, cacheReadUsdPerMtok: 0.025, cacheWriteUsdPerMtok: 0.3125 },
  { id: "claude-3-opus", mode: "token", inputUsdPerMtok: 15, outputUsdPerMtok: 75, cacheReadUsdPerMtok: 1.5, cacheWriteUsdPerMtok: 18.75 },
  { id: "claude-4.6-opus", mode: "token", inputUsdPerMtok: 15, outputUsdPerMtok: 75, cacheReadUsdPerMtok: 1.5, cacheWriteUsdPerMtok: 18.75 },
  { id: "claude-4.5-opus", mode: "token", inputUsdPerMtok: 15, outputUsdPerMtok: 75, cacheReadUsdPerMtok: 1.5, cacheWriteUsdPerMtok: 18.75 },

  // ── OpenAI ──
  { id: "gpt-6-astra", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-6-sol", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-6-luna", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-6", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-5.6-sol", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-5.6-terra", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-5.6-luna", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-5.6", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-5.4", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-5.4-mini", mode: "token", inputUsdPerMtok: 0.25, outputUsdPerMtok: 1.5, cacheReadUsdPerMtok: 0.125, cacheWriteUsdPerMtok: 0.25 },
  { id: "gpt-4o-mini", mode: "token", inputUsdPerMtok: 0.15, outputUsdPerMtok: 0.6, cacheReadUsdPerMtok: 0.075, cacheWriteUsdPerMtok: 0.15 },
  { id: "gpt-4o", mode: "token", inputUsdPerMtok: 2.5, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 1.25, cacheWriteUsdPerMtok: 2.5 },
  { id: "gpt-4.1-mini", mode: "token", inputUsdPerMtok: 0.15, outputUsdPerMtok: 0.6, cacheReadUsdPerMtok: 0.075, cacheWriteUsdPerMtok: 0.15 },
  { id: "gpt-4.1", mode: "token", inputUsdPerMtok: 2.5, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 1.25, cacheWriteUsdPerMtok: 2.5 },
  { id: "gpt-5-codex-mini", mode: "token", inputUsdPerMtok: 0.25, outputUsdPerMtok: 1.5, cacheReadUsdPerMtok: 0.125, cacheWriteUsdPerMtok: 0.25 },
  { id: "gpt-5-codex", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-5-mini", mode: "token", inputUsdPerMtok: 0.25, outputUsdPerMtok: 1.5, cacheReadUsdPerMtok: 0.125, cacheWriteUsdPerMtok: 0.25 },
  { id: "gpt-5.5", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-5.3", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-5.2", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-5.1", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "gpt-5", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.625, cacheWriteUsdPerMtok: 1.25 },
  { id: "o1-mini", mode: "token", inputUsdPerMtok: 1.1, outputUsdPerMtok: 4.4, cacheReadUsdPerMtok: 0.55, cacheWriteUsdPerMtok: 1.1 },
  { id: "o1", mode: "token", inputUsdPerMtok: 15, outputUsdPerMtok: 60, cacheReadUsdPerMtok: 7.5, cacheWriteUsdPerMtok: 15 },
  { id: "o3-mini", mode: "token", inputUsdPerMtok: 1.1, outputUsdPerMtok: 4.4, cacheReadUsdPerMtok: 0.55, cacheWriteUsdPerMtok: 1.1 },
  { id: "o3", mode: "token", inputUsdPerMtok: 15, outputUsdPerMtok: 60, cacheReadUsdPerMtok: 7.5, cacheWriteUsdPerMtok: 15 },
  { id: "gpt-oss-120b-medium", mode: "token", inputUsdPerMtok: 0.3, outputUsdPerMtok: 1.2, cacheReadUsdPerMtok: 0.075, cacheWriteUsdPerMtok: 0.3 },
  { id: "chatgpt-4o", mode: "token", inputUsdPerMtok: 2.5, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 1.25, cacheWriteUsdPerMtok: 2.5 },

  // ── Google Gemini & Antigravity ──
  { id: "gemini-4-argon-thinking", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 5, cacheReadUsdPerMtok: 0.3125, cacheWriteUsdPerMtok: 1.25 },
  { id: "gemini-4-argon", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 5, cacheReadUsdPerMtok: 0.3125, cacheWriteUsdPerMtok: 1.25 },
  { id: "gemini-3.8-flash-high", mode: "token", inputUsdPerMtok: 0.15, outputUsdPerMtok: 0.6, cacheReadUsdPerMtok: 0.0375, cacheWriteUsdPerMtok: 0.15 },
  { id: "gemini-3.8-flash-medium", mode: "token", inputUsdPerMtok: 0.15, outputUsdPerMtok: 0.6, cacheReadUsdPerMtok: 0.0375, cacheWriteUsdPerMtok: 0.15 },
  { id: "gemini-3.8-flash-low", mode: "token", inputUsdPerMtok: 0.15, outputUsdPerMtok: 0.6, cacheReadUsdPerMtok: 0.0375, cacheWriteUsdPerMtok: 0.15 },
  { id: "gemini-3.8-flash-tiered", mode: "token", inputUsdPerMtok: 0.15, outputUsdPerMtok: 0.6, cacheReadUsdPerMtok: 0.0375, cacheWriteUsdPerMtok: 0.15 },
  { id: "gemini-3.8-flash", mode: "token", inputUsdPerMtok: 0.15, outputUsdPerMtok: 0.6, cacheReadUsdPerMtok: 0.0375, cacheWriteUsdPerMtok: 0.15 },
  { id: "gemini-3.1-flash-lite", mode: "token", inputUsdPerMtok: 0.075, outputUsdPerMtok: 0.3, cacheReadUsdPerMtok: 0.01875, cacheWriteUsdPerMtok: 0.075 },
  { id: "gemini-3.1-flash-image", mode: "image", usdPerImage: { default: 0.03 } },
  { id: "gemini-2.5-flash", mode: "token", inputUsdPerMtok: 0.1, outputUsdPerMtok: 0.4, cacheReadUsdPerMtok: 0.025, cacheWriteUsdPerMtok: 0.1 },
  { id: "gemini-2.5-pro", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 5, cacheReadUsdPerMtok: 0.3125, cacheWriteUsdPerMtok: 1.25 },
  { id: "gemini-2.0-flash", mode: "token", inputUsdPerMtok: 0.1, outputUsdPerMtok: 0.4, cacheReadUsdPerMtok: 0.025, cacheWriteUsdPerMtok: 0.1 },
  { id: "gemini-3.7-flash", mode: "token", inputUsdPerMtok: 0.1, outputUsdPerMtok: 0.4, cacheReadUsdPerMtok: 0.025, cacheWriteUsdPerMtok: 0.1 },
  { id: "gemini-3", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.3125, cacheWriteUsdPerMtok: 1.25 },
  { id: "gemini-1.5-pro", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 5, cacheReadUsdPerMtok: 0.3125, cacheWriteUsdPerMtok: 1.25 },
  { id: "gemini-1.5-flash", mode: "token", inputUsdPerMtok: 0.075, outputUsdPerMtok: 0.3, cacheReadUsdPerMtok: 0.01875, cacheWriteUsdPerMtok: 0.075 },

  // ── xAI Grok ──
  { id: "grok-4.7", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.75, cacheWriteUsdPerMtok: 3.0 },
  { id: "grok-4.6", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.75, cacheWriteUsdPerMtok: 3.0 },
  { id: "grok-4.5", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.75, cacheWriteUsdPerMtok: 3.0 },
  { id: "grok-4-fast", mode: "token", inputUsdPerMtok: 1, outputUsdPerMtok: 5, cacheReadUsdPerMtok: 0.25, cacheWriteUsdPerMtok: 1.0 },
  { id: "grok-code", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.75, cacheWriteUsdPerMtok: 3.0 },
  { id: "grok-4", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.75, cacheWriteUsdPerMtok: 3.0 },
  { id: "grok-3-mini", mode: "token", inputUsdPerMtok: 0.3, outputUsdPerMtok: 1.5, cacheReadUsdPerMtok: 0.075, cacheWriteUsdPerMtok: 0.3 },
  { id: "grok-3", mode: "token", inputUsdPerMtok: 3, outputUsdPerMtok: 15, cacheReadUsdPerMtok: 0.75, cacheWriteUsdPerMtok: 3.0 },
  { id: "grok-2", mode: "token", inputUsdPerMtok: 2, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.5, cacheWriteUsdPerMtok: 2.0 },

  // ── DeepSeek ──
  { id: "deepseek-chat", mode: "token", inputUsdPerMtok: 0.14, outputUsdPerMtok: 0.28, cacheReadUsdPerMtok: 0.014, cacheWriteUsdPerMtok: 0.14 },
  { id: "deepseek-reasoner", mode: "token", inputUsdPerMtok: 0.55, outputUsdPerMtok: 2.19, cacheReadUsdPerMtok: 0.14, cacheWriteUsdPerMtok: 0.55 },
  { id: "deepseek-v3", mode: "token", inputUsdPerMtok: 0.14, outputUsdPerMtok: 0.28, cacheReadUsdPerMtok: 0.014, cacheWriteUsdPerMtok: 0.14 },
  { id: "deepseek-r1", mode: "token", inputUsdPerMtok: 0.55, outputUsdPerMtok: 2.19, cacheReadUsdPerMtok: 0.14, cacheWriteUsdPerMtok: 0.55 },

  // ── Moonshot / Kimi ──
  { id: "kimi-k3", mode: "token", inputUsdPerMtok: 1, outputUsdPerMtok: 5, cacheReadUsdPerMtok: 0.15, cacheWriteUsdPerMtok: 1.0 },
  { id: "moonshot-v1", mode: "token", inputUsdPerMtok: 1.6, outputUsdPerMtok: 1.6, cacheReadUsdPerMtok: 0.4, cacheWriteUsdPerMtok: 1.6 },

  // ── Zhipu GLM ──
  { id: "glm-4-plus", mode: "token", inputUsdPerMtok: 1.4, outputUsdPerMtok: 1.4, cacheReadUsdPerMtok: 0.28, cacheWriteUsdPerMtok: 1.4 },
  { id: "glm-4-air", mode: "token", inputUsdPerMtok: 0.14, outputUsdPerMtok: 0.14, cacheReadUsdPerMtok: 0.028, cacheWriteUsdPerMtok: 0.14 },
  { id: "glm-4-flash", mode: "token", inputUsdPerMtok: 0.01, outputUsdPerMtok: 0.01, cacheReadUsdPerMtok: 0.002, cacheWriteUsdPerMtok: 0.01 },
  { id: "glm-5.2", mode: "token", inputUsdPerMtok: 1, outputUsdPerMtok: 5, cacheReadUsdPerMtok: 0.2, cacheWriteUsdPerMtok: 1.0 },
  { id: "glm-4", mode: "token", inputUsdPerMtok: 1.4, outputUsdPerMtok: 1.4, cacheReadUsdPerMtok: 0.28, cacheWriteUsdPerMtok: 1.4 },

  // ── Alibaba Qwen ──
  { id: "qwen-2.5-coder-32b", mode: "token", inputUsdPerMtok: 0.18, outputUsdPerMtok: 0.18, cacheReadUsdPerMtok: 0.045, cacheWriteUsdPerMtok: 0.18 },
  { id: "qwen-2.5-72b", mode: "token", inputUsdPerMtok: 0.35, outputUsdPerMtok: 0.35, cacheReadUsdPerMtok: 0.088, cacheWriteUsdPerMtok: 0.35 },
  { id: "qwen-max", mode: "token", inputUsdPerMtok: 2.8, outputUsdPerMtok: 8.4, cacheReadUsdPerMtok: 0.7, cacheWriteUsdPerMtok: 2.8 },
  { id: "qwen-plus", mode: "token", inputUsdPerMtok: 0.42, outputUsdPerMtok: 1.26, cacheReadUsdPerMtok: 0.105, cacheWriteUsdPerMtok: 0.42 },
  { id: "qwen-turbo", mode: "token", inputUsdPerMtok: 0.042, outputUsdPerMtok: 0.084, cacheReadUsdPerMtok: 0.01, cacheWriteUsdPerMtok: 0.042 },

  // ── Image Models ──
  { id: "gpt-image-2", mode: "image", usdPerImage: { default: 0.04, "1024x1024": 0.04, "1536x1024": 0.08 } },
  { id: "gpt-image-1", mode: "image", usdPerImage: { default: 0.04, "1024x1024": 0.04 } },
  { id: "nano-banana-2", mode: "image", usdPerImage: { default: 0.04, "1536x1024": 0.04 } },
  { id: "dall-e-3", mode: "image", usdPerImage: { default: 0.04, "1024x1024": 0.04, "1792x1024": 0.08 } },
  { id: "dall-e-2", mode: "image", usdPerImage: { default: 0.02, "1024x1024": 0.02 } },

  // ── Routing Fallbacks ──
  { id: "auto", mode: "token", inputUsdPerMtok: 1.25, outputUsdPerMtok: 10, cacheReadUsdPerMtok: 0.3, cacheWriteUsdPerMtok: 1.25 },
];

/** 剥除通道前缀与路由命名空间（如 `cursor/`, `chatgpt/`, `test01/`, `mesh/`）。 */
export function stripChannelPrefix(modelId: string): string {
  const slash = modelId.indexOf("/");
  return slash >= 0 ? modelId.slice(slash + 1) : modelId;
}

/** 剥除档位后缀（如 `-thinking-max-fast`, `-thinking-high`, `-max`）。 */
export function stripTierSuffix(modelId: string): string {
  return modelId
    .replace(/-thinking-max-fast$/, "")
    .replace(/-thinking-max$/, "")
    .replace(/-thinking-high$/, "")
    .replace(/-thinking$/, "")
    .replace(/-max-fast$/, "")
    .replace(/-xhigh$/, "")
    .replace(/-max$/, "")
    .replace(/-high$/, "")
    .replace(/-medium$/, "")
    .replace(/-low$/, "")
    .replace(/-fast$/, "");
}

// 建立 LiteLLM 目录的索引表
const rawCards = (catalogData as unknown as { cards: Record<string, PriceCard> }).cards || {};
const byBareMap = new Map<string, PriceCard>();

for (const [key, card] of Object.entries(rawCards)) {
  const slash = key.lastIndexOf("/");
  const bare = (slash >= 0 ? key.slice(slash + 1) : key).toLowerCase().trim();
  if (!bare) continue;
  if (!byBareMap.has(bare)) {
    const pc = { ...(card as PriceCard) };
    if (pc.mode === "token" && pc.inputUsdPerMtok !== undefined) {
      if (pc.cacheReadUsdPerMtok === undefined) {
        pc.cacheReadUsdPerMtok = Number((pc.inputUsdPerMtok * 0.1).toFixed(4));
      }
      if (pc.cacheWriteUsdPerMtok === undefined) {
        pc.cacheWriteUsdPerMtok = pc.inputUsdPerMtok;
      }
    }
    byBareMap.set(bare, pc);
  }
}

/**
 * 解析任意模型 ID 的官方定价。
 *
 * 搜索层次：
 * 1. 规范化纯模型名（移除通道前缀）
 * 2. 精确匹配精选表（CURATED_PRICES）
 * 3. 前缀匹配精选表（按最长前缀优先）
 * 4. 剥除档位后缀再匹配精选表
 * 5. 匹配 LiteLLM 全量官方模型库（bare name 或 exact key）
 * 6. 兜底价 FALLBACK_PRICE
 */
export function resolveModelPrice(modelId: string): PriceCard {
  if (!modelId) return FALLBACK_PRICE;
  const raw = stripChannelPrefix(modelId).toLowerCase().trim();
  if (!raw) return FALLBACK_PRICE;

  // 1. 精选表精确匹配
  for (const c of CURATED_PRICES) {
    if (c.id.toLowerCase() === raw) return c;
  }

  // 2. 剥除档位后缀后再精确匹配
  const withoutTier = stripTierSuffix(raw);
  if (withoutTier !== raw) {
    for (const c of CURATED_PRICES) {
      if (c.id.toLowerCase() === withoutTier) return c;
    }
  }

  // 3. 最长前缀匹配精选表
  let bestCurated: PriceCard | undefined;
  for (const c of CURATED_PRICES) {
    const cid = c.id.toLowerCase();
    if (cid === "*") continue;
    if (raw.startsWith(cid) || withoutTier.startsWith(cid)) {
      if (!bestCurated || cid.length > bestCurated.id.length) {
        bestCurated = c;
      }
    }
  }
  if (bestCurated) return bestCurated;

  // 4. LiteLLM 全量表匹配（裸名或带点换横杠名）
  const bareHit = byBareMap.get(raw) || byBareMap.get(withoutTier) || byBareMap.get(raw.replace(/\./g, "-"));
  if (bareHit) return bareHit;

  // 5. LiteLLM 最长前缀匹配
  for (const [bare, card] of byBareMap) {
    if (raw.startsWith(bare)) {
      return card;
    }
  }

  return FALLBACK_PRICE;
}

/** 格式化美元价格数字，确保精致工整。 */
export function formatPrice(n: number | undefined): string {
  if (n === undefined || !Number.isFinite(n)) return "0.00";
  if (n >= 1) {
    return n.toFixed(2);
  }
  if (n >= 0.01) {
    const s = n.toFixed(3);
    return s.endsWith("0") ? n.toFixed(2) : s;
  }
  return n.toFixed(4).replace(/0+$/, "");
}
