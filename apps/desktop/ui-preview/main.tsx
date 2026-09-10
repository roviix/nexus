import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "../src/App";
import { PLATFORM } from "../src/ui/platform";
import { initTheme, isThemePref } from "../src/ui/theme";
import "../src/styles.css";
import "../src/framework.css";

// 和 src/main.tsx 一样按 UA 标平台：红绿灯让位、顶部那条拖拽带都挂在这个属性上，
// 预览里不标就会照着一个不存在的系统排版。
document.documentElement.dataset.platform = PLATFORM;

// `?theme=light|dark|system` 直接看某一套；不带就照存下的偏好走（和真应用一样）。
const theme = new URLSearchParams(location.search).get("theme");
initTheme(isThemePref(theme) ? theme : undefined);

// `?route=use/gateway` → `#use/gateway`：让外壳自己的 hash 路由接管。
const route = new URLSearchParams(location.search).get("route");
if (route && !location.hash) location.hash = `#${route.replace(/^#?\/?/, "")}`;

const root = document.getElementById("root");
if (!root) throw new Error("找不到 #root");

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);

/**
 * `?click=试一下,发送`：截图用。渲染完按顺序点这几个键，文本或 aria-label 命中都算 ——
 * 抽屉、弹窗、流式回字这些要点一下才看得见的状态，无头浏览器自己点不了。
 * 每步之间留一拍，等上一步的假延迟走完。
 *
 * 先找整段相等的，找不到再退到「包含」：卡片型的键（工具卡、通道卡）整段文字是名字
 * 加一串状态，地址栏里没法照抄。
 */
const clicks = new URLSearchParams(location.search).get("click");
if (clicks) {
  const hit = (label: string) => {
    const all = [...document.querySelectorAll("button")];
    return (
      all.find((b) => b.textContent?.trim() === label || b.getAttribute("aria-label") === label) ??
      all.find((b) => b.textContent?.includes(label))
    );
  };
  clicks.split(",").forEach((label, i) => {
    window.setTimeout(() => hit(label.trim())?.click(), 1200 + i * 700);
  });
}
