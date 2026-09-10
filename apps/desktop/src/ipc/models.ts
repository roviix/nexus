/**
 * 模型广场的数据：来自网关的映射目录，以及「试一下」的流式事件。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** 不认识的厂商按字符串原样带过来。 */
export type ModelVendor =
  | "anthropic"
  | "openai"
  | "google"
  | "xai"
  | "cursor"
  | "moonshot"
  | "zhipu"
  | "bytedance"
  | "alibaba"
  | "deepseek"
  | "minimax"
  | "other"
  | (string & {});

export type Modality = "chat" | "image" | "video";

/** 本地网关能替客户端映射到的一个模型。 */
export interface LocalModel {
  id: string;
  vendor: ModelVendor;
  vendorLabel: string;
  /** `chat` | `image` | `video`。老版本网关的目录没有这个字段，缺省按对话算。 */
  modality?: "chat" | "image" | "video";
  /** 同底座的档位共用的系列名（不带档位后缀的那个模型名）。 */
  series: string;
  /** `standard` 或 `thinking-max-fast` 这类后缀。 */
  variant: string;
  /** 会映射到它的客户端叫法（Claude Code / OpenAI SDK 世界里的名字）。 */
  aliases: string[];
  note: string | null;
  /**
   * 出图模型固定出多大（`1536x1024`，Cursor 那条链路）；缺省 / 空 = 认 `size`（ChatGPT 的
   * gpt-image）。老版本网关的目录没有这个字段：本地出图当时只有 Cursor 一条路，按固定算。
   */
  fixedSize?: string | null;
}

export const models = {
  /** 目录：静态、不联网。 */
  local: () => invoke<LocalModel[]>("gateway_models"),
};

// ── 试一下 ──────────────────────────────────────────────────────────────────

export interface TryUsage {
  promptTokens: number;
  completionTokens: number;
}

/** 与 Rust `playground::TryEvent` 一一对应（`kind` 标签，snake_case）。 */
export type TryEvent =
  | { kind: "routed"; model: string }
  | { kind: "delta"; text: string }
  | { kind: "thinking"; text: string }
  | { kind: "done"; finish: string | null; usage: TryUsage | null }
  /** `done` 之后单独补来的用量帧（OpenAI `include_usage` 的形态）。 */
  | { kind: "usage"; usage: TryUsage }
  | { kind: "error"; message: string };

export type TryFrame = TryEvent & { id: string };

export const TRY_EVENT = "gateway://try";

export const tryRun = {
  /**
   * 对着网关发一句话。Promise 在流走完时 resolve；字都在 `gateway://try` 事件里。
   * `requestId` 由调用方生成，用来在事件里认出自己那一次。
   */
  start: (requestId: string, model: string, prompt: string) =>
    invoke<void>("gateway_try", { requestId, model, prompt }),
  listen: (cb: (f: TryFrame) => void) => listen<TryFrame>(TRY_EVENT, (e) => cb(e.payload)),
};
