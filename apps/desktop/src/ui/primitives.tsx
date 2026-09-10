/** 共用小组件。刻意保持薄：它们只封装样式与一两条判断，不藏业务逻辑。 */
import type { CSSProperties, ReactNode } from "react";
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { errorText, isAppError } from "../ipc/api";
import type { AppError } from "../ipc/types";
import { meterColor, meterWidth, pctText, shortDateTime, untilText } from "./usage";

/**
 * 浮层一律挂到 `<body>` 上，**不能留在页面的 DOM 里**。
 *
 * 踩过的坑：抽屉的高度会跟着页面内容长短变 —— 账号多到出滚动条时抽屉拖得老长，
 * 切号池只有几个号时又矮得不像话。原因不在抽屉自己身上：`.main > *`（每个页面的根节点）
 * 挂着 `page-in` 那个入场动效，而它的关键帧里有 `transform`。**有 transform 的祖先会
 * 成为 `position: fixed` 后代的包含块** —— 于是 `.scrim` 的 `inset: 0` 量的不是视口，
 * 是那个页面节点的高度，也就是全部内容的高度。
 *
 * 顺带还解决一件事：`.main > *` 有 `position: relative; z-index: 1`，自成一个层叠上下文，
 * 浮层写多大的 z-index 都爬不出去。挂到 body 上之后这两个问题一起没了。
 */
function Portal({ children }: { children: ReactNode }) {
  // SSR 没有 document；桌面端不会走到，但别让这个组件成为将来的地雷。
  if (typeof document === "undefined") return null;
  return createPortal(children, document.body);
}

/* ── 浮层的 Esc ─────────────────────────────────────────────────────────────
   抽屉上面还能再开一个弹窗（详情里点「授权」）。Esc 只该关最上面那一层 ——
   两层都各自监听 window 的话，一下 Esc 会把两层一起关掉，用户就直接掉回列表了。
   所以浮层按挂载顺序登记，只有栈顶那个响应。 */

interface Layer {
  close: () => void;
}

const layers: Layer[] = [];

export function useEscapeToClose(onClose: () => void) {
  // 登记的是一个每实例固定的令牌，不是 onClose 本身：onClose 多半是内联箭头函数，
  // 每次渲染都换；若拿它当身份，底下那层一重渲染就会把自己顶到栈顶。
  const token = useRef<Layer>({ close: onClose });
  token.current.close = onClose;
  useEffect(() => {
    const t = token.current;
    layers.push(t);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && layers[layers.length - 1] === t) t.close();
    };
    window.addEventListener("keydown", onKey);
    return () => {
      const i = layers.lastIndexOf(t);
      if (i >= 0) layers.splice(i, 1);
      window.removeEventListener("keydown", onKey);
    };
  }, []);
}

export type Tone = "default" | "ok" | "warn" | "bad" | "info";

export function Tag({ tone = "default", children }: { tone?: Tone; children: ReactNode }) {
  return <span className={tone === "default" ? "tag" : `tag tag-${tone}`}>{children}</span>;
}

/**
 * 健康度：一个有色圆点 + 一个词。比 `Tag` 轻，能领在一行元信息的最前面，
 * 不会跟旁边的档位徽章争字体和高度。
 */
export function Health({ tone, children }: { tone: "ok" | "warn" | "bad"; children: ReactNode }) {
  return <span className={`health is-${tone}`}>{children}</span>;
}

export function Banner({
  tone = "default",
  title,
  hint,
  action,
}: {
  tone?: Tone;
  title: ReactNode;
  hint?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className={tone === "default" ? "banner" : `banner banner-${tone}`}>
      <span className="banner-dot" />
      <div className="grow">
        <div>{title}</div>
        {hint ? <span className="banner-hint">{hint}</span> : null}
      </div>
      {action}
    </div>
  );
}

