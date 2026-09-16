/**
 * 切号页 —— **一个切号池，一个动作**（ARCHITECTURE §4.1，目标是十秒内完成）。
 *
 * 「账号」是总库，切号池是用户明确挑出来的子集。`SwitchProfile` 的存在就是成员关系：
 * 加入时从账号凭证换一份登录态，移出时只删切号档，不删除账号。
 *
 * 正在用的成员置顶，样式和其余行一致。当前登录不在池中时只提供「加入当前账号」，
 * 不把它悄悄混进池里。
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { AccountCard } from "../accounts/AccountCard";
import { AccountInspector } from "../accounts/AccountInspector";
import { createCursorAccountView, type AccountView } from "../accounts/model";
import { accounts, app, switcher } from "../ipc/api";
import type { Account, AuthBackup, Overview, SwitchProfile, SwitchProgress } from "../ipc/types";
import { onSwitchProgress } from "../ipc/api";
import { go, type Route } from "../shell/nav";
import { Banner, Empty, ErrorNote, Icon, Modal, Picker, Spinner, Tag } from "../ui/primitives";
import { timeAgo } from "../ui/format";
import { sessionOnly } from "../ui/accounts";
import {
  buildSwitchPool,
  canAddToSwitchPool,
  DEFAULT_SWITCH_SORT,
  listAvailableSwitchAccounts,
  SWITCH_SORT_LABEL,
  type SwitchPoolEntry,
  type SwitchSort,
} from "../ui/switcher";
import { accountProblem, planLabel, planTone } from "../ui/usage";

const STEP_LABEL: Record<string, string> = {
  started: "开始切换",
  backedUp: "已备份当前登录",
  backupSkipped: "当前没有登录态，跳过备份",
  hotLoginSent: "已通知 Cursor 换号",
  hotLoginConfirmed: "Cursor 已吃进新登录态",
  hotProfileWritten: "已刷新账号名与档位缓存",
  cursorQuit: "Cursor 已退出",
  authWritten: "已写入登录态",
  machineSwitched: "已切换机器码",
  cursorLaunched: "已启动 Cursor",
  done: "完成",
  failed: "失败",
};

/** `failedAt` 用的是另一套词（见 Rust 侧 `step_of`），不能拿 STEP_LABEL 去查。 */
const FAILED_AT_LABEL: Record<string, string> = {
  prepare: "准备",
  "hot-login": "通知 Cursor 换号",
  "hot-confirm": "确认新登录态",
  quit: "退出 Cursor",
  "write-auth": "写入登录态",
  "write-machine": "写入机器码",
};

function describe(p: SwitchProgress): string {
  switch (p.step) {
    case "backedUp":
      return p.email ? `已备份当前登录（${p.email}）` : "已备份当前登录";
    case "cursorQuit":
      if (!p.wasRunning) return "Cursor 本来没在运行";
      return p.forced ? "已强制结束 Cursor" : "Cursor 已退出";
    case "authWritten":
      return `已写入登录态（${p.keys} 个键）`;
    case "machineSwitched":
      return `已切换机器码（${p.machineIdShort}…）`;
    case "failed":
      return `卡在「${FAILED_AT_LABEL[p.failedAt] ?? p.failedAt}」：${p.message}`;
    default:
      return STEP_LABEL[p.step] ?? p.step;
  }
}

function switchable(entry: SwitchPoolEntry): boolean {
  return entry.profile.hasAuth;
}

function blockReason(entry: SwitchPoolEntry): string | null {
  if (switchable(entry)) return null;
  if (entry.account?.status === "dead") return "已失效";
  return "缺登录态";
}

