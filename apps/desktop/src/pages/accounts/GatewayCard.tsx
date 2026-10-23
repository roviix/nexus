/**
 * 网关账号卡。和 ChatGPT 同一副骨架：身份、档位、用量行、重置、一枚刷新。
 * 启停、备注、删除都在抽屉里，卡片上不再各排一套按钮。
 */
import type { ReactNode } from "react";
import { AccountLine, FootResets, QuotaRows, type RailTone } from "../../ui/AccountLine";
import { Icon } from "../../ui/primitives";

export type GatewayQuotaRow = {
  key: string;
  label: string;
  hint: string;
  percent: number | null;
  note?: string;
  resetText?: string | null;
  resetAt?: number | null;
};

export type GatewayEmptyState = {
  title: string;
  hint?: string;
  tone?: "default" | "warn" | "bad";
};

export function GatewayCard({
  logo,
  title,
  plan,
  planClass,
  badges,
  tone = "none",
  current: _current,
  dimmed,
  highlighted,
  rows,
  cols,
  customQuota,
  kv,
  blank,
  emptyState,
  resets,
  spend,
  stamp,
  problem,
  needsLogin,
  refreshing,
  refreshLabel = "刷新用量",
  reloginLabel = "授权",
  compact,
  channelPill,
  recentActivity,
  onOpen,
  onRefresh,
  onRelogin,
}: {
  logo?: ReactNode;
  title: string;
  plan?: string | null;
  planClass?: string | null;
  badges?: ReactNode;
  tone?: RailTone;
  current?: boolean;
  dimmed?: boolean;
  highlighted?: boolean;
  rows?: GatewayQuotaRow[];
  cols?: boolean;
  customQuota?: ReactNode;
  kv?: ReactNode;
  blank?: string;
  emptyState?: GatewayEmptyState;
  resets?: Array<{ label?: string | null; at?: number | null }>;
  spend?: ReactNode;
  stamp?: ReactNode;
  problem?: string | null;
  needsLogin?: boolean;
  refreshing?: boolean;
  refreshLabel?: string;
  reloginLabel?: string;
  compact?: boolean;
  channelPill?: ReactNode;
  recentActivity?: ReactNode;
  onOpen?: () => void;
  onRefresh?: () => void;
  onRelogin?: () => void;
}) {
  const showRefresh = Boolean(onRefresh) && !needsLogin;
  const showRelogin = Boolean(needsLogin && onRelogin);

  const planCls = planClass ?? (plan ? "plan" : undefined);
  const headerBadges = (
    <>
      {logo ? <span className="acct-brand-logo">{logo}</span> : null}
      {plan ? <span className={planCls}>{plan}</span> : null}
      {badges}
    </>
  );

  const renderQuota = () => {
    if (customQuota) {
      return customQuota;
    }
    if (kv) {
      return <div className="acct-kv">{kv}</div>;
    }
    if (rows && rows.length > 0) {
      return <QuotaRows rows={rows} cols={cols} />;
    }
    if (emptyState) {
      const toneCls = emptyState.tone && emptyState.tone !== "default" ? ` is-${emptyState.tone}` : "";
      return (
        <div className={`acct-empty-state${toneCls}`}>
          <span className="acct-empty-title">{emptyState.title}</span>
          {emptyState.hint ? <span className="acct-empty-hint">{emptyState.hint}</span> : null}
        </div>
      );
    }
    if (blank) {
      const toneCls = needsLogin ? " is-logged-out" : "";
      return (
        <div className={`acct-empty-state${toneCls}`}>
          <span className="acct-empty-title">{blank}</span>
        </div>
      );
    }
    return null;
  };

  const alertBanner = needsLogin && onRelogin ? (
    <div className="acct-alert danger">
      <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="2">
        <circle cx="12" cy="12" r="10" />
        <path d="M12 8v4M12 16h.01" />
      </svg>
      <span>{problem || "登录态已失效，需重新授权"}</span>
      <button
        type="button"
        className="act"
        onClick={(e) => {
          e.stopPropagation();
          onRelogin();
        }}
      >
        {reloginLabel || "重新授权"}
      </button>
    </div>
  ) : null;

  return (
    <AccountLine
      tone={tone}
      current={false}
      dimmed={dimmed}
      highlighted={highlighted}
      compact={compact ?? (!rows?.length && !customQuota)}
      alert={alertBanner}
      channelPill={channelPill}
      recentActivity={recentActivity}
      onOpen={onOpen}
      openLabel={onOpen ? `查看 ${title} 的详情` : undefined}
      title={title}
      badges={headerBadges}
      note={problem && !needsLogin ? <span className="acct-problem">{problem}</span> : undefined}
      quota={renderQuota()}
      resets={resets && resets.some((r) => r.at != null) ? <FootResets items={resets} /> : null}
      spend={spend}
      stamp={stamp}
      actions={
        showRefresh || showRelogin ? (
          <>
            {showRelogin ? (
              <button type="button" className="btn btn-sm btn-soft" onClick={onRelogin}>
                {reloginLabel}
              </button>
            ) : null}
            {showRefresh ? (
              <button
                type="button"
                className="ibtn"
                disabled={refreshing}
                onClick={onRefresh}
                aria-label={refreshLabel}
              >
                <Icon name="refresh" size={13} className={refreshing ? "is-spinning" : undefined} />
              </button>
            ) : null}
          </>
        ) : undefined
      }
    />
  );
}

/** 抽屉里的「在不在本地网关」。各平台以前各写一份，边距和文案都不一样。 */
export function GatewayLane({
  enabled,
  current: _current,
  onToggle,
  k = "本地网关",
  idle,
  active,
  join,
  leave,
}: {
  enabled: boolean;
  current: boolean;
  onToggle: (on: boolean) => void;
  k?: string;
  idle?: string;
  active?: string;
  join?: string;
  leave?: string;
}) {
  return (
    <div className="usedin">
      <div className={`usedin-row${enabled ? " is-in" : ""}`}>
        <span className="usedin-ico">
          <Icon name="gateway" size={13} />
        </span>
        <span className="usedin-k">{k}</span>
        <span className="usedin-v">{enabled ? (active ?? "已加入号池") : (idle ?? "未加入")}</span>
        <button
          type="button"
          className={`btn btn-sm${enabled ? " btn-quiet" : " btn-primary"}`}
          onClick={() => onToggle(!enabled)}
        >
          {enabled ? (leave ?? "移出") : (join ?? "加入号池")}
        </button>
      </div>
    </div>
  );
}
