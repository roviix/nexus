/**
 * ChatGPT 账号详情抽屉。外壳跟 Cursor 那份一样（身份、备注、所在池、页脚删除），
 * 里面分两页：用量（Codex / Spark 窗口 + 本地网关账本）和账单（订阅到期 / 会不会续）。
 * 没有标价和发票——Codex OAuth 打不开 ChatGPT 的 Stripe 门户。加入 / 移出本地网关、
 * 「用这个」都在这里，不在卡片上。
 */
import { useEffect, useRef, useState, type ReactNode } from "react";
import type { ChatGptAccountView } from "../../accounts/model";
import { GATEWAY_MEMBERSHIP_LABEL, inGatewayRoster } from "../../accounts/pools";
import { chatgpt as chatgptApi } from "../../ipc/api";
import type { ChatGptAccount, ChatGptBilling } from "../../ipc/types";
import { timeAgo, timeUntil } from "../../ui/format";
import { shortDateTime } from "../../ui/usage";
import { Banner, CopyButton, Drawer, ErrorNote, Gauge, Health, Icon, Reset, Spinner, Tag } from "../../ui/primitives";
import { DeleteAccount, NoteLine } from "./AccountDrawer";
import {
  chatgptActiveText,
  chatgptExpiresMs,
  chatgptGatewayMembership,
  chatgptHasCredits,
  chatgptOrgLabel,
  chatgptPeriodLabel,
  chatgptPlanLabel,
  chatgptProblem,
  chatgptRenewText,
  chatgptSubscriptionExpired,
  chatgptSubscriptionText,
  chatgptTrafficText,
  chatgptUsable,
  chatgptWindowViews,
  laneBadge,
  planClass,
} from "./chatgpt";

type Tab = "usage" | "bill";

const TABS: Array<[Tab, string]> = [
  ["usage", "用量"],
  ["bill", "账单"],
];

export function ChatGptDrawer({
  view,
  refreshing,
  actionError,
  onClose,
  onRefresh,
  onAuthorize,
  onSaveNote,
  onEnable,
  onUse,
  onRemove,
  onReload,
}: {
  view: ChatGptAccountView;
  refreshing: boolean;
  actionError?: unknown;
  onClose: () => void;
  onRefresh: () => void;
  onAuthorize: () => void;
  onSaveNote: (note: string) => Promise<void>;
  onEnable: (on: boolean) => Promise<void>;
  onUse: () => Promise<void>;
  onRemove: () => Promise<void>;
  onReload: () => Promise<void>;
}) {
  const [tab, setTab] = useState<Tab>("usage");
  const account = view.managed;
  const usage = account.usage;
  const problem = chatgptProblem(account);
  const usable = chatgptUsable(account);
  const membership = chatgptGatewayMembership(account, view.lane);
  const enrolled = inGatewayRoster(membership);
  const isCurrent = membership === "current";
  const plan = chatgptPlanLabel(account.planType);
  const planCls = planClass(account.planType);
  const remain = chatgptSubscriptionText(account.billing);

  const head = (
    <div className="dr-id">
      <div className="dr-title">
        <span className="dr-email selectable truncate" title={view.label}>
          {view.label}
        </span>
        <CopyButton value={view.label} icon label="复制账号" />
      </div>
      <div className="dr-badges">
        {plan && planCls ? <span className={planCls}>{plan}</span> : null}
        <span className="dr-meta">
          {problem ? <Health tone={problem.tone}>{problem.label}</Health> : null}
          <span>{chatgptOrgLabel(account.organizationTitle)}</span>
          <span>{usage ? `用量 ${timeAgo(usage.checkedAt)}更新` : "还没查过用量"}</span>
          {remain ? <span>{remain}</span> : null}
          {account.accessExpiresAt && usable ? (
            <span>凭证 {timeUntil(new Date(account.accessExpiresAt).getTime())} 后续期</span>
          ) : null}
        </span>
      </div>
      <NoteLine key={account.id} note={account.note ?? ""} onSave={onSaveNote} />
      <div className="dr-actions">
        {usable ? (
          <button
            type="button"
            className="btn btn-sm btn-icon btn-soft"
            data-tip={refreshing ? "刷新中…" : "刷新用量"}
            aria-label="刷新用量"
            disabled={refreshing}
            onClick={onRefresh}
          >
            <Icon name="refresh" size={14} className={refreshing ? "is-spinning" : undefined} />
          </button>
        ) : null}
        <button type="button" className="btn btn-sm btn-soft" onClick={onAuthorize}>
          <Icon name="external" size={13} />
          {account.hasRefresh ? "重新授权" : "授权"}
        </button>
        {isCurrent ? (
          <button type="button" className="btn btn-sm dr-cta" disabled title="本地网关现在用的就是它">
            <Icon name="gateway" size={13} />
            网关正在用
          </button>
        ) : (
          <button
            type="button"
            className="btn btn-sm btn-primary dr-cta"
            disabled={!usable || !enrolled}
            title={
              !usable
                ? account.status === "dead"
                  ? "这个号已失效"
                  : "需要先重新授权"
                : enrolled
                  ? "下一个请求走这个号"
                  : "先加入本地网关"
            }
            onClick={() => void onUse()}
          >
            <Icon name="gateway" size={13} />
            用这个
          </button>
        )}
      </div>
    </div>
  );

  const foot = (
    <>
      <span className="dr-foot-meta">{timeAgo(account.createdAt)}添加</span>
      <DeleteAccount
        warning={
          isCurrent
            ? "会连同 refresh_token 一起从本机清除，不可恢复。它正在被网关使用，进行中的对话会换号并丢上游缓存。"
            : "会连同 refresh_token 一起从本机清除，不可恢复。"
        }
        onConfirm={async () => {
          await onRemove();
          onClose();
        }}
      />
    </>
  );

  return (
    <Drawer label={`账号详情 ${view.label}`} onClose={onClose} head={head} footer={foot}>
      <div className="dr-body">
        <ErrorNote error={actionError} />
        {account.lastError ? <Banner tone="warn" title="上次操作出了错" hint={account.lastError} /> : null}

        <GatewayUsedIn
          account={account}
          membership={membership}
          enrolled={enrolled}
          isCurrent={isCurrent}
          laneText={gatewayLineText(account, view, membership)}
          onEnable={onEnable}
        />

        <IdentityPane account={account} />

        <div className="tabs tabs-block">
          {TABS.map(([id, label]) => (
            <button key={id} type="button" className="tab" aria-selected={tab === id} onClick={() => setTab(id)}>
              {label}
            </button>
          ))}
        </div>

        {tab === "usage" ? <UsagePane account={account} refreshing={refreshing} onRefresh={onRefresh} /> : null}
        {tab === "bill" ? <BillPane account={account} onReload={onReload} /> : null}
      </div>
    </Drawer>
  );
}

