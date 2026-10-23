/**
 * 体验（多模态工作台与生成资产）—— Nexus 2.0
 *
 * 顶部 Segmented 胶囊切换栏：[💬 对话] [🎨 图片生成] [🎬 视频生成] | [🗂 资产画廊]
 * 与右侧本地网关运行状态实时联动，支持一键跃迁至模型广场。
 */
import { useEffect, useState } from "react";
import { useRelay } from "../relay/useRelay";
import { go, type PlaygroundView, type Route } from "../shell/nav";
import { AssetsView } from "./playground/AssetsView";
import { rememberSelected, Workbench } from "./playground/Workbench";
import "./playground/playground.css";

const VIEW_KEY = "playground.view";

function readView(): PlaygroundView {
  const v = localStorage.getItem(VIEW_KEY);
  return v === "image" || v === "video" || v === "assets" ? v : "chat";
}

export function PlaygroundPage({ route, onGo }: { route: Route; onGo: (r: Route) => void }) {
  const relay = useRelay({ catalogs: true });

  const [view, setView] = useState<PlaygroundView>(() => {
    if (route.view === "image" || route.view === "video" || route.view === "assets" || route.view === "chat") {
      return route.view;
    }
    return readView();
  });

  useEffect(() => {
    if (route.view && (route.view === "image" || route.view === "video" || route.view === "assets" || route.view === "chat")) {
      setView(route.view);
      localStorage.setItem(VIEW_KEY, route.view);
    }
  }, [route.view]);

  function switchView(nextView: PlaygroundView) {
    setView(nextView);
    localStorage.setItem(VIEW_KEY, nextView);
    onGo(go("playground", { view: nextView }));
  }

  const hint = route.view === view && route.model ? { model: route.model } : undefined;

  return (
    <div className="playground-shell-wrap">
      {/* 顶部多模态 Segmented 胶囊切换器 (参考原生胶囊 Pill 设计，支持自由点击切换) */}
      <header className="playground-subnav-bar" data-tauri-drag-region>
        <div className="playground-nav-pill-wrap" data-tauri-drag-region="none" role="tablist">
          <button
            type="button"
            role="tab"
            aria-selected={view === "chat"}
            className={`pg-nav-pill${view === "chat" ? " is-active" : ""}`}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => switchView("chat")}
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/>
            </svg>
            <span>聊天</span>
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={view === "image"}
            className={`pg-nav-pill${view === "image" ? " is-active" : ""}`}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => switchView("image")}
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <rect x="3" y="3" width="18" height="18" rx="2" ry="2"/>
              <circle cx="8.5" cy="8.5" r="1.5"/>
              <polyline points="21 15 16 10 5 21"/>
            </svg>
            <span>绘图</span>
          </button>
          <button
            type="button"
            role="tab"
            aria-selected={view === "video"}
            className={`pg-nav-pill${view === "video" ? " is-active" : ""}`}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => switchView("video")}
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <rect x="2" y="2" width="20" height="20" rx="2.18" ry="2.18"/>
              <line x1="7" y1="2" x2="7" y2="22"/>
              <line x1="17" y1="2" x2="17" y2="22"/>
              <line x1="2" y1="12" x2="22" y2="12"/>
              <line x1="2" y1="7" x2="7" y2="7"/>
              <line x1="2" y1="17" x2="7" y2="17"/>
              <line x1="17" y1="17" x2="22" y2="17"/>
              <line x1="17" y1="7" x2="22" y2="7"/>
            </svg>
            <span>视频</span>
          </button>
          <span className="pg-nav-pill-sep" />
          <button
            type="button"
            role="tab"
            aria-selected={view === "assets"}
            className={`pg-nav-pill${view === "assets" ? " is-active" : ""}`}
            onMouseDown={(e) => e.stopPropagation()}
            onClick={() => switchView("assets")}
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <polygon points="12 2 2 7 12 12 22 7 12 2"/>
              <polyline points="2 17 12 22 22 17"/>
              <polyline points="2 12 12 17 22 12"/>
            </svg>
            <span>资产画廊</span>
          </button>
        </div>
      </header>

      <div className="playground-content-area">
        {view === "assets" ? (
          <AssetsView
            onOpenThread={(threadId, kind) => {
              const v = kind === "video" ? "video" : "image";
              rememberSelected(v, threadId);
              switchView(v);
            }}
            onGoImages={() => switchView("image")}
          />
        ) : (
          <Workbench key={view} kind={view} relay={relay} hint={hint} onGo={onGo} />
        )}
      </div>
    </div>
  );
}
