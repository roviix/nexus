/**
 * 模型广场：全景聚合 + 通道胶囊筛选（Nexus 2.0 §4.3）。
 *
 * 核心升级：
 * 1. 从「先选通道才能看」升级为「全量视图 + 通道胶囊过滤」；
 * 2. 默认展示所有已就绪通道的模型，无需先选通道；
 * 3. 智能隐藏空通道，末尾提供微型气泡提示去配置；
 * 4. 模型卡集成「试一下 (Live Probe)」与「指派给软件 (一键绑定)」。
 */
import { useEffect, useMemo, useState } from "react";
import { channelOfModel, localChannels } from "../gateway/channels";
import { meshGetStatus } from "../ipc/api";
import type { LocalModel, Modality } from "../ipc/models";
import type { MeshPeer, MeshStatus } from "../ipc/types";
import { ModelCard } from "../relay/ModelCard";
import { useRelay } from "../relay/useRelay";
import { go, type Route } from "../shell/nav";
import { filterCards, groupLocal, modalityRail, vendorRail, type CardFilter } from "../ui/models";
import { Icon } from "../ui/primitives";
import { TryDrawer, type TryLane } from "./models/TryDrawer";

const MODALITY_LABEL: Record<Modality, string> = { chat: "对话", image: "图片", video: "视频" };

