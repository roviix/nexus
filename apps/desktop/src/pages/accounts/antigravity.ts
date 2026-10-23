/**
 * Google 反重力 (Antigravity) 纯函数与用量模型转换。
 *
 * 官方标准：Google 反重力在客户端总共划分为两类核心模型配额：
 * 1. Gemini 限额 (所有 Gemini 2.5/3 系列共享)
 * 2. Claude & GPT 限额 (所有 Claude 3.7/Sonnet/Opus 及 GPT 模型共享)
 */
import type { AntigravityUsage } from "../../ipc/types";

export interface QuotaLimitItem {
  name: string;
  percentRemaining: number;
  resetsInText: string | null;
  tone: "ok" | "warn" | "bad";
}

export interface AntigravityCategoryQuota {
  category: "gemini" | "claude_gpt";
  title: string;
  items: QuotaLimitItem[];
}

export function parseAntigravityCategories(
  usage: AntigravityUsage | null | undefined,
): AntigravityCategoryQuota[] {
  const groups = usage?.quotaGroups ?? [];
  if (groups.length > 0) {
    return groups.flatMap((group) => {
      const name = group.displayName.toLowerCase();
      const category: AntigravityCategoryQuota["category"] = name.includes("gemini")
        ? "gemini"
        : "claude_gpt";
      const title = category === "gemini" ? "Gemini" : "Others";
      const items = group.buckets.flatMap((bucket) => {
        const window = bucket.window.toLowerCase();
        const weekly = window.includes("week") || window.includes("7d");
        const fiveHour = window.includes("5h") || window.includes("5-h") || window === "5";
        if (!weekly && !fiveHour) return [];
        const percentRemaining = Math.max(0, Math.min(100, Math.round(bucket.remainingFraction * 100)));
        return [
          {
            name: weekly ? "Weekly" : "5h",
            percentRemaining,
            resetsInText: formatTimeUntilShort(bucket.resetTime),
            tone: toneOf(percentRemaining),
          },
        ];
      });
      if (items.length === 0) return [];
      return [{ category, title, items }];
    });
  }

  const buckets = usage?.modelBuckets ?? [];

  // 1. Gemini Models: 匹配所有以 gemini- 开头的模型桶
  const geminiBuckets = buckets.filter((b) => b.modelId.toLowerCase().startsWith("gemini-"));
  const geminiActive = geminiBuckets.length > 0
    ? geminiBuckets.reduce((prev, curr) => (curr.remainingFraction < prev.remainingFraction ? curr : prev))
    : null;

  const gemini5hPct = geminiActive
    ? Math.round(geminiActive.remainingFraction * 100)
    : (usage?.percentRemaining != null ? Math.round(usage.percentRemaining) : 100);

  const gemini5hReset = geminiActive?.resetTime
    ? formatTimeUntilShort(geminiActive.resetTime)
    : formatTimeUntilShort(usage?.resetsAt);

  // 2. Claude & GPT models: 匹配 claude- 或 gpt- 开头的模型桶
  const claudeGptBuckets = buckets.filter((b) => {
    const id = b.modelId.toLowerCase();
    return id.startsWith("claude-") || id.startsWith("gpt-");
  });

  const claudeGptActive = claudeGptBuckets.length > 0
    ? claudeGptBuckets.reduce((prev, curr) => (curr.remainingFraction < prev.remainingFraction ? curr : prev))
    : null;

  const claudeGpt5hPct = claudeGptActive
    ? Math.round(claudeGptActive.remainingFraction * 100)
    : 100;

  const claudeGpt5hReset = claudeGptActive?.resetTime
    ? formatTimeUntilShort(claudeGptActive.resetTime)
    : null;

  const out: AntigravityCategoryQuota[] = [];
  if (geminiActive || usage?.percentRemaining != null) {
    out.push({
      category: "gemini",
      title: "Gemini",
      items: [
        {
          name: "5h",
          percentRemaining: Math.max(0, Math.min(100, gemini5hPct)),
          resetsInText: gemini5hReset,
          tone: toneOf(gemini5hPct),
        },
      ],
    });
  }
  if (claudeGptActive) {
    out.push({
      category: "claude_gpt",
      title: "Others",
      items: [
        {
          name: "5h",
          percentRemaining: Math.max(0, Math.min(100, claudeGpt5hPct)),
          resetsInText: claudeGpt5hReset,
          tone: toneOf(claudeGpt5hPct),
        },
      ],
    });
  }
  return out;
}

function toneOf(pct: number): "ok" | "warn" | "bad" {
  if (pct <= 10) return "bad";
  if (pct <= 30) return "warn";
  return "ok";
}

export function formatTimeUntilShort(resetTimeIso?: string | null): string | null {
  if (!resetTimeIso) return null;
  const ts = Date.parse(resetTimeIso);
  if (!Number.isFinite(ts)) return null;
  const diffMs = ts - Date.now();
  if (diffMs <= 0) return "已重置";
  const mins = Math.floor(diffMs / 60_000);
  if (mins < 60) return `${Math.max(1, mins)} 分钟后重置`;
  const hours = Math.floor(mins / 60);
  if (hours < 48) return `${hours} 小时后重置`;
  return `${Math.floor(hours / 24)} 天后重置`;
}

export function formatTierLabel(tier?: string | null): string {
  if (!tier) return "Google AI 订阅";
  const lower = tier.toLowerCase();
  if (lower === "ultra" || lower.includes("ultra") || lower.includes("helium")) return "Google AI Ultra";
  if (lower === "pro" || lower.includes("pro") || lower.includes("premium") || lower.includes("advanced")) {
    return "Google AI Pro";
  }
  if (lower === "free" || lower.includes("free") || lower.includes("starter")) return "免费体验版";
  return tier;
}

/** 卡片和抽屉共用的用量行。按 Gemini 与 Others 自动分组展示。百分比是已用，不是剩余。 */
export function antigravityQuotaRows(usage: AntigravityUsage | null | undefined) {
  return parseAntigravityCategories(usage).flatMap((group) =>
    group.items.map((item) => {
      const windowLabel = item.name === "Weekly" ? "Weekly" : "5h";
      return {
        key: `${group.category}-${item.name}`,
        group: group.title,
        label: windowLabel,
        hint: item.resetsInText ? `${group.title} · ${windowLabel} · ${item.resetsInText}` : `${group.title} · ${windowLabel}`,
        percent: Math.max(0, Math.min(100, Math.round(100 - item.percentRemaining))),
        resetText: item.resetsInText,
      };
    }),
  );
}

export function planClassName(tier?: string | null): string {
  if (!tier) return "plan plan-pro";
  const lower = tier.toLowerCase();
  if (lower.includes("ultra")) return "plan plan-team";
  if (lower.includes("free")) return "plan plan-free";
  return "plan plan-pro";
}
