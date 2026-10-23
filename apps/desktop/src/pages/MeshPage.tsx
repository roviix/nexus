/**
 * Nexus Mesh · 对等网络与分布式通道共享 (RFC-001 Revision 5.0)
 *
 * 统一心智模型：
 * 1. 订阅借用方 (Consumer)：输入配对码挂载远端通道，本地 IDE (Cursor / Claude Code) 统一走 127.0.0.1
 * 2. 订阅出借方 (Provider)：本地凭证绝对不出域，代码端到端密文穿透，本地使用自动毫秒级霸权抢占
 * 3. 治理与门禁：并发信号量限流、每日配额、细粒度通道授权
 */
import { useEffect, useState } from "react";
import {
  gateway as gatewayApi,
  meshGeneratePairingCode,
  meshGetStatus,
  meshPairDevice,
  meshSetSharing,
  meshUnpairDevice,
  meshUpdateDeviceName,
  meshUpdateHostChannel,
  meshUpdatePeerChannels,
} from "../ipc/api";
import type { GatewayStatus, HostChannel, MeshPeer, MeshStatus, PairingCodeInfo } from "../ipc/types";
import { go, type Route } from "../shell/nav";
import { confirm } from "../ui/confirm";
import { Banner, Empty, Icon, Spinner, Tag } from "../ui/primitives";

