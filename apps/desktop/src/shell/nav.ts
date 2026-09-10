/**
 * 导航模型。外壳、各页的跳转、预览脚手架的 `?route=` 都读这一份。
 *
 * 信息架构按「这东西是什么」分成三组，而不是按「谁在用」：
 *
 * - **中转 API**：对外只有一副面孔 —— 模型广场（能调什么）、游乐场（直接用起来：
 *   多轮对话、生图，记录都留在本机）、接入（怎么配客户端），再往下是本机那台引擎：
 *   本地网关（开关、地址、口令、号的接力）。
 * - **账号**：号池本身（按平台分：Cursor 的号、ChatGPT 的号——两种都是网关背后的号源），
 *   以及把 Cursor 的号写进 IDE 的切号。
 * - **补丁**：Sand 通道。它给 IDE 打补丁走 bot 额度，跟号池、网关都没有关系，
 *   所以自己一组，不再跟切号、网关挤在一个「使用」屋顶下。
 *
 * 概览压顶，设置压尾。
 */

export type Section =
  | "overview"
  | "models"
  | "playground"
  | "connect"
  | "gateway"
  | "accounts"
  | "switcher"
  | "sand"
  | "settings";

/**
 * 账号页的两个平台。Cursor 的号能切进 IDE、能进切号池 / 网关号池；ChatGPT 的号
 * 只有一个用途——给本机网关跑 Codex 模型。Grok Build / Kiro 同样只喂网关，各自一页签。
 */
export type AccountPlatform = "cursor" | "chatgpt" | "grok" | "kiro";

/**
 * 游乐场的三个子项。对话与图片是两种会话（走的接口不同：chat completions / images），
 * 资产是所有生成图片的画廊。它们在侧栏里挂在「游乐场」下面。
 */
export type PlaygroundView = "chat" | "image" | "video" | "assets";

/**
 * 设置页顶部的页签。放进地址（`#settings/advanced`）是为了让别的页能直达：概览上的
 * 「设置路径」落到高级、「全部记录」落到日志，而不是把人丢在设置首页再让他找。
 */
export type SettingsTab = "general" | "permissions" | "advanced" | "log" | "about";

/**
 * 本地网关的下钻页：`#gateway/pool` 是 Cursor 号池。它曾经摊在网关页「通道」卡的 Cursor 那一行
 * 底下（点一下展开一格账号卡），一张卡里套着一列卡、抽屉又从卡里弹出来，层次乱；号多了那一格
 * 比页面上其它所有东西加起来还长。拆成自己一页：网关页只剩「开没开」和「几条通道各什么光景」，
 * 号池有完整的宽度摆卡、有自己的动作行，地址栏也能直达。
 */
export type GatewaySub = "pool";

export interface Route {
  section: Section;
  /**
   * 只对 `connect` / `models` 有意义：网关里的哪一条通道（平台 id：`cursor` / `chatgpt` …）。
   * 缺省由页面按状态定（默认 Cursor）。
   */
  channel?: string;
  /**
   * 只对 `connect` / `models` / `playground` 有意义：预选的模型 id
   * （从模型广场「用它接入」/「试一下」带过来）。
   */
  model?: string;
  /** 只对 `switcher` 有意义：池内确认切换，池外预选加入。 */
  email?: string;
  /** 只对 `accounts` 有意义：哪个平台的号。缺省是 Cursor。 */
  platform?: AccountPlatform;
  /** 只对 `playground` 有意义：哪个子项。缺省由页面按上次停留的地方决定。 */
  view?: PlaygroundView;
  /** 只对 `settings` 有意义：哪个页签。缺省是通用。 */
  tab?: SettingsTab;
  /** 只对 `gateway` 有意义：下钻到哪一页。缺省是网关主页。 */
  sub?: GatewaySub;
}

export const DEFAULT_ROUTE: Route = { section: "overview" };

export interface SectionMeta {
  id: Section;
  label: string;
  /** 侧栏图标名（`ShellIcon` / `Icon` 的 name）。 */
  icon: string;
}

