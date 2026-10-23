import React from "react";
import ReactDOM from "react-dom/client";
import { App } from "./App";
import { PLATFORM } from "./ui/platform";
import { initTheme } from "./ui/theme";
import "./styles.css";
import "./framework.css";

// 全局异常兜底：避免任何首屏 Promise、模块执行或网络异常导致静默白屏
function renderFatalScreen(title: string, error: unknown) {
  const root = document.getElementById("root");
  if (!root) return;
  if (root.children.length > 0 && !root.querySelector(".fatal-error-card")) {
    return;
  }
  root.innerHTML = `
    <div class="fatal-error-card" style="display:flex;flex-direction:column;align-items:center;justify-content:center;min-height:100vh;background:#0d0f12;color:#e6edf3;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',Roboto,sans-serif;padding:32px;text-align:center;">
      <div style="max-width:540px;background:#161b22;border:1px solid #30363d;border-radius:12px;padding:28px 24px;box-shadow:0 8px 24px rgba(0,0,0,0.5);">
        <div style="font-size:36px;margin-bottom:12px;">⚠️</div>
        <h2 style="margin:0 0 8px 0;font-size:18px;font-weight:600;color:#f0f6fc;">${title}</h2>
        <p style="margin:0 0 16px 0;font-size:13px;color:#8b949e;line-height:1.5;">应用界面初始化遇到异常，您可以尝试直接刷新或重置前端本地缓存。</p>
        <pre style="margin:0 0 20px 0;padding:12px;background:#0d1117;border:1px solid #21262d;border-radius:6px;color:#ff7b72;font-size:12px;text-align:left;white-space:pre-wrap;word-break:break-all;max-height:160px;overflow-y:auto;">${String(error instanceof Error ? error.stack || error.message : error)}</pre>
        <div style="display:flex;gap:12px;justify-content:center;">
          <button id="btn-reload" style="background:#238636;color:#fff;border:none;padding:8px 18px;border-radius:6px;font-size:13px;font-weight:500;cursor:pointer;">重试刷新</button>
          <button id="btn-reset" style="background:#21262d;color:#c9d1d9;border:1px solid #30363d;padding:8px 18px;border-radius:6px;font-size:13px;cursor:pointer;">清空缓存并刷新</button>
        </div>
      </div>
    </div>
  `;
  document.getElementById("btn-reload")?.addEventListener("click", () => {
    window.location.reload();
  });
  document.getElementById("btn-reset")?.addEventListener("click", () => {
    try {
      localStorage.clear();
      sessionStorage.clear();
    } catch {}
    window.location.href = window.location.origin + window.location.pathname;
  });
}

window.addEventListener("error", (event) => {
  console.error("Window Error:", event.error || event.message);
  const root = document.getElementById("root");
  if (!root || root.children.length === 0) {
    renderFatalScreen("界面加载失败", event.error || event.message);
  }
});
window.addEventListener("unhandledrejection", (event) => {
  console.error("Unhandled Rejection:", event.reason);
  const root = document.getElementById("root");
  if (!root || root.children.length === 0) {
    renderFatalScreen("异步初始化异常", event.reason);
  }
});

// 外壳样式按系统分叉（红绿灯留白、顶部拖拽区）。挂在 <html> 上而不是用 JS 改样式：
// CSS 自己就能分支，首帧即生效，不会先画错再纠正。
document.documentElement.dataset.platform = PLATFORM;

// 动态全屏检测：全屏时移除 macOS 预留的红绿灯空白与拖拽区域，消除顶部大额头
const syncFullscreen = () => {
  const isFs = Boolean(
    document.fullscreenElement ||
      (typeof window !== "undefined" &&
        window.matchMedia &&
        window.matchMedia("(display-mode: fullscreen)").matches) ||
      (typeof window !== "undefined" &&
        window.innerHeight > 0 &&
        window.innerHeight === window.screen.height)
  );
  if (isFs) {
    document.documentElement.dataset.fullscreen = "true";
  } else {
    delete document.documentElement.dataset.fullscreen;
  }
};

window.addEventListener("resize", syncFullscreen);
document.addEventListener("fullscreenchange", syncFullscreen);
syncFullscreen();

// 在 Tauri 容器中订阅窗口真实全屏事件
void (async () => {
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    const win = getCurrentWindow();
    const updateTauriFs = async () => {
      const fs = await win.isFullscreen();
      if (fs) {
        document.documentElement.dataset.fullscreen = "true";
      } else {
        delete document.documentElement.dataset.fullscreen;
      }
    };
    await updateTauriFs();
    await win.onResized(() => {
      void updateTauriFs();
    });
  } catch {
    // 纯网页环境忽略
  }
})();

// 主题同理：渲染之前就把 data-theme 写上，首帧就是用户选的那套颜色。
try {
  initTheme();
} catch (e) {
  console.error("initTheme error:", e);
}

class RootErrorBoundary extends React.Component<
  { children: React.ReactNode },
  { hasError: boolean; error: Error | null }
> {
  constructor(props: { children: React.ReactNode }) {
    super(props);
    this.state = { hasError: false, error: null };
  }
  static getDerivedStateFromError(error: Error) {
    return { hasError: true, error };
  }
  componentDidCatch(error: Error, errorInfo: React.ErrorInfo) {
    console.error("RootErrorBoundary caught:", error, errorInfo);
  }
  render() {
    if (this.state.hasError) {
      return (
        <div style={{ padding: 32, color: "#fff", background: "#111", fontFamily: "system-ui" }}>
          <h2>应用渲染遇到错误</h2>
          <pre style={{ color: "#ff6b6b", whiteSpace: "pre-wrap", marginTop: 12 }}>
            {String(this.state.error?.stack || this.state.error?.message)}
          </pre>
          <button
            onClick={() => window.location.reload()}
            style={{ padding: "8px 16px", marginTop: 16, cursor: "pointer" }}
          >
            重试刷新
          </button>
        </div>
      );
    }
    return this.props.children;
  }
}

const root = document.getElementById("root");
if (!root) throw new Error("找不到 #root，index.html 被改坏了");

ReactDOM.createRoot(root).render(
  <React.StrictMode>
    <RootErrorBoundary>
      <App />
    </RootErrorBoundary>
  </React.StrictMode>,
);
