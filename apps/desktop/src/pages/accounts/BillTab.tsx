/**
 * 账单页：上面是订阅实付（Stripe 门户），下面是用量花费（dashboard）。
 *
 * 两套数不是同一口径。门户说的是「下个月扣多少、有没有券」；用量说的是
 * 「这个账期 Auto / API 烧了多少」。混在一个大数里会把 $0 Ultra 和花了 $21
 * 的用量叠成一句糊涂话，所以分成两张卡。
 */
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import type { Account, AccountBilling, BillingInvoice, ModelUsage, UsageEventsReport } from "../../ipc/types";
import { accounts } from "../../ipc/api";
import { Banner, ErrorNote, Icon, Spinner, Tag } from "../../ui/primitives";
import { timeAgo } from "../../ui/format";
import { canQueryUsage, canUseDashboard } from "../../ui/accounts";
import { compactNumber } from "../../ui/traffic";
import {
  collectionLabel,
  discountOffText,
  discountStateLabel,
  durationText,
  intervalLabel,
  invoiceStatusLabel,
  productName,
  subStatusAlert,
  subStatusLabel,
} from "../../ui/billing";
import {
  daysText,
  meterColor,
  meterWidth,
  money,
  moneyFx,
  moneyShort,
  creditPoints,
  onDemandText,
  pctText,
  shortDate,
  shortDateTime,
  spendPace,
  type SpendPace,
  bonusSpend,
  startOfLocalDay,
} from "../../ui/usage";

export function BillTab({ account, onReload }: { account: Account; onReload: () => Promise<void> }) {
  const u = account.usage;
  const [range, setRange] = useState<BillRange>("cycle");

  return (
    <div className="stack" style={{ gap: 18 }}>
      <SubscriptionBill account={account} onReload={onReload} />

      {!u ? (
        <p className="sect-none">还没查过用量，下面的花费明细要先刷一次用量。</p>
      ) : (
        <>
          {u.via === "apiKey" ? (
            <Banner tone="warn" title="基础用量" hint="session 不可用，花费来自 crsr_ API Key 兑票后的逐条事件。没有账期百分比和 Stripe 订阅详情。" />
          ) : null}
          <UsageSpend account={account} range={range} onRange={setRange} />
        </>
      )}
    </div>
  );
}

