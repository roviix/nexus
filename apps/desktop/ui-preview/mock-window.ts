/** 假的 `@tauri-apps/api/window`：只有主题同步会用到它，浏览器里没有窗口可同步。 */
export function getCurrentWindow() {
  return {
    async setTheme(theme: "light" | "dark" | null | undefined): Promise<void> {
      console.info("[preview] window.setTheme", theme ?? "system");
    },
  };
}
