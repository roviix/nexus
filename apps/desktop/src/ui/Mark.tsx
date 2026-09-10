/**
 * Nexus 品牌标：莫比乌斯无限环。
 *
 * 几何、渐变档位、墨迹范围都与 shop 的 `_components/brand-mark.ts` 逐字一致 ——
 * 两边显示的必须是同一个标，改了要一起改。
 *
 * 为什么是描边不是填充：填充版在 20px 下会糊成一团，描边才保得住笔画。
 * 为什么渐变只走薄荷→翠→青→靛紫这一段邻近色：跨度再拉大，过渡处必然经过一段脏色，
 * 小尺寸下那段灰会盖掉整个标。
 */
import { useId } from "react";

export const MARK_PATH =
  "M12 12C10.2 9.2 8.9 7.6 6.9 7.6C4.4 7.6 2.6 9.6 2.6 12C2.6 14.4 4.4 16.4 6.9 16.4C8.9 16.4 10.2 14.8 12 12C13.8 9.2 15.1 7.6 17.1 7.6C19.6 7.6 21.4 9.6 21.4 12C21.4 14.4 19.6 16.4 17.1 16.4C15.1 16.4 13.8 14.8 12 12Z";

export const MARK_GRADIENT = [
  { offset: "0%", color: "#6ff0c4" },
  { offset: "38%", color: "#2dd4a0" },
  { offset: "72%", color: "#22a8c8" },
  { offset: "100%", color: "#7b7ef0" },
];

/**
 * `tight` 用算进描边后的实际墨迹范围（环只占 24×24 视框竖向的 47%）。
 * 和文字排在一起时要用它，否则按视框对齐会让标看起来小掉一半。
 */
export function Mark({
  size = 24,
  tight = false,
  className,
}: {
  size?: number;
  tight?: boolean;
  className?: string;
}) {
  const id = useId();
  return (
    <svg
      viewBox={tight ? "1.3 6.3 21.4 11.4" : "0 0 24 24"}
      width={size}
      height={tight ? (size * 11.4) / 21.4 : size}
      className={className}
      role="img"
      aria-label="Nexus"
    >
      <defs>
        {/* userSpaceOnUse + useId：同一页面里侧栏和弹窗各渲染一个也不会互相抢 defs。 */}
        <linearGradient id={id} x1="2" y1="17" x2="22" y2="7" gradientUnits="userSpaceOnUse">
          {MARK_GRADIENT.map((s) => (
            <stop key={s.offset} offset={s.offset} stopColor={s.color} />
          ))}
        </linearGradient>
      </defs>
      <path
        d={MARK_PATH}
        fill="none"
        stroke={`url(#${id})`}
        strokeWidth={2.6}
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

/** 标 + 字标。侧栏顶部用。 */
export function Wordmark() {
  return (
    <span className="wordmark">
      <Mark size={26} tight />
      <span className="wordmark-text">Nexus</span>
    </span>
  );
}
