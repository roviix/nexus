/**
 * 「我的账号」的视图：一组筛选 + 排序的组合，有名字，记在本机。
 *
 * 号多了以后，来这一页的人通常带着固定的几个问题——「哪些付费号还能派上用场」「哪些号掉了登录
 * 要补」——而回答每个问题都要下同一组筛子、选同一种排序。以前这些状态是 `useState`，关掉页面就
 * 归零，于是每次进来都重做一遍。这里做两件事：
 *
 *  1. 当前的筛选 / 排序落到 localStorage，下次打开还在原地；
 *  2. 常用组合起名存成「视图」，一键切换。内建几个覆盖最常问的问题，用户还可以把当前组合存下来。
 *
 * 搜索词不进视图也不持久化：它是一次性的，记住它只会让人下次进来纳闷「号怎么少了」。
 */
import type { AccountSort, AvailFilter, PlanFilter, QuotaFilter } from "../ui/accounts";
import { DEFAULT_SORT } from "../ui/accounts";
import type { PoolFilter } from "./pools";

export interface ViewSpec {
  /** 可用性（分布条那一维）：此刻能不能用。 */
  avail: AvailFilter;
  /** 额度状态：还剩多少。 */
  quota: QuotaFilter;
  pool: PoolFilter;
  plan: PlanFilter;
  /** 分组（标签）："all" 全部，或具体的分组名称。 */
  tag: string;
  sort: AccountSort;
  /** 看归档的还是没归档的。归档的号平时不出现，这是唯一能看见它们的地方。 */
  archived: boolean;
}

export const DEFAULT_VIEW: ViewSpec = {
  avail: "all",
  quota: "all",
  pool: "any",
  plan: "all",
  tag: "all",
  sort: DEFAULT_SORT,
  archived: false,
};

export interface SavedView {
  id: string;
  label: string;
  spec: ViewSpec;
  /** 内建视图不可删、不可改名。 */
  builtin?: boolean;
}

export const BUILTIN_VIEWS: SavedView[] = [
  { id: "all", label: "全部", spec: DEFAULT_VIEW, builtin: true },
  {
    id: "paid-ready",
    label: "付费可用",
    // 长期可用、非 Free 的号。顺序不动（添加时间）：这批号是「现在该用哪个」的候选，
    // 三个桶的数字并排就在卡上，让人自己比，比一个解释不清的「余量」排序可靠。
    spec: { ...DEFAULT_VIEW, plan: "paid", avail: "long_lived" },
    builtin: true,
  },
  {
    id: "needs-action",
    label: "需处理",
    // 掉登录的号：能修的都在这儿（授权一次 / 粘一份新 token），失效的另有一档，修不了就不混进来。
    spec: { ...DEFAULT_VIEW, avail: "logged_out" },
    builtin: true,
  },
  {
    id: "archived",
    label: "已归档",
    spec: { ...DEFAULT_VIEW, archived: true },
    builtin: true,
  },
];

export function sameSpec(a: ViewSpec, b: ViewSpec): boolean {
  return (
    a.avail === b.avail &&
    a.quota === b.quota &&
    a.pool === b.pool &&
    a.plan === b.plan &&
    a.tag === b.tag &&
    a.sort === b.sort &&
    a.archived === b.archived
  );
}

export function isDefaultView(spec: ViewSpec): boolean {
  return sameSpec(spec, DEFAULT_VIEW);
}

/** 当前组合命中的视图（内建优先）；一个都不命中就是「自定义中」。 */
export function matchView(spec: ViewSpec, saved: SavedView[]): SavedView | null {
  return [...BUILTIN_VIEWS, ...saved].find((v) => sameSpec(v.spec, spec)) ?? null;
}

/* ── 持久化 ─────────────────────────────────────────────────────────────── */

const SPEC_KEY = "nexus.accounts.view";
const SAVED_KEY = "nexus.accounts.savedViews";

