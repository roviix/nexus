/**
 * 多选复制：复制什么、附带什么。
 *
 * 凭证那一行（`邮箱----密码` 这类）是给脚本吃的，格式不能动；用量这类说明**另起一行**跟在后面。
 * 文案在这里排好（按本地时区），Rust 侧只负责把它放到凭证行下面（文本）或 `info` 字段里（JSON）。
 */

import type { Account, AccountUsage } from "../ipc/types";
import { creditPoints, onDemandParts } from "../ui/usage";

export type CopyFormat = "email" | "email_password" | "email_refresh" | "email_session" | "json";

export const COPY_FORMATS: Array<{ id: CopyFormat; label: string; sample: string }> = [
  { id: "email", label: "仅邮箱", sample: "a@example.com" },
  { id: "email_password", label: "邮箱----密码", sample: "a@example.com----P@ssw0rd" },
  { id: "email_refresh", label: "邮箱----Refresh Token", sample: "a@example.com----eyJhbGci…" },
  { id: "email_session", label: "邮箱----Session Token", sample: "a@example.com----user_01ABC…::eyJhbGci…" },
  { id: "json", label: "结构化 JSON", sample: '{ "accounts": [ … ] }' },
];

export type CopyExtra = "api" | "on_demand" | "credits" | "resets";

export const COPY_EXTRAS: Array<{ id: CopyExtra; label: string; hint: string }> = [
  { id: "api", label: "API 剩余", hint: "高级模型额度还剩几成" },
  { id: "on_demand", label: "按需用量", hint: "超出订阅额度后花了多少、上限多少" },
  { id: "credits", label: "积分", hint: "Cursor 赠送的 credit grant 还剩多少" },
  { id: "resets", label: "重置时间", hint: "月额与 Bot 周额各自哪天重置" },
];

const EXTRA_ORDER: CopyExtra[] = COPY_EXTRAS.map((e) => e.id);

export interface CopyChoice {
  format: CopyFormat;
  extras: CopyExtra[];
}

export const DEFAULT_COPY_CHOICE: CopyChoice = { format: "email", extras: [] };

const STORAGE_KEY = "nexus.accounts.copy";

/** 上次选的格式与附加项。选一次记住，下次弹窗直接是上次那套。 */
export function loadCopyChoice(storage: Pick<Storage, "getItem"> | null = safeStorage()): CopyChoice {
  try {
    const raw = storage?.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_COPY_CHOICE;
    return normalizeChoice(JSON.parse(raw));
  } catch {
    return DEFAULT_COPY_CHOICE;
  }
}

export function saveCopyChoice(choice: CopyChoice, storage: Pick<Storage, "setItem"> | null = safeStorage()) {
  try {
    storage?.setItem(STORAGE_KEY, JSON.stringify(normalizeChoice(choice)));
  } catch {
    // 私密模式 / 配额满：记不住就记不住，不影响复制本身。
  }
}

/** 不认识的值一律回默认；附加项按固定顺序去重，输出行的顺序才稳定。 */
export function normalizeChoice(raw: unknown): CopyChoice {
  const r = (raw ?? {}) as Partial<Record<keyof CopyChoice, unknown>>;
  const format = COPY_FORMATS.some((f) => f.id === r.format) ? (r.format as CopyFormat) : DEFAULT_COPY_CHOICE.format;
  const wanted = new Set(Array.isArray(r.extras) ? r.extras : []);
  const extras = EXTRA_ORDER.filter((e) => wanted.has(e));
  return { format, extras };
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}

/* ── 说明行 ──────────────────────────────────────────────────────────────── */

/**
 * 一个号的说明行，如 `API 余 54% · 按需未开启 · 积分 100 · 月额 09/30 重置 · Bot 09/18 重置`。
 * 没选附加项时返回空串（Rust 侧当没有）。没查过用量的号只说一句「未查用量」——
 * 与其四个「未知」占着行，不如一句话讲清为什么没数。
 */
export function copyInfoLine(account: Pick<Account, "usage">, extras: readonly CopyExtra[]): string {
  if (extras.length === 0) return "";
  const usage = account.usage;
  if (!usage) return "未查用量";
  const parts: string[] = [];
  for (const extra of EXTRA_ORDER) {
    if (!extras.includes(extra)) continue;
    const text = extraText(usage, extra);
    if (text) parts.push(text);
  }
  return parts.join(" · ");
}

function extraText(usage: AccountUsage, extra: CopyExtra): string {
  switch (extra) {
    case "api":
      return apiRemainingText(usage);
    case "on_demand": {
      const { k, v, sub } = onDemandParts(usage);
      return [v ? `${k} ${v}` : k, sub].filter(Boolean).join(" · ");
    }
    case "credits": {
      const remaining = usage.creditGrantRemainingCents;
      return remaining != null && remaining > 0 ? `积分 ${creditPoints(remaining)}` : "无积分";
    }
    case "resets": {
      const items: string[] = [];
      if (usage.cycleEnd != null && Number.isFinite(usage.cycleEnd)) items.push(`月额 ${dateMinute(usage.cycleEnd)} 重置`);
      if (usage.bot?.resetAt != null && Number.isFinite(usage.bot.resetAt)) items.push(`Bot ${dateMinute(usage.bot.resetAt)} 重置`);
      return items.length ? items.join(" · ") : "重置时间未知";
    }
  }
}

/**
 * 重置时刻到分钟，`09/30 20:00`，本地时区。卡片上写「17 天后」够了，复制出去的那行是给别人对表的：
 * 月额几点重置决定这个号今晚还能不能用，只给日期不够。
 */
export function dateMinute(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getMonth() + 1)}/${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** API 桶没单独计量时退到总额度，两个都没有才说未知。 */
function apiRemainingText(usage: AccountUsage): string {
  const used = usage.apiPercentUsed ?? usage.totalPercentUsed;
  if (used == null || !Number.isFinite(used)) return "API 未知";
  const remaining = Math.max(0, Math.min(100, 100 - Math.round(used)));
  return `API 余 ${remaining}%`;
}

/** 每个号一行说明，按 id 交给 Rust。没选附加项就是空表。 */
export function copyInfoMap(list: readonly Pick<Account, "id" | "usage">[], extras: readonly CopyExtra[]): Record<string, string> {
  const out: Record<string, string> = {};
  if (extras.length === 0) return out;
  for (const a of list) out[a.id] = copyInfoLine(a, extras);
  return out;
}
