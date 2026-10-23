/**
 * ZCode 账号卡。没有可查询的 Credits 计数，就不画一条假进度。
 * 卡片只说套餐和凭证形式，和别的网关号同一副骨架。
 */
import type { DeviceAccount, GatewayCandidate } from "../../ipc/types";
import { timeAgo } from "../../ui/format";
import { VendorLogo } from "../../relay/VendorLogo";
import { GatewayCard } from "./GatewayCard";

export function ZcodeAccountCard({
  account: a,
  prefix,
  lane,
  onRelogin,
  onOpen,
}: {
  account: DeviceAccount;
  prefix: string;
  lane: GatewayCandidate | null;
  disabled: boolean;
  onToggle: (on: boolean) => void;
  onUse?: () => void;
  onRelogin?: () => void;
  onRemove: () => void;
  onNote?: (note: string | null) => void;
  onOpen?: () => void;
}) {
  const needsLogin = a.status !== "active";
  const title = a.label || a.email || `${prefix}…${a.accountRef.slice(-6)}`;
  const isStartPlan = a.plan === "start-plan" || (!a.hasApiKey && a.hasJwt);
  const plan = isStartPlan
    ? "体验套餐"
    : a.family?.includes("team")
      ? "团队版"
      : a.family?.includes("individual")
        ? "个人版"
        : "编码套餐";

  const planClass = isStartPlan
    ? "plan plan-free"
    : a.family?.includes("team")
      ? "plan plan-team"
      : "plan plan-pro";

  const credential = a.hasApiKey ? "API Key" : "令牌";

  const kv = (
    <>
      <div>
        <span>凭证掩码</span>
        <b>••••{a.keyHint || a.accountRef.slice(-6)}</b>
      </div>
      <div>
        <span>接入模式</span>
        <b>{a.hasApiKey ? "永久 API Key" : "客户端令牌"}</b>
      </div>
      <div>
        <span>官方网关</span>
        <b>{a.provider === "bigmodel" ? "智谱开放平台" : "ZAI 原生"}</b>
      </div>
    </>
  );

  const emptyState = needsLogin
    ? { title: "需官方客户端登录", hint: "登录态已失效", tone: "warn" as const }
    : undefined;

  const footSpend = (
    <span className="qf-meta">
      {a.createdAt ? <span className="qf-stamp">{timeAgo(a.createdAt)}添加</span> : null}
    </span>
  );

  return (
    <GatewayCard
      logo={<VendorLogo vendor="zhipu" size={14} />}
      title={title}
      plan={plan}
      planClass={planClass}
      badges={<span className="pill pill-quiet">{credential}</span>}
      current={lane?.state.kind === "current"}
      dimmed={needsLogin || a.status === "dead" || !a.enabled}
      tone={needsLogin || a.status === "dead" ? "bad" : !a.enabled ? "none" : "ok"}
      compact={true}
      kv={!needsLogin ? kv : undefined}
      emptyState={emptyState}
      spend={footSpend}
      stamp={a.lastCheckedAt ? `${timeAgo(a.lastCheckedAt)}检查` : null}
      problem={a.lastError}
      needsLogin={needsLogin}
      onRelogin={onRelogin}
      onOpen={onOpen}
    />
  );
}
