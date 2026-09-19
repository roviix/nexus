/**
 * 通道选择：网关的几条（Cursor / ChatGPT / Grok Build / Kiro），一排卡里挑一张。
 *
 * 上一版只有一张「本地网关」卡，把四队号压成了一张，选完还得靠模型名前缀猜请求会走哪条。
 * 现在每条通道各一张卡：用户指定的默认通道接裸名，其余要写 `{通道}/模型`。
 *
 * 卡的形状：名字顶格；底部一个大号数字（能接的号数）配一枚状态胶囊。选中靠描边 + 淡底（`.pick-on`）。
 * 这张卡还没法用（网关没开、没有号）时，补齐那一步的键就压在卡的右下角。
 *
 * 地址、端口这类东西不在卡上：接入页的配置块里自然会写出来，用户不该在选通道的时候先看见 `:8787`。
 */
import type { ReactNode } from "react";
import { defaultChannelId, laneCount, localChannels, modelsOf, type LocalChannel, type LocalChannelId } from "../gateway/channels";
import type { LocalModel } from "../ipc/models";
import type { GatewayStatus } from "../ipc/types";
import { VendorLogo } from "./VendorLogo";

type ToneKey = "live" | "uneven" | "down" | "none" | "off";
const TONE_LABEL: Record<ToneKey, string> = { live: "正常", uneven: "波动", down: "不可用", none: "暂无", off: "未开启" };
const TONE_COLOR: Record<ToneKey, string> = {
  live: "var(--ok)",
  uneven: "var(--warn)",
  down: "var(--bad)",
  none: "var(--color-faint)",
  off: "var(--color-faint)",
};

/** 通道卡上的厂商标。 */
const LOCAL_VENDOR: Record<LocalChannelId, Parameters<typeof VendorLogo>[0]["vendor"]> = {
  cursor: "cursor",
  chatgpt: "openai",
  grok: "xai",
  kiro: "other",
  zcode: "zhipu",
};

