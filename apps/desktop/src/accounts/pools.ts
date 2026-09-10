/**
 * 一个账号「在哪儿被用着」：切号池里有没有它、网关号池里有没有它。
 *
 * 账号总库是一份，切号池与网关池是用户明确挑出来的两个子集（ARCHITECTURE §5.2）。以前只有
 * 走到那两页才知道一个号进了没进；现在账号卡和抽屉直接说。这里只做纯的关联与一个
 * 拉两份名单的 hook，加入 / 移出仍走各自原有的命令。
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { gateway as gatewayApi, switcher as switcherApi } from "../ipc/api";
import type { GatewayStatus, SwitchProfile } from "../ipc/types";

/**
 * 在网关号池里的处境：
 * - `current`   名单里，且正在接力；
 * - `enrolled`  名单里，等着接力；
 * - `skipped`   名单里，但此刻拿不到凭证，接力时跳过；
 * - `available` 不在名单里，但网关能用它（有凭证）；
 * - `none`      不在名单里，也用不了（没授权 / 已失效）。
 */
export type GatewayMembership = "current" | "enrolled" | "skipped" | "available" | "none";

export interface PoolMembership {
  /** 切号池里的那一档；没进池是 null。 */
  switcher: SwitchProfile | null;
  gateway: GatewayMembership;
}

export function poolMembership(
  email: string,
  profiles: SwitchProfile[],
  gateway: GatewayStatus | null,
): PoolMembership {
  const key = email.trim().toLowerCase();
  const profile = profiles.find((p) => p.email.toLowerCase() === key) ?? null;
  let g: GatewayMembership = "none";
  if (gateway) {
    const lane = gateway.lane;
    const candidate = lane.candidates.find((c) => c.label.toLowerCase() === key);
    if (candidate) g = candidate.state.kind === "current" ? "current" : "enrolled";
    else if (lane.missing.some((m) => m.toLowerCase() === key)) g = "skipped";
    else if (lane.available.some((a) => a.label.toLowerCase() === key)) g = "available";
  }
  return { switcher: profile, gateway: g };
}

/** 进了网关名单（不管此刻能不能用）。 */
export function inGatewayRoster(m: GatewayMembership): boolean {
  return m === "current" || m === "enrolled" || m === "skipped";
}

export const GATEWAY_MEMBERSHIP_LABEL: Record<GatewayMembership, string> = {
  current: "正在接力",
  enrolled: "已加入",
  skipped: "已加入 · 接力时跳过",
  available: "未加入",
  none: "未加入",
};

/**
 * 按「这个号被谁用着」筛账号库。
 *
 * 和按额度状态筛（`AccountFilter`）是两个互不相干的问题，所以是两个筛子不是一个：
 * 「切号池里那些号还剩多少额度」这种问题要两个条件一起下。
 * `unpooled` 是两个池都没进的 —— 那些号买回来就一直躺着，值得能一眼捞出来。
 */
export type PoolFilter = "any" | "switcher" | "gateway" | "unpooled";

export const POOL_FILTER_LABEL: Record<PoolFilter, string> = {
  any: "所在池：全部",
  switcher: "在切号池",
  gateway: "在网关",
  unpooled: "未入池",
};

export function matchesPoolFilter(m: PoolMembership, filter: PoolFilter): boolean {
  switch (filter) {
    case "any":
      return true;
    case "switcher":
      return m.switcher != null;
    case "gateway":
      return inGatewayRoster(m.gateway);
    case "unpooled":
      return m.switcher == null && !inGatewayRoster(m.gateway);
  }
}

export interface Pools {
  profiles: SwitchProfile[];
  gateway: GatewayStatus | null;
  /** 首轮还没回来。 */
  loading: boolean;
  reload: () => Promise<void>;
  membership: (email: string) => PoolMembership;
}

/**
 * 两份名单一次拉齐。拉不到就当空：这只影响几个徽章和一块「所在池」，不值得把整页顶成错误态。
 */
export function usePools(): Pools {
  const [profiles, setProfiles] = useState<SwitchProfile[]>([]);
  const [gateway, setGateway] = useState<GatewayStatus | null>(null);
  const [loading, setLoading] = useState(true);

  const reload = useCallback(async () => {
    const [p, g] = await Promise.allSettled([switcherApi.list(), gatewayApi.status()]);
    setProfiles(p.status === "fulfilled" ? p.value : []);
    setGateway(g.status === "fulfilled" ? g.value : null);
    setLoading(false);
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  return useMemo(
    () => ({
      profiles,
      gateway,
      loading,
      reload,
      membership: (email: string) => poolMembership(email, profiles, gateway),
    }),
    [profiles, gateway, loading, reload],
  );
}
