/**
 * Grok Build 账号卡。订阅号看周期额度；API Key 号没有套餐用量，就不编一条。
 */
import type { DeviceAccount, GatewayCandidate } from "../../ipc/types";
import { timeAgo } from "../../ui/format";
import { VendorLogo } from "../../relay/VendorLogo";
import { GatewayCard } from "./GatewayCard";

export function GrokAccountCard({
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
  const isApiKey = a.authKind === "api_key";
  const needsLogin = a.status !== "active" || (isApiKey ? !a.hasRefresh && !a.hasToken : a.hasRefresh === false);
  const quota = a.usage ?? null;
  const title = a.label || a.email || `${prefix}…${a.accountRef.slice(-6)}`;
  const plan = isApiKey ? "API Key" : (a.subscriptionTier || quota?.subscriptionTier || a.planType || "SuperGrok");
  const pct = quota?.creditUsagePercent != null ? clamp(quota.creditUsagePercent) : null;
  const resetAt = quota?.periodEnd ? Date.parse(quota.periodEnd) : null;
  const period =
    quota?.periodType === "monthly"
      ? "月度 30d"
      : quota?.periodType === "five_hour"
        ? "会话 5h"
        : "周期 7d";

  const planClass = isApiKey
    ? "plan pill-key"
    : plan.toLowerCase().includes("super") || plan.toLowerCase().includes("premium+")
      ? "plan plan-ultra"
      : plan.toLowerCase().includes("premium") || plan.toLowerCase().includes("pro")
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
        <b>xAI API Key</b>
      </div>
      <div>
        <span>能力支持</span>
        <b className="is-ok">{a.mediaEligible ? "文本 + 图像" : "标准对话"}</b>
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

  return (
    <GatewayCard
      logo={<VendorLogo vendor="xai" size={14} />}
      title={title}
      plan={plan}
      planClass={planClass}
      badges={!isApiKey && a.mediaEligible ? <span className="pill pill-quiet">生图</span> : null}
      current={lane?.state.kind === "current"}
      dimmed={needsLogin || a.status === "dead" || !a.enabled}
      tone={toneOf(needsLogin, a.status, a.enabled, pct)}
      compact={isApiKey}
      kv={kv}
      rows={
        !isApiKey && pct != null
          ? [
              {
                key: "credit",
                label: period,
                hint: `${period}已用 ${pct}%`,
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

function toneOf(
  needsLogin: boolean,
  status: DeviceAccount["status"],
  enabled: boolean,
  pct: number | null,
): "ok" | "warn" | "bad" | "none" {
  if (status === "dead" || needsLogin) return "bad";
  if (!enabled) return "none";
  if (pct != null && pct >= 90) return "bad";
  if (pct != null && pct >= 70) return "warn";
  return "ok";
}

function clamp(n: number): number {
  return Math.max(0, Math.min(100, Math.round(n)));
}
