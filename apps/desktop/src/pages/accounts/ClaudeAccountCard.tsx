/**
 * Claude 账号卡。订阅号看套餐已用；Console API Key 没有这档额度。
 */
import type { DeviceAccount, GatewayCandidate } from "../../ipc/types";
import { timeAgo } from "../../ui/format";
import { VendorLogo } from "../../relay/VendorLogo";
import { GatewayCard } from "./GatewayCard";
import { assignAccountToChannel, channelOfAccount, loadCustomChannels } from "../../gateway/customChannels";

export function ClaudeAccountCard({
  account: a,
  prefix,
  lane,
  refreshing,
  onRelogin,
  onRefresh,
  onOpen,
}: {
  account: DeviceAccount;
  prefix: string;
  lane: GatewayCandidate | null;
  refreshing?: boolean;
  onRelogin: () => void;
  onRefresh?: () => void;
  onOpen?: () => void;
}) {
  const isApiKey = a.authKind === "api_key" || (a.hasApiKey === true && !a.hasRefresh && !a.hasToken);
  const needsLogin = a.status !== "active";
  const quota = a.usage ?? null;
  const title = a.label || a.email || `${prefix}…${a.accountRef.slice(-6)}`;
  const plan = isApiKey ? "API Key" : (quota?.subscriptionTier || a.planType || a.tier || "Pro");
  const pct = !isApiKey && quota?.creditUsagePercent != null ? clamp(quota.creditUsagePercent) : null;
  const resetAt = quota?.periodEnd ? Date.parse(quota.periodEnd) : null;
  const period =
    quota?.periodType === "monthly"
      ? "月度 30d"
      : quota?.periodType === "five_hour"
        ? "会话 5h"
        : "周期 7d";

  const planClass = isApiKey
    ? "plan pill-key"
    : plan.toLowerCase().includes("team")
      ? "plan plan-team"
      : plan.toLowerCase().includes("pro")
        ? "plan plan-pro"
        : plan.toLowerCase().includes("free")
          ? "plan plan-free"
          : "plan";

  const kv = isApiKey ? (
    <>
      <div>
        <span>凭证掩码</span>
        <b>••••{a.keyHint || a.accountRef.slice(-6)}</b>
      </div>
      <div>
        <span>接入模式</span>
        <b>Console 直连</b>
      </div>
      <div>
        <span>计费方式</span>
        <b className="is-ok">按量实时扣费</b>
      </div>
    </>
  ) : undefined;

  const emptyState = needsLogin
    ? { title: "需重新授权", tone: "warn" as const }
    : { title: "尚未同步用量" };

  const footSpend = (
    <span className="qf-meta">
      {a.createdAt ? <span className="qf-stamp">{timeAgo(a.createdAt)}添加</span> : null}
    </span>
  );

  const customChs = loadCustomChannels().filter((c) => c.platform === "claude");
  const accKey = a.accountRef || a.email || "";
  const currentChId = channelOfAccount(accKey, "claude");

  const channelPill = (
    <span className="acct-ch-capsule" title="点击分配所属分流通道">
      <span className="acct-ch-label">所属通道:</span>
      <select
        className="acct-ch-select mono"
        value={currentChId}
        onClick={(e) => e.stopPropagation()}
        onChange={(e) => {
          assignAccountToChannel(accKey, "claude", e.target.value);
          if (onRefresh) onRefresh();
        }}
      >
        <option value="claude">● Claude-默认 (claude/*)</option>
        {customChs.map((c) => (
          <option key={c.id} value={c.id}>
            ● {c.label} ({c.prefix}/*)
          </option>
        ))}
      </select>
    </span>
  );

  const recentActivity =
    lane?.state.kind === "current"
      ? "最近活动: 刚才由 Claude Code 调用"
      : a.updatedAt
        ? `最近同步: ${timeAgo(a.updatedAt)}`
        : null;

  const remainingPct = pct != null ? Math.max(0, 100 - pct) : null;

  return (
    <GatewayCard
      logo={<VendorLogo vendor="anthropic" size={14} />}
      title={title}
      plan={plan}
      planClass={planClass}
      channelPill={channelPill}
      recentActivity={recentActivity}
      current={lane?.state.kind === "current"}
      dimmed={needsLogin || a.status === "dead" || !a.enabled}
      tone={needsLogin || a.status === "dead" ? "bad" : !a.enabled ? "none" : pct != null && pct >= 90 ? "bad" : pct != null && pct >= 70 ? "warn" : "ok"}
      compact={isApiKey}
      kv={kv}
      rows={
        !isApiKey && pct != null
          ? [
              {
                key: "credit",
                label: `订阅窗口额度 (${period})`,
                hint: `剩余 ${remainingPct}% (已用 ${pct}%)`,
                percent: pct,
                resetAt: resetAt,
              },
            ]
          : []
      }
      emptyState={!isApiKey && pct == null ? emptyState : undefined}
      spend={footSpend}
      stamp={quota ? `${timeAgo(quota.checkedAt)}更新` : null}
      problem={a.lastError}
      needsLogin={needsLogin && !isApiKey}
      refreshing={refreshing}
      onOpen={onOpen}
      onRefresh={isApiKey ? undefined : onRefresh}
      onRelogin={onRelogin}
    />
  );
}

function clamp(n: number): number {
  return Math.max(0, Math.min(100, Math.round(n)));
}
