/**
 * 模型广场的一张卡：卡头（厂商标 · 等宽标题）→ 说明 → 底部一排档位芯片 + 试用 / 接入入口。
 * 路由别名只参与搜索，不上卡。
 */
import { useEffect, useRef, useState } from "react";
import type { Modality } from "../ipc/models";
import { ShellIcon } from "../shell/ShellIcon";
import type { ModelCardGroup } from "../ui/models";
import { Icon } from "../ui/primitives";
import { VendorLogo } from "./VendorLogo";

const MODALITY_LABEL: Record<Modality, string> = { chat: "对话", image: "图片", video: "视频" };

export function ModelCard({
  g,
  onTry,
  onConnect,
}: {
  g: ModelCardGroup;
  onTry: (id: string) => void;
  onConnect: (id: string) => void;
}) {
  const [copied, setCopied] = useState("");
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  async function copy(id: string) {
    await navigator.clipboard.writeText(id).catch(() => {});
    setCopied(id);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(""), 1500);
  }

  const chat = g.modality === "chat";
  const primary = g.variants[0]?.id ?? g.title;

  return (
    <div className="card mcard">
      <div className="mcard-head">
        <VendorLogo vendor={g.vendor} size={15} className="mcard-logo" />
        {chat ? (
          <span className="mono mcard-title truncate" title={g.title}>
            {g.title}
          </span>
        ) : (
          // 出图 / 视频没有档位，底部那排芯片对它们会退化成一枚和标题一字不差的重复标签，
          // 所以复制入口直接做在标题上。
          <button type="button" className={`mcard-title-btn${copied === g.title ? " is-on" : ""}`} onClick={() => void copy(g.title)} title={`复制模型名 ${g.title}`}>
            <span className="mono mcard-title truncate">{g.title}</span>
            <Icon name={copied === g.title ? "check" : "copy"} size={12} />
          </button>
        )}
        {!chat ? <span className="pill">{MODALITY_LABEL[g.modality]}</span> : null}
        {/* 两个动作放在标题行的最右：档位那一排要整行的宽度 —— 和它们挤在一起时，
            `thinking-max-fast` 这种长档位会被挤到滑出去，看着像卡片坏了。
            挂在标题行上还有一个好处：一列卡片的动作落在同一个横坐标，扫得到。 */}
        <span className="mcard-acts">
          <button type="button" className="ibtn tip-end" data-tip="经本地网关试一下" aria-label="试一下" onClick={() => onTry(primary)}>
            <ShellIcon name="flask" size={13} />
          </button>
          <button type="button" className="ibtn tip-end" aria-label="用它接入" onClick={() => onConnect(primary)}>
            <ShellIcon name="plug" size={13} />
          </button>
        </span>
      </div>

      {/* mt-auto 让同一行的卡片底部对齐，不会因为多一条说明就长短不一。 */}
      <div className="mcard-tail">
        {g.note ? <p className="mcard-note">{g.note}</p> : null}
        {/* 出图 / 视频没有档位，这一排整个不出现 —— 空着的话卡片底下会多一条什么都没有的带子。 */}
        {chat && g.variants.length > 0 ? (
          <div className="mcard-tiers">
            <div className="scroll-x tiers">
              {g.variants.map((v) => {
                const on = copied === v.id;
                return (
                  <button key={v.id} type="button" className={`tier${on ? " tier-on" : ""}`} onClick={() => void copy(v.id)} title={`复制 ${v.id}`}>
                    <span className="mono">{v.variant}</span>
                    <Icon name={on ? "check" : "copy"} size={11} className="tier-copy" />
                  </button>
                );
              })}
            </div>
          </div>
        ) : null}
      </div>
    </div>
  );
}
