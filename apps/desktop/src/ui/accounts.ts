/**
 * 「我的账号」列表的筛选 / 搜索 / 排序。纯函数，页面只负责把结果画出来。
 *
 * 手里几十个号时，用户来这一页多半带着一个具体问题：「哪个还能用」「哪个余量最多」
 * 「哪个快重置了」。这些答案不该靠肉眼逐行比，所以排序按这几个问题来设。
 */
import type { Account } from "../ipc/types";
import { planGroup, railTone, type PlanGroup } from "./usage";

/**
 * 一个号在池子里的状态。
 *
 * 分档的依据是**最紧的那个额度桶**（与卡片上那个圆点、与额度条的配色同一套阈值），
 * 凭证出了问题的（失效 / 待登录 / Bot 被封）按严重程度并进 告警 / 已满 ——
 * 对「现在还能不能派上用场」这个问题来说，没额度和用不了是同一类答案。
 *
 * `unknown` 是「还没查过用量」：它不该混进「正常」里冒充一个已核实的号。
 */
export type PoolState = "ok" | "warn" | "full" | "unknown";

export const POOL_LABEL: Record<PoolState, string> = {
  ok: "正常",
  warn: "告警",
  full: "已满",
  unknown: "未查",
};

export function poolState(a: Account): PoolState {
  const tone = railTone(a, a.usage);
  if (tone === "bad") return "full";
  if (tone === "warn") return "warn";
  if (tone === "ok") return "ok";
  return "unknown";
}

export type AccountFilter = "all" | PoolState;

/* ── 按档筛选 ─────────────────────────────────────────────────────────────── */

/** `paid` = 除 Free / 未知之外的所有档：「把 Free 去掉」是最常下的一个筛子，不该让人逐档点。 */
export type PlanFilter = "all" | "paid" | PlanGroup;

export const PLAN_FILTER_LABEL: Record<PlanFilter, string> = {
  all: "全部档位",
  paid: "付费档",
  ultra: "Ultra",
  proplus: "Pro+",
  pro: "Pro",
  team: "Team",
  free: "Free",
  unknown: "未知",
};

/** 下拉里的固定顺序：从高到低，「未知」垫底 —— 和卡片徽章的配色同一套次序感。 */
export const PLAN_FILTER_ORDER: PlanGroup[] = ["ultra", "proplus", "pro", "team", "free", "unknown"];

/** 一个号归哪一档：用量里的 plan 优先，其次入库时记下的 membership —— 与卡片徽章同一口径。 */
export function accountPlanGroup(a: Account): PlanGroup {
  return planGroup(a.usage?.plan ?? a.membership ?? "");
}

export function isPaidPlan(g: PlanGroup): boolean {
  return g !== "free" && g !== "unknown";
}

export function applyPlanFilter(list: Account[], filter: PlanFilter): Account[] {
  if (filter === "all") return list;
  if (filter === "paid") return list.filter((a) => isPaidPlan(accountPlanGroup(a)));
  return list.filter((a) => accountPlanGroup(a) === filter);
}

/* ── 凭证 ─────────────────────────────────────────────────────────────────── */

/** 手上那把 access 还没过期（留 60 秒余量，与 Rust 侧 `session_expired` 同口径）。 */
export function hasLiveAccess(a: Pick<Account, "hasAccess" | "accessExpiresAt">, now = Date.now()): boolean {
  if (!a.hasAccess || !a.accessExpiresAt) return false;
  const at = Date.parse(a.accessExpiresAt);
  return Number.isFinite(at) && at > now + 60_000;
}

/** 只靠 session token 撑着、没有 refresh 的号。到期就得重新粘一份。 */
export function sessionOnly(a: Pick<Account, "hasRefresh" | "hasAccess">): boolean {
  return !a.hasRefresh && a.hasAccess;
}

/**
 * 能不能拿到一把会话去干活（刷用量、进网关、换 Grok 额度）。有 refresh 永远行；
 * 没 refresh 就看 access 还活着没。与 Rust 侧 `Account::can_query_usage` 同口径。
 */
export function canQueryUsage(a: Account, now = Date.now()): boolean {
  return a.hasRefresh || hasLiveAccess(a, now);
}

/**
 * 凭证状态是独立于额度的一维：一个 Pro 号额度满满，refresh token 掉了照样进不了池、刷不了用量。
 * 分布条把这类问题并进「告警 / 已满」是为了回答「现在能不能用」；这里单拆出来是为了回答
 * 「哪些号要我动手」——两个问题都常问，所以两种切法都留着。
 *
 * `session` 是「仅会话」：此刻能用，但没有 refresh、到期就掉——它既不是「已授权」（那意味着长期），
 * 也还不是「掉授权」，单列一档让人知道这批号要盯着到期。
 */
export type CredFilter = "any" | "authorized" | "session" | "needsAuth" | "dead";

export const CRED_FILTER_LABEL: Record<CredFilter, string> = {
  any: "凭证：全部",
  authorized: "已授权",
  session: "仅会话",
  needsAuth: "掉授权",
  dead: "已失效",
};

export const CRED_FILTERS: CredFilter[] = ["any", "authorized", "session", "needsAuth", "dead"];

export function credState(a: Account, now = Date.now()): Exclude<CredFilter, "any"> {
  if (a.status === "dead") return "dead";
  if (a.hasRefresh && a.status !== "needs_login") return "authorized";
  if (sessionOnly(a) && hasLiveAccess(a, now)) return "session";
  return "needsAuth";
}

export function applyCredFilter(list: Account[], filter: CredFilter): Account[] {
  if (filter === "any") return list;
  return list.filter((a) => credState(a) === filter);
}

