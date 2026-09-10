/**
 * 药丸形的下拉：一个「标签：当前值 ▾」按钮，点开一列候选。
 *
 * 不用原生 `<select>`：WebView 里它长得像表单控件，和这一页工具条上别的药丸不是一家人；
 * 而且原生下拉没法在候选右边挂计数 / 状态。弹层沿用 `ModelPicker` 那套 `.mpick-*` 类，
 * 两种下拉在同一条工具条上开出来才是同一副样子。
 */
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Icon } from "../../ui/primitives";

export interface PillOption<T extends string | null> {
  id: T;
  label: string;
  meta?: ReactNode;
}

export function PillMenu<T extends string | null>({
  label,
  value,
  options,
  onChange,
  disabled,
  icon,
  align = "left",
}: {
  /** 药丸左半边的小标签（「密钥」「模型」）。 */
  label?: string;
  value: T;
  options: PillOption<T>[];
  onChange: (v: T) => void;
  disabled?: boolean;
  icon?: ReactNode;
  /** 弹层对齐到药丸的哪一边。工具条右端的药丸要往左开，不然弹出屏外。 */
  align?: "left" | "right";
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
    // 捕获阶段：Tauri 的拖拽脚本在 document 上先挂了 mousedown，点到拖拽区时会
    // stopImmediatePropagation，冒泡阶段的这一个就收不到，弹层会一直开着。
    document.addEventListener("mousedown", onDown, true);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDown, true);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const current = options.find((o) => o.id === value);

  return (
    <div ref={box} className={`pg-pill${open ? " is-open" : ""}`}>
      <button type="button" className="pg-pill-btn" disabled={disabled || !options.length} aria-haspopup="listbox" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        {icon}
        {label ? <span className="pg-pill-k">{label}</span> : null}
        <span className="pg-pill-v truncate">{current?.label ?? "—"}</span>
        <Icon name="chevron" size={12} className="pg-pill-chev" />
      </button>
      {open ? (
        <div className={`mpick-pop pg-pill-pop${align === "right" ? " is-right" : ""}`} role="listbox">
          {options.map((o) => (
            <button
              key={String(o.id)}
              type="button"
              role="option"
              aria-selected={o.id === value}
              aria-label={o.label}
              className={`mpick-opt${o.id === value ? " is-on" : ""}`}
              onClick={() => {
                onChange(o.id);
                setOpen(false);
              }}
            >
              <span className="truncate">{o.label}</span>
              {o.meta ? <span className="mpick-meta">{o.meta}</span> : null}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}
