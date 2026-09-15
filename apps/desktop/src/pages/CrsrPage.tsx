/**
 * CRSR 页 —— 给本机 Cursor 打独立鉴权补丁。
 *
 * 装补丁是一次性的机制；用哪个号在账号抽屉里点「用作 CRSR 通道」，不必重装。
 */
import { useCallback, useEffect, useState } from "react";
import { crsr, onCrsrProgress } from "../ipc/api";
import type { CrsrBackup, CrsrOutcome, CrsrProgress, CrsrStatus } from "../ipc/types";
import { go, type Route } from "../shell/nav";
import { Banner, Empty, ErrorNote, Icon, Modal, Spinner, Tag } from "../ui/primitives";
import { timeAgo, timeUntil } from "../ui/format";

export const STEP_LABEL: Record<CrsrProgress["step"], string> = {
  preflight: "预检版本与锚点",
  backup: "备份改动前的文件",
  quit_cursor: "退出 Cursor",
  write: "写入补丁",
  verify: "校验完整性",
  launch: "启动 Cursor",
  done: "完成",
};

/**
 * `embedded`：作为「Cursor 面板」页的一档渲染时，标题降一级——页头由外层给。
 */
export function CrsrPage({ onGo, embedded = false }: { onGo: (r: Route) => void; embedded?: boolean }) {
  const [status, setStatus] = useState<CrsrStatus | null>(null);
  const [backups, setBackups] = useState<CrsrBackup[]>([]);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [progress, setProgress] = useState<CrsrProgress[]>([]);
  const [outcome, setOutcome] = useState<CrsrOutcome | null>(null);
  const [showBackups, setShowBackups] = useState(false);
  const [relaunch, setRelaunch] = useState(true);

  const reload = useCallback(async () => {
    setError(null);
    const results = await Promise.allSettled([
      crsr.status().then(setStatus),
      crsr.backups().then(setBackups),
    ]);
    for (const result of results) {
      if (result.status === "rejected") {
        setError(result.reason);
        break;
      }
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    const off = onCrsrProgress((p) => setProgress((prev) => [...prev, p]));
    return () => {
      void off.then((f) => f());
    };
  }, []);

  async function run(action: () => Promise<CrsrOutcome>) {
    setBusy(true);
    setError(null);
    setOutcome(null);
    setProgress([]);
    try {
      const o = await action();
      setOutcome(o);
      setStatus(o.status);
      setBackups(await crsr.backups());
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const canInstall = !!status && status.versionSupported && !status.sandConflict && !busy;
  const canUninstall = !!status && status.installed && !busy;
  const needsAccount = !!status?.complete && !status.credential;

  return (
    <div>
      <div className={embedded ? "section-head" : "page-head"}>
        {embedded ? <h2>CRSR 通道</h2> : <h1>CRSR 通道</h1>}
        <div className="row" style={{ flexWrap: "wrap", justifyContent: "flex-end" }}>
          {backups.length > 0 ? (
            <button type="button" className="btn btn-sm" onClick={() => setShowBackups(true)}>
              <Icon name="archive" size={13} />
              备份 {backups.length}
            </button>
          ) : null}
          <button
            type="button"
            className="btn btn-sm btn-icon btn-soft"
            onClick={() => void reload()}
            disabled={busy}
            title="刷新"
            aria-label="刷新"
          >
            <Icon name="refresh" size={13} />
          </button>
        </div>
      </div>

      <ErrorNote error={error} onRetry={() => void reload()} />

      {!status ? (
        <div className="skeleton" style={{ height: 96 }} />
      ) : (
        <>
          <StatusCard status={status} />

          {!status.versionSupported ? (
            <div style={{ marginTop: 12 }}>
              <Banner
                tone="warn"
                title={`当前 Cursor ${status.cursorVersion ?? "未知"}，补丁只适配 ${status.supportedVersion}。`}
                hint="等待适配即可，Cursor 不会被改动。版本与 Sand 通道绑在同一份 Cursor 发行上。"
              />
            </div>
          ) : null}

          {status.sandConflict ? (
            <div style={{ marginTop: 12 }}>
              <Banner
                tone="bad"
                title={status.sandConflict}
                hint="两条补丁改的是同一段 applyAuthorization，不能同时装。"
                action={
                  <button type="button" className="btn btn-sm" onClick={() => onGo(go("panel", { mode: "sand" }))}>
                    去 Sand 那一档
                  </button>
                }
              />
            </div>
          ) : null}

          {needsAccount ? (
            <div style={{ marginTop: 12 }}>
              <Banner
                tone="warn"
                title="补丁已装，还没选号。"
                hint="到账号抽屉凭证页，对一份有 crsr_ API Key 的号点「用作 CRSR 通道」。不必重装。"
                action={
                  <button type="button" className="btn btn-sm" onClick={() => onGo(go("accounts"))}>
                    去账号
                  </button>
                }
              />
            </div>
          ) : null}

          {outcome ? <OutcomeBanner outcome={outcome} needsAccount={needsAccount} onGoAccounts={() => onGo(go("accounts"))} /> : null}

          <CurrentAccount status={status} onGoAccounts={() => onGo(go("accounts"))} />

          {progress.length > 0 ? (
            <Modal
              title={busy ? "正在处理" : "已中止"}
              subtitle={busy ? "会退出 Cursor；未保存的工作请先保存。" : undefined}
              onClose={busy ? () => {} : () => setProgress([])}
              compact
            >
              <div className="steps">
                {progress.map((p, i) => (
                  <div key={`${p.step}-${i}`} className="step">
                    <span className="step-dot">{i === progress.length - 1 && busy ? <Spinner /> : "✓"}</span>
                    <span>{STEP_LABEL[p.step]}</span>
                    <span className="faint tiny">{p.detail}</span>
                  </div>
                ))}
              </div>
            </Modal>
          ) : null}

          <div className="row" style={{ marginTop: 20, flexWrap: "wrap", gap: 8 }}>
            <label className="row items-center tiny muted" style={{ gap: 6, marginRight: "auto" }}>
              <input
                type="checkbox"
                checked={relaunch}
                disabled={busy}
                onChange={(e) => setRelaunch(e.target.checked)}
              />
              写完重启 Cursor
            </label>
            {canUninstall ? (
              <button
                type="button"
                className="btn"
                disabled={busy}
                onClick={() => void run(() => crsr.uninstall(relaunch))}
              >
                {busy ? <Spinner /> : null}
                卸载
              </button>
            ) : null}
            <button
              type="button"
              className="btn btn-primary"
              disabled={!canInstall}
              title={
                !status.versionSupported
                  ? "版本未适配"
                  : status.sandConflict
                    ? "先卸载 Sand"
                    : status.complete
                      ? "已是目标状态，点一下会重启 Cursor"
                      : undefined
              }
              onClick={() => void run(() => crsr.install(relaunch))}
            >
              {busy ? <Spinner /> : <Icon name="crsr" size={14} />}
              {status.complete ? "重新安装" : "安装"}
            </button>
          </div>
        </>
      )}

      {showBackups ? (
        <Modal title="CRSR 备份" onClose={() => setShowBackups(false)}>
          {backups.length === 0 ? (
            <Empty title="还没有备份" />
          ) : (
            <ul className="stack gap-sm">
              {backups
                .slice()
                .reverse()
                .map((b) => (
                  <li key={b.id} className="row items-center gap-sm">
                    <div className="grow">
                      <div className="mono tiny">{b.id}</div>
                      <div className="faint tiny">
                        {b.operation} · Cursor {b.cursorVersion} · {b.files} 个文件 · {timeAgo(b.createdAt)} · {b.state}
                      </div>
                    </div>
                    <button
                      type="button"
                      className="btn btn-sm"
                      disabled={busy}
                      onClick={() => {
                        setShowBackups(false);
                        void run(() => crsr.restoreBackup(b.id, relaunch));
                      }}
                    >
                      还原
                    </button>
                    <button
                      type="button"
                      className="btn btn-sm btn-quiet btn-danger"
                      disabled={busy}
                      onClick={() => {
                        void (async () => {
                          await crsr.removeBackup(b.id);
                          setBackups(await crsr.backups());
                        })();
                      }}
                    >
                      删除
                    </button>
                  </li>
                ))}
            </ul>
          )}
        </Modal>
      ) : null}
    </div>
  );
}

function StatusCard({ status }: { status: CrsrStatus }) {
  const state = !status.versionSupported
    ? "未适配"
    : status.complete
      ? "已安装"
      : status.installed
        ? "不完整"
        : "未安装";
  const dot = status.complete ? "is-ok" : status.installed ? "is-warn" : "is-off";
  return (
    <div className={`card sandcard ${status.complete ? "is-live" : ""}`}>
      <div className="sandcard-head">
        <span className={`sand-dot ${dot}`} />
        <strong className="sandcard-state">{state}</strong>
        <span className="sandcard-n">
          挂点 {status.hits} / {status.expectedHits}
        </span>
      </div>
      <div className="sandcard-meta">
        <span className="mono">Cursor {status.cursorVersion ?? "未知"}</span>
        {status.versionSupported ? (
          <Tag tone="ok">已适配</Tag>
        ) : (
          <Tag tone="warn">适配 {status.supportedVersion}</Tag>
        )}
        {status.patchedFiles.length > 0 ? <span>已改 {status.patchedFiles.length} 个文件</span> : null}
        {status.anchors > 0 && !status.complete ? <span>锚点 {status.anchors} 处</span> : null}
      </div>
      <p className="sect-none" style={{ marginTop: 10 }}>
        只换 Bearer，不改 client-type、不改路由。额度走普通 ide / cli 桶，不是 Bot。
      </p>
    </div>
  );
}

function CurrentAccount({ status, onGoAccounts }: { status: CrsrStatus; onGoAccounts: () => void }) {
  const cred = status.credential;
  return (
    <div className="card" style={{ marginTop: 16 }}>
      <div className="usedin">
        <div className={`usedin-row${cred && !cred.expired ? " is-in is-live" : ""}`}>
          <span className="usedin-ico">
            <Icon name="crsr" size={13} />
          </span>
          <span className="usedin-k">当前号</span>
          <span className="usedin-v">
            {cred
              ? cred.expired
                ? `${cred.accountEmail ?? "未知"} · 下一发自动续`
                : `${cred.accountEmail ?? "未知"} · ${timeUntil(cred.expiresAtMs)}续`
              : "到账号里选用"}
          </span>
          <button type="button" className="btn btn-sm" onClick={onGoAccounts}>
            去账号
          </button>
        </div>
      </div>
    </div>
  );
}

function OutcomeBanner({
  outcome,
  needsAccount,
  onGoAccounts,
}: {
  outcome: CrsrOutcome;
  needsAccount: boolean;
  onGoAccounts: () => void;
}) {
  const verb =
    outcome.operation === "install" ? "安装" : outcome.operation === "uninstall" ? "卸载" : "还原";
  if (!outcome.wrote) {
    return (
      <div style={{ marginTop: 12 }}>
        <Banner
          tone="ok"
          title={`${verb}：已经是目标状态，Cursor 没有被改动。`}
          hint={outcome.cursorRelaunched ? "已重启 Cursor。" : undefined}
        />
      </div>
    );
  }
  return (
    <div style={{ marginTop: 12 }}>
      <Banner
        tone="ok"
        title={`${verb}完成，写了 ${outcome.filesWritten} 个文件。`}
        hint={
          outcome.operation === "install" && needsAccount
            ? "到账号里点「用作 CRSR 通道」，下一发就走那个号。不必重装。"
            : outcome.cursorRelaunched
              ? "已重启 Cursor，让它重新加载补丁。"
              : "下次启动 Cursor 才会用上这份补丁。"
        }
        action={
          outcome.operation === "install" && needsAccount ? (
            <button type="button" className="btn btn-sm" onClick={onGoAccounts}>
              去账号
            </button>
          ) : undefined
        }
      />
    </div>
  );
}