/**
 * 四种排序。曾经还有「按状态」和「余量最多」，都撤了：状态那一维已经由分布条 + 凭证筛子
 * 回答（筛出来比排出来直接）；「余量最多」按最紧的桶排，可卡上并排摆着三个桶的数字，
 * 排出来的第一名常常不是人眼看到的「最空的那张」，解释不清的排序不如没有。
 */
export type AccountSort = "added" | "reset" | "botReset" | "checked";

/**
 * 默认按**加入的先后**排。
 *
 * 这一条比它看起来重要：列表的顺序是用户的肌肉记忆，「第三个是我那个主力号」这种
 * 认知一旦被打乱，每次回到这一页都得重新找。而其余几种排序的依据（重置时刻、检查时间）
 * **都会被一次刷新用量改写** —— 那意味着点一下刷新，整列就重排了。
 * 所以它们只作为可选项，默认永远是那个不会动的。
 */
export const DEFAULT_SORT: AccountSort = "added";

export const SORT_LABEL: Record<AccountSort, string> = {
  added: "添加时间",
  reset: "月账期最快重置",
  botReset: "Bot 最快重置",
  checked: "最近查过",
};

export function applyFilter(list: Account[], filter: AccountFilter): Account[] {
  if (filter === "all") return list;
  return list.filter((a) => poolState(a) === filter);
}

/** 邮箱、备注、标签都搜；大小写不敏感。空串匹配一切。 */
export function matchesQuery(a: Account, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  if (a.email.toLowerCase().includes(q)) return true;
  if ((a.note ?? "").toLowerCase().includes(q)) return true;
  return a.tags.some((t) => t.toLowerCase().includes(q));
}

/**
 * 排序。
 *
 * - 添加时间：先加的在前。**新号追加在末尾，已有的一个都不动** —— 这正是它当默认的理由。
 * - 月账期最快重置：`cycleEnd` 越近越靠前；不知道的排最后。**只看月账期，不看「最紧的桶」**：
 *   以前按最紧的桶的重置时刻排，可 Bot 桶按周、其余按月，一个号最紧的是 Bot、另一个最紧的是 API，
 *   两个号就在拿周和月比 —— 卡上写着「月账期 20 天后重置」的排到了「3 天后」的前面，看着就是排错了。
 * - Bot 最快重置：**只看 Bot 周额**那一个桶。它按周重置、跟月账期分开计量，
 *   「哪个号的 Bot 快回来了」是个独立的问题，所以单独一档。
 * - 最近查过：lastCheckedAt 越新越靠前；没查过的排最后。
 *
 * 同分时按 id 兜底，**不按后端给的顺序**：后端是 `ORDER BY updated_at DESC`，刷一次
 * 用量它就变了，拿它当兜底等于把「排序不动」的承诺又漏掉。
 */
export function sortAccounts(list: Account[], by: AccountSort, now = Date.now()): Account[] {
  const keyed = list.map((a) => ({ a, k: sortKey(a, by, now) }));
  keyed.sort((x, y) => {
    // 失效的号在「重置 / 查过」里垫底：它的额度再快回来也用不上，摆在头一个是误导。
    // 但「添加时间」里不挪 —— 那一档承诺的就是顺序不变，一个号今天失效了也不该换位置。
    if (by !== "added") {
      const dx = deadRank(x.a) - deadRank(y.a);
      if (dx !== 0) return dx;
    }
    if (x.k !== y.k) return x.k - y.k;
    return x.a.id < y.a.id ? -1 : x.a.id > y.a.id ? 1 : 0;
  });
  return keyed.map((k) => k.a);
}

function deadRank(a: Account): number {
  return a.status === "dead" ? 1 : 0;
}

/** 越小越靠前。`Infinity` 表示「没有这项数据」，自然沉底。 */
function sortKey(a: Account, by: AccountSort, now: number): number {
  switch (by) {
    case "added": {
      const t = Date.parse(a.createdAt);
      return Number.isFinite(t) ? t : Number.POSITIVE_INFINITY;
    }
    case "reset": {
      const at = a.usage?.cycleEnd;
      if (at == null || !Number.isFinite(at)) return Number.POSITIVE_INFINITY;
      // 已经过了重置点的（用量数据旧了），视为「现在就是新的」，排最前 —— 卡上也写着「已重置」。
      return Math.max(0, at - now);
    }
    case "botReset": {
      // 没有 Bot 通道、或没拿到重置时刻的沉底：它们回答不了「Bot 什么时候回来」。
      const at = a.usage?.bot?.resetAt;
      if (at == null || !Number.isFinite(at)) return Number.POSITIVE_INFINITY;
      return Math.max(0, at - now);
    }
    case "checked": {
      const t = a.lastCheckedAt ? Date.parse(a.lastCheckedAt) : Number.NaN;
      return Number.isFinite(t) ? -t : Number.POSITIVE_INFINITY;
    }
  }
}

export interface AccountSummary {
  total: number;
  /** 各档的数量。四档相加必然等于 total —— 顶部那条分布条就是照它画的。 */
  by: Record<PoolState, number>;
  /** 此刻能拼出会话、能刷用量的号（有 refresh，或仅会话且没过期）。 */
  refreshable: number;
}

export function summarize(list: Account[]): AccountSummary {
  const by: Record<PoolState, number> = { ok: 0, warn: 0, full: 0, unknown: 0 };
  let refreshable = 0;
  for (const a of list) {
    by[poolState(a)] += 1;
    if (canQueryUsage(a)) refreshable += 1;
  }
  return { total: list.length, by, refreshable };
}
