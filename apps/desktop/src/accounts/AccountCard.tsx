import type { ReactNode } from "react";
import {
  chatgptOrgLabel,
  chatgptPlanLabel,
  chatgptProblem,
  chatgptRailTone,
  chatgptSubscriptionText,
  chatgptTrafficText,
  chatgptWindowViews,
  planClass,
} from "../pages/accounts/chatgpt";
import {
  AccountLine,
  AccountResets,
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
  creditPoints,
  planLabel,
  planTone,
  railTone,
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

  // 卡片只展示添加时间，不外露实际金额与按需计费（账单相关去抽屉看）
  const footSpend = managed ? (
    <span className="qf-meta">
      {timeAgo(managed.createdAt)}添加
    </span>
  ) : null;

  const quota = fullUsage ? (
    <QuotaStrip usage={fullUsage} />
  ) : managed?.availability === "logged_out" ? (
    <div className="acct-empty-state is-logged-out">
      <span className="acct-empty-title">登录态失效</span>
      <span className="acct-empty-hint">上游要求重新登录，重新授权后恢复</span>
    </div>
  ) : managed?.availability === "dead" ? (
    <div className="acct-empty-state is-dead">
      <span className="acct-empty-title">凭证已失效</span>
      <span className="acct-empty-hint">此账号已被上游失效，无法使用</span>
    </div>
  ) : (
    <div className="acct-empty-state">
      <span className="acct-empty-title">尚未获取用量</span>
      <span className="acct-empty-hint">点击刷新拉取最新额度与账期</span>
    </div>
  );

  const grantRemaining = fullUsage?.creditGrantRemainingCents;

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
          {grantRemaining != null && grantRemaining > 0 ? (
            <span className="pill pill-grant" title={`赠送积分剩余：${creditPoints(grantRemaining)} / ${creditPoints(fullUsage?.creditGrantTotalCents)}`}>
              积分 {creditPoints(grantRemaining)}
            </span>
          ) : null}
          {badges}
        </>
      }
      note={note ?? (managed?.availability === "logged_out" || managed?.availability === "dead" ? null : defaultNote)}
      quota={quota}
      resets={
        fullUsage ? (
          <AccountResets
            usage={fullUsage}
            weeklyLabel={!note && problem?.label.startsWith("Bot") ? null : "Bot"}
          />
        ) : null
      }
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