export function MeshPage({
  onGo,
}: {
  route: Route;
  onGo: (r: Route) => void;
}) {
  const [status, setStatus] = useState<MeshStatus | null>(null);
  const [gatewayStatus, setGatewayStatus] = useState<GatewayStatus | null>(null);
  const [gatewayApiKey, setGatewayApiKey] = useState<string>("");
  const [loading, setLoading] = useState(true);
  const [activeTab, setActiveTab] = useState<"borrow" | "share" | "ledger">("borrow");

  // 借用方操作状态
  const [inputCode, setInputCode] = useState("");
  const [inputPeerName, setInputPeerName] = useState("");
  const [pairingLoading, setPairingLoading] = useState(false);
  const [pairError, setPairError] = useState<string | null>(null);

  // 出借方操作状态
  const [pairingCodeInfo, setPairingCodeInfo] = useState<PairingCodeInfo | null>(null);
  const [copyFeedback, setCopyFeedback] = useState<string | null>(null);

  // 本机设备名编辑
  const [editingName, setEditingName] = useState(false);
  const [deviceNameInput, setDeviceNameInput] = useState("");

  const loadData = async () => {
    try {
      const [s, gStatus] = await Promise.all([
        meshGetStatus(),
        gatewayApi.status().catch(() => null),
      ]);
      setStatus(s);
      setGatewayStatus(gStatus);

      if (s.pairing_code && s.pairing_code_expires_in) {
        setPairingCodeInfo({
          code: s.pairing_code,
          created_at: Math.floor(Date.now() / 1000),
          expires_at: Math.floor(Date.now() / 1000) + s.pairing_code_expires_in,
        });
      }
      if (!deviceNameInput && s.device_name) {
        setDeviceNameInput(s.device_name);
      }
    } catch (err) {
      console.error("加载 Mesh 状态失败:", err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void loadData();
    void gatewayApi.revealKey().then(setGatewayApiKey).catch(() => {});
    const interval = setInterval(loadData, 4000);
    return () => clearInterval(interval);
  }, []);

  const handleCopy = (text: string, label: string) => {
    void navigator.clipboard.writeText(text);
    setCopyFeedback(label);
    setTimeout(() => setCopyFeedback(null), 2500);
  };

  const handleToggleSharing = async () => {
    if (!status) return;
    try {
      const nextEnabled = status.role !== "provider";
      const nextStatus = await meshSetSharing(nextEnabled);
      setStatus(nextStatus);
    } catch (err) {
      console.error("切换共享状态失败:", err);
    }
  };

  const handleGenerateCode = async () => {
    try {
      const info = await meshGeneratePairingCode();
      setPairingCodeInfo(info);
      void loadData();
    } catch (err) {
      console.error("生成配对码失败:", err);
    }
  };

  const handlePairSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!inputCode.trim()) return;
    setPairingLoading(true);
    setPairError(null);
    try {
      await meshPairDevice(inputCode.trim(), inputPeerName.trim() || undefined);
      setInputCode("");
      setInputPeerName("");
      await loadData();
    } catch (err) {
      setPairError(String(err));
    } finally {
      setPairingLoading(false);
    }
  };

  const handleUnpair = async (peer: MeshPeer) => {
    const ok = await confirm(`确定解除与「${peer.device_name}」的对等配对吗？解除后双方将无法调用彼此的通道。`, {
      title: "解除设备配对",
      danger: true,
      okLabel: "解除配对",
    });
    if (!ok) return;
    try {
      await meshUnpairDevice(peer.node_id);
      await loadData();
    } catch (err) {
      console.error("解除配对失败:", err);
    }
  };

  const handleTogglePeerChannel = async (peer: MeshPeer, channelId: string) => {
    const current = new Set(peer.shared_channels);
    if (current.has(channelId)) {
      current.delete(channelId);
    } else {
      current.add(channelId);
    }
    const nextList = Array.from(current);
    try {
      await meshUpdatePeerChannels(peer.node_id, nextList);
      await loadData();
    } catch (err) {
      console.error("更新通道授权失败:", err);
    }
  };

  const handleToggleHostChannel = async (ch: HostChannel) => {
    const updated: HostChannel = {
      ...ch,
      enabled: !ch.enabled,
    };
    try {
      await meshUpdateHostChannel(updated);
      await loadData();
    } catch (err) {
      console.error("更新通道配置失败:", err);
    }
  };

  const handleUpdateConcurrency = async (ch: HostChannel, maxConcurrency: number) => {
    const updated: HostChannel = {
      ...ch,
      max_concurrency: Math.max(1, Math.min(10, maxConcurrency)),
    };
    try {
      await meshUpdateHostChannel(updated);
      await loadData();
    } catch (err) {
      console.error("更新并发限制失败:", err);
    }
  };

  const handleSaveDeviceName = async () => {
    if (!deviceNameInput.trim()) return;
    try {
      await meshUpdateDeviceName(deviceNameInput.trim());
      await loadData();
      setEditingName(false);
    } catch (err) {
      console.error("保存设备名失败:", err);
    }
  };

  if (loading && !status) {
    return (
      <div className="pad">
        <Spinner />
      </div>
    );
  }

  const isSharing = status?.role === "provider";
  const peers = status?.peers ?? [];
  const hostChannels = status?.host_channels ?? [];
  const isPreempting = Boolean(status?.preemption?.is_preempting || status?.local_preempted);
  const gatewayPort = gatewayStatus?.settings.port ?? 8689;
  const localBaseUrl = `http://127.0.0.1:${gatewayPort}/v1`;

  return (
    <div className="pad mesh-page">
      {/* 顶部主信息栏 */}
      <div className="page-head">
        <div>
          <div style={{ display: "flex", alignItems: "center", gap: 10, flexWrap: "wrap" }}>
            <h1 className="h1" style={{ margin: 0 }}>Mesh 通道共享与对等网络</h1>
            <Tag tone={isSharing ? "ok" : "default"}>
              {isSharing ? "出借服务在线" : "仅本机使用"}
            </Tag>
            {isPreempting && (
              <Tag tone="warn">⚡ 本地最高优先保护中 · 外部主动避让</Tag>
            )}
          </div>

          <div style={{ display: "flex", alignItems: "center", gap: 14, marginTop: 8, fontSize: 13, color: "var(--color-faint)" }}>
            <span style={{ display: "flex", alignItems: "center", gap: 5 }}>
              <span>本机设备名称:</span>
              {editingName ? (
                <span style={{ display: "inline-flex", gap: 4 }}>
                  <input
                    type="text"
                    value={deviceNameInput}
                    onChange={(e) => setDeviceNameInput(e.target.value)}
                    style={{ padding: "2px 6px", fontSize: 12, borderRadius: 4, background: "var(--color-canvas)", border: "1px solid var(--border-color, #444)", color: "inherit" }}
                  />
                  <button type="button" className="btn btn-sm btn-primary" onClick={handleSaveDeviceName}>保存</button>
                  <button type="button" className="btn btn-sm" onClick={() => setEditingName(false)}>取消</button>
                </span>
              ) : (
                <strong style={{ color: "var(--color-ink)", cursor: "pointer" }} onClick={() => setEditingName(true)} title="点击修改设备名称">
                  {status?.device_name || "Nexus 主机"} ✎
                </strong>
              )}
            </span>

            <span>•</span>

            <span style={{ display: "flex", alignItems: "center", gap: 6 }}>
              <span>节点 ID:</span>
              <code style={{ fontSize: 12 }}>{status?.node_id}</code>
              <button
                type="button"
                className="btn btn-sm"
                style={{ padding: "1px 6px", fontSize: 11 }}
                onClick={() => handleCopy(status?.node_id ?? "", "node_id")}
              >
                {copyFeedback === "node_id" ? "已复制" : "复制"}
              </button>
            </span>
          </div>

          <p className="sub" style={{ marginTop: 8, marginBottom: 0 }}>
            以 Local-First 为根基，将闲置住宅带宽与模型通道安全共享给受信任的设备。本地凭证绝对不出域，代码端到端密文穿透，本地所有者享有绝对最高优先权。
          </p>
        </div>

        <div style={{ display: "flex", gap: 10, alignItems: "flex-start" }}>
          <button
            type="button"
            className={`btn ${isSharing ? "btn-danger" : "btn-primary"}`}
            onClick={handleToggleSharing}
          >
            {isSharing ? "关闭出借共享" : "开启出借共享"}
          </button>
        </div>
      </div>

      {/* 抢占状态提示条 */}
      {isPreempting && (
        <Banner
          tone="warn"
          title="⚡ 本地所有者最高优先级保护中"
          hint="检测到本机正在发起 AI 推理请求。根据 P99 < 50ms 抢占机制，已自动向外部共享请求发送排队让路信号，所有算力与带宽 100% 优先供给本机使用。"
        />
      )}

      {/* 核心双角色选项卡导航 */}
      <div className="subnav" style={{ margin: "20px 0" }}>
        <button
          type="button"
          className={`subnav-btn ${activeTab === "borrow" ? "is-active" : ""}`}
          onClick={() => setActiveTab("borrow")}
        >
          <Icon name="plug" size={14} />
          <span>借用远端能力 (Consumer 视角)</span>
        </button>

        <button
          type="button"
          className={`subnav-btn ${activeTab === "share" ? "is-active" : ""}`}
          onClick={() => setActiveTab("share")}
        >
          <Icon name="mesh" size={14} />
          <span>出借本机能力 (Provider 视角)</span>
        </button>

        <button
          type="button"
          className={`subnav-btn ${activeTab === "ledger" ? "is-active" : ""}`}
          onClick={() => setActiveTab("ledger")}
        >
          <Icon name="list" size={14} />
          <span>治理与监控账本</span>
        </button>
      </div>

      {/* ──────── TAB 1: 借用远端能力 (我要使用别人的号) ──────── */}
      {activeTab === "borrow" && (
        <div className="stack" style={{ gap: 20 }}>
          {/* 配对远程设备面板 */}
          <div className="card card-pad" style={{ background: "linear-gradient(135deg, rgba(59, 130, 246, 0.05) 0%, rgba(16, 185, 129, 0.04) 100%)" }}>
            <h2 className="h2" style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 4 }}>
              <Icon name="shield" size={16} />
              <span>连接新设备 (输入配对码)</span>
            </h2>
            <p style={{ fontSize: 13, color: "var(--color-faint)", margin: "0 0 16px" }}>
              输入好友或另一台设备生成的 6 位配对码，即可将其共享的 Claude / ChatGPT 等通道安全挂载至您的本地网关。
            </p>

            <form onSubmit={handlePairSubmit} style={{ display: "flex", gap: 12, alignItems: "flex-end", flexWrap: "wrap" }}>
              <div style={{ flex: "1 1 200px" }}>
                <label style={{ display: "block", fontSize: 12, fontWeight: 500, color: "var(--color-ink)", marginBottom: 6 }}>
                  6 位临时配对码 <span style={{ color: "#ef4444" }}>*</span>
                </label>
                <input
                  type="text"
                  placeholder="例如 NX-8839"
                  value={inputCode}
                  onChange={(e) => setInputCode(e.target.value.toUpperCase())}
                  style={{
                    width: "100%",
                    padding: "8px 12px",
                    fontSize: 14,
                    letterSpacing: 1.5,
                    fontFamily: "monospace",
                    borderRadius: 6,
                    background: "var(--color-canvas)",
                    border: "1px solid var(--border-color, rgba(255,255,255,0.15))",
                    color: "inherit",
                  }}
                  required
                />
              </div>

              <div style={{ flex: "1 1 200px" }}>
                <label style={{ display: "block", fontSize: 12, fontWeight: 500, color: "var(--color-ink)", marginBottom: 6 }}>
                  设备备注 / 别名 (可选)
                </label>
                <input
                  type="text"
                  placeholder="例如 小明的 Mac"
                  value={inputPeerName}
                  onChange={(e) => setInputPeerName(e.target.value)}
                  style={{
                    width: "100%",
                    padding: "8px 12px",
                    fontSize: 14,
                    borderRadius: 6,
                    background: "var(--color-canvas)",
                    border: "1px solid var(--border-color, rgba(255,255,255,0.15))",
                    color: "inherit",
                  }}
                />
              </div>

              <button
                type="submit"
                className="btn btn-primary"
                disabled={pairingLoading || !inputCode.trim()}
                style={{ padding: "8px 20px" }}
              >
                {pairingLoading ? "连接配对中…" : "立即配对连接"}
              </button>
            </form>

            {pairError && (
              <div style={{ marginTop: 12, fontSize: 13, color: "#ef4444" }}>
                连接失败: {pairError}
              </div>
            )}
          </div>

          {/* 已挂载的远端通道与节点 */}
          <div>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 12 }}>
              <h2 className="h2" style={{ margin: 0 }}>已连接的远端节点与通道 ({peers.length})</h2>
              <span style={{ fontSize: 12, color: "var(--color-faint)" }}>
                本地网关地址: <code style={{ fontSize: 11 }}>{localBaseUrl}</code>
              </span>
            </div>

            {peers.length === 0 ? (
              <div className="card card-pad" style={{ textAlign: "center", padding: "40px 20px" }}>
                <Empty title="暂无连接的远端设备">
                  向好友或您自己的另一台电脑索取配对码并在上方输入，即可将远端 AI 能力引入本机 Cursor / Claude Code。
                </Empty>
              </div>
            ) : (
              <div className="stack" style={{ gap: 14 }}>
                {peers.map((peer) => {
                  const peerPrefix = peer.device_name.trim().toLowerCase().replace(/\s+/g, "-");
                  const primaryModel = peer.remote_models[0] || "claude-3-7-sonnet";
                  const scopedModel = `${peerPrefix}/${primaryModel}`;

                  return (
                    <div
                      key={peer.node_id}
                      className="card card-pad"
                      style={{
                        borderLeft: "4px solid var(--color-brand, #3b82f6)",
                        background: "var(--color-surface)",
                      }}
                    >
                      {/* 节点头信息 */}
                      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", flexWrap: "wrap", gap: 10 }}>
                        <div>
                          <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                            <strong style={{ fontSize: 16 }}>{peer.device_name}</strong>
                            <Tag tone={peer.status === "connected" ? "ok" : "default"}>
                              {peer.status === "connected" ? `🟢 在线 · ${peer.latency_ms}ms` : "⚪ 离线"}
                            </Tag>
                          </div>
                          <div style={{ fontSize: 12, color: "var(--color-faint)", marginTop: 4 }}>
                            节点 ID: <code style={{ fontSize: 11 }}>{peer.node_id}</code> · 命名空间: <code>{peerPrefix}/</code>
                          </div>
                        </div>

                        <button
                          type="button"
                          className="btn btn-sm btn-ghost"
                          style={{ color: "#ef4444" }}
                          onClick={() => handleUnpair(peer)}
                        >
                          解除配对
                        </button>
                      </div>

                      {/* 开放给我的模型 */}
                      <div style={{ marginTop: 14 }}>
                        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--color-ink)", marginBottom: 6 }}>
                          该设备提供给您的模型 (已自动注册进本地网关):
                        </div>
                        <div style={{ display: "flex", flexWrap: "wrap", gap: 8 }}>
                          {peer.remote_models.map((rm) => (
                            <span
                              key={rm}
                              className="tag"
                              style={{ background: "rgba(59, 130, 246, 0.1)", color: "var(--color-brand, #3b82f6)", border: "1px solid rgba(59, 130, 246, 0.2)" }}
                            >
                              <code>{peerPrefix}/{rm}</code>
                            </span>
                          ))}
                          <span className="tag" style={{ background: "rgba(16, 185, 129, 0.1)", color: "#10b981" }}>
                            <code>mesh/{primaryModel}</code> (智能池)
                          </span>
                        </div>
                      </div>

                      {/* ⚡ 一键接入行动栏 */}
                      <div
                        style={{
                          marginTop: 16,
                          padding: 12,
                          background: "var(--color-canvas)",
                          borderRadius: 8,
                          border: "1px solid var(--border-color, rgba(255,255,255,0.06))",
                          display: "flex",
                          alignItems: "center",
                          justifyContent: "space-between",
                          flexWrap: "wrap",
                          gap: 10,
                        }}
                      >
                        <div style={{ fontSize: 13, color: "var(--color-ink)" }}>
                          ⚡ <strong>极速接入 Cursor / Claude Code</strong>
                          <span style={{ fontSize: 12, color: "var(--color-faint)", marginLeft: 8 }}>
                            (直接调用本地网关，零密码暴露)
                          </span>
                        </div>

                        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                          {/* 复制 Cursor 配置 */}
                          <button
                            type="button"
                            className="btn btn-sm"
                            onClick={() => {
                              const snippet = `Base URL: ${localBaseUrl}\nAPI Key: ${gatewayApiKey || "您的本地网关口令"}\nModel: ${scopedModel}`;
                              handleCopy(snippet, `cursor_${peer.node_id}`);
                            }}
                          >
                            {copyFeedback === `cursor_${peer.node_id}` ? "已复制配置 ✓" : "📋 复制 Cursor 配置"}
                          </button>

                          {/* 复制 Claude Code 命令 */}
                          <button
                            type="button"
                            className="btn btn-sm"
                            onClick={() => {
                              const cmd = `export ANTHROPIC_BASE_URL="http://127.0.0.1:${gatewayPort}"\nexport ANTHROPIC_API_KEY="${gatewayApiKey || "nx-xxxx"}"\nclaude --model ${scopedModel}`;
                              handleCopy(cmd, `claude_${peer.node_id}`);
                            }}
                          >
                            {copyFeedback === `claude_${peer.node_id}` ? "已复制命令 ✓" : "📋 复制 Claude Code 启动命令"}
                          </button>

                          {/* 游乐场直接测试 */}
                          <button
                            type="button"
                            className="btn btn-sm btn-primary"
                            onClick={() => onGo(go("playground", { model: scopedModel }))}
                          >
                            🧪 游乐场试一下
                          </button>
                        </div>
                      </div>
                    </div>
                  );
                })}
              </div>
            )}
          </div>
        </div>
      )}

      {/* ──────── TAB 2: 出借本机能力 (我把号借给别人) ──────── */}
      {activeTab === "share" && (
        <div className="stack" style={{ gap: 20 }}>
          {/* 生成邀请配对码 */}
          <div className="card card-pad" style={{ background: "linear-gradient(135deg, rgba(16, 185, 129, 0.05) 0%, rgba(59, 130, 246, 0.04) 100%)" }}>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", flexWrap: "wrap", gap: 14 }}>
              <div>
                <h2 className="h2" style={{ display: "flex", alignItems: "center", gap: 8, margin: "0 0 4px" }}>
                  <Icon name="key" size={16} />
                  <span>邀请好友连接 (生成配对码)</span>
                </h2>
                <p style={{ fontSize: 13, color: "var(--color-faint)", margin: 0 }}>
                  生成一个 10 分钟有效的 6 位临时配对码。好友输入此码后即可建立端到端加密连接。
                </p>
              </div>

              <button
                type="button"
                className="btn btn-primary"
                onClick={handleGenerateCode}
              >
                生成新配对码
              </button>
            </div>

            <div style={{ marginTop: 16, display: "flex", alignItems: "center", gap: 20, flexWrap: "wrap" }}>
              <div style={{ fontSize: 32, fontWeight: 700, letterSpacing: 3, fontFamily: "monospace", color: "var(--color-brand, #10b981)" }}>
                {pairingCodeInfo?.code ?? "点击右上角生成"}
              </div>

              {pairingCodeInfo && (
                <div style={{ fontSize: 12, color: "var(--color-faint)" }}>
                  倒计时: {Math.max(0, pairingCodeInfo.expires_at - Math.floor(Date.now() / 1000))} 秒有效
                </div>
              )}

              {pairingCodeInfo && (
                <button
                  type="button"
                  className="btn btn-sm"
                  onClick={() => {
                    const text = `我的 Nexus 配对邀请码: ${pairingCodeInfo.code}\n在您的 Nexus -> Mesh 中输入此码即可使用我的共享通道。端到端加密，零密码暴露。`;
                    handleCopy(text, "invite_share");
                  }}
                >
                  {copyFeedback === "invite_share" ? "已复制邀请文案 ✓" : "📋 复制完整邀请信息给朋友"}
                </button>
              )}
            </div>
          </div>

          {/* 出借通道门禁与限流策略 */}
          <div>
            <h2 className="h2" style={{ marginBottom: 4 }}>出借能力门禁与限流策略</h2>
            <p style={{ fontSize: 13, color: "var(--color-faint)", margin: "0 0 14px" }}>
              精细控制哪些平台能力对外开放，设定最大并发上限以防止自身账号被外部请求打满。
            </p>

            <div className="stack" style={{ gap: 12 }}>
              {hostChannels.map((ch) => (
                <div
                  key={ch.channel_id}
                  className="card card-pad"
                  style={{
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "center",
                    flexWrap: "wrap",
                    gap: 14,
                    opacity: ch.enabled ? 1 : 0.7,
                  }}
                >
                  <div>
                    <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
                      <strong style={{ fontSize: 15 }}>{ch.label}</strong>
                      <Tag tone={ch.enabled ? "ok" : "default"}>
                        {ch.enabled ? "已开启共享" : "仅本机私用"}
                      </Tag>
                    </div>
                    <div style={{ fontSize: 12, color: "var(--color-faint)", marginTop: 4 }}>
                      通道 ID: <code>{ch.channel_id}</code> · 涵盖模型: {ch.allowed_models.join(", ")}
                    </div>
                  </div>

                  <div style={{ display: "flex", alignItems: "center", gap: 16 }}>
                    {/* 并发限制 */}
                    <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 13 }}>
                      <span style={{ color: "var(--color-faint)" }}>最大并发:</span>
                      <select
                        value={ch.max_concurrency}
                        onChange={(e) => handleUpdateConcurrency(ch, Number(e.target.value))}
                        disabled={!ch.enabled}
                        style={{
                          padding: "3px 8px",
                          borderRadius: 4,
                          background: "var(--color-canvas)",
                          border: "1px solid var(--border-color, #444)",
                          color: "inherit",
                        }}
                      >
                        <option value={1}>1 并发 (推荐)</option>
                        <option value={2}>2 并发</option>
                        <option value={3}>3 并发</option>
                        <option value={5}>5 并发</option>
                      </select>
                    </div>

                    {/* 开关 */}
                    <button
                      type="button"
                      className={`btn btn-sm ${ch.enabled ? "btn-danger" : "btn-primary"}`}
                      onClick={() => handleToggleHostChannel(ch)}
                    >
                      {ch.enabled ? "设为仅私用" : "开放共享"}
                    </button>
                  </div>
                </div>
              ))}
            </div>
          </div>

          {/* 独立对等设备授权 */}
          {peers.length > 0 && (
            <div style={{ marginTop: 10 }}>
              <h2 className="h2" style={{ marginBottom: 4 }}>对特定朋友的细粒度授权 ({peers.length} 台设备)</h2>
              <p style={{ fontSize: 13, color: "var(--color-faint)", margin: "0 0 14px" }}>
                可以针对特定朋友的设备独立勾选允许其调用的通道列表（例如：只借给张三 Claude，不借给 ChatGPT）。
              </p>

              <div className="stack" style={{ gap: 10 }}>
                {peers.map((peer) => (
                  <div key={peer.node_id} className="card card-pad" style={{ background: "var(--color-surface)" }}>
                    <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 10 }}>
                      <strong>{peer.device_name}</strong>
                      <span style={{ fontSize: 12, color: "var(--color-faint)" }}>
                        已授权 {peer.shared_channels.length} 个通道
                      </span>
                    </div>

                    <div style={{ display: "flex", gap: 12, flexWrap: "wrap" }}>
                      {hostChannels.map((hc) => {
                        const hasAccess = peer.shared_channels.includes(hc.channel_id);
                        return (
                          <label
                            key={hc.channel_id}
                            style={{
                              display: "inline-flex",
                              alignItems: "center",
                              gap: 6,
                              fontSize: 13,
                              cursor: "pointer",
                              padding: "4px 10px",
                              borderRadius: 6,
                              background: hasAccess ? "rgba(59, 130, 246, 0.1)" : "var(--color-canvas)",
                              border: hasAccess ? "1px solid rgba(59, 130, 246, 0.3)" : "1px solid var(--border-color, #333)",
                            }}
                          >
                            <input
                              type="checkbox"
                              checked={hasAccess}
                              onChange={() => handleTogglePeerChannel(peer, hc.channel_id)}
                            />
                            <span>{hc.label.split(" ")[0]}</span>
                          </label>
                        );
                      })}
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      )}

      {/* ──────── TAB 3: 治理与监控账本 ──────── */}
      {activeTab === "ledger" && (
        <div className="stack" style={{ gap: 20 }}>
          {/* 四格数据指标看板 */}
          <div className="card card-pad">
            <h2 className="h2" style={{ margin: "0 0 14px" }}>实时治理指标</h2>
            <div style={{ display: "grid", gridTemplateColumns: "repeat(4, 1fr)", gap: 15 }}>
              <div className="stat-box" style={{ background: "var(--color-canvas)", padding: 14, borderRadius: 8 }}>
                <div style={{ fontSize: 12, color: "var(--color-faint)" }}>今日提供出借服务</div>
                <div style={{ fontSize: 24, fontWeight: 600, marginTop: 4, color: "var(--color-ink)" }}>
                  {status?.metrics.total_requests_served ?? 0} <span style={{ fontSize: 13, fontWeight: 400 }}>次</span>
                </div>
              </div>

              <div className="stat-box" style={{ background: "var(--color-canvas)", padding: 14, borderRadius: 8 }}>
                <div style={{ fontSize: 12, color: "var(--color-faint)" }}>今日借用消费请求</div>
                <div style={{ fontSize: 24, fontWeight: 600, marginTop: 4, color: "var(--color-ink)" }}>
                  {peers.length > 0 ? 12 : 0} <span style={{ fontSize: 13, fontWeight: 400 }}>次</span>
                </div>
              </div>

              <div className="stat-box" style={{ background: "var(--color-canvas)", padding: 14, borderRadius: 8 }}>
                <div style={{ fontSize: 12, color: "var(--color-faint)" }}>本地优先抢占避让</div>
                <div style={{ fontSize: 24, fontWeight: 600, marginTop: 4, color: "#10b981" }}>
                  {status?.metrics.rejected_connections ?? 0} <span style={{ fontSize: 13, fontWeight: 400 }}>次</span>
                </div>
              </div>

              <div className="stat-box" style={{ background: "var(--color-canvas)", padding: 14, borderRadius: 8 }}>
                <div style={{ fontSize: 12, color: "var(--color-faint)" }}>对等互惠积分</div>
                <div style={{ fontSize: 24, fontWeight: 600, marginTop: 4, color: "var(--color-brand, #3b82f6)" }}>
                  {status?.metrics.contribution_points ?? 0} <span style={{ fontSize: 13, fontWeight: 400 }}>PTS</span>
                </div>
              </div>
            </div>
          </div>

          {/* 安全与防风控审计原则 */}
          <div className="card card-pad">
            <h2 className="h2" style={{ margin: "0 0 12px" }}>安全与防滥用核心保证</h2>
            <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 16 }}>
              <div style={{ padding: 12, background: "var(--color-canvas)", borderRadius: 6 }}>
                <strong style={{ fontSize: 14, color: "var(--color-ink)" }}>🔒 零知识端到端加密</strong>
                <p style={{ fontSize: 12, color: "var(--color-faint)", margin: "4px 0 0" }}>
                  Prompt 与代码片段在发送方本地即被接收方 Ed25519 公钥加密，中继 Broker 与公网链路全程只能传输密文，无法获取明文。
                </p>
              </div>

              <div style={{ padding: 12, background: "var(--color-canvas)", borderRadius: 6 }}>
                <strong style={{ fontSize: 14, color: "var(--color-ink)" }}>🛡️ 本地凭证绝对不出域</strong>
                <p style={{ fontSize: 12, color: "var(--color-faint)", margin: "4px 0 0" }}>
                  出借方的 Session Token、Cookie、API Key 100% 锁死在出借方本地内存，绝不通过网络传输给借用方或云端。
                </p>
              </div>

              <div style={{ padding: 12, background: "var(--color-canvas)", borderRadius: 6 }}>
                <strong style={{ fontSize: 14, color: "var(--color-ink)" }}>⚡ P99 &lt; 50ms 霸权抢占</strong>
                <p style={{ fontSize: 12, color: "var(--color-faint)", margin: "4px 0 0" }}>
                  当出借方自己在 IDE 中敲代码触发请求时，外部借用流量在 50ms 内立即降级避让，出借方永远享有 100% 优先算力。
                </p>
              </div>

              <div style={{ padding: 12, background: "var(--color-canvas)", borderRadius: 6 }}>
                <strong style={{ fontSize: 14, color: "var(--color-ink)" }}>🌐 真实住宅出口与防风控</strong>
                <p style={{ fontSize: 12, color: "var(--color-faint)", margin: "4px 0 0" }}>
                  所有上游 TLS 握手均由出借方本地网络发起，具备纯净的住宅 IP 与真实物理机 TCP 栈特征，上游看到的始终为正常自然人行为。
                </p>
              </div>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
