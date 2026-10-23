/**
 * 通道选择：网关的几条（Cursor / ChatGPT / Grok Build / Kiro / ZCode / Qoder / Claude / 供应商），一排卡里挑一张。
 *
 * 上一版只有一张「本地网关」卡，把四队号压成了一张，选完还得靠模型名前缀猜请求会走哪条。
 * 现在每条通道各一张卡：用户指定的默认通道接裸名，其余要写 `{通道}/模型`。
 *
 * 卡的形状：名字顶格；底部一个大号数字（能接的号数）配一枚状态胶囊。选中靠描边 + 淡底（`.pick-on`）。
 * 这张卡还没法用（网关没开、没有号）时，补齐那一步的键就压在卡的右下角。
 *
 * 地址、端口这类东西不在卡上：接入页的配置块里自然会写出来，用户不该在选通道的时候先看见 `:8787`。
 */
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { defaultChannelId, laneCount, localChannels, modelsOf, type LocalChannel, type LocalChannelId } from "../gateway/channels";
import type { LocalModel } from "../ipc/models";
import type { GatewayStatus, MeshPeer } from "../ipc/types";
import { Icon } from "../ui/primitives";
import { VendorLogo } from "./VendorLogo";

type ToneKey = "live" | "uneven" | "down" | "none" | "off";

/** 通道卡上的标。供应商画钥匙，Mesh 画网络节点。 */
export function ChannelLogo({ id, size = 13 }: { id: string; size?: number }) {
  if (id === "provider") return <Icon name="key" size={size} />;
  if (id === "mesh") return <Icon name="mesh" size={size} />;
  const vendor: Record<string, Parameters<typeof VendorLogo>[0]["vendor"]> = {
    cursor: "cursor",
    chatgpt: "openai",
    grok: "xai",
    kiro: "kiro",
    zcode: "zhipu",
    qoder: "qoder",
    claude: "anthropic",
    antigravity: "google",
    "gemini-web": "google",
  };
  const v = vendor[id];
  if (v) return <VendorLogo vendor={v} size={size} />;
  return <Icon name="mesh" size={size} />;
}

export function ChannelCard({
  logo,
  name,
  pill,
  statusText,
  tone,
  aside,
  cta,
  on,
  disabled,
  onPick,
}: {
  logo?: ReactNode;
  name: string;
  pill?: string;
  statusText: string;
  tone: ToneKey;
  /** 底部右侧的一小段：模型数。 */
  aside?: string;
  cta?: ReactNode;
  on: boolean;
  disabled?: boolean;
  onPick: () => void;
}) {
  const card = (
    <button type="button" className={`pick${on ? " pick-on" : ""}`} onClick={onPick} disabled={disabled} aria-pressed={on}>
      <span className="pick-head">
        <span className="pick-title">
          {logo ? <span className="pick-logo">{logo}</span> : null}
          <span className="truncate" title={name}>
            {name}
          </span>
        </span>
        {pill ? <span className="pill pick-badge">{pill}</span> : null}
      </span>
      <span className="pick-foot">
        <span className="pick-status-wrap">
          <span className={`pick-dot is-${tone}`} />
          <span className="pick-status-text">{statusText}</span>
        </span>
        {aside && !cta ? (
          <span className="pick-aside num truncate" title={aside}>
            {aside}
          </span>
        ) : null}
      </span>
    </button>
  );

  if (!cta) return card;
  return (
    <div className="pick-slot">
      {card}
      <span className="pick-cta">{cta}</span>
    </div>
  );
}

/** 一条通道卡上的文字与状态：几个号能接、状态、模型数。 */
function localCard(ch: LocalChannel, gateway: GatewayStatus | null, peer?: MeshPeer): { statusText: string; tone: ToneKey; aside?: string } {
  if (ch.vendor === "nexus-mesh") {
    const models = modelsOf(ch).length;
    const aside = peer?.latency_ms ? `${peer.latency_ms}ms` : models ? `${models} 模型` : undefined;
    return {
      statusText: `${models} 模型`,
      tone: "live",
      aside,
    };
  }
  const running = Boolean(gateway?.running);
  const { usable, total } = laneCount(ch.lane);
  const models = modelsOf(ch).length;
  const aside = models ? `${models} 模型` : undefined;
  if (ch.unavailableReason) {
    return { statusText: "对话停用", tone: "down", aside };
  }
  if (!gateway || !running) {
    return { statusText: total > 0 ? `${total} 账号` : "未开启", tone: "off", aside };
  }
  if (total === 0) {
    return { statusText: ch.id === "mesh" ? "未连接" : "未添加", tone: ch.isDefault ? "uneven" : "none", aside };
  }
  if (usable === 0) {
    return { statusText: `${total} 个不可用`, tone: "down", aside };
  }
  return {
    statusText: usable === total ? `${usable} 可用` : `${usable}/${total} 可用`,
    tone: "live",
    aside,
  };
}