export function ChannelCard({
  logo,
  name,
  pill,
  big,
  bigSuffix,
  bigColor,
  tone,
  toneLabel,
  aside,
  cta,
  on,
  disabled,
  onPick,
}: {
  logo?: ReactNode;
  name: string;
  pill?: string;
  /** 底部的大号数字；`null` 画一个「—」。 */
  big: string | null;
  bigSuffix?: string;
  bigColor?: string;
  tone: ToneKey;
  toneLabel?: string;
  /** 底部右侧的一小段：模型数。 */
  aside?: string;
  /**
   * 这张卡自己缺什么就自己要：没开的卡要「去开启」，没号的卡要「添加号」。
   * 挡在卡外面当横幅是把一句话和它指的那张卡拆到了两处。
   * 卡本身是 `<button>`，键不能套在里面 —— 用一个定位壳把它压在卡的右下角。
   */
  cta?: ReactNode;
  on: boolean;
  disabled?: boolean;
  onPick: () => void;
}) {
  const color = TONE_COLOR[tone];
  // 卡上压了键，右下角那一格就归键 —— 挤在一起谁也读不清，何况这时候真正该做的事只有一件。
  const filler = !cta;
  const card = (
    <button type="button" className={`pick${on ? " pick-on" : ""}`} onClick={onPick} disabled={disabled} aria-pressed={on}>
      {/* 按钮里只用 span：<p> / <div> 不是 button 允许的内容。 */}
      <span className="pick-head">
        <span className="pick-title">
          {logo ? <span className="pick-logo">{logo}</span> : null}
          <span className="truncate" title={name}>
            {name}
          </span>
          {pill ? <span className="pill">{pill}</span> : null}
        </span>
      </span>
      <span className="pick-foot">
        <span className="pick-big-wrap">
          {big != null ? (
            <span className="pick-big num" style={{ color: bigColor ?? "var(--color-ink)" }}>
              {big}
              {bigSuffix ? <span className="pick-big-sub">{bigSuffix}</span> : null}
            </span>
          ) : cta ? null : (
            // 没数也没键才画破折号。键都摆在那儿了，再占一格「暂无」是白占。
            <span className="pick-big num" style={{ color: "var(--color-faint)" }}>
              —
            </span>
          )}
          <span className="pill" style={{ color, borderColor: `color-mix(in oklab, ${color} 35%, var(--color-line))` }}>
            {toneLabel ?? TONE_LABEL[tone]}
          </span>
        </span>
        {aside && filler ? (
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

/** 一条通道卡上的那几个字：几个号能接、状态胶囊、模型数。 */
function localCard(ch: LocalChannel, gateway: GatewayStatus | null): { big: string | null; bigColor: string; tone: ToneKey; toneLabel?: string; aside?: string } {
  const running = Boolean(gateway?.running);
  const { usable, total } = laneCount(ch.lane);
  const models = modelsOf(ch).length;
  // 卡窄，副文只留「几个模型」；前缀这种细节放网关页和模型广场的小标题里。
  const aside = models ? `${models} 个模型` : undefined;
  if (!gateway) return { big: null, bigColor: "var(--color-faint)", tone: "none", aside };
  if (!running) return { big: String(total), bigColor: "var(--color-faint)", tone: "off", aside };
  if (total === 0) {
    // 默认通道没号是真没有（裸名会被拒）；其余没号只是「要写前缀才走这里」。
    return { big: "0", bigColor: ch.isDefault ? "var(--warn)" : "var(--color-faint)", tone: ch.isDefault ? "uneven" : "none", toneLabel: "没有号", aside };
  }
  if (usable === 0) return { big: `0/${total}`, bigColor: "var(--bad)", tone: "down", toneLabel: "号都不可用", aside };
  return { big: usable === total ? String(usable) : `${usable}/${total}`, bigColor: "var(--ok)", tone: "live", toneLabel: "可接", aside };
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
  bare,
  onStartGateway,
  onManageLocal,
}: {
  channel: string | null;
  onChange: (channel: string | null) => void;
  gateway: GatewayStatus | null;
  /** 模型目录；给了才能在卡上写模型数。 */
  local?: LocalModel[] | null;
  /** 不带「通道」面板外壳，只出一排卡：外面已经有一层分步标题时用。 */
  bare?: boolean;
  /** 给了就在卡上出「补齐这一步」的键；不给就不出（只读的地方不该催人做事）。 */
  onStartGateway?: () => void;
  /** 某条通道没有号：Cursor 去网关页加，其余去账号页对应页签。 */
  onManageLocal?: (id: LocalChannelId) => void;
}) {
  const running = Boolean(gateway?.running);
  const locals = localChannels(gateway, local);

  const cards = (
    <div className="chan-grid">
      {locals.map((ch) => {
        const c = localCard(ch, gateway);
        const { total } = laneCount(ch.lane);
        const cta =
          onStartGateway && gateway && !running ? (
            <button type="button" className="btn btn-sm" onClick={onStartGateway}>
              去开启
            </button>
          ) : onManageLocal && gateway && running && total === 0 ? (
            <button type="button" className="btn btn-sm" onClick={() => onManageLocal(ch.id)}>
              添加号
            </button>
          ) : null;
        return (
          <ChannelCard
            key={ch.id}
            logo={<VendorLogo vendor={LOCAL_VENDOR[ch.id]} size={13} />}
            name={ch.label}
            pill={ch.isDefault ? "默认" : undefined}
            big={c.big}
            bigSuffix=" 个号"
            bigColor={c.bigColor}
            tone={c.tone}
            toneLabel={c.toneLabel}
            aside={c.aside}
            cta={cta}
            on={(channel ?? defaultChannelId(gateway)) === ch.id}
            onPick={() => onChange(ch.id)}
          />
        );
      })}
    </div>
  );

  const note = !gateway ? "本机网关" : !running ? "本机网关 · 未开启" : "本机网关 · 运行中";
  const body = (
    <div className="chan-groups">
      <div className="chan-group">
        <p className="chan-group-cap">
          通道
          <span>{note}</span>
        </p>
        {cards}
      </div>
    </div>
  );

  if (bare) return body;
  return (
    <section className="chan">
      <p className="chan-cap">通道</p>
      {body}
    </section>
  );
}