const AVAILS = new Set<AvailFilter>(["all", "long_lived", "session", "api_key", "logged_out", "dead"]);
const QUOTAS = new Set<QuotaFilter>(["all", "ok", "warn", "full", "unknown"]);
const POOLS = new Set<PoolFilter>(["any", "switcher", "gateway", "unpooled"]);
const PLANS = new Set<PlanFilter>(["all", "paid", "ultra", "proplus", "pro", "team", "free", "unknown"]);
const SORTS = new Set<AccountSort>(["added", "reset", "botReset", "checked"]);

/** 上一版的凭证筛子怎么对到可用性上。`authorized` 那时的含义就是「有 refresh 且没掉」。 */
const LEGACY_CRED: Record<string, AvailFilter> = {
  any: "all",
  authorized: "long_lived",
  session: "session",
  needsAuth: "logged_out",
  dead: "dead",
};

/**
 * 盘上的值可能来自旧版本；不认识的字段回退默认，别让一个陈旧的枚举把整页筛成空。
 *
 * 上一版的形状是 `{ filter（额度）, pool, plan, cred（凭证）, sort }`：`filter` 对到 `quota`、
 * `cred` 对到 `avail`，存过的视图静默升级。撤掉的排序（`status` / `headroom`）也走这条：落回「添加时间」。
 */
export function normalizeSpec(raw: unknown): ViewSpec {
  const r = (raw ?? {}) as Record<string, unknown>;
  const pick = <T extends string>(v: unknown, ok: Set<T>, dflt: T): T =>
    typeof v === "string" && ok.has(v as T) ? (v as T) : dflt;
  const legacyAvail = typeof r.cred === "string" ? LEGACY_CRED[r.cred] : undefined;
  return {
    avail: pick(r.avail ?? legacyAvail, AVAILS, DEFAULT_VIEW.avail),
    quota: pick(r.quota ?? r.filter, QUOTAS, DEFAULT_VIEW.quota),
    pool: pick(r.pool, POOLS, DEFAULT_VIEW.pool),
    plan: pick(r.plan, PLANS, DEFAULT_VIEW.plan),
    tag: typeof r.tag === "string" ? r.tag : DEFAULT_VIEW.tag,
    sort: pick(r.sort, SORTS, DEFAULT_VIEW.sort),
    archived: r.archived === true,
  };
}

type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;

function storage(): Store | null {
  try {
    return typeof window !== "undefined" ? window.localStorage : null;
  } catch {
    return null;
  }
}

export function loadSpec(store: Store | null = storage()): ViewSpec {
  try {
    const raw = store?.getItem(SPEC_KEY);
    return raw ? normalizeSpec(JSON.parse(raw)) : DEFAULT_VIEW;
  } catch {
    return DEFAULT_VIEW;
  }
}

export function saveSpec(spec: ViewSpec, store: Store | null = storage()): void {
  try {
    if (isDefaultView(spec)) store?.removeItem(SPEC_KEY);
    else store?.setItem(SPEC_KEY, JSON.stringify(spec));
  } catch {
    /* 记不住就记不住，功能照常 */
  }
}

export function loadSavedViews(store: Store | null = storage()): SavedView[] {
  try {
    const raw = store?.getItem(SAVED_KEY);
    if (!raw) return [];
    const list = JSON.parse(raw);
    if (!Array.isArray(list)) return [];
    return list
      .filter((v) => v && typeof v.id === "string" && typeof v.label === "string")
      .map((v) => ({ id: v.id, label: v.label, spec: normalizeSpec(v.spec) }));
  } catch {
    return [];
  }
}

export function persistSavedViews(views: SavedView[], store: Store | null = storage()): void {
  try {
    const own = views.filter((v) => !v.builtin);
    if (own.length === 0) store?.removeItem(SAVED_KEY);
    else store?.setItem(SAVED_KEY, JSON.stringify(own));
  } catch {
    /* 同上 */
  }
}

/** 同名视图覆盖而不是并存：用户改了筛子再存同一个名字，意图就是更新它。 */
export function upsertView(views: SavedView[], label: string, spec: ViewSpec): SavedView[] {
  const name = label.trim();
  const rest = views.filter((v) => v.label !== name);
  const id = `v-${name.toLowerCase().replace(/\s+/g, "-")}`;
  return [...rest, { id, label: name, spec }];
}
