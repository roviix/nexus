/**
 * ChatGPT 账号页签的纯函数：标签、窗口名、接力状态文案。和 `ChatGptAccounts` 分开是为了能单测。
 */
import type { ChatGptAccount, ChatGptUsageWindow, GatewayCandidate } from "../../ipc/types";

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

/** 套餐徽章的样式类。gpt 的套餐名和 Cursor 的不一样，只认三个词。 */
export function planClass(plan: string | null | undefined): string | null {
  if (!plan) return null;
  const p = plan.toLowerCase();
  if (p.includes("pro")) return "plan plan-pro";
  if (p.includes("team")) return "plan plan-team";
  if (p.includes("free")) return "plan plan-free";
  return "plan";
}