/**
 * 错误展示。
 *
 * 永远把 `hint` 一起显示出来 —— 那是 `AppError` 存在的理由：用户不该看完错误还得猜
 * 下一步做什么。
 */
export function ErrorNote({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  if (!error) return null;
  const app: AppError | null = isAppError(error) ? error : null;
  const retryable =
    onRetry && app && ["network", "upstream", "cursor_running", "busy"].includes(app.code);
  return (
    <div style={{ marginBottom: 14 }}>
      <Banner
        tone="bad"
        title={app ? app.message : errorText(error)}
        hint={app?.hint}
        action={
          retryable ? (
            <button type="button" className="btn btn-sm" onClick={onRetry}>
              重试
            </button>
          ) : undefined
        }
      />
    </div>
  );
}

/**
 * 空态。
 *
 * 一枚淡淡的标 + 一句话 + 一个下一步。标是默认给的（不传 `icon` 就用一个中性的方块），
 * 没有它的话「一行字加一个按钮浮在灰底上」看着像内容没加载出来。
 */
export function Empty({
  icon = "empty",
  title,
  children,
  action,
}: {
  /** 传 `null` 可以不要那枚标（内容本身已经很短的行内空态）。 */
  icon?: string | null;
  title?: ReactNode;
  children?: ReactNode;
  action?: ReactNode;
}) {
  return (
    <div className="empty">
      {icon ? (
        <span className="empty-mark" aria-hidden>
          <Icon name={icon} size={18} />
        </span>
      ) : null}
      {title ? <div className="empty-title">{title}</div> : null}
      {children ? <div style={{ maxWidth: "46ch" }}>{children}</div> : null}
      {action}
    </div>
  );
}

export function Spinner() {
  return <span className="spinner" aria-label="加载中" />;
}

export function Modal({
  title,
  subtitle,
  onClose,
  children,
  footer,
  wide,
  compact,
}: {
  title: ReactNode;
  subtitle?: ReactNode;
  onClose: () => void;
  children: ReactNode;
  footer?: ReactNode;
  /** 需要摊开内容时（批量导入的预览表）。 */
  wide?: boolean;
  /** 只有几个字段时。默认宽度摊着三个输入框会显得空。 */
  compact?: boolean;
}) {
  // Esc 关闭：桌面应用里这是肌肉记忆，没有会让人觉得卡住了。
  useEscapeToClose(onClose);

  return (
    <Portal>
      <div
        className="overlay"
        role="presentation"
        onMouseDown={(e) => {
          if (e.target === e.currentTarget) onClose();
        }}
      >
        <div
          className={`modal${wide ? " modal-wide" : ""}${compact ? " modal-compact" : ""}`}
          role="dialog"
          aria-modal="true"
          aria-label={typeof title === "string" ? title : undefined}
        >
          <div className="modal-head">
            <div>
              <h3 className="modal-title">{title}</h3>
              {subtitle ? <p className="subtitle">{subtitle}</p> : null}
            </div>
            <button
              type="button"
              className="btn btn-icon btn-quiet"
              onClick={onClose}
              aria-label="关闭"
            >
              <Icon name="close" />
            </button>
          </div>
          <div className="modal-body">{children}</div>
          {footer ? <div className="modal-foot">{footer}</div> : null}
        </div>
      </div>
    </Portal>
  );
}

/**
 * 右侧抽屉。给「从一列东西里看其中一个」用：列表留在原地，抽屉盖在右边。
 *
 * 跟居中弹窗的差别在于位置感 —— 关掉之后眼睛还落在刚才那一行上，不用重新找。
 * `head` 是抽屉自己的标题区（通常比一行标题复杂），关闭按钮固定在它右上角。
 */
export function Drawer({
  label,
  onClose,
  head,
  children,
  footer,
}: {
  /** 给读屏器的名字。 */
  label: string;
  onClose: () => void;
  head: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
}) {
  useEscapeToClose(onClose);

  return (
    <Portal>
      <div
        className="scrim"
        role="presentation"
        onMouseDown={(e) => {
          if (e.target === e.currentTarget) onClose();
        }}
      >
        <aside className="drawer" role="dialog" aria-modal="true" aria-label={label}>
          {/* 关闭键绝对定位在角上，而不是占掉标题区的一栏 —— 否则标题区里那个主 CTA
              会被它顶得离右边缘差一截，跟下面的内容对不齐。 */}
          <div className="drawer-head">
            {head}
            <button type="button" className="btn btn-icon btn-quiet drawer-close" onClick={onClose} aria-label="关闭">
              <Icon name="close" />
            </button>
          </div>
          <div className="drawer-body">{children}</div>
          {footer ? <div className="drawer-foot">{footer}</div> : null}
        </aside>
      </div>
    </Portal>
  );
}

/**
 * 额度条。标签在左、数值在右、进度条占满下面一整行。
 *
 * 比「标签 | 条 | 数值」那种三栏挤在一行强：条子拿到全宽才看得出长短，数值靠右
 * 且加粗才扫得到。`compact` 是列表行里那一版，整体压扁。
 */
export function Gauge({
  label,
  percent,
  compact,
  note,
  title,
}: {
  label: string;
  percent?: number | null;
  compact?: boolean;
  /** 数值右边的补充，比如 `$400.00 / $400.00`。 */
  note?: ReactNode;
  title?: string;
}) {
  const known = percent != null && Number.isFinite(percent);
  return (
    <div className={compact ? "gauge is-compact" : "gauge"} title={title}>
      <div className="gauge-head">
        <span className="gauge-k">{label}</span>
        <span className="gauge-v">{note ?? pctText(percent)}</span>
      </div>
      <span className="gauge-track">
        <i
          style={{
            width: `${meterWidth(percent)}%`,
            background: known ? meterColor(percent) : "transparent",
          }}
        />
      </span>
    </div>
  );
}

/**
 * 「什么时候重置」的信息条。
 *
 * 之前这句话是缀在额度条角落的一行灰字，太不显眼 —— 而用户看完「用了多少」，下一个
 * 问题就是「什么时候能再用」，它该和额度条平级。所以：一个时钟图标、倒计时用正文亮度
 * 加粗、精确时刻退到后面用等宽小字。`progress` 给月账期画一条「走过多少」的底条。
 */
export function Reset({
  label,
  at,
  now = Date.now(),
  tag,
  progress,
}: {
  label: string;
  at?: number | null;
  now?: number;
  tag?: ReactNode;
  /** 0–1，账期已经走过的比例。 */
  progress?: number | null;
}) {
  const known = at != null && Number.isFinite(at);
  return (
    <div className="rstbar">
      <div className="rstbar-line">
        <Icon name="clock" size={13} className="rstbar-icon" />
        <span className="rstbar-k">{label}</span>
        <span className="rstbar-v">{known ? untilText(at, now) : "—"}</span>
        {known ? <span className="rstbar-at">{shortDateTime(at)}</span> : null}
        <span className="grow" />
        {tag}
      </div>
      {progress != null ? (
        <span className="cycle">
          <i style={{ width: `${progress * 100}%` }} />
        </span>
      ) : null}
    </div>
  );
}

/**
 * 一条设置：左边一个坐在小方框里的图标、中间名字（最多再跟半句后果）、右边控件。
 *
 * 图标替掉解释 —— 芯片就是机器码、文件夹就是目录，一行设置该靠名字和图标说清自己是什么。
 * `tone` 给图标框上色：开着的开关点亮成品牌色，出事的变红、待处理的变琥珀；一列扫下来，
 * 有颜色的就是要看的，所以「正常」不上色。`body` 给需要整行铺开的控件（输入框），
 * 标题行还在上面，控件落到下面一整行。`hint` 是悬停才出现的补充，不占地方。
 * 几条同类的放进 `.opts`（一张卡）；已经在卡或弹窗里的用 `.opts.is-flush`（只留发丝线）。
 */
export function Opt({
  icon,
  title,
  desc,
  tone,
  hint,
  body,
  children,
}: {
  /** 图标名；也可以直接给一个节点（关于页那一行放的是品牌标，不是图标）。 */
  icon: string | ReactNode;
  title: ReactNode;
  desc?: ReactNode;
  tone?: "on" | "warn" | "bad" | "off";
  hint?: string;
  body?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <div className={`opt${tone ? ` is-${tone}` : ""}${body ? " is-stack" : ""}`} title={hint}>
      <span className="opt-ico">{typeof icon === "string" ? <Icon name={icon} size={15} /> : icon}</span>
      <div className="opt-copy">
        <div className="opt-title">{title}</div>
        {desc ? <div className="opt-desc truncate">{desc}</div> : null}
      </div>
      <div className="opt-ctl">{children}</div>
      {body ? <div className="opt-body">{body}</div> : null}
    </div>
  );
}

/**
 * 一枚安静的下拉：按钮上是**当前选的那一项**，点开才列出别的。
 *
 * 不用原生 `<select>`：它在 macOS 上弹的是系统菜单，字体、圆角、暗色都不跟应用走，
 * 一条工具条上摆一个原生控件，那一处就是全页最糙的地方。也不做成一排并列的筛子 ——
 * 排序有五六档，摊开来比列表本身还占地方；「此刻按什么排」只需要一个词说清。
 */
export function Picker<T extends string>({
  icon,
  value,
  options,
  onChange,
  label,
}: {
  icon?: string;
  value: T;
  /** `meta` 是选项右侧的一点弱信息（这一档有几个号），只出现在展开的下拉里。 */
  options: { id: T; label: string; hint?: string; meta?: ReactNode }[];
  onChange: (next: T) => void;
  /** 给读屏器的名字，也是没选中任何一项时按钮上的兜底文案。 */
  label: string;
}) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement | null>(null);
  const current = options.find((o) => o.id === value);

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

  return (
    <div ref={box} className={`picker${open ? " is-open" : ""}`}>
      <button
        type="button"
        className="picker-btn"
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-label={label}
        onClick={() => setOpen((v) => !v)}
      >
        {icon ? <Icon name={icon} size={13} /> : null}
        <span className="picker-v truncate">{current?.label ?? label}</span>
        <Icon name="chevron" size={11} className="picker-chev" />
      </button>
      {open ? (
        <div className="picker-pop" role="listbox" aria-label={label}>
          {options.map((o) => (
            <button
              key={o.id}
              type="button"
              role="option"
              aria-selected={o.id === value}
              className={`picker-opt${o.id === value ? " is-on" : ""}`}
              title={o.hint}
              onClick={() => {
                onChange(o.id);
                setOpen(false);
              }}
            >
              <span className="truncate">{o.label}</span>
              {o.meta != null ? <span className="picker-meta num">{o.meta}</span> : null}
              {o.id === value ? <Icon name="check" size={12} /> : null}
            </button>
          ))}
        </div>
      ) : null}
    </div>
  );
}

