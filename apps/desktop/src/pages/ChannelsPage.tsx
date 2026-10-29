import { useEffect, useMemo, useState } from "react";
import {
  defaultChannelId,
  localChannels,
  modelsOf,
  usableCandidate,
  type LocalChannel,
} from "../gateway/channels";
import {
  deleteCustomChannel,
  loadCustomChannels,
  upsertCustomChannel,
  type ChannelStrategy,
  type CustomLogicalChannel,
} from "../gateway/customChannels";
import {
  accounts as accountsApi,
  gateway as gatewayApi,
  meshGeneratePairingCode,
  meshGetStatus,
  meshPairDevice,
  meshSetSharing,
  meshUnpairDevice,
  meshUpdateHostChannel,
} from "../ipc/api";
import type { HostChannel, MeshPeer, MeshStatus, PairingCodeInfo } from "../ipc/types";
import { ChannelLogo } from "../relay/ChannelPicker";
import { useRelay } from "../relay/useRelay";
import { go, type AccountPlatform, type Route } from "../shell/nav";
import { Drawer, Icon, Modal } from "../ui/primitives";

export function ChannelsPage({
  route,
  onGo,
}: {
  route: Route;
  onGo: (r: Route) => void;
}) {
  useEffect(() => {
    if (route.sub === "pool") {
      const targetPlatform = (route.channel && route.channel !== "mesh" ? route.channel : "cursor") as AccountPlatform;
      onGo(go("accounts", { platform: targetPlatform }));
    } else if (route.sub === "models") {
      onGo(go("models", { channel: route.channel || "cursor" }));
    }
  }, [route.sub, route.channel, onGo]);

  return <ChannelsMainView route={route} onGo={onGo} />;
}

