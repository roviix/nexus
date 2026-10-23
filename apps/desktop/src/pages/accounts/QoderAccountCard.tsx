/**
 * Qoder 账号卡。套餐额度、加购、组织额度各一行，和别的网关号同一副骨架。
 */
import type { DeviceAccount, GatewayCandidate } from "../../ipc/types";
import { timeAgo } from "../../ui/format";
import { VendorLogo } from "../../relay/VendorLogo";
import { GatewayCard } from "./GatewayCard";
import { bucketLabel, qoderPlan, qoderRegion, qoderTitle, usedPercent } from "./qoder";

export function QoderAccountCard({
  account: a,
  lane,
  refreshing,
  onRelogin,
  onRefresh,
  onOpen,
}: {
  account: DeviceAccount;
  lane: GatewayCandidate | null;
  disabled: boolean;
  refreshing?: boolean;
  onToggle: (on: boolean) => void;
  onRelogin: () => void;
  onRemove: () => void;
  onRefresh?: () => void;
  onNote?: (note: string | null) => void;
  onOpen?: () => void;
}) {
  const needsLogin = a.status !== "active" || !a.hasToken;
  const usage = a.qoderUsage ?? null;
  const title = qoderTitle(a);
  const plan = usage ? qoderPlan(a) : "Qoder";
  const region = qoderRegion(a.backend);
  const pct = usedPercent(usage);
  const rawResetAt = usage?.resetAt ? Date.parse(usage.resetAt) : null;
  const resetAt =
    rawResetAt &&
    Number.isFinite(rawResetAt) &&
    rawResetAt > Date.now() &&
    rawResetAt - Date.now() < 90 * 86_400_000
      ? rawResetAt
      : null;

  const planClass = plan.toLowerCase().includes("pro")
    ? "plan plan-pro"
    : plan.toLowerCase().includes("team") || plan.toLowerCase().includes("enterprise")
      ? "plan plan-team"
      : plan.toLowerCase().includes("free")
        ? "plan plan-free"
        : "plan";

  const rows =
    usage?.buckets
      .filter((b) => b.percent != null)
      .slice(0, 3)
      .map((b, idx) => ({
        key: b.id,
        label: bucketLabel(b),
        hint: bucketLabel(b),
        percent: Math.max(0, Math.min(100, Math.round(b.percent!))),
        resetAt: idx === 0 ? resetAt : null,
      })) ?? [];

  const emptyState = needsLogin
    ? { title: "需更新 PAT", tone: "warn" as const }
    : usage?.unlimited
      ? { title: "组织共享席位", tone: "default" as const }
      : { title: "尚未同步用量" };

  const footSpend = (
    <span className="qf-meta">
      {a.createdAt ? <span className="qf-stamp">{timeAgo(a.createdAt)}添加</span> : null}
    </span>
  );

  return (
    <GatewayCard
      logo={<VendorLogo vendor="qoder" size={14} />}
      title={title}
      plan={plan}
      planClass={planClass}
      badges={<span className="pill pill-quiet">{region}</span>}
      current={lane?.state.kind === "current"}
      dimmed={needsLogin || a.status === "dead" || !a.enabled}
      tone={toneOf(needsLogin, a.status, a.enabled, usage?.exceeded ?? false, pct)}
      rows={rows}
      emptyState={rows.length === 0 ? emptyState : undefined}
      spend={footSpend}
      stamp={usage ? `${timeAgo(usage.checkedAt)}更新` : null}
      problem={a.lastError}
      needsLogin={needsLogin}
      refreshing={refreshing}
      onOpen={onOpen}
      onRefresh={onRefresh}
      onRelogin={onRelogin}
      reloginLabel="更新"
    />
  );
}

function toneOf(
  needsLogin: boolean,
  status: DeviceAccount["status"],
  enabled: boolean,
  exceeded: boolean,
  pct: number | null,
): "ok" | "warn" | "bad" | "none" {
  if (status === "dead" || needsLogin) return "bad";
  if (!enabled) return "none";
  if (exceeded || (pct != null && pct >= 90)) return "bad";
  if (pct != null && pct >= 70) return "warn";
  return "ok";
}
