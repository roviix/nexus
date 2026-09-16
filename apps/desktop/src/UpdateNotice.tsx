import { relaunch } from "@tauri-apps/plugin-process";
import { check, type DownloadEvent, type Update } from "@tauri-apps/plugin-updater";
import { useEffect, useState } from "react";

type UpdateStage = "ready" | "downloading" | "installing" | "failed";

let startupCheck: Promise<Update | null> | null = null;

function checkOnce(): Promise<Update | null> {
  if (!startupCheck) {
    startupCheck = check({ timeout: 15_000 }).catch(() => null);
  }
  return startupCheck;
}

export function UpdateNotice() {
  const [update, setUpdate] = useState<Update | null>(null);
  const [stage, setStage] = useState<UpdateStage>("ready");
  const [downloadedBytes, setDownloadedBytes] = useState(0);
  const [contentLength, setContentLength] = useState(0);
  const [failure, setFailure] = useState("");
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    if (import.meta.env.DEV) return;
    const timer = window.setTimeout(() => {
      void checkOnce().then(setUpdate);
    }, 2_500);
    return () => window.clearTimeout(timer);
  }, []);

  if (!update || dismissed) return null;

  const progress =
    contentLength > 0
      ? Math.min(100, Math.round((downloadedBytes / contentLength) * 100))
      : null;

  async function installUpdate() {
    if (!update) return;
    const availableUpdate = update;
    setFailure("");
    setDownloadedBytes(0);
    setContentLength(0);
    setStage("downloading");

    try {
      await availableUpdate.downloadAndInstall((event: DownloadEvent) => {
        if (event.event === "Started") {
          setContentLength(event.data.contentLength ?? 0);
          return;
        }
        if (event.event === "Progress") {
          setDownloadedBytes((current) => current + event.data.chunkLength);
          return;
        }
        setStage("installing");
      });
      await relaunch();
    } catch (error) {
      setFailure(error instanceof Error ? error.message : "更新失败，请稍后重试。");
      setStage("failed");
    }
  }

  return (
    <section className="update-notice" aria-live="polite" aria-label="Nexus 更新">
      <div className="update-mark" aria-hidden>
        ↓
      </div>
      <div className="update-copy">
        <div className="update-title-row">
          <strong>发现新版本 v{update.version}</strong>
          <span className="tag tag-ok">来源已校验</span>
        </div>
        {stage === "ready" ? (
          <p>{update.body?.trim() || "包含功能改进与稳定性修复。"}</p>
        ) : null}
        {stage === "downloading" ? (
          <div>
            <p>{progress === null ? "正在下载安装包…" : `正在下载… ${progress}%`}</p>
            <div className="update-progress" aria-hidden>
              <span style={{ width: `${progress ?? 12}%` }} />
            </div>
          </div>
        ) : null}
        {stage === "installing" ? <p>下载完成，正在安装并重新启动…</p> : null}
        {stage === "failed" ? <p className="update-error">{failure}</p> : null}
      </div>
      <div className="update-actions">
        {stage === "ready" || stage === "failed" ? (
          <button type="button" className="btn btn-sm btn-primary" onClick={() => void installUpdate()}>
            {stage === "failed" ? "重试" : "立即更新"}
          </button>
        ) : null}
        {stage !== "installing" ? (
          <button
            type="button"
            className="btn btn-sm btn-ghost"
            disabled={stage === "downloading"}
            onClick={() => setDismissed(true)}
          >
            稍后
          </button>
        ) : null}
      </div>
    </section>
  );
}