export function Switch({
  checked,
  disabled,
  onChange,
  label,
}: {
  checked: boolean;
  disabled?: boolean;
  onChange: (next: boolean) => void;
  label: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      className="switch"
      disabled={disabled}
      onClick={() => onChange(!checked)}
    />
  );
}

/**
 * 复制按钮。复制成功给一个短暂的确认 —— 没有反馈的话用户会连点好几下。
 * `icon` 版只有一个图标，给一行里塞不下文字的地方（凭证行、抽屉标题）。
 */
export function CopyButton({
  value,
  label = "复制",
  icon,
}: {
  value: string;
  label?: string;
  icon?: boolean;
}) {
  const [copied, setCopied] = useState(false);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(timer.current), []);

  const copy = () => {
    void navigator.clipboard.writeText(value);
    setCopied(true);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setCopied(false), 1100);
  };

  if (icon) {
    return (
      <button
        type="button"
        className={copied ? "btn btn-sm btn-icon btn-quiet is-done" : "btn btn-sm btn-icon btn-quiet"}
        onClick={copy}
        // 复制完那一秒气泡改口说「已复制」—— 图标已经变成勾了，气泡跟上才不矛盾。
        data-tip={copied ? "已复制" : label}
        aria-label={label}
      >
        <Icon name={copied ? "check" : "copy"} size={13} />
      </button>
    );
  }
  return (
    <button type="button" className="btn btn-sm btn-quiet" onClick={copy}>
      {copied ? "已复制" : label}
    </button>
  );
}