function ChannelsMainView({
  route,
  onGo,
}: {
  route: Route;
  onGo: (r: Route) => void;
}) {
  const relay = useRelay({ catalogs: true });
  const [meshStatus, setMeshStatus] = useState<MeshStatus | null>(null);
  const [loadingMesh, setLoadingMesh] = useState<boolean>(true);
  const [filter, setFilter] = useState<"all" | "local" | "remote" | "shared">("all");
  const [search, setSearch] = useState<string>(route.channel ?? "");
  const [customChannels, setCustomChannels] = useState<CustomLogicalChannel[]>(() => loadCustomChannels());

  useEffect(() => {
    if (route.channel && !route.sub) {
      setSearch(route.channel);
    }
  }, [route.channel, route.sub]);

  // Mesh Modals state
  const [connectModalOpen, setConnectModalOpen] = useState<boolean>(false);
  const [sharingModalChannel, setSharingModalChannel] = useState<LocalChannel | null>(null);
  const [activeInvite, setActiveInvite] = useState<PairingCodeInfo | null>(null);
  const [inviteSecondsLeft, setInviteSecondsLeft] = useState<number>(600);

  // Composer Drawer state
  const [composerOpen, setComposerOpen] = useState<boolean>(false);
  const [composerChannel, setComposerChannel] = useState<CustomLogicalChannel | null>(null);

  // Pairing form
  const [pairCode, setPairCode] = useState<string>("");
  const [pairAlias, setPairAlias] = useState<string>("");
  const [pairingBusy, setPairingBusy] = useState<boolean>(false);
  const [pairingError, setPairingError] = useState<string | null>(null);

  // Sharing form
  const [shareConcurrency, setShareConcurrency] = useState<number>(2);
  const [sharingBusy, setSharingBusy] = useState<boolean>(false);
  const [copiedInvite, setCopiedInvite] = useState<boolean>(false);

  // Action status message
  const [actionNotice, setActionNotice] = useState<string | null>(null);

  async function loadMesh() {
    setLoadingMesh(true);
    try {
      const s = await meshGetStatus();
      setMeshStatus(s);
    } catch {
    } finally {
      setLoadingMesh(false);
    }
  }

  useEffect(() => {
    void loadMesh();
    const timer = setInterval(() => {
      void loadMesh();
    }, 6000);
    return () => clearInterval(timer);
  }, []);

  // Countdown timer for active invite
  useEffect(() => {
    if (!activeInvite) return;
    const interval = setInterval(() => {
      const now = Math.floor(Date.now() / 1000);
      const left = Math.max(0, activeInvite.expires_at - now);
      setInviteSecondsLeft(left);
    }, 1000);
    return () => clearInterval(interval);
  }, [activeInvite]);

  // Notice auto-dismiss
  useEffect(() => {
    if (!actionNotice) return;
    const t = setTimeout(() => setActionNotice(null), 3500);
    return () => clearTimeout(t);
  }, [actionNotice]);

  const peers: MeshPeer[] = useMemo(
    () => meshStatus?.peers ?? [],
    [meshStatus?.peers],
  );

  const locals = useMemo(
    () => localChannels(relay.gateway, relay.local, peers, customChannels),
    [relay.gateway, relay.local, peers, customChannels],
  );

  const defaultId = useMemo(
    () => defaultChannelId(relay.gateway, locals),
    [relay.gateway, locals],
  );

  const sharedHostChannels = useMemo(
    () => (meshStatus?.host_channels ?? []).filter((h) => h.enabled),
    [meshStatus?.host_channels],
  );

  async function handleSetDefault(id: string) {
    try {
      await gatewayApi.updateSettings({ defaultChannel: id });
      setActionNotice(`已将【${id}】设为全局默认路由通道`);
      await relay.reload();
    } catch (e: any) {
      setActionNotice(`设置失败: ${e?.message ?? e}`);
    }
  }

  // Open sharing modal with TWO-STAGE state machine
  function openShareModal(ch: LocalChannel) {
    const existing = (meshStatus?.host_channels ?? []).find(
      (h) => h.channel_id === ch.id,
    );
    const concurrency = existing ? existing.max_concurrency : 2;
    setShareConcurrency(concurrency);
    setSharingModalChannel(ch);
    setCopiedInvite(false);
    setActiveInvite(null); // 两段式：点开时不生成配对码，由用户显式点击
  }

  // Stage 1 -> Stage 2: Explicit confirmation to share and generate code
  async function handleActivateShare() {
    if (!sharingModalChannel) return;
    setSharingBusy(true);
    try {
      let cleanModels = modelsOf(sharingModalChannel).map((m) => {
        const idx = m.indexOf("/");
        return idx >= 0 ? m.slice(idx + 1) : m;
      });
      if (cleanModels.length === 0 && sharingModalChannel.id === "cursor") {
        cleanModels = ["claude-sonnet-5", "claude-opus-5", "gpt-5.6-sol", "grok-4.6", "auto"];
      }
      const hostChannel: HostChannel = {
        channel_id: sharingModalChannel.id,
        label: sharingModalChannel.label,
        tier: "tier1_local",
        enabled: true,
        max_concurrency: shareConcurrency,
        reserve_quota_percent: 20,
        allowed_models: cleanModels,
      };
      await meshUpdateHostChannel(hostChannel);
      await meshSetSharing(true);
      await loadMesh();
      const invite = await meshGeneratePairingCode();
      setActiveInvite(invite);
      setInviteSecondsLeft(600);
      setActionNotice(`已开启【${sharingModalChannel.label}】的 Mesh 共享并生成配对码`);
    } catch (e: any) {
      setActionNotice(`生成配对码失败: ${e?.message ?? e}`);
    } finally {
      setSharingBusy(false);
    }
  }

  // Stop sharing
  async function handleStopShare() {
    if (!sharingModalChannel) return;
    setSharingBusy(true);
    try {
      await meshUpdateHostChannel({
        channel_id: sharingModalChannel.id,
        label: sharingModalChannel.label,
        tier: "tier1_local",
        enabled: false,
        max_concurrency: shareConcurrency,
        reserve_quota_percent: 20,
        allowed_models: [],
      });
      await loadMesh();
      setActiveInvite(null);
      setActionNotice(`已停止【${sharingModalChannel.label}】的 Mesh 共享`);
    } catch (e: any) {
      setActionNotice(`停止失败: ${e?.message ?? e}`);
    } finally {
      setSharingBusy(false);
    }
  }

  // Connect remote peer
  async function handlePairRemote() {
    const code = pairCode.trim().toUpperCase();
    if (!code) {
      setPairingError("请输入 6 位配对码");
      return;
    }
    setPairingBusy(true);
    setPairingError(null);
    try {
      await meshPairDevice(code, pairAlias.trim() || undefined);
      setConnectModalOpen(false);
      setPairCode("");
      setPairAlias("");
      await loadMesh();
      await relay.reload();
      setActionNotice("远端通道已成功挂载！可在列表中立即调用");
    } catch (e: any) {
      setPairingError(e?.message ?? "配对连接失败，请检查配对码是否过期");
    } finally {
      setPairingBusy(false);
    }
  }

  // Unpair remote peer
  async function handleUnpairPeer(nodeId: string) {
    try {
      await meshUnpairDevice(nodeId);
      await loadMesh();
      await relay.reload();
      setActionNotice("已断开远端节点");
    } catch (e: any) {
      setActionNotice(`操作失败: ${e?.message ?? e}`);
    }
  }

  // Open Composer
  function handleOpenComposer(ch?: LocalChannel) {
    if (ch && ch.kind === "custom") {
      const existing = customChannels.find((c) => c.id === ch.id);
      setComposerChannel(
        existing ?? {
          id: ch.id,
          label: ch.label,
          platform: (ch.platform as AccountPlatform) || "claude",
          prefix: ch.prefixes[0]?.replace(/\/$/, "") || ch.id,
          accountIds: ch.boundAccountLabels ?? [],
          strategy: ch.strategy ?? "quota_first",
          createdAt: Date.now(),
        }
      );
    } else if (ch) {
      const existing = customChannels.find((c) => c.id === ch.id);
      const basePlatform = (ch.platform as AccountPlatform) || (ch.id as AccountPlatform) || "claude";
      setComposerChannel(
        existing ?? {
          id: ch.id,
          label: ch.label,
          platform: basePlatform,
          prefix: ch.prefixes[0]?.replace(/\/$/, "") || ch.id,
          accountIds: ch.boundAccountLabels ?? [],
          strategy: ch.strategy ?? "quota_first",
          createdAt: Date.now(),
        }
      );
    } else {
      setComposerChannel({
        id: `custom-pool-${Date.now().toString().slice(-4)}`,
        label: "Claude-主力工作组",
        platform: "claude",
        prefix: "claude-work",
        accountIds: [],
        strategy: "quota_first",
        createdAt: Date.now(),
      });
    }
    setComposerOpen(true);
  }

  function handleSaveComposer(channelToSave: CustomLogicalChannel) {
    upsertCustomChannel(channelToSave);
    const updated = loadCustomChannels();
    setCustomChannels(updated);
    setComposerOpen(false);
    setComposerChannel(null);
    setActionNotice(`已保存通道【${channelToSave.label}】编排策略`);
    void relay.reload();
  }

  function handleDeleteCustomChannel(id: string) {
    deleteCustomChannel(id);
    const updated = loadCustomChannels();
    setCustomChannels(updated);
    setComposerOpen(false);
    setComposerChannel(null);
    setActionNotice("已删除自定义分流通道");
    void relay.reload();
  }

  // Filter channels
  const query = search.trim().toLowerCase();

  const actualLocals = useMemo(
    () => locals.filter((c) => c.kind !== "mesh"),
    [locals],
  );

  const searchLocals = useMemo(() => {
    if (!query) return actualLocals;
    return actualLocals.filter((ch) => {
      const matchName = ch.label.toLowerCase().includes(query) || ch.id.toLowerCase().includes(query);
      const matchVendor = ch.vendor.toLowerCase().includes(query);
      const matchModels = modelsOf(ch).some((m) => m.toLowerCase().includes(query));
      return matchName || matchVendor || matchModels;
    });
  }, [actualLocals, query]);

  const searchPeers = useMemo(() => {
    if (!query) return peers;
    return peers.filter((p) => {
      const matchName = p.device_name.toLowerCase().includes(query) || p.node_id.toLowerCase().includes(query);
      const matchModels = p.remote_models.some((m) => m.toLowerCase().includes(query));
      return matchName || matchModels;
    });
  }, [peers, query]);

  const filteredLocals = useMemo(() => {
    if (filter === "remote") return [];
    if (filter === "shared") {
      return searchLocals.filter((ch) => sharedHostChannels.some((h) => h.channel_id === ch.id));
    }
    return searchLocals;
  }, [searchLocals, filter, sharedHostChannels]);

  const filteredPeers = useMemo(() => {
    if (filter === "local" || filter === "shared") return [];
    return searchPeers;
  }, [searchPeers, filter]);

  const sharedMatchesCount = useMemo(() => {
    return searchLocals.filter((ch) => sharedHostChannels.some((h) => h.channel_id === ch.id)).length;
  }, [searchLocals, sharedHostChannels]);

  // 计算各通道实际供能的具体客户端软件
  const poweringMap = useMemo(() => {
    const map = new Map<string, string[]>();
    const routes = relay.gateway?.routes ?? {};
    const clientNames: Record<string, string> = {
      cursor: "Cursor IDE",
      claude: "Claude Code",
      codex: "Codex CLI",
      opencode: "OpenCode",
      grok: "Grok CLI",
    };
    for (const [tool, r] of Object.entries(routes)) {
      if (!r?.model) continue;
      const toolName = clientNames[tool] || tool;
      const slash = r.model.indexOf("/");
      let chId = slash > 0 ? r.model.slice(0, slash) : defaultId || "claude";
      const peerMatch = peers.find((p) => p.device_name.toLowerCase() === chId.toLowerCase() || p.node_id === chId);
      if (peerMatch) chId = peerMatch.device_name.toLowerCase();
      const list = map.get(chId) ?? [];
      if (!list.includes(toolName)) list.push(toolName);
      map.set(chId, list);
    }
    return map;
  }, [relay.gateway?.routes, defaultId, peers]);

  return (
    <div className="page-wrap" style={{ paddingBottom: 40 }}>
      {/* Toast notification */}
      {actionNotice ? (
        <div className="app-toast">
          <Icon name="check" size={14} style={{ color: "var(--accent)" }} />
          <span>{actionNotice}</span>
        </div>
      ) : null}

      {/* 单行集成顶栏：左侧筛选胶囊，右侧搜索+操作 */}
      <div className="channels-topbar">
        <div className="channels-topbar-left">
          <div className="tabs" role="tablist">
            <button
              type="button"
              role="tab"
              aria-selected={filter === "all"}
              className={filter === "all" ? "tab is-active" : "tab"}
              onClick={() => setFilter("all")}
            >
              全部 {searchLocals.length + searchPeers.length}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={filter === "local"}
              className={filter === "local" ? "tab is-active" : "tab"}
              onClick={() => setFilter("local")}
            >
              本地 {searchLocals.length}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={filter === "remote"}
              className={filter === "remote" ? "tab is-active" : "tab"}
              onClick={() => setFilter("remote")}
            >
              Mesh 云端 {searchPeers.length}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={filter === "shared"}
              className={filter === "shared" ? "tab is-active" : "tab"}
              onClick={() => setFilter("shared")}
            >
              已共享 {sharedMatchesCount}
            </button>
          </div>
        </div>

        <div className="channels-topbar-right">
          <label className="search channels-search">
            <Icon name="search" size={13} />
            <input
              value={search}
              placeholder="搜索通道、模型、前缀"
              onChange={(e) => setSearch(e.target.value)}
            />
            {search ? (
              <button
                type="button"
                className="search-clear"
                onClick={() => setSearch("")}
                aria-label="清空"
              >
                <Icon name="close" size={12} />
              </button>
            ) : null}
          </label>

          <button
            type="button"
            className="btn btn-sm btn-soft"
            onClick={() => {
              setPairCode("");
              setPairAlias("");
              setPairingError(null);
              setConnectModalOpen(true);
            }}
          >
            <Icon name="mesh" size={13} style={{ marginRight: 5, color: "var(--accent)" }} />
            接入 Mesh
          </button>

          <button
            type="button"
            className="btn btn-sm btn-primary channels-new-btn"
            onClick={() => handleOpenComposer()}
          >
            <Icon name="plus" size={13} />
            新建通道
          </button>

          <button
            type="button"
            className="btn btn-sm btn-icon btn-soft"
            disabled={relay.loading || loadingMesh}
            onClick={() => {
              void relay.reload();
              void loadMesh();
            }}
            title="刷新通道与网络"
            aria-label="刷新"
          >
            <Icon name="refresh" size={13} className={relay.loading || loadingMesh ? "is-spinning" : undefined} />
          </button>
        </div>
      </div>

      {/* Channels Grid */}
      <div className="channels-grid">
        {/* Remote Mesh Peers as Cloud Channel Cards */}
        {filteredPeers.map((peer) => {
          const displayName = peer.device_name;
          const prefix = displayName.toLowerCase();
          return (
            <div key={peer.node_id} className="card channel-card channel-cloud-card">
              <div className="channel-card-head" style={{ alignItems: "center" }}>
                <div className="channel-card-brand">
                  <div
                    className="channel-logo-box"
                    style={{
                      color: "var(--accent)",
                      background: "color-mix(in srgb, var(--accent) 8%, var(--wash-1))",
                      borderColor: "color-mix(in srgb, var(--accent) 22%, var(--line-soft))",
                    }}
                  >
                    <Icon name="mesh" size={17} />
                  </div>
                  <span className="channel-card-title truncate" title={displayName}>
                    {displayName}
                  </span>
                  <span className="channel-route-prefix mono">{prefix}/*</span>
                </div>

                <div className="channel-card-badges">
                  <span className="pill pill-mesh-badge">Mesh 远端</span>
                  <span className="pill pill-latency mono">
                    <span className="pod-dot is-live" style={{ width: 5, height: 5 }} />
                    {peer.latency_ms}ms
                  </span>
                </div>
              </div>

              {/* Source & Capacity Info */}
              <div className="channel-detail-box" style={{ padding: 0, background: "transparent", border: "none" }}>
                <div className="chan-metrics-grid">
                  <div
                    className="chan-metric-box"
                    title={`远端设备: ${peer.device_name}\n节点指纹: ${peer.node_id}\n链路 RTT: ${peer.latency_ms}ms`}
                  >
                    <div className="chan-metric-head-row">
                      <span className="chan-metric-label">节点与拓扑</span>
                      <span className="pill tiny chan-strategy-pill">
                        {peer.latency_ms > 0 && peer.latency_ms < 30 ? "⚡ LAN 直连" : "🛡️ 加密隧道"}
                      </span>
                    </div>
                    <div className="chan-metric-content">
                      <span className="pod-dot is-live" />
                      <span className="chan-metric-val mono" title={peer.node_id}>
                        node_{peer.node_id.slice(0, 8)}
                      </span>
                    </div>
                  </div>

                  <div
                    className="chan-metric-box chan-metric-clickable"
                    onClick={() => onGo(go("models", { channel: prefix }))}
                    title={`共 ${peer.remote_models.length} 款远端可用模型，并发上限 4 路，点击前往模型广场查看`}
                  >
                    <div className="chan-metric-head-row">
                      <span className="chan-metric-label">可用模型</span>
                      <Icon name="arrow" size={11} className="chan-models-arrow faint" />
                    </div>
                    <div className="chan-metric-content">
                      <span className="chan-metric-val">
                        {peer.remote_models.length > 0 ? `${peer.remote_models.length} 款模型` : "待同步"}
                      </span>
                    </div>
                  </div>
                </div>
              </div>

              {/* Card Foot */}
              <div className="channel-card-foot">
                <div className="chan-foot-left truncate">
                  <span className="chan-foot-k">接入:</span>
                  {(() => {
                    const pid = peer.device_name.toLowerCase();
                    const poweringList = poweringMap.get(pid) ?? poweringMap.get(peer.node_id) ?? [];
                    if (poweringList.length > 0) {
                      return (
                        <div className="chan-foot-clients truncate">
                          {poweringList.map((tool) => (
                            <span key={tool} className="chan-client-tag is-mesh" title={`已作为 ${tool} 的路由通道`}>
                              <span className="chan-client-name">{tool}</span>
                            </span>
                          ))}
                        </div>
                      );
                    }
                    return (
                      <span className="chan-client-tag is-mesh" title="远端 Mesh 节点待命中">
                        <Icon name="mesh" size={10} style={{ opacity: 0.8 }} />
                        远端待命
                      </span>
                    );
                  })()}
                </div>
                <div className="chan-foot-actions">
                  <button
                    type="button"
                    className="btn btn-xs btn-soft text-bad"
                    onClick={() => void handleUnpairPeer(peer.node_id)}
                    title="断开与此远端节点的互联挂载"
                  >
                    断开连接
                  </button>
                </div>
              </div>
            </div>
          );
        })}

        {/* Local Channels as Cards */}
        {filteredLocals.map((ch) => {
          const isDefault = ch.id === defaultId;
          const allModels = modelsOf(ch);
          const isShared = sharedHostChannels.some((h) => h.channel_id === ch.id);
          const prefix = ch.prefixes[0]?.replace(/\/$/, "") || ch.id;
          const poweringList = poweringMap.get(ch.id) ?? [];
          const totalCandidates = ch.lane.candidates.length;
          const readyCount = ch.lane.candidates.filter(usableCandidate).length;

          return (
            <div key={ch.id} className={`card channel-card${isDefault ? " is-default" : ""}`}>
              {/* Card Head: 单行标题 + 前缀 + 标签 */}
              <div className="channel-card-head" style={{ alignItems: "center" }}>
                <div className="channel-card-brand">
                  <div className="channel-logo-box">
                    <ChannelLogo id={ch.id} size={20} />
                  </div>
                  <span className="channel-card-title">{ch.label}</span>
                  {ch.id !== "cursor" ? (
                    <span className="channel-route-prefix mono">{prefix}/*</span>
                  ) : (
                    <span className="channel-route-prefix mono">cursor/* (原生直通)</span>
                  )}
                </div>

                <div className="channel-card-badges">
                  {ch.id === "cursor" ? (
                    <span className="pill pill-mesh-badge" style={{ background: "rgba(59, 130, 246, 0.15)", color: "var(--accent)" }}>
                      ● 原生直通
                    </span>
                  ) : null}
                  {isDefault ? (
                    <span className="pill pill-default-badge">★ 默认</span>
                  ) : ch.id !== "cursor" ? (
                    <button
                      type="button"
                      className="pill pill-set-default-btn"
                      onClick={() => void handleSetDefault(ch.id)}
                      title="设为全局默认路由通道"
                    >
                      设为默认
                    </button>
                  ) : null}
                  {isShared ? (
                    <span className="pill pill-shared-badge">↗ 共享中</span>
                  ) : null}
                </div>
              </div>

              {/* Metrics Grid: 账号数量（含前缀与策略）与可用模型 */}
              <div className="channel-detail-box" style={{ padding: 0, background: "transparent", border: "none" }}>
                <div className="chan-metrics-grid">
                  <div
                    className="chan-metric-box chan-metric-clickable"
                    onClick={() => onGo(go("accounts", { platform: ch.id as any }))}
                    title={
                      totalCandidates > 0
                        ? `共 ${totalCandidates} 个账号（${readyCount} 个就绪），点击前往账号库管理`
                        : "暂未绑定账号，点击前往添加"
                    }
                  >
                    <div className="chan-metric-head-row">
                      <span className="chan-metric-label">调度账号</span>
                    </div>
                    <div className="chan-metric-content">
                      <span className={`chan-acc-dot ${readyCount > 0 ? (readyCount < totalCandidates ? "is-warn" : "is-ok") : totalCandidates > 0 ? "is-err" : "is-off"}`} />
                      <span className="chan-metric-val">
                        {totalCandidates > 0 ? `${totalCandidates} 个账号` : "未配置账号"}
                      </span>
                      {readyCount < totalCandidates && totalCandidates > 0 && (
                        <span className="pill-tag is-warn" style={{ marginLeft: "auto", fontSize: "10px" }}>
                          {readyCount}/{totalCandidates} 可用
                        </span>
                      )}
                    </div>
                  </div>

                  <div
                    className="chan-metric-box chan-metric-clickable"
                    onClick={() => onGo(go("models", { channel: ch.id }))}
                    title={`共 ${allModels.length} 款可用模型，点击前往模型广场查看与调优`}
                  >
                    <div className="chan-metric-head-row">
                      <span className="chan-metric-label">可用模型</span>
                      <Icon name="arrow" size={11} className="chan-models-arrow faint" />
                    </div>
                    <div className="chan-metric-content">
                      <span className="chan-metric-val">
                        {allModels.length > 0 ? `${allModels.length} 款模型` : "全系可用"}
                      </span>
                    </div>
                  </div>
                </div>
              </div>

              {/* Card Foot Actions: 单行合并展示接入客户端与操作按钮 */}
              <div className="channel-card-foot">
                <div className="chan-foot-left truncate">
                  <span className="chan-foot-k">接入:</span>
                  {poweringList.length > 0 ? (
                    <div className="chan-foot-clients truncate">
                      {poweringList.map((tool) => (
                        <span key={tool} className="chan-client-tag" title={`已作为 ${tool} 的路由通道`}>
                          <span className="chan-client-name">{tool}</span>
                        </span>
                      ))}
                    </div>
                  ) : isShared ? (
                    <span className="chan-client-tag is-mesh" title="出借至 Mesh 远端节点">
                      ☁️ Mesh 远端
                    </span>
                  ) : (
                    <span className="chan-client-idle">未接入</span>
                  )}
                </div>

                <div className="chan-foot-actions">
                  <button
                    type="button"
                    className="btn btn-xs btn-soft"
                    onClick={() => handleOpenComposer(ch)}
                  >
                    编辑
                  </button>
                  <button
                    type="button"
                    className="btn btn-xs btn-soft"
                    onClick={() => openShareModal(ch)}
                  >
                    {isShared ? "管理出借" : "↗ 出借"}
                  </button>
                </div>
              </div>
            </div>
          );
        })}
      </div>

      {/* Two-Stage Mesh Sharing Modal */}
      {sharingModalChannel ? (
        <Modal
          className="modal-mesh-dialog"
          onClose={() => setSharingModalChannel(null)}
          title={`Mesh 跨机共享 · ${sharingModalChannel.label}`}
          footer={
            !activeInvite ? (
              <>
                <button
                  type="button"
                  className="btn btn-sm btn-soft"
                  onClick={() => setSharingModalChannel(null)}
                >
                  取消
                </button>
                <button
                  type="button"
                  className="btn btn-sm btn-primary"
                  disabled={sharingBusy}
                  onClick={handleActivateShare}
                >
                  {sharingBusy ? "正在生成配对码…" : "开启共享并生成配对码"}
                </button>
              </>
            ) : (
              <div className="mesh-modal-foot-split">
                <button
                  type="button"
                  className="btn btn-sm btn-quiet text-bad mesh-stop-btn"
                  onClick={handleStopShare}
                  title="停止对外共享此通道"
                >
                  停止共享
                </button>

                <div className="mesh-foot-right">
                  <button
                    type="button"
                    className="btn btn-sm btn-primary"
                    onClick={async () => {
                      await navigator.clipboard.writeText(activeInvite.code);
                      setCopiedInvite(true);
                      setTimeout(() => setCopiedInvite(false), 2000);
                    }}
                  >
                    <Icon name={copiedInvite ? "check" : "copy"} size={13} />
                    <span>{copiedInvite ? "已复制配对码" : "复制配对码"}</span>
                  </button>
                  <button
                    type="button"
                    className="btn btn-sm btn-soft"
                    onClick={() => setSharingModalChannel(null)}
                  >
                    完成
                  </button>
                </div>
              </div>
            )
          }
        >
          <div className="mesh-share-dialog">
            {!activeInvite ? (
              /* Stage 1: Read-Only Config & Explicit Confirmation */
              <div className="share-stage-one">
                <div className="mesh-connect-callout">
                  <Icon name="mesh" size={15} />
                  <div className="mesh-connect-callout-text">
                    将通道作为 Mesh 跨机通道共享给局域网或远端设备。<b>账号凭证绝不离开本机</b>，远端仅发送安全代理推理请求。
                  </div>
                </div>

                <div className="mesh-field-group">
                  <div className="mesh-field-head">
                    <label className="mesh-field-label">最大远端并发限制</label>
                    <span className="mesh-field-opt">防冲撞本机窗口</span>
                  </div>
                  <div className="mesh-concurrency-row">
                    {[1, 2, 4, 8].map((c) => (
                      <button
                        key={c}
                        type="button"
                        className={`mesh-conc-btn${shareConcurrency === c ? " is-active" : ""}`}
                        onClick={() => setShareConcurrency(c)}
                      >
                        {c} 路并发
                      </button>
                    ))}
                  </div>
                </div>

                <div className="mesh-field-group">
                  <div className="mesh-field-head">
                    <label className="mesh-field-label">本机优先</label>
                    <span className="pill pill-tag is-ok">已启用</span>
                  </div>
                  <div className="mesh-field-desc">
                    本机在用网关时暂停出借，空闲 30 秒后恢复。
                  </div>
                </div>
              </div>
            ) : (
              /* Stage 2: Active Pairing Code Display */
              <div className="share-stage-two">
                <div className="mesh-active-card">
                  <div className="mesh-active-banner">
                    <span className="pod-dot is-live" />
                    <span className="mesh-active-status">配对广播已就绪 · 等待远端接入</span>
                  </div>

                  <div
                    className="mesh-code-display-box"
                    onClick={async () => {
                      await navigator.clipboard.writeText(activeInvite.code);
                      setCopiedInvite(true);
                      setTimeout(() => setCopiedInvite(false), 2000);
                    }}
                    title="点击复制配对码"
                  >
                    <div className="mesh-code-num mono">{activeInvite.code}</div>
                    <div className={`mesh-code-copy-pill mono${copiedInvite ? " is-copied" : ""}`}>
                      <Icon name={copiedInvite ? "check" : "copy"} size={11} />
                      <span>{copiedInvite ? "已复制" : "点击复制"}</span>
                    </div>
                  </div>

                  <div className="mesh-active-meta-row">
                    <div className="mesh-active-meta-item">
                      <span className="mesh-meta-k">有效期剩余</span>
                      <span className="mono bold tabular-nums mesh-meta-v">
                        {Math.floor(inviteSecondsLeft / 60)
                          .toString()
                          .padStart(2, "0")}
                        :
                        {(inviteSecondsLeft % 60).toString().padStart(2, "0")}
                      </span>
                    </div>
                    <div className="mesh-active-meta-sep" />
                    <div className="mesh-active-meta-item">
                      <span className="mesh-meta-k">并发上限</span>
                      <span className="mono bold mesh-meta-v">{shareConcurrency} 路</span>
                    </div>
                  </div>
                </div>

                <div className="mesh-active-hint">
                  在另一台运行 Nexus 的设备上，进入<b>「通道 › 接入 Mesh」</b>并填入此配对码即可建立直连。
                </div>
              </div>
            )}
          </div>
        </Modal>
      ) : null}

      {/* Connect Remote Peer Modal */}
      {connectModalOpen ? (
        <Modal
          className="modal-mesh-dialog"
          onClose={() => setConnectModalOpen(false)}
          title="接入 Mesh 云端通道"
          footer={
            <>
              <button
                type="button"
                className="btn btn-sm btn-soft"
                onClick={() => setConnectModalOpen(false)}
              >
                取消
              </button>
              <button
                type="button"
                className="btn btn-sm btn-primary"
                disabled={pairingBusy || pairCode.trim().length < 4}
                onClick={handlePairRemote}
              >
                {pairingBusy ? "连接配对中…" : "确认连接并挂载"}
              </button>
            </>
          }
        >
          <div className="connect-mesh-dialog">
            <div className="mesh-connect-callout">
              <Icon name="mesh" size={15} />
              <div className="mesh-connect-callout-text">
                在另一台运行 Nexus 的设备上，点击通道卡片的<b>「出借」</b>即可生成专属动态配对码。
              </div>
            </div>

            <div className="mesh-field-group">
              <div className="mesh-field-head">
                <label className="mesh-field-label">动态配对码</label>
                <span className="mesh-field-badge">必填</span>
              </div>
              <div className="mesh-code-wrap">
                <input
                  className="mesh-code-input mono"
                  value={pairCode}
                  placeholder="NX-XXXX-XXXX"
                  maxLength={16}
                  onChange={(e) => setPairCode(e.target.value.toUpperCase().replace(/[^A-Z0-9-]/g, ""))}
                  autoFocus
                />
                {pairCode.length > 0 ? (
                  <span className="mesh-code-counter">{pairCode.length} 字符</span>
                ) : null}
              </div>
            </div>

            <div className="mesh-field-group">
              <div className="mesh-field-head">
                <label className="mesh-field-label">通道别名</label>
                <span className="mesh-field-opt">可选</span>
              </div>
              <input
                className="input mesh-alias-input"
                value={pairAlias}
                placeholder="例如：家里台式机 (home-pc)"
                onChange={(e) => setPairAlias(e.target.value)}
              />
            </div>

            {pairingError ? (
              <div className="dialog-error">
                {pairingError}
              </div>
            ) : null}
          </div>
        </Modal>
      ) : null}

      {/* Channel Composer Drawer */}
      {composerOpen && composerChannel ? (
        <ChannelComposerDrawer
          channel={composerChannel}
          allChannels={actualLocals}
          onClose={() => {
            setComposerOpen(false);
            setComposerChannel(null);
          }}
          onSave={handleSaveComposer}
          onDelete={handleDeleteCustomChannel}
        />
      ) : null}
    </div>
  );
}

