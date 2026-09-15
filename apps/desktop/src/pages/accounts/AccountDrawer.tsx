/**
 * 一个账号的详情：右侧抽屉。
 *
 * 抽屉而不是居中弹窗：列表还在左边，关掉之后眼睛仍落在刚才那张卡上；纵向空间也
 * 更宽裕，账单里的模型明细不用再挤。也**不在卡内展开** —— 详情的形状和一张卡差太远。
 *
 * 标题区把这个号的「身份」摆齐：邮箱、档位、健康度、来源、备注（可就地改）。
 * 动作也在标题区 —— 刷新 / 授权 / 切号 是来这一页最常按的三个键，不该藏在页脚。
 * 「切号」就地热切：Cursor 在跑时不退出；只有开了「切换时同时切机器码」才会问一句。
 * 仅会话（token 导入、没有 refresh）的号，有效期内同样能切。
 *
 * 标题区下面先答「它在哪儿被用着」：切号池里有没有、网关号池里有没有，各一行，能就地加入 /
 * 移出。账号总库是一份、两个使用池是子集（ARCHITECTURE §5.2），以前要走到那两页才知道一个号进了
 * 没进，现在在这个号自己的抽屉里就说清。
 *
 * 然后分四页：用量 / 账单 / 凭证 / Grok Bot。来意不同，一次只有一种。
 * 账单页拆成两张卡：上面是 Stripe 订阅实付（标价 / 券 / 发票），下面是用量花费。
 * 「踢掉其它会话」住在凭证页 —— 它管的是这个号的登录态，跟凭证是一回事。
 *
 * 页脚只剩两样：什么时候加的、删除。删除要按两次 —— 它会连凭证一起清掉，没有回头路。
 */
import { useEffect, useState, type ReactNode } from "react";
import type { AccountPlacement } from "../../accounts/model";
import { GATEWAY_MEMBERSHIP_LABEL, inGatewayRoster, usePools } from "../../accounts/pools";
import type { Account, CrsrStatus, KickOutcome, SecretKind } from "../../ipc/types";
import { accounts, crsr, gateway as gatewayApi, switcher as switcherApi } from "../../ipc/api";
import { Banner, CopyButton, Drawer, ErrorNote, Gauge, Health, Icon, Reset, Spinner, Switch, Tag } from "../../ui/primitives";
import { accountSourceLabel, timeAgo, timeUntil } from "../../ui/format";
import { canQueryUsage, canUseDashboard, hasLiveAccess, sessionOnly } from "../../ui/accounts";
import { canAddToSwitchPool } from "../../ui/switcher";
import { GrokBotTab } from "./GrokBotTab";
import { BillTab } from "./BillTab";
import {
  accountProblem,
  blockReasonText,
  cycleProgress,
  isFresh,
  money,
  creditPoints,
  onDemandText,
  planBudget,
  planLabel,
  planTone,
  shortDate,
} from "../../ui/usage";

type Tab = "usage" | "bill" | "creds" | "grokbot";

/** 第四页「Grok Bot」：Bot 通道用这个号的 Grok 额度（顺带可把 Cursor 切到它）。 */
const TABS: Array<[Tab, string]> = [
  ["usage", "用量"],
  ["bill", "账单"],
  ["creds", "凭证"],
  ["grokbot", "Grok Bot"],
];