export const SECTIONS: SectionMeta[] = [
  { id: "overview", label: "概览", icon: "grid" },
  { id: "models", label: "模型广场", icon: "layers" },
  { id: "playground", label: "游乐场", icon: "flask" },
  { id: "connect", label: "接入", icon: "plug" },
  { id: "gateway", label: "本地网关", icon: "gateway" },
  { id: "accounts", label: "账号", icon: "accounts" },
  { id: "switcher", label: "切号", icon: "switcher" },
  { id: "sand", label: "Sand 通道", icon: "sand" },
  { id: "settings", label: "设置", icon: "settings" },
];

export interface NavGroup {
  id: string;
  /** 分组小标题；顶部那组没有。 */
  label: string | null;
  items: Section[];
}

/** 侧栏的分组。设置不在这里 —— 它固定在侧栏底部。 */
export const NAV_GROUPS: NavGroup[] = [
  { id: "top", label: null, items: ["overview"] },
  { id: "relay", label: "中转 API", items: ["models", "playground", "connect", "gateway"] },
  { id: "accounts", label: "账号", items: ["accounts", "switcher"] },
  { id: "patch", label: "补丁", items: ["sand"] },
];

export interface PlaygroundViewMeta {
  id: PlaygroundView;
  label: string;
  icon: string;
}

/** 侧栏里「游乐场」下面的四行。 */
export const PLAYGROUND_VIEWS: PlaygroundViewMeta[] = [
  { id: "chat", label: "对话", icon: "chat" },
  { id: "image", label: "图片", icon: "image" },
  { id: "video", label: "视频", icon: "play" },
  { id: "assets", label: "资产", icon: "gallery" },
];

export interface AccountPlatformMeta {
  id: AccountPlatform;
  label: string;
}

/** 账号页顶部的平台页签。 */
export const ACCOUNT_PLATFORMS: AccountPlatformMeta[] = [
  { id: "cursor", label: "Cursor" },
  { id: "chatgpt", label: "ChatGPT" },
  { id: "grok", label: "Grok Build" },
  { id: "kiro", label: "Kiro" },
];

export interface SettingsTabMeta {
  id: SettingsTab;
  label: string;
  /** `Icon` 的 name。 */
  icon: string;
}

/** 设置页顶部的五个页签，按「常改 → 少改 → 只看」排。 */
export const SETTINGS_TABS: SettingsTabMeta[] = [
  { id: "general", label: "通用", icon: "settings" },
  { id: "permissions", label: "权限", icon: "shield" },
  { id: "advanced", label: "高级", icon: "folder" },
  { id: "log", label: "日志", icon: "list" },
  { id: "about", label: "关于", icon: "info" },
];

const SECTION_IDS = new Set<string>(SECTIONS.map((s) => s.id));
const VIEW_IDS = new Set<string>(PLAYGROUND_VIEWS.map((v) => v.id));
const TAB_IDS = new Set<string>(SETTINGS_TABS.map((t) => t.id));
const PLATFORM_IDS = new Set<string>(ACCOUNT_PLATFORMS.map((p) => p.id));
/** 认 `?channel=` / `?model=` 的页：`#connect?channel=chatgpt&model=gpt-5`。 */
const CHANNELED = new Set<Section>(["connect", "models"]);
/** 子路径是平台的页：`#accounts/chatgpt`。 */
const PLATFORMED = new Set<Section>(["accounts"]);
/**
 * 游乐场的子路径是子项，模型退到 query：`#playground/image?model=gpt-image-1`。
 * 子项才是这一页的「在哪」，模型只是带进来的一个预选。
 */
const VIEWED = new Set<Section>(["playground"]);
/** 认 `?email=` 的页：`#switcher?email=a@b.com`。 */
const EMAILED = new Set<Section>(["switcher"]);
/** 子路径是页签的页：`#settings/advanced`。 */
const TABBED = new Set<Section>(["settings"]);
/** 子路径是下钻页的页：`#gateway/pool`。 */
const SUBBED = new Set<Section>(["gateway"]);
const SUB_IDS = new Set<string>(["pool"]);

