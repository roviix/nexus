/**
 * Nexus 2.0 切号页 —— **统一切号轨道，免二次入池**。
 *
 * 合并 Cursor 账号与切号快照为一个即改即用的列表。
 * - 热切（不改机器码，Cursor 运行中）：十秒内静默换号，不打断 IDE 工作流。
 * - 冷切（勾选重置机器码或 Cursor 未启动）：确认后退出 Cursor、重置机器码并自动重启。
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { AccountCard } from "../accounts/AccountCard";
import { AccountInspector } from "../accounts/AccountInspector";
import { createCursorAccountView, type AccountView } from "../accounts/model";
import { accounts, app, switcher, onSwitchProgress } from "../ipc/api";
import type { Account, AuthBackup, Overview, SwitchProfile, SwitchProgress } from "../ipc/types";
import { go, type Route } from "../shell/nav";
import { Banner, Empty, ErrorNote, Icon, Modal, Picker, Spinner, Tag } from "../ui/primitives";
import { confirm } from "../ui/confirm";
import { timeAgo } from "../ui/format";
import { sessionOnly } from "../ui/accounts";
import {
  buildUnifiedSwitchList,
  canAddToSwitchPool,
  DEFAULT_SWITCH_SORT,
  SWITCH_SORT_LABEL,
  type SwitchSort,
  type UnifiedSwitchEntry,
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

function blockReason(entry: UnifiedSwitchEntry): string | null {
  if (entry.canSwitch) return null;
  if (entry.account?.status === "dead") return "已失效";
  if (entry.account && !canAddToSwitchPool(entry.account)) return "需先授权或更新 Token";
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

  const [confirming, setConfirming] = useState<UnifiedSwitchEntry | null>(null);
  const [progress, setProgress] = useState<SwitchProgress[] | null>(null);
  const [showBackups, setShowBackups] = useState(false);
  const [openKey, setOpenKey] = useState<string | null>(null);
  const [sort, setSort] = useState<SwitchSort>(DEFAULT_SWITCH_SORT);
  const [notice, setNotice] = useState<string | null>(null);
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

  useEffect(() => {
    const off = onSwitchProgress((p) =>
      setProgress((prev) => (prev === null ? null : [...prev, p])),
    );
    return () => {
      void off.then((fn) => fn());
    };
  }, []);

  const unifiedList = useMemo(
    () => buildUnifiedSwitchList(profiles, known, overview?.current?.email),
    [known, profiles, overview?.current?.email],
  );

  const current = useMemo(() => unifiedList.find((e) => e.isCurrent) ?? null, [unifiedList]);
  const currentAccount = useMemo(
    () =>
      known.find(
        (a) =>
          a.email.toLowerCase() ===
          (overview?.current?.email || current?.email || "").toLowerCase(),
      ) ?? null,
    [known, overview?.current?.email, current?.email],
  );

  const openEntry = useMemo(
    () => unifiedList.find((entry) => entry.key === openKey) ?? null,
    [openKey, unifiedList],
  );

  const currentOutsidePool =
    overview?.current?.email && !current?.profile ? overview.current.email : null;
  const readOnly = overview ? !overview.check.writable : false;

  const wanted = route.email?.toLowerCase();
  useEffect(() => {
    if (!wanted || !loaded) return;
    const entry = unifiedList.find((e) => e.key === wanted);
    const account = known.find((candidate) => candidate.email.toLowerCase() === wanted);
    onGo(go("switcher"));
    setNotice(null);

    if (entry?.isCurrent) {
      setNotice(`Cursor 当前已登录 ${entry.email}`);
      return;
    }
    if (entry && entry.canSwitch) {
      requestSwitch(entry);
      return;
    }
    setNotice(
      account
        ? sessionOnly(account)
          ? `${account.email} 的 session token 已过期；到凭证页更新 Token 即可换号`
          : `${account.email} 需要先授权或粘贴有效凭证`
        : `${wanted} 不在账号库中`,
    );
  }, [wanted, loaded, unifiedList, known, onGo]);

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

  async function doSwitch(entry: UnifiedSwitchEntry) {
    setConfirming(null);
    setProgress([]);
    await run(async () => {
      let profileId = entry.profile?.id;
      if (!profileId && entry.account) {
        // 自动完成入池
        const enrolled = await accounts.addToSwitchBook(entry.account.id);
        profileId = enrolled.id;
      }
      if (!profileId) {
        throw new Error("无法切换：未找到可用的账号或快照档案");
      }
      await switcher.switchTo(profileId);
    });
  }

  function requestSwitch(entry: UnifiedSwitchEntry) {
    // 热切条件：Cursor 正在运行且未勾选重置机器码
    if (overview?.cursorRunning && !switchMachineIds) {
      void doSwitch(entry);
      return;
    }
    setConfirming(entry);
  }

  async function removeFromPool(entry: UnifiedSwitchEntry) {
    if (!entry.profile) return;
    if (!entry.account) {
      const ok = await confirm(
        `${entry.email} 未在账号库托管，移出后此快照凭证将彻底丢失。`,
        {
          title: "移除切号快照",
          okLabel: "移除",
          danger: true,
        },
      );
      if (!ok) return;
    }
    setOpenKey(null);
    await run(() => switcher.remove(entry.profile!.id));
  }

  return (
    <div>
      <div className="page-head">
        <div className="page-title-line">
          <h1>Cursor 换号</h1>
          {loaded ? (
            <span className="page-title-meta">
              {unifiedList.length} 个账号 · 独立 IDE 轨道
            </span>
          ) : null}
        </div>
        <div className="row" style={{ gap: 8 }}>
          {backups.length > 0 ? (
            <button type="button" className="btn btn-sm" onClick={() => setShowBackups(true)}>
              备份快照 ({backups.length})
            </button>
          ) : null}
          <button
            type="button"
            className="btn btn-sm"
            onClick={() => onGo(go("accounts"))}
          >
            <Icon name="plus" size={13} />
            账号库管理
          </button>
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
            title={overview.check.blockedReason ?? "Cursor 状态库不可写，切号已降级为只读。"}
          />
        </div>
      ) : null}

      {/* Nexus 2.0 当前 IDE 身份与机器码状态看板 */}
      <div className="cursor-identity-banner">
        <div className="cursor-identity-left">
          <div className="cursor-identity-avatar">
            <Icon name="cursor" size={20} />
          </div>
          <div className="cursor-identity-info">
            <div className="cursor-identity-header">
              <span className="cursor-identity-label">当前 IDE 登录身份</span>
              {overview?.cursorRunning ? (
                <span className="live-pill ok">● Cursor 运行中</span>
              ) : (
                <span className="live-pill muted">○ Cursor 未运行</span>
              )}
            </div>
            <div className="cursor-identity-meta">
              <span className="cursor-identity-email mono">
                {overview?.current?.email || "Cursor 当前未登录任何账号"}
              </span>
              {currentAccount?.usage?.plan ? (
                <span className={`plan ${planTone(currentAccount.usage)}`}>
                  {planLabel(currentAccount.usage)}
                </span>
              ) : overview?.current?.membership ? (
                <span className="plan plan-pro">{overview.current.membership}</span>
              ) : null}
              {currentAccount?.usage?.totalPercentUsed != null ? (
                <span className="cursor-quota-pill">
                  总用量 {Math.round(currentAccount.usage.totalPercentUsed)}%
                </span>
              ) : null}
              <span className="mono text-faint" title="本机当前 Cursor 机器标识">
                机器码: {overview?.machineIdShort || "—"}
              </span>
            </div>
          </div>
        </div>

        <div className="cursor-identity-right">
          <label className="cursor-toggle-label" title="开启后每次换号将生成全新机器码并重启 Cursor IDE">
            <input
              type="checkbox"
              checked={switchMachineIds}
              disabled={busy}
              onChange={async (e) => {
                const nextVal = e.target.checked;
                setSwitchMachineIds(nextVal);
                try {
                  await app.updateSettings({ switchMachineIds: nextVal });
                } catch (err) {
                  console.error("更新机器码配置失败", err);
                }
              }}
            />
            <span>切换时同时重置机器码 (需重启 IDE)</span>
          </label>
          {currentOutsidePool ? (
            <button
              type="button"
              className="btn btn-sm btn-soft"
              disabled={busy}
              onClick={() => void run(() => switcher.captureCurrent())}
              title={currentOutsidePool}
            >
              <Icon name="plus" size={13} />
              收录当前登录
            </button>
          ) : null}
        </div>
      </div>

      {!loaded ? (
        <div className="accts">
          <div className="skeleton" style={{ height: 120 }} />
          <div className="skeleton" style={{ height: 120 }} />
        </div>
      ) : unifiedList.length === 0 ? (
        <Empty
          title="暂无 Cursor 账号"
          action={
            <button type="button" className="btn btn-sm btn-primary" onClick={() => onGo(go("accounts"))}>
              + 添加 Cursor 账号
            </button>
          }
        >
          在「账号库」中添加或导入 Cursor 账号凭证，无需复杂的配置，即可在此一键自由换号。
        </Empty>
      ) : (
        <>
          <div className="toolbar" style={{ marginTop: 16 }}>
            <span className="faint tiny">共 {unifiedList.length} 个账号可随时切换</span>
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
            {unifiedList.map((entry) => (
              <UnifiedPoolRow
                key={entry.key}
                entry={entry}
                busy={busy}
                readOnly={readOnly}
                switchMachineIds={switchMachineIds}
                open={entry.key === openKey}
                onOpen={() => setOpenKey(entry.key)}
                onSwitch={() => requestSwitch(entry)}
                onRemove={() => void removeFromPool(entry)}
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
            } else if (openEntry.canSwitch) {
              requestSwitch(openEntry);
            }
          }}
          placementActions={
            openEntry.profile ? (
              <button
                type="button"
                className="btn btn-sm btn-danger"
                disabled={busy}
                onClick={() => void removeFromPool(openEntry)}
              >
                移除切号快照
              </button>
            ) : null
          }
          onOpenLibrary={() => onGo(go("accounts"))}
        />
      ) : null}

      {confirming ? (
        <ConfirmSwitch
          entry={confirming}
          hot={Boolean(overview?.cursorRunning) && !switchMachineIds}
          resetMachine={switchMachineIds}
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
    </div>
  );
}