export function SwitcherPage({ route, onGo }: { route: Route; onGo: (r: Route) => void }) {
  const [overview, setOverview] = useState<Overview | null>(null);
  const [profiles, setProfiles] = useState<SwitchProfile[]>([]);
  const [backups, setBackups] = useState<AuthBackup[]>([]);
  const [known, setKnown] = useState<Account[]>([]);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [loaded, setLoaded] = useState(false);

  const [confirming, setConfirming] = useState<SwitchPoolEntry | null>(null);
  const [progress, setProgress] = useState<SwitchProgress[] | null>(null);
  const [showBackups, setShowBackups] = useState(false);
  const [adding, setAdding] = useState(false);
  const [preferredAccountId, setPreferredAccountId] = useState<string | null>(null);
  const [openKey, setOpenKey] = useState<string | null>(null);
  /** 池子的排法。只有「切号时间倒序」一档（默认），下拉先把顺序说出来，给以后的档留位置。 */
  const [sort, setSort] = useState<SwitchSort>(DEFAULT_SWITCH_SORT);
  /** 从别处带着某个号跳过来、但这个号切不了时的那句交代。 */
  const [notice, setNotice] = useState<string | null>(null);
  /** 设置里「切换时同时切机器码」。打开 = 强制冷切。 */
  const [switchMachineIds, setSwitchMachineIds] = useState(false);

  const reload = useCallback(async () => {
    try {
      const [o, list, b, status] = await Promise.all([
        switcher.overview(),
        switcher.list(),
        switcher.backups(),
        app.status(),
      ]);
      setOverview(o);
      setProfiles(list);
      setBackups(b);
      setSwitchMachineIds(status.switchMachineIds);
      setError(null);
    } catch (err) {
      setError(err);
    }
    // 账号拉不到时切号池仍可用，只是没有额度和可添加列表。
    try {
      setKnown(await accounts.list());
    } catch {
      setKnown([]);
    }
    setLoaded(true);
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // 进度靠事件推，不轮询（§4.3）。
  // `prev === null` 表示用户已经把进度弹窗关掉了；后续事件不再把它拉回来 ——
  // 切号在后台继续跑，但界面听用户的。
  useEffect(() => {
    const off = onSwitchProgress((p) =>
      setProgress((prev) => (prev === null ? null : [...prev, p])),
    );
    return () => {
      void off.then((fn) => fn());
    };
  }, []);

  const pool = useMemo(
    () => buildSwitchPool(profiles, known, overview?.current?.email),
    [known, profiles, overview?.current?.email],
  );
  const available = useMemo(
    () => listAvailableSwitchAccounts(known, profiles),
    [known, profiles],
  );

  const current = useMemo(() => pool.find((e) => e.isCurrent) ?? null, [pool]);
  const openEntry = useMemo(
    () => pool.find((entry) => entry.key === openKey) ?? null,
    [openKey, pool],
  );
  const currentOutsidePool =
    overview?.current?.email && !current ? overview.current.email : null;
  const readOnly = overview ? !isWritable(overview) : false;

  /** 从账号抽屉过来：池内账号直接确认，池外账号打开显式加入弹窗。 */
  const wanted = route.email?.toLowerCase();
  useEffect(() => {
    if (!wanted || !loaded) return;
    const entry = pool.find((e) => e.key === wanted);
    const account = known.find((candidate) => candidate.email.toLowerCase() === wanted);
    onGo(go("switcher"));
    setNotice(null);

    if (entry?.isCurrent) {
      setNotice(`Cursor 当前已登录 ${entry.email}`);
      return;
    }
    if (entry && switchable(entry)) {
      requestSwitch(entry);
      return;
    }
    if (account && canAddToSwitchPool(account)) {
      setPreferredAccountId(account.id);
      setAdding(true);
      return;
    }
    // 说不能切的**真实**原因：仅会话的号活着就能切，切不了只能是它过期了。
    setNotice(
      account
        ? sessionOnly(account)
          ? `${account.email} 的 session token 已过期；到凭证页粘一份新的，或授权一次拿到 refresh_token`
          : `${account.email} 需要先授权拿到 refresh_token，或粘一份 session token`
        : `${wanted} 不在账号中`,
    );
  }, [wanted, loaded, pool, known, onGo]);

  async function run(action: () => Promise<unknown>): Promise<void> {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
      await reload();
    }
  }

  async function doSwitch(entry: SwitchPoolEntry) {
    setConfirming(null);
    setProgress([]);
    await run(() => switcher.switchTo(entry.profile.id));
  }

  /** 热切不打断 Cursor，不必再确认；冷切会退出重启，仍要问一句。 */
  function requestSwitch(entry: SwitchPoolEntry) {
    if (overview?.cursorRunning && !switchMachineIds) {
      void doSwitch(entry);
      return;
    }
    setConfirming(entry);
  }

  function removeFromPool(entry: SwitchPoolEntry) {
    if (
      !entry.account &&
      !window.confirm(`移出 ${entry.email}？这个账号已不在“账号”中。`)
    ) {
      return;
    }
    setOpenKey(null);
    void run(() => switcher.remove(entry.profile.id));
  }

  return (
    <div>
      <div className="page-head">
        <div className="page-title-line">
          <h1>切号池</h1>
          {loaded ? (
            <span className="page-title-meta">
              {pool.length} 个账号{overview?.current?.email ? "" : " · Cursor 未登录"}
            </span>
          ) : null}
          {overview ? (
            <span
              className="page-title-meta"
              title={overview.machineIdOwner ? `机器码属于 ${overview.machineIdOwner}` : "本机原始机器码"}
            >
              机器码 <code className="mono">{overview.machineIdShort || "—"}</code>
            </span>
          ) : null}
        </div>
        <div className="row">
          {/* 一份备份都没有时不摆一个灰按钮 —— 那只是在告诉用户「这里有个东西你用不了」。 */}
          {backups.length > 0 ? (
            <button type="button" className="btn" onClick={() => setShowBackups(true)}>
              备份 {backups.length}
            </button>
          ) : null}
          {loaded ? (
            <button
              type="button"
              className="btn"
              disabled={busy}
              onClick={() => {
                setPreferredAccountId(null);
                setAdding(true);
              }}
            >
              <Icon name="plus" size={13} />
              添加账号
            </button>
          ) : null}
          {currentOutsidePool ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy}
              onClick={() => void run(() => switcher.captureCurrent())}
              title={currentOutsidePool}
            >
              <Icon name="plus" size={14} />
              加入当前账号
            </button>
          ) : null}
        </div>
      </div>

      <ErrorNote error={error} onRetry={() => void reload()} />
      {notice ? (
        <div style={{ marginBottom: 14 }}>
          <Banner
            title={notice}
            action={
              <button type="button" className="btn btn-sm btn-quiet" onClick={() => setNotice(null)}>
                知道了
              </button>
            }
          />
        </div>
      ) : null}

      {readOnly && overview ? (
        <div style={{ marginBottom: 14 }}>
          <Banner
            tone="warn"
            title={explain(overview) ?? "Cursor 状态库不可写，切号已降级为只读。"}
          />
        </div>
      ) : null}

      {!loaded ? (
        <div className="accts">
          <div className="skeleton" style={{ height: 164 }} />
          <div className="skeleton" style={{ height: 164 }} />
          <div className="skeleton" style={{ height: 164 }} />
        </div>
      ) : pool.length === 0 ? (
        <Empty title="切号池为空" />
      ) : (
        <>
          {/* 和「账号」页同一条工具条的位置。此刻排序只有一档，它先把「这列是按什么排的」
              说出来 —— 一列号凭什么这个顺序，不该靠用户自己悟。 */}
          <div className="toolbar">
            <span className="grow" />
            <Picker<SwitchSort>
              icon="sort"
              label="排序"
              value={sort}
              options={(Object.keys(SWITCH_SORT_LABEL) as SwitchSort[]).map((s) => ({
                id: s,
                label: SWITCH_SORT_LABEL[s],
              }))}
              onChange={setSort}
            />
          </div>
          <div className="accts">
            {pool.map((entry) => (
              <PoolRow
                key={entry.key}
                entry={entry}
                busy={busy}
                readOnly={readOnly}
                open={entry.key === openKey}
                onOpen={() => setOpenKey(entry.key)}
                onSwitch={() => requestSwitch(entry)}
                onRemove={() => removeFromPool(entry)}
              />
            ))}
          </div>
        </>
      )}

      {openEntry ? (
        <AccountInspector
          view={switcherAccountView(openEntry)}
          inCursor={openEntry.isCurrent}
          onClose={() => setOpenKey(null)}
          onChanged={reload}
          onSwitch={() => {
            setOpenKey(null);
            if (openEntry.isCurrent) {
              setNotice(`Cursor 当前已登录 ${openEntry.email}`);
            } else if (switchable(openEntry)) {
              requestSwitch(openEntry);
            }
          }}
          placementActions={
            <button
              type="button"
              className="btn btn-sm btn-danger"
              disabled={busy}
              onClick={() => removeFromPool(openEntry)}
            >
              移出切号池
            </button>
          }
          onOpenLibrary={() => onGo(go("accounts"))}
        />
      ) : null}

      {confirming ? (
        <ConfirmSwitch
          entry={confirming}
          hot={Boolean(overview?.cursorRunning) && !switchMachineIds}
          onCancel={() => setConfirming(null)}
          onConfirm={() => void doSwitch(confirming)}
        />
      ) : null}

      {progress ? (
        <ProgressModal
          steps={progress}
          busy={busy}
          onClose={() => setProgress(null)}
          onRestore={(id) => {
            setProgress(null);
            void run(() => switcher.restoreBackup(id));
          }}
        />
      ) : null}

      {showBackups ? (
        <BackupsModal
          backups={backups}
          busy={busy}
          onClose={() => setShowBackups(false)}
          onRestore={(id) => {
            setShowBackups(false);
            setProgress([]);
            void run(() => switcher.restoreBackup(id));
          }}
          onRemove={(id) => void run(() => switcher.removeBackup(id))}
        />
      ) : null}

      {adding ? (
        <AddToSwitchPoolModal
          accounts={available}
          busy={busy}
          preferredAccountId={preferredAccountId}
          onClose={() => {
            setAdding(false);
            setPreferredAccountId(null);
          }}
          onAdd={async (accountIds) => {
            await run(async () => {
              for (const accountId of accountIds) {
                await accounts.addToSwitchBook(accountId);
              }
            });
            setAdding(false);
            setPreferredAccountId(null);
          }}
          onGoAccounts={() => {
            setAdding(false);
            setPreferredAccountId(null);
            onGo(go("accounts"));
          }}
        />
      ) : null}
    </div>
  );
}

