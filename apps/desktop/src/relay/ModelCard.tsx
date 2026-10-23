/**
 * 模型广场精密卡片规范：三段式钛白卡片 + 四宫格价格槽（Nexus 2.0 §4.3）。
 * 顶部模型名主次分明（弱化前缀，突出模型名）
 * 中部四列等宽价格槽（绝不折行，图片/文本模型统一高度）
 * 底部档位芯片 + 试一下 / 指派明确操作按钮。
 */
import { useEffect, useRef, useState } from "react";
import type { Modality } from "../ipc/models";
import { formatPrice, resolveModelPrice } from "../gateway/pricing";
import { ShellIcon } from "../shell/ShellIcon";
import type { ModelCardGroup } from "../ui/models";
import { Icon } from "../ui/primitives";
import { VendorLogo } from "./VendorLogo";

const MODALITY_LABEL: Record<Modality, string> = { chat: "对话", image: "图片", video: "视频" };

export function ModelCard({
  g,
  onTry,
}: {
  g: ModelCardGroup;
  onTry: (id: string) => void;
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
  const pricing = resolveModelPrice(primary);

  // 拆分通道前缀与模型真实名，主次分明
  const slashIndex = g.title.indexOf("/");
  const prefix = slashIndex !== -1 ? g.title.slice(0, slashIndex + 1) : "";
  const name = slashIndex !== -1 ? g.title.slice(slashIndex + 1) : g.title;

  return (
    <div className="card model-card">
      {/* 1. 顶部：左侧图标 + 前缀与高亮模型名，右侧复制完整路由 */}
      <div className="mc-header">
        <div className="mc-title-group truncate">
          <VendorLogo vendor={g.vendor} size={16} className="mcard-logo" />
          <div className="mc-name-col truncate">
            <span className="mono mc-name-row truncate" title={primary}>
              {prefix ? <span className="mc-prefix">{prefix}</span> : null}
              <span className="mc-name">{name}</span>
            </span>
          </div>
          {!chat ? <span className="pill tiny mono mc-modality-pill">{MODALITY_LABEL[g.modality]}</span> : null}
        </div>
        <button
          type="button"
          className={`mc-icon-btn${copied === primary ? " is-copied" : ""}`}
          onClick={() => void copy(primary)}
          title={`点击复制完整模型名 ${primary}`}
          aria-label="复制模型名"
        >
          <Icon name={copied === primary ? "check" : "copy"} size={13} />
        </button>
      </div>

      {/* 2. 中部：精密价格矩阵 (永不折行，直接带上计价单位) */}
      <div className={`mc-price-matrix${pricing.mode === "image" ? " is-image" : ""}`}>
        {pricing.mode === "image" ? (
          <>
            <div className="price-cell">
              <span className="p-label">计费模式</span>
              <span className="p-val">按次生成</span>
            </div>
            <div className="price-cell">
              <span className="p-label">单价</span>
              <span className="p-val mono">${formatPrice(pricing.usdPerImage?.default ?? 0.04)} <span className="p-unit">/ 张</span></span>
            </div>
          </>
        ) : (
          <>
            <div className="price-cell">
              <span className="p-label">Input</span>
              <span className="p-val mono">${formatPrice(pricing.inputUsdPerMtok)} <span className="p-unit">/ 1M</span></span>
            </div>
            <div className="price-cell">
              <span className="p-label">Output</span>
              <span className="p-val mono">${formatPrice(pricing.outputUsdPerMtok)} <span className="p-unit">/ 1M</span></span>
            </div>
            <div className="price-cell">
              <span className="p-label">Cache Read</span>
              <span className="p-val mono">{pricing.cacheReadUsdPerMtok !== undefined ? <>${formatPrice(pricing.cacheReadUsdPerMtok)} <span className="p-unit">/ 1M</span></> : "—"}</span>
            </div>
            <div className="price-cell">
              <span className="p-label">Cache Write</span>
              <span className="p-val mono">{pricing.cacheWriteUsdPerMtok !== undefined ? <>${formatPrice(pricing.cacheWriteUsdPerMtok)} <span className="p-unit">/ 1M</span></> : "—"}</span>
            </div>
          </>
        )}
      </div>

      {/* 3. 底部：左侧档位胶囊，右侧明确的操作按钮 */}
      <div className="mc-footer">
        <div className="mc-footer-left truncate">
          {chat && g.variants.length > 0 ? (
            g.variants.slice(0, 2).map((v) => {
              const on = copied === v.id;
              return (
                <button
                  key={v.id}
                  type="button"
                  className={`tier-pill${on ? " is-on" : ""}`}
                  onClick={() => void copy(v.id)}
                  title={`复制档位 ${v.id}`}
                >
                  <span className="mono">{v.variant}</span>
                  <Icon name={on ? "check" : "copy"} size={10} />
                </button>
              );
            })
          ) : (
            <span className="tier-pill default mono">standard</span>
          )}
        </div>

        <div className="mc-actions">
          <button
            type="button"
            className="mc-btn"
            onClick={() => onTry(primary)}
            title="在调试台中测试该模型"
          >
            <ShellIcon name="flask" size={11} />
            <span>试一下</span>
          </button>
        </div>
      </div>
    </div>
  );
}
