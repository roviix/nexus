/**
 * ChatGPT 账号页签的纯函数：标签、窗口名、接力状态、卡片用量。和页面分开是为了能单测。
 */
import type { GatewayMembership } from "../../accounts/pools";
import type {
  ChatGptAccount,
  ChatGptBilling,
  ChatGptImportOutcome,
  ChatGptRateLimitBucket,
  ChatGptTraffic,
  ChatGptUsage,
  ChatGptUsageWindow,
  GatewayCandidate,
} from "../../ipc/types";
import { compactNumber } from "../../ui/traffic";
import type { RailTone } from "../../ui/AccountLine";

/** 给人看的名字：邮箱；没有就用 chatgpt_account_id 的尾巴（纯 access_token 导入时读不到邮箱）。 */
export function labelOf(a: Pick<ChatGptAccount, "email" | "accountRef">): string {
  const email = a.email?.trim();
  return email ? email : `chatgpt…${a.accountRef.slice(-6)}`;
}

/** 5 小时 / 7 天——按窗口长度算，不写死：上游改过窗口长度。 */
export function windowLabel(minutes: number | null | undefined, fallback: string): string {
  if (minutes == null || minutes <= 0) return fallback;
  if (minutes % 1440 === 0) return `${minutes / 1440} 天`;
  if (minutes % 60 === 0) return `${minutes / 60} 小时`;
  return `${minutes} 分钟`;
}

/** 窗口用满且知道何时重置，才值得在条子旁说一句「几点重置」。 */
export function windowIsFull(w: ChatGptUsageWindow | null | undefined): boolean {
  return w?.usedPercent != null && w.usedPercent >= 100;
}

export type LaneBadge = { text: string; tone: "ok" | "bad" | "default" } | null;

/** 接力里的位置。只有耗尽 / 到线是坏消息；「待接力」是常态，不标。 */
export function laneBadge(c: GatewayCandidate | null, enabled: boolean): LaneBadge {
  if (!enabled) return { text: "已暂停", tone: "default" };
  if (!c) return null;
  switch (c.state.kind) {
    case "current":
      return { text: "正在用", tone: "ok" };
    case "exhausted":
      return { text: `耗尽 · ${Math.ceil(c.state.retryInSecs / 60)} 分后重试`, tone: "bad" };
    case "quota_line":
      return { text: "额度到线", tone: "bad" };
    case "cooled":
      return { text: `${c.state.models.join(", ")} 冷却 ${Math.ceil(c.state.secsLeft / 60)} 分`, tone: "default" };
    default:
      return null;
  }
}

/** 粘贴导入的一句话结论。失败条数单独说，好让人知道哪些没进去。 */
export function importSummary(o: Pick<ChatGptImportOutcome, "created" | "updated" | "failed">): string {
  const bits: string[] = [];
  if (o.created) bits.push(`新建 ${o.created} 个`);
  if (o.updated) bits.push(`更新 ${o.updated} 个`);
  if (o.failed) bits.push(`${o.failed} 个没进去`);
  return bits.join(" · ") || "没有导入任何账号";
}

/** 套餐徽章的样式类。gpt 的套餐名和 Cursor 的不一样，只认三个词。 */
export function planClass(plan: string | null | undefined): string | null {
  if (!plan) return null;
  const p = plan.toLowerCase();
  if (p.includes("pro")) return "plan plan-pro";
  if (p.includes("team")) return "plan plan-team";
  if (p.includes("free")) return "plan plan-free";
  return "plan";
}

/** 徽章上的词。Cursor 的 `planLabel` 会把 plus 收成 Pro+，这里不能借。 */
export function chatgptPlanLabel(plan: string | null | undefined): string | null {
  if (!plan?.trim()) return null;
  const p = plan.toLowerCase();
  if (p.includes("prolite") || p === "pro_lite" || p === "pro-lite") return "Pro Lite";
  if (p.includes("pro")) return "Pro";
  if (p.includes("team") || p.includes("business") || p.includes("enterprise")) return "Team";
  if (p.includes("plus")) return "Plus";
  if (p.includes("free")) return "Free";
  return plan.trim();
}

