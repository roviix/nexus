import type { Account, AccountUsage, ChatGptAccount, ChatGptUsage, GatewayCandidate } from "../ipc/types";
import { canQueryUsage } from "../ui/accounts";

/**
 * 平台账号的 id。
 *
 * 这是可辨识联合，不是把 Cursor 定成所有平台的基类：接新平台在这里加一支，而不是给
 * Cursor 的 `Account` 不断塞别的平台字段。这个 id 是 UI / adapter 边界，不替代后端的
 * `(platform, external_id)` 持久化身份。
 */
export type AccountPlatform = "cursor" | "chatgpt";

export type AccountPlacementKind = "library" | "overview" | "switcher" | "gateway";

/** 同一个平台账号此刻出现在哪个场景，以及这个场景怎么看它。 */
export interface AccountPlacement {
  kind: AccountPlacementKind;
  label: string;
  detail?: string;
}

/**
 * 卡片能展示的用量。
 *
 * `cursor` 是平台 adapter 给出的完整四桶；`chatgpt` 是 Codex 的两个滚动窗口；
 * `summary` 是网关等运行时只知道一个百分比的降级形态；`unavailable` 必须带原因，
 * 不能把“拿不到”画成 0%。
 *
 * 新平台应增加自己的可辨识分支并由对应 renderer 消费，不要伪造 `AccountUsage`。
 */
export type AccountUsageView =
  | { kind: "cursor"; value: AccountUsage; checkedAt?: string | null }
  | { kind: "chatgpt"; value: ChatGptUsage }
  | { kind: "summary"; label: string; percentUsed: number }
  | { kind: "unavailable"; reason: string };

/**
 * 一个账号在任意页面上的统一视图。
 *
 * `managed` 表示它是否存在于总账号库。切号快照、Cursor 当前登录、网关名单都可能只知道
 * 一个邮箱；这种“未托管账号”仍然是可打开的账号，只是抽屉要明确说哪些能力不可用。
 */
export interface CursorAccountView {
  platform: "cursor";
  key: string;
  label: string;
  managed: Account | null;
  usage: AccountUsageView;
  membership?: string | null;
  placement: AccountPlacement;
}

/**
 * ChatGPT 订阅号。身份是 `chatgpt_account_id`，用量是两个滚动窗口，进不进网关看
 * `enabled` —— 没有切号池。`lane` 是网关这一刻的接力位置，网关没开时为 null。
 */
export interface ChatGptAccountView {
  platform: "chatgpt";
  key: string;
  label: string;
  managed: ChatGptAccount;
  usage: AccountUsageView;
  membership?: string | null;
  placement: AccountPlacement;
  lane: GatewayCandidate | null;
}

export type AccountView = CursorAccountView | ChatGptAccountView;

export interface CursorAccountViewInput {
  label: string;
  /**
   * 卡片上要画的那一行。缺省用托管邮箱。账号页打码时把打过码的字符串从这里传进来——
   * 以前这里被 `managed.email` 盖掉，右上角「隐藏」按了等于没按。
   */
  displayLabel?: string;
  managed?: Account | null;
  placement: AccountPlacement;
  fallbackPercentUsed?: number | null;
  unavailableReason?: string;
}

export function createCursorAccountView({
  label,
  displayLabel,
  managed = null,
  placement,
  fallbackPercentUsed,
  unavailableReason,
}: CursorAccountViewInput): CursorAccountView {
  const normalizedLabel = label.trim().toLowerCase();
  const usage: AccountUsageView = managed?.usage
    ? {
        kind: "cursor",
        value: managed.usage,
        checkedAt: managed.lastCheckedAt,
      }
    : fallbackPercentUsed != null && Number.isFinite(fallbackPercentUsed)
      ? {
          kind: "summary",
          label: "总额度",
          percentUsed: fallbackPercentUsed,
        }
      : {
          kind: "unavailable",
          reason:
            unavailableReason ??
            (managed
              ? canQueryUsage(managed)
                ? "还没查过用量"
                : managed.hasAccess
                  ? "session token 已过期，更新后才能查用量"
                  : "授权后才能查用量"
              : "这个账号未在账号库托管，无法查询完整用量"),
        };

  return {
    platform: "cursor",
    // 未托管账号只能用邮箱做临时 UI identity；真正多平台持久化不能沿用这条规则。
    key: `cursor:${managed ? `managed:${managed.id}` : `external:${normalizedLabel}`}`,
    label: displayLabel ?? managed?.email ?? label,
    managed,
    usage,
    membership: managed?.usage?.plan ?? managed?.membership ?? null,
    placement,
  };
}

export interface ChatGptAccountViewInput {
  account: ChatGptAccount;
  placement?: AccountPlacement;
  lane?: GatewayCandidate | null;
}

export function createChatGptAccountView({
  account,
  placement = { kind: "library", label: "账号库" },
  lane = null,
}: ChatGptAccountViewInput): ChatGptAccountView {
  const usage: AccountUsageView = account.usage
    ? { kind: "chatgpt", value: account.usage }
    : {
        kind: "unavailable",
        reason:
          account.status !== "active" || !account.hasRefresh
            ? "授权后才能查用量"
            : "还没查过用量",
      };

  return {
    platform: "chatgpt",
    key: `chatgpt:managed:${account.id}`,
    label: chatgptLabel(account),
    managed: account,
    usage,
    membership: account.planType,
    placement,
    lane,
  };
}

export const ACCOUNT_PLATFORM_LABEL: Record<AccountPlatform, string> = {
  cursor: "Cursor",
  chatgpt: "ChatGPT",
};

/** 跟人看的名字：邮箱；没有就用 chatgpt_account_id 的尾巴（纯 token 导入时读不到邮箱）。 */
function chatgptLabel(a: Pick<ChatGptAccount, "email" | "accountRef">): string {
  const email = a.email?.trim();
  return email ? email : `chatgpt…${a.accountRef.slice(-6)}`;
}
