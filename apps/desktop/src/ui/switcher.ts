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
 * 能不能把这个号的登录态写进 Cursor。**必须有 refresh**，和 Rust 侧
 * `Account::can_write_cursor_login` 同口径。
 *
 * 仅会话的号曾经也放行，代价是要拿 access 去占 `cursorAuth/refreshToken` 那一格——
 * Cursor 拿这个假 refresh 续期必然 401 然后掉登录，而那批号（没密码、接不了验证码）
 * 掉了就找不回来。它们该走 CRSR 通道 / 网关用额度，那两条都不写登录态。
 */
export function canAddToSwitchPool(account: Account): boolean {
  if (account.status === "dead") return false;
  return account.hasRefresh;
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