/** JWT 工作区名。个人号常写成 Personal，收成「个人账户」。 */
export function chatgptOrgLabel(title: string | null | undefined): string {
  const t = title?.trim();
  if (!t || /^personal$/i.test(t) || t === "个人" || t === "个人账户") return "个人账户";
  return t;
}

/** `GPT-5.3-Codex-Spark` → `Spark`，卡片上要短。 */
export function chatgptLimitShort(name: string | null | undefined): string {
  if (!name?.trim()) return "附加额度";
  const n = name.trim();
  if (/spark/i.test(n)) return "Spark";
  if (/reserve/i.test(n)) return "Reserve";
  return n.replace(/^GPT-?[\d.]+-?/i, "").replace(/Codex-?/gi, "").replace(/[-_]+/g, " ").trim() || n;
}

export function chatgptHasCredits(c: ChatGptUsage["credits"]): boolean {
  if (!c) return false;
  if (c.unlimited) return true;
  if (c.hasCredits === false) return false;
  const bal = c.balance?.trim();
  return Boolean(bal && bal !== "0" && bal !== "0.0");
}

export function chatgptTrafficText(t: ChatGptTraffic | null | undefined): string | null {
  if (!t || (t.requests <= 0 && t.tokens <= 0)) return null;
  return `${compactNumber(t.requests)} 次 · ${compactNumber(t.tokens)} token`;
}

export function chatgptUsable(a: Pick<ChatGptAccount, "status" | "hasRefresh">): boolean {
  return a.status === "active" && a.hasRefresh;
}

export type ChatGptProblem = { label: string; tone: "bad" | "warn" };

/** 卡上 / 抽屉头上那句健康度。没有问题就别标。 */
export function chatgptProblem(a: ChatGptAccount, now = Date.now()): ChatGptProblem | null {
  if (a.status === "dead") return { label: "已停用", tone: "bad" };
  if (!chatgptUsable(a)) return { label: "需要重新授权", tone: "warn" };
  if (chatgptSubscriptionExpired(a.billing, now)) return { label: "订阅已过期", tone: "bad" };
  const u = a.usage;
  if (u && windowIsFull(u.primary) && windowIsFull(u.secondary)) return { label: "额度用尽", tone: "bad" };
  return null;
}

export function chatgptExpiresMs(iso: string | null | undefined): number | null {
  if (!iso) return null;
  const t = Date.parse(iso);
  return Number.isFinite(t) ? t : null;
}

/** 按日历日算还剩几天。到期日没读到就是 `null`，不要编 0。 */
export function chatgptRemainingDays(expiresAt: string | null | undefined, now = Date.now()): number | null {
  const t = chatgptExpiresMs(expiresAt);
  if (t == null) return null;
  const start = new Date(now);
  start.setUTCHours(0, 0, 0, 0);
  const end = new Date(t);
  end.setUTCHours(0, 0, 0, 0);
  return Math.round((end.getTime() - start.getTime()) / 86_400_000);
}

/**
 * 订阅过期：API 明确说没在订，或到期日已过且没说会续。
 * `hasActiveSubscription === true` 或 `willRenew === true` 都不算过期。
 */
export function chatgptSubscriptionExpired(b: ChatGptBilling | null | undefined, now = Date.now()): boolean {
  if (!b) return false;
  if (b.hasActiveSubscription === true || b.willRenew === true) return false;
  if (b.hasActiveSubscription === false) return true;
  const days = chatgptRemainingDays(b.expiresAt, now);
  return days != null && days < 0;
}

/** 卡片末行那句「还剩 N 天」。到期日没读到就别写。 */
export function chatgptSubscriptionText(b: ChatGptBilling | null | undefined, now = Date.now()): string | null {
  if (!b) return null;
  const days = chatgptRemainingDays(b.expiresAt, now);
  if (days == null) return null;
  if (days > 0) return `订阅还剩 ${days} 天`;
  if (days === 0) return "订阅今天到期";
  if (b.willRenew === true) return "账期已过 · 将续费";
  if (b.willRenew === false) return "订阅已过期 · 不续费";
  return "订阅已过期";
}

