/**
 * 外壳自己用的几个图标（概览的九宫格、模型广场的层叠、接入的插头、播放、箭头），
 * 其余都走 `ui/primitives` 的 `Icon`。单独放一份是为了不动那张公共图标表；等它稳定下来再并进去。
 * 描边 1.8，比公共表的 1.6 略粗 —— 这几个多数出现在导航和按钮上，小一号也要看得清。
 */
import { Icon } from "../ui/primitives";

export function ShellIcon({ name, size = 16, className }: { name: string; size?: number; className?: string }) {
  const common = {
    width: size,
    height: size,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.8,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
    className,
    "aria-hidden": true,
  };
  if (name === "grid") {
    return (
      <svg {...common}>
        <rect x="3.5" y="3.5" width="7" height="7" rx="1.5" />
        <rect x="13.5" y="3.5" width="7" height="7" rx="1.5" />
        <rect x="3.5" y="13.5" width="7" height="7" rx="1.5" />
        <rect x="13.5" y="13.5" width="7" height="7" rx="1.5" />
      </svg>
    );
  }
  if (name === "layers") {
    return (
      <svg {...common}>
        <path d="M12 3.5 3.5 8 12 12.5 20.5 8 12 3.5Z" />
        <path d="m3.5 12 8.5 4.5 8.5-4.5" />
        <path d="m3.5 16 8.5 4.5 8.5-4.5" />
      </svg>
    );
  }
  // 插头：两根插脚 + 一个壳 + 一根线。「接入」就是把线插上。
  if (name === "plug") {
    return (
      <svg {...common}>
        <path d="M9 2.5v5M15 2.5v5" />
        <path d="M6 7.5h12v3a6 6 0 0 1-12 0v-3Z" />
        <path d="M12 16.5v5" />
      </svg>
    );
  }
  // 烧瓶：「拿它试一发」。三角形是「播放」，不是试验；液面那一横让它在 14px 上不会糊成一个瓶子轮廓。
  if (name === "flask") {
    return (
      <svg {...common}>
        <path d="M9.6 3.2h4.8" />
        <path d="M11 3.2v6.4l-5.1 8.5a1.9 1.9 0 0 0 1.6 2.9h9a1.9 1.9 0 0 0 1.6-2.9L13 9.6V3.2" />
        <path d="M8.4 15.4h7.2" />
      </svg>
    );
  }
  if (name === "play") {
    return (
      <svg {...common}>
        <path d="M7 4.5v15l12-7.5-12-7.5Z" />
      </svg>
    );
  }
  if (name === "arrow") {
    return (
      <svg {...common}>
        <path d="M5 12h14" />
        <path d="m13 6 6 6-6 6" />
      </svg>
    );
  }
  if (name === "orders") {
    return (
      <svg {...common}>
        <path d="M6 3.5h12v17l-3-1.8-3 1.8-3-1.8-3 1.8v-17Z" />
        <path d="M9 8h6M9 12h6M9 16h3" />
      </svg>
    );
  }
  // 游乐场的三个子项：气泡（对话）、一张有山有太阳的图（图片）、两张叠着的图（资产）。
  if (name === "chat") {
    return (
      <svg {...common}>
        <path d="M5 6.5A2.5 2.5 0 0 1 7.5 4h9A2.5 2.5 0 0 1 19 6.5v6a2.5 2.5 0 0 1-2.5 2.5H11l-3.5 3.2V15H7.5A2.5 2.5 0 0 1 5 12.5v-6Z" />
      </svg>
    );
  }
  if (name === "image") {
    return (
      <svg {...common}>
        <rect x="3.5" y="5" width="17" height="14" rx="2.5" />
        <circle cx="9" cy="10" r="1.6" />
        <path d="m7 17 3.6-3.6a1.2 1.2 0 0 1 1.7 0L20 17" />
      </svg>
    );
  }
  if (name === "gallery") {
    return (
      <svg {...common}>
        <rect x="3.5" y="7.5" width="14" height="12" rx="2.2" />
        <path d="M7.5 7.5V6a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2h-1" opacity="0.6" />
        <path d="m5.5 17.5 3.2-3.2a1.2 1.2 0 0 1 1.7 0l4.6 4.6" />
        <circle cx="8.3" cy="11.2" r="1.3" />
      </svg>
    );
  }
  return <Icon name={name} size={size} className={className} />;
}