function gatewayLineText(
  account: ChatGptAccountView["managed"],
  view: ChatGptAccountView,
  membership: ReturnType<typeof chatgptGatewayMembership>,
): string {
  if (membership === "current" || membership === "available" || membership === "none") {
    return GATEWAY_MEMBERSHIP_LABEL[membership];
  }
  const badge = laneBadge(view.lane, account.enabled);
  return badge ? `已加入 · ${badge.text}` : GATEWAY_MEMBERSHIP_LABEL[membership];
}

function GatewayUsedIn({
  account,
  membership,
  enrolled,
  isCurrent,
  laneText,
  onEnable,
}: {
  account: ChatGptAccountView["managed"];
  membership: ReturnType<typeof chatgptGatewayMembership>;
  enrolled: boolean;
  isCurrent: boolean;
  laneText: string;
  onEnable: (on: boolean) => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const usable = chatgptUsable(account);

  async function act(on: boolean) {
    setBusy(true);
    try {
      await onEnable(on);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section>
      <div className="usedin">
        <div
          className={`usedin-row${enrolled ? " is-in" : ""}${membership === "current" ? " is-live" : ""}${membership === "skipped" ? " is-warn" : ""}`}
        >
          <span className="usedin-ico">
            <Icon name="gateway" size={13} />
          </span>
          <span className="usedin-k">本地网关</span>
          <span className="usedin-v">{laneText}</span>
          {enrolled ? (
            <button
              type="button"
              className="btn btn-sm btn-quiet btn-danger"
              disabled={busy}
              onClick={() => {
                if (isCurrent && !window.confirm(`${account.email ?? "这个号"} 正在被网关使用。移出后下一个请求会换号，正在进行的对话会丢上游缓存。继续？`)) {
                  return;
                }
                void act(false);
              }}
            >
              {busy ? <Spinner /> : "移出"}
            </button>
          ) : (
            <button
              type="button"
              className="btn btn-sm btn-soft"
              disabled={busy || !usable}
              title={usable ? "交给网关，额度到线时接力用它" : account.status === "dead" ? "这个号已失效" : "需要先授权"}
              onClick={() => void act(true)}
            >
              {busy ? <Spinner /> : "加入"}
            </button>
          )}
        </div>
      </div>
    </section>
  );
}

function IdentityPane({ account }: { account: ChatGptAccountView["managed"] }) {
  return (
    <section>
      <div className="sect-cap">
        <span>账号</span>
      </div>
      <div className="kv">
        <IdentityRow label="工作区" value={chatgptOrgLabel(account.organizationTitle)} />
        {account.userId ? <IdentityRow label="用户 ID" value={account.userId} copy /> : null}
        <IdentityRow label="账号 ID" value={account.accountRef} copy />
      </div>
    </section>
  );
}

function IdentityRow({
  label,
  value,
  copy,
  hint,
}: {
  label: string;
  value: string;
  copy?: boolean;
  hint?: string;
}) {
  return (
    <div className="kv-row" title={hint}>
      <span className="kv-k">{label}</span>
      <span className="kv-v">
        <span className="mono truncate" title={value}>
          {value}
        </span>
        {copy ? <CopyButton value={value} icon label={`复制${label}`} /> : null}
      </span>
    </div>
  );
}

function UsagePane({
  account,
  refreshing,
  onRefresh,
}: {
  account: ChatGptAccountView["managed"];
  refreshing: boolean;
  onRefresh: () => void;
}) {
  const u = account.usage;
  if (!u) {
    return (
      <div className="dr-blank">
        <p>还没查过这个号的用量。</p>
        {chatgptUsable(account) ? (
          <button type="button" className="btn btn-sm" disabled={refreshing} onClick={onRefresh}>
            {refreshing ? <Spinner /> : "拉一次"}
          </button>
        ) : (
          <p className="faint tiny">先授权才能查。</p>
        )}
      </div>
    );
  }

  const now = Date.now();
  const groups: Array<{ name: string; rows: ReturnType<typeof chatgptWindowViews> }> = [];
  for (const row of chatgptWindowViews(u)) {
    const last = groups.at(-1);
    if (last && last.name === row.group) last.rows.push(row);
    else groups.push({ name: row.group, rows: [row] });
  }

  const traffic = chatgptTrafficText(account.traffic);
  const credits = u.credits;
  const showCredits = chatgptHasCredits(credits);

  return (
    <div className="stack" style={{ gap: 20 }}>
      {traffic || showCredits || u.limitReached ? (
        <section>
          <div className="sect-cap">
            <span>用量摘要</span>
          </div>
          <div className="kv">
            {traffic ? (
              <IdentityRow
                label={`本地 ${account.traffic?.days ?? 90} 天`}
                value={traffic}
                hint="只算经过本机网关的请求，不是 ChatGPT 网页上的终身用量"
              />
            ) : null}
            {showCredits ? (
              <IdentityRow label="点数" value={credits?.unlimited ? "不限" : (credits?.balance ?? "—")} />
            ) : null}
            {credits?.resetAvailable != null && credits.resetAvailable > 0 ? (
              <IdentityRow label="可重置" value={`${credits.resetAvailable} 次`} />
            ) : null}
            {u.limitReached ? <IdentityRow label="主窗口" value="上游已打满" /> : null}
          </div>
        </section>
      ) : null}
      {groups.map((g) => (
        <section key={g.name} className="sect">
          <div className="sect-cap">
            <span>{g.name}</span>
          </div>
          {g.rows.map((w) => (
            <div key={w.key} className="cg-win">
              {w.resetAt ? (
                <Reset
                  label={w.label}
                  at={w.resetAt}
                  now={now}
                  tag={w.percent != null && w.percent >= 100 ? <Tag tone="bad">已耗尽</Tag> : null}
                />
              ) : (
                <div className="sect-cap">
                  <span>{w.label}</span>
                </div>
              )}
              <Gauge label="已用" percent={w.percent} title={w.hint} />
            </div>
          ))}
        </section>
      ))}
    </div>
  );
}

function BillPane({ account, onReload }: { account: ChatGptAccount; onReload: () => Promise<void> }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const tried = useRef(false);
  const b = account.billing;
  const can = chatgptUsable(account);

  async function check() {
    setBusy(true);
    setError(null);
    try {
      await chatgptApi.refreshBilling(account.id);
      await onReload();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  useEffect(() => {
    if (tried.current || b || !can) return;
    tried.current = true;
    void check();
    // 只在第一次打开、还没有快照时自动查。失败后交给按钮。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [account.id]);

  const refresh = (
    <button
      type="button"
      className="btn btn-sm btn-icon btn-soft"
      data-tip={busy ? "正在读订阅…" : "检查订阅"}
      aria-label="检查订阅"
      disabled={busy || !can}
      onClick={() => void check()}
    >
      <Icon name="refresh" size={14} className={busy ? "is-spinning" : undefined} />
    </button>
  );

  if (!b && !error && (busy || (can && !tried.current))) {
    return (
      <section className="sub-hero">
        <div className="sub-hero-top">
          <span className="bill-k">订阅</span>
          {refresh}
        </div>
        <p className="bill-none" style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <Spinner />
          正在读 ChatGPT 订阅…
        </p>
      </section>
    );
  }

  if (!b) {
    return (
      <section className="sub-hero">
        <div className="sub-hero-top">
          <div className="bill-hero-main">
            <span className="bill-k">订阅</span>
            <span className="bill-big" style={{ fontSize: 18, letterSpacing: "-0.02em" }}>
              {can ? "还没查过订阅" : "现在查不了订阅"}
            </span>
            <span className="bill-sub">
              {can
                ? "用手里的 Codex 凭证问到期日和会不会续。标价和发票在 ChatGPT 网页账单里，这里打不开。"
                : "需要还活着的 refresh token。"}
            </span>
          </div>
          {can ? (
            <button type="button" className="btn btn-sm btn-primary" disabled={busy} onClick={() => void check()}>
              {busy ? <Spinner /> : <Icon name="refresh" size={13} />}
              检查订阅
            </button>
          ) : null}
        </div>
        <ErrorNote error={error} />
      </section>
    );
  }

  return <BillingSnapshot billing={b} planType={account.planType} refresh={refresh} error={error} />;
}

function BillingSnapshot({
  billing,
  planType,
  refresh,
  error,
}: {
  billing: ChatGptBilling;
  planType: string | null;
  refresh: ReactNode;
  error: unknown;
}) {
  const plan = chatgptPlanLabel(billing.planType ?? planType);
  const period = chatgptPeriodLabel(billing.billingPeriod);
  const remain = chatgptSubscriptionText(billing);
  const expired = chatgptSubscriptionExpired(billing);
  const expiresMs = chatgptExpiresMs(billing.expiresAt);
  const cadence = [plan, period].filter(Boolean).join(" · ") || "订阅";

  return (
    <div className="stack" style={{ gap: 14 }}>
      <section className="sub-hero">
        <div className="sub-hero-top">
          <div className="bill-hero-main">
            <span className="bill-k">{cadence}</span>
            <span className="bill-big" style={{ fontSize: remain ? 28 : 18, letterSpacing: "-0.03em" }}>
              {remain ?? "到期日未知"}
            </span>
            <span className="bill-sub">
              {expiresMs != null ? `${shortDateTime(expiresMs)} 到期 · ${chatgptRenewText(billing.willRenew)}` : chatgptRenewText(billing.willRenew)}
            </span>
          </div>
          {refresh}
        </div>
        <dl className="facts facts-4">
          <div>
            <dt>状态</dt>
            <dd className={expired ? "is-warn" : undefined}>{chatgptActiveText(billing.hasActiveSubscription, expired)}</dd>
          </div>
          <div>
            <dt>到期</dt>
            <dd>{expiresMs != null ? shortDateTime(expiresMs) : "未知"}</dd>
          </div>
          <div>
            <dt>续费</dt>
            <dd>{billing.willRenew == null ? "未知" : billing.willRenew ? "自动" : "不续"}</dd>
          </div>
          <div>
            <dt>套餐</dt>
            <dd>{plan ?? "未知"}</dd>
          </div>
        </dl>
        <p className="bill-sub" style={{ marginTop: 8 }}>
          没有标价和发票。Codex 凭证读不到 ChatGPT 网页账单里的 Stripe 门户。
        </p>
        {billing.checkedAt ? <p className="faint tiny">{timeAgo(billing.checkedAt)}更新</p> : null}
      </section>
      <ErrorNote error={error} />
    </div>
  );
}
