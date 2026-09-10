/**
 * 首次启动的「准备工作」：把会弹系统窗的权限一次问完，而不是等到装补丁到一半、切号切到一半。
 *
 * 只弹一次（`perms.preflight_done`），跳过也算走完 —— 以后在设置页里随时能再申请。弹窗先说清
 * 我们要动什么、为什么，用户按下「一次申请」之后系统窗才会跟着出来：人知道自己在批什么。
 * 没有任何一项需要申请（Windows / Linux，或 macOS 上都已允许过）就不弹，直接标记完成。
 */
import { useEffect, useMemo, useState } from "react";
import { perms as permsApi } from "../../ipc/api";
import { Modal } from "../../ui/primitives";
import { PermissionList, pendingItems, usePermissions } from "./Permissions";

export function PreflightModal() {
  const { report, busy, error, request, openSettings } = usePermissions();
  const [dismissed, setDismissed] = useState(false);
  const [asked, setAsked] = useState(false);

  const pending = useMemo(() => pendingItems(report), [report]);
  const needed = report != null && !report.preflightDone && !dismissed;

  // 没什么可问的就静默标记完成，不打扰人。
  useEffect(() => {
    if (report && !report.preflightDone && pending.length === 0 && !asked) {
      void permsApi.markPreflight();
      setDismissed(true);
    }
  }, [report, pending.length, asked]);

  if (!needed || (pending.length === 0 && !asked)) return null;

  async function finish() {
    await permsApi.markPreflight().catch(() => {});
    setDismissed(true);
  }

  const allSettled = pending.length === 0;

  return (
    <Modal
      title="准备工作"
      subtitle="会弹系统窗的几项权限，现在一次问完，免得在切号、装补丁的半路上被打断。"
      onClose={() => void finish()}
      footer={
        <>
          <button type="button" className="btn btn-quiet" onClick={() => void finish()}>
            {allSettled ? "完成" : "以后再说"}
          </button>
          {!allSettled ? (
            <button
              type="button"
              className="btn btn-primary"
              disabled={busy != null}
              onClick={() => {
                setAsked(true);
                void request();
              }}
            >
              {busy ? "申请中…" : asked ? "再申请一次" : "一次申请"}
            </button>
          ) : (
            <button type="button" className="btn btn-primary" onClick={() => void finish()}>
              开始使用
            </button>
          )}
        </>
      }
    >
      <PermissionList report={report} busy={busy} error={error} flush onRequest={(id) => void request(id)} onOpenSettings={(id) => void openSettings(id)} />
      <p className="subtitle" style={{ marginTop: 12 }}>以后随时可以在「设置 → 权限」里再申请。</p>
    </Modal>
  );
}
