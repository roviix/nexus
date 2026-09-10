/**
 * 一个账号的详情：右侧抽屉。
 *
 * 抽屉而不是居中弹窗：列表还在左边，关掉之后眼睛仍落在刚才那张卡上；纵向空间也
 * 更宽裕，账单里的模型明细不用再挤。也**不在卡内展开** —— 详情的形状和一张卡差太远。
 *
 * 标题区把这个号的「身份」摆齐：邮箱、档位、健康度、来源、备注（可就地改）。
 * 动作也在标题区 —— 刷新 / 授权 / 切号 是来这一页最常按的三个键，不该藏在页脚。
 * 「切号」只是把人送到切号页：池内账号进入确认，池外账号先确认加入切号池。
 *
 * 标题区下面先答「它在哪儿被用着」：切号池里有没有、网关号池里有没有，各一行，能就地加入 /
 * 移出。账号总库是一份、两个使用池是子集（ARCHITECTURE §5.2），以前要走到那两页才知道一个号进了
 * 没进，现在在这个号自己的抽屉里就说清。
 *
 * 然后分三页：用量 / 账单 / 凭证。这是三种不同的来意（「还能不能用」「花了多少」
 * 「我要复制密码」），一次只有一种；摊在一屏里既长又逼着人略读。每页单列满宽。
 * 「踢掉其它会话」住在凭证页 —— 它管的是这个号的登录态，跟凭证是一回事。
 *
 * 页脚只剩两样：什么时候加的、删除。删除要按两次 —— 它会连凭证一起清掉，没有回头路。
 */