function switcherAccountView(entry: UnifiedSwitchEntry): AccountView {
  return createCursorAccountView({
    label: entry.email,
    managed: entry.account,
    placement: {
      kind: "switcher",
      label: "切号",
      detail: entry.isCurrent
        ? "Cursor 当前登录"
        : entry.profile?.lastSwitchedAt
          ? `${timeAgo(entry.profile.lastSwitchedAt)}切过`
          : "可随时一键上号",
    },
    unavailableReason: entry.account
      ? undefined
      : "只保存了切号登录态快照；未在账号库托管，无法查询完整用量",
  });
}

function UnifiedPoolRow({
  entry,
  busy,
  readOnly,
  switchMachineIds,
  open,
  onOpen,
  onSwitch,
  onRemove,
}: {
  entry: UnifiedSwitchEntry;
  busy: boolean;
  readOnly: boolean;
  switchMachineIds: boolean;
  open: boolean;
  onOpen: () => void;
  onSwitch: () => void;
  onRemove: () => void;
}) {
  const { account, profile, isCurrent } = entry;
  const u = account?.usage;
  const blocked = blockReason(entry);
  const problem = account ? accountProblem(account, u) : null;
  const wrong = problem?.label ?? blocked;

  return (
    <AccountCard
      view={switcherAccountView(entry)}
      highlighted={open}
      current={isCurrent}
      onOpen={onOpen}
      badges={null}
      note={
        wrong ? (
          <span className="acct-problem">{wrong}</span>
        ) : (
          <span>
            {profile?.lastSwitchedAt
              ? `${timeAgo(profile.lastSwitchedAt)}切过`
              : "准备就绪"}
          </span>
        )
      }
      actions={
        <>
          {profile ? (
            <span className="acct-hover">
              <button
                type="button"
                className="btn btn-sm btn-icon btn-soft btn-danger"
                disabled={busy}
                onClick={onRemove}
                data-tip="移除切号快照"
                aria-label={`将 ${entry.email} 移出切号快照`}
              >
                <Icon name="trash" size={13} />
              </button>
            </span>
          ) : null}
          {isCurrent ? (
            <button type="button" className="btn btn-sm" disabled>
              当前登录
            </button>
          ) : (
            <button
              type="button"
              className={switchMachineIds ? "btn btn-sm" : "btn btn-sm btn-primary"}
              disabled={busy || !entry.canSwitch || readOnly}
              onClick={onSwitch}
              data-tip={
                blocked
                  ? blocked
                  : switchMachineIds
                    ? "重置机器码并冷切重启 IDE"
                    : "热切上号，无需重启 IDE"
              }
            >
              {switchMachineIds ? "↻ 重启并换号" : "⚡ 热切上号"}
            </button>
          )}
        </>
      }
    />
  );
}

function ConfirmSwitch({
  entry,
  hot,
  resetMachine,
  onCancel,
  onConfirm,
}: {
  entry: UnifiedSwitchEntry;
  hot: boolean;
  resetMachine: boolean;
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
        <Tag tone="ok">无需重启 Cursor (热切)</Tag>
      ) : resetMachine ? (
        <Banner tone="warn" title="将同时重置机器码并重启 Cursor，请先保存未保存的改动。" />
      ) : (
        <Banner tone="warn" title="Cursor 未在运行，切换后将自动启动 Cursor。" />
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
