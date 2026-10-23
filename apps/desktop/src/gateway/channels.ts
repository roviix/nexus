/**
 * 网关通道的纯函数：从 `GatewayStatus` 里把所有通道摆成**同一种东西**，再算一句结论。
 *
 * 网关背后是好几队号：Cursor / ChatGPT / Grok Build / Kiro / ZCode / Qoder / Claude，外加用户自己的
 * API Key 供应商。Rust 侧把 Cursor 放在 `status.lane`、其余放在 `status.channels`——那是选路
 * 实现上的主次；对用户来说它们是并列的通道。这里把两处并成一份 `LocalChannel[]`，Cursor 永远
 * 排第一。默认通道由用户指定，不再写死 Cursor。
 */
import type { LocalModel } from "../ipc/models";
import type { ChannelSnapshot, GatewayCandidate, GatewayChannelId, GatewayLane, GatewayStatus } from "../ipc/types";
import type { AccountPlatform } from "../shell/nav";

const EMPTY_LANE: GatewayLane = { current: null, candidates: [], missing: [], available: [] };

import type { CustomLogicalChannel, ChannelStrategy } from "./customChannels";
import { loadCustomChannels } from "./customChannels";

/** 本地通道的 id：账号页的平台 id 或是 Mesh 共享网络 / 远端节点。 */
export type LocalChannelId = AccountPlatform | "mesh" | (string & {});

export const CURSOR: LocalChannelId = "cursor";

export interface LocalChannel {
  id: LocalChannelId;
  label: string;
  /** `/v1/models` 的 owned_by：cursor / openai / xai / aws / zhipu / nexus-mesh。 */
  vendor: string;
  /** 用户指定的默认通道：裸名 / 空模型走这里。Cursor 的号池仍在网关页管。 */
  isDefault: boolean;
  /** 平台级不可用原因；与某一个账号冷却/掉登录不同。 */
  unavailableReason: string | null;
  /** 有没有号能接聊天。 */
  ready: boolean;
  /** 有没有号能接媒体（生图 / 生视频）。 */
  mediaReady: boolean;
  lane: GatewayLane;
  chatModels: string[];
  imageModels: string[];
  videoModels: string[];
  prefixes: string[];
  /** Nexus 2.0 通道类型：原生本地 / Mesh 远端云端 / 自定义逻辑编排分流 */
  kind?: "local" | "mesh" | "custom";
  latencyMs?: number;
  strategy?: ChannelStrategy;
  boundAccountCount?: number;
  boundAccountLabels?: string[];
  avgQuotaPercent?: number | null;
  platform?: AccountPlatform;
  peerInfo?: { node_id: string; device_name: string; latency_ms: number };
}

/**
 * 把 `status.lane`（Cursor）和 `status.channels` 并成一份。
 *
 * Cursor 的模型清单不在快照里（它是静态表 + 兜底），给了 `local` 目录时按「不归任何就绪
 * 订阅通道」反推；没给就留空——网关页只关心号，不关心模型数。
 */
const CHANNEL_IDS: readonly string[] = ["cursor", "chatgpt", "grok", "kiro", "zcode", "qoder", "claude", "antigravity", "provider"];

/** 请求前缀（含别名）→ 通道。别名 `codex/` `xai/` `glm/` `agy/` `google/` `p2p/` 只认来源，不进目录。 */
const PREFIX_ALIASES: Record<string, LocalChannelId> = {
  cursor: "cursor",
  chatgpt: "chatgpt",
  codex: "chatgpt",
  grok: "grok",
  xai: "grok",
  kiro: "kiro",
  zcode: "zcode",
  glm: "zcode",
  qoder: "qoder",
  "qoder-cn": "qoder",
  claude: "claude",
  antigravity: "antigravity",
  agy: "antigravity",
  google: "antigravity",
  "gemini-web": "gemini-web",
  gweb: "gemini-web",
  provider: "provider",
  mesh: "mesh",
  p2p: "mesh",
};

export function isChannelId(id: string | null | undefined, channels?: LocalChannel[]): id is LocalChannelId {
  if (!id) return false;
  const lower = id.toLowerCase();
  if ((CHANNEL_IDS as readonly string[]).includes(lower)) return true;
  if (channels && channels.some((c) => c.id.toLowerCase() === lower)) return true;
  return false;
}

export function defaultChannelId(status: GatewayStatus | null | undefined, channels?: LocalChannel[]): LocalChannelId {
  const id = status?.settings.defaultChannel;
  return isChannelId(id, channels) ? id : CURSOR;
}

/** `{通道}/{模型}` 的第一段。对不上已知通道时当裸名（供应商的模型 id 自己也可能带斜杠）。 */
export function splitModelId(id: string, channels?: LocalChannel[]): { channel: LocalChannelId | null; name: string } {
  const slash = id.indexOf("/");
  if (slash <= 0) return { channel: null, name: id };
  const prefix = id.slice(0, slash).toLowerCase();
  const channel = PREFIX_ALIASES[prefix] ?? null;
  if (channel) return { channel, name: id.slice(slash + 1) };
  if (channels) {
    const ch = channels.find((c) => c.id.toLowerCase() === prefix);
    if (ch) return { channel: ch.id, name: id.slice(slash + 1) };
  }
  return { channel: null, name: id };
}