import { useEffect, useState, type ReactNode } from "react";
import type { AccountPlacement } from "../../accounts/model";
import { GATEWAY_MEMBERSHIP_LABEL, inGatewayRoster, usePools } from "../../accounts/pools";
import type { Account, KickOutcome, ModelUsage, SecretKind } from "../../ipc/types";
import { accounts, gateway as gatewayApi, switcher as switcherApi } from "../../ipc/api";
import { Banner, CopyButton, Drawer, ErrorNote, Gauge, Health, Icon, Reset, Spinner, Tag } from "../../ui/primitives";
import { accountSourceLabel, timeAgo, timeUntil } from "../../ui/format";
import { canQueryUsage, hasLiveAccess, sessionOnly } from "../../ui/accounts";
import { canAddToSwitchPool } from "../../ui/switcher";
import { compactNumber } from "../../ui/traffic";
import { GrokBotTab } from "./GrokBotTab";
import {
  accountProblem,
  blockReasonText,
  cycleProgress,
  daysText,
  isFresh,
  meterColor,
  meterWidth,
  money,
  moneyShort,
  onDemandText,
  pctText,
  planLabel,
  planTone,
  shortDate,
  spendPace,
  type SpendPace,
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
  /** 去「切号」页。池外账号会先进入显式加入流程。 */
  onSwitch: () => void;
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
  const u = account.usage;
  const problem = accountProblem(account, u);
  const dead = account.status === "dead";

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
        {inCursor ? (
          <button type="button" className="btn btn-sm dr-cta" disabled title="Cursor 现在登着的就是它">
            <Icon name="switcher" size={13} />
            Cursor 正在用
          </button>
        ) : (
          <button
            type="button"
            className="btn btn-sm btn-primary dr-cta"
            disabled={!account.hasRefresh || dead}
            onClick={onSwitch}
            title={
              !account.hasRefresh
                ? sessionOnly(account)
                  ? "只有 session token 切不进 Cursor：写进去的登录态到期没法自己续。授权一次拿到 refresh_token 即可"
                  : "需要先授权拿到 refresh_token"
                : dead
                  ? "这个号已失效"
                  : "前往切号池"
            }
          >
            <Icon name="switcher" size={13} />
            切号
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

        {tab === "usage" ? <UsageTab account={account} refreshing={refreshing} onRefresh={onRefresh} /> : null}
        {tab === "bill" ? <BillTab account={account} /> : null}
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
        title={canAddToSwitchPool(account) ? "把它的登录态拷进切号池，之后可以一键切进 Cursor" : dead ? "这个号已失效" : "需要先授权拿到 refresh_token"}
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
function NoteLine({ note, onSave }: { note: string; onSave: (note: string) => Promise<void> }) {
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
function DeleteAccount({ onConfirm }: { onConfirm: () => Promise<void> }) {
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
      <span className="confirm-text">{error ? "删除失败，再试一次？" : "会连同 refresh_token 和密码一起从本机清除，不可恢复。"}</span>
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

function UsageTab({ account, refreshing, onRefresh }: { account: Account; refreshing: boolean; onRefresh: () => void }) {
  const u = account.usage;
  if (!u) {
    return (
      <div className="dr-blank">
        <p>还没查过这个号的用量。</p>
        {canQueryUsage(account) ? (
          <button type="button" className="btn btn-sm" disabled={refreshing} onClick={onRefresh}>
            {refreshing ? <Spinner /> : "拉一次"}
          </button>
        ) : (
          <p className="faint tiny">{sessionOnly(account) ? "会话已过期，到凭证页粘一份新的 session token。" : "先授权拿到 refresh_token 才能查。"}</p>
        )}
      </div>
    );
  }

  const now = Date.now();
  const bot = u.bot;
  const cycle = cycleProgress(u, now);

  return (
    <div className="stack" style={{ gap: 20 }}>
      <section className="sect">
        <div className="sect-cap">
          <span>Bot 通道</span>
          {bot?.planLabel ? <span className="sect-aside">{bot.planLabel}</span> : null}
        </div>
        {/* 「什么时候能再用」和「用了多少」一样重要，所以重置时刻是一块正经的信息条，
            不是缀在角落的一行灰字。没有这个通道时就不摆 —— 一个「—」什么也没说。 */}
        {bot?.resetAt ? (
          <Reset
            label="周额重置"
            at={bot.resetAt}
            now={now}
            tag={bot.hasAvailable === false ? <Tag tone="bad">已耗尽</Tag> : isFresh(bot.periodStart, now) ? <Tag tone="ok">刚重置</Tag> : null}
          />
        ) : null}
        {!bot ? (
          <p className="sect-none">这个号没有 Bot 通道（老档 pro-legacy 没有这套，属正常）。</p>
        ) : bot.access === "blocked" ? (
          <Banner tone="bad" title="无权限" hint={bot.blockReason ? blockReasonText(bot.blockReason) : undefined} />
        ) : (
          <Gauge label="周用量" percent={bot.percentUsed} />
        )}
      </section>

      <section className="sect">
        <div className="sect-cap">
          <span>月账期</span>
          <span className="sect-aside mono">
            {shortDate(u.cycleStart)} → {shortDate(u.cycleEnd)}
          </span>
        </div>
        <Reset
          label="月额重置"
          at={u.cycleEnd}
          now={now}
          tag={isFresh(u.cycleStart, now) ? <Tag tone="ok">刚重置</Tag> : null}
          // 账期走过多少：画在重置条里，它说的是「周期」不是「额度」，
          // 混在下面三条额度条里会被当成第四个指标。
          progress={cycle}
        />
        <Gauge label="总额度" percent={u.totalPercentUsed} title="月账期包含额度的整体已用比例" />
        <Gauge label="Auto" percent={u.autoPercentUsed} title="composer / grok 等由 Cursor 调度的模型" />
        <Gauge label="API" percent={u.apiPercentUsed} title="点名调用的 claude / gpt 等；打满后这类模型调不动" />
      </section>
    </div>
  );
}

/* ── 账单 ─────────────────────────────────────────────────────────────────── */

/** 账单看哪一段：本账期是结算口径；近 7 天 / 今天来自刷用量时多问的两个时间窗。 */
type BillRange = "cycle" | "week" | "today";

const RANGE_LABEL: Record<BillRange, string> = { cycle: "本账期", week: "近 7 天", today: "今天" };
const RANGE_HEAD: Record<BillRange, string> = { cycle: "本期消费", week: "近 7 天消费", today: "今天消费" };

/** 一段范围里要画的东西：花费、按模型、tokens。三个范围长同一个形状，下面的组件才能不分叉。 */
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

/**
 * 账单：一个范围开关管整页 —— 本账期 / 近 7 天 / 今天，大数、分色条、模型排行、tokens 全跟着换。
 *
 * 钱是这一页唯一要紧的数，就该大；Auto 和 API 是分桶计量的（见用量页），花费也按这两桶分色，
 * 人一眼看出钱主要烧在哪条路上。本账期多一条**节奏**：花费进度对着时间进度画，再给日均、
 * 预计账期末、剩余额度三格 —— 「够不够撑到重置」这个问题，单看一个 42% 答不了。
 * 排行按花费从多到少，条子按第一名算比例 —— 「主力是哪个模型」比每一格的精确数字先被问到。
 */
function BillTab({ account }: { account: Account }) {
  const u = account.usage;
  const [range, setRange] = useState<BillRange>("cycle");
  if (!u) return <div className="dr-blank">还没查过用量，没有账单可看。</div>;

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

  // 大数底下那一句：本账期说额度，时间窗说「占本期多少、和日均比怎样」。
  let sub: ReactNode = null;
  if (shown === "cycle") {
    sub = pace?.budget != null ? (
      <>
        额度 <b className="num">{money(pace.budget)}</b>
        <span className="bill-sub-sep">·</span>
        {pace.remaining! >= 0 ? (
          <>
            还剩 <b className="num">{money(pace.remaining)}</b>
          </>
        ) : (
          <span className="is-bad">
            超支 <b className="num">{money(-pace.remaining!)}</b>
          </span>
        )}
        {u.bonusCents ? (
          <>
            <span className="bill-sub-sep">·</span>含赠送 <b className="num">{money(u.bonusCents)}</b>
          </>
        ) : null}
      </>
    ) : (
      <>
        账期 <b className="num mono">{shortDate(u.cycleStart)} → {shortDate(u.cycleEnd)}</b>
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
    <div className="stack" style={{ gap: 18 }}>
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
                <button key={r} type="button" role="tab" className="range-opt" aria-selected={shown === r} onClick={() => setRange(r)}>
                  {RANGE_LABEL[r]}
                </button>
              ))}
            </div>
          ) : null}
        </div>

        {shown === "cycle" && pace?.budget != null ? <PaceBar pace={pace} /> : null}

        {tierTotal > 0 ? (
          <div className="bill-split">
            <span className="bill-split-bar">
              {byTier.auto > 0 ? <i className="is-auto" style={{ width: `${(byTier.auto / tierTotal) * 100}%` }} title={`Auto ${money(byTier.auto)}`} /> : null}
              {byTier.api > 0 ? <i className="is-api" style={{ width: `${(byTier.api / tierTotal) * 100}%` }} title={`API ${money(byTier.api)}`} /> : null}
              {byTier.other > 0 ? <i className="is-other" style={{ width: `${(byTier.other / tierTotal) * 100}%` }} title={`其他 ${money(byTier.other)}`} /> : null}
            </span>
            <span className="bill-legend">
              {byTier.auto > 0 ? (
                <span>
                  <i className="is-auto" />
                  Auto <b className="num">{money(byTier.auto)}</b>
                </span>
              ) : null}
              {byTier.api > 0 ? (
                <span>
                  <i className="is-api" />
                  API <b className="num">{money(byTier.api)}</b>
                </span>
              ) : null}
              {byTier.other > 0 ? (
                <span>
                  <i className="is-other" />
                  其他 <b className="num">{money(byTier.other)}</b>
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
            <span>节奏</span>
            <span className="sect-aside num">
              账期第 {Math.ceil(pace.elapsedDays)} / {Math.round(pace.totalDays)} 天
            </span>
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
          <span>按模型</span>
          <span className="sect-aside">{view.models.length ? `${view.models.length} 个模型 · 按花费` : null}</span>
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
            <span>Tokens</span>
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
    </div>
  );
}

/**
 * 花费进度对着时间进度画：一条按额度填色的条，上面一道细刻度标着「账期走到哪了」。
 * 填色越过刻度 = 花得比时间快。一句话把结论说出来，人不用自己比两个百分比。
 */
function PaceBar({ pace }: { pace: SpendPace }) {
  const spentPct = pace.budget! > 0 ? Math.min(100, ((pace.budget! - pace.remaining!) / pace.budget!) * 100) : 0;
  const timePct = Math.min(100, (pace.elapsedDays / pace.totalDays) * 100);
  const ahead = pace.aheadPct ?? 0;
  const verdict = pace.remaining! <= 0 ? { tone: "is-bad", text: "额度已用完" } : ahead > 8 ? { tone: "is-warn", text: `比时间快 ${Math.round(ahead)} 个点` } : ahead < -8 ? { tone: "is-ok", text: `比时间慢 ${Math.round(-ahead)} 个点` } : { tone: "", text: "节奏正常" };
  return (
    <div className="pace">
      <span className="pace-track" title={`已用 ${Math.round(spentPct)}% · 账期过了 ${Math.round(timePct)}%`}>
        <i className="pace-fill" style={{ width: `${meterWidth(spentPct)}%`, background: meterColor(spentPct) }} />
        <i className="pace-tick" style={{ left: `${timePct}%` }} />
      </span>
      <span className="pace-legend num">
        <span>
          已用 <b>{pctText(spentPct)}</b>
        </span>
        <span className="pace-time">
          <i />
          账期过了 <b>{Math.round(timePct)}%</b>
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
          {canQueryUsage(account) ? (
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

      {/* 会话管理跟凭证是一回事：都是这个号的登录态。放在这里，页脚就只剩「删除」一件事。 */}
      {canQueryUsage(account) && account.status !== "dead" ? (
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
