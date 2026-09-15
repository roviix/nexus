import type { ReactNode } from "react";
import type { Account, AccountUsage } from "../ipc/types";
import {
  chatgptOrgLabel,
  chatgptPlanLabel,
  chatgptProblem,
  chatgptRailTone,
  chatgptTrafficText,
  chatgptSubscriptionText,
  chatgptWindowViews,
  planClass,
} from "../pages/accounts/chatgpt";
import {
  AccountLine,
  AccountSpend,
  FootResets,
  QuotaBlank,
  QuotaRows,
  QuotaStrip,
  QuotaSummary,
  type RailTone,
} from "../ui/AccountLine";
import { timeAgo } from "../ui/format";
import {
  accountProblem,
  bonusSpend,
  creditPoints,
  money,
  moneyShort,
  overallPercent,
  pctText,
  planBudget,
  planLabel,
  planSpend,
  planTone,
  railTone,
  resetInShort,
  shortDate,
} from "../ui/usage";
import type { AccountView } from "./model";

export interface AccountCardProps {
  view: AccountView;
  highlighted?: boolean;
  dimmed?: boolean;
  /** 场景徽章：当前登录、正在接力、接力时跳过等。档位由组件自己统一添加。 */
  badges?: ReactNode;
  /** 场景状态；不传时回退到账户本身的问题 / 上次错误。 */
  note?: ReactNode;
  actions?: ReactNode;
  onOpen?: () => void;
}

/**
 * 一个平台账号在任何列表里的统一卡片。
 *
 * 平台 adapter (`AccountView`) 决定身份、用量和托管能力；页面只注入“它在这个场景里是什么
 * 状态”和右侧动作。切号池、网关池、总账号库因此不会再各自拼一遍档位、额度和更新时间。
 */
export function AccountCard({
  view,
  highlighted,
  dimmed,
  badges,
  note,
  actions,
  onOpen,
}: AccountCardProps) {
  // 平台 renderer 必须穷尽：不能默默把 Cursor 四桶套到 ChatGPT 的两个窗口上。
  switch (view.platform) {
    case "cursor":
      return (
        <CursorAccountCard
          view={view}
          highlighted={highlighted}
          dimmed={dimmed}
          badges={badges}
          note={note}
          actions={actions}
          onOpen={onOpen}
        />
      );
    case "chatgpt":
      return (
        <ChatGptAccountCard
          view={view}
          highlighted={highlighted}
          dimmed={dimmed}
          badges={badges}
          note={note}
          actions={actions}
          onOpen={onOpen}
        />
      );
  }
}

function CursorAccountCard({
  view,
  highlighted,
  dimmed,
  badges,
  note,
  actions,
  onOpen,
}: AccountCardProps & { view: Extract<AccountView, { platform: "cursor" }> }) {
  const managed = view.managed;
  const fullUsage = view.usage.kind === "cursor" ? view.usage.value : null;
  const problem = managed ? accountProblem(managed, fullUsage) : null;
  const defaultNote = problem ? (
    <span className="acct-problem">{problem.label}</span>
  ) : managed?.lastError ? (
    <span className="acct-problem">{managed.lastError}</span>
  ) : null;
  const tone = cursorTone(view);

  const footSpend = managed ? (
    <span className="qf-meta">
      <span title={fullUsage?.accountCreatedAt ? `注册于 ${shortDate(Date.parse(fullUsage.accountCreatedAt))}` : undefined}>
        {timeAgo(managed.createdAt)}添加
      </span>
      {fullUsage?.onDemandEnabled && (fullUsage?.onDemandUsedCents ?? 0) > 0 ? (
        <>
          <i className="qf-sep">·</i>
          <span className="qf-in">按需 {money(fullUsage.onDemandUsedCents)}</span>
        </>
      ) : null}
    </span>
  ) : fullUsage ? (
    <AccountSpend usage={fullUsage} />
  ) : null;

  return (
    <AccountLine
      tone={tone}
      dimmed={dimmed ?? managed?.status === "dead"}
      highlighted={highlighted}
      onOpen={onOpen}
      openLabel={onOpen ? `查看 ${view.label} 的账号详情` : undefined}
      title={view.label}
      badges={
        <>
          {view.membership ? (
            <span className={`plan ${planTone(view.membership)}`}>
              {planLabel(view.membership)}
            </span>
          ) : null}
          {managed?.availability === "session" ? (
            <span className="pill pill-session" title={managed.accessExpiresAt ? `到期时间：${shortDate(Date.parse(managed.accessExpiresAt))}` : undefined}>
              仅会话
            </span>
          ) : managed?.availability === "api_key" ? (
            <span className="pill pill-key">仅 Key</span>
          ) : managed?.availability === "logged_out" ? (
            <span className="pill pill-warn">掉登录</span>
          ) : managed?.availability === "dead" ? (
            <span className="pill pill-bad">已失效</span>
          ) : null}
          {badges}
        </>
      }
      note={note ?? (managed?.availability === "logged_out" || managed?.availability === "dead" ? null : defaultNote)}
      quota={<CursorCardHero usage={fullUsage} managed={managed} />}
      spend={footSpend}
      stamp={
        view.usage.kind === "cursor" && view.usage.checkedAt
          ? `${timeAgo(view.usage.checkedAt)}更新`
          : null
      }
      actions={actions}
    />
  );
}

