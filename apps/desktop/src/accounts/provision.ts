/**
 * 自动配置：勾哪几步、这一批里各步会摊到几个号、跑完怎么讲。
 *
 * 真正的「该不该跑」判在 Rust（`nexus_accounts::provision::decide`），那里能看到凭证。这里只
 * 做**开跑前的估算**，用的是列表上已有的字段和 `ui/switcher` 里现成的那几个谓词——按下按钮前
 * 得先知道「这 30 个号里有 12 个要换 session」，否则用户只能盲点。估算和后端结论偶尔差一两个
 * （比如 token 在这几秒里过期了）不影响正确性：后端仍然自己判一次，界面照它的报告显示。
 */

import type { Account, ProvisionPlan, ProvisionReport, ProvisionStep } from "../ipc/types";
import { canQueryUsage, canUseDashboard } from "../ui/accounts";
import { canMintApiKey, switchNeedsWebConversion } from "../ui/switcher";

export const PROVISION_STEPS: Array<{
  id: ProvisionStep;
  label: string;
  hint: string;
}> = [
  {
    id: "mintApiKey",
    label: "铸 crsr_ Key",
    hint: "一把长期 API Key。放在换 session 之前当保命绳：万一 token 掉了，查花费、进网关还有这把。仅会话号唯一的保命动作",
  },
  {
    id: "convertSession",
    label: "换桌面 session",
    hint: "把网站 web token 走一次官方登录换成桌面 session + refresh：号从此能续期、能切、能复制给别人用",
  },
  {
    id: "onDemand",
    label: "按需开到不封顶",
    hint: "Spending 页那个开关。Apple 内购的号开不了，团队号只有管理员能改",
  },
  {
    id: "dataRetention",
    label: "开数据保留策略",
    hint: "打开 Fable 5 的数据保留同意（仪表盘 Privacy 那格）。幂等，重复跑也没副作用",
  },
  {
    id: "refreshUsage",
    label: "最后刷一遍用量",
    hint: "配置完再读一次，卡片上看到的才是配完之后的数",
  },
];

/** 进度条上那几枚小胶囊的短名。摆一行里，用不了上面那个完整说法。 */
export const PROVISION_STEP_LABEL: Record<ProvisionStep, string> = {
  mintApiKey: "铸 Key",
  convertSession: "换 session",
  onDemand: "开按需",
  dataRetention: "数据保留",
  refreshUsage: "刷用量",
};

/** 全做、按需不封顶。日常批量入库要的就是这一套。 */
export const DEFAULT_PROVISION_PLAN: ProvisionPlan = {
  mintApiKey: true,
  convertSession: true,
  onDemand: true,
  onDemandLimitCents: null,
  dataRetention: true,
  refreshUsage: true,
};

const STORAGE_KEY = "nexus.accounts.provision";

/** 上次勾的那几项。跟复制弹窗一个规矩：选一次记住，下次进来还是这套。 */
export function loadProvisionPlan(
  storage: Pick<Storage, "getItem"> | null = safeStorage(),
): ProvisionPlan {
  try {
    const raw = storage?.getItem(STORAGE_KEY);
    if (!raw) return DEFAULT_PROVISION_PLAN;
    return normalizePlan(JSON.parse(raw));
  } catch {
    return DEFAULT_PROVISION_PLAN;
  }
}

export function saveProvisionPlan(
  plan: ProvisionPlan,
  storage: Pick<Storage, "setItem"> | null = safeStorage(),
) {
  try {
    storage?.setItem(STORAGE_KEY, JSON.stringify(normalizePlan(plan)));
  } catch {
    // 私密模式 / 配额满：记不住就记不住，不影响这次配置。
  }
}