export function ModelsPage({ route, onGo }: { route: Route; onGo: (r: Route) => void }) {
  const relay = useRelay({ catalogs: true });
  const [meshStatus, setMeshStatus] = useState<MeshStatus | null>(null);
  /** 选中的通道：null 或 "all" = 查看全部已就绪通道 */
  const [channelFilter, setChannelFilter] = useState<string | null>(route.channel ?? "all");
  const [filter, setFilter] = useState<CardFilter>({ query: "", vendor: "all", modality: "all" });
  /** 打开「试一下」时预选的模型；null = 抽屉关着 */
  const [trying, setTrying] = useState<string | null>(null);

  useEffect(() => {
    if (route.channel) setChannelFilter(route.channel);
  }, [route.channel]);

  useEffect(() => {
    void meshGetStatus().then(setMeshStatus).catch(() => {});
    const timer = setInterval(() => {
      void meshGetStatus().then(setMeshStatus).catch(() => {});
    }, 5000);
    return () => clearInterval(timer);
  }, []);

  const peers: MeshPeer[] = useMemo(() => meshStatus?.peers ?? [], [meshStatus?.peers]);

  const channels = useMemo(
    () => localChannels(relay.gateway, relay.local, peers),
    [relay.gateway, relay.local, peers],
  );

  // Ready channels
  const readyChannels = useMemo(() => {
    return channels.filter((c) => {
      if (c.kind === "mesh") return true;
      return c.ready || (c.chatModels.length > 0 && c.lane.candidates.length > 0);
    });
  }, [channels]);

  // 全量或过滤后的模型列表
  const localShown = useMemo(() => {
    const allLocal = relay.local ?? [];

    // 构建远端 Mesh 虚拟模型
    const meshModels: LocalModel[] = [];
    for (const p of peers) {
      const pName = p.device_name.toLowerCase();
      const existing = allLocal.filter((m) => m.id.toLowerCase().startsWith(`${pName}/`));
      if (existing.length > 0) {
        meshModels.push(...existing);
      } else {
        meshModels.push(
          ...p.remote_models.map((m) => ({
            id: `${pName}/${m}`,
            vendor: "nexus-mesh" as const,
            vendorLabel: p.device_name,
            modality: "chat" as const,
            series: m,
            variant: "standard",
            aliases: [`mesh/${pName}/${m}`],
            note: `来自远端节点 [${p.device_name}] 共享的推理算力`,
          }))
        );
      }
    }

    const combined = [
      ...allLocal.filter((m) => channelFilter === "cursor" || !m.id.startsWith("cursor/")),
      ...meshModels,
    ];

    if (!channelFilter || channelFilter === "all") {
      // 默认全量：只展示就绪通道的模型
      const readyIds = new Set(readyChannels.map((c) => c.id.toLowerCase()));
      return combined.filter((m) => {
        const ch = channelOfModel(channels, m.id).toLowerCase();
        return readyIds.has(ch);
      });
    }

    // 选中具体通道
    const target = channelFilter.toLowerCase();
    const isPeer = peers.some((p) => p.device_name.toLowerCase() === target || p.node_id === channelFilter);
    if (isPeer) {
      return combined.filter((m) => m.id.toLowerCase().startsWith(`${target}/`));
    }
    return combined.filter((m) => channelOfModel(channels, m.id).toLowerCase() === target);
  }, [relay.local, peers, channelFilter, readyChannels, channels]);

  const groups = useMemo(() => groupLocal(localShown), [localShown]);
  const shown = useMemo(() => filterCards(groups, filter), [groups, filter]);
  const vendors = useMemo(() => vendorRail(groups, filter), [groups, filter]);
  const modalities = useMemo(() => modalityRail(groups, filter), [groups, filter]);

  const localIds = useMemo(() => localShown.map((m) => m.id), [localShown]);

  const lane: TryLane = {
    ready: Boolean(relay.gateway?.running),
    blocker: relay.gateway?.running
      ? undefined
      : {
          text: "本地网关未开启，暂无法在线调试",
          hint: "试用走的是本地网关的本地接口 (127.0.0.1)。可一键开启。",
          fix: () => {},
          fixLabel: "一键开启网关",
        },
    modelIds: localIds,
    foot: relay.gateway?.running
      ? `本地网关 · 端口 ${relay.gateway.running.addr.split(":").pop()}`
      : "本地网关未开启",
  };

  const loading = relay.local == null;

  return (
    <div>
      {trying ? (
        <TryDrawer
          lane={lane}
          initialModel={trying}
          onClose={() => setTrying(null)}
          onGo={(r) => {
            setTrying(null);
            onGo(r);
          }}
        />
      ) : null}

      {/* 第一行：主通道过滤层 + 右侧刷新按钮 */}
      <div className="plaza-channel-filter-bar">
        <div className="pcf-scroll">
          <button
            type="button"
            className={`pcf-pill${channelFilter === "all" ? " is-active" : ""}`}
            onClick={() => setChannelFilter("all")}
          >
            全部可用通道 ({localShown.length})
          </button>

          {readyChannels.map((ch) => {
            const isMesh = ch.kind === "mesh";
            const isSel = channelFilter === ch.id;
            const modelsCount = ch.chatModels.length + ch.imageModels.length + ch.videoModels.length;
            return (
              <button
                key={ch.id}
                type="button"
                className={`pcf-pill${isSel ? " is-active" : ""}${isMesh ? " is-mesh" : ""}`}
                onClick={() => setChannelFilter(ch.id)}
              >
                {isMesh ? <span>☁️</span> : <span className="pod-dot is-live" />}
                <span>{ch.label}</span>
                <span className="pcf-count mono">({modelsCount})</span>
                {isMesh && ch.latencyMs ? (
                  <span className="pcf-latency mono">{ch.latencyMs}ms</span>
                ) : null}
              </button>
            );
          })}
        </div>

        <button
          type="button"
          className="btn btn-sm btn-icon btn-soft plaza-refresh-btn"
          disabled={relay.loading}
          onClick={() => void relay.reload()}
          title="刷新模型列表"
          aria-label="刷新"
        >
          <Icon name="refresh" size={13} />
        </button>
      </div>

      {/* 第二行：细粒度聚合筛选层 (类型 Tab + 右侧厂商下拉与搜索) */}
      <div className="plaza-sub-filter-bar">
        <div className="plaza-sub-left">
          {/* 能力类型分段器 */}
          <div className="tabs" role="tablist">
            <button
              type="button"
              role="tab"
              className={`tab${filter.modality === "all" ? " is-active" : ""}`}
              onClick={() => setFilter({ ...filter, modality: "all" })}
            >
              全部 {modalities.reduce((n, k) => n + k.n, 0)}
            </button>
            {modalities.map((k) => (
              <button
                key={k.id}
                type="button"
                role="tab"
                className={`tab${filter.modality === k.id ? " is-active" : ""}`}
                onClick={() => setFilter({ ...filter, modality: k.id })}
              >
                {MODALITY_LABEL[k.id]} {k.n}
              </button>
            ))}
          </div>
        </div>

        {/* 右侧：厂商下拉选择器 + 搜索框 */}
        <div className="plaza-sub-right">
          <div className="plaza-vendor-dropdown-wrap">
            <select
              className="select plaza-vendor-select"
              value={filter.vendor}
              onChange={(e) => setFilter({ ...filter, vendor: e.target.value as any })}
              aria-label="筛选厂商"
            >
              <option value="all">全部厂商 ({groups.length})</option>
              {vendors.map((v) => (
                <option key={v.vendor} value={v.vendor}>
                  {v.label} ({v.n})
                </option>
              ))}
            </select>
          </div>

          <label className="search plaza-search-input">
            <Icon name="search" size={13} />
            <input
              value={filter.query}
              placeholder="搜索模型、前缀或能力"
              onChange={(e) => setFilter({ ...filter, query: e.target.value })}
            />
            {filter.query ? (
              <button
                type="button"
                className="search-clear"
                aria-label="清空"
                onClick={() => setFilter({ ...filter, query: "" })}
              >
                <Icon name="close" size={12} />
              </button>
            ) : null}
          </label>
        </div>
      </div>

      {/* 主视区：独占全宽的三列模型卡片网格 */}
      <div className="plaza-content">
        {loading ? (
          <div className="mgrid">
            {[0, 1, 2, 3, 4, 5].map((i) => (
              <div key={i} className="card skeleton" style={{ height: 160 }} />
            ))}
          </div>
        ) : shown.length === 0 ? (
          <div className="card plaza-empty">
            <p>当前通道或筛选条件下没有可用的模型</p>
            <button
              type="button"
              className="btn btn-sm btn-primary"
              onClick={() => {
                setChannelFilter("all");
                setFilter({ query: "", vendor: "all", modality: "all" });
              }}
            >
              重置筛选条件
            </button>
          </div>
        ) : (
          <div className="mgrid">
            {shown.map((g) => (
              <div key={g.key} className="model-card-wrapper">
                <ModelCard
                  g={g}
                  onTry={(id) =>
                    g.modality === "image" || g.modality === "video"
                      ? onGo(go("playground", { view: g.modality, model: id }))
                      : setTrying(id)
                  }
                />
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