/**
 * 能不能写 —— 必须和 Rust 侧 `SchemaCheck::writable` 逐条一致，否则界面会禁掉一个
 * 其实做得了的操作（或者反过来）。
 *
 * 要点：一个 auth 键都没有 = Cursor 没登录过，那正是该写的时候；有几个却缺了必需的
 * 才是键名漂移。
 */
function isWritable(o: Overview): boolean {
  const c = o.check;
  if (!c.dbPresent || !c.tablePresent) return false;
  if (c.presentKeys.length === 0) return true;
  return ["cursorAuth/accessToken", "cursorAuth/refreshToken"].every((k) =>
    c.presentKeys.includes(k),
  );
}

function explain(o: Overview): string | null {
  const c = o.check;
  if (!c.dbPresent) return "没找到 Cursor 的登录态库，切号功能不可用。";
  if (!c.tablePresent) return "Cursor 的登录态库结构与预期不符。";
  if (!isWritable(o)) return "Cursor 的登录态键名与预期不符，它可能升级后改了存储结构。";
  return null;
}

/**
 * 池子里的一行。骨架与「账号」「本地网关」共用（`AccountLine`），**正在用的那一行也是同一副**。
 *
 * 额度来自 `account` 那一半：决定「现在要不要换一个」看的就是这些数字，和另外两页同一个判断。
 * 只剩快照的号（从没进过号池）没有额度可查，就照实说一句，不装作它是零。
 */