/** 认不出来的值一律回默认。上限只收正数，0 和负数当「不封顶」。 */
export function normalizePlan(raw: unknown): ProvisionPlan {
  const r = (raw ?? {}) as Partial<Record<keyof ProvisionPlan, unknown>>;
  const bool = (v: unknown, fallback: boolean) => (typeof v === "boolean" ? v : fallback);
  const cents =
    typeof r.onDemandLimitCents === "number" && r.onDemandLimitCents > 0 ? r.onDemandLimitCents : null;
  return {
    mintApiKey: bool(r.mintApiKey, DEFAULT_PROVISION_PLAN.mintApiKey),
    convertSession: bool(r.convertSession, DEFAULT_PROVISION_PLAN.convertSession),
    onDemand: bool(r.onDemand, DEFAULT_PROVISION_PLAN.onDemand),
    onDemandLimitCents: cents,
    dataRetention: bool(r.dataRetention, DEFAULT_PROVISION_PLAN.dataRetention),
    refreshUsage: bool(r.refreshUsage, DEFAULT_PROVISION_PLAN.refreshUsage),
  };
}

const AUTO_KEY = "nexus.accounts.provision.auto";

/**
 * 导入页那个「顺手配置」勾没勾。**默认不勾**。
 *
 * 这几步会去改对方账号上的东西（铸 key 在人家 Dashboard 上留一条记录、按需是笔计费设置），
 * 默认替人做了不合适；勾一次记住，日常批量入库也就勾这一次。
 */
export function loadAutoProvision(storage: Pick<Storage, "getItem"> | null = safeStorage()): boolean {
  try {
    return storage?.getItem(AUTO_KEY) === "1";
  } catch {
    return false;
  }
}

export function saveAutoProvision(on: boolean, storage: Pick<Storage, "setItem"> | null = safeStorage()) {
  try {
    storage?.setItem(AUTO_KEY, on ? "1" : "0");
  } catch {
    // 记不住就记不住，下次再勾一下。
  }
}

export function planIsEmpty(plan: ProvisionPlan): boolean {
  return (
    !plan.mintApiKey &&
    !plan.convertSession &&
    !plan.onDemand &&
    !plan.dataRetention &&
    !plan.refreshUsage
  );
}

export function togglePlanStep(plan: ProvisionPlan, step: ProvisionStep): ProvisionPlan {
  return { ...plan, [step]: !plan[step] };
}

/* ── 开跑前的估算 ────────────────────────────────────────────────────────── */

/**
 * 这一批里每一步会落到几个号上。
 *
 * 按需那一步给不出数：要不要写取决于上游此刻的按需状态，没查过用量的号根本不知道。
 * 与其猜一个数写在界面上，不如返回 `null`，让弹窗说「看情况」。
 */
export function estimateTargets(
  list: readonly Account[],
  now = Date.now(),
): Record<ProvisionStep, number | null> {
  return {
    mintApiKey: list.filter((a) => canMintApiKey(a, now)).length,
    convertSession: list.filter((a) => switchNeedsWebConversion(a, now)).length,
    onDemand: null,
    dataRetention: list.filter((a) => canUseDashboard(a, now)).length,
    refreshUsage: list.filter((a) => canQueryUsage(a, now)).length,
  };
}

/* ── 跑完怎么讲 ──────────────────────────────────────────────────────────── */

/** 一个号的一行结论，如 `铸 Key✓ · 换 session✓ · 开按需✗`。全跳过时说一句「本来就配好了」。 */
export function reportLine(report: ProvisionReport): string {
  const parts = report.steps
    .filter((s) => s.state !== "skipped")
    .map((s) => `${PROVISION_STEP_LABEL[s.step]}${s.state === "done" ? "✓" : "✗"}`);
  return parts.length ? parts.join(" · ") : "本来就配好了";
}

/**
 * 一批跑完的一句话：几个号全好、几个有步骤失败。
 *
 * 报「失败的号数」而不是「失败的步骤数」：用户接着要做的是回去看那几个号，不是统计动作。
 */
export function batchSummary(reports: readonly ProvisionReport[]): string {
  if (!reports.length) return "没有需要配置的账号。";
  const bad = reports.filter(reportFailed);
  if (!bad.length) return `已配置 ${reports.length} 个账号。`;
  return `已配置 ${reports.length} 个账号，其中 ${bad.length} 个有步骤没成功。`;
}

/** 这个号有没有步骤失败。列表里给它一个显眼的标。 */
export function reportFailed(report: ProvisionReport): boolean {
  return report.steps.some((s) => s.state === "failed");
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage;
  } catch {
    return null;
  }
}
