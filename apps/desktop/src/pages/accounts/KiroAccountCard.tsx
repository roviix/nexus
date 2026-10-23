/**
 * Kiro 账号卡。骨架跟 ChatGPT 一样，只换用量：一套餐额度、一个重置日。
 */
import type { DeviceAccount, GatewayCandidate } from "../../ipc/types";
import { timeAgo } from "../../ui/format";
import { resetInShort } from "../../ui/usage";
import { VendorLogo } from "../../relay/VendorLogo";
import { GatewayCard } from "./GatewayCard";

export function KiroAccountCard({
  account: a,
  prefix,
  lane,
  isClientActive,
  refreshing,
  onRelogin,
  onRefresh,
  onOpen,
}: {
  account: DeviceAccount;
  prefix: string;
  lane: GatewayCandidate | null;
  disabled: boolean;
  isClientActive?: boolean;
  refreshing?: boolean;
  onToggle: (on: boolean) => void;
  onUse?: () => void;
  onRelogin: () => void;
  onRemove: () => void;
  onRefresh?: () => void;
  onNote?: (note: string | null) => void;
  onOpen?: () => void;
}) {
  const needsLogin = a.status !== "active" || !canServe(a);
  const usage = a.kiroUsage ?? null;
  const title = labelOf(a, prefix);
  const plan = usage?.subscriptionTitle || a.planType || "Free";
  const pct = usage ? clamp(usage.percent) : null;
  const resetAt = usage?.resetAt ? Date.parse(usage.resetAt) : null;

  const planLower = plan.toLowerCase();
  const planClass = planLower.includes("power")
    ? "plan plan-ultra"
    : planLower.includes("pro+") || planLower.includes("pro plus") || planLower.includes("proplus")
      ? "plan plan-proplus"
      : planLower.includes("pro")
        ? "plan plan-pro"
        : planLower.includes("team") || planLower.includes("enterprise")
          ? "plan plan-team"
          : planLower.includes("free")
            ? "plan plan-free"
            : "plan";

  const emptyState = needsLogin
    ? { title: "需重新授权", tone: "warn" as const }
    : { title: "尚未同步用量" };

  const remaining = usage && usage.limit != null ? Math.max(0, usage.limit - usage.used) : null;
  const pctTone = pct == null ? "" : pct > 90 ? " is-bad" : pct >= 70 ? " is-warn" : " is-ok";
  const customQuota =
    usage && pct != null ? (
      <div className="acct-credits-wrap">
        <div className="acct-credits-head">
          <div className="acct-credit-val">
            <b>{qty(remaining ?? usage.used)}</b>
            <span>{remaining != null ? "点可用" : "点已用"}</span>
            {usage.limit != null ? <span className="acct-credit-total">/ {qty(usage.limit)}</span> : null}
          </div>
          <span className={`acct-credit-pct${pctTone}`}>
            {Math.round(pct)}%
          </span>
        </div>
        <div className="qc-track">
          <i
            style={{
              width: `${Math.min(100, Math.max(0, pct))}%`,
              background: pct > 90 ? "var(--bad)" : pct >= 70 ? "var(--warn)" : "var(--ok)",
            }}
          />
        </div>
        <div className="acct-credits-sub">
          <span>
            本期已用 <b>{qty(usage.used)}</b> 点
            {usage.trialLimit ? (
              <span className="faint" style={{ marginLeft: 6 }}>
                (含试用 {qty(usage.trialUsed)} / {qty(usage.trialLimit)})
              </span>
            ) : null}
          </span>
          <span>{usage.overageEnabled ? "超额扣费已开启" : "无超额扣费"}</span>
        </div>
      </div>
    ) : undefined;

  const footSpend = (
    <span className="qf-meta">
      {resetAt ? <span className="qf-in">账期 {resetInShort(resetAt)}</span> : null}
      {resetAt && a.createdAt ? <i className="qf-sep">·</i> : null}
      {a.createdAt ? <span className="qf-stamp">{timeAgo(a.createdAt)}添加</span> : null}
    </span>
  );

  return (
    <GatewayCard
      logo={<VendorLogo vendor="kiro" size={14} />}
      title={title}
      plan={plan}
      planClass={planClass}
      current={lane?.state.kind === "current"}
      dimmed={needsLogin || a.status === "dead" || !a.enabled}
      tone={toneOf(needsLogin, a.status, a.enabled, pct)}
      customQuota={customQuota}
      emptyState={pct == null ? emptyState : undefined}
      spend={footSpend}
      channelPill={
        isClientActive ? (
          <span
            className="pill tiny"
            title="Kiro 客户端当前正在登录此账号"
            style={{
              background: "rgba(16, 185, 129, 0.12)",
              color: "var(--ok)",
              border: "1px solid rgba(16, 185, 129, 0.25)",
              fontWeight: 600,
            }}
          >
            ● Kiro 当前登录
          </span>
        ) : undefined
      }
      stamp={usage ? `${timeAgo(usage.checkedAt)}更新` : null}
      problem={a.lastError}
      needsLogin={needsLogin}
      refreshing={refreshing}
      onOpen={onOpen}
      onRefresh={onRefresh}
      onRelogin={onRelogin}
    />
  );
}

function labelOf(a: Pick<DeviceAccount, "email" | "accountRef" | "label">, prefix: string): string {
  const given = a.label?.trim();
  if (given) return given;
  const email = a.email?.trim();
  return email ? email : `${prefix}…${a.accountRef.slice(-6)}`;
}

function canServe(a: DeviceAccount): boolean {
  if (a.hasToken) return true;
  if (a.hasRefresh !== undefined) return a.hasRefresh;
  return (a.hasApiKey ?? false) || (a.hasJwt ?? false);
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

function qty(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n)) return "—";
  const rounded = Math.abs(n - Math.round(n)) < 0.05 ? Math.round(n) : Math.round(n * 10) / 10;
  return rounded.toLocaleString("zh-CN");
}
