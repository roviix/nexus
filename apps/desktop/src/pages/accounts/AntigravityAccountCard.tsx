/**
 * Antigravity 账号卡。和 ChatGPT 同一副骨架：Gemini / Claude 各一行已用比例。
 * 没有查到的窗口不补一条 100%。
 */
import type { DeviceAccount, GatewayCandidate } from "../../ipc/types";
import { timeAgo } from "../../ui/format";
import { VendorLogo } from "../../relay/VendorLogo";
import { GatewayCard } from "./GatewayCard";
import { antigravityQuotaRows, formatTierLabel, planClassName } from "./antigravity";

export function AntigravityAccountCard({
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
  disabled: boolean;
  refreshing?: boolean;
  onToggle: (on: boolean) => void;
  onUse?: () => void;
  onRelogin: () => void;
  onRemove: () => void;
  onRefresh?: () => void;
  onNote?: (note: string | null) => void;
  onOpen?: () => void;
}) {
  const needsLogin = a.status !== "active";
  const title = a.label || a.email || (prefix && !a.accountRef.includes("@") ? `${prefix}…${a.accountRef.slice(-6)}` : a.accountRef);
  const usage = a.antigravityUsage;
  const forbidden = usage?.lastStatus === "forbidden";
  const rawPlan = usage?.tierName || a.tier || a.planType;
  const plan = rawPlan ? formatTierLabel(rawPlan) : null;
  const planClass = planClassName(rawPlan);
  const rows = antigravityQuotaRows(usage);

  const emptyState = needsLogin
    ? { title: "需重新授权", tone: "warn" as const }
    : forbidden
      ? { title: "账号被拒绝", hint: "上游不再接受这个号", tone: "bad" as const }
      : { title: "尚未同步用量" };

  const isCustomProject = Boolean(
    a.projectId && !a.projectId.includes("aicode-consumer")
  );
  const footSpend = (
    <span className="qf-meta">
      {isCustomProject ? <span className="mono">{a.projectId}</span> : null}
      {isCustomProject && a.createdAt ? <i className="qf-sep">·</i> : null}
      {a.createdAt ? <span className="qf-stamp">{timeAgo(a.createdAt)}添加</span> : null}
    </span>
  );

  return (
    <GatewayCard
      logo={<VendorLogo vendor="google" size={14} />}
      title={title}
      plan={plan}
      planClass={planClass}
      current={lane?.state.kind === "current"}
      dimmed={needsLogin || forbidden || a.status === "dead" || !a.enabled}
      tone={needsLogin || forbidden || a.status === "dead" ? "bad" : !a.enabled ? "none" : rows.some((r) => (r.percent ?? 0) >= 90) ? "bad" : rows.some((r) => (r.percent ?? 0) >= 70) ? "warn" : rows.length ? "ok" : "none"}
      rows={rows}
      emptyState={rows.length === 0 ? emptyState : undefined}
      spend={footSpend}
      stamp={usage?.checkedAt ? `${timeAgo(usage.checkedAt)}更新` : null}
      problem={needsLogin ? a.lastError : forbidden ? "上游拒绝了这个账号" : null}
      needsLogin={needsLogin}
      refreshing={refreshing}
      onOpen={onOpen}
      onRefresh={onRefresh}
      onRelogin={onRelogin}
    />
  );
}
