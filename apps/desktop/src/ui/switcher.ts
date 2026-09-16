import type { Account, SwitchProfile } from "../ipc/types";
import { hasLiveAccess } from "./accounts";

export interface SwitchPoolEntry {
  key: string;
  email: string;
  account: Account | null;
  profile: SwitchProfile;
  isCurrent: boolean;
}

/**
 * 池子的排序。目前只有一档：**切号时间倒序** —— 最近切过的在最前，从没切过的垫底。
 * 正在用的那个号永远置顶：它就是最近一次切过去的，与倒序同向，不算破例。
 * 工具条上那个下拉此刻是在「说」顺序而不是「选」顺序；加第二档时让
 * `buildSwitchPool` 吃下这个参数，控件就成真了。
 */
export type SwitchSort = "switched";

export const SWITCH_SORT_LABEL: Record<SwitchSort, string> = {
  switched: "切号时间",
};

export const DEFAULT_SWITCH_SORT: SwitchSort = "switched";

/**
 * 能不能加进切号池 / 一键切进 Cursor：有 refresh，**或**手上的 access JWT 还活着。
 *
 * 三类都能切，只是写盘前那一步不同（后端 `accounts_add_to_switch_book` 处理，前端只管放行）：
 * - 有 refresh：换一把新鲜 session 再写；
 * - `type=session` 仅会话号：同一把 JWT 写进两格（Cursor 续期后的盘上稳态）；
 * - `type=web` 仅会话号：先走一次官方 `loginDeepControl` 把 web 换成 session（无密码、无验证码、
 *   不掉原会话），号顺带升级成长期号——所以这里放行 web，切号时自动转换，不用碰 crsr。
 *
 * 只有 dead / 过期的号不放行。
 */
export function canAddToSwitchPool(account: Account, now = Date.now()): boolean {
  if (account.status === "dead") return false;
  return account.hasRefresh || hasLiveAccess(account, now);
}

/** 这个号切号时要先做一次 web→session 转换（活着的 web-only 号）。给界面提示「首次切号会多花几秒」。 */
export function switchNeedsWebConversion(account: Account, now = Date.now()): boolean {
  return (
    account.status !== "dead" &&
    !account.hasRefresh &&
    hasLiveAccess(account, now) &&
    account.accessTokenType === "web"
  );
}

/**
 * 能不能给这个号铸一把长期 `crsr_`：要一把此刻拿得出的会话，且还没有 key。
 *
 * 这是仅会话号唯一的保命动作，所以判据跟着 `hasLiveAccess` 走而不是 `hasRefresh`——
 * 恰恰是没有 refresh 的号最需要它。
 */
export function canMintApiKey(account: Account, now = Date.now()): boolean {
  if (account.status === "dead" || account.hasApiKey) return false;
  return account.hasRefresh || hasLiveAccess(account, now);
}

export function buildSwitchPool(
  profiles: SwitchProfile[],
  accounts: Account[],
  currentEmail?: string | null,
): SwitchPoolEntry[] {
  const accountByEmail = new Map(
    accounts.map((account) => [account.email.toLowerCase(), account]),
  );

  return profiles
    .map((profile): SwitchPoolEntry => {
      const key = profile.email.toLowerCase();
      return {
        key,
        email: profile.email,
        account: accountByEmail.get(key) ?? null,
        profile,
        isCurrent:
          profile.isCurrent ||
          currentEmail?.toLowerCase() === key,
      };
    })
    .sort(compareSwitchPoolEntries);
}

export function listAvailableSwitchAccounts(
  accounts: Account[],
  profiles: SwitchProfile[],
): Account[] {
  const enrolledEmails = new Set(
    profiles.map((profile) => profile.email.toLowerCase()),
  );

  return accounts
    .filter(
      (account) =>
        canAddToSwitchPool(account) &&
        !enrolledEmails.has(account.email.toLowerCase()),
    )
    .sort((left, right) => left.email.localeCompare(right.email));
}

/**
 * 「切号时间倒序」那一档（目前唯一的一档，见 `SwitchSort`）：
 * 当前登录置顶，可切的号按最近切换倒序，从没切过的按邮箱兜住，顺序不乱跳。
 */
function compareSwitchPoolEntries(
  left: SwitchPoolEntry,
  right: SwitchPoolEntry,
): number {
  if (left.isCurrent !== right.isCurrent) {
    return left.isCurrent ? -1 : 1;
  }

  if (left.profile.hasAuth !== right.profile.hasAuth) {
    return left.profile.hasAuth ? -1 : 1;
  }

  const leftSwitchedAt = left.profile.lastSwitchedAt
    ? Date.parse(left.profile.lastSwitchedAt)
    : 0;
  const rightSwitchedAt = right.profile.lastSwitchedAt
    ? Date.parse(right.profile.lastSwitchedAt)
    : 0;

  if (leftSwitchedAt !== rightSwitchedAt) {
    return rightSwitchedAt - leftSwitchedAt;
  }

  return left.email.localeCompare(right.email);
}