/**
 * 精致的下拉通道选择器 (ChannelSelect)。
 * 可在接入页、模型广场工具栏中紧凑渲染，支持分组（本地通道 / Mesh 远端节点）。
 */
export function ChannelSelect({
  channel,
  onChange,
  gateway,
  local,
  peers,
  disabled,
  style,
  className,
  filterUnavailable,
}: {
  channel: string | null;
  onChange: (channel: string) => void;
  gateway: GatewayStatus | null;
  local?: LocalModel[] | null;
  peers?: MeshPeer[];
  disabled?: boolean;
  style?: React.CSSProperties;
  className?: string;
  filterUnavailable?: boolean;
}) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!box.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDown, true);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown, true);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const locals = useMemo(() => {
    const list = localChannels(gateway, local, peers);
    if (!filterUnavailable) return list;
    return list.filter((c) => !c.unavailableReason && c.id !== "cursor");
  }, [gateway, local, peers, filterUnavailable]);

  const selectedId = (channel ?? defaultChannelId(gateway, locals)).toLowerCase();
  const current = locals.find((c) => c.id.toLowerCase() === selectedId) ?? locals[0];
  const activePeer = (peers ?? []).find(
    (p) => p.device_name.toLowerCase() === selectedId || p.node_id === channel,
  );
  const currentCard = current ? localCard(current, gateway, activePeer) : null;

  const localList = locals.filter((c) => c.vendor !== "nexus-mesh");
  const meshList = locals.filter((c) => c.vendor === "nexus-mesh");

  return (
    <div ref={box} className={`cselect${open ? " is-open" : ""}${className ? ` ${className}` : ""}`} style={style}>
      <button
        type="button"
        className="cselect-btn"
        disabled={disabled || !locals.length}
        aria-haspopup="listbox"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        <div className="cselect-left">
          {current ? (
            <span className="cselect-logo">
              <ChannelLogo id={current.id} size={15} />
            </span>
          ) : null}
          <span className="cselect-title">{current?.label ?? "选择通道"}</span>
          {current?.isDefault ? <span className="pill tiny">默认</span> : null}
          {activePeer ? (
            <span className="pill pill-session tiny" style={{ color: "var(--accent)" }}>
              Mesh · {activePeer.latency_ms ? `${activePeer.latency_ms}ms` : "在线"}
            </span>
          ) : null}
        </div>
        <div className="cselect-right">
          {currentCard ? (
            <span className="row" style={{ gap: 6, alignItems: "center" }}>
              <span className={`pick-dot is-${currentCard.tone}`} />
              <span className="pick-status-text tiny">{currentCard.statusText}</span>
            </span>
          ) : null}
          <Icon name="chevron" size={12} className="cselect-chev" />
        </div>
      </button>

      {open ? (
        <div className="cselect-pop" role="listbox">
          {localList.length ? (
            <>
              {meshList.length > 0 ? <div className="cselect-section">本地网关通道</div> : null}
              {localList.map((ch) => {
                const c = localCard(ch, gateway);
                const isSelected = ch.id.toLowerCase() === selectedId;
                return (
                  <button
                    key={ch.id}
                    type="button"
                    role="option"
                    aria-selected={isSelected}
                    className={`cselect-opt${isSelected ? " is-on" : ""}`}
                    onClick={() => {
                      onChange(ch.id);
                      setOpen(false);
                    }}
                  >
                    <div className="cselect-opt-left">
                      <span className="cselect-logo">
                        <ChannelLogo id={ch.id} size={14} />
                      </span>
                      <span className="truncate">{ch.label}</span>
                      {ch.isDefault ? <span className="pill tiny">默认</span> : null}
                    </div>
                    <div className="cselect-opt-meta">
                      <span className={`pick-dot is-${c.tone}`} />
                      <span>{c.statusText}</span>
                      {isSelected ? <Icon name="check" size={13} style={{ color: "var(--accent)" }} /> : null}
                    </div>
                  </button>
                );
              })}
            </>
          ) : null}

          {meshList.length > 0 ? (
            <>
              <div className="cselect-section" style={{ marginTop: 4 }}>Mesh 远端节点</div>
              {meshList.map((ch) => {
                const peer = (peers ?? []).find((p) => p.device_name.toLowerCase() === ch.id.toLowerCase());
                const c = localCard(ch, gateway, peer);
                const isSelected = ch.id.toLowerCase() === selectedId || (peer && channel === peer.node_id);
                return (
                  <button
                    key={ch.id}
                    type="button"
                    role="option"
                    aria-selected={isSelected}
                    className={`cselect-opt${isSelected ? " is-on" : ""}`}
                    onClick={() => {
                      onChange(ch.id);
                      setOpen(false);
                    }}
                  >
                    <div className="cselect-opt-left">
                      <span className="cselect-logo">
                        <ChannelLogo id={ch.id} size={14} />
                      </span>
                      <span className="truncate">{ch.label}</span>
                      <span className="pill pill-session tiny" style={{ color: "var(--accent)" }}>
                        {peer?.latency_ms ? `${peer.latency_ms}ms` : "Mesh 远端"}
                      </span>
                    </div>
                    <div className="cselect-opt-meta">
                      <span className={`pick-dot is-${c.tone}`} />
                      <span>{c.statusText}</span>
                      {isSelected ? <Icon name="check" size={13} style={{ color: "var(--accent)" }} /> : null}
                    </div>
                  </button>
                );
              })}
            </>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/**
 * 一排通道卡 + 「通道」小标题。模型广场和接入页共用。
 *
 * `channel` 是平台 id（`cursor` / `chatgpt` …）；`null` 按用户指定的默认通道算。
 */
export function ChannelPicker({
  channel,
  onChange,
  gateway,
  local,
  peers,
  bare,
  plain,
  onStartGateway,
  onManageLocal,
}: {
  channel: string | null;
  onChange: (channel: string | null) => void;
  gateway: GatewayStatus | null;
  /** 模型目录；给了才能在卡上写模型数。 */
  local?: LocalModel[] | null;
  /** 已连接的 Mesh 远端节点列表，动态增减展示独立通道卡。 */
  peers?: MeshPeer[];
  /** 不带「通道」面板外壳，只出一排卡：外面已经有一层分步标题时用。 */
  bare?: boolean;
  /** 连「通道 · 本机网关」那行小标题也不要：外面那一行已经写着「通道」。 */
  plain?: boolean;
  /** 给了就在卡上出「补齐这一步」的键；不给就不出（只读的地方不该催人做事）。 */
  onStartGateway?: () => void;
  /** 某条通道没有号：Cursor 去网关页加，其余去账号页对应页签。 */
  onManageLocal?: (id: LocalChannelId) => void;
}) {
  const running = Boolean(gateway?.running);
  const locals = localChannels(gateway, local, peers);

  const cards = (
    <div className="chan-grid">
      {locals.map((ch) => {
        const peer = (peers ?? []).find((p) => p.device_name.toLowerCase() === ch.id.toLowerCase());
        const c = localCard(ch, gateway, peer);
        const { total } = laneCount(ch.lane);
        const isPeer = Boolean(peer);
        const cta =
          onStartGateway && gateway && !running && !isPeer ? (
            <button type="button" className="btn btn-sm btn-quiet" onClick={(e) => { e.stopPropagation(); onStartGateway(); }}>
              去开启
            </button>
          ) : onManageLocal && gateway && !ch.unavailableReason && total === 0 && !isPeer ? (
            <button type="button" className="btn btn-sm btn-quiet" onClick={(e) => { e.stopPropagation(); onManageLocal(ch.id); }}>
              {ch.id === "provider" ? "添加" : ch.id === "mesh" ? "连接节点" : "添加号"}
            </button>
          ) : null;
        const isSelected = Boolean(
          (channel ?? defaultChannelId(gateway)).toLowerCase() === ch.id.toLowerCase() ||
          (peer && channel === peer.node_id),
        );
        return (
          <ChannelCard
            key={ch.id}
            logo={<ChannelLogo id={ch.id} />}
            name={ch.label}
            pill={ch.isDefault ? "默认" : isPeer ? "Mesh 远端" : undefined}
            statusText={c.statusText}
            tone={c.tone}
            aside={c.aside}
            cta={cta}
            on={isSelected}
            onPick={() => onChange(ch.id)}
          />
        );
      })}
    </div>
  );

  if (plain) return cards;
  const note = !gateway ? "未连接" : !running ? "未开启" : "运行中";
  const body = (
    <div className="chan-groups">
      <div className="chan-group">
        <div className="row-between chan-group-cap" style={{ alignItems: "baseline" }}>
          <span className="row" style={{ gap: 8, alignItems: "baseline" }}>
            <strong>通道选择</strong>
            <span className="muted tiny">本机网关 · {note}</span>
          </span>
        </div>
        {cards}
      </div>
    </div>
  );

  if (bare) return body;
  return (
    <section className="chan">
      {body}
    </section>
  );
}
