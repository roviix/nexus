/**
 * 游乐场 —— 直接用起来：多轮对话、生图，记录都留在本机。
 *
 * 模型广场回答「能调什么」，接入页回答「客户端怎么配」；这一页回答「好不好用」——
 * 而且走的是和客户端**完全相同**的地址、钥匙与链路（本地网关），这里跑通了
 * 客户端照抄就能跑通。
 *
 * 四个子项挂在侧栏里：**对话**、**图片**、**视频**是三种会话（接口不同：chat completions /
 * images / videos，所以不做「在聊天里顺手出图」——那等于在前端猜意图，和真实链路对不上）；
 * **资产**是所有生成图片与视频的画廊。这一层只管在四者之间切，会话的事在 `Workbench`，画廊在 `AssetsView`。
 */
import { useEffect } from "react";
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

  // 没指定子项（点了侧栏的「游乐场」本身、或旧地址）：回到上次停留的那个。
  // 把它写进地址，侧栏的高亮才落得到子项上。
  useEffect(() => {
    if (route.view) {
      localStorage.setItem(VIEW_KEY, route.view);
      return;
    }
    onGo(go("playground", { view: readView(), model: route.model }));
  }, [route.view, route.model, onGo]);

  const view = route.view ?? readView();
  const hint = route.model ? { model: route.model } : undefined;

  if (view === "assets") {
    return (
      <AssetsView
        onOpenThread={(threadId, kind) => {
          const view = kind === "video" ? "video" : "image";
          rememberSelected(view, threadId);
          onGo(go("playground", { view }));
        }}
        onGoImages={() => onGo(go("playground", { view: "image" }))}
      />
    );
  }
  return <Workbench key={view} kind={view} relay={relay} hint={hint} onGo={onGo} />;
}
