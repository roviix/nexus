import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { PLATFORM } from "./ui/platform";
import { initTheme } from "./ui/theme";
import "./styles.css";
import "./framework.css";

// 外壳样式按系统分叉（红绿灯留白、顶部拖拽区）。挂在 <html> 上而不是用 JS 改样式：
// CSS 自己就能分支，首帧即生效，不会先画错再纠正。
document.documentElement.dataset.platform = PLATFORM;
// 主题同理：渲染之前就把 data-theme 写上，首帧就是用户选的那套颜色。
initTheme();

const root = document.getElementById("root");
if (!root) throw new Error("找不到 #root，index.html 被改坏了");

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
