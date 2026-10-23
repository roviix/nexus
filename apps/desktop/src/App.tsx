import { useCallback, useEffect, useState } from "react";
import { app } from "./ipc/api";
import type { AppStatus } from "./ipc/types";
import { AccountsPage } from "./pages/AccountsPage";
import { ConnectPage } from "./pages/ConnectPage";
import { ModelsPage } from "./pages/ModelsPage";
import { OverviewPage } from "./pages/OverviewPage";
import { PlaygroundPage } from "./pages/PlaygroundPage";
import { CursorPanelPage } from "./pages/CursorPanelPage";
import { ChannelsPage } from "./pages/ChannelsPage";
import { PreflightModal } from "./pages/settings/PreflightModal";
import { SettingsPage } from "./pages/SettingsPage";
import { SwitcherPage } from "./pages/SwitcherPage";
import { go, NAV_GROUPS, parseRoute, routeHash, sectionMeta, type Route } from "./shell/nav";
import { ShellIcon } from "./shell/ShellIcon";
import { ConfirmHost } from "./ui/confirm";
import { Icon } from "./ui/primitives";
import { UpdateNotice } from "./UpdateNotice";
import { GatewayPod } from "./shell/GatewayPod";
import { LiveActivityDrawer } from "./shell/LiveActivityDrawer";
import { CommandPalette } from "./shell/CommandPalette";

export function App() {
  return <Shell />;
}

/**
 * 路由存在 `location.hash` 里（`#connect?model=…`）。
 *
 * 不引路由库：十几个一级页、少量带参数，一个 hash 就够。放进 hash 的好处是刷新 / 重开
 * 回到原处，预览脚手架也能用同一个地址直达某一页。
 */
function useRoute(): [Route, (r: Route) => void] {
  const [route, setRoute] = useState<Route>(() => parseRoute(window.location.hash));
  useEffect(() => {
    const onHash = () => setRoute(parseRoute(window.location.hash));
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);
  const navigate = useCallback((r: Route) => {
    const hash = routeHash(r);
    if (window.location.hash !== hash) window.location.hash = hash;
    else setRoute(r);
  }, []);
  return [route, navigate];
}

function Shell() {
  const [route, navigate] = useRoute();
  const [status, setStatus] = useState<AppStatus | null>(null);
  const [activityOpen, setActivityOpen] = useState(false);
  const [cmdOpen, setCmdOpen] = useState(false);

  useEffect(() => {
    void app
      .status()
      .then(setStatus)
      .catch(() => setStatus(null));
  }, [route.section]);

  // 全局 ⌘K / Ctrl+K 唤出快捷搜索
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setCmdOpen((v) => !v);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  // 只有「Cursor 都找不到」这一件事值得在导航上挂红点：它意味着切号和 Sand 都干不了活。
  const broken = status != null && !status.cursor.dbPresent;
  const settings = sectionMeta("settings");

  return (
    <div className="shell">
      <div className="drag-region" data-tauri-drag-region />

      {/* 侧栏 */}
      <nav className="sidebar" data-tauri-drag-region>
        {/* 顶部自适应呼吸与拖拽带（macOS 留出红绿灯位，全屏/Win 自动收敛） */}
        <div className="sidebar-head" data-tauri-drag-region />

        <div className="nav-scroll">
          {NAV_GROUPS.map((g) => (
            <div key={g.id} className="nav-group">
              {g.label ? <div className="nav-label">{g.label}</div> : null}
              {g.items.map((id) => {
                const s = sectionMeta(id);
                return (
                  <button
                    key={id}
                    type="button"
                    className="nav-item"
                    aria-current={route.section === id ? "page" : undefined}
                    onClick={() => navigate(go(id))}
                  >
                    <ShellIcon name={s.icon} className="nav-icon" />
                    <span>{s.label}</span>
                  </button>
                );
              })}
            </div>
          ))}
        </div>

        <div className="nav-spacer" />

        {/* 全局常驻【本地网关与实时请求流底座】 */}
        <GatewayPod onGo={navigate} onOpenActivity={() => navigate(go("overview"))} />

        <button
          type="button"
          className="nav-item"
          aria-current={route.section === "settings" ? "page" : undefined}
          onClick={() => navigate(go("settings"))}
        >
          <Icon name={settings.icon} className="nav-icon" />
          <span>{settings.label}</span>
          {broken ? <span className="nav-badge" title="有需要处理的问题" /> : null}
        </button>
      </nav>

      {/* 主视图画布（直接拉满到顶） */}
      <div className="workspace-container">
        <main className={`main${route.section === "playground" ? " main-flush" : ""}`}>
          {route.section === "overview" ? <OverviewPage onGo={navigate} /> : null}
          {route.section === "channels" ? <ChannelsPage route={route} onGo={navigate} /> : null}
          {route.section === "models" ? <ModelsPage route={route} onGo={navigate} /> : null}
          {route.section === "playground" ? <PlaygroundPage route={route} onGo={navigate} /> : null}
          {route.section === "connect" ? <ConnectPage route={route} onGo={navigate} /> : null}
          {route.section === "gateway" ? <OverviewPage onGo={navigate} /> : null}
          {route.section === "accounts" ? <AccountsPage route={route} onGo={navigate} /> : null}
          {route.section === "switcher" ? <SwitcherPage route={route} onGo={navigate} /> : null}
          {route.section === "panel" ? <CursorPanelPage route={route} onGo={navigate} /> : null}
          {route.section === "settings" ? <SettingsPage route={route} onGo={navigate} /> : null}
        </main>
      </div>

      {/* 实时请求流抽屉 */}
      <LiveActivityDrawer open={activityOpen} onClose={() => setActivityOpen(false)} />

      {/* 全局 ⌘K 搜索浮层 */}
      <CommandPalette open={cmdOpen} onClose={() => setCmdOpen(false)} onGo={navigate} />

      {/* 首次启动把会弹系统窗的权限一次问完 */}
      <PreflightModal />
      <UpdateNotice />
      {/* 所有「确认？」都从这里弹 */}
      <ConfirmHost />
    </div>
  );
}
