/**
 * 一个账号的详情：右侧抽屉。
 *
 * 抽屉而不是居中弹窗：列表还在左边，关掉之后眼睛仍落在刚才那张卡上；纵向空间也
 * 更宽裕，账单里的模型明细不用再挤。也**不在卡内展开** —— 详情的形状和一张卡差太远。
 *
 * 标题区把这个号的「身份」摆齐：邮箱、档位、健康度、来源、备注（可就地改）。
 * 动作也在标题区 —— 刷新 / 授权 / 切号 是来这一页最常按的三个键，不该藏在页脚。
 * 「切号」就地热切：Cursor 在跑时不退出；只有开了「切换时同时切机器码」才会问一句。
 * 切号会把号带进切号池（切号器只认「档」），所以号还不在池里时按钮写「加入切号池并切号」——
 * 一键做完，但不藏着；进了池随时可在下面「所在池」那一行移出。
 * 仅会话（token 导入、没有 refresh）的号切不了，凭证页给它「铸一把 crsr_ Key」当出路。
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
import { bareAccessJwt, flycursorHint } from "../../accounts/tokenPaste";
import { GATEWAY_MEMBERSHIP_LABEL, inGatewayRoster, usePools, type Pools } from "../../accounts/pools";
import type { Account, CrsrStatus, KickOutcome, MintedApiKey, SecretKind } from "../../ipc/types";
import { accounts, crsr, gateway as gatewayApi, switcher as switcherApi } from "../../ipc/api";
import { confirm } from "../../ui/confirm";
import { Banner, CopyButton, Drawer, ErrorNote, Health, Icon, Spinner, Switch } from "../../ui/primitives";
import { accountSourceLabel, timeAgo, timeUntil } from "../../ui/format";
import { canQueryUsage, canUseDashboard, hasLiveAccess, sessionOnly } from "../../ui/accounts";
import { canAddToSwitchPool, canMintApiKey, switchNeedsWebConversion } from "../../ui/switcher";
import { GrokBotTab } from "./GrokBotTab";
import { BillTab } from "./BillTab";
import {
  accountProblem,
  bonusSpend,
  money,
  creditPoints,
  onDemandText,
  planBudget,
  planLabel,
  planSpend,
  planTone,
  resetInShort,
  shortDate,
  shortDateTime,
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
  // 两个池的名单在抽屉这一层拉一次：标题区的主键要按「在不在切号池」换文案，
  // 下面「所在池」那一段也用同一份，切完号刷一次两边一起变。
  const pools = usePools();
  const inSwitchPool = pools.membership(account.email).switcher != null;

  async function switchNow() {
    setSwitching(true);
    try {
      await onSwitch();
      // 切号会顺带入池；不刷的话「所在池」那一行还写着「未加入」，主键也还写着「加入并切号」。
      await pools.reload();
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
                    ? "这个号的 session token 已过期，写进 Cursor 只会显示掉登录。到凭证页粘一份新的，或授权一次拿到 refresh_token。"
                    : "切号要一把活着的会话：粘一份 session token，或授权一次拿到 refresh_token"
                : (inSwitchPool ? "" : "先把它加进切号池（之后随时可在下方「所在池」移出），再") +
                  (switchNeedsWebConversion(account)
                    ? "切入 Cursor。这个号是网站 web token：首次切号会先自动换成桌面登录（几秒，无需密码 / 验证码），之后就是长期号"
                    : sessionOnly(account)
                      ? "切入 Cursor（Cursor 在跑时不重启）。这个号只有 session token：Cursor 会拿它自己续期，到期前都能用"
                      : "切入 Cursor（Cursor 在跑时不重启）")
            }
          >
            {switching ? <Spinner /> : <Icon name="switcher" size={13} />}
            {switching ? "切换中" : inSwitchPool || pools.loading ? "切号" : "加入切号池并切号"}
          </button>
        )}
      </div>
    </div>
  );

  const isArchived = Boolean(account.archivedAt);
  const [archiving, setArchiving] = useState(false);

  const foot = (
    <>
      <span className="dr-foot-meta">
        {timeAgo(account.createdAt)}添加
        <span className="dr-foot-sep">·</span>
        {accountSourceLabel(account.source)}
        {account.archivedAt ? (
          <>
            <span className="dr-foot-sep">·</span>
            <span className="pill pill-session">已归档</span>
          </>
        ) : null}
      </span>
      <div className="row" style={{ gap: 8, alignItems: "center" }}>
        <button
          type="button"
          className="btn btn-sm btn-quiet"
          disabled={archiving}
          onClick={async () => {
            setArchiving(true);
            try {
              await accounts.setArchived([account.id], !isArchived);
              await onReload();
            } finally {
              setArchiving(false);
            }
          }}
          title={isArchived ? "取回此账号" : "归档此账号"}
        >
          <Icon name={isArchived ? "undo" : "archive"} size={13} />
          {isArchived ? "取回" : "归档"}
        </button>
        <DeleteAccount
          onConfirm={async () => {
            await onRemove();
            onClose();
          }}
        />
      </div>
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

        <UsedIn account={account} pools={pools} placement={placement} onChanged={onReload} />

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
function UsedIn({
  account,
  pools,
  placement,
  onChanged,
}: {
  account: Account;
  /** 抽屉那一层拉好的名单：标题区主键和这里共用一份，谁改了另一边立刻跟上。 */
  pools: Pools;
  placement?: AccountPlacement;
  onChanged: () => Promise<void>;
}) {
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
        onClick={() =>
          void (async () => {
            const ok = await confirm(`会删掉为 ${account.email} 保存的登录态快照；账号本身不受影响，随时能再加。`, {
              title: "移出切号池",
              okLabel: "移出",
              danger: true,
            });
            if (!ok) return;
            await act("switcher", () => switcherApi.remove(m.switcher!.id));
          })()
        }
      >
        移出
      </button>
    ) : (
      <button
        type="button"
        className="btn btn-sm btn-soft"
        disabled={busy != null || !canAddToSwitchPool(account)}
        title={canAddToSwitchPool(account) ? "把它的登录态拷进切号池，之后可以一键切进 Cursor" : dead ? "这个号已失效" : sessionOnly(account) ? "这个号的 session token 已过期；粘一份新的，或授权一次拿到 refresh_token" : "切号要一把活着的会话：粘一份 session token，或授权一次拿到 refresh_token"}
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
        onClick={() =>
          void (async () => {
            if (m.gateway === "current") {
              const ok = await confirm(`${account.email} 正在被网关使用。移出后下一个请求会换号，正在进行的对话会丢上游缓存。`, {
                title: "移出网关号池",
                okLabel: "移出",
                danger: true,
              });
              if (!ok) return;
            }
            await act("gateway", () => gatewayApi.unenroll(account.email));
          })()
        }
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
  warning = "会连同 refresh_token 和密码一起从本机清除，不可恢复；在切号池里的话也一并移出。",
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
  const budget = planBudget(u);
  // 三本账分开说：订阅额度（included 对 limit）、免费加量（bonus）、按需（另一节）。
  // 以前把 included + bonus 的总数对着 limit 摆「已用 / 剩余」，18/70 个号被显示成
  // 「剩余 $0 · 超出的走按需」，可它们按需是 $0 —— 超出的是白送的。
  const spend = planSpend(u);
  const bonus = bonusSpend(u);
  const grantRemaining = u.creditGrantRemainingCents;
  const grantTotal = u.creditGrantTotalCents;
  const grantUsed = u.creditGrantUsedCents;
  const hasGrant = grantRemaining != null || grantTotal != null;
  const grants = u.creditGrants ?? [];

  const pctTone =
    u.totalPercentUsed == null
      ? "idle"
      : u.totalPercentUsed > 90
        ? "bad"
        : u.totalPercentUsed >= 70
          ? "warn"
          : "ok";
  const remainingCents =
    budget != null && spend != null ? Math.max(0, budget - spend) : null;
  const cycleCountdown = u.cycleEnd ? resetInShort(u.cycleEnd, now) : null;
  const earliestGrantExpiry = grants.length
    ? Math.min(
        ...grants.map((g) => g.expiresAt ?? Number.POSITIVE_INFINITY),
      )
    : null;
  const grantExpiryText =
    earliestGrantExpiry != null && Number.isFinite(earliestGrantExpiry)
      ? `${shortDate(earliestGrantExpiry)} 到期`
      : "永久有效";

  if (u.via === "apiKey") {
    const models = [...(u.byModel ?? [])].sort((a, b) => b.cents - a.cents);
    return (
      <div className="stack" style={{ gap: 14 }}>
        <Banner tone="warn" title="基础用量" hint="session 不可用，这次是 crsr_ API Key 兑出来的逐条花费。没有额度百分比、账期和 Grok 周额。完整账单在「账单」页。" />
        <div className="drawer-card">
          <div className="dc-head">
            <span className="dc-title">扣费明细</span>
            {u.plan ? <span className="dc-sub">{u.plan}</span> : null}
          </div>
          <div className="grant-summary-row">
            <div className="gs-item">
              <span className="gs-k">合计扣费</span>
              <span className="gs-v num">{money(u.spendCents)}</span>
            </div>
            <div className="gs-item">
              <span className="gs-k">今天消费</span>
              <span className="gs-v num">{u.today ? money(u.today.cents) : "—"}</span>
            </div>
            <div className="gs-item">
              <span className="gs-k">近 7 天</span>
              <span className="gs-v num">{u.week ? money(u.week.cents) : "—"}</span>
            </div>
          </div>
        </div>
        {models.length > 0 ? (
          <div className="drawer-card">
            <div className="dc-head">
              <span className="dc-title">按模型消费</span>
              <span className="dc-sub">{models.length} 个模型</span>
            </div>
            <div className="grant-items-list">
              {models.map((m) => (
                <div key={m.model} className="grant-item-row">
                  <span className="grant-name truncate mono">{m.model}</span>
                  <span className="grant-amt num font-mono">{money(m.cents)}</span>
                </div>
              ))}
            </div>
          </div>
        ) : null}
      </div>
    );
  }

  return (
    <div className="stack" style={{ gap: 14 }}>
      {/* 1. Hero 订阅配额主卡片：核心指标大字突出，单条流线进度条 */}
      <div className="drawer-quota-hero">
        <div className="dqh-head">
          <div className="dqh-title-row">
            <span className="dqh-title">月度订阅配额</span>
            {u.plan ? (
              <span className={`plan ${planTone(u.plan)}`}>
                {planLabel(u.plan)}
              </span>
            ) : null}
          </div>
          <span className="dqh-cycle mono">
            {shortDateTime(u.cycleStart)} → {shortDateTime(u.cycleEnd)}
          </span>
        </div>

        <div className="dqh-metric-block">
          <div className="dqh-metric-primary">
            <span className="dqh-num num">
              {remainingCents != null ? money(remainingCents) : money(spend)}
            </span>
            <span className="dqh-lbl">
              {remainingCents != null ? "剩余可用额度" : "已用额度"}
            </span>
          </div>

          <div className="dqh-metric-meta">
            <span>
              已用 <b className="num">{money(spend)}</b>
            </span>
            {budget != null ? (
              <>
                <span className="dqh-sep">/</span>
                <span>
                  总配额 <b className="num">{money(budget)}</b>
                </span>
                <span className="dqh-sep">·</span>
                <span>
                  <b className="num">{Math.round(u.totalPercentUsed ?? 0)}%</b> 已用
                </span>
              </>
            ) : null}
            {cycleCountdown ? (
              <span className="dqh-reset-tag">
                <Icon name="clock" size={11} />
                <span>{cycleCountdown}</span>
              </span>
            ) : null}
          </div>
        </div>

        {/* 6px 微进度条 */}
        <div className="dqh-bar-track" role="progressbar" aria-valuenow={u.totalPercentUsed ?? 0}>
          <div
            className={`dqh-bar-fill is-${pctTone}`}
            style={{ width: `${Math.min(100, Math.max(0, u.totalPercentUsed ?? 0))}%` }}
          />
        </div>

        {/* Auto 额度 */}
        {bonus != null ? (
          <div className="dqh-bonus-strip">
            <span style={{ fontSize: 13 }}>⚡</span>
            <div style={{ display: "flex", flexDirection: "column", gap: 1 }}>
              <span>
                Auto 额外用量 <b className="num">{money(bonus)}</b>
              </span>
              <span className="faint tiny">
                由 Auto 智能调度产生的消费，不占基础额度，亦非按需扣费
              </span>
            </div>
          </div>
        ) : null}
      </div>

      {/* 2. 额度使用情况（各桶百分比与状态） */}
      <div className="drawer-card">
        <div className="dc-head">
          <span className="dc-title">额度使用情况</span>
          <span className="dc-sub faint">各通道额度与包含配额</span>
        </div>

        <div className="quota-buckets-grid">
          {/* 总额度 */}
          <div className="qb-card">
            <div className="qb-header">
              <span className="qb-label">总包含额度</span>
              <span className="qb-pct num">
                {u.totalPercentUsed != null ? `${Math.round(u.totalPercentUsed)}%` : "—"}
              </span>
            </div>
            <div className="qb-track">
              <div
                className={`qb-fill is-${pctTone}`}
                style={{ width: `${Math.min(100, Math.max(0, u.totalPercentUsed ?? 0))}%` }}
              />
            </div>
            <div className="qb-meta">
              <span>{budget != null && spend != null ? `${money(spend)} / ${money(budget)}` : "月账期包含额度"}</span>
            </div>
          </div>

          {/* Auto 额度 */}
          <div className="qb-card">
            <div className="qb-header">
              <span className="qb-label">Auto 额度</span>
              <span className="qb-pct num">
                {u.autoPercentUsed != null ? `${Math.round(u.autoPercentUsed)}%` : "—"}
              </span>
            </div>
            <div className="qb-track">
              <div
                className={`qb-fill is-${
                  u.autoPercentUsed == null
                    ? "idle"
                    : u.autoPercentUsed > 90
                      ? "bad"
                      : u.autoPercentUsed >= 70
                        ? "warn"
                        : "info"
                }`}
                style={{ width: `${Math.min(100, Math.max(0, u.autoPercentUsed ?? 0))}%` }}
              />
            </div>
            <div className="qb-meta">
              <span>Composer / Grok 等模型</span>
            </div>
          </div>

          {/* API 额度 */}
          <div className="qb-card">
            <div className="qb-header">
              <span className="qb-label">点名 API 额度</span>
              <span className="qb-pct num">
                {u.apiPercentUsed != null ? `${Math.round(u.apiPercentUsed)}%` : "—"}
              </span>
            </div>
            <div className="qb-track">
              <div
                className={`qb-fill is-${
                  u.apiPercentUsed == null
                    ? "idle"
                    : u.apiPercentUsed > 90
                      ? "bad"
                      : u.apiPercentUsed >= 70
                        ? "warn"
                        : "ok"
                }`}
                style={{ width: `${Math.min(100, Math.max(0, u.apiPercentUsed ?? 0))}%` }}
              />
            </div>
            <div className="qb-meta">
              <span>Claude / GPT 等模型</span>
            </div>
          </div>

          {/* Bot 周额 */}
          <div className="qb-card">
            <div className="qb-header">
              <span className="qb-label">Grok Bot 周额</span>
              <span className="qb-pct num">
                {!bot ? (
                  "—"
                ) : bot.access === "blocked" ? (
                  "无权限"
                ) : bot.hasAvailable === false ? (
                  "已耗尽"
                ) : (
                  `${Math.round(bot.percentUsed ?? 0)}%`
                )}
              </span>
            </div>
            <div className="qb-track">
              <div
                className={`qb-fill is-${
                  !bot || bot.hasAvailable === false || bot.access === "blocked"
                    ? "bad"
                    : (bot.percentUsed ?? 0) > 90
                      ? "bad"
                      : (bot.percentUsed ?? 0) >= 70
                        ? "warn"
                        : "ok"
                }`}
                style={{
                  width: `${
                    !bot
                      ? 0
                      : bot.hasAvailable === false
                        ? 100
                        : Math.min(100, Math.max(0, bot.percentUsed ?? 0))
                  }%`,
                }}
              />
            </div>
            <div className="qb-meta">
              <span>{bot?.resetAt ? `周额 ${resetInShort(bot.resetAt, now)}` : "按周重置"}</span>
            </div>
          </div>
        </div>
      </div>

      {/* 3. 赠送积分 (Credit Grants) - 仅在真有积分时展示，绝不留空卡占位 */}
      {hasGrant ? (
        <div className="drawer-card is-grant">
          <div className="dc-head">
            <div className="row" style={{ gap: 6, alignItems: "center" }}>
              <span style={{ fontSize: 13 }}>🎟</span>
              <span className="dc-title">赠送积分 (Credit Grant)</span>
            </div>
            {grantTotal != null ? (
              <span className="dc-sub num mono">
                共 {creditPoints(grantTotal)} 积分 · 1 积分 = $1
              </span>
            ) : null}
          </div>

          <div className="grant-summary-row">
            <div className="gs-item">
              <span className="gs-k">剩余可用</span>
              <span className="gs-v num">{creditPoints(grantRemaining)}</span>
            </div>
            <div className="gs-item">
              <span className="gs-k">已用积分</span>
              <span className="gs-v num">{creditPoints(grantUsed)}</span>
            </div>
            <div className="gs-item">
              <span className="gs-k">有效期</span>
              <span className="gs-v num" style={{ fontSize: 12 }}>{grantExpiryText}</span>
            </div>
          </div>

          {grants.length ? (
            <div className="grant-items-list">
              {grants.map((g, idx) => (
                <div key={idx} className="grant-item-row">
                  <span className="grant-name truncate">
                    {g.displayName || "Power user grant"}
                  </span>
                  <span className="grant-amt num font-mono">
                    {creditPoints(g.remainingCents)} / {creditPoints(g.totalCents)}
                    {g.expiresAt ? (
                      <span className="faint tiny"> · {timeUntil(g.expiresAt)}到期</span>
                    ) : null}
                  </span>
                </div>
              ))}
            </div>
          ) : null}
        </div>
      ) : null}

      {/* 4. 按需计费设置 */}
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
      if (!(await confirm(`${cap}。`, { title: "开启按需计费？", okLabel: "开启" }))) {
        setOn(enabled);
        return;
      }
    }
    if (!nextOn && enabled && !(await confirm("关闭后，包含额度用完即停。", { title: "关闭按需计费？", okLabel: "关闭" }))) {
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
    <div className="drawer-card">
      <div className="dc-head">
        <span className="dc-title">按需超额扣费</span>
        <span className="dc-sub">{enabled ? `已用 ${money(used)} · ${od.sub}` : "未开启 · 配额用完即停"}</span>
      </div>

      <div className="od-bar">
        <Switch
          checked={on}
          disabled={!can || busy}
          label="开启按需扣费"
          onChange={setOn}
        />
        <input
          className="input od-limit"
          inputMode="decimal"
          placeholder="不设上限"
          disabled={!can || busy || !on}
          value={limitText}
          onChange={(e) => setLimitText(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && dirty && parsed !== "invalid") {
              void save(on, parsed);
            }
          }}
          aria-label="按需月度预算上限（美元）"
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
      {parsed === "invalid" ? <p className="sect-none" style={{ color: "var(--bad)" }}>上限需为有效金额数字；留空表示不设上限。</p> : null}
      {!can ? <p className="sect-none">需要有效会话凭证才能修改按需设置。</p> : null}
      <ErrorNote error={error} />
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
  access: "access",
  web: "Web 网页 token",
  cursorPassword: "Cursor 密码",
  emailPassword: "邮箱密码",
  recoveryEmail: "辅助邮箱",
  apiKey: "crsr_ API Key",
};

