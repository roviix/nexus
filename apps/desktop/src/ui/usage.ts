/**
 * 用量的展示语汇。
 *
 * 口径与 shop 的运营面板（`admin/pool/_usage.tsx`）保持一致，一部分规则是那边用真实
 * 数据磨出来的，照抄过来而不是重新发明：
 *   - 所有百分比都是「**已用**」不是「可用」，条填满 = 额度用完 = 红。Cursor 原始字段
 *     就叫 percentUsed，中间翻转一次只会在排查时多一层脑内换算。
 *   - 用了但不到 1% 显示 `<1%`，不显示 `0%` —— 后者会被读成「一次没用过」。
 *   - 条子最小给 2% 宽度，否则「用了一点」和「完全没用」在视觉上一样。
 */
import type { AccountUsage, BotQuota, Availability } from "../ipc/types";

export const DAY_MS = 86_400_000;

/* ── 订阅档 ───────────────────────────────────────────────────────────────── */

/**
 * Cursor 的 Team 套餐在接口里叫 `enterprise`（旧写法 `business`）。用户买的、看到的都是「Team」，
 * 徽章上写 Enterprise 只会让人对着筛选里的「Team」纳闷是不是两回事 —— 所以三种写法都落 Team。
 */
const PLAN_LABEL: Record<string, string> = {
  ultra: "Ultra",
  pro_plus: "Pro+",
  "pro-plus": "Pro+",
  proplus: "Pro+",
  pro: "Pro",
  free: "Free",
  free_trial: "Pro Trial",
  enterprise: "Team",
  business: "Team",
  team: "Team",
};

/**
 * 也接受一个裸档位字符串：切号本里的档是从登录态直接带过来的，没有完整的用量对象，
 * 但它该跟账号卡片上的那个长得一模一样。
 */
type PlanLike = AccountUsage | string | null | undefined;

function planOf(p: PlanLike): { plan: string; trialing: boolean } {
  if (typeof p === "string") return { plan: p, trialing: false };
  return { plan: p?.plan ?? "", trialing: p?.subscriptionStatus === "trialing" };
}

/** Cursor 原样返回 `pro_plus` 这种，直接摆出来不好看。 */
export function planLabel(p: PlanLike): string {
  const { plan, trialing } = planOf(p);
  const raw = plan.toLowerCase().trim();
  if (!raw) return "未知";
  const base =
    PLAN_LABEL[raw] || raw.replace(/_/g, " ").replace(/\b\w/g, (c) => c.toUpperCase());
  // trialing 的号随时会掉档，值不一样，得标出来。
  if (trialing && !/trial/i.test(base)) return `${base} Trial`;
  return base;
}

/**
 * 档位的分组，列表按档筛选就按这个口径。
 *
 * 「未知」是**没查过用量、或 Cursor 返回了一个认不出的档**：它不该混进 Free 里冒充一个
 * 已核实的档 —— 同 poolState 那条「未查不进正常」的规矩。判断顺序要紧：`pro_plus` 既含
 * plus 又含 pro，plus 必须先判；`free_trial` 落在 free。
 */
export type PlanGroup = "ultra" | "proplus" | "team" | "pro" | "free" | "unknown";

export function planGroup(p: PlanLike): PlanGroup {
  const raw = planOf(p).plan.toLowerCase().trim();
  if (!raw) return "unknown";
  if (raw.includes("ultra")) return "ultra";
  if (raw.includes("plus")) return "proplus";
  if (raw.includes("enterprise") || raw.includes("team") || raw.includes("business")) return "team";
  if (raw.includes("pro")) return "pro";
  // `credit-grant-churn-power-user-3p` 这类：订阅已退、靠 Cursor 赠的积分在用 —— 没有付费档，按 Free 归。
  if (raw.includes("free") || raw.includes("trial") || raw.includes("credit-grant")) return "free";
  return "unknown";
}

/** 档位的样式类。Ultra 和 Free 摆在一起不能只差一个字。 */
export function planTone(p: PlanLike): string {
  const g = planGroup(p);
  // 认不出的档沿用 Free 那身灰：徽章配色不该多出一档没人见过的颜色。
  return g === "unknown" ? "plan-free" : `plan-${g}`;
}