/** 通道里「号」的量词：供应商按家数，Mesh 按节点数，别的按号数。 */
export function unitOf(id: LocalChannelId): string {
  if (id === "provider") return "家";
  if (id === "mesh") return "个节点";
  return "个号";
}

export function localChannels(
  status: GatewayStatus | null | undefined,
  local?: LocalModel[] | null,
  peers?: { node_id: string; device_name: string; remote_models: string[]; latency_ms: number }[] | null,
  customChannelsInput?: CustomLogicalChannel[] | null,
): LocalChannel[] {
  const defaultId = defaultChannelId(status);
  let extras: LocalChannel[] = (status?.channels ?? [])
    .filter((ch) => (ch.id as string) !== "mesh" && (ch.id as string) !== "gemini-web")
    .map((ch) => ({
      id: ch.id,
      label: ch.label,
      vendor: ch.vendor,
      isDefault: ch.id === defaultId,
      unavailableReason: null,
      ready: ch.ready,
      mediaReady: ch.mediaReady,
      lane: ch.lane,
      chatModels: ch.chatModels,
      imageModels: ch.imageModels,
      videoModels: ch.videoModels,
      prefixes: ch.prefixes,
      kind: "local" as const,
      platform: ch.id as AccountPlatform,
    }));

  const cursorModels = (local ?? []).filter((m) => splitModelId(m.id).channel === CURSOR);
  const cursorLane = status?.lane ?? EMPTY_LANE;
  const cursorUnavailable = status?.cursorInferenceUnavailableReason ?? null;
  const cursor: LocalChannel = {
    id: CURSOR,
    label: "Cursor",
    vendor: "cursor",
    isDefault: defaultId === CURSOR,
    unavailableReason: cursorUnavailable,
    ready: !cursorUnavailable && cursorLane.candidates.some(usableCandidate),
    // 生图走另一条 AiService RPC；Stream 的停用不替它下结论。
    mediaReady: cursorLane.candidates.some(usableCandidate),
    lane: cursorLane,
    chatModels: cursorUnavailable
      ? []
      : cursorModels.filter((m) => (m.modality ?? "chat") === "chat").map((m) => m.id),
    imageModels: cursorModels.filter((m) => m.modality === "image").map((m) => m.id),
    videoModels: cursorModels.filter((m) => m.modality === "video").map((m) => m.id),
    prefixes: ["cursor/"],
    kind: "local" as const,
    platform: "cursor",
  };

  // 加载自定义逻辑通道
  const customs = customChannelsInput ?? (typeof window !== "undefined" ? loadCustomChannels() : []);
  if (customs && customs.length > 0) {
    const baseList = [cursor, ...extras];
    for (const c of customs) {
      // 如果自定义配置的是基础通道本身（id 与 platform 一致），直接增强该通道
      if (c.id === c.platform) {
        const target = baseList.find((ch) => ch.id === c.platform);
        if (target) {
          if (c.accountIds.length > 0) {
            const filtered = target.lane.candidates.filter((cand) =>
              c.accountIds.some((accId) => cand.label.includes(accId) || cand.storedId === accId),
            );
            target.lane = {
              ...target.lane,
              candidates: filtered,
              current: filtered.find((cand) => cand.state.kind === "current")?.label ?? null,
            };
            target.boundAccountCount = c.accountIds.length;
            target.boundAccountLabels = c.accountIds;
          }
          target.strategy = c.strategy;
          if (c.label) target.label = c.label;
        }
        continue;
      }

      const baseCh = baseList.find((ch) => ch.id === c.platform);
      if (baseCh) {
        // 根据 boundAccountIds 筛选 candidate
        const filteredCandidates = c.accountIds.length > 0
          ? baseCh.lane.candidates.filter((cand) => c.accountIds.some((accId) => cand.label.includes(accId) || cand.storedId === accId))
          : baseCh.lane.candidates;
        const isReady = filteredCandidates.some(usableCandidate);
        const prefix = c.prefix.trim().toLowerCase();
        extras.push({
          id: c.id,
          label: c.label,
          vendor: baseCh.vendor,
          isDefault: c.isDefault ?? false,
          unavailableReason: null,
          ready: isReady,
          mediaReady: baseCh.mediaReady && isReady,
          lane: {
            ...baseCh.lane,
            candidates: filteredCandidates,
            current: filteredCandidates.find((cand) => cand.state.kind === "current")?.label ?? null,
          },
          chatModels: baseCh.chatModels.map((m) => {
            const clean = m.includes("/") ? m.slice(m.indexOf("/") + 1) : m;
            return `${prefix}/${clean}`;
          }),
          imageModels: baseCh.imageModels.map((m) => {
            const clean = m.includes("/") ? m.slice(m.indexOf("/") + 1) : m;
            return `${prefix}/${clean}`;
          }),
          videoModels: baseCh.videoModels.map((m) => {
            const clean = m.includes("/") ? m.slice(m.indexOf("/") + 1) : m;
            return `${prefix}/${clean}`;
          }),
          prefixes: [`${prefix}/`],
          kind: "custom" as const,
          strategy: c.strategy,
          boundAccountCount: c.accountIds.length,
          boundAccountLabels: c.accountIds,
          platform: c.platform,
        });
      }
    }
  }

  if (peers && peers.length > 0) {
    for (const p of peers) {
      const pid = p.device_name.toLowerCase();
      extras.push({
        id: pid,
        label: p.device_name,
        vendor: "nexus-mesh",
        isDefault: false,
        unavailableReason: null,
        ready: true,
        mediaReady: false,
        lane: { current: null, candidates: [], missing: [], available: [] },
        chatModels: p.remote_models.map((m) => (m.toLowerCase().startsWith(`${pid}/`) ? m : `${pid}/${m}`)),
        imageModels: [],
        videoModels: [],
        prefixes: [`${pid}/`, `mesh/${pid}/`],
        kind: "mesh" as const,
        latencyMs: p.latency_ms,
        peerInfo: p,
      });
    }
  }

  return [cursor, ...extras];
}

