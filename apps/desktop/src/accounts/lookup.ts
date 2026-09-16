/**
 * 批量查找：粘一堆邮箱进来，在库里把它们找出来。
 *
 * 只认邮箱。粘进来的可能是一行一个、逗号隔开、`邮箱----密码` 这种导出清单、甚至一段 JSON ——
 * 都不用先整理，正则把邮箱全部捞出来就行。大小写不敏感，重复的只算一次，顺序按出现先后。
 */

import type { Account } from "../ipc/types";

const EMAIL = /[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}/gi;

/** 从任意文本里捞邮箱：小写、去重、保序。 */
export function parseLookup(text: string): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  // `----` 是清单里邮箱和密码之间的分隔，可域名里也允许 `-`，正则会把 `x.com----P@ss` 一路吃进去。
  // 先把分隔换成空格再捞。
  for (const hit of text.replace(/----+/g, " ").match(EMAIL) ?? []) {
    const email = hit.toLowerCase();
    if (seen.has(email)) continue;
    seen.add(email);
    out.push(email);
  }
  return out;
}

export interface LookupResult<A extends Pick<Account, "email" | "archivedAt">> {
  /** 库里有的，按**粘贴的顺序**排：对着自己的清单一行行核对时顺序不能乱。 */
  found: A[];
  /** 找到的里面已经归档的那几个。 */
  archived: A[];
  /** 清单里有、库里没有的邮箱，按粘贴顺序。 */
  missing: string[];
}

/** 把清单对到账号上。同一邮箱在库里只可能有一条（`email` 唯一），所以直接查表。 */
export function matchLookup<A extends Pick<Account, "email" | "archivedAt">>(
  list: readonly A[],
  emails: readonly string[],
): LookupResult<A> {
  const byEmail = new Map<string, A>();
  for (const a of list) byEmail.set(a.email.toLowerCase(), a);
  const found: A[] = [];
  const missing: string[] = [];
  for (const email of emails) {
    const hit = byEmail.get(email);
    if (hit) found.push(hit);
    else missing.push(email);
  }
  return { found, archived: found.filter((a) => Boolean(a.archivedAt)), missing };
}

/**
 * 粘进搜索框的内容像不像一份清单：两个以上邮箱、或者带换行的多个条目。
 * 像的话就不当作关键字搜，直接转去批量查找 —— 一段清单塞进单行输入框谁也搜不到东西。
 */
export function looksLikeLookupPaste(text: string): boolean {
  return parseLookup(text).length >= 2;
}
