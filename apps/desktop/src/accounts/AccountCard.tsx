import type { ReactNode } from "react";
import {
  AccountLine,
  AccountResets,
  AccountSpend,
  QuotaBlank,
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
  // 现在 union 只有 Cursor。以后新增平台时，这个 switch 会迫使 renderer 明确处理它，
  // 而不是默默把 Cursor 四桶套到 ChatGPT / Claude 上。
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
  const tone = accountTone(view);

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

function AccountUsage({ usage }: { usage: AccountView["usage"] }) {
  switch (usage.kind) {
    case "cursor":
      return <QuotaStrip usage={usage.value} />;
    case "summary":
      return <QuotaSummary label={usage.label} percentUsed={usage.percentUsed} />;
    case "unavailable":
      return <QuotaBlank>{usage.reason}</QuotaBlank>;
  }
}

function accountTone(view: AccountView): RailTone {
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
