/**
 * 网关通道的纯函数：从 `GatewayStatus` 里把所有通道摆成**同一种东西**，再算一句结论。
 *
 * 网关背后是好几队号：Cursor 的号（默认通道，不带前缀的请求都落这儿）、ChatGPT / Grok Build /
 * Kiro 的号（订阅通道，各接各的模型）。Rust 侧把 Cursor 放在 `status.lane`、其余放在
 * `status.channels`——那是选路实现上的主次；对用户来说它们是并列的四条通道，本地网关页、
 * 接入页、模型广场都该按并列摆。这里把两处并成一份 `LocalChannel[]`，Cursor 永远排第一。
 */
import type { LocalModel } from "../ipc/models";
import type { ChannelSnapshot, GatewayCandidate, GatewayChannelId, GatewayLane, GatewayStatus } from "../ipc/types";
import type { AccountPlatform } from "../shell/nav";

const EMPTY_LANE: GatewayLane = { current: null, candidates: [], missing: [], available: [] };

/** 本地通道的 id：就是账号页的平台 id——每条通道背后就是那个平台的号。 */
export type LocalChannelId = AccountPlatform;

export const CURSOR: LocalChannelId = "cursor";

export interface LocalChannel {
  id: LocalChannelId;
  label: string;
  /** `/v1/models` 的 owned_by：cursor / openai / xai / aws。 */
  vendor: string;
  /** 默认通道：不带前缀、别的通道不接的模型都落到它；号池在网关页管，不在账号页。 */
  isDefault: boolean;
  /** 有没有号能接聊天。 */
  ready: boolean;
  /** 有没有号能接媒体（生图 / 生视频）。 */
  mediaReady: boolean;
  lane: GatewayLane;
  chatModels: string[];
  imageModels: string[];
  videoModels: string[];
  prefixes: string[];
}

/**
 * 把 `status.lane`（Cursor）和 `status.channels` 并成一份。
 *
 * Cursor 的模型清单不在快照里（它是静态表 + 兜底），给了 `local` 目录时按「不归任何就绪
 * 订阅通道」反推；没给就留空——网关页只关心号，不关心模型数。
 */
export function localChannels(status: GatewayStatus | null | undefined, local?: LocalModel[] | null): LocalChannel[] {
  const extras: LocalChannel[] = (status?.channels ?? []).map((ch) => ({
    id: ch.id,
    label: ch.label,
    vendor: ch.vendor,
    isDefault: false,
    ready: ch.ready,
    mediaReady: ch.mediaReady,
    lane: ch.lane,
    chatModels: ch.chatModels,
    imageModels: ch.imageModels,
    videoModels: ch.videoModels,
    prefixes: ch.prefixes,
  }));

  const claimed = new Set<string>();
  for (const ch of extras) {
    if (ch.ready) for (const m of ch.chatModels) claimed.add(m);
    if (ch.mediaReady) for (const m of [...ch.imageModels, ...ch.videoModels]) claimed.add(m);
  }
  const cursorModels = (local ?? []).filter((m) => !claimed.has(m.id));
  const cursorLane = status?.lane ?? EMPTY_LANE;
  const cursor: LocalChannel = {
    id: CURSOR,
    label: "Cursor",
    vendor: "cursor",
    isDefault: true,
    ready: cursorLane.candidates.some(usableCandidate),
    mediaReady: cursorLane.candidates.some(usableCandidate),
    lane: cursorLane,
    chatModels: cursorModels.filter((m) => (m.modality ?? "chat") === "chat").map((m) => m.id),
    imageModels: cursorModels.filter((m) => m.modality === "image").map((m) => m.id),
    videoModels: cursorModels.filter((m) => m.modality === "video").map((m) => m.id),
    prefixes: [],
  };
  return [cursor, ...extras];
}

/** 一条通道能接的全部模型 id。 */
export function modelsOf(ch: LocalChannel): string[] {
  return [...ch.chatModels, ...ch.imageModels, ...ch.videoModels];
}

