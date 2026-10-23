import { useCallback, useEffect, useState } from "react";
import { gateway as gatewayApi } from "../ipc/api";
import type { GatewayStatus, UsageSummary } from "../ipc/types";
import { compactNumber } from "../ui/traffic";
import { Modal, Switch } from "../ui/primitives";
import { go, type Route } from "./nav";

export function GatewayPod({
  onGo,
}: {
  onGo: (r: Route) => void;
  onOpenActivity?: () => void;
}) {
  const [status, setStatus] = useState<GatewayStatus | null>(null);
  const [usage, setUsage] = useState<UsageSummary | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const s = await gatewayApi.status();
      setStatus(s);
      if (s.running) {
        const u = await gatewayApi.usage(1);
        setUsage(u);
      }
    } catch {}
  }, []);

  useEffect(() => {
    void load();
    const interval = setInterval(load, 5000);
    return () => clearInterval(interval);
  }, [load]);

  const running = Boolean(status?.running);
  const port = status?.running ? status.running.addr.split(":").pop() : status?.settings.port ?? 8787;
  const todayTokens = usage?.today ? (usage.today.inputTokens + usage.today.outputTokens + usage.today.cacheReadTokens) : 0;

  async function handleToggle(next: boolean) {
    setBusy(true);
    try {
      if (next) {
        await gatewayApi.start();
      } else {
        await gatewayApi.stop();
      }
      await load();
    } catch {} finally {
      setBusy(false);
    }
  }

  return (
    <div className="gateway-pod-wrap">
      <div className={`gateway-stream-pod${running ? " is-running" : " is-stopped"}`}>
        {/* 上半层：网关状态，点击直达概览 */}
        <div
          className="gsp-top-tier"
          onClick={() => onGo(go("overview"))}
          role="button"
          tabIndex={0}
          title="点击直达系统概览与网关面板"
        >
          <div className="gsp-top-left">
            <div className="gsp-title-row">
              <span className={`pod-dot${running ? " is-live" : ""}`} />
              <span className="gsp-name">本地网关</span>
            </div>
            <div className="gsp-sub-row mono">
              {running ? `127.0.0.1:${port}` : `已停用 · :${port}`}
            </div>
          </div>
          <div
            className="gateway-pod-switch"
            onClick={(e) => e.stopPropagation()}
          >
            <Switch
              checked={running}
              disabled={busy}
              onChange={handleToggle}
              label={running ? "关闭本地网关" : "启动本地网关"}
            />
          </div>
        </div>

        {/* 1px 微分割线 */}
        <div className="gsp-divider" />

        {/* 下半层：实时请求流入口，直接直达概览 */}
        <div
          className="gsp-bottom-tier"
          onClick={() => onGo(go("overview"))}
          role="button"
          tabIndex={0}
          title="点击前往概览与最近请求记录"
        >
          <div className="gsp-bottom-left">
            <span className="gsp-stream-icon">⚡</span>
            <span className="gsp-stream-label">实时请求流</span>
            {running ? <span className="gsp-stream-pulse" /> : null}
          </div>
          <div className="gsp-bottom-right">
            <span className="gsp-metric-pill">
              今日 <span className="mono">{todayTokens > 0 ? compactNumber(todayTokens) : "0"}</span> tok
            </span>
            <span className="gsp-arrow">›</span>
          </div>
        </div>
      </div>
    </div>
  );
}

