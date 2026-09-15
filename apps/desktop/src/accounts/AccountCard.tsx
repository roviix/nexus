import type { ReactNode } from "react";
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
  AccountResets,
  AccountSpend,
  FootResets,
  QuotaBlank,
  QuotaRows,
  QuotaStrip,
  QuotaSummary,
  type RailTone,
} from "../ui/AccountLine";
import { timeAgo } from "../ui/format";
import { accountProblem, planLabel, planTone, railTone } from "../ui/usage";
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
  // 重置时刻和按需计费只有完整用量才有。网关那种只知道一个总百分比的降级形态不能
  // 凭空造出这两样 —— 缺了就让末行空着那一格，不编。
  const fullUsage = view.usage.kind === "cursor" ? view.usage.value : null;
  const problem = managed ? accountProblem(managed, fullUsage) : null;
  const defaultNote = problem ? (
    <span className="acct-problem">{problem.label}</span>
  ) : managed?.lastError ? (
    <span className="acct-problem">{managed.lastError}</span>
  ) : null;
  const tone = cursorTone(view);

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
          {badges}
        </>
      }
      note={note ?? defaultNote}
      quota={<AccountUsage usage={view.usage} />}
      resets={
        fullUsage ? (
          // 「Bot 已耗尽 · Bot 13:26 重置」两个 Bot 挨着，第二个是废话。页面自己传的 note
          // 我们不知道说的是谁，那就照常写全。
          <AccountResets usage={fullUsage} weeklyLabel={!note && problem?.label.startsWith("Bot") ? null : "Bot"} />
        ) : null
      }
      spend={fullUsage ? <AccountSpend usage={fullUsage} /> : null}
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