export function AccountDrawer({
  account,
  refreshing,
  onClose,
  onRefresh,
  onAuthorize,
  inCursor,
  onSwitch,
  placement,
  placementActions,
  actionError,
  onSaveNote,
  onReload,
  onRemove,
}: {
  account: Account;
  refreshing: boolean;
  onClose: () => void;
  onRefresh: () => void;
  onAuthorize: () => void;
  /** Cursor 此刻登着的就是这个号。那就没什么可切的 —— 按钮要说出来，不能装作能按。 */
  inCursor: boolean;
  /** 切入 Cursor。父级负责加入切号池并热切；这里只负责按钮的忙态。 */
  onSwitch: () => void | Promise<void>;
  /** 从哪个页面打开，以及这个场景怎样使用它。 */
  placement?: AccountPlacement;
  /** 切号池的移出、网关池的“用这个 / 移出”等场景动作。 */
  placementActions?: ReactNode;
  /** 从外层统一控制器执行刷新 / 删除等动作时的错误。 */
  actionError?: unknown;
  onSaveNote: (note: string) => Promise<void>;
  /** 改完凭证后重新拉一遍列表 —— 卡片上的「有没有 token」标记要跟着变。 */
  onReload: () => Promise<void>;
  onRemove: () => Promise<void>;
}) {
  const [tab, setTab] = useState<Tab>("usage");
  const [switching, setSwitching] = useState(false);
  const u = account.usage;
  const problem = accountProblem(account, u);
  const dead = account.status === "dead";
  const switchable = canAddToSwitchPool(account);

  async function switchNow() {
    setSwitching(true);
    try {
      await onSwitch();
    } finally {
      setSwitching(false);
    }
  }

  const head = (
    <div className="dr-id">
      <div className="dr-title">
        <span className="dr-email selectable truncate" title={account.email}>
          {account.email}
        </span>
        <CopyButton value={account.email} icon label="复制邮箱" />
      </div>
      <div className="dr-badges">
        {u?.plan ? <span className={`plan ${planTone(u)}`}>{planLabel(u)}</span> : null}
        <span className="dr-meta">
          {problem ? <Health tone={problem.tone}>{problem.label}</Health> : null}
          {/* 仅会话的号：此刻能用，但到期就掉——把到期时刻摆在最前，这是它最要紧的一件事。 */}
          {sessionOnly(account) && !problem ? (
            <Health tone={hasLiveAccess(account) ? "warn" : "bad"}>
              {hasLiveAccess(account) ? `仅会话 · ${timeUntil(Date.parse(account.accessExpiresAt!))}到期` : "会话已过期"}
            </Health>
          ) : null}
          {account.source !== "local" ? <span>{accountSourceLabel(account.source)}</span> : null}
          <span>{account.lastCheckedAt ? `用量 ${timeAgo(account.lastCheckedAt)}更新` : "还没查过用量"}</span>
        </span>
      </div>
      <NoteLine key={account.id} note={account.note ?? ""} onSave={onSaveNote} />
      <div className="dr-actions">
        {/* 刷新只留图标 —— 抽屉里这一排该只有「授权」和主动作带字。 */}
        {canQueryUsage(account) ? (
          <button
            type="button"
            className="btn btn-sm btn-icon btn-soft"
            data-tip={refreshing ? "刷新中…" : account.hasApiKey && !account.hasRefresh && !hasLiveAccess(account) ? "刷新基础用量（API Key）" : "刷新用量"}
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
        {inCursor ? (
          <button type="button" className="btn btn-sm dr-cta" disabled title="Cursor 现在登着的就是它">
            <Icon name="switcher" size={13} />
            Cursor 正在用
          </button>
        ) : (
          <button
            type="button"
            className="btn btn-sm btn-primary dr-cta"
            disabled={!switchable || switching}
            onClick={() => void switchNow()}
            title={
              !switchable
                ? dead
                  ? "这个号已失效"
                  : sessionOnly(account)
                    ? "session token 已过期，到凭证页粘一份新的"
                    : "需要一份还活着的 session token，或授权一次拿到 refresh_token"
                : sessionOnly(account)
                  ? "切入 Cursor（仅会话，到期会掉登录；Cursor 在跑时不重启）"
                  : "切入 Cursor（Cursor 在跑时不重启）"
            }
          >
            {switching ? <Spinner /> : <Icon name="switcher" size={13} />}
            {switching ? "切换中" : "切号"}
          </button>
        )}
      </div>
    </div>
  );

  const foot = (
    <>
      <span className="dr-foot-meta">
        {timeAgo(account.createdAt)}添加
        <span className="dr-foot-sep">·</span>
        {accountSourceLabel(account.source)}
      </span>
      <DeleteAccount
        onConfirm={async () => {
          await onRemove();
          onClose();
        }}
      />
    </>
  );

  return (
    <Drawer label={`账号详情 ${account.email}`} onClose={onClose} head={head} footer={foot}>
      <div className="dr-body">
        <ErrorNote error={actionError} />
        {account.lastError ? <Banner tone="warn" title="上次操作出了错" hint={account.lastError} /> : null}

        {placement && placement.kind !== "library" ? (
          <div className="account-context">
            <span className="account-context-copy">
              <strong>{placement.label}</strong>
              {placement.detail ? <span>{placement.detail}</span> : null}
            </span>
            {placementActions ? <span className="row">{placementActions}</span> : null}
          </div>
        ) : null}

        <UsedIn account={account} placement={placement} onChanged={onReload} />

        <div className="tabs tabs-block">
          {TABS.map(([id, label]) => (
            <button key={id} type="button" className="tab" aria-selected={tab === id} onClick={() => setTab(id)}>
              {label}
            </button>
          ))}
        </div>

        {tab === "usage" ? (
          <UsageTab account={account} refreshing={refreshing} onRefresh={onRefresh} onReload={onReload} />
        ) : null}
        {tab === "bill" ? <BillTab account={account} onReload={onReload} /> : null}
        {tab === "creds" ? <CredsTab account={account} onChanged={onReload} /> : null}
        {tab === "grokbot" ? <GrokBotTab account={account} /> : null}
      </div>
    </Drawer>
  );
}

/* ── 所在池 ───────────────────────────────────────────────────────────────── */

/**
 * 这个号在哪儿被用着：切号池一行、网关号池一行。
 *
 * 每行右边一个键：没进就「加入」，进了就「移出」。从切号页 / 网关页打开时，那一页的场景
 * 动作已经摆在上面的 `account-context` 里了，这里对应那一行就只报状态、不再放第二个键 ——
 * 同一件事两个入口会让人以为是两件事。
 */
function UsedIn({ account, placement, onChanged }: { account: Account; placement?: AccountPlacement; onChanged: () => Promise<void> }) {
  const pools = usePools();
  const [busy, setBusy] = useState<"switcher" | "gateway" | null>(null);
  const [error, setError] = useState<unknown>(null);
  const m = pools.membership(account.email);
  const gwIn = inGatewayRoster(m.gateway);
  const dead = account.status === "dead";

  async function act(which: "switcher" | "gateway", fn: () => Promise<unknown>) {
    setBusy(which);
    setError(null);
    try {
      await fn();
      await pools.reload();
      await onChanged();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(null);
    }
  }

  const switcherAction =
    placement?.kind === "switcher" ? null : m.switcher ? (
      <button
        type="button"
        className="btn btn-sm btn-quiet btn-danger"
        disabled={busy != null}
        onClick={() => {
          if (!window.confirm(`把 ${account.email} 移出切号池？会删掉为它保存的登录态快照；账号本身不受影响，随时能再加。`)) return;
          void act("switcher", () => switcherApi.remove(m.switcher!.id));
        }}
      >
        移出
      </button>
    ) : (
      <button
        type="button"
        className="btn btn-sm btn-soft"
        disabled={busy != null || !canAddToSwitchPool(account)}
        title={canAddToSwitchPool(account) ? "把它的登录态拷进切号池，之后可以一键切进 Cursor" : dead ? "这个号已失效" : sessionOnly(account) ? "session token 已过期，更新后再加入" : "需要一份还活着的 session token，或授权一次拿到 refresh_token"}
        onClick={() => void act("switcher", () => accounts.addToSwitchBook(account.id))}
      >
        {busy === "switcher" ? <Spinner /> : "加入"}
      </button>
    );

  const gatewayAction =
    placement?.kind === "gateway" ? null : gwIn ? (
      <button
        type="button"
        className="btn btn-sm btn-quiet btn-danger"
        disabled={busy != null}
        onClick={() => {
          if (m.gateway === "current" && !window.confirm(`${account.email} 正在被网关使用。移出后下一个请求会换号，正在进行的对话会丢上游缓存。继续？`)) return;
          void act("gateway", () => gatewayApi.unenroll(account.email));
        }}
      >
        移出
      </button>
    ) : (
      <button
        type="button"
        className="btn btn-sm btn-soft"
        disabled={busy != null || m.gateway !== "available"}
        title={m.gateway === "available" ? "交给网关，额度到线时接力用它" : dead ? "这个号已失效" : "网关用不了它：需要先授权拿到 refresh_token"}
        onClick={() => void act("gateway", () => gatewayApi.enroll([account.email]))}
      >
        {busy === "gateway" ? <Spinner /> : "加入"}
      </button>
    );

  return (
    <section>
      <ErrorNote error={error} />
      <div className="usedin">
      <div className={`usedin-row${m.switcher ? " is-in" : ""}`}>
        <span className="usedin-ico">
          <Icon name="switcher" size={13} />
        </span>
        <span className="usedin-k">切号池</span>
        <span className="usedin-v">
          {pools.loading ? "…" : m.switcher ? (m.switcher.isCurrent ? "已加入 · Cursor 当前登录" : m.switcher.lastSwitchedAt ? `已加入 · ${timeAgo(m.switcher.lastSwitchedAt)}切过` : "已加入") : "未加入"}
        </span>
        {switcherAction}
      </div>
      <div className={`usedin-row${gwIn ? " is-in" : ""}${m.gateway === "current" ? " is-live" : ""}${m.gateway === "skipped" ? " is-warn" : ""}`}>
        <span className="usedin-ico">
          <Icon name="gateway" size={13} />
        </span>
        <span className="usedin-k">本地网关</span>
        <span className="usedin-v">{pools.loading ? "…" : GATEWAY_MEMBERSHIP_LABEL[m.gateway]}</span>
        {gatewayAction}
      </div>
      </div>
    </section>
  );
}

/* ── 备注 ─────────────────────────────────────────────────────────────────── */

/** 备注就地改。点文字进入编辑，回车保存、Esc 放弃。 */
export function NoteLine({ note, onSave }: { note: string; onSave: (note: string) => Promise<void> }) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(note);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!editing) setDraft(note);
  }, [note, editing]);

  async function commit() {
    const next = draft.trim();
    if (next === note) {
      setEditing(false);
      return;
    }
    setSaving(true);
    try {
      await onSave(next);
      setEditing(false);
    } finally {
      setSaving(false);
    }
  }

  if (!editing) {
    return (
      <button type="button" className={note ? "note-line" : "note-line is-empty"} onClick={() => setEditing(true)}>
        <Icon name="pencil" size={12} />
        <span className="truncate">{note || "添加备注"}</span>
      </button>
    );
  }

  return (
    <form
      className="note-edit"
      onSubmit={(e) => {
        e.preventDefault();
        void commit();
      }}
    >
      <input
        className="input"
        autoFocus
        value={draft}
        disabled={saving}
        placeholder="这个号是谁的、用来干什么"
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          // 抽屉也在听 Esc；这里先拦住，免得放弃编辑的同时把抽屉也关了。
          if (e.key === "Escape") {
            e.stopPropagation();
            setEditing(false);
          }
        }}
      />
      <button type="submit" className="btn btn-sm btn-primary" disabled={saving}>
        {saving ? <Spinner /> : "保存"}
      </button>
      <button type="button" className="btn btn-sm" disabled={saving} onClick={() => setEditing(false)}>
        取消
      </button>
    </form>
  );
}

