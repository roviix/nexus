/**
 * 模型广场：顶上一排通道卡（Cursor / ChatGPT / Grok Build / Kiro），中间按厂商排开的模型卡
 * （档位芯片、说明），右栏搜索与筛选。
 *
 * 通道决定下面整页：选某一条，卡上是**这条通道**此刻能接的模型 id。
 *
 * 「试一下」长在每张模型卡上，不另设一个页头入口：一个不知道要试哪个模型的「试一下」
 * 只能随手挑第一个，那不是用户想按的键。
 */
import { useEffect, useMemo, useState } from "react";
import { channelOfModel, localChannels, type LocalChannelId } from "../gateway/channels";
import type { Modality } from "../ipc/models";
import { ChannelPicker } from "../relay/ChannelPicker";
import { ModelCard } from "../relay/ModelCard";
import { useRelay } from "../relay/useRelay";
import { VendorLogo } from "../relay/VendorLogo";
import { go, type Route } from "../shell/nav";
import { filterCards, groupLocal, modalityRail, vendorRail, type CardFilter } from "../ui/models";
import { Icon } from "../ui/primitives";
import { TryDrawer, type TryLane } from "./models/TryDrawer";

const MODALITY_LABEL: Record<Modality, string> = { chat: "对话", image: "图片", video: "视频" };

