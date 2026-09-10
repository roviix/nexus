/**
 * 进行中的发送，按会话记一份。**活在模块里而不是组件里**：用户发出去一句、切去别的会话
 * 或别的页、再切回来，半截回复还在、命令返回时也还有人负责刷新——组件挂了又卸，这份状态
 * 不跟着丢。整个应用只挂一个 `playground://chat` 监听，按 `requestId` 分发到对应的 run。
 *
 * 回复的**真相在库里**：run 只是「命令还没回来那段时间」的画面。命令一返回（回复已落库）
 * 就标 `done`，视图据此重新拉一遍会话，然后把 run 收掉。
 */
import { useSyncExternalStore } from "react";
import { errorText } from "../../ipc/api";
import type { TryFrame, TryUsage } from "../../ipc/models";
import { newRequestId, playground, type ImageRequest, type Kind, type Message, type VideoRequest } from "../../ipc/playground";
import { toChatAttachments, type ImageAttachment } from "./attachments";

/** 待定气泡里的一张附件缩略图。地址是输入框建的 object URL，run 收掉时释放。 */
export interface RunAttachment {
  id: string;
  name: string;
  url: string;
}

export interface Run {
  requestId: string;
  threadId: string;
  kind: Kind;
  /** 这一轮发出去的那句话；重新生成时为 null（历史里已经有了）。 */
  prompt: string | null;
  /** 这一轮随那句话发上去的图；回复落库后以库里的为准。 */
  attachments: RunAttachment[];
  /** 出图请求的参数，给占位格子按比例画。 */
  image?: ImageRequest;
  /** 生视频请求的参数（不含首帧字节），给占位格子写规格。 */
  video?: Pick<VideoRequest, "duration" | "aspectRatio" | "resolution">;
  text: string;
  thinking: string;
  routed: string | null;
  usage: TryUsage | null;
  startedAt: number;
  firstByteAt?: number;
  /** 流内错误 / 命令抛错，两种都进这里；回复落库后以库里的为准。 */
  error?: string;
  /** 命令已返回：回复已在库里（或命令根本没发出去）。 */
  done: boolean;
  reply?: Message;
}

const runs = new Map<string, Run>();
const subs = new Set<() => void>();
let listening: Promise<void> | null = null;

/** 已排队的那一帧；0 = 没有。 */
let frame = 0;

function notify() {
  for (const s of subs) s();
}

/** 立刻通知，并把已排队的那一帧收掉 —— 否则这次之后还会多刷一遍。 */
function emit() {
  if (frame) {
    cancelAnimationFrame(frame);
    frame = 0;
  }
  notify();
}

/**
 * 流式帧**合并到下一帧**再叫醒 React。
 *
 * 上游一秒能吐上百个 delta，而每次通知都要把整篇累积文本重新过一遍 Markdown 解析
 * （`parseBlocks` 是 `useMemo` 在 `text` 上，可 `text` 每帧都在变）—— 每个 token O(n)、
 * 整轮 O(n²)，长回复的后半段就是这么卡住的。
 *
 * **状态不延迟，只延迟通知**：`runs.set` 是同步做完的，合并期间任何人读到的都是最新值，
 * 不存在丢帧。渲染频率因此封顶在屏幕刷新率上，肉眼看不出差别。
 */
function emitSoon() {
  if (frame) return;
  frame = requestAnimationFrame(() => {
    frame = 0;
    notify();
  });
}

function subscribe(fn: () => void) {
  subs.add(fn);
  return () => {
    subs.delete(fn);
  };
}

/**
 * `soon` 只给流式帧用。开始 / 结束 / 出错这些一轮只发生一次的状态要立刻画出来 ——
 * 它们决定按钮禁不禁用，压一帧就会让人觉得点了没反应。
 */
function patch(threadId: string, f: (r: Run) => Partial<Run>, soon = false) {
  const cur = runs.get(threadId);
  if (!cur) return;
  runs.set(threadId, { ...cur, ...f(cur) });
  if (soon) emitSoon();
  else emit();
}

function apply(run: Run, f: TryFrame): Partial<Run> {
  switch (f.kind) {
    case "routed":
      return { routed: f.model };
    case "delta":
      return { text: run.text + f.text, firstByteAt: run.firstByteAt ?? Date.now() };
    case "thinking":
      return { thinking: run.thinking + f.text, firstByteAt: run.firstByteAt ?? Date.now() };
    case "done":
      return { usage: f.usage ?? run.usage };
    case "usage":
      return { usage: f.usage };
    case "error":
      return { error: f.message };
  }
}

function ensureListening() {
  listening ??= playground
    .listen((f) => {
      for (const [threadId, run] of runs) {
        if (run.requestId === f.id) {
          // 正文与思考是一秒上百帧的那两种，合并；其余（routed / usage / done / error）
          // 一轮只来一次，立刻画。
          patch(threadId, (r) => apply(r, f), f.kind === "delta" || f.kind === "thinking");
          break;
        }
      }
    })
    .then(() => undefined);
  return listening;
}

/** 这个会话此刻的 run（没有就是 undefined）。 */
export function useRun(threadId: string | null): Run | undefined {
  return useSyncExternalStore(subscribe, () => (threadId ? runs.get(threadId) : undefined));
}

/** 有没有任何一路在跑（侧栏页签上点灯用）。 */
export function useAnyRunning(kind: Kind): boolean {
  return useSyncExternalStore(subscribe, () => {
    for (const r of runs.values()) if (r.kind === kind && !r.done) return true;
    return false;
  });
}

