/**
 * 假的 `@tauri-apps/api/event`：一个进程内的事件总线，让 mock-core 能像 Rust 那样往前端推事件
 * （试一下的流式回字就靠它）。
 */
export type UnlistenFn = () => void;

type Handler = (e: { payload: unknown }) => void;
const handlers = new Map<string, Set<Handler>>();

export async function listen<T>(event: string, handler: (e: { payload: T }) => void): Promise<UnlistenFn> {
  const set = handlers.get(event) ?? new Set<Handler>();
  set.add(handler as Handler);
  handlers.set(event, set);
  return () => {
    set.delete(handler as Handler);
  };
}

export function emitMock(event: string, payload: unknown): void {
  for (const h of handlers.get(event) ?? []) h({ payload });
}
