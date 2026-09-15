import { useCallback, useEffect, useState } from "react";
import { app } from "./ipc/api";
import type { AppStatus } from "./ipc/types";
import { AccountsPage } from "./pages/AccountsPage";
import { ConnectPage } from "./pages/ConnectPage";
import { GatewayPage } from "./pages/GatewayPage";
import { ModelsPage } from "./pages/ModelsPage";
import { OverviewPage } from "./pages/OverviewPage";
import { PlaygroundPage } from "./pages/PlaygroundPage";
import { CursorPanelPage } from "./pages/CursorPanelPage";
import { PreflightModal } from "./pages/settings/PreflightModal";
import { SettingsPage } from "./pages/SettingsPage";
import { SwitcherPage } from "./pages/SwitcherPage";
import { go, NAV_GROUPS, parseRoute, PLAYGROUND_VIEWS, routeHash, sectionMeta, type Route } from "./shell/nav";
import { ShellIcon } from "./shell/ShellIcon";
import { Wordmark } from "./ui/Mark";
import { Icon } from "./ui/primitives";
import { UpdateNotice } from "./UpdateNotice";

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

  useEffect(() => {
    void app
      .status()
      .then(setStatus)
      .catch(() => setStatus(null));
  }, [route.section]);

  // 只有「Cursor 都找不到」这一件事值得在导航上挂红点：它意味着切号和 Sand 都干不了活。
  // 存储后端之类的内部件不往这里放 —— 那是我们的实现细节，不是用户要操心的事。
  const broken = status != null && !status.cursor.dbPresent;
  const settings = sectionMeta("settings");

  return (
    <div className="shell">
      <div className="drag-region" data-tauri-drag-region />

      {/* 不带值时只有事件正好落在这个元素上才算拖拽，不往子节点传；`deep` 才连整棵子树，
          且按钮 / 链接 / 输入框那些天生可点的照旧优先。所以侧栏用前者（露出来的底色能拖，
          导航按钮照常按），品牌区用后者（字标和标本身都是死物，整块让给拖拽）。 */}
      <nav className="sidebar" data-tauri-drag-region>
        <div className="brand" data-tauri-drag-region="deep">
          <Wordmark />
        </div>

        <div className="nav-scroll">
          {NAV_GROUPS.map((g) => (
            <div key={g.id} className="nav-group">
              {g.label ? <div className="nav-label">{g.label}</div> : null}
              {g.items.map((id) => {
                const s = sectionMeta(id);
                // 游乐场是唯一带子项的一级页：停在它上面时把「对话 / 图片 / 资产」摊开，
                // 高亮落在子项上，父项只提亮字色 —— 两层同时亮成一块会读不出层级。
                const open = id === "playground" && route.section === "playground";
                return (
                  <div key={id}>
                    <button type="button" className={`nav-item${open ? " is-open" : ""}`} aria-current={route.section === id && !open ? "page" : undefined} onClick={() => navigate(go(id))}>
                      <ShellIcon name={s.icon} className="nav-icon" />
                      <span>{s.label}</span>
                      {id === "playground" ? <Icon name="chevron" size={12} className={`nav-chevron${open ? " is-open" : ""}`} /> : null}
                    </button>
                    {open ? (
                      <div className="nav-subs">
                        {PLAYGROUND_VIEWS.map((v) => (
                          <button key={v.id} type="button" className="nav-item nav-sub" aria-current={route.view === v.id ? "page" : undefined} onClick={() => navigate(go("playground", { view: v.id }))}>
                            <ShellIcon name={v.icon} className="nav-icon" />
                            <span>{v.label}</span>
                          </button>
                        ))}
                      </div>
                    ) : null}
                  </div>
                );
              })}
            </div>
          ))}
        </div>

        <div className="nav-spacer" />

        <button type="button" className="nav-item" aria-current={route.section === "settings" ? "page" : undefined} onClick={() => navigate(go("settings"))}>
          <Icon name={settings.icon} className="nav-icon" />
          <span>{settings.label}</span>
          {broken ? <span className="nav-badge" title="有需要处理的问题" /> : null}
        </button>
      </nav>

      {/* 游乐场是个工作台不是一页文档：它自己铺满内容区、自己管边距，所以摘掉 .main 的页边距与环境光。 */}
      <main className={`main${route.section === "playground" ? " main-flush" : ""}`} key={route.section}>
        {route.section === "overview" ? <OverviewPage onGo={navigate} /> : null}
        {route.section === "models" ? <ModelsPage route={route} onGo={navigate} /> : null}
        {route.section === "playground" ? <PlaygroundPage route={route} onGo={navigate} /> : null}
        {route.section === "connect" ? <ConnectPage route={route} onGo={navigate} /> : null}
        {route.section === "gateway" ? <GatewayPage route={route} onGo={navigate} /> : null}
        {route.section === "accounts" ? <AccountsPage route={route} onGo={navigate} /> : null}
        {route.section === "switcher" ? <SwitcherPage route={route} onGo={navigate} /> : null}
        {route.section === "panel" ? <CursorPanelPage route={route} onGo={navigate} /> : null}
        {route.section === "settings" ? <SettingsPage route={route} onGo={navigate} /> : null}
      </main>

      {/* 首次启动把会弹系统窗的权限一次问完；只弹一次，跳过也算走完，以后在「设置 → 权限」里再申请。 */}
      <PreflightModal />
      <UpdateNotice />
    </div>
  );
}