/* ── 踢会话 ───────────────────────────────────────────────────────────────── */

/**
 * 一键踢掉 Cursor 侧其它活跃会话（IDE / 网页 / 移动端…），尽量保留 Nexus 手里这把。
 * 对不上当前会话时会全踢，可能要重新授权 —— 所以要两步确认。
 */
function KickSessions({ accountId, onDone }: { accountId: string; onDone: () => Promise<void> }) {
  const [armed, setArmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [result, setResult] = useState<KickOutcome | null>(null);

  if (result) {
    const text = result.keptCurrent
      ? `已踢掉 ${result.revoked} 个，Nexus 这把已保留`
      : result.refreshAlive
        ? `已踢掉 ${result.revoked} 个；refresh_token 仍可用`
        : `已踢掉 ${result.revoked} 个；Nexus 这把没保住，请重新授权`;
    return (
      <div className="confirm">
        <span className={result.keptCurrent || result.refreshAlive ? "confirm-text is-ok" : "confirm-text"}>
          {text}
          {result.failed ? ` · ${result.failed} 个失败` : ""}
        </span>
        <button type="button" className="btn btn-sm" onClick={() => setResult(null)}>
          好
        </button>
      </div>
    );
  }

  if (!armed) {
    return (
      <button
        type="button"
        className="btn btn-sm btn-soft"
        style={{ alignSelf: "flex-start" }}
        onClick={() => {
          setError(null);
          setArmed(true);
        }}
      >
        <Icon name="logout" size={13} />
        踢掉其它会话
      </button>
    );
  }

  return (
    <div className="confirm">
      <span className="confirm-text">{error ? "没踢成，再试一次？" : "会踢掉除 Nexus 以外的全部 Cursor 登录；保不住 Nexus 这把时需重新授权。"}</span>
      <button type="button" className="btn btn-sm" disabled={busy} onClick={() => setArmed(false)}>
        取消
      </button>
      <button
        type="button"
        className="btn btn-sm btn-danger is-armed"
        disabled={busy}
        onClick={() =>
          void (async () => {
            setBusy(true);
            setError(null);
            try {
              const outcome = await accounts.kickSessions(accountId);
              setResult(outcome);
              setArmed(false);
              await onDone();
            } catch (err) {
              setError(err);
            } finally {
              setBusy(false);
            }
          })()
        }
      >
        {busy ? <Spinner /> : "确认踢掉"}
      </button>
    </div>
  );
}

/* ── 删除 ─────────────────────────────────────────────────────────────────── */

/**
 * 两步删除。第一步只是亮出后果，第二步才真删。
 *
 * 未上膛时是一个安静的文字键（红只出现在 hover）：页脚不该常驻一个红框，它会跟标题区
 * 那个主 CTA 抢注意力，而删除并不是这一屏想让人做的事。
 */
export function DeleteAccount({
  onConfirm,
  warning = "会连同 refresh_token 和密码一起从本机清除，不可恢复。",
}: {
  onConfirm: () => Promise<void>;
  warning?: string;
}) {
  const [armed, setArmed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  if (!armed) {
    return (
      <button type="button" className="btn btn-sm btn-quiet btn-danger" onClick={() => setArmed(true)}>
        <Icon name="trash" size={13} />
        删除账号
      </button>
    );
  }

  return (
    <div className="confirm">
      <span className="confirm-text">{error ? "删除失败，再试一次？" : warning}</span>
      <button type="button" className="btn btn-sm" disabled={busy} onClick={() => setArmed(false)}>
        取消
      </button>
      <button
        type="button"
        className="btn btn-sm btn-danger is-armed"
        disabled={busy}
        onClick={() =>
          void (async () => {
            setBusy(true);
            setError(null);
            try {
              await onConfirm();
            } catch (err) {
              setError(err);
              setBusy(false);
            }
          })()
        }
      >
        {busy ? <Spinner /> : "确认删除"}
      </button>
    </div>
  );
}

/* ── 用量 ─────────────────────────────────────────────────────────────────── */

function UsageTab({
  account,
  refreshing,
  onRefresh,
  onReload,
}: {
  account: Account;
  refreshing: boolean;
  onRefresh: () => void;
  onReload: () => Promise<void>;
}) {
  const u = account.usage;
  if (!u) {
    return (
      <div className="dr-blank">
        <p>还没查过这个号的用量。</p>
        {canQueryUsage(account) ? (
          <>
            <button type="button" className="btn btn-sm" disabled={refreshing} onClick={onRefresh}>
              {refreshing ? <Spinner /> : "拉一次"}
            </button>
            {account.hasApiKey && !account.hasRefresh && !hasLiveAccess(account) ? (
              <p className="faint tiny">走 API Key，只能看到花费流水，没有额度百分比。</p>
            ) : null}
          </>
        ) : (
          <p className="faint tiny">
            {sessionOnly(account)
              ? "会话已过期，到凭证页粘一份新的 session token 或 crsr_ API Key。"
              : "先授权拿到 refresh_token，或到凭证页填 crsr_ API Key 查基础用量。"}
          </p>
        )}
      </div>
    );
  }

  const now = Date.now();
  const bot = u.bot;
  const cycle = cycleProgress(u, now);
  const budget = planBudget(u);
  const spend = u.spendCents;
  const grantRemaining = u.creditGrantRemainingCents;
  const grantTotal = u.creditGrantTotalCents;
  const grantUsed = u.creditGrantUsedCents;
  const hasGrant = grantRemaining != null || grantTotal != null;

  if (u.via === "apiKey") {
    const models = [...(u.byModel ?? [])].sort((a, b) => b.cents - a.cents);
    return (
      <div className="stack" style={{ gap: 20 }}>
        <Banner tone="warn" title="基础用量" hint="session 不可用，这次是 crsr_ API Key 兑出来的逐条花费。没有额度百分比、账期和 Grok 周额。完整账单在「账单」页。" />
        <section className="sect">
          <div className="sect-cap">
            <span>花费</span>
            {u.plan ? <span className="sect-aside">{u.plan}</span> : null}
          </div>
          <dl className="facts">
            <div>
              <dt>合计</dt>
              <dd>{money(spend)}</dd>
              <small>这段时间的扣费合计</small>
            </div>
            <div>
              <dt>今天</dt>
              <dd>{u.today ? money(u.today.cents) : "—"}</dd>
              <small>本地零点起</small>
            </div>
            <div>
              <dt>近 7 天</dt>
              <dd>{u.week ? money(u.week.cents) : "—"}</dd>
              <small>含今天</small>
            </div>
          </dl>
        </section>
        <section className="sect">
          <div className="sect-cap">
            <span>按模型</span>
            <span className="sect-aside">{models.length ? `${models.length} 个` : null}</span>
          </div>
          {models.length === 0 ? (
            <p className="sect-none">这段时间没有按模型的消费明细。</p>
          ) : (
            <div className="kv">
              {models.map((m) => (
                <div key={m.model} className="kv-row">
                  <span className="kv-k">{m.model}</span>
                  <span className="kv-v num">{money(m.cents)}</span>
                </div>
              ))}
            </div>
          )}
        </section>
      </div>
    );
  }

  return (
    <div className="stack" style={{ gap: 20 }}>
      <section className="sect">
        <div className="sect-cap">
          <span>赠送积分</span>
          {hasGrant && grantTotal != null ? <span className="sect-aside">总额 {creditPoints(grantTotal)}</span> : null}
        </div>
        {!hasGrant ? (
          <p className="sect-none">这个号没有 Cursor 赠送的 credit grant（25 / 100 那种）。</p>
        ) : (
          <dl className="facts">
            <div>
              <dt>剩余</dt>
              <dd>{creditPoints(grantRemaining)}</dd>
              <small>还能花的赠送额度</small>
            </div>
            <div>
              <dt>已用</dt>
              <dd>{creditPoints(grantUsed)}</dd>
              <small>从赠送里扣掉的</small>
            </div>
            <div>
              <dt>总额</dt>
              <dd>{creditPoints(grantTotal)}</dd>
              <small>1 积分 = $1</small>
            </div>
          </dl>
        )}
      </section>

      <section className="sect">
        <div className="sect-cap">
          <span>Grok Bot</span>
          {bot?.planLabel ? <span className="sect-aside">{bot.planLabel}</span> : null}
        </div>
        {bot?.resetAt ? (
          <Reset
            label="周额重置"
            at={bot.resetAt}
            now={now}
            tag={bot.hasAvailable === false ? <Tag tone="bad">已耗尽</Tag> : isFresh(bot.periodStart, now) ? <Tag tone="ok">刚重置</Tag> : null}
          />
        ) : null}
        {!bot ? (
          <p className="sect-none">这个号没有 Grok Bot 周额（老档 pro-legacy 没有这套，属正常）。</p>
        ) : bot.access === "blocked" ? (
          <Banner tone="bad" title="无权限" hint={bot.blockReason ? blockReasonText(bot.blockReason) : undefined} />
        ) : (
          <Gauge label="周用量" percent={bot.percentUsed} title="Grok Bot 通道的周额度；和下面月账期是两套计量" />
        )}
      </section>

      <section className="sect">
        <div className="sect-cap">
          <span>月额度</span>
          <span className="sect-aside mono">
            {shortDate(u.cycleStart)} → {shortDate(u.cycleEnd)}
          </span>
        </div>
        <Reset
          label="月额重置"
          at={u.cycleEnd}
          now={now}
          tag={isFresh(u.cycleStart, now) ? <Tag tone="ok">刚重置</Tag> : null}
          progress={cycle}
        />
        <dl className="facts">
          <div>
            <dt>已用</dt>
            <dd>{money(spend)}</dd>
            <small>本账期 included + 赠送已花</small>
          </div>
          <div>
            <dt>订阅额度</dt>
            <dd>{money(budget)}</dd>
            <small>plan.limit</small>
          </div>
          <div>
            <dt>剩余</dt>
            <dd>{budget != null && spend != null ? money(Math.max(0, budget - spend)) : "—"}</dd>
            <small>{budget != null && spend != null && spend > budget ? "超出的走按需" : "到重置前还能花"}</small>
          </div>
        </dl>
        <Gauge
          label="总额度"
          percent={u.totalPercentUsed}
          note={budget != null || spend != null ? `${money(spend)} / ${money(budget)}` : undefined}
          title="月账期包含额度的整体已用比例"
        />
        <Gauge label="Auto" percent={u.autoPercentUsed} title="composer / grok 等由 Cursor 调度的模型" />
        <Gauge label="API" percent={u.apiPercentUsed} title="点名调用的 claude / gpt 等；打满后这类模型调不动" />
      </section>

      <OnDemandEditor account={account} onReload={onReload} />
    </div>
  );
}

function dollarsField(cents?: number | null): string {
  if (cents == null || !Number.isFinite(cents)) return "";
  const dollars = cents / 100;
  return Number.isInteger(dollars) ? String(dollars) : dollars.toFixed(2);
}

function parseLimitCents(raw: string): number | null | "invalid" {
  const t = raw.trim();
  if (!t) return null;
  const n = Number(t);
  if (!Number.isFinite(n) || n < 0) return "invalid";
  return Math.round(n * 100);
}

function OnDemandEditor({ account, onReload }: { account: Account; onReload: () => Promise<void> }) {
  const u = account.usage;
  const enabled = Boolean(u?.onDemandEnabled);
  const used = u?.onDemandUsedCents ?? 0;
  const storedLimit = u?.onDemandLimitCents ?? null;
  const od = onDemandText(u);
  const can = canUseDashboard(account);

  const [on, setOn] = useState(enabled);
  const [limitText, setLimitText] = useState(dollarsField(enabled ? storedLimit : null));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  useEffect(() => {
    setOn(enabled);
    setLimitText(dollarsField(enabled ? storedLimit : null));
    setError(null);
  }, [account.id, enabled, storedLimit]);

  const parsed = parseLimitCents(limitText);
  const currentLimit = enabled ? storedLimit : null;
  const dirty = on !== enabled || (on && parsed !== "invalid" && parsed !== currentLimit);

  async function save(nextOn: boolean, nextLimit: number | null) {
    if (nextOn && !enabled) {
      const cap =
        nextLimit == null
          ? "不设上限，额度用完会继续扣信用卡"
          : `上限 $${(nextLimit / 100).toFixed(0)}，额度用完后按需扣到这个数`;
      if (!window.confirm(`开启按需计费？${cap}。继续？`)) {
        setOn(enabled);
        return;
      }
    }
    if (!nextOn && enabled && !window.confirm("关闭按需计费后，包含额度用完即停。继续？")) {
      setOn(enabled);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await accounts.setOnDemand(account.id, nextOn, nextOn ? nextLimit : null);
      await onReload();
    } catch (err) {
      setError(err);
      setOn(enabled);
      setLimitText(dollarsField(enabled ? storedLimit : null));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="sect">
      <div className="sect-cap">
        <span>按需</span>
        <span className="sect-aside">{enabled ? od.sub : "额度用完即停"}</span>
      </div>
      <div className="od-bar">
        <Switch
          checked={on}
          disabled={!can || busy}
          label="按需计费"
          onChange={setOn}
        />
        <span className="od-used">
          已用 <b className="num">{money(used)}</b>
        </span>
        <input
          className="input od-limit"
          inputMode="decimal"
          placeholder="不封顶"
          disabled={!can || busy || !on}
          value={limitText}
          onChange={(e) => setLimitText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && dirty && parsed !== "invalid") {
              void save(on, parsed);
            }
          }}
          aria-label="按需上限（美元）"
        />
        <span className="od-unit">美元 / 月</span>
        <button
          type="button"
          className="btn btn-sm btn-primary"
          disabled={!can || busy || !dirty || parsed === "invalid"}
          onClick={() => void save(on, parsed === "invalid" ? null : parsed)}
        >
          {busy ? <Spinner /> : "保存"}
        </button>
      </div>
      {parsed === "invalid" ? <p className="sect-none">上限要是一个不小于 0 的数字；留空表示不封顶。</p> : null}
      {!can ? <p className="sect-none">需要一份还活着的 session token 才能改。</p> : null}
      {can && on && !limitText.trim() ? (
        <p className="sect-none">不填上限也能开；Cursor 对「不封顶」有时不认，填一个美元整数更稳。</p>
      ) : null}
      <ErrorNote error={error} />
    </section>
  );
}

/* ── 凭证 ─────────────────────────────────────────────────────────────────── */

/**
 * 改一条凭证。
 *
 * 输入框是明文的：用户正在**核对自己刚粘进去的东西对不对**，这时候打码只会碍事
 * （多一个空格都查不出来）。清除单独一个键，不靠「存一个空值」——那是两个意图。
 */
function SecretEditor({
  label,
  current,
  canClear,
  onCancel,
  onSave,
}: {
  label: string;
  current: string;
  canClear: boolean;
  onCancel: () => void;
  onSave: (value: string) => Promise<void>;
}) {
  const [value, setValue] = useState(current);
  const [busy, setBusy] = useState(false);

  const commit = (next: string) => {
    setBusy(true);
    void onSave(next).finally(() => setBusy(false));
  };

  return (
    <form
      className="kv-row is-editing"
      onSubmit={(e) => {
        e.preventDefault();
        if (value.trim()) commit(value);
      }}
    >
      <span className="kv-k">{label}</span>
      <span className="kv-v">
        <input
          className="input mono"
          autoFocus
          spellCheck={false}
          autoComplete="off"
          value={value}
          disabled={busy}
          placeholder="粘贴新的值"
          onChange={(e) => setValue(e.target.value)}
          onKeyDown={(e) => {
            // 抽屉也在听 Esc，先拦住，免得放弃编辑的同时把抽屉关了。
            if (e.key === "Escape") {
              e.stopPropagation();
              onCancel();
            }
          }}
        />
        <button type="submit" className="btn btn-sm btn-primary" disabled={busy || !value.trim()}>
          {busy ? <Spinner /> : "保存"}
        </button>
        {canClear ? (
          <button type="button" className="btn btn-sm btn-quiet btn-danger" disabled={busy} onClick={() => commit("")} title="从本机清除这一条">
            清除
          </button>
        ) : null}
        <button type="button" className="btn btn-sm btn-quiet" disabled={busy} onClick={onCancel}>
          取消
        </button>
      </span>
    </form>
  );
}

const SECRET_LABEL: Record<SecretKind, string> = {
  refresh: "refresh_token",
  access: "access token",
  cursorPassword: "Cursor 密码",
  emailPassword: "邮箱密码",
  recoveryEmail: "辅助邮箱",
  apiKey: "crsr_ API Key",
};

function CredsTab({ account, onChanged }: { account: Account; onChanged: () => Promise<void> }) {
  const [revealed, setRevealed] = useState<Partial<Record<SecretKind, string>>>({});
  /** 会话 token 是派生物，不在 `revealed` 那张表里：它没有「未保存 / 修改」这些状态。 */
  const [session, setSession] = useState<string | null>(null);
  const [sessionBusy, setSessionBusy] = useState(false);
  const [editing, setEditing] = useState<SecretKind | null>(null);
  const [error, setError] = useState<unknown>(null);

  // access 那一行只在它是这个号的命根子时才摆（仅会话）：有 refresh 的号 access 只是换 token 顺手
  // 存下的副产品，摆出来只会让人以为要维护它。
  const held: Array<[SecretKind, boolean]> = [
    ["refresh", account.hasRefresh],
    ...(!account.hasRefresh ? [["access", account.hasAccess] as [SecretKind, boolean]] : []),
    ["cursorPassword", account.hasPassword],
    ["emailPassword", account.hasEmailPassword],
    ["recoveryEmail", account.hasRecoveryEmail],
    ["apiKey", account.hasApiKey],
  ];
  const accessExpiry = account.hasAccess && account.accessExpiresAt ? Date.parse(account.accessExpiresAt) : null;

  async function reveal(kind: SecretKind) {
    try {
      const value = await accounts.revealSecret(account.id, kind);
      setRevealed((p) => ({ ...p, [kind]: value }));
      setError(null);
    } catch (err) {
      setError(err);
    }
  }

  function hide(kind: SecretKind) {
    setRevealed((p) => {
      const next = { ...p };
      delete next[kind];
      return next;
    });
  }

  async function revealSession() {
    setSessionBusy(true);
    try {
      setSession(await accounts.revealSession(account.id));
      setError(null);
    } catch (err) {
      setError(err);
    } finally {
      setSessionBusy(false);
    }
  }

  async function save(kind: SecretKind, value: string) {
    try {
      await accounts.setSecret(account.id, kind, value);
      hide(kind);
      setEditing(null);
      setError(null);
      await onChanged();
    } catch (err) {
      setError(err);
    }
  }

  return (
    <div className="stack" style={{ gap: 20 }}>
      <ErrorNote error={error} />

      <section className="sect">
        <div className="sect-cap">
          <span>登录凭证</span>
          <span className="sect-aside">明文只在你点「显示」时读出</span>
        </div>
        <div className="kv">
          <div className="kv-row">
            <span className="kv-k">邮箱</span>
            <span className="kv-v">
              <span className="selectable truncate">{account.email}</span>
              <CopyButton value={account.email} icon />
            </span>
          </div>
          {held.map(([kind, has]) => {
            const shown = revealed[kind];
            if (editing === kind) {
              return <SecretEditor key={kind} label={SECRET_LABEL[kind]} current={shown ?? ""} canClear={has} onCancel={() => setEditing(null)} onSave={(v) => save(kind, v)} />;
            }
            return (
              <div className="kv-row" key={kind}>
                <span className="kv-k">{SECRET_LABEL[kind]}</span>
                <span className="kv-v">
                  {/* access 是有期限的，期限就摆在值旁边：过期了要换的就是这一行。 */}
                  {kind === "access" && has && accessExpiry != null ? (
                    <span className={accessExpiry > Date.now() + 60_000 ? "faint tiny" : "tiny"} style={accessExpiry > Date.now() + 60_000 ? undefined : { color: "var(--bad)" }}>
                      {accessExpiry > Date.now() + 60_000 ? `${timeUntil(accessExpiry)}到期` : "已过期"}
                    </span>
                  ) : null}
                  {!has ? (
                    <span className="faint">未保存</span>
                  ) : shown != null ? (
                    <>
                      <code className="secret selectable truncate" title={shown}>
                        {shown}
                      </code>
                      <CopyButton value={shown} icon />
                      <button type="button" className="btn btn-sm btn-icon btn-quiet" onClick={() => hide(kind)} title="隐藏" aria-label="隐藏">
                        <Icon name="eyeOff" size={13} />
                      </button>
                    </>
                  ) : (
                    <>
                      <span className="secret is-masked">••••••••••••</span>
                      <button type="button" className="btn btn-sm btn-icon btn-quiet" onClick={() => void reveal(kind)} title="显示" aria-label="显示">
                        <Icon name="eye" size={13} />
                      </button>
                    </>
                  )}
                  {/* 线下改过密码、换了一份 token，都从这里改 —— 不用删了账号重加。 */}
                  <button type="button" className="btn btn-sm btn-icon btn-quiet" onClick={() => setEditing(kind)} title={has ? "修改" : "填入"} aria-label={has ? "修改" : "填入"}>
                    <Icon name="pencil" size={13} />
                  </button>
                </span>
              </div>
            );
          })}
          {/* 派生物：cursor.com 的 WorkosCursorSessionToken cookie 值（user_xxx::access）。有 refresh 时过期自动换；
              仅会话的号只在 access 还活着时能拼出来。两种都拼不出的不摆。 */}
          {canUseDashboard(account) ? (
            <div className="kv-row">
              <span className="kv-k" title="user_xxx::<access jwt>，即 WorkosCursorSessionToken">
                会话 cookie
              </span>
              <span className="kv-v">
                {session != null ? (
                  <>
                    <code className="secret selectable truncate" title={session}>
                      {session}
                    </code>
                    <CopyButton value={session} icon />
                    <button type="button" className="btn btn-sm btn-icon btn-quiet" onClick={() => setSession(null)} title="隐藏" aria-label="隐藏">
                      <Icon name="eyeOff" size={13} />
                    </button>
                  </>
                ) : (
                  <>
                    <span className="secret is-masked">••••••••••••</span>
                    <button
                      type="button"
                      className="btn btn-sm btn-icon btn-quiet"
                      disabled={sessionBusy}
                      onClick={() => void revealSession()}
                      title={account.hasRefresh ? "显示（手上那把过期就换一把新的）" : "显示"}
                      aria-label="显示"
                    >
                      {sessionBusy ? <Spinner /> : <Icon name="eye" size={13} />}
                    </button>
                  </>
                )}
              </span>
            </div>
          ) : null}
        </div>
      </section>

      {account.hasApiKey ? <CrsrCredRow account={account} /> : null}


      {/* 会话管理跟凭证是一回事：都是这个号的登录态。放在这里，页脚就只剩「删除」一件事。 */}
      {canUseDashboard(account) && account.status !== "dead" ? (
        <section className="sect">
          <div className="sect-cap">
            <span>其它设备上的登录</span>
            <span className="sect-aside">IDE / 网页 / 移动端</span>
          </div>
          <p className="sect-none">怀疑号被别人用着、或想让旧设备下线，可以把 Cursor 侧其它会话全部踢掉；Nexus 自己这把会尽量保留。</p>
          <KickSessions accountId={account.id} onDone={onChanged} />
        </section>
      ) : null}
    </div>
  );
}

function CrsrCredRow({ account }: { account: Account }) {
  const [st, setSt] = useState<CrsrStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  useEffect(() => {
    void crsr
      .status()
      .then(setSt)
      .catch(() => setSt(null));
  }, [account.id]);

  const cred = st?.credential ?? null;
  const mine = !!cred && cred.accountId === account.id;
  const ownerMatch = cred?.accountEmail?.toLowerCase() === account.email.toLowerCase();
  const usingThis = mine || (ownerMatch && !cred?.accountId);

  async function use() {
    setBusy(true);
    setError(null);
    try {
      await crsr.mintForAccount(account.id);
      setSt(await crsr.status());
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="sect">
      <div className="sect-cap">
        <span>CRSR 通道</span>
        <span className="sect-aside">原生 Agent 面板走这个号的 ide/cli 额度</span>
      </div>
      <ErrorNote error={error} />
      <div className="usedin">
        <div className={`usedin-row${usingThis ? " is-in is-live" : ""}`}>
          <span className="usedin-ico">
            <Icon name="crsr" size={13} />
          </span>
          <span className="usedin-k">当前凭证</span>
          <span className="usedin-v">
            {usingThis
              ? cred!.expired
                ? "这个号 · 下一发自动续"
                : `这个号 · ${timeUntil(cred!.expiresAtMs)}续`
              : cred
                ? cred.accountEmail ?? "另一个号"
                : st && !st.complete
                  ? "补丁还没装，仍可先指定"
                  : "未设置"}
          </span>
          <button
            type="button"
            className="btn btn-sm"
            disabled={busy}
            title={st && !st.complete ? "先到「CRSR 通道」安装补丁；指定这个号不必等装完" : undefined}
            onClick={() => void use()}
          >
            {busy ? <Spinner /> : null}
            {usingThis ? "再兑一次" : "用作 CRSR 通道"}
          </button>
        </div>
      </div>
    </section>
  );
}