export function ModelsPage({ route, onGo }: { route: Route; onGo: (r: Route) => void }) {
  const relay = useRelay({ catalogs: true });
  /** 哪一条通道（平台 id）；null 按默认（Cursor）。 */
  const [channel, setChannel] = useState<string | null>(route.channel ?? null);
  const [filter, setFilter] = useState<CardFilter>({ query: "", vendor: "all", modality: "all" });
  /** 打开「试一下」时预选的模型；null = 抽屉关着。 */
  const [trying, setTrying] = useState<string | null>(null);

  useEffect(() => {
    if (route.channel) setChannel(route.channel);
  }, [route.channel]);

  const channels = useMemo(() => localChannels(relay.gateway, relay.local), [relay.gateway, relay.local]);
  const localId: LocalChannelId = (channel as LocalChannelId | null) ?? "cursor";
  /** 目录里归当前那条通道的模型。 */
  const localShown = useMemo(() => (relay.local ?? []).filter((m) => channelOfModel(channels, m.id) === localId), [relay.local, channels, localId]);
  const activeLocal = channels.find((c) => c.id === localId) ?? channels[0];

  const groups = useMemo(() => groupLocal(localShown), [localShown]);
  const shown = useMemo(() => filterCards(groups, filter), [groups, filter]);
  const vendors = useMemo(() => vendorRail(groups, filter), [groups, filter]);
  const modalities = useMemo(() => modalityRail(groups, filter), [groups, filter]);
  const tiers = shown.reduce((n, g) => n + g.variants.length, 0);

  const localIds = useMemo(() => (relay.local ?? []).map((m) => m.id), [relay.local]);

  const lane: TryLane = {
    ready: Boolean(relay.gateway?.running),
    blocker: relay.gateway?.running
      ? undefined
      : { text: "本地网关没开，试不了", hint: "试用走的就是网关自己的地址与口令，得先把它开起来。", fix: () => onGo(go("gateway")), fixLabel: "去开启" },
    modelIds: localIds,
    foot: relay.gateway?.running ? `本地网关${relay.gateway.lane.current ? ` · Cursor 当前号 ${relay.gateway.lane.current}` : ""}` : "本地网关未开启",
  };

  const loading = relay.local == null;

  /** 某条通道一个模型都没有时说清为什么：没号 / 网关没开。 */
  const emptyNote =
    !loading && groups.length === 0 && activeLocal
      ? activeLocal.isDefault
        ? "Cursor 通道的目录是空的。"
        : `${activeLocal.label} 通道此刻没有号能接，它的模型不会出现在目录里。去「账号 → ${activeLocal.label}」加一个号。`
      : null;

  return (
    <div>
      <div className="page-head">
        <h1>模型广场</h1>
        <div className="row" style={{ gap: 8 }}>
          <button type="button" className="btn btn-sm btn-icon btn-soft" disabled={relay.loading} onClick={() => void relay.reload()} title="刷新" aria-label="刷新">
            <Icon name="refresh" size={13} />
          </button>
        </div>
      </div>

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

      <div className="plaza">
        <div className="plaza-main">
          <ChannelPicker channel={channel} onChange={setChannel} gateway={relay.gateway} local={relay.local} />

          <div>
            <p className="chan-cap">
              模型
              {shown.length ? <span className="chan-cap-n num">{shown.length}</span> : null}
              {activeLocal && !activeLocal.isDefault && activeLocal.prefixes[0] ? (
                <span className="chan-cap-n">
                  · 加 <code className="mono">{activeLocal.prefixes[0]}</code> 前缀可强制走这条通道
                </span>
              ) : null}
            </p>

            {loading ? (
              <div className="mgrid">
                {[0, 1, 2, 3].map((i) => (
                  <div key={i} className="card skeleton" style={{ height: 196 }} />
                ))}
              </div>
            ) : shown.length === 0 ? (
              <div className="card plaza-empty">
                <p>{groups.length === 0 ? emptyNote ?? "目录是空的。" : "没有匹配的模型"}</p>
                {groups.length === 0 && activeLocal && !activeLocal.isDefault ? (
                  <button type="button" className="btn btn-sm" onClick={() => onGo(go("accounts", { platform: activeLocal.id }))}>
                    去添加 {activeLocal.label} 账号
                  </button>
                ) : null}
              </div>
            ) : (
              <div className="mgrid">
                {shown.map((g) => (
                  <ModelCard
                    key={g.key}
                    g={g}
                    // 生图模型没有「说一句话」可试：直接带进游乐场的图片会话。
                    onTry={(id) => (g.modality === "image" || g.modality === "video" ? onGo(go("playground", { view: g.modality, model: id })) : setTrying(id))}
                    onConnect={(id) => onGo(go("connect", { channel: channel ?? undefined, model: id }))}
                  />
                ))}
              </div>
            )}
          </div>
        </div>

        {/* 右栏只管「看哪几张卡」：搜索、类别、厂商。sticky 让它在长列表里一直够得着。 */}
        <aside className="plaza-rail">
          <div className="plaza-rail-in">
            <label className="search" style={{ maxWidth: "none", height: 32 }}>
              <Icon name="search" size={13} />
              <input value={filter.query} placeholder="搜索模型" onChange={(e) => setFilter({ ...filter, query: e.target.value })} />
              {filter.query ? (
                <button type="button" className="search-clear" aria-label="清空" onClick={() => setFilter({ ...filter, query: "" })}>
                  <Icon name="close" size={12} />
                </button>
              ) : null}
            </label>

            {modalities.length > 1 ? (
              <div>
                <p className="rail-cap">类别</p>
                <nav className="rail-list">
                  <FilterRow label="全部" meta={String(modalities.reduce((n, k) => n + k.n, 0))} on={filter.modality === "all"} onClick={() => setFilter({ ...filter, modality: "all" })} />
                  {modalities.map((k) => (
                    <FilterRow key={k.id} label={MODALITY_LABEL[k.id]} meta={String(k.n)} on={filter.modality === k.id} onClick={() => setFilter({ ...filter, modality: k.id })} />
                  ))}
                </nav>
              </div>
            ) : null}

            <div>
              <p className="rail-cap">厂商</p>
              <nav className="rail-list">
                <FilterRow label="全部" meta={String(vendors.reduce((n, v) => n + v.n, 0))} on={filter.vendor === "all"} onClick={() => setFilter({ ...filter, vendor: "all" })} />
                {vendors.map((v) => (
                  <FilterRow key={v.vendor} vendor={v.vendor} label={v.label} meta={String(v.n)} on={filter.vendor === v.vendor} onClick={() => setFilter({ ...filter, vendor: v.vendor })} />
                ))}
              </nav>
            </div>

            <p className="rail-count num">
              {shown.length} 个模型 · {tiers} 个档位
            </p>
          </div>
        </aside>
      </div>
    </div>
  );
}

/**
 * 右栏的一行筛选项，类别和厂商共用。整行可点的列表项而不是芯片：竖排的芯片会因为
 * 名字长短不一而右边缘参差，整行铺满则天然对齐，右侧那一列数字也能对齐成一列。
 */
function FilterRow({ vendor, label, meta, on, onClick }: { vendor?: Parameters<typeof VendorLogo>[0]["vendor"]; label: string; meta: string; on: boolean; onClick: () => void }) {
  return (
    <button type="button" className={`rail-row${on ? " is-on" : ""}`} onClick={onClick} title={label}>
      {vendor ? <VendorLogo vendor={vendor} size={13} /> : null}
      <span className="truncate">{label}</span>
      <span className="rail-meta num">{meta}</span>
    </button>
  );
}