function ChannelComposerDrawer({
  channel: initial,
  allChannels,
  onClose,
  onSave,
  onDelete,
}: {
  channel: CustomLogicalChannel;
  allChannels: LocalChannel[];
  onClose: () => void;
  onSave: (c: CustomLogicalChannel) => void;
  onDelete: (id: string) => void;
}) {
  const [label, setLabel] = useState(initial.label);
  const [prefix, setPrefix] = useState(initial.prefix);
  const [strategy, setStrategy] = useState<ChannelStrategy>(initial.strategy);
  const [selectedAccounts, setSelectedAccounts] = useState<string[]>(initial.accountIds);
  const platform = initial.platform;

  const baseCh = allChannels.find((c) => c.id === platform);
  const availableCandidates = baseCh?.lane.candidates ?? [];

  const [extraCandidates, setExtraCandidates] = useState<any[]>([]);
  useEffect(() => {
    if (availableCandidates.length === 0) {
      accountsApi
        .list()
        .then((list) => {
          const matched = list.filter((a) => {
            const ch = a.codeChannelResolved || a.codeChannel;
            return ch === platform;
          });
          setExtraCandidates(
            matched.map((a) => ({
              label: a.email,
              storedId: a.id,
              percentUsed: a.usage?.apiPercentUsed ?? null,
              state: { kind: a.status === "active" ? "idle" : "cooldown" },
            })),
          );
        })
        .catch(() => {});
    }
  }, [availableCandidates.length, platform]);

  const candidatesToDisplay =
    availableCandidates.length > 0 ? availableCandidates : extraCandidates;

  function toggleAccount(accId: string) {
    if (selectedAccounts.includes(accId)) {
      setSelectedAccounts(selectedAccounts.filter((id) => id !== accId));
    } else {
      setSelectedAccounts([...selectedAccounts, accId]);
    }
  }

  function handleSave() {
    onSave({
      ...initial,
      label: label.trim() || initial.label,
      prefix: prefix.trim().toLowerCase().replace(/[^a-z0-9_-]/g, "") || initial.prefix,
      platform,
      strategy,
      accountIds: selectedAccounts,
    });
  }

  const BUILTIN_PLATFORMS = [
    "cursor",
    "claude",
    "chatgpt",
    "grok",
    "kiro",
    "zcode",
    "qoder",
    "antigravity",
  ];
  const isBuiltin = BUILTIN_PLATFORMS.includes(initial.id);
  const isEditing = Boolean(initial.id && initial.label);

  const head = (
    <div className="act-drawer-title-wrap">
      <Icon name="channels" size={16} />
      <h2 className="act-drawer-title">
        {isBuiltin
          ? `配置通道 · ${initial.label}`
          : isEditing
          ? `编辑通道 · ${initial.label}`
          : "新建分流通道"}
      </h2>
    </div>
  );

  const footer = (
    <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", width: "100%" }}>
      <div>
        {!isBuiltin ? (
          <button
            type="button"
            className="btn btn-sm btn-soft composer-del-btn"
            onClick={() => onDelete(initial.id)}
          >
            删除通道
          </button>
        ) : null}
      </div>
      <div className="row" style={{ gap: 10 }}>
        <button type="button" className="btn btn-sm btn-soft" onClick={onClose}>
          取消
        </button>
        <button type="button" className="btn btn-sm btn-primary" onClick={handleSave}>
          {isEditing ? "保存通道" : "创建通道"}
        </button>
      </div>
    </div>
  );

  return (
    <Drawer
      label={isEditing ? `编辑通道 · ${initial.label}` : "新建分流通道"}
      head={head}
      footer={footer}
      onClose={onClose}
    >
      <div className="composer-form-section">
        <label className="composer-form-label">通道名称</label>
        <input
          className="input composer-input-field"
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          placeholder="例如：Claude-主力工作组"
        />
      </div>

      <div className="composer-form-section">
        <div className="composer-form-label-row">
          <label className="composer-form-label">路由前缀 (Route Prefix)</label>
        </div>
        <div className="composer-input-group">
          <input
            className="composer-group-input mono"
            value={prefix}
            onChange={(e) => setPrefix(e.target.value)}
            placeholder="例如：claude-work"
          />
          <span className="composer-group-suffix mono">/*</span>
        </div>
        <span className="composer-form-hint">
          客户端请求以此前缀开头（如 <code className="mono">{prefix || "..."}/*</code>）时，网关将自动分配至本通道
        </span>
      </div>

      <div className="composer-form-section">
        <label className="composer-form-label">调度策略</label>
        <div className="composer-strategy-grid">
          <button
            type="button"
            className={`strategy-card${strategy === "quota_first" ? " is-active" : ""}`}
            onClick={() => setStrategy("quota_first")}
          >
            <div className="strategy-card-head">
              <span className="strategy-card-title">额度充足优先</span>
              <span className={`strategy-dot${strategy === "quota_first" ? " is-active" : ""}`} />
            </div>
            <div className="strategy-card-desc">按剩余百分比择优调用</div>
          </button>

          <button
            type="button"
            className={`strategy-card${strategy === "round_robin" ? " is-active" : ""}`}
            onClick={() => setStrategy("round_robin")}
          >
            <div className="strategy-card-head">
              <span className="strategy-card-title">顺序轮询</span>
              <span className={`strategy-dot${strategy === "round_robin" ? " is-active" : ""}`} />
            </div>
            <div className="strategy-card-desc">按顺序轮换可用账号</div>
          </button>

          <button
            type="button"
            className={`strategy-card${strategy === "sticky" ? " is-active" : ""}`}
            onClick={() => setStrategy("sticky")}
          >
            <div className="strategy-card-head">
              <span className="strategy-card-title">单号粘性</span>
              <span className={`strategy-dot${strategy === "sticky" ? " is-active" : ""}`} />
            </div>
            <div className="strategy-card-desc">锁定当前号直至耗尽</div>
          </button>
        </div>
      </div>

      <div className="composer-form-section">
        <div className="composer-form-label-row">
          <label className="composer-form-label">选择要绑定的账号</label>
          <span className="pill tiny mono" style={{ background: "var(--wash-2)", color: "var(--color-ink-2)" }}>
            {selectedAccounts.length} / {candidatesToDisplay.length} 已选
          </span>
        </div>
        <div className="composer-accounts-box" style={{ display: "flex", flexDirection: "column", gap: 6 }}>
          {candidatesToDisplay.length > 0 ? (
            candidatesToDisplay.map((cand) => {
              const checked = selectedAccounts.includes(cand.label) || (cand.storedId ? selectedAccounts.includes(cand.storedId) : false);
              const isReady = usableCandidate(cand);
              const pct = cand.percentUsed != null ? Math.round(Math.max(0, Math.min(100, 100 - cand.percentUsed))) : 100;
              return (
                <div
                  key={cand.label}
                  className={`composer-acc-row${checked ? " is-checked" : ""}`}
                  onClick={() => toggleAccount(cand.storedId || cand.label)}
                >
                  <div className="composer-acc-left">
                    <div className={`composer-checkbox${checked ? " is-on" : ""}`}>
                      {checked ? <Icon name="check" size={10} /> : null}
                    </div>
                    <span className={`chan-acc-dot ${isReady ? "is-ok" : "is-err"}`} />
                    <span className="composer-acc-email mono truncate" title={cand.label}>
                      {cand.label}
                    </span>
                    {cand.pinned ? (
                      <span className="pill tiny mono" style={{ fontSize: 9.5, padding: "0 4px" }}>
                        当前登录
                      </span>
                    ) : null}
                  </div>
                  <div className="composer-acc-right">
                    <div className="chan-meter-track" style={{ width: 44 }}>
                      <div
                        className={`chan-meter-fill${pct < 25 ? " is-warn" : ""}`}
                        style={{ width: `${pct}%` }}
                      />
                    </div>
                    <span className="mono tabular-nums" style={{ fontSize: 11, color: "var(--color-muted)", minWidth: 28, textAlign: "right" }}>
                      {pct}%
                    </span>
                  </div>
                </div>
              );
            })
          ) : (
            <div className="composer-empty-slot">
              <Icon name="layers" size={18} style={{ opacity: 0.6 }} />
              <div className="composer-empty-texts" style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                <span style={{ fontWeight: 600, color: "var(--color-ink-2)" }}>当前平台暂无托管账号</span>
                <span className="faint" style={{ fontSize: 11 }}>请先在「资产库 › 账号」中添加可用账号</span>
              </div>
            </div>
          )}
        </div>
      </div>
    </Drawer>
  );
}