export function chatgptRenewText(willRenew: boolean | null | undefined): string {
  if (willRenew === true) return "会自动续费";
  if (willRenew === false) return "不会自动续费";
  return "会不会续费未知";
}

export function chatgptActiveText(hasActive: boolean | null | undefined, expired: boolean): string {
  if (hasActive === true) return "有效";
  if (hasActive === false) return "无有效订阅";
  if (expired) return "已过期";
  return "未知";
}

export function chatgptPeriodLabel(period: string | null | undefined): string | null {
  if (!period?.trim()) return null;
  const p = period.trim().toLowerCase();
  if (p === "month" || p === "monthly") return "按月";
  if (p === "year" || p === "yearly" || p === "annual") return "按年";
  return period.trim();
}

/**
 * ChatGPT 没有切号池，名单就是 `account.enabled`。
 * 网关没开、lane 对不上时，开着的号仍算已加入 —— 开了网关才会出现在接力队里。
 */
export function chatgptGatewayMembership(a: ChatGptAccount, lane: GatewayCandidate | null): GatewayMembership {
  if (!a.enabled) return chatgptUsable(a) ? "available" : "none";
  if (lane?.state.kind === "current") return "current";
  if (lane?.state.kind === "exhausted" || lane?.state.kind === "quota_line") return "skipped";
  return "enrolled";
}

export function chatgptRailTone(a: ChatGptAccount): RailTone {
  if (a.status === "dead") return "bad";
  if (!chatgptUsable(a)) return "warn";
  const u = a.usage;
  if (!u) return "none";
  const worst = Math.max(u.primary?.usedPercent ?? 0, u.secondary?.usedPercent ?? 0);
  if (worst >= 90) return "bad";
  if (worst >= 70) return "warn";
  return "ok";
}

export interface ChatGptWindowView {
  key: string;
  label: string;
  hint: string;
  percent: number | null;
  resetAt: number | null;
  group: string;
}

/** 卡片上的条子。主窗口缺了也占一行；附加桶（Spark）只画上游给了的。 */
export function chatgptWindowViews(usage: ChatGptUsage): ChatGptWindowView[] {
  const extras = usage.additional ?? [];
  const prefix = extras.length ? "Codex " : "";
  const main: ChatGptWindowView[] = [
    windowView("primary", usage.primary, `${prefix}${windowLabel(usage.primary?.windowMinutes, "5 小时")}`, "Codex"),
    windowView("secondary", usage.secondary, `${prefix}${windowLabel(usage.secondary?.windowMinutes, "7 天")}`, "Codex"),
  ];
  return [...main, ...extras.flatMap((bucket, i) => extraViews(bucket, i))];
}

function extraViews(bucket: ChatGptRateLimitBucket, i: number): ChatGptWindowView[] {
  const short = chatgptLimitShort(bucket.name);
  const group = bucket.name ?? short;
  const out: ChatGptWindowView[] = [];
  if (bucket.primary) {
    out.push(
      windowView(`a${i}p`, bucket.primary, `${short} ${windowLabel(bucket.primary.windowMinutes, "5 小时")}`, group),
    );
  }
  if (bucket.secondary) {
    out.push(
      windowView(`a${i}s`, bucket.secondary, `${short} ${windowLabel(bucket.secondary.windowMinutes, "7 天")}`, group),
    );
  }
  return out;
}

function windowView(key: string, w: ChatGptUsageWindow | null, label: string, group: string): ChatGptWindowView {
  return {
    key,
    label,
    hint: w?.resetAtMs != null ? `${label} · 到点重置` : `${label}已用比例`,
    percent: w?.usedPercent ?? null,
    resetAt: w?.resetAtMs ?? null,
    group,
  };
}