function SubscriptionBill({ account, onReload }: { account: Account; onReload: () => Promise<void> }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const tried = useRef(false);
  const b = account.billing;
  const can = canUseDashboard(account);

  async function check() {
    setBusy(true);
    setError(null);
    try {
      await accounts.refreshBilling(account.id);
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
    // 只在第一次打开、还没有快照时自动查。失败后交给按钮，避免抽屉一开就连打门户。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [account.id]);

  const refresh = (
    <button
      type="button"
      className="btn btn-sm btn-icon btn-soft"
      data-tip={busy ? "正在读账单…" : "检查订阅账单"}
      aria-label="检查订阅账单"
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
          <span className="bill-k">订阅账单</span>
          {refresh}
        </div>
        <p className="bill-none" style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <Spinner />
          正在读 Stripe 账单门户…
        </p>
      </section>
    );
  }

  if (!b) {
    return (
      <section className="sub-hero">
        <div className="sub-hero-top">
          <div className="bill-hero-main">
            <span className="bill-k">订阅账单</span>
            <span className="bill-big" style={{ fontSize: 18, letterSpacing: "-0.02em" }}>
              {can ? "还没查过订阅实付" : "现在查不了订阅账单"}
            </span>
            <span className="bill-sub">
              {can
                ? "打开 Cursor 的 Stripe 门户，读套餐标价、优惠券和历史发票。门户密钥不会留下。"
                : "需要 refresh_token 或还活着的 session token。"}
            </span>
          </div>
          {can ? (
            <button type="button" className="btn btn-sm btn-primary" disabled={busy} onClick={() => void check()}>
              {busy ? <Spinner /> : <Icon name="refresh" size={13} />}
              检查账单
            </button>
          ) : null}
        </div>
        <ErrorNote error={error} />
      </section>
    );
  }

  const ccy = b.currency;
  const tone = heroTone(b);
  const list = b.listPrice;
  const payable = b.currentAmount;
  const discounted = b.discountState === "active" && list != null && payable != null && payable < list;
  const free = discounted && payable === 0;
  const name = productName(b);
  const cadence = [name, intervalLabel(b.interval)].filter(Boolean).join(" · ");
  const alert = subStatusAlert(b.subscriptionStatus);

  return (
    <div className="stack" style={{ gap: 14 }}>
      <section className={`sub-hero ${tone}`}>
        <div className="sub-hero-top">
          <div className="bill-hero-main">
            <span className="bill-k">{cadence || "订阅账单"}</span>
            <span className="sub-price">
              {discounted ? (
                <>
                  <span className="sub-strike num">{moneyFx(list, ccy)}</span>
                  <span className="sub-arrow" aria-hidden>
                    →
                  </span>
                  <span className={`bill-big num ${free ? "is-deal" : ""}`}>{moneyFx(payable, ccy)}</span>
                </>
              ) : (
                <span className="bill-big num">{moneyFx(payable ?? list, ccy)}</span>
              )}
            </span>
            <span className="bill-sub">{renewalCopy(b)}</span>
          </div>
          <div className="sub-hero-meta">
            <DiscountPill state={b.discountState} />
            {refresh}
          </div>
        </div>

        {b.discount && b.discountState !== "none" ? <CouponCard discount={b.discount} currency={ccy} /> : null}

        <dl className="facts facts-4">
          <div>
            <dt>标价</dt>
            <dd>{moneyFx(list, ccy)}</dd>
            <small>{cadence || "门户订阅行"}</small>
          </div>
          <div>
            <dt>{b.discountState === "active" ? "下次应付" : "下次续费"}</dt>
            <dd className={free ? "is-ok" : undefined}>{moneyFx(payable, ccy)}</dd>
            <small>{b.discountState === "active" ? "已算上当前折扣" : "按当前订阅"}</small>
          </div>
          <div>
            <dt>账期</dt>
            <dd className="num" style={{ fontSize: 12 }}>
              {shortDateTime(b.currentPeriodStart)} → {shortDateTime(b.currentPeriodEnd)}
            </dd>
            <small>{collectionLabel(b.collectionMethod) || "—"}</small>
          </div>
          <div>
            <dt>订阅</dt>
            <dd className={alert === "bad" ? "is-bad" : alert === "warn" ? "is-warn" : undefined}>
              {b.cancelAtPeriodEnd ? "账期末取消" : subStatusLabel(b.subscriptionStatus) || "—"}
            </dd>
            <small>{ccy ? ccy.toUpperCase() : "币种未知"}</small>
          </div>
        </dl>
      </section>

      <ErrorNote error={error} />

      {b.discountState === "unknown" ? (
        <Banner tone="warn" title="折扣状态未知" hint="门户回来了，但订阅对象不完整，不能当成「没有折扣」。再检查一次。" />
      ) : null}

      <section className="sect">
        <div className="sect-cap">
          <span>历史发票</span>
          <span className="sect-aside">
            {b.invoices?.length ? `${b.invoices.length} 张 · ${timeAgo(b.fetchedAt)}更新` : timeAgo(b.fetchedAt) + "更新"}
          </span>
        </div>
        {!b.invoices?.length ? (
          <p className="sect-none">门户里没有发票。新号或团队成员常见。</p>
        ) : (
          <div className="inv-rows">
            {b.invoices.map((inv, i) => (
              <InvoiceRow key={`${inv.number ?? inv.created ?? i}`} inv={inv} fallbackCurrency={ccy} />
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

function DiscountPill({ state }: { state: AccountBilling["discountState"] }) {
  const tone = state === "active" ? "ok" : state === "expired" ? "warn" : state === "none" ? "default" : "info";
  return <Tag tone={tone}>{discountStateLabel(state)}</Tag>;
}

function CouponCard({ discount, currency }: { discount: NonNullable<AccountBilling["discount"]>; currency?: string }) {
  const off = discountOffText(discount, currency);
  const how = durationText(discount);
  const until = discount.endsAt ? `至 ${shortDate(discount.endsAt)}` : "";
  return (
    <div className="coupon">
      <span className="coupon-mark" aria-hidden>
        %
      </span>
      <span className="coupon-body">
        <span className="coupon-name">{discount.name || "优惠券"}</span>
        <span className="coupon-how">
          {[how, until].filter(Boolean).join(" · ") || "门户未写有效方式"}
        </span>
      </span>
      {off ? <span className="coupon-off num">{off}</span> : null}
    </div>
  );
}

function InvoiceRow({ inv, fallbackCurrency }: { inv: BillingInvoice; fallbackCurrency?: string }) {
  const ccy = inv.currency ?? fallbackCurrency;
  const coupons = (inv.discounts ?? []).map((d) => d.name).filter(Boolean).join(" · ");
  const line = inv.lines?.[0]?.description || inv.description || "发票";
  const paid = inv.amountPaid ?? inv.total;
  const listed = inv.subtotal;
  const cut = listed != null && paid != null && paid < listed;
  return (
    <div className="inv-row">
      <span className="inv-when num">{shortDate(inv.created)}</span>
      <span className="inv-what">
        <span className="truncate">{line}</span>
        {coupons ? <small>{coupons}</small> : null}
      </span>
      <span className={`inv-st is-${inv.status ?? "open"}`}>{invoiceStatusLabel(inv.status) || "—"}</span>
      <span className="inv-pay num">
        {cut ? <s>{moneyFx(listed, ccy)}</s> : null}
        <b>{moneyFx(paid, ccy)}</b>
      </span>
    </div>
  );
}

function heroTone(b: AccountBilling): string {
  if (b.discountState === "active" && (b.currentAmount ?? 1) === 0) return "is-free";
  if (b.discountState === "active") return "is-deal";
  return "";
}

function renewalCopy(b: AccountBilling): ReactNode {
  const ccy = b.currency;
  if (b.cancelAtPeriodEnd) {
    return (
      <>
        已预约在 <b className="num">{shortDate(b.currentPeriodEnd)}</b> 取消，到期不再扣款
      </>
    );
  }
  if (b.discountState === "active") {
    return (
      <>
        下次按 <b className="num">{moneyFx(b.currentAmount, ccy)}</b> 续
        {b.listPrice != null ? (
          <>
            <span className="bill-sub-sep">·</span>
            标价 <b className="num">{moneyFx(b.listPrice, ccy)}</b>
          </>
        ) : null}
        {b.discount?.name ? (
          <>
            <span className="bill-sub-sep">·</span>
            {b.discount.name}
          </>
        ) : null}
      </>
    );
  }
  if (b.discountState === "expired") {
    return (
      <>
        券已经用完，下次按标价 <b className="num">{moneyFx(b.listPrice ?? b.currentAmount, ccy)}</b> 续
      </>
    );
  }
  if (b.discountState === "none") {
    return (
      <>
        当前没有折扣，下次按 <b className="num">{moneyFx(b.currentAmount ?? b.listPrice, ccy)}</b> 续
      </>
    );
  }
  return <>门户没有给出完整的折扣字段，下次扣多少还不能下结论</>;
}

/* ── 用量花费（原来的账单页）─────────────────────────────────────────────── */

type BillRange = "cycle" | "week" | "today";

const RANGE_LABEL: Record<BillRange, string> = { cycle: "本账期", week: "近 7 天", today: "今天" };
const RANGE_HEAD: Record<BillRange, string> = { cycle: "本期消费", week: "近 7 天消费", today: "今天消费" };

interface RangeView {
  cents: number;
  models: ModelUsage[];
  tokens: { input: number; output: number; cacheRead: number; cacheWrite: number };
}

function rangeView(u: NonNullable<Account["usage"]>, range: BillRange): RangeView {
  if (range !== "cycle") {
    const w = range === "week" ? u.week : u.today;
    const models = [...(w?.byModel ?? [])].sort((a, b) => b.cents - a.cents);
    return {
      cents: w?.cents ?? 0,
      models,
      tokens: { input: w?.inputTokens ?? 0, output: w?.outputTokens ?? 0, cacheRead: w?.cacheReadTokens ?? 0, cacheWrite: w?.cacheWriteTokens ?? 0 },
    };
  }
  const models = [...(u.byModel ?? [])].sort((a, b) => b.cents - a.cents);
  const sum = (pick: (m: ModelUsage) => number) => models.reduce((s, m) => s + pick(m), 0);
  return {
    cents: u.spendCents ?? sum((m) => m.cents),
    models,
    tokens: {
      input: u.inputTokens ?? sum((m) => m.input),
      output: u.outputTokens ?? sum((m) => m.output),
      cacheRead: u.cacheReadTokens ?? sum((m) => m.cacheRead),
      cacheWrite: u.cacheWriteTokens ?? sum((m) => m.cacheWrite),
    },
  };
}

function UsageSpend({
  account,
  range,
  onRange,
}: {
  account: Account;
  range: BillRange;
  onRange: (r: BillRange) => void;
}) {
  const u = account.usage!;
  const now = Date.now();
  const hasWindows = Boolean(u.today && u.week);
  const shown: BillRange = hasWindows ? range : "cycle";
  const view = rangeView(u, shown);
  const pace = spendPace(u, now);
  const od = onDemandText(u);
  const cycleSpend = u.spendCents ?? 0;

  const byTier = view.models.reduce(
    (acc, m) => {
      if (m.tier === 2) acc.auto += m.cents;
      else if (m.tier === 1) acc.api += m.cents;
      else acc.other += m.cents;
      return acc;
    },
    { auto: 0, api: 0, other: 0 },
  );
  const tierTotal = byTier.auto + byTier.api + byTier.other;

  let sub: ReactNode = null;
  if (shown === "cycle") {
    // 四本账各说各的：订阅额度还剩多少、按需扣了多少、免费加量白送了多少、积分还剩几个。
    // 以前拿 included + bonus 对着额度算「超支」，一个 Pro 号会显示成「超支 $68」，可那是白送的。
    const bonus = bonusSpend(u);
    const onDemandUsed = u.onDemandEnabled && (u.onDemandUsedCents ?? 0) > 0 ? u.onDemandUsedCents! : null;
    sub = pace?.budget != null ? (
      <>
        额度 <b className="num">{money(pace.budget)}</b>
        <span className="bill-sub-sep">·</span>
        还剩 <b className="num">{money(pace.remaining)}</b>
        {onDemandUsed != null ? (
          <span className="is-warn">
            <span className="bill-sub-sep">·</span>按需 <b className="num">{money(onDemandUsed)}</b>
          </span>
        ) : null}
        {bonus != null ? (
          <>
            <span className="bill-sub-sep">·</span>Auto <b className="num">{money(bonus)}</b>
          </>
        ) : null}
        {u.creditGrantRemainingCents != null ? (
          <>
            <span className="bill-sub-sep">·</span>积分 <b className="num">{creditPoints(u.creditGrantRemainingCents)}</b>
          </>
        ) : null}
      </>
    ) : (
      <>
        账期 <b className="num mono">{shortDateTime(u.cycleStart)} → {shortDateTime(u.cycleEnd)}</b>
      </>
    );
  } else {
    const share = cycleSpend > 0 ? Math.min(100, (view.cents / cycleSpend) * 100) : null;
    const perDay = shown === "week" ? view.cents / 7 : null;
    const vsAvg = shown === "today" && pace && pace.perDay > 0 ? ((view.cents - pace.perDay) / pace.perDay) * 100 : null;
    sub = (
      <>
        {perDay != null ? (
          <>
            日均 <b className="num">{money(perDay)}</b>
          </>
        ) : null}
        {vsAvg != null ? (
          <>
            本期日均 <b className="num">{money(pace!.perDay)}</b>
            <span className="bill-sub-sep">·</span>
            {Math.abs(vsAvg) < 5 ? "和日均差不多" : vsAvg > 0 ? <span className="is-warn">比日均高 {Math.round(vsAvg)}%</span> : <span className="is-ok">比日均低 {Math.round(-vsAvg)}%</span>}
          </>
        ) : null}
        {share != null ? (
          <>
            <span className="bill-sub-sep">·</span>占本期 <b className="num">{share < 1 && view.cents > 0 ? "<1" : Math.round(share)}%</b>
          </>
        ) : null}
      </>
    );
  }

  return (
    <>
      <section className="bill-hero">
        <div className="bill-hero-top">
          <div className="bill-hero-main">
            <span className="bill-k">{RANGE_HEAD[shown]}</span>
            <span className="bill-big num">{money(view.cents)}</span>
            <span className="bill-sub">{sub}</span>
          </div>
          {hasWindows ? (
            <div className="range" role="tablist" aria-label="账单范围">
              {(Object.keys(RANGE_LABEL) as BillRange[]).map((r) => (
                <button key={r} type="button" role="tab" className="range-opt" aria-selected={shown === r} onClick={() => onRange(r)}>
                  {RANGE_LABEL[r]}
                </button>
              ))}
            </div>
          ) : null}
        </div>

        {tierTotal > 0 ? (
          <div className="bill-split">
            <div className="bill-split-cap">
              <span>消费分类构成</span>
              <span className="num faint">共 {money(tierTotal)}</span>
            </div>
            <span className="bill-split-bar">
              {byTier.auto > 0 ? <i className="is-auto" style={{ width: `${(byTier.auto / tierTotal) * 100}%` }} title={`Auto ${money(byTier.auto)} (${Math.round((byTier.auto / tierTotal) * 100)}%)`} /> : null}
              {byTier.api > 0 ? <i className="is-api" style={{ width: `${(byTier.api / tierTotal) * 100}%` }} title={`点名 API ${money(byTier.api)} (${Math.round((byTier.api / tierTotal) * 100)}%)`} /> : null}
              {byTier.other > 0 ? <i className="is-other" style={{ width: `${(byTier.other / tierTotal) * 100}%` }} title={`其他 ${money(byTier.other)} (${Math.round((byTier.other / tierTotal) * 100)}%)`} /> : null}
            </span>
            <span className="bill-legend">
              {byTier.auto > 0 ? (
                <span>
                  <i className="is-auto" />
                  Auto 消费 <b className="num">{money(byTier.auto)}</b>
                  <span className="bill-legend-pct num">({Math.round((byTier.auto / tierTotal) * 100)}%)</span>
                </span>
              ) : null}
              {byTier.api > 0 ? (
                <span>
                  <i className="is-api" />
                  点名 API 消费 <b className="num">{money(byTier.api)}</b>
                  <span className="bill-legend-pct num">({Math.round((byTier.api / tierTotal) * 100)}%)</span>
                </span>
              ) : null}
              {byTier.other > 0 ? (
                <span>
                  <i className="is-other" />
                  其他消费 <b className="num">{money(byTier.other)}</b>
                  <span className="bill-legend-pct num">({Math.round((byTier.other / tierTotal) * 100)}%)</span>
                </span>
              ) : null}
            </span>
          </div>
        ) : shown !== "cycle" ? (
          <p className="bill-none">{shown === "today" ? "今天还没有消费。" : "近 7 天没有消费。"}</p>
        ) : null}
      </section>

      {!hasWindows && canQueryUsage(account) ? <p className="sect-none">刷新一次用量，就能按「今天 / 近 7 天」看花费。</p> : null}

      {shown === "cycle" && pace ? (
        <section className="sect">
          <div className="sect-cap">
            <span>账期节奏</span>
            <span className="sect-aside num">
              账期第 {Math.ceil(pace.elapsedDays)} / {Math.round(pace.totalDays)} 天
            </span>
          </div>

          <div style={{ marginBottom: 4 }}>
            <PaceBar pace={pace} />
          </div>

          <dl className="facts facts-4">
            <div>
              <dt>日均</dt>
              <dd>{moneyShort(pace.perDay)}</dd>
              <small>近 {daysText(pace.elapsedDays)}</small>
            </div>
            <div>
              <dt>预计账期末</dt>
              <dd className={pace.budget != null && pace.projected > pace.budget ? "is-warn" : undefined}>{moneyShort(pace.projected)}</dd>
              <small>{pace.budget == null ? "照当前日均" : pace.projected > pace.budget ? `超额度 ${moneyShort(pace.projected - pace.budget)}` : "在额度内"}</small>
            </div>
            <div>
              <dt>剩余额度</dt>
              <dd className={pace.remaining != null && pace.remaining <= 0 ? "is-bad" : undefined}>{pace.remaining == null ? "—" : pace.remaining <= 0 ? "已用完" : moneyShort(pace.remaining)}</dd>
              <small>
                {pace.remaining == null
                  ? "没有额度信息"
                  : pace.remaining <= 0
                    ? "超出的部分走按需"
                    : pace.runwayDays == null
                      ? "还没开始花"
                      : pace.runwayDays >= pace.totalDays - pace.elapsedDays
                        ? "够用到重置"
                        : `照日均还能撑 ${daysText(pace.runwayDays)}`}
              </small>
            </div>
            <div>
              <dt>按需</dt>
              <dd>{u.onDemandEnabled ? od.value : "未开启"}</dd>
              <small>{u.onDemandEnabled ? od.sub : "额度用完即停"}</small>
            </div>
          </dl>
        </section>
      ) : null}

      <section className="sect">
        <div className="sect-cap">
          <span>按模型消费</span>
          <span className="sect-aside">{view.models.length ? `${view.models.length} 个模型 · 按花费排序` : null}</span>
        </div>
        {view.models.length === 0 ? (
          <p className="sect-none">{shown === "cycle" ? "这个账期还没有按模型的消费明细。" : "这段时间没有按模型的消费明细。"}</p>
        ) : (
          <ModelRanking rows={view.models} />
        )}
      </section>

      {view.tokens.input || view.tokens.output ? (
        <section className="sect">
          <div className="sect-cap">
            <span>Tokens 统计</span>
            <span className="sect-aside">{RANGE_LABEL[shown]}</span>
          </div>
          <dl className="facts facts-4">
            <div>
              <dt>输入</dt>
              <dd>{compactNumber(view.tokens.input)}</dd>
            </div>
            <div>
              <dt>输出</dt>
              <dd>{compactNumber(view.tokens.output)}</dd>
            </div>
            <div>
              <dt>缓存读</dt>
              <dd>{compactNumber(view.tokens.cacheRead)}</dd>
            </div>
            <div>
              <dt>缓存写</dt>
              <dd>{compactNumber(view.tokens.cacheWrite)}</dd>
            </div>
          </dl>
        </section>
      ) : null}

      <UsageEventsSection account={account} range={shown} />
    </>
  );
}

function PaceBar({ pace }: { pace: SpendPace }) {
  const spentPct = pace.budget! > 0 ? Math.min(100, ((pace.budget! - pace.remaining!) / pace.budget!) * 100) : 0;
  const timePct = Math.min(100, (pace.elapsedDays / pace.totalDays) * 100);
  const ahead = pace.aheadPct ?? 0;
  const verdict = pace.remaining! <= 0 ? { tone: "is-bad", text: "额度已用完" } : ahead > 8 ? { tone: "is-warn", text: `比时间快 ${Math.round(ahead)} 个点` } : ahead < -8 ? { tone: "is-ok", text: `比时间慢 ${Math.round(-ahead)} 个点` } : { tone: "", text: "节奏正常" };
  return (
    <div className="pace">
      <span className="pace-track" title={`额度已用 ${Math.round(spentPct)}% · 账期已过 ${Math.round(timePct)}%`}>
        <i className="pace-fill" style={{ width: `${meterWidth(spentPct)}%`, background: meterColor(spentPct) }} />
        <i className="pace-tick" style={{ left: `${timePct}%` }} />
      </span>
      <span className="pace-legend num">
        <span>
          额度已用 <b>{pctText(spentPct)}</b>
        </span>
        <span className="pace-time">
          <i />
          账期已过 <b>{Math.round(timePct)}%</b>
        </span>
        <span className={`pace-verdict ${verdict.tone}`}>{verdict.text}</span>
      </span>
    </div>
  );
}

function ModelRanking({ rows }: { rows: ModelUsage[] }) {
  const top = rows[0]?.cents || 0;
  return (
    <div className="bill-rows">
      {rows.map((m, i) => (
        <div key={m.model} className="bill-row" title={`${m.model} · 输入 ${compactNumber(m.input)} · 输出 ${compactNumber(m.output)}${m.cacheRead ? ` · 缓存读 ${compactNumber(m.cacheRead)}` : ""}`}>
          <span className="bill-i num">{i + 1}</span>
          <span className="bill-model">
            <span className="mono truncate">{m.model}</span>
            {m.tier === 2 ? <span className="bill-tier is-auto">Auto</span> : m.tier === 1 ? <span className="bill-tier is-api">API</span> : null}
          </span>
          <span className="bill-bar">
            <i className={m.tier === 2 ? "is-auto" : m.tier === 1 ? "is-api" : "is-other"} style={{ width: `${top > 0 ? Math.max(2, (m.cents / top) * 100) : 0}%` }} />
          </span>
          <span className="bill-cents num">{money(m.cents)}</span>
          <span className="bill-tok num">
            {compactNumber(m.input)} <span className="faint">/</span> {compactNumber(m.output)}
          </span>
        </div>
      ))}
    </div>
  );
}

function formatCallTime(ts: number): { time: string; sub: string } {
  if (!ts) return { time: "—", sub: "" };
  const d = new Date(ts);
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  const h = String(d.getHours()).padStart(2, "0");
  const min = String(d.getMinutes()).padStart(2, "0");
  const s = String(d.getSeconds()).padStart(2, "0");
  return {
    time: `${h}:${min}:${s}`,
    sub: `${m}-${day}`,
  };
}

function UsageEventsSection({
  account,
  range,
}: {
  account: Account;
  range: BillRange;
}) {
  const [report, setReport] = useState<UsageEventsReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [page, setPage] = useState(1);
  const can = canUseDashboard(account) || account.hasApiKey;
  const u = account.usage;

  const { startMs, endMs } = useMemo(() => {
    const now = Date.now();
    if (range === "today") {
      return { startMs: startOfLocalDay(now), endMs: now };
    }
    if (range === "week") {
      return { startMs: startOfLocalDay(now) - 6 * 86_400_000, endMs: now };
    }
    return { startMs: u?.cycleStart, endMs: u?.cycleEnd };
  }, [range, u?.cycleStart, u?.cycleEnd]);

  const loadEvents = useCallback(
    async (targetPage = 1) => {
      if (!can) return;
      setLoading(true);
      setError(null);
      try {
        const res = await accounts.listUsageEvents(account.id, {
          page: targetPage,
          pageSize: 25,
          startMs,
          endMs,
        });
        setReport(res);
        setPage(targetPage);
      } catch (err) {
        setError(err);
      } finally {
        setLoading(false);
      }
    },
    [account.id, can, startMs, endMs],
  );

  useEffect(() => {
    void loadEvents(1);
  }, [loadEvents]);

  const totalPages = report ? Math.max(1, Math.ceil(report.totalCount / report.pageSize)) : 1;

  return (
    <section className="sect">
      <div className="sect-cap">
        <span>调用明细</span>
        <span className="sect-aside">
          {report ? `共 ${report.totalCount} 次 · ${RANGE_LABEL[range]}` : RANGE_LABEL[range]}
        </span>
        <div style={{ marginLeft: "auto", display: "flex", alignItems: "center", gap: 6 }}>
          <button
            type="button"
            className="btn btn-sm btn-icon btn-quiet"
            disabled={loading || !can}
            data-tip={loading ? "正在拉取…" : "刷新调用明细"}
            aria-label="刷新调用明细"
            onClick={() => void loadEvents(page)}
          >
            <Icon name="refresh" size={13} className={loading ? "is-spinning" : undefined} />
          </button>
        </div>
      </div>

      {!can ? (
        <p className="sect-none">需要有效 session token 或 crsr_ API Key 才能查看官方每次调用明细。</p>
      ) : error ? (
        <div className="stack" style={{ gap: 8 }}>
          <ErrorNote error={error} />
          <div>
            <button type="button" className="btn btn-sm btn-quiet" onClick={() => void loadEvents(page)}>
              重试拉取
            </button>
          </div>
        </div>
      ) : loading && (!report || report.events.length === 0) ? (
        <div className="call-loading">
          <Spinner />
          <span>正在拉取官方调用记录…</span>
        </div>
      ) : !report || report.events.length === 0 ? (
        <p className="sect-none">{range === "today" ? "今天暂无调用记录。" : range === "week" ? "近 7 天暂无调用记录。" : "本账期暂无调用记录。"}</p>
      ) : (
        <div className="call-table">
          <div className="call-head">
            <span>时间</span>
            <span>模型</span>
            <span>类型</span>
            <span>Tokens</span>
            <span>费用</span>
          </div>

          <div className="call-rows">
            {report.events.map((ev, i) => {
              const t = formatCallTime(ev.timestamp);
              const isCharged = ev.chargedCents > 0;
              const isIncluded = !isCharged && (Boolean(ev.kind && /included/i.test(ev.kind)) || ev.totalCents === 0);
              const totalTok = ev.inputTokens + ev.outputTokens;

              return (
                <div key={`${ev.timestamp}-${i}`} className="call-row">
                  <div className="call-time" title={ev.timestamp ? new Date(ev.timestamp).toLocaleString("zh-CN") : undefined}>
                    <span className="call-time-val num">{t.time}</span>
                    <span className="call-time-date num">{t.sub}</span>
                  </div>

                  <div className="call-model-wrap">
                    <span className="call-model-name mono truncate" title={ev.model}>
                      {ev.model || "—"}
                    </span>
                    {ev.isHeadless ? <span className="call-headless-tag">后台</span> : null}
                  </div>

                  <div>
                    <span className={`call-kind ${isCharged ? "is-warn" : isIncluded ? "is-included" : "is-quiet"}`}>
                      {isCharged ? "按需计费" : isIncluded ? "额度内" : ev.kind || "标准"}
                    </span>
                  </div>

                  <div
                    className="call-tokens-cell"
                    title={`输入: ${compactNumber(ev.inputTokens)} · 输出: ${compactNumber(ev.outputTokens)}${ev.cacheReadTokens ? ` · 缓存读: ${compactNumber(ev.cacheReadTokens)}` : ""}${ev.cacheWriteTokens ? ` · 缓存写: ${compactNumber(ev.cacheWriteTokens)}` : ""}`}
                  >
                    <span className="call-tokens-total num">{compactNumber(totalTok)}</span>
                    <span className="call-tokens-detail faint">
                      入 {compactNumber(ev.inputTokens)} / 出 {compactNumber(ev.outputTokens)}
                      {ev.cacheReadTokens ? ` · 读 ${compactNumber(ev.cacheReadTokens)}` : ""}
                    </span>
                  </div>

                  <div className="call-cost-cell">
                    {ev.chargedCents > 0 ? (
                      <span className="call-cost-charged num">${(ev.chargedCents / 100).toFixed(2)}</span>
                    ) : ev.totalCents > 0 ? (
                      <span className="call-cost-included num" title={`折算 ${money(ev.totalCents)}`}>
                        额度扣除
                      </span>
                    ) : (
                      <span className="call-cost-free faint">免费</span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>

          {totalPages > 1 || report.totalCount > report.pageSize ? (
            <div className="call-pagination">
              <span className="call-page-stat num faint">
                共 <b>{report.totalCount}</b> 次调用 · 第 {report.page} / {totalPages} 页
              </span>
              <div className="call-page-btns">
                <button
                  type="button"
                  className="btn btn-sm btn-quiet"
                  disabled={page <= 1 || loading}
                  onClick={() => void loadEvents(page - 1)}
                >
                  ‹ 上一页
                </button>
                <button
                  type="button"
                  className="btn btn-sm btn-quiet"
                  disabled={page >= totalPages || loading}
                  onClick={() => void loadEvents(page + 1)}
                >
                  下一页 ›
                </button>
              </div>
            </div>
          ) : null}
        </div>
      )}
    </section>
  );
}