/** 这个本地模型此刻会走哪条通道。目录里没有的名字按默认通道（Cursor）算——网关也是这么兜底的。 */
export function channelOfModel(channels: LocalChannel[], id: string): LocalChannelId {
  for (const ch of channels) {
    if (ch.isDefault) continue;
    if (modelsOf(ch).includes(id)) return ch.id;
  }
  return CURSOR;
}

export function channelOf(status: GatewayStatus | null | undefined, id: GatewayChannelId): ChannelSnapshot | null {
  return status?.channels.find((c) => c.id === id) ?? null;
}

export function laneOf(status: GatewayStatus | null | undefined, id: GatewayChannelId): GatewayLane {
  return channelOf(status, id)?.lane ?? EMPTY_LANE;
}

/** 能接请求的号：正在用、待接力、或只是对某个模型冷却。 */
export function usableCandidate(c: GatewayCandidate): boolean {
  return c.state.kind === "current" || c.state.kind === "ready" || c.state.kind === "cooled";
}

/** 网关开着，但所有通道（含 Cursor）都没有一个号能接。 */
export function starved(status: GatewayStatus): boolean {
  if (!status.running) return false;
  if (status.lane.candidates.some(usableCandidate)) return false;
  return !status.channels.some((c) => c.lane.candidates.some(usableCandidate));
}

export type Tone = "ok" | "warn" | "default";

export interface ChannelSummary {
  text: string;
  tone: Tone;
}

/** 这条通道的模型在没有号时会退到哪儿、怎么叫。 */
function modelWord(ch: Pick<LocalChannel, "id" | "chatModels">): string {
  switch (ch.id) {
    case "chatgpt":
      return "GPT / Codex 模型";
    case "grok":
      return "grok-*";
    case "kiro":
      return "kiro-claude-*";
    default:
      return ch.chatModels[0] ?? ch.id;
  }
}

/** 一条通道里能接的号 / 总数 / 正在用的那个。 */
export function laneCount(lane: GatewayLane): { usable: number; total: number; current: GatewayCandidate | null } {
  return {
    usable: lane.candidates.filter(usableCandidate).length,
    total: lane.candidates.length + lane.missing.length,
    current: lane.candidates.find((c) => c.state.kind === "current") ?? null,
  };
}

/**
 * 一句网关视角的结论：几个能接、谁在用、没有号时请求去哪。
 *
 * 订阅通道没有号不是坏消息（请求照旧走 Cursor），所以是 default；有号但全不可用才 warn。
 * Cursor 是兜底，它没有号就是真没有——请求会被拒，所以是 warn。
 */
export function channelSummary(
  ch: Pick<LocalChannel, "id" | "label" | "lane" | "imageModels" | "videoModels" | "chatModels" | "mediaReady"> & { isDefault?: boolean },
): ChannelSummary {
  const { usable, total, current } = laneCount(ch.lane);
  if (ch.isDefault) {
    if (total === 0) return { text: "号池为空 · 不带前缀的请求会被拒", tone: "warn" };
    if (usable === 0) return { text: `${total} 个号都不可用 · 请求会被拒`, tone: "warn" };
    return { text: `${usable} / ${total} 个号可接${current ? ` · 正在用 ${current.label}` : ""}`, tone: "ok" };
  }
  const word = modelWord(ch);
  if (total === 0) return { text: `没有 ${ch.label} 账号 · ${word} 会走 Cursor 的号`, tone: "default" };
  if (usable === 0) return { text: `${total} 个 ${ch.label} 账号都不可用 · ${word} 请求会退回 Cursor 的号`, tone: "warn" };
  const media = ch.imageModels.length + ch.videoModels.length > 0 ? (ch.mediaReady ? " · 可出图 / 出视频" : " · 无号可出媒体") : "";
  return {
    text: `${usable} / ${total} 个 ${ch.label} 账号可接${current ? ` · 正在用 ${current.label}` : ""}${media}`,
    tone: "ok",
  };
}
