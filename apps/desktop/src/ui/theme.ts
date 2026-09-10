/**
 * 外观：跟随系统 / 浅色 / 深色。
 *
 * 主题挂在 `<html data-theme>` 上，tokens.css 按这个属性切两套变量；组件样式只认变量，
 * 所以切主题就是改一个属性。偏好存在 localStorage 而不是走 Rust 的设置表：它是纯粹的
 * 显示偏好，而且必须在**首帧之前**就定下来 —— 走一趟 IPC 再改会先画出暗色再闪成亮色。
 *
 * 「跟随系统」听 `prefers-color-scheme`。同时把 Tauri 窗口本身的主题也同步过去：
 * 原生控件（滚动条、下拉菜单、右键菜单）跟网页是两套渲染，不同步的话亮色页面上会
 * 弹出黑色的菜单。
 */
import { useSyncExternalStore } from "react";

export type ThemePref = "system" | "light" | "dark";
export type Theme = "light" | "dark";

const KEY = "nexus.theme";
const PREFS: readonly ThemePref[] = ["system", "light", "dark"];

export const THEME_OPTIONS: { id: ThemePref; label: string; hint: string }[] = [
  { id: "system", label: "跟随系统", hint: "和系统外观一致" },
  { id: "light", label: "浅色", hint: "白纸、翠绿" },
  { id: "dark", label: "深色", hint: "近黑、薄荷" },
];

export function isThemePref(v: unknown): v is ThemePref {
  return typeof v === "string" && (PREFS as readonly string[]).includes(v);
}

export function readPref(): ThemePref {
  try {
    const v = localStorage.getItem(KEY);
    return isThemePref(v) ? v : "system";
  } catch {
    return "system";
  }
}

/** 偏好 + 系统当前是不是深色 → 实际用哪套。纯函数，好测。 */
export function resolveTheme(pref: ThemePref, systemDark: boolean): Theme {
  if (pref === "system") return systemDark ? "dark" : "light";
  return pref;
}

const media = typeof matchMedia === "function" ? matchMedia("(prefers-color-scheme: dark)") : null;

let pref: ThemePref = readPref();
const listeners = new Set<() => void>();

function notify() {
  for (const fn of listeners) fn();
}

export function currentTheme(): Theme {
  return resolveTheme(pref, media?.matches ?? true);
}

function paint(theme: Theme) {
  document.documentElement.dataset.theme = theme;
}

/**
 * 同步 Tauri 窗口主题。用动态 import：预览脚手架跑在普通浏览器里没有 Tauri，
 * 这个模块也不该因为它加载失败就整个挂掉。
 */
function syncWindow(theme: Theme | null) {
  import("@tauri-apps/api/window")
    .then(({ getCurrentWindow }) => getCurrentWindow().setTheme(theme))
    .catch(() => {
      /* 浏览器预览 / 权限没开：页面主题已经切了，窗口的那一层不影响使用。 */
    });
}

function apply() {
  paint(currentTheme());
  syncWindow(pref === "system" ? null : pref);
  notify();
}

export function setThemePref(next: ThemePref) {
  if (next === pref) return;
  pref = next;
  try {
    localStorage.setItem(KEY, next);
  } catch {
    /* 私密模式等存不住的环境：本次会话仍然生效。 */
  }
  apply();
}

let started = false;

/**
 * 应用启动时调一次，**在 React 渲染之前**：先把 `data-theme` 写上，首帧就是对的颜色。
 * 可选的 `force` 给预览脚手架用（`?theme=light` 直接看某一套）。
 */
export function initTheme(force?: ThemePref) {
  if (force) pref = force;
  paint(currentTheme());
  if (started) return;
  started = true;
  syncWindow(pref === "system" ? null : pref);
  media?.addEventListener("change", () => {
    if (pref === "system") apply();
  });
}

function subscribe(fn: () => void) {
  listeners.add(fn);
  return () => {
    listeners.delete(fn);
  };
}

/** 当前偏好与实际主题。任何地方改了偏好，所有用到它的组件一起更新。 */
export function useTheme(): { pref: ThemePref; theme: Theme; setPref: (p: ThemePref) => void } {
  const p = useSyncExternalStore(subscribe, () => pref, () => pref);
  const t = useSyncExternalStore(subscribe, currentTheme, currentTheme);
  return { pref: p, theme: t, setPref: setThemePref };
}