/** 凭证行名：session 仍用 claim 原词；web 写成「Web 网页 token」，免得和桌面 session 混。 */
function accessKindLabel(type?: string | null): string {
  if (type === "web") return SECRET_LABEL.web;
  if (type === "session" || type === "api_key_token") return type;
  return SECRET_LABEL.access;
}

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
    ["web", account.hasWeb ?? false],
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

  const bare = session != null ? bareAccessJwt(session) : null;
  const pasteHint = flycursorHint(account.accessTokenType);

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
            const label = kind === "access" ? accessKindLabel(account.accessTokenType) : SECRET_LABEL[kind];
            if (editing === kind) {
              return <SecretEditor key={kind} label={label} current={shown ?? ""} canClear={has} onCancel={() => setEditing(null)} onSave={(v) => save(kind, v)} />;
            }
            return (
              <div className="kv-row" key={kind}>
                <span className={`kv-k${kind === "access" || kind === "web" || kind === "refresh" || kind === "apiKey" ? " is-token" : ""}`}>{label}</span>
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
              <span className="kv-k is-token" title="user_xxx::<access jwt>，网站 cookie。FlyCursor 的 access token 框不收这一整段。">
                WorkosCursorSessionToken
              </span>
              <span className="kv-v">
                <TokenTypeTag type={account.accessTokenType} />
                {session != null ? (
                  <>
                    <code className="secret selectable truncate" title={session}>
                      {session}
                    </code>
                    <CopyButton
                      value={session}
                      icon
                      label={account.accessTokenType === "session" && bare && bare !== session ? "复制 cookie" : "复制"}
                    />
                    {account.accessTokenType === "session" && bare && bare !== session ? (
                      <CopyButton value={bare} icon label="复制 access token" />
                    ) : null}
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
        {canUseDashboard(account) && pasteHint ? (
          <p className="sect-none" style={{ marginTop: 8 }}>
            {pasteHint}
          </p>
        ) : null}
      </section>

      {switchNeedsWebConversion(account) ? <ConvertSessionRow account={account} onChanged={onChanged} /> : null}

      {account.hasApiKey ? (
        <CrsrCredRow account={account} />
      ) : canMintApiKey(account) ? (
        <MintApiKeyRow account={account} onChanged={onChanged} />
      ) : null}


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

/**
 * JWT `type` claim 原样标出来：`session` / `web` / `api_key_token`。
 *
 * 不翻译、不加「桌面 / 网站」——那几个字才是这把票的身份。`web` 写进 Cursor 会掉登录，
 * 用 warn 色把它和其他两型分开。没解析出来就不猜。
 */
function TokenTypeTag({ type }: { type?: string | null }) {
  if (!type) return null;
  const known = type === "session" || type === "web" || type === "api_key_token";
  const title =
    type === "session"
      ? "type=session：桌面会话，能切进 Cursor"
      : type === "web"
        ? "type=web：网站会话，写进 Cursor 会掉登录"
        : type === "api_key_token"
          ? "type=api_key_token：crsr_ Key 兑出来的短票，不能当会话"
          : `type=${type}`;
  return (
    <span className={`token-type${type === "web" ? " is-web" : type === "api_key_token" ? " is-key" : known ? " is-session" : ""}`} title={title}>
      {type}
    </span>
  );
}

/**
 * 「换成桌面 session」。
 *
 * 拿这个号还活着的网站会话走一次官方 `loginDeepControl` 深链，换出桌面 session + refresh。
 * 无密码、无验证码、不掉原来那个会话。
 *
 * 只对**活着的 web-only 号**出现（`switchNeedsWebConversion`）。切号时本来就会自动做这件事，
 * 单独给一个按钮是因为它的价值跟切号无关：换完这个号就有了 refresh，从「几小时后就死」变成
 * 能一直续期的长期号，也才能复制出一把别人也能用的 session token。
 */
function ConvertSessionRow({ account, onChanged }: { account: Account; onChanged: () => Promise<void> }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  async function convert() {
    setBusy(true);
    setError(null);
    try {
      await accounts.convertWebToSession(account.id);
      await onChanged();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="sect">
      <div className="sect-cap">
        <span>换成桌面 session</span>
        <span className="sect-aside">不需要密码或验证码</span>
      </div>
      <ErrorNote error={error} />
      <p className="sect-none">
        这个号手上是一把网站 web token：能查用量、能进网关，但写进 Cursor 会掉登录，而且过期后没法续。
        换一次就有了 refresh_token——从此能切号、能续期、复制出去的 session token 别人也能用。切号时会自动做这件事，在这里可以先做掉。
      </p>
      <div className="row" style={{ marginTop: 10 }}>
        <button type="button" className="btn btn-sm btn-primary" disabled={busy} onClick={() => void convert()}>
          {busy ? <Spinner /> : null}
          换一把桌面 session
        </button>
      </div>
    </section>
  );
}

/**
 * 「铸一把 crsr_ Key」。
 *
 * 对仅会话的号这是**保命动作**，所以它摆在凭证页最显眼的位置而不是折在某个菜单里：
 * 那批号没有 refresh、接不了验证码，手上那把 access 一过期整个号就拿不回来了。铸 key
 * 只认 access（`DashboardService/CreateUserApiKey`），趁它还活着铸出来，额度就不再挂在
 * 一个会死的凭证上。
 *
 * 文案不许含糊两件事：铸完**不能**恢复切号能力；以及这件事有时限。
 */
function MintApiKeyRow({ account, onChanged }: { account: Account; onChanged: () => Promise<void> }) {
  const [busy, setBusy] = useState(false);
  const [minted, setMinted] = useState<MintedApiKey | null>(null);
  const [error, setError] = useState<unknown>(null);
  const urgent = sessionOnly(account);

  async function mint() {
    setBusy(true);
    setError(null);
    try {
      setMinted(await accounts.mintApiKey(account.id));
      await onChanged();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="sect">
      <div className="sect-cap">
        <span>长期 API Key</span>
        <span className="sect-aside">不需要密码或验证码</span>
      </div>
      <ErrorNote error={error} />
      {minted ? (
        <Banner
          tone="ok"
          title={`已铸出 ${minted.masked}`}
          hint={
            minted.expiresAt
              ? `${timeUntil(Date.parse(minted.expiresAt))}到期；完整钥匙在上面「crsr_ API Key」那一行点「显示」。`
              : "完整钥匙在上面「crsr_ API Key」那一行点「显示」。"
          }
        />
      ) : (
        <p className="sect-none">
          {urgent
            ? "这个号只有一把 session token：没有 refresh、接不了验证码，access 一到期就再也拿不回来。趁它还活着铸一把 crsr_，之后查用量、进网关、走 CRSR 通道都不再依赖它。注意 crsr_ 兑出来的 JWT 登不进 Cursor——切号仍要靠那把 session token，它过期就切不了。"
            : "铸一把长期 crsr_ Key 当备份凭证。session 过期后它还能查基础用量、走 CRSR 通道。"}
        </p>
      )}
      <div className="row" style={{ marginTop: 10 }}>
        <button type="button" className={urgent ? "btn btn-sm btn-primary" : "btn btn-sm"} disabled={busy} onClick={() => void mint()}>
          {busy ? <Spinner /> : null}
          {minted ? "再铸一把" : "铸一把 crsr_ Key"}
        </button>
      </div>
    </section>
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
