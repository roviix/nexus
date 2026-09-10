/**
 * 「试一下 / 发送测试」的一次流式运行：发出去、逐帧收字、算首字与总耗时。
 *
 * 接入页的测试和模型广场的试用抽屉走的是同一条路（`gateway_try`），所以状态机也只写一份。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { errorText } from "../ipc/api";
import { tryRun, type TryUsage } from "../ipc/models";

export interface Run {
  id: string;
  model: string;
  prompt: string;
  startedAt: number;
  firstByteAt?: number;
  endedAt?: number;
  /** 上游实际路由到的模型（`auto` 时才和 `model` 不同）。 */
  routed?: string;
  text: string;
  thinking: string;
  finish?: string | null;
  usage?: TryUsage | null;
  /** 流内错误：上游出错，流已终结。 */
  error?: string;
  /** 连流都没进：口令被拒、地址不通、没开网关。 */
  failed?: string;
}

export interface RunStats {
  /** 总耗时，秒，一位小数。 */
  total: string;
  /** 首字延迟。 */
  ttft: string;
}

export function useTryRun() {
  const [run, setRun] = useState<Run | null>(null);
  const runRef = useRef<Run | null>(null);

  const commit = (next: Run | null) => {
    runRef.current = next;
    setRun(next);
  };

  // 事件只认自己这一次的 id：连点两次，上一轮尾巴上的字不会串进来。
  //
  // `listen` 是异步拿到解绑函数的。effect 的清理若在它 resolve 之前跑（StrictMode 的
  // 挂载-卸载-再挂载就是这样），得记下「已经不要了」，等它到手立刻解绑——否则第一个
  // 监听器永远活着，每个 delta 被记两遍。
  useEffect(() => {
    let alive = true;
    let unlisten: (() => void) | undefined;
    void tryRun
      .listen((f) => {
        const cur = runRef.current;
        if (!cur || f.id !== cur.id) return;
        const next: Run = { ...cur };
        if (f.kind === "routed") next.routed = f.model;
        if (f.kind === "delta") {
          next.text += f.text;
          next.firstByteAt ??= Date.now();
        }
        if (f.kind === "thinking") {
          next.thinking += f.text;
          next.firstByteAt ??= Date.now();
        }
        if (f.kind === "done") {
          next.finish = f.finish;
          next.usage = f.usage ?? next.usage;
          next.endedAt = Date.now();
        }
        if (f.kind === "usage") next.usage = f.usage;
        if (f.kind === "error") {
          next.error = f.message;
          next.endedAt = Date.now();
        }
        commit(next);
      })
      .then((u) => {
        if (alive) unlisten = u;
        else u();
      });
    return () => {
      alive = false;
      unlisten?.();
    };
  }, []);

  const start = useCallback(async (model: string, prompt: string) => {
    if (runRef.current && !runRef.current.endedAt) return;
    const id = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
    commit({ id, model, prompt, startedAt: Date.now(), text: "", thinking: "" });
    try {
      await tryRun.start(id, model, prompt);
      // 流走完但没收到 done（比如只发了 [DONE]）：按结束处理。
      const cur = runRef.current;
      if (cur && cur.id === id && !cur.endedAt) commit({ ...cur, endedAt: Date.now() });
    } catch (e) {
      const cur = runRef.current;
      if (cur && cur.id === id) commit({ ...cur, failed: errorText(e), endedAt: Date.now() });
    }
  }, []);

  const reset = useCallback(() => commit(null), []);

  const stats = useMemo<RunStats | null>(() => {
    if (!run?.endedAt) return null;
    return {
      total: ((run.endedAt - run.startedAt) / 1000).toFixed(1),
      ttft: run.firstByteAt ? `${run.firstByteAt - run.startedAt} ms` : "—",
    };
  }, [run]);

  return { run, busy: Boolean(run && !run.endedAt), start, reset, stats };
}
