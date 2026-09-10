/**
 * 这台机器是什么系统。
 *
 * 用 UA 判断而不是问 Rust：外壳的形状（标题栏留白、拖拽区在不在）必须在**首帧**就定下来，
 * 走一趟 IPC 再改会让窗口先歪一下再跳正。UA 在 WKWebView 与 WebView2 里都稳定带系统名，
 * 而我们只需要区分三个大类，不需要版本号那种精度。
 *
 * 只拿它决定外壳形状。要判断某个能力在不在，问 Rust —— UA 骗得过，真实 target 骗不过。
 */
export type Platform = "macos" | "windows" | "linux";

export function detectPlatform(ua: string): Platform {
  if (/Windows/i.test(ua)) return "windows";
  if (/Macintosh|Mac OS X/i.test(ua)) return "macos";
  return "linux";
}

export const PLATFORM: Platform = detectPlatform(
  typeof navigator === "undefined" ? "" : navigator.userAgent,
);

/**
 * 把 `~/x/y` 这类路径写成当前系统看得懂的样子。
 *
 * 用户是要照着它去资源管理器里翻文件的：给 Windows 用户看 `~/.claude/settings.json`
 * 等于没说，`%USERPROFILE%\.claude\settings.json` 才能直接粘进地址栏。
 */
export function homePath(rel: string, platform: Platform = PLATFORM): string {
  return platform === "windows"
    ? `%USERPROFILE%\\${rel.replace(/\//g, "\\")}`
    : `~/${rel}`;
}
