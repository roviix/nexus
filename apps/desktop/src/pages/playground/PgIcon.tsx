/**
 * 游乐场自己用的几个图标（发送、停止、生成、文件夹、放大），其余都走 `ShellIcon` →
 * `ui/primitives` 的 `Icon`（对话 / 图片 / 资产三个子项的图标在 `ShellIcon` 里，侧栏也用）。
 * 同一套笔触：24 网格、round 端点。
 */
import { ShellIcon } from "../../shell/ShellIcon";

export function PgIcon({ name, size = 16, className }: { name: string; size?: number; className?: string }) {
  const common = {
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.7,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
    className,
    "aria-hidden": true,
  };
  // 纸飞机：发送。
  if (name === "send") {
    return (
      <svg {...common}>
        <path d="M4.5 12 20 4.5 16.5 20l-4.6-5.4L4.5 12Z" />
        <path d="m11.9 14.6 8.1-10.1" opacity="0.55" />
      </svg>
    );
  }
  // 向上的箭头：发送。圆键里只放一个箭头，比纸飞机在 16px 上更利落。
  if (name === "up") {
    return (
      <svg {...common} strokeWidth={2}>
        <path d="M12 19.5V5" />
        <path d="m5.5 11.5 6.5-6.5 6.5 6.5" />
      </svg>
    );
  }
  // 一张纸：文本附件。
  if (name === "file") {
    return (
      <svg {...common}>
        <path d="M13.5 3.5H7.5a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2h9a2 2 0 0 0 2-2V8.5l-5-5Z" />
        <path d="M13.5 3.5v5h5" opacity="0.6" />
      </svg>
    );
  }
  // 回形针：带点东西上去。
  if (name === "clip") {
    return (
      <svg {...common}>
        <path d="M18.5 11.3 12 17.8a4.1 4.1 0 0 1-5.8-5.8l7-7a2.7 2.7 0 0 1 3.9 3.9l-7 7a1.4 1.4 0 0 1-1.9-1.9l6.2-6.2" />
      </svg>
    );
  }
  // 实心方块：停止。描边太细的方块在 13px 上读不出「停」。
  if (name === "stop") {
    return (
      <svg {...common}>
        <rect x="6.5" y="6.5" width="11" height="11" rx="2" fill="currentColor" stroke="none" />
      </svg>
    );
  }
  // 四角星：生成。
  if (name === "spark") {
    return (
      <svg {...common}>
        <path d="M12 3.5c.6 4.6 3.9 7.9 8.5 8.5-4.6.6-7.9 3.9-8.5 8.5-.6-4.6-3.9-7.9-8.5-8.5 4.6-.6 7.9-3.9 8.5-8.5Z" />
      </svg>
    );
  }
  if (name === "folder") {
    return (
      <svg {...common}>
        <path d="M3.5 7.5A2 2 0 0 1 5.5 5.5h4l2 2.2h7a2 2 0 0 1 2 2v7.8a2 2 0 0 1-2 2h-13a2 2 0 0 1-2-2v-10Z" />
      </svg>
    );
  }
  // 四角外扩：看大图。
  if (name === "expand") {
    return (
      <svg {...common}>
        <path d="M14 4.5h5.5V10M10 19.5H4.5V14M19.5 4.5 14 10M4.5 19.5 10 14" />
      </svg>
    );
  }
  // 左右箭头：大图里翻上一张 / 下一张。
  if (name === "prev") {
    return (
      <svg {...common}>
        <path d="m14.5 5.5-6.5 6.5 6.5 6.5" />
      </svg>
    );
  }
  if (name === "next") {
    return (
      <svg {...common}>
        <path d="m9.5 5.5 6.5 6.5-6.5 6.5" />
      </svg>
    );
  }
  return <ShellIcon name={name} size={size} className={className} />;
}