function switcherAccountView(entry: SwitchPoolEntry): AccountView {
  return createCursorAccountView({
    label: entry.email,
    managed: entry.account,
    placement: {
      kind: "switcher",
      label: "切号池",
      detail: entry.isCurrent
        ? "Cursor 当前登录"
        : entry.profile.lastSwitchedAt
          ? `${timeAgo(entry.profile.lastSwitchedAt)}切过`
          : "还没切换过",
    },
    unavailableReason: entry.account
      ? undefined
      : "只保存了切号登录态；未在账号库托管，无法查询完整用量",
  });
}

function PoolRow({
  entry,
  busy,
  readOnly,
  open,
  onOpen,
  onSwitch,
  onRemove,
}: {
  entry: SwitchPoolEntry;
  busy: boolean;
  readOnly: boolean;
  open: boolean;
  onOpen: () => void;
  onSwitch: () => void;
  onRemove: () => void;
}) {
  const { account, profile, isCurrent } = entry;
  const u = account?.usage;
  const blocked = blockReason(entry);
  const problem = account ? accountProblem(account, u) : null;
  // 一行最多一个坏消息。「待登录」和「需要先授权」是同一件事的两种说法，两个都摆
  // 只会让人以为有两处要修；账号自己的问题更靠近根因，优先说它。
  const wrong = problem?.label ?? blocked;

  return (
    <AccountCard
      view={switcherAccountView(entry)}
      highlighted={isCurrent || open}
      onOpen={onOpen}
      badges={
        isCurrent ? <Tag tone="ok">当前登录</Tag> : null
      }
      // 备注不摆（那是私事，不是扫列表时读的）；只留问题和「上次什么时候切过」——
      // 后者是这一页专有的操作状态，决定「该轮到哪个了」。红字的样式和「账号」页一致。
      note={
        wrong ? (
          <span className="acct-problem">{wrong}</span>
        ) : (
          <span>
            {profile.lastSwitchedAt ? `${timeAgo(profile.lastSwitchedAt)}切过` : "未切换过"}
          </span>
        )
      }
      actions={
        <>
          <span className="acct-hover">
            <button
              type="button"
              className="btn btn-sm btn-icon btn-soft btn-danger"
              disabled={busy}
              onClick={onRemove}
              data-tip="移出切号池"
              aria-label={`将 ${entry.email} 移出切号池`}
            >
              <Icon name="trash" size={13} />
            </button>
          </span>
          {/* 用有厚度的默认按钮而不是实心薄荷：一列几十行，每行一个亮色 CTA 会把列表
              点成一片；而带内高光和描边的那一版仍然一眼看得出可以按。
              切换只留 ⇄，文案交给悬停的气泡 —— 一屏几十张卡，每张都写「切换」是同一句话说几十遍。
              「使用中」是状态不是动作，它得留着字。 */}
          {isCurrent ? (
            <button type="button" className="btn btn-sm" disabled>
              使用中
            </button>
          ) : (
            <button
              type="button"
              className="btn btn-sm btn-icon tip-end"
              disabled={busy || Boolean(blocked) || readOnly}
              onClick={onSwitch}
              aria-label={`切换到 ${entry.email}`}
              data-tip={blocked ? "移出后重新加入切号池" : readOnly ? "切号已降级为只读" : "切到这个号"}
            >
              <Icon name="switcher" size={14} />
            </button>
          )}
        </>
      }
    />
  );
}