/* ── 百分比与颜色 ─────────────────────────────────────────────────────────── */

export function pctText(v?: number | null): string {
  if (v == null || !Number.isFinite(v)) return "—";
  if (v > 0 && v < 1) return "<1%";
  return `${Math.round(v)}%`;
}

/**
 * 消耗型指标的配色：**只有需要注意的那两档才有颜色**。
 *
 * 阈值与 shop 运营面板一致（>90 红、>=70 琥珀）。70% 以下给中性槽色，不给薄荷 ——
 * 一屏几十个号、每个号三条，健康的占绝大多数，全染成品牌色之后「有颜色 = 要看的」
 * 这条规矩就废了，红的还得在一片绿里抢注意力。同 §14.11 那句「一列绿键等于没挂」，
 * 只是那时候只把它用在了设置页，没用到额度条上。
 */
export function meterColor(percent: number): string {
  const p = Math.max(0, Math.min(100, percent));
  if (p > 90) return "var(--bad)";
  if (p >= 70) return "var(--warn)";
  return "var(--track-fill)";
}

/** 条子宽度。用了一点点也要看得见。 */
export function meterWidth(percent?: number | null): number {
  if (percent == null || !Number.isFinite(percent)) return 0;
  if (percent <= 0) return 0;
  return Math.max(2, Math.min(100, percent));
}

/* ── 时间 ─────────────────────────────────────────────────────────────────── */

/** 「还剩 6 天 22 小时」。跨度大时只说天，免得一串数字读不出重点。 */
export function untilText(endMs?: number | null, now = Date.now()): string {
  if (endMs == null || !Number.isFinite(endMs)) return "—";
  const left = endMs - now;
  if (left <= 0) return "已到期";
  const d = Math.floor(left / DAY_MS);
  const h = Math.floor((left % DAY_MS) / 3_600_000);
  if (d >= 7) return `还剩 ${d} 天`;
  if (d >= 1) return `还剩 ${d} 天 ${h} 小时`;
  if (h >= 1) return `还剩 ${h} 小时`;
  return "不到 1 小时";
}

/**
 * 列表行里的倒计时，只给最大的那一档。
 *
 * `untilText` 的「还剩 1 天 3 小时」在一列里太长，会挤掉旁边的数字；扫列表的人
 * 要的也只是量级，精确到小时是展开之后的事。
 */
export function untilShort(endMs?: number | null, now = Date.now()): string {
  if (endMs == null || !Number.isFinite(endMs)) return "—";
  const left = endMs - now;
  if (left <= 0) return "已重置";
  const d = Math.floor(left / DAY_MS);
  if (d >= 1) return `${d} 天`;
  const h = Math.floor(left / 3_600_000);
  if (h >= 1) return `${h} 小时`;
  return "<1 小时";
}

/**
 * 「3 天后重置」那一格。已到期是「已重置」，未知是「—」。
 *
 * **不到一天就报钟点。** 倒计时答的是「还要等多久」，那是扫列表时的问题；可一旦进了当天，
 * 倒计时只剩「3 小时」这种粗粒度，而人这时候想知道的已经变成「几点回来」了。
 * 所以 24 小时以内换成 `21:00 重置`，跨过零点的加一个「明天」。
 *
 * **不上颜色。** 额度重置是好消息，越近越好 —— 染成琥珀会和仪表条抢「有颜色 = 要注意」
 * 那条规矩（§14.11）。要精确到年月日的，抽屉里有。
 */
export function resetInShort(endMs?: number | null, now = Date.now()): string {
  if (endMs == null || !Number.isFinite(endMs)) return "—";
  if (endMs <= now) return "已重置";
  if (endMs - now < DAY_MS) return `${clockShort(endMs, now)} 重置`;
  return `${untilShort(endMs, now)}后重置`;
}

