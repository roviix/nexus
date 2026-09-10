/**
 * 平台判断错了，外壳就会画错（红绿灯留白、拖拽区），而且是一眼可见的那种错。
 * 这里钉住三个系统真实的 UA 形状。
 */
import { describe, expect, it } from "vitest";
import { detectPlatform, homePath } from "./platform";

const UA = {
  // WebView2（Tauri 在 Windows 上用的壳）。注意它的 UA 里同时含 "Windows NT"。
  windows:
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0",
  // WKWebView（macOS）。
  macos:
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.0 Safari/605.1.15",
  linux:
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
};

describe("detectPlatform", () => {
  it("认得三个系统真实的 UA", () => {
    expect(detectPlatform(UA.windows)).toBe("windows");
    expect(detectPlatform(UA.macos)).toBe("macos");
    expect(detectPlatform(UA.linux)).toBe("linux");
  });

  it("认不出来时当 linux，而不是崩掉或当成 macOS", () => {
    // 判断只用来选样式，猜错的代价是留白不对；抛异常的代价是整个界面白屏。
    expect(detectPlatform("")).toBe("linux");
  });
});

describe("homePath", () => {
  it("按系统写家目录路径", () => {
    expect(homePath(".claude/settings.json", "windows")).toBe(
      "%USERPROFILE%\\.claude\\settings.json",
    );
    expect(homePath(".claude/settings.json", "macos")).toBe("~/.claude/settings.json");
    expect(homePath(".codex/config.toml", "linux")).toBe("~/.codex/config.toml");
  });
});