/* ── 图标 ─────────────────────────────────────────────────────────────────
   自己画而不是引一个图标库：只用到十来个，一个包换来 200KB 和一套跟品牌不搭的
   线条不划算。统一 16×16、1.6 描边、round 端点，和品牌标同一套笔触。 */

const PATHS: Record<string, ReactNode> = {
  // 一对反向箭头 = 互换。
  switcher: (
    <>
      <path d="M4 8h13l-3.4-3.4M20 16H7l3.4 3.4" />
    </>
  ),
  accounts: (
    <>
      <path d="M4 19v-1a4 4 0 0 1 4-4h4a4 4 0 0 1 4 4v1" />
      <circle cx="10" cy="7" r="3.2" />
      <path d="M17 11.5a3 3 0 0 0 0-6" opacity="0.55" />
    </>
  ),
  shop: (
    <>
      <path d="M3.5 6.5h17l-1.4 11a2 2 0 0 1-2 1.7H6.9a2 2 0 0 1-2-1.7z" />
      <path d="M8.6 9.5V6a3.4 3.4 0 0 1 6.8 0v3.5" />
    </>
  ),
  // 推子而不是齿轮：齿轮的齿在 16px 下糊成一圈毛边，读起来像太阳。
  settings: (
    <>
      <path d="M4 7h6M14 7h6M4 17h10M18 17h2" />
      <circle cx="12" cy="7" r="2.2" />
      <circle cx="16" cy="17" r="2.2" />
    </>
  ),
  // 三条并行的流线 + 一个分叉：走另一条通道。
  sand: (
    <>
      <path d="M4 7h9a3 3 0 0 1 3 3v0a3 3 0 0 0 3 3h1" />
      <path d="M4 12h6" opacity="0.55" />
      <path d="M4 17h9a3 3 0 0 0 3-3v0a3 3 0 0 1 3-3h1" />
    </>
  ),
  // 一个回环里的节点 + 两条向外的连线：本机的一个口，别的东西都从它出。
  gateway: (
    <>
      <circle cx="12" cy="12" r="3.2" />
      <path d="M12 3.5v5.3M12 15.2v5.3" />
      <path d="M4.5 8.5l4.6 1.9M19.5 8.5l-4.6 1.9" opacity="0.55" />
      <path d="M4.5 15.5l4.6-1.9M19.5 15.5l-4.6-1.9" opacity="0.55" />
    </>
  ),
  close: <path d="M6.5 6.5l11 11M17.5 6.5l-11 11" />,
  plus: <path d="M12 5.5v13M5.5 12h13" />,
  // 一整圈留一个缺口，箭头正好补在缺口上。之前那版弧线太短、箭头飘在外面，
  // 16px 下读不出「循环」。
  refresh: (
    <>
      <path d="M19.9 9.4A8.2 8.2 0 1 0 20 14.4" />
      <path d="M20.6 4.6v5h-5" />
    </>
  ),
  // 一朵云：云端号源 / Nexus 云端相关的地方用它。
  cloud: (
    <path d="M7.3 19h9.9a4.3 4.3 0 0 0 .5-8.6 5.6 5.6 0 0 0-10.8-1A3.9 3.9 0 0 0 7.3 19z" />
  ),
  chevron: <path d="M9.5 5.5l6.5 6.5-6.5 6.5" />,
  // 回上一级的箭头：下钻页（订单详情、网关号池）面包屑上那一枚。
  back: <path d="M19 12H5.5M11.5 5.5L5 12l6.5 6.5" />,
  check: <path d="M5 12.5l4.5 4.5L19 7" />,
  clipboard: (
    <>
      <path d="M9 4.8H7.4A1.9 1.9 0 0 0 5.5 6.7v12.4a1.9 1.9 0 0 0 1.9 1.9h9.2a1.9 1.9 0 0 0 1.9-1.9V6.7a1.9 1.9 0 0 0-1.9-1.9H15" />
      <rect x="9" y="2.9" width="6" height="3.8" rx="1.3" />
      <path d="M8.8 11.5h6.4M8.8 15.2h4.2" opacity="0.6" />
    </>
  ),
  key: (
    <>
      <circle cx="8" cy="12" r="3.5" />
      <path d="M11.5 12H21M18 12v3M15 12v2.2" />
    </>
  ),
  mail: (
    <>
      <rect x="3" y="5.5" width="18" height="13" rx="2.4" />
      <path d="M3.6 7.2l7.3 5.2a2 2 0 0 0 2.2 0l7.3-5.2" />
    </>
  ),
  user: (
    <>
      <circle cx="12" cy="8" r="3.4" />
      <path d="M5 19.2a7 7 0 0 1 14 0" />
    </>
  ),
  lock: (
    <>
      <rect x="4.8" y="10.5" width="14.4" height="9.5" rx="2.4" />
      <path d="M8.4 10.5V7.9a3.6 3.6 0 0 1 7.2 0v2.6" />
    </>
  ),
  shield: (
    <>
      <path d="M12 3.2l7 2.6v5.4c0 4.3-2.9 7.8-7 9.6-4.1-1.8-7-5.3-7-9.6V5.8z" />
      <path d="M9.2 12.1l2 2 3.6-4" />
    </>
  ),
  external: (
    <>
      <path d="M14 4.5h5.5V10" />
      <path d="M19.5 4.5L11 13" />
      <path d="M18 14.5v3.6a1.9 1.9 0 0 1-1.9 1.9H5.9A1.9 1.9 0 0 1 4 18.1V7.9A1.9 1.9 0 0 1 5.9 6h3.6" />
    </>
  ),
  logout: (
    <>
      <path d="M14 6.5V5a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h6a2 2 0 0 0 2-2v-1.5" />
      <path d="M10 12h11M18 9l3 3-3 3" />
    </>
  ),
  search: (
    <>
      <circle cx="11" cy="11" r="6" />
      <path d="M15.6 15.6L20 20" />
    </>
  ),
  // 两张叠着的纸。
  copy: (
    <>
      <rect x="9" y="9" width="11" height="11" rx="2" />
      <path d="M6.5 15H5.9A1.9 1.9 0 0 1 4 13.1V5.9A1.9 1.9 0 0 1 5.9 4h7.2A1.9 1.9 0 0 1 15 5.9v.6" />
    </>
  ),
  trash: (
    <>
      <path d="M5 7h14M9.5 7V5.3a1.3 1.3 0 0 1 1.3-1.3h2.4a1.3 1.3 0 0 1 1.3 1.3V7" />
      <path d="M7 7l.8 11.2A1.9 1.9 0 0 0 9.7 20h4.6a1.9 1.9 0 0 0 1.9-1.8L17 7" />
      <path d="M10.2 10.5v6M13.8 10.5v6" opacity="0.55" />
    </>
  ),
  pencil: (
    <>
      <path d="M4.5 19.5l4-.9L19 8.1a1.6 1.6 0 0 0 0-2.3l-.8-.8a1.6 1.6 0 0 0-2.3 0L5.4 15.5z" />
      <path d="M14.3 6.6l3.1 3.1" opacity="0.55" />
    </>
  ),
  eye: (
    <>
      <path d="M2.8 12s3.4-6 9.2-6 9.2 6 9.2 6-3.4 6-9.2 6-9.2-6-9.2-6z" />
      <circle cx="12" cy="12" r="2.8" />
    </>
  ),
  eyeOff: (
    <>
      <path d="M2.8 12s3.4-6 9.2-6 9.2 6 9.2 6-3.4 6-9.2 6-9.2-6-9.2-6z" />
      <circle cx="12" cy="12" r="2.8" />
      <path d="M4.5 4.5l15 15" />
    </>
  ),
  clock: (
    <>
      <circle cx="12" cy="12" r="8.2" />
      <path d="M12 7.4V12l3.1 2.1" />
    </>
  ),
  // 上下两个箭头：排序。
  sort: (
    <>
      <path d="M8 5v14M8 19l-3-3M8 19l3-3" />
      <path d="M16 19V5M16 5l-3 3M16 5l3 3" opacity="0.55" />
    </>
  ),
  // 一个托盘、一支从里面往上出去的箭头：把东西从应用里拿出去。
  export: (
    <>
      <path d="M4.5 14.5v3.6a1.9 1.9 0 0 0 1.9 1.9h11.2a1.9 1.9 0 0 0 1.9-1.9v-3.6" />
      <path d="M12 4.5v10.5M8.2 8.3L12 4.5l3.8 3.8" />
    </>
  ),
  // 空态默认的那枚标：一个虚着的方框 + 中间一道短线。不画具体的东西（文件夹、收件箱），
  // 空态出现在十几个地方，画具体了就会有一半场合对不上。
  empty: (
    <>
      <rect x="4" y="5.5" width="16" height="13" rx="2.6" opacity="0.5" />
      <path d="M9 12h6" />
    </>
  ),
  // 一个带盖的箱子：备份。
  archive: (
    <>
      <rect x="3.5" y="4.5" width="17" height="4.6" rx="1.3" />
      <path d="M5.2 9.1v8.4a2 2 0 0 0 2 2h9.6a2 2 0 0 0 2-2V9.1" />
      <path d="M10 13.4h4" opacity="0.6" />
    </>
  ),

  /* ── 设置页那一组 ────────────────────────────────────────────────────────
     每一项设置左边坐一个图标，替掉一行解释。图标要一眼读出「这一项管什么」，
     所以宁可用最老实的隐喻：半黑半白 = 外观，芯片 = 机器码，文件夹 = 目录。 */

  // 半黑半白的圆：外观 / 深浅。
  contrast: (
    <>
      <circle cx="12" cy="12" r="8.2" />
      <path d="M12 3.8a8.2 8.2 0 0 1 0 16.4z" fill="currentColor" stroke="none" opacity="0.85" />
    </>
  ),
  // 芯片：机器码 / 设备指纹。
  cpu: (
    <>
      <rect x="7" y="7" width="10" height="10" rx="2" />
      <rect x="10.2" y="10.2" width="3.6" height="3.6" rx="0.8" opacity="0.55" />
      <path d="M9.5 3.5V7M14.5 3.5V7M9.5 17v3.5M14.5 17v3.5M3.5 9.5H7M3.5 14.5H7M17 9.5h3.5M17 14.5h3.5" />
    </>
  ),
  // 叠着的几张：保留几份。
  layers: (
    <>
      <path d="M12 4.2l8 4.1-8 4.1-8-4.1z" />
      <path d="M4 12.4l8 4.1 8-4.1" opacity="0.55" />
      <path d="M4 16.4l8 4.1 8-4.1" opacity="0.3" />
    </>
  ),
  // 逆时针转回去：还原。
  undo: (
    <>
      <path d="M4.4 12a7.8 7.8 0 1 0 7.8-7.8 8.3 8.3 0 0 0-5.9 2.5L4.4 8.6" />
      <path d="M4.2 3.9v4.7h4.7" />
    </>
  ),
  folder: (
    <path d="M3.5 7.4a1.9 1.9 0 0 1 1.9-1.9h4.1l2.1 2.2h7a1.9 1.9 0 0 1 1.9 1.9v7a1.9 1.9 0 0 1-1.9 1.9H5.4a1.9 1.9 0 0 1-1.9-1.9z" />
  ),
  // 一个立方体：装好的程序本体。
  box: (
    <>
      <path d="M12 3.6l7.4 4.1v8.6L12 20.4l-7.4-4.1V7.7z" />
      <path d="M4.9 7.9L12 12l7.1-4.1M12 12v8.2" opacity="0.55" />
    </>
  ),
  globe: (
    <>
      <circle cx="12" cy="12" r="8.2" />
      <path d="M3.8 12h16.4" opacity="0.55" />
      <path d="M12 3.8c2.5 2.4 3.6 5.1 3.6 8.2s-1.1 5.8-3.6 8.2c-2.5-2.4-3.6-5.1-3.6-8.2s1.1-5.8 3.6-8.2z" opacity="0.55" />
    </>
  ),
  // 几个圆盘叠起来：库、快照。
  database: (
    <>
      <ellipse cx="12" cy="6.2" rx="7.4" ry="2.7" />
      <path d="M4.6 6.2v11.6c0 1.5 3.3 2.7 7.4 2.7s7.4-1.2 7.4-2.7V6.2" />
      <path d="M4.6 12c0 1.5 3.3 2.7 7.4 2.7s7.4-1.2 7.4-2.7" opacity="0.55" />
    </>
  ),
  // 三行带点：日志、清单。
  list: (
    <>
      <path d="M9 6.5h11M9 12h11M9 17.5h11" />
      <path d="M4.6 6.5h.01M4.6 12h.01M4.6 17.5h.01" strokeWidth={2.6} />
    </>
  ),
  info: (
    <>
      <circle cx="12" cy="12" r="8.2" />
      <path d="M12 11v5.2" />
      <path d="M12 7.9h.01" strokeWidth={2.4} />
    </>
  ),
  // 托盘 + 往下进来的箭头：更新、下载。跟 export 是一对。
  download: (
    <>
      <path d="M4.5 15v2.6a1.9 1.9 0 0 0 1.9 1.9h11.2a1.9 1.9 0 0 0 1.9-1.9V15" />
      <path d="M12 4v10.2M8.2 10.4l3.8 3.8 3.8-3.8" />
    </>
  ),
  power: (
    <>
      <path d="M12 3.8v8" />
      <path d="M7.3 6.7a7.3 7.3 0 1 0 9.4 0" />
    </>
  ),
  // 应用窗口：一个圆角矩形，顶上一道标题栏。
  window: (
    <>
      <rect x="3.5" y="4.5" width="17" height="15" rx="2.2" />
      <path d="M3.5 9h17" opacity="0.55" />
      <path d="M6.6 6.8h.01M9.2 6.8h.01" strokeWidth={2.2} />
    </>
  ),
  // 一顶小皇冠：订阅档位。三个尖角 + 一道底边，16px 下也读得出。
  crown: (
    <>
      <path d="M4.6 16.6 3.6 8.4l4.3 3.2L12 6l4.1 5.6 4.3-3.2-1 8.2z" />
      <path d="M5 19.6h14" opacity="0.55" />
    </>
  ),
};

export function Icon({
  name,
  size = 16,
  className,
  style,
}: {
  name: keyof typeof PATHS | string;
  size?: number;
  className?: string;
  style?: CSSProperties;
}) {
  const path = PATHS[name];
  if (!path) return null;
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      style={style}
      aria-hidden
    >
      {path}
    </svg>
  );
}