/** 一条通道能接的全部模型 id。 */
export function modelsOf(ch: LocalChannel): string[] {
  return [...ch.chatModels, ...ch.imageModels, ...ch.videoModels];
}

/** 这个本地模型此刻会走哪条通道。带前缀认前缀；裸名走用户默认通道。 */
export function channelOfModel(channels: LocalChannel[], id: string): LocalChannelId {
  const lower = id.toLowerCase();
  for (const ch of channels) {
    if (lower.startsWith(`${ch.id.toLowerCase()}/`)) return ch.id;
    if (ch.prefixes.some((p) => lower.startsWith(p.toLowerCase()))) return ch.id;
  }
  const split = splitModelId(id);
  if (split.channel) {
    const matched = channels.find((c) => c.id.toLowerCase() === split.channel?.toLowerCase());
    if (matched) return matched.id;
  }
  return channels.find((c) => c.isDefault)?.id ?? CURSOR;
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
  if (!status.cursorInferenceUnavailableReason && status.lane.candidates.some(usableCandidate)) return false;
  return !status.channels.some((c) => c.lane.candidates.some(usableCandidate));
}

export type Tone = "ok" | "warn" | "default";

export interface ChannelSummary {
  text: string;
  tone: Tone;
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
 * 一句网关视角的结论：几个能接、谁在用。默认通道没号是真没有（裸名会被拒）；
 * 其余通道没号只是「写成 {id}/… 才会走这里」，不是坏消息。
 */
export function channelSummary(
  ch: Pick<LocalChannel, "id" | "label" | "lane" | "imageModels" | "videoModels" | "chatModels" | "mediaReady"> & {
    isDefault?: boolean;
    unavailableReason?: string | null;
  },
): ChannelSummary {
  if (ch.unavailableReason) {
    const shortReason = ch.unavailableReason.length > 30 ? "暂不可用 · 请切换至其他通道" : ch.unavailableReason;
    return { text: shortReason, tone: "warn" };
  }
  const { usable, total, current } = laneCount(ch.lane);
  if (ch.isDefault) {
    if (total === 0) return { text: "未配置可用账号", tone: "warn" };
    if (usable === 0) return { text: `${total} 个账号不可用`, tone: "warn" };
    return { text: `${usable} / ${total} 可用${current ? ` · 当前：${current.label}` : ""}`, tone: "ok" };
  }
  const prefix = `${ch.id}/`;
  if (ch.id === "provider") {
    if (total === 0) return { text: `还没有供应商 · 前缀 ${prefix}`, tone: "default" };
    if (usable === 0) return { text: `${total} 家供应商不可用`, tone: "warn" };
    return { text: `${usable} / ${total} 可用${current ? ` · 当前：${current.label}` : ""}`, tone: "ok" };
  }
  if (ch.id === "mesh") {
    if (total === 0) return { text: `去中心化网络 · 前缀 ${prefix}`, tone: "default" };
    return { text: `${usable} / ${total} 节点就绪 · 前缀 ${prefix}`, tone: "ok" };
  }
  if (total === 0) return { text: `未配置账号 · 前缀 ${prefix}`, tone: "default" };
  if (usable === 0) return { text: `${total} 个账号不可用`, tone: "warn" };
  const media = ch.imageModels.length + ch.videoModels.length > 0 ? (ch.mediaReady ? " · 可出图" : " · 无媒体额度") : "";
  return {
    text: `${usable} / ${total} 可用${current ? ` · 当前：${current.label}` : ""}${media}`,
    tone: "ok",
  };
}