/**
 * 账号卡主体：以高对比排版和宏观进度取代多根彩色进度条轰炸。
 * 眼睛一眼回答「用了多少、还剩多少、什么时候重置」。
 */
function CursorCardHero({
  usage,
  managed,
}: {
  usage: AccountUsage | null;
  managed?: Account | null;
}) {
  if (!usage) {
    if (managed?.availability === "logged_out") {
      return (
        <div className="acct-empty-state is-logged-out">
          <span className="acct-empty-title">登录态失效</span>
          <span className="acct-empty-hint">上游要求重新登录，重新授权后恢复使用</span>
        </div>
      );
    }
    if (managed?.availability === "dead") {
      return (
        <div className="acct-empty-state is-dead">
          <span className="acct-empty-title">凭证已失效</span>
          <span className="acct-empty-hint">此账号已被上游失效，无法用于会话</span>
        </div>
      );
    }
    return (
      <div className="acct-empty-state">
        <span className="acct-empty-title">尚未获取用量</span>
        <span className="acct-empty-hint">点击刷新按钮获取实时额度与账期数据</span>
      </div>
    );
  }

  const spend = planSpend(usage);
  const budget = planBudget(usage);
  const pct = overallPercent(usage);
  const bonus = bonusSpend(usage);
  const grantRemaining = usage.creditGrantRemainingCents;
  const resetAt = usage.cycleEnd;
  const resetText = resetAt ? resetInShort(resetAt) : null;
  const cycleDate = resetAt ? shortDate(resetAt) : null;

  const autoPct = usage.autoPercentUsed;
  const apiPct = usage.apiPercentUsed;
  const bot = usage.bot;

  const pctTone = pct == null ? "idle" : pct > 90 ? "bad" : pct >= 70 ? "warn" : "ok";

  return (
    <div className="acct-hero">
      <div className="acct-hero-row">
        <div className="acct-hero-val">
          {budget != null && spend != null ? (
            <>
              <span className="acct-num-spend num">{money(spend)}</span>
              <span className="acct-num-sep">/</span>
              <span className="acct-num-budget num">{moneyShort(budget)}</span>
              {pct != null ? <span className={`acct-num-pct num is-${pctTone}`}>{Math.round(pct)}%</span> : null}
            </>
          ) : pct != null ? (
            <>
              <span className="acct-num-spend num">{Math.round(pct)}%</span>
              <span className="acct-num-budget">已用</span>
            </>
          ) : (
            <span className="acct-num-spend">额度就绪</span>
          )}
        </div>
        {resetText ? (
          <span className="acct-hero-reset" title={cycleDate ? `账期重置于 ${cycleDate}` : undefined}>
            {resetText}
          </span>
        ) : null}
      </div>

      <div className="acct-hero-track" role="progressbar" aria-valuenow={pct ?? 0} aria-valuemin={0} aria-valuemax={100}>
        <i
          className={`acct-hero-bar is-${pctTone}`}
          style={{ width: `${Math.min(100, Math.max(0, pct ?? 0))}%` }}
        />
      </div>

      <div className="acct-hero-tags">
        <span className="acct-tag" title="Auto 调度（Composer / Grok）">
          <span className="acct-tag-k">Auto</span>
          <span className="acct-tag-v num">{pctText(autoPct)}</span>
        </span>
        <span className="acct-tag-sep">·</span>
        <span className={`acct-tag${(apiPct ?? 0) >= 90 ? " is-warn" : ""}`} title="点名 API 调用（Claude / GPT）">
          <span className="acct-tag-k">API</span>
          <span className="acct-tag-v num">{pctText(apiPct)}</span>
        </span>
        {bot ? (
          <>
            <span className="acct-tag-sep">·</span>
            <span
              className={`acct-tag${bot.access === "blocked" || bot.hasAvailable === false ? " is-bad" : ""}`}
              title="Grok Bot 周额"
            >
              <span className="acct-tag-k">Bot</span>
              <span className="acct-tag-v num">
                {bot.access === "blocked" ? "无权限" : bot.hasAvailable === false ? "耗尽" : pctText(bot.percentUsed)}
              </span>
            </span>
          </>
        ) : null}

        {bonus != null ? (
          <span className="acct-pill-bonus" title="Cursor 与模型厂商补贴的额外用量，不占订阅配额">
            +加量 {moneyShort(bonus)}
          </span>
        ) : null}

        {grantRemaining != null && grantRemaining > 0 ? (
          <span className="acct-pill-grant" title="Cursor 赠送的积分余额 (1 积分 = $1)">
            积分 {creditPoints(grantRemaining)}
          </span>
        ) : null}
      </div>
    </div>
  );
}

