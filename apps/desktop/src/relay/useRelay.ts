/**
 * 中转 API 的状态，一次拉齐：本地网关（开没开、几个号）与它的模型目录。
 * 模型广场、接入、概览三页都从这里取，各自决定要哪几样 —— 三页各拉各的，
 * 同一份数据就会在三处长成三个样子。
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { gateway as gatewayApi } from "../ipc/api";
import { models, type LocalModel } from "../ipc/models";
import type { GatewayStatus } from "../ipc/types";

export interface RelayState {
  gateway: GatewayStatus | null;
  local: LocalModel[] | null;
  /** 首轮还没回来。 */
  loading: boolean;
  reload: () => Promise<void>;
  /** 本地网关此刻能不能接请求：开着且至少一个候选号。 */
  localReady: boolean;
}

export interface UseRelayOptions {
  /** 拉模型目录（模型广场、接入要；概览不要）。 */
  catalogs?: boolean;
  /** 网关开着时每隔这么久刷一次状态（接力 / 冷却会随请求变）。0 = 不刷。 */
  pollMs?: number;
}

export function useRelay(opts: UseRelayOptions = {}): RelayState {
  const { catalogs = false, pollMs = 0 } = opts;
  const [gateway, setGateway] = useState<GatewayStatus | null>(null);
  const [local, setLocal] = useState<LocalModel[] | null>(null);
  const [loading, setLoading] = useState(true);

  const reloadLocal = useCallback(async () => {
    const [g, l] = await Promise.allSettled([gatewayApi.status(), catalogs ? models.local() : Promise.resolve(null)]);
    setGateway(g.status === "fulfilled" ? g.value : null);
    if (catalogs) setLocal(l.status === "fulfilled" ? (l.value ?? []) : []);
  }, [catalogs]);

  const reload = useCallback(async () => {
    await reloadLocal();
    setLoading(false);
  }, [reloadLocal]);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    if (!pollMs || !gateway?.running) return;
    const t = window.setInterval(() => void reloadLocal(), pollMs);
    return () => window.clearInterval(t);
  }, [pollMs, gateway?.running, reloadLocal]);

  return useMemo<RelayState>(
    () => ({
      gateway,
      local,
      loading,
      reload,
      localReady: Boolean(gateway?.running && gateway.lane.candidates.length > 0),
    }),
    [gateway, local, loading, reload],
  );
}