function AddToSwitchPoolModal({
  accounts: availableAccounts,
  busy,
  preferredAccountId,
  onClose,
  onAdd,
  onGoAccounts,
}: {
  accounts: Account[];
  busy: boolean;
  preferredAccountId: string | null;
  onClose: () => void;
  onAdd: (accountIds: string[]) => Promise<void>;
  onGoAccounts: () => void;
}) {
  const [selectedAccountIds, setSelectedAccountIds] = useState<Set<string>>(
    () =>
      preferredAccountId &&
      availableAccounts.some((account) => account.id === preferredAccountId)
        ? new Set([preferredAccountId])
        : new Set(),
  );

  const allSelected =
    availableAccounts.length > 0 &&
    selectedAccountIds.size === availableAccounts.length;

  function toggleAccount(accountId: string) {
    setSelectedAccountIds((currentIds) => {
      const nextIds = new Set(currentIds);
      if (nextIds.has(accountId)) {
        nextIds.delete(accountId);
      } else {
        nextIds.add(accountId);
      }
      return nextIds;
    });
  }

  return (
    <Modal
      title="添加到切号池"
      onClose={busy ? () => {} : onClose}
      footer={
        <>
          <button type="button" className="btn" disabled={busy} onClick={onClose}>
            取消
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy || selectedAccountIds.size === 0}
            onClick={() => void onAdd([...selectedAccountIds])}
          >
            {busy
              ? <Spinner />
              : selectedAccountIds.size > 0
                ? `添加 ${selectedAccountIds.size} 个`
                : "添加"}
          </button>
        </>
      }
    >
      {availableAccounts.length === 0 ? (
        <Empty
          title="没有可添加的账号"
          action={
            <button type="button" className="btn btn-sm" onClick={onGoAccounts}>
              去账号
            </button>
          }
        />
      ) : (
        <div className="stack" style={{ gap: 10 }}>
          <div className="row-between">
            <span className="muted tiny">{availableAccounts.length} 个可添加</span>
            <button
              type="button"
              className="linkish"
              disabled={busy}
              onClick={() =>
                setSelectedAccountIds(
                  allSelected
                    ? new Set()
                    : new Set(availableAccounts.map((account) => account.id)),
                )
              }
            >
              {allSelected ? "全不选" : "全选"}
            </button>
          </div>
          <div className="list">
            {availableAccounts.map((account) => {
              const selected = selectedAccountIds.has(account.id);
              const usage = account.usage;
              return (
                <label
                  key={account.id}
                  className={
                    selected
                      ? "list-row enroll-row is-on"
                      : "list-row enroll-row"
                  }
                >
                  <input
                    type="checkbox"
                    className="tick"
                    checked={selected}
                    disabled={busy}
                    onChange={() => toggleAccount(account.id)}
                  />
                  <span className="grow" style={{ minWidth: 0 }}>
                    <span className="row" style={{ gap: 8 }}>
                      <span
                        className="mono selectable truncate"
                        style={{ fontSize: 13 }}
                      >
                        {account.email}
                      </span>
                      {usage?.plan ? (
                        <span className={`plan ${planTone(usage)}`}>
                          {planLabel(usage)}
                        </span>
                      ) : null}
                    </span>
                    {usage?.totalPercentUsed != null ? (
                      <span className="faint tiny">
                        总额度已用 {Math.round(usage.totalPercentUsed)}%
                      </span>
                    ) : null}
                  </span>
                </label>
              );
            })}
          </div>
        </div>
      )}
    </Modal>
  );
}

