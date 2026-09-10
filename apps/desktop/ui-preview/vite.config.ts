/**
 * 界面预览：不起 Tauri，把 `@tauri-apps/*` 换成本目录的假实现，用浏览器看真页面。
 *
 *   npm run preview:ui            # http://127.0.0.1:1500/?route=models
 *
 * `route` 取 shell/nav 的 hash 写法（overview / models / connect / gateway / playground / accounts /
 * switcher / sand / settings）。另有 `&empty=1`（空态）、
 * `&theme=light`（直接看亮色 / 暗色，不带就照存下的偏好）、
 * `&click=详情`（渲染完自动点几个键，给无头截图用）。
 * 只给看版式与交互，数据全是 fixtures；改了 `src/` 立即热更新。
 */
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// 不引 node:url：这个 tsconfig 没带 Node 类型。`import.meta.url` 是 file:// 绝对地址，
// 直接拼相对路径再去掉协议头就是磁盘路径。
const here = (p: string) => decodeURIComponent(new URL(p, import.meta.url).pathname);

export default defineConfig({
  root: here("."),
  plugins: [react()],
  resolve: {
    alias: {
      "@tauri-apps/api/core": here("./mock-core.ts"),
      "@tauri-apps/api/event": here("./mock-event.ts"),
      "@tauri-apps/api/window": here("./mock-window.ts"),
      "@tauri-apps/plugin-opener": here("./mock-opener.ts"),
    },
  },
  server: { port: 1500, strictPort: true },
});
