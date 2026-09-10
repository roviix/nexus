/**
 * README 配图：用 `ui-preview`（mock core，纯浏览器）把几张主要页面拍下来。
 *
 * 数据全是 `ui-preview/mock-core.ts` 里的 fixtures —— 不连任何上游、不碰真凭证，
 * 所以这些图可以进版本库，也可以随便重拍。
 *
 * playwright **不是本仓库的依赖** —— 为一个偶尔跑一次的配图脚本把它拖进 devDependencies
 * 不划算。临时装一份再跑：
 *
 *   cd apps/desktop && npm run preview:ui          # 另开一个终端，起预览服务
 *   mkdir -p /tmp/shots && cd /tmp/shots && npm i playwright
 *   cp scripts/shoot-screenshots.mjs /tmp/shots/           # ESM 从脚本自己的位置找依赖
 *   SHOTS_OUT="$PWD/docs/images" node /tmp/shots/shoot-screenshots.mjs
 *
 * 用系统装的 Chrome（`channel: "chrome"`），不额外下 playwright 自带的那套浏览器。
 */
import { chromium } from "playwright";
import { mkdir } from "node:fs/promises";

const BASE = process.env.PREVIEW_URL ?? "http://127.0.0.1:1500";
const OUT = process.env.SHOTS_OUT
  ? `${process.env.SHOTS_OUT}/`
  : new URL("../docs/images/", import.meta.url).pathname;

// 宽高按应用窗口的实际比例取。1.5 倍图是体积与清晰度的折中：2 倍时这几张加起来 2.5 MB，
// 对一个要 clone 的仓库来说不值得，而 README 里本来就是缩着显示的。
const VIEWPORT = { width: 1280, height: 800 };
const SCALE = 1.5;

const SHOTS = [
  { name: "overview", route: "overview", theme: "dark" },
  { name: "gateway", route: "gateway", theme: "dark" },
  { name: "connect", route: "connect", theme: "dark" },
  { name: "accounts", route: "accounts", theme: "dark" },
  { name: "switcher", route: "switcher", theme: "dark" },
  // 游乐场默认落在「新对话」空态，拍不出东西；点进 fixtures 里那条已有的会话。
  { name: "playground", route: "playground", theme: "dark", click: "用 Rust 写一个 LRU" },
];

const browser = await chromium.launch({ channel: "chrome" });
const page = await browser.newPage({ viewport: VIEWPORT, deviceScaleFactor: SCALE });

await mkdir(OUT, { recursive: true });

for (const shot of SHOTS) {
  const params = new URLSearchParams({ route: shot.route, theme: shot.theme });
  if (shot.click) params.set("click", shot.click);
  await page.goto(`${BASE}/?${params}`, { waitUntil: "networkidle" });
  // mock core 里每个命令都带假延迟；等它们走完再拍，否则拍到一屏骨架。
  await page.waitForTimeout(shot.click ? 4000 : 2500);
  const file = `${OUT}${shot.name}.png`;
  await page.screenshot({ path: file });
  console.log("拍了", file);
}

await browser.close();