/**
 * 上一版的地址还可能留在书签或预览链接里。认出来、换成新地址，而不是掉回概览。
 */
const LEGACY: Record<string, Section> = {
  "use/switcher": "switcher",
  "use/gateway": "gateway",
  "use/sand": "sand",
  use: "switcher",
  "connect/local": "connect",
  "models/local": "models",
};

export function sectionMeta(id: Section): SectionMeta {
  return SECTIONS.find((s) => s.id === id)!;
}

/**
 * `#connect?model=claude-sonnet-5` 这种 hash → 路由。认不出的部分回默认值而不是报错：
 * 用户手改地址栏、旧版本留下的书签，都不该把界面弄成空白。
 */
export function parseRoute(hash: string): Route {
  const raw = hash.replace(/^#\/?/, "");
  const [pathPart = "", queryPart = ""] = raw.split("?", 2);
  const legacy = LEGACY[pathPart];
  const [head = "", tail = ""] = pathPart.split("/", 2);
  const section: Section = legacy ?? (SECTION_IDS.has(head) ? (head as Section) : DEFAULT_ROUTE.section);
  const route: Route = { section };
  const query = new URLSearchParams(queryPart);
  if (CHANNELED.has(section)) {
    const model = query.get("model")?.trim();
    if (model) route.model = model;
    const channel = query.get("channel")?.trim();
    if (channel) route.channel = channel;
  }
  if (VIEWED.has(section)) {
    if (VIEW_IDS.has(tail)) route.view = tail as PlaygroundView;
    const model = query.get("model")?.trim();
    if (model) route.model = model;
  }
  if (EMAILED.has(section)) {
    const email = query.get("email")?.trim();
    if (email) route.email = email;
  }
  if (PLATFORMED.has(section) && PLATFORM_IDS.has(tail)) route.platform = tail as AccountPlatform;
  if (TABBED.has(section) && TAB_IDS.has(tail)) route.tab = tail as SettingsTab;
  if (!legacy && SUBBED.has(section) && SUB_IDS.has(tail)) route.sub = tail as GatewaySub;
  return route;
}

export function routeHash(r: Route): string {
  let hash = `#${r.section}`;
  if (CHANNELED.has(r.section)) {
    const q = new URLSearchParams();
    if (r.channel) q.set("channel", r.channel);
    if (r.model) q.set("model", r.model);
    const qs = q.toString();
    if (qs) hash += `?${qs}`;
  }
  if (VIEWED.has(r.section)) {
    if (r.view) hash += `/${r.view}`;
    if (r.model) hash += `?model=${encodeURIComponent(r.model)}`;
  }
  if (EMAILED.has(r.section) && r.email) hash += `?email=${encodeURIComponent(r.email)}`;
  if (PLATFORMED.has(r.section) && r.platform) hash += `/${r.platform}`;
  if (TABBED.has(r.section) && r.tab) hash += `/${r.tab}`;
  if (SUBBED.has(r.section) && r.sub) hash += `/${r.sub}`;
  return hash;
}

/** 从任何地方「去某一页」用这个，别手拼对象。 */
export function go(
  section: Section,
  opts?: {
    channel?: string;
    model?: string;
    email?: string;
    platform?: AccountPlatform;
    view?: PlaygroundView;
    tab?: SettingsTab;
    sub?: GatewaySub;
  },
): Route {
  const r: Route = { section };
  if ((CHANNELED.has(section) || VIEWED.has(section)) && opts?.model?.trim()) r.model = opts.model.trim();
  if (CHANNELED.has(section) && opts?.channel?.trim()) r.channel = opts.channel.trim();
  if (VIEWED.has(section) && opts?.view) r.view = opts.view;
  if (EMAILED.has(section) && opts?.email?.trim()) r.email = opts.email.trim();
  if (PLATFORMED.has(section) && opts?.platform) r.platform = opts.platform;
  if (TABBED.has(section) && opts?.tab) r.tab = opts.tab;
  if (SUBBED.has(section) && opts?.sub) r.sub = opts.sub;
  return r;
}
