/**
 * 本地备份：`~/.roviix/backups` 下的整库快照。
 *
 * 一份快照就是一个完整的 `nexus.db`：账号、凭证、切号本、登录态备份、设置全在里面，
 * 拷到另一台机器上还原就是搬家。
 *
 * 两句话得让人看见：快照里有明文凭证（所以目录和文件只有本人可读）—— 这句钉在标题下；
 * 还原会重启应用（库是在同一个连接里整体换掉的，网关名单这类模块启动时读过一次就攥在内存里）
 * —— 这句在按下「还原」的那一刻说，不提前唠叨。
 */
import { relaunch } from "@tauri-apps/plugin-process";
import { useCallback, useEffect, useState } from "react";
import { backup, errorText } from "../../ipc/api";
import type { LocalBackup } from "../../ipc/types";
import { confirm } from "../../ui/confirm";
import { bytes, timeAgo } from "../../ui/format";
import { Banner, Empty, Icon, Tag } from "../../ui/primitives";

function when(b: LocalBackup): string {
  const t = Date.parse(b.createdAt);
  return Number.isFinite(t) ? new Date(t).toLocaleString("zh-CN", { hour12: false }) : b.fileName;
}

export function LocalBackups() {
  const [list, setList] = useState<LocalBackup[] | null>(null);
  /** 正在忙哪一件：`create`，或某个文件名。同一时刻只做一件，别让两个还原撞在一起。 */
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [made, setMade] = useState<LocalBackup | null>(null);

  const reload = useCallback(async () => {
    try {
      setList(await backup.list());
      setError(null);
    } catch (err) {
      setError(errorText(err));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function run(key: string, fn: () => Promise<void>) {
    setBusy(key);
    setError(null);
    try {
      await fn();
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(null);
    }
  }

  function create() {
    return run("create", async () => {
      setMade(await backup.create());
      await reload();
    });
  }

  async function restore(b: LocalBackup) {
    const ok = await confirm(
      `用 ${when(b)} 的备份覆盖当前的账号、凭证、切号本和设置。\n\n` +
        "当前状态会先自动另存一份，还原错了能回来。还原完成后应用会重启。",
      { title: "还原这份备份？", okLabel: "还原并重启", danger: true },
    );
    if (!ok) return;
    return run(b.fileName, async () => {
      await backup.restore(b.fileName);
      await relaunch();
    });
  }

  async function remove(b: LocalBackup) {
    if (!(await confirm(`删除 ${when(b)} 的备份？`, { okLabel: "删除", danger: true }))) return;
    return run(b.fileName, async () => {
      await backup.remove(b.fileName);
      await reload();
    });
  }

  return (
    <section className="set-sect">
      <header className="set-sect-head">
        <Icon name="database" size={14} className="set-sect-ico" />
        <h2>本地备份</h2>
        <span className="set-sect-acts">
          <button
            type="button"
            className="btn btn-sm btn-icon btn-soft"
            aria-label="打开备份目录"
            onClick={() => void backup.reveal()}
          >
            <Icon name="folder" size={13} />
          </button>
          <button type="button" className="btn btn-sm btn-primary" disabled={busy !== null} onClick={() => void create()}>
            <Icon name="archive" size={13} />
            {busy === "create" ? "备份中…" : "立即备份"}
          </button>
        </span>
      </header>
      <p className="set-sect-sub">
        整库快照，含明文凭证，仅本机用户可读 · <code className="mono">~/.roviix/backups</code>
      </p>

      {error ? (
        <div style={{ marginBottom: 10 }}>
          <Banner tone="bad" title="备份操作失败" hint={error} />
        </div>
      ) : null}
      {made ? (
        <div style={{ marginBottom: 10 }}>
          <Banner
            tone="ok"
            title={`已备份 ${made.fileName} · ${bytes(made.sizeBytes)}`}
            action={
              <span className="row" style={{ gap: 4 }}>
                <button
                  type="button"
                  className="btn btn-sm btn-icon btn-quiet"
                  aria-label="显示文件"
                  onClick={() => void backup.reveal(made.path)}
                >
                  <Icon name="external" size={13} />
                </button>
                <button type="button" className="btn btn-sm btn-quiet" onClick={() => setMade(null)}>
                  知道了
                </button>
              </span>
            }
          />
        </div>
      ) : null}

      {list === null ? (
        <div className="skeleton" style={{ height: 112, borderRadius: 12 }} />
      ) : list.length === 0 ? (
        <Empty>还没有备份</Empty>
      ) : (
        <div className="opts">
          {list.map((b) => (
            <div className="opt" key={b.fileName}>
              <span className="opt-ico">
                <Icon name="archive" size={15} />
              </span>
              <div className="opt-copy">
                <div className="opt-title">
                  <span className="selectable mono truncate">{when(b)}</span>
                  {b.reason === "pre-restore" ? <Tag tone="warn">还原前自动备份</Tag> : null}
                </div>
                <div className="opt-desc truncate">
                  {timeAgo(b.createdAt)} · {bytes(b.sizeBytes)} · <span className="mono selectable">{b.fileName}</span>
                </div>
              </div>
              <div className="opt-ctl">
                <span className="opt-acts">
                  <button
                    type="button"
                    className="btn btn-sm btn-icon btn-quiet"
                    aria-label="还原这份备份"
                    disabled={busy !== null}
                    onClick={() => void restore(b)}
                  >
                    <Icon name="undo" size={13} />
                  </button>
                  <button
                    type="button"
                    className="btn btn-sm btn-icon btn-quiet btn-danger"
                    aria-label="删除"
                    disabled={busy !== null}
                    onClick={() => void remove(b)}
                  >
                    <Icon name="trash" size={13} />
                  </button>
                </span>
              </div>
            </div>
          ))}
        </div>
      )}
    </section>
  );
}