function ConfirmSwitch({
  entry,
  hot,
  onCancel,
  onConfirm,
}: {
  entry: SwitchPoolEntry;
  /** 这次会走热切（不退出 Cursor）。 */
  hot: boolean;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  return (
    <Modal
      title={`切换到 ${entry.email}`}
      onClose={onCancel}
      footer={
        <>
          <button type="button" className="btn" onClick={onCancel}>
            取消
          </button>
          <button type="button" className="btn btn-primary" onClick={onConfirm}>
            确认切换
          </button>
        </>
      }
    >
      {hot ? (
        <Tag tone="ok">无需重启 Cursor</Tag>
      ) : (
        <Banner tone="warn" title="将重启 Cursor，请先保存未保存的改动" />
      )}
    </Modal>
  );
}

function ProgressModal({
  steps,
  busy,
  onClose,
  onRestore,
}: {
  steps: SwitchProgress[];
  busy: boolean;
  onClose: () => void;
  onRestore: (backupId: string) => void;
}) {
  const failed = steps.find((s) => s.step === "failed");
  const done = steps.some((s) => s.step === "done");
  const restorable = failed?.step === "failed" ? failed.backupId : null;
  // 写库之后才失败的话，Cursor 里已经是新号了。这时说「没有被改动」是在误导用户。
  const wroteAuth = steps.some((s) => s.step === "authWritten");

  return (
    <Modal
      title={failed ? "切换失败" : done ? "切换完成" : "正在切换…"}
      onClose={onClose}
      footer={
        <>
          {restorable ? (
            <button type="button" className="btn" onClick={() => onRestore(restorable)}>
              还原到切换前
            </button>
          ) : null}
          <button type="button" className="btn btn-primary" onClick={onClose} disabled={busy}>
            {busy ? "进行中…" : "关闭"}
          </button>
        </>
      }
    >
      <div className="steps">
        {steps.map((s, i) => (
          <div key={`${s.step}-${i}`} className={`step${s.step === "failed" ? " is-failed" : ""}`}>
            <span className="step-dot">{s.step === "failed" ? "✕" : "✓"}</span>
            <span>{describe(s)}</span>
          </div>
        ))}
        {busy && !failed && !done ? (
          <div className="step">
            <span className="step-dot">
              <Spinner />
            </span>
            <span>处理中…</span>
          </div>
        ) : null}
      </div>
      {failed ? (
        <div style={{ marginTop: 16 }}>
          <Banner
            tone="bad"
            title={
              wroteAuth
                ? "登录态已经切成新号了，只是后面的步骤没做完。"
                : "已停在原地，登录态没有被改动。"
            }
            hint={
              wroteAuth
                ? "重新切一次即可补上剩下的步骤；也可以用上面的「还原到切换前」退回去。"
                : "可以直接重试；或者用上面的「还原到切换前」回到刚才的状态。"
            }
          />
        </div>
      ) : null}
    </Modal>
  );
}

const REASON_LABEL: Record<string, string> = {
  "pre-switch": "切换前",
  "pre-restore": "还原前",
  manual: "手动",
};

function BackupsModal({
  backups,
  busy,
  onClose,
  onRestore,
  onRemove,
}: {
  backups: AuthBackup[];
  busy: boolean;
  onClose: () => void;
  onRestore: (id: string) => void;
  onRemove: (id: string) => void;
}) {
  return (
    <Modal title="登录态备份" subtitle="还原只改登录态，不动机器码。" onClose={onClose}>
      {backups.length === 0 ? (
        <Empty>还没有备份。</Empty>
      ) : (
        <div className="list">
          {backups.map((b) => (
            <div className="list-row" key={b.id}>
              <div className="grow">
                <div className="selectable truncate">{b.email ?? "（未登录状态）"}</div>
                <div className="faint" style={{ fontSize: 12.5 }}>
                  {REASON_LABEL[b.reason] ?? b.reason} · {timeAgo(b.createdAt)}
                </div>
              </div>
              <button
                type="button"
                className="btn btn-sm"
                disabled={busy}
                onClick={() => onRestore(b.id)}
              >
                还原
              </button>
              <button
                type="button"
                className="btn btn-sm btn-danger"
                disabled={busy}
                onClick={() => onRemove(b.id)}
              >
                删除
              </button>
            </div>
          ))}
        </div>
      )}
    </Modal>
  );
}