/** 24 小时以内的那个时刻。同一天只报钟点，跨过零点的才需要「明天」。 */
function clockShort(ms: number, now: number): string {
  const d = new Date(ms);
  const hhmm = `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
  return new Date(now).getDate() === d.getDate() ? hhmm : `明天 ${hhmm}`;
}

/** 账期日期，`08/15` 这种短形。摆在一行里不能占太宽。 */
export function shortDate(ms?: number | null): string {
  if (ms == null || !Number.isFinite(ms)) return "—";
  const d = new Date(ms);
  return `${String(d.getMonth() + 1).padStart(2, "0")}/${String(d.getDate()).padStart(2, "0")}`;
}

/** 重置时刻，`9/9 16:00`。 */
export function shortDateTime(ms?: number | null): string {
  if (ms == null || !Number.isFinite(ms)) return "—";
  const d = new Date(ms);
  return `${d.getMonth() + 1}/${d.getDate()} ${String(d.getHours()).padStart(2, "0")}:${String(
    d.getMinutes(),
  ).padStart(2, "0")}`;
}

/** 账期已经走过的比例，用来画那条底色进度。 */
export function cycleProgress(usage?: AccountUsage | null, now = Date.now()): number | null {
  if (!usage?.cycleStart || !usage.cycleEnd || usage.cycleEnd <= usage.cycleStart) return null;
  return Math.max(0, Math.min(1, (now - usage.cycleStart) / (usage.cycleEnd - usage.cycleStart)));
}

export function isFresh(startMs?: number | null, now = Date.now()): boolean {
  return startMs != null && now - startMs < 2 * DAY_MS;
}

export function isEndingSoon(endMs?: number | null, now = Date.now()): boolean {
  return endMs != null && endMs > now && endMs - now < 2 * DAY_MS;
}

/** 本地零点，epoch ms。刷用量时递给 Rust，「今天」从这一刻起算。 */
export function startOfLocalDay(now = Date.now()): number {
  const d = new Date(now);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/* ── 金额 ─────────────────────────────────────────────────────────────────── */

/** Cursor 一律用美分记账。只在展示层除 100，别在数据层转，会丢精度。 */
export function money(cents?: number | null, digits = 2): string {
  if (cents == null || !Number.isFinite(cents)) return "—";
  return `$${(cents / 100).toFixed(digits)}`;
}

/** Stripe 零小数币种：金额就是面值，不再除 100。 */
const ZERO_DECIMAL = new Set([
  "bif",
  "clp",
  "djf",
  "gnf",
  "jpy",
  "kmf",
  "krw",
  "mga",
  "pyg",
  "rwf",
  "ugx",
  "vnd",
  "vuv",
  "xaf",
  "xof",
  "xpf",
]);

/**
 * Stripe 金额：带上币种。门户国家不一定是美元（实测出现过日元标价）。
 * 认不出的币种退回「CODE 12.34」，不要硬套 `$`。
 */
export function moneyFx(amount?: number | null, currency?: string | null, digits = 2): string {
  if (amount == null || !Number.isFinite(amount)) return "—";
  const ccy = (currency || "usd").toLowerCase();
  const zero = ZERO_DECIMAL.has(ccy);
  const major = zero ? amount : amount / 100;
  try {
    return new Intl.NumberFormat("zh-CN", {
      style: "currency",
      currency: ccy.toUpperCase(),
      minimumFractionDigits: zero ? 0 : digits,
      maximumFractionDigits: zero ? 0 : digits,
    }).format(major);
  } catch {
    return `${ccy.toUpperCase()} ${major.toFixed(zero ? 0 : digits)}`;
  }
}

/**
 * 卡片和账单里的「大数」用短一档的写法：`$8.40`、`$21`、`$1.2k`。
 * 两位小数留给对账的表格；扫一眼的地方，数字越短越先被读到。
 */
export function moneyShort(cents?: number | null): string {
  if (cents == null || !Number.isFinite(cents)) return "—";
  const d = cents / 100;
  if (Math.abs(d) >= 1000) return `$${(d / 1000).toFixed(1)}k`;
  if (Math.abs(d) >= 100) return `$${Math.round(d)}`;
  return `$${d.toFixed(2)}`;
}

/**
 * Cursor 赠送积分。credit grant 存在美分里，仪表盘上的「25 / 100 积分」就是整美元。
 * 非整美元才带分位，避免把 25 写成 $25.00 让人以为是另一套账。
 */
export function creditPoints(cents?: number | null): string {
  if (cents == null || !Number.isFinite(cents)) return "—";
  const d = cents / 100;
  if (Math.abs(d - Math.round(d)) < 0.005) return String(Math.round(d));
  return d.toFixed(2);
}

/* ── 花费节奏 ─────────────────────────────────────────────────────────────── */

/**
 * 本账期的额度上限（美分）。口径是 `plan.limit`；缺它时用「花费 ÷ 已用比例」倒推 ——
 * 老档偶尔不给 limit，但百分比总是有的。
 *
 * **不用 `includedCents`**：`breakdown.included / bonus / total` 都是消费分量（含额度里
 * 花掉的、赠送里花掉的、两者之和），不是额度本身。拿它当额度，用满的号会显示成
 * 「已用 100%」永远刚好用完，超支的那部分就看不见了。
 */
export function planBudget(usage?: AccountUsage | null): number | null {
  if (!usage) return null;
  const limit = usage.planLimitCents;
  if (limit != null && Number.isFinite(limit) && limit > 0) return limit;
  const spend = planSpend(usage);
  const pct = usage.totalPercentUsed;
  if (spend != null && pct != null && pct > 0 && spend > 0) return (spend / pct) * 100;
  return null;
}

/**
 * 本账期**算在订阅额度上**的花费（美分）。
 *
 * Cursor 把一期的消费拆成三份：`breakdown.included` 从订阅额度里扣，`breakdown.bonus` 是
 * Cursor 与模型厂商补贴的**免费加量**（"free usage beyond what you've purchased"），
 * `onDemand.used` 是额度用完后扣信用卡的按需。`spendCents = included + bonus`。
 *
 * 以前拿 `spendCents` 对着 `plan.limit` 算「已用 / 剩余」，于是一个 Pro 号显示
 * 「已用 $88.68 / 额度 $20 / 剩余 $0 · 超出的走按需」，可它按需实际是 $0 —— 超出的 $68 是
 * 白送的。要回答「订阅额度还剩多少」，只能拿 included 那一份。没有 breakdown 的老档退回
 * spendCents，那时也没有 bonus 这个概念。
 */
export function planSpend(usage?: AccountUsage | null): number | null {
  if (!usage) return null;
  const inc = usage.includedCents;
  if (inc != null && Number.isFinite(inc)) return inc;
  return usage.spendCents ?? null;
}

/** 本账期的免费加量（bonus）；没有或为零时 null，界面整行不出现。 */
export function bonusSpend(usage?: AccountUsage | null): number | null {
  const b = usage?.bonusCents;
  return b != null && Number.isFinite(b) && b > 0 ? b : null;
}

/**
 * 花费的节奏：不是「花了多少」，而是「照这个速度下去会怎样」。
 *
 * 账单页最要紧的判断其实是这两个：这个月的额度够不够撑到重置、花得比时间快还是慢。
 * 单看一个 42% 答不了 —— 账期刚过 10% 时的 42% 和过了 90% 时的 42% 是两种局面。
 */
export interface SpendPace {
  /** 账期总天数与已走过的天数（至少算 1 天，头一天的日均才不会除零爆掉）。 */
  totalDays: number;
  elapsedDays: number;
  /** 本期日均花费，美分。 */
  perDay: number;
  /** 照当前日均走到账期末的预计总花费。 */
  projected: number;
  /** 额度上限；没有就 null，下面几项也跟着 null。 */
  budget: number | null;
  /** 还剩多少订阅额度，≥ 0：Cursor 把 included 截顶在 limit，超出的走按需，不在这里。 */
  remaining: number | null;
  /** 花费进度 − 时间进度，百分点。正 = 花得比时间快。 */
  aheadPct: number | null;
  /** 照日均还能撑几天；额度已尽为 0；没花过或没额度为 null。 */
  runwayDays: number | null;
}

export function spendPace(usage?: AccountUsage | null, now = Date.now()): SpendPace | null {
  if (!usage?.cycleStart || !usage.cycleEnd || usage.cycleEnd <= usage.cycleStart) return null;
  // 节奏看的是订阅额度被吃掉的速度：免费加量不占额度，按需另有一格。
  const spend = planSpend(usage);
  if (spend == null || !Number.isFinite(spend)) return null;
  const totalDays = (usage.cycleEnd - usage.cycleStart) / DAY_MS;
  const elapsedDays = Math.max(1, Math.min(totalDays, (now - usage.cycleStart) / DAY_MS));
  const perDay = spend / elapsedDays;
  const projected = perDay * totalDays;
  const budget = planBudget(usage);
  if (budget == null) {
    return { totalDays, elapsedDays, perDay, projected, budget: null, remaining: null, aheadPct: null, runwayDays: null };
  }
  // included 被 Cursor 截顶在 limit，所以这里不会出现负数；真超出的部分在 onDemand.used 里。
  const remaining = Math.max(0, budget - spend);
  const aheadPct = (spend / budget) * 100 - (elapsedDays / totalDays) * 100;
  const runwayDays = remaining <= 0 ? 0 : perDay > 0 ? remaining / perDay : null;
  return { totalDays, elapsedDays, perDay, projected, budget, remaining, aheadPct, runwayDays };
}

/** 「12 天」「1.5 天」「不到 1 天」：节奏那几格里的天数。 */
export function daysText(days: number): string {
  if (!Number.isFinite(days)) return "—";
  if (days < 1) return "不到 1 天";
  if (days < 10) return `${(Math.round(days * 2) / 2).toString().replace(/\.0$/, "")} 天`;
  return `${Math.round(days)} 天`;
}

/** 按需计费：`null` + enabled 是「不封顶」，不是「没有数据」，两者得分清楚。 */
export function onDemandText(usage?: AccountUsage | null): { value: string; sub: string } {
  if (!usage?.onDemandEnabled) return { value: money(usage?.onDemandUsedCents ?? 0), sub: "未开启" };
  const used = money(usage.onDemandUsedCents ?? 0);
  if (usage.onDemandLimitCents == null) return { value: used, sub: "不封顶" };
  return { value: used, sub: `上限 ${money(usage.onDemandLimitCents)}` };
}

/**
 * 卡片末行那一句按需计费，比抽屉里的 `onDemandText` 短一档。
 *
 * 卡片收窄到 320-420px 之后末行只有一行的宽度，所以上限省掉分位（`$500` 而不是
 * `$500.00`）。但「没开」这一半仍然要说 —— 额度用完之后还扣不扣钱，正是用户想确认的事。
 *
 * 拆成三段而不是一整句：末行上面那行是「标签弱、值强」的 `Bot 2 天后重置`，
 * 这行整句一个灰度的话，两行就不押韵了。
 */
export function onDemandParts(usage?: AccountUsage | null): { k: string; v?: string; sub?: string } {
  if (!usage?.onDemandEnabled) return { k: "按需未开启" };
  const used = money(usage.onDemandUsedCents ?? 0);
  if (usage.onDemandLimitCents == null) return { k: "按需", v: used, sub: "不封顶" };
  return { k: "按需", v: `${used} / ${money(usage.onDemandLimitCents, 0)}` };
}

/* ── Bot 通道 ─────────────────────────────────────────────────────────────── */

/** Cursor 的阻断原因是 `SAND_ACCESS_BLOCK_REASON_XXX` 这种长枚举，原样摆进卡片会撑爆布局。 */
export function blockReasonText(raw?: string | null): string {
  if (!raw) return "";
  return raw.replace(/^SAND_ACCESS_BLOCK_REASON_/, "").toLowerCase().replace(/_/g, " ");
}

/**
 * 这个号**出了什么问题** —— 没问题就什么都不说。
 *
 * 以前这里还会返回「额度将尽 / 额度过半 / 可用」：
 *   - 「可用」是废话。一列账号里绝大多数都可用，每张卡都挂一个绿标签，等于没标。
 *   - 「额度将尽」是个说不清的结论。一个号有四个额度桶（Bot 周额 / 总额度 / Auto / API），
 *     哪个算「将尽」、几个满了才算，怎么定都不对。**把四个桶的数字直接摆出来**，
 *     比归纳成一个词有用得多（见 `allBuckets`）。
 *
 * 所以这里只留真正需要人动手的事：号能不能用、Bot 通道有没有被卡。
 */
export function accountProblem(
  account: { availability: Availability },
  usage?: AccountUsage | null,
): { tone: "warn" | "bad"; label: string } | null {
  // 凭证那一维只认 Rust 算好的 availability：卡片、筛子、抽屉说的是同一个词。
  if (account.availability === "dead") return { tone: "bad", label: "已失效" };
  if (account.availability === "logged_out") return { tone: "warn", label: "掉登录" };
  if (account.availability === "api_key") return { tone: "warn", label: "仅 API Key" };

  const bot = usage?.bot;
  if (bot?.access === "blocked") return { tone: "bad", label: "Bot 无权限" };
  if (bot?.hasAvailable === false) return { tone: "bad", label: "Bot 已耗尽" };
  return null;
}

/**
 * 「这个号整体还行不行」看哪个数：**月账期总额度**。
 *
 * 不看「最满的那个桶」。Auto 和 API 是分桶计量的，API 打满只是点名调用 claude / gpt
 * 这类模型不动了，Auto 那条路还通着 —— 号还能用。实测一批号常常是 API 常年 100%
 * 而总额度才 15%，按「最紧的桶」判会把整池子都标成红的，那个分级就没有信息量了。
 *
 * 分桶的详情不会因此丢失：四列各自的数字和颜色就在卡上，API 满了那一列自己是红的。
 * 总额度缺数时（老档偶尔没有）退回最紧的桶，总比什么都不说强。
 */
export function overallPercent(usage?: AccountUsage | null): number | null {
  if (!usage) return null;
  const total = usage.totalPercentUsed;
  if (total != null && Number.isFinite(total)) return total;
  return worstBucket(usage)?.percent ?? null;
}

/**
 * 卡片上那颗状态点的颜色，也是池子分档（正常 / 告警 / 已满）的依据。
 *
 * 它不是一个新结论，只是把 `overallPercent` 的颜色摆到点上，让人扫一列时不用逐个读数字。
 * 阈值与额度条本身一致（`meterColor`），所以点和条永远说同一件事。
 */
export function railTone(
  account: { availability: Availability },
  usage?: AccountUsage | null,
): "ok" | "warn" | "bad" | "none" {
  const problem = accountProblem(account, usage);
  if (problem) return problem.tone;
  const pct = overallPercent(usage);
  if (pct == null) return "none";
  if (pct > 90) return "bad";
  if (pct >= 70) return "warn";
  return "ok";
}

/** 一个额度桶。四个桶的重置周期不一样，所以重置时刻要跟着桶走。 */
export interface Bucket {
  key: "bot" | "total" | "auto" | "api";
  label: string;
  percent: number;
  /** 这个桶什么时候重置，epoch ms。Bot 按周，其余按月账期。 */
  resetAt?: number | null;
}

/** 四个桶，缺数的用 -1 占位，调用方自己过滤。 */
function buckets(usage: AccountUsage): Bucket[] {
  const cycle = usage.cycleEnd;
  return [
    { key: "bot", label: "Bot 周额", percent: usage.bot?.percentUsed ?? -1, resetAt: usage.bot?.resetAt },
    { key: "total", label: "总额度", percent: usage.totalPercentUsed ?? -1, resetAt: cycle },
    { key: "auto", label: "Auto", percent: usage.autoPercentUsed ?? -1, resetAt: cycle },
    { key: "api", label: "API", percent: usage.apiPercentUsed ?? -1, resetAt: cycle },
  ];
}

/**
 * 四个桶**全给**，缺数的 `percent` 为 null。
 *
 * 列表上不再只挑一个桶显示：Auto 和 API 是分开计量的，任一打满那类模型就停了，
 * 而「总额度」看着还宽裕。四个数字并排摆出来，各列上下对齐，扫一列就能比。
 */
export interface BucketView {
  key: Bucket["key"];
  /** 列头上那个词。短，四列并排要放得下。 */
  label: string;
  /** 悬停时的全称与解释。 */
  hint: string;
  percent: number | null;
  resetAt?: number | null;
  /** 有数也说不清的情形：没有这个通道、被封、已耗尽。 */
  note?: string;
}

const BUCKET_HINT: Record<Bucket["key"], string> = {
  bot: "Bot 通道的周额度。按周重置，与月账期分开计量",
  total: "月账期包含额度的整体已用比例",
  auto: "Auto 桶：composer / grok 等由 Cursor 调度的模型",
  api: "API 桶：点名调用的 claude / gpt 等，打满后这类模型调不动",
};

const BUCKET_LABEL: Record<Bucket["key"], string> = {
  bot: "Bot",
  total: "总额度",
  auto: "Auto",
  api: "API",
};

export function allBuckets(usage?: AccountUsage | null): BucketView[] {
  if (!usage) return [];
  const bot = usage.bot;
  const botView: BucketView = {
    key: "bot",
    label: BUCKET_LABEL.bot,
    hint: BUCKET_HINT.bot,
    percent: null,
    resetAt: bot?.resetAt,
  };
  if (!bot) botView.note = "无";
  else if (bot.access === "blocked") botView.note = "无权限";
  else if (bot.hasAvailable === false) {
    botView.note = "已耗尽";
    botView.percent = 100;
  } else botView.percent = bot.percentUsed ?? null;

  return [
    botView,
    ...buckets(usage)
      .filter((b) => b.key !== "bot")
      .map((b) => ({
        key: b.key,
        label: BUCKET_LABEL[b.key],
        hint: BUCKET_HINT[b.key],
        percent: b.percent >= 0 ? b.percent : null,
        resetAt: b.resetAt,
      })),
  ];
}

/**
 * 卡片上摆哪几个桶：Bot / Auto / API，**不含总额度**。
 *
 * 总额度不是又一个并列的桶，它是「这个号整体还行不行」那个结论（`overallPercent`），
 * 而结论已经由卡片左上那颗状态点在说了 —— `railTone` 就是按它染的色。再画一条同样
 * 粗细的条子，等于把同一件事说两遍，还占掉列表里最贵的一行。
 *
 * 留下的三个才是「这个号现在能不能干活」：Bot 按周单独计量，Auto 或 API 任一打满，
 * 那类模型就调不动了。精确的总额度数字在抽屉里，那是看清一个号时才要的。
 */
export function cardBuckets(usage?: AccountUsage | null): BucketView[] {
  return allBuckets(usage).filter((b) => b.key !== "total");
}

function tightest(list: Bucket[]): Bucket | null {
  const known = list.filter((b) => b.percent >= 0);
  if (known.length === 0) return null;
  return known.reduce((worst, b) => (b.percent > worst.percent ? b : worst));
}

/**
 * 四个桶里最紧的那一个。
 *
 * 决定「这个号还能不能用」的是最满的那个桶，不是总量：Cursor 把额度拆成 Auto / API
 * 分别计量，任一打满那类模型就停了 —— 总量 15% 而 API 97% 的号，答案是「不能用」，
 * 不是「才用了 15%」。
 */
export function worstBucket(usage?: AccountUsage | null): Bucket | null {
  if (!usage) return null;
  return tightest(buckets(usage));
}

/**
 * 月账期三个桶（总额度 / Auto / API）里最紧的那个，**不含 Bot**。
 *
 * 账号卡上 Bot 周额单独一条 —— 它按周重置、跟月账期不是一回事；这一条给月账期，
 * 标签跟着桶走，让人一眼知道是哪个桶在拖后腿。
 */
export function worstMonthlyBucket(usage?: AccountUsage | null): Bucket | null {
  if (!usage) return null;
  return tightest(buckets(usage).filter((b) => b.key !== "bot"));
}

/** Bot 周额那一行的附注：刚重置 / 已耗尽 / 计划名。 */
export function botNote(bot?: BotQuota | null, now = Date.now()): string | null {
  if (!bot) return null;
  if (bot.hasAvailable === false) return "已耗尽";
  if (isFresh(bot.periodStart, now)) return "刚重置";
  return bot.planLabel ?? null;
}