/**
 * 发一轮。同一会话已有一路在跑就什么都不做（按钮本该已经禁用）。
 * 命令的成败都落在 run 上，不往外抛——调用方只管在 `done` 时刷新会话。
 *
 * `attachments` 的字节这一趟随命令上去；缩略图的 object URL 交给这条 run 保管，
 * 待定气泡照它画，run 收掉时释放。
 */
export async function startChat(threadId: string, prompt: string | null, attachments: ImageAttachment[] = []): Promise<void> {
  if (runs.get(threadId) && !runs.get(threadId)!.done) return;
  await ensureListening();
  const requestId = newRequestId();
  runs.set(threadId, {
    requestId,
    threadId,
    kind: "chat",
    prompt,
    attachments: attachments.map((a) => ({ id: a.id, name: a.name, url: a.url })),
    text: "",
    thinking: "",
    routed: null,
    usage: null,
    startedAt: Date.now(),
    done: false,
  });
  emit();
  try {
    const reply = await playground.send(requestId, threadId, prompt ?? undefined, toChatAttachments(attachments));
    patch(threadId, () => ({ reply, done: true }));
  } catch (e) {
    patch(threadId, () => ({ error: errorText(e), done: true }));
  }
}

export async function startImage(threadId: string, request: ImageRequest): Promise<void> {
  if (runs.get(threadId) && !runs.get(threadId)!.done) return;
  const requestId = newRequestId();
  runs.set(threadId, {
    requestId,
    threadId,
    kind: "image",
    prompt: request.prompt,
    attachments: [],
    image: request,
    text: "",
    thinking: "",
    routed: null,
    usage: null,
    startedAt: Date.now(),
    done: false,
  });
  emit();
  try {
    const reply = await playground.generateImage(requestId, threadId, request);
    patch(threadId, () => ({ reply, done: true }));
  } catch (e) {
    patch(threadId, () => ({ error: errorText(e), done: true }));
  }
}

/**
 * 出一段视频。首帧图的字节随命令上去；缩略图交给 run 当待定气泡里的附件。
 * 命令要等上游做完（可能几分钟），期间 `stop` 让 Rust 不再轮询。
 */
export async function startVideo(threadId: string, request: VideoRequest, attachments: ImageAttachment[] = []): Promise<void> {
  if (runs.get(threadId) && !runs.get(threadId)!.done) return;
  const requestId = newRequestId();
  runs.set(threadId, {
    requestId,
    threadId,
    kind: "video",
    prompt: request.prompt,
    attachments: attachments.map((a) => ({ id: a.id, name: a.name, url: a.url })),
    video: { duration: request.duration, aspectRatio: request.aspectRatio, resolution: request.resolution },
    text: "",
    thinking: "",
    routed: null,
    usage: null,
    startedAt: Date.now(),
    done: false,
  });
  emit();
  try {
    const reply = await playground.generateVideo(requestId, threadId, request);
    patch(threadId, () => ({ reply, done: true }));
  } catch (e) {
    patch(threadId, () => ({ error: errorText(e), done: true }));
  }
}

/** 停止。Rust 侧把半截回复带着「已停止」落库，命令随后返回，run 照常走到 `done`。 */
export async function stopRun(threadId: string): Promise<void> {
  const run = runs.get(threadId);
  if (!run || run.done) return;
  await playground.stop(run.requestId).catch(() => false);
}

/**
 * 视图刷新完会话后调：把已完成的 run 收掉。还在跑的不动。
 * 附件缩略图的 object URL 在这里释放——到这一步库里的那份已经画在气泡里了。
 */
export function consumeRun(threadId: string) {
  const run = runs.get(threadId);
  if (run?.done) {
    for (const a of run.attachments) URL.revokeObjectURL(a.url);
    runs.delete(threadId);
    emit();
  }
}

/**
 * WebView 重载之后模块状态是空的，但 Rust 侧可能还有一路在跑（比如一张 2K 图要两分钟）。
 * 打开会话时问一下，有就接回来：半截文字直接补上，之后的帧照常按 requestId 分发。
 * 命令的 Promise 已经拿不回来了，所以这条 run 靠收尾帧（done / error）判完。
 */
export async function adoptActive(threadId: string): Promise<void> {
  if (runs.has(threadId)) return;
  const active = await playground.active(threadId).catch(() => null);
  if (!active || runs.has(threadId)) return;
  await ensureListening();
  runs.set(threadId, {
    requestId: active.requestId,
    threadId,
    kind: active.kind,
    prompt: null,
    // 附件的缩略图是那次发送在本页建的 object URL，重载之后没了；
    // 图本身已经落库，刷新会话时会连消息一起回来。
    attachments: [],
    text: active.text,
    thinking: active.thinking,
    routed: active.routed,
    usage: null,
    startedAt: Date.now() - active.startedMsAgo,
    done: false,
  });
  emit();
  // 没有 Promise 可等，就盯着 Rust 那边「还在不在跑」；它注销之后回复已经落库。
  const poll = window.setInterval(() => {
    void playground
      .active(threadId)
      .then((a) => {
        if (a) return;
        window.clearInterval(poll);
        patch(threadId, () => ({ done: true }));
      })
      .catch(() => {
        window.clearInterval(poll);
        patch(threadId, () => ({ done: true }));
      });
  }, 800);
}
