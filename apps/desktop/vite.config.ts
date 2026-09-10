// vitest/config 的 defineConfig 是 vite 那个的超集，多认一个 `test` 段。
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// Tauri 在固定端口上等前端；端口被占时直接失败而不是悄悄换一个，
// 否则 devUrl 对不上，窗口会是一片白。
const PORT = 1420;

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: PORT,
    strictPort: true,
    watch: {
      // src-tauri 由 cargo 自己看着，Vite 再看一遍只会互相触发重建。
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    // 主窗口不加载远程内容，产物全打进包里。
    target: "es2022",
    sourcemap: false,
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts", "src/**/*.test.tsx"],
  },
});