function ChatGptAccountCard({
  view,
  highlighted,
  dimmed,
  badges,
  note,
  actions,
  onOpen,
}: AccountCardProps & { view: Extract<AccountView, { platform: "chatgpt" }> }) {
  const managed = view.managed;
  const usage = view.usage.kind === "chatgpt" ? view.usage.value : null;
  const problem = chatgptProblem(managed);
  const defaultNote = problem ? (
    <span className="acct-problem">{problem.label}</span>
  ) : managed.lastError ? (
    <span className="acct-problem">{managed.lastError}</span>
  ) : null;
  const plan = chatgptPlanLabel(view.membership);
  const planCls = planClass(view.membership);
  const org = chatgptOrgLabel(managed.organizationTitle);
  const traffic = chatgptTrafficText(managed.traffic);
  const remain = chatgptSubscriptionText(managed.billing);
  const spend = [remain, traffic].filter(Boolean).join(" · ");

  return (
    <AccountLine
      tone={chatgptRailTone(managed)}
      dimmed={dimmed ?? managed.status === "dead"}
      highlighted={highlighted}
      onOpen={onOpen}
      openLabel={onOpen ? `查看 ${view.label} 的账号详情` : undefined}
      title={view.label}
      badges={
        <>
          {plan && planCls ? <span className={planCls}>{plan}</span> : null}
          {org !== "个人账户" ? <span className="plan">{org}</span> : null}
          {badges}
        </>
      }
      note={note ?? defaultNote}
      quota={<AccountUsage usage={view.usage} />}
      resets={
        usage ? (
          <FootResets
            items={chatgptWindowViews(usage)
              .filter((w) => w.resetAt)
              .slice(0, 3)
              .map((w) => ({ label: w.label, at: w.resetAt }))}
          />
        ) : null
      }
      spend={
        spend ? (
          <span title={traffic ? "订阅剩余来自 accounts/check；次数只算本机网关" : undefined}>{spend}</span>
        ) : null
      }
      stamp={usage ? `${timeAgo(usage.checkedAt)}更新` : null}
      actions={actions}
    />
  );
}

function AccountUsage({ usage }: { usage: AccountView["usage"] }) {
  switch (usage.kind) {
    case "cursor":
      return <QuotaStrip usage={usage.value} />;
    case "chatgpt":
      return <QuotaRows rows={chatgptWindowViews(usage.value)} />;
    case "summary":
      return <QuotaSummary label={usage.label} percentUsed={usage.percentUsed} />;
    case "unavailable":
      return <QuotaBlank>{usage.reason}</QuotaBlank>;
  }
}

function cursorTone(view: Extract<AccountView, { platform: "cursor" }>): RailTone {
  if (view.managed) {
    return railTone(
      view.managed,
      view.usage.kind === "cursor" ? view.usage.value : null,
    );
  }
  if (view.usage.kind !== "summary") return "none";
  if (view.usage.percentUsed > 90) return "bad";
  if (view.usage.percentUsed >= 70) return "warn";
  return "ok";
}