export function GatewayInspectorModal({
  status,
  onClose,
  onReload,
}: {
  status: GatewayStatus | null;
  usage?: UsageSummary | null;
  onClose: () => void;
  onReload: () => Promise<void>;
  onGo?: (r: Route) => void;
}) {
  const [portInput, setPortInput] = useState<string>(String(status?.settings.port ?? 8787));
  const [autostart, setAutostart] = useState<boolean>(status?.settings.autostart ?? false);
  const [revealedKey, setRevealedKey] = useState<string | null>(null);
  const [keyCopied, setKeyCopied] = useState(false);
  const [savingSettings, setSavingSettings] = useState(false);
  const [settingNotice, setSettingNotice] = useState<string | null>(null);

  const running = Boolean(status?.running);
  const currentAddr = status?.running ? status.running.addr : `127.0.0.1:${status?.settings.port ?? 8787}`;

  async function handleRevealKey() {
    try {
      const key = await gatewayApi.revealKey();
      setRevealedKey(key);
    } catch {}
  }

  async function handleCopyKey() {
    try {
      const key = revealedKey ?? (await gatewayApi.revealKey());
      setRevealedKey(key);
      await navigator.clipboard.writeText(key);
      setKeyCopied(true);
      setTimeout(() => setKeyCopied(false), 2000);
    } catch {}
  }

  async function handleSaveSettings() {
    const p = parseInt(portInput.trim(), 10);
    if (isNaN(p) || p < 1024 || p > 65535) {
      setSettingNotice("端口必须在 1024 到 65535 之间");
      return;
    }
    setSavingSettings(true);
    setSettingNotice(null);
    try {
      await gatewayApi.updateSettings({
        port: p,
        autostart,
      });
      await onReload();
      setSettingNotice("设置已更新（若端口已变更，重启网关后生效）");
    } catch (e: any) {
      setSettingNotice(`更新失败: ${e?.message ?? e}`);
    } finally {
      setSavingSettings(false);
    }
  }

  return (
    <Modal onClose={onClose} title="本地网关设置">
      <div className="gateway-inspector">
        {/* 顶部运行状态徽标 */}
        <div className="ginsp-hero">
          <div className="ginsp-hero-left">
            <span className={`pod-dot${running ? " is-live" : ""}`} />
            <div>
              <div className="ginsp-status-title">
                {running ? "网关运行中 (127.0.0.1 守护进程)" : "网关已停止"}
              </div>
              <div className="ginsp-status-sub mono">{currentAddr}</div>
            </div>
          </div>
          <div className="ginsp-hero-right">
            <span className="pill pill-session tiny">
              {running ? "TCP 监听活跃" : "空闲未启动"}
            </span>
          </div>
        </div>

        {/* 端口与口令配置 */}
        <div className="ginsp-section">
          <h3 className="ginsp-section-title">网络与访问口令</h3>
          <div className="ginsp-form-row">
            <label className="ginsp-label">监听端口</label>
            <div className="ginsp-control-group">
              <input
                type="number"
                className="input input-sm mono"
                style={{ width: 100 }}
                value={portInput}
                onChange={(e) => setPortInput(e.target.value)}
                min={1024}
                max={65535}
              />
              <label className="ginsp-check-label">
                <input
                  type="checkbox"
                  checked={autostart}
                  onChange={(e) => setAutostart(e.target.checked)}
                />
                随应用自动启动
              </label>
            </div>
          </div>

          <div className="ginsp-form-row">
            <label className="ginsp-label">访问口令 (API Key)</label>
            <div className="ginsp-key-row">
              <span className="mono ginsp-key-text">
                {revealedKey ? revealedKey : "••••••••••••••••••••••••"}
              </span>
              <div className="row" style={{ gap: 6 }}>
                {!revealedKey ? (
                  <button type="button" className="btn btn-sm btn-soft" onClick={handleRevealKey}>
                    显示
                  </button>
                ) : null}
                <button type="button" className="btn btn-sm" onClick={handleCopyKey}>
                  {keyCopied ? "✓ 已复制" : "复制"}
                </button>
              </div>
            </div>
          </div>

          {settingNotice ? (
            <div className="ginsp-notice">{settingNotice}</div>
          ) : null}
        </div>

        {/* 底部操作栏 */}
        <div className="ginsp-foot" style={{ marginTop: 16 }}>
          <button type="button" className="btn btn-sm btn-quiet" onClick={onClose}>
            关闭
          </button>
          <button
            type="button"
            className="btn btn-sm btn-primary"
            disabled={savingSettings}
            onClick={handleSaveSettings}
          >
            {savingSettings ? "保存中..." : "保存"}
          </button>
        </div>
      </div>
    </Modal>
  );
}
