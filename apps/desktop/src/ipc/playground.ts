/**
 * 游乐场：会话、消息、图片，以及发一轮 / 出一批图 / 停。
 *
 * 与 `commands/playground.rs` 一一对应；类型是 `nexus_playground::model` 的镜像。
 * 前端每次只交「thread_id + 新的一句话」，历史在 Rust 侧从库里拼；流式回字走
 * `playground://chat` 事件，帧形状与「试一下」同一套（`TryFrame`）。
 * 图片字节不过 IPC：凭 `ImageRef.id` 拼成 `nexus-image://` 地址交给 `<img>`。
 */
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { TryFrame, TryUsage } from "./models";

export type Kind = "chat" | "image" | "video";
export type Role = "user" | "assistant";

export interface Thread {
  id: string;
  kind: Kind;
  /** 空串 = 还没标题（第一条消息进来时用它的开头）。 */
  title: string;
  model: string;
  createdAt: string;
  updatedAt: string;
}

export interface ThreadSummary extends Thread {
  messageCount: number;
  /** 最后一条有内容的消息的开头。 */
  preview: string | null;
  /** 图片会话的封面：最近一张图的 id。 */
  coverImageId: string | null;
}

export interface ImageRef {
  id: string;
  messageId: string;
  mime: string;
  width: number | null;
  height: number | null;
  bytes: number;
  /** 请求时要的规格（`1024x1024`）。 */
  size: string | null;
  createdAt: string;
}

export interface Message {
  id: string;
  threadId: string;
  seq: number;
  role: Role;
  content: string;
  thinking: string | null;
  /** 产出这条回复时请求的模型（只有 assistant 有）。 */
  model: string | null;
  /** 上游实际路由到的模型。 */
  routed: string | null;
  usage: TryUsage | null;
  /** 这一轮失败的原因；有内容也可能有它（中途断掉、被停止）。 */
  error: string | null;
  durationMs: number | null;
  ttftMs: number | null;
  createdAt: string;
  images: ImageRef[];
}

export interface ThreadDetail {
  thread: Thread;
  messages: Message[];
}

/** 资产页里的一张图：图片本身 + 它出自哪个会话、哪个模型、哪句提示词。 */
export interface Asset extends ImageRef {
  threadId: string;
  threadTitle: string;
  model: string | null;
  prompt: string | null;
}

/** 这个会话正在跑的那一次请求，连同攒到现在的半截回复。 */
export interface ActiveRun {
  requestId: string;
  threadId: string;
  kind: Kind;
  text: string;
  thinking: string;
  routed: string | null;
  startedMsAgo: number;
}

export interface ImageRequest {
  prompt: string;
  size: string | null;
  n: number;
}

/** 一次生视频。首帧图的字节随命令上去（不带 `data:` 前缀）。 */
export interface VideoRequest {
  prompt: string;
  imageBase64: string | null;
  imageMime: string | null;
  /** 1–15 秒。 */
  duration: number | null;
  /** `16:9` 这类。 */
  aspectRatio: string | null;
  /** `480p` / `720p` / `1080p`。 */
  resolution: string | null;
}

/** 视频成片也存在 `ImageRef` 里：靠 MIME 认。 */
export function isVideoRef(ref: Pick<ImageRef, "mime">): boolean {
  return ref.mime.startsWith("video/");
}

/**
 * 随一句话发上去的图。字节在这里**要**过 IPC（只有这一趟：Rust 落盘之后前端就凭 id 取图）。
 * `dataBase64` 不带 `data:` 前缀。
 */
export interface ChatAttachment {
  name: string;
  mime: string;
  dataBase64: string;
}

export const PLAYGROUND_CHAT_EVENT = "playground://chat";

export const playground = {
  threads: (kind?: Kind) => invoke<ThreadSummary[]>("playground_threads", { kind: kind ?? null }),
  thread: (id: string) => invoke<ThreadDetail>("playground_thread", { id }),
  createThread: (kind: Kind, model: string) => invoke<Thread>("playground_thread_create", { kind, model }),
  renameThread: (id: string, title: string) => invoke<Thread>("playground_thread_rename", { id, title }),
  /** 换这个会话下一次发送用的模型。 */
  setTarget: (id: string, model: string) => invoke<Thread>("playground_thread_set_target", { id, model }),
  deleteThread: (id: string) => invoke<void>("playground_thread_delete", { id }),
  deleteMessage: (id: string) => invoke<void>("playground_message_delete", { id }),
  active: (threadId: string) => invoke<ActiveRun | null>("playground_active", { threadId }),
  /**
   * 发一轮。`prompt` 缺省 = 重新生成末尾那条回复（那时不能带附件）。Promise 在回复
   * **落库后**才 resolve，中途的字在 `playground://chat` 事件里，按 `requestId` 认。
   */
  send: (requestId: string, threadId: string, prompt?: string, attachments?: ChatAttachment[]) =>
    invoke<Message>("playground_chat_send", { requestId, threadId, prompt: prompt ?? null, attachments: attachments ?? [] }),
  stop: (requestId: string) => invoke<boolean>("playground_stop", { requestId }),
  generateImage: (requestId: string, threadId: string, request: ImageRequest) =>
    invoke<Message>("playground_image_generate", { requestId, threadId, request }),
  /** 出一段视频。Promise 等上游做完、成片落盘后才 resolve（可能几分钟）；中途 `stop` 就不再等。 */
  generateVideo: (requestId: string, threadId: string, request: VideoRequest) =>
    invoke<Message>("playground_video_generate", { requestId, threadId, request }),
  revealImage: (id: string) => invoke<void>("playground_image_reveal", { id }),
  /** 全部生成过的图，新的在前。 */
  assets: () => invoke<Asset[]>("playground_assets"),
  /** 删一张图（连文件）；消息留着。 */
  deleteImage: (id: string) => invoke<void>("playground_image_delete", { id }),
  listen: (cb: (f: TryFrame) => void): Promise<UnlistenFn> => listen<TryFrame>(PLAYGROUND_CHAT_EVENT, (e) => cb(e.payload)),
};

/** 一张图在 WebView 里的地址。macOS / Linux 是 `nexus-image://localhost/<id>`，Windows 是 `http://nexus-image.localhost/<id>`。 */
export function imageSrc(id: string): string {
  return convertFileSrc(id, "nexus-image");
}

/** 一次发送的请求 id：时间戳 + 随机尾巴，同一页里连点两次也分得清。 */
export function newRequestId(): string {
  return `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}
