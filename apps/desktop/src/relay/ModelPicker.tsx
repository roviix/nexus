/**
 * 可输入的模型下拉。
 *
 * 不用原生 `<select>`：目录外的新 id 得留一条手输的路（中转不校验模型名）；
 * 也不用 `<datalist>`：它没有可见的展开入口，多数环境还要先打字才弹候选。
 * 只在正在打字时把输入当筛选词 —— 否则点开箭头想浏览列表时，框里那个完整的 id
 * 会把候选筛得只剩它自己。
 */
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Icon } from "../ui/primitives";

export interface PickerOption {
  id: string;
  /** 右侧的一小段补充（价格 / 别名数 / 状态）。 */
  meta?: ReactNode;
  /** 不推荐（维护中之类）：压暗，但仍可选。 */
  dim?: boolean;
}

export function ModelPicker({
  value,
  onChange,
  options,
  disabled,
  placeholder = "选一个模型，或直接输入 id",
}: {
  value: string;
  onChange: (v: string) => void;
  options: PickerOption[];
  disabled?: boolean;
  placeholder?: string;
}) {
  const [open, setOpen] = useState(false);
  const [typing, setTyping] = useState(false);
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

  const kw = typing ? value.trim().toLowerCase() : "";
  const hit = options.filter((o) => o.id.toLowerCase().includes(kw));
  // 输入的是目录外的 id 时不要给一个空列表，仍然把全部候选摆出来。
  const list = kw && hit.length ? hit : options;

  return (
    <div ref={box} className="mpick">
      <input
        className="input mono"
        value={value}
        disabled={disabled}
        placeholder={placeholder}
        spellCheck={false}
        onChange={(e) => {
          onChange(e.target.value);
          setTyping(true);
          setOpen(true);
        }}
        onFocus={() => {
          setTyping(false);
          setOpen(true);
        }}
      />
      <button
        type="button"
        className="mpick-toggle"
        aria-label={open ? "收起" : "展开"}
        disabled={disabled}
        onClick={() => {
          setTyping(false);
          setOpen((v) => !v);
        }}
      >
        <Icon name="chevron" size={13} style={{ transform: open ? "rotate(-90deg)" : "rotate(90deg)" }} />
      </button>
      {open && list.length ? (
        <div className="mpick-pop" role="listbox">
          {list.map((o) => (
            <button
              key={o.id}
              type="button"
              role="option"
              aria-selected={o.id === value}
              className={`mpick-opt${o.id === value ? " is-on" : ""}${o.dim ? " is-dim" : ""}`}
              onClick={() => {
                onChange(o.id);
                setTyping(false);
                setOpen(false);
              }}
            >
              <span className="mono truncate">{o.id}</span>
              {o.meta ? <span className="mpick-meta">{o.meta}</span> : null}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}
