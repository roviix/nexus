/**
 * 主题选错了整个界面就是错的颜色，而且首帧就错。钉住三件事：
 * 偏好 → 实际主题的换算、存取偏好时对脏数据的容错、切换后 `<html data-theme>` 确实变了。
 */
import { beforeEach, describe, expect, it } from "vitest";
import { initTheme, isThemePref, readPref, resolveTheme, setThemePref } from "./theme";

describe("resolveTheme", () => {
  it("跟随系统时看系统，否则听用户的", () => {
    expect(resolveTheme("system", true)).toBe("dark");
    expect(resolveTheme("system", false)).toBe("light");
    expect(resolveTheme("light", true)).toBe("light");
    expect(resolveTheme("dark", false)).toBe("dark");
  });
});

describe("isThemePref / readPref", () => {
  beforeEach(() => localStorage.clear());

  it("只认三个值", () => {
    expect(isThemePref("light")).toBe(true);
    expect(isThemePref("dark")).toBe(true);
    expect(isThemePref("system")).toBe(true);
    expect(isThemePref("auto")).toBe(false);
    expect(isThemePref(null)).toBe(false);
  });

  it("存过的读得回来，没存过或存坏了都当跟随系统", () => {
    expect(readPref()).toBe("system");
    localStorage.setItem("nexus.theme", "light");
    expect(readPref()).toBe("light");
    localStorage.setItem("nexus.theme", "blue");
    expect(readPref()).toBe("system");
  });
});

describe("initTheme / setThemePref", () => {
  it("把主题写到 <html data-theme>，切换立刻生效并落盘", () => {
    initTheme("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
    setThemePref("light");
    expect(document.documentElement.dataset.theme).toBe("light");
    expect(localStorage.getItem("nexus.theme")).toBe("light");
    setThemePref("dark");
    expect(document.documentElement.dataset.theme).toBe("dark");
  });
});
