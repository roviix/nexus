import type { Account, AccountUsage } from "../ipc/types";
import { canQueryUsage } from "../ui/accounts";

/**
 * 平台账号的 id。
 *
 * 现在只有 Cursor；以后接 ChatGPT / Claude 时在这里扩可辨识联合，而不是给 Cursor 的
 * `Account` 不断塞别的平台字段。这个 id 是 UI / adapter 边界，不替代后端将来的
 * `(platform, external_id)` 持久化身份。
 */
export type AccountPlatform = "cursor";

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
 * `cursor` 是平台 adapter 给出的完整四桶；`summary` 是网关等运行时只知道一个百分比的
 * 降级形态；`unavailable` 必须带原因，不能把“拿不到”画成 0%。
 *
 * 新平台应增加自己的可辨识分支并由对应 renderer 消费，不要伪造 `AccountUsage`。
 */
export type AccountUsageView =
  | { kind: "cursor"; value: AccountUsage; checkedAt?: string | null }
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

export type AccountView = CursorAccountView;

export interface CursorAccountViewInput {
  label: string;
  managed?: Account | null;
  placement: AccountPlacement;
  fallbackPercentUsed?: number | null;
  unavailableReason?: string;
}

export function createCursorAccountView({
  label,
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
    label: managed?.email ?? label,
    managed,
    usage,
    membership: managed?.usage?.plan ?? managed?.membership ?? null,
    placement,
  };
}

export const ACCOUNT_PLATFORM_LABEL: Record<AccountPlatform, string> = {
  cursor: "Cursor",
};
