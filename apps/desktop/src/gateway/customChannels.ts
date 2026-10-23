/**
 * 逻辑通道（Custom Logical Channel）持久化与编排模型。
 *
 * 核心设计（Nexus 2.0 §1 & §4.2）：
 * 通道不再与平台 1:1 死板绑定，而是「可自由编排的逻辑路由池与云端算力节点」。
 * 用户可以把同一个平台的多个账号（例如 3 个 Claude 号）拆分成多条独立前缀的通道
 * （如 Claude-主力工作组 和 Claude-日常与出借），并指定调度策略与路由前缀。
 */
import type { AccountPlatform } from "../shell/nav";

export type ChannelStrategy = "quota_first" | "round_robin" | "sticky";

export interface CustomLogicalChannel {
  id: string; // 唯一通道 ID，如 "claude-work"
  label: string; // 通道名称，如 "Claude-主力工作组"
  platform: AccountPlatform; // 平台归属，如 "claude"
  prefix: string; // 路由前缀，如 "claude-work"
  accountIds: string[]; // 绑定的账号邮箱或凭证标识
  strategy: ChannelStrategy; // 调度策略
  isDefault?: boolean; // 是否设为默认通道
  createdAt: number;
}

const STORAGE_KEY = "nexus.custom_logical_channels";

const STRATEGY_LABELS: Record<ChannelStrategy, string> = {
  quota_first: "额度充足优先 (推荐)",
  round_robin: "顺序轮询 (Round-Robin)",
  sticky: "单号粘性直至耗尽",
};

export function getStrategyLabel(strategy: ChannelStrategy): string {
  return STRATEGY_LABELS[strategy] ?? "额度充足优先";
}

/** 读取所有自定义逻辑通道 */
export function loadCustomChannels(): CustomLogicalChannel[] {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return [];
    const list = JSON.parse(raw);
    return Array.isArray(list) ? list : [];
  } catch {
    return [];
  }
}

/** 保存所有自定义逻辑通道 */
export function saveCustomChannels(channels: CustomLogicalChannel[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(channels));
  } catch {}
}

/** 插入或更新自定义逻辑通道 */
export function upsertCustomChannel(channel: CustomLogicalChannel): void {
  const current = loadCustomChannels();
  const idx = current.findIndex((c) => c.id === channel.id);
  if (idx >= 0) {
    current[idx] = channel;
  } else {
    current.push(channel);
  }
  saveCustomChannels(current);
}

/** 删除自定义逻辑通道 */
export function deleteCustomChannel(id: string): void {
  const current = loadCustomChannels();
  const next = current.filter((c) => c.id !== id);
  saveCustomChannels(next);
}

/** 根据账号 ID 查找所属自定义通道（如未单独指定，则属于平台默认通道） */
export function channelOfAccount(accountId: string, platform: AccountPlatform): string {
  const channels = loadCustomChannels().filter((c) => c.platform === platform);
  for (const ch of channels) {
    if (ch.accountIds.includes(accountId)) {
      return ch.id;
    }
  }
  return platform;
}

/** 将账号指派给指定自定义通道（或移回默认通道） */
export function assignAccountToChannel(accountId: string, platform: AccountPlatform, targetChannelId: string): void {
  const channels = loadCustomChannels();
  let changed = false;
  for (const ch of channels) {
    if (ch.platform === platform) {
      if (ch.id === targetChannelId) {
        if (!ch.accountIds.includes(accountId)) {
          ch.accountIds.push(accountId);
          changed = true;
        }
      } else {
        if (ch.accountIds.includes(accountId)) {
          ch.accountIds = ch.accountIds.filter((id) => id !== accountId);
          changed = true;
        }
      }
    }
  }
  if (changed) {
    saveCustomChannels(channels);
  }
}
