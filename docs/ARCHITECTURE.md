# Nexus · 架构与设计取舍

这份文档解释 Nexus **为什么长成现在这样**。功能清单在 [README](../README.md)，Sand 补丁的
完整决策记录在 [SAND.md](./SAND.md)，这里只放读代码之前该先知道的那些事：分层为什么这么切、
哪些约束是编译期强制的、凭证为什么明文躺在本地库里、以及哪些取舍是有意为之。

代码注释里引用的 `§x.y` 指的是本文的小节号。

---

## 1. 这是什么

### 1.1 一页纸

Nexus 在 `127.0.0.1` 上起一个网关，用用户**自己的** Cursor / ChatGPT / Grok / Kiro 订阅账号做上游，
对外说 OpenAI 与 Anthropic 的标准方言。任何讲这两种方言的客户端都可以直接指过来，不需要再申请 API key。
围绕这条主线还有三件配套的事：管理这些账号（登录、看额度、存凭证）、把 Cursor IDE 的登录态在多个号
之间切换、以及一个应用内的对话工作台用来验证链路是否通。

整个应用是单进程的：Tauri v2 主进程里既跑 Rust 逻辑也挂系统 WebView，没有 sidecar。所有能力
（SQLite、HTTP、进程控制、SSH）都是纯 Rust，这是「不做浏览器自动化」这个决定的直接好处——
否则要拖一个 Node/Playwright 进程一起发布。

**没有服务端。** 没有账户体系、没有遥测、没有云端同步。数据、凭证、账本、备份全在用户自己的
机器上。换机器走本地文件导出导入，不走网络。

### 1.2 边界：不做什么

- **不改 Cursor 的字节**——除了 `nexus-sand` 与 `nexus-crsr`。网关、切号、账号池对 Cursor 本体
  只读不写，Cursor 升级它们照样能用。这两条补丁通道是这条规则仅有的、有意的例外，它们改的是
  Cursor 的**代码**而不是**数据**，所以必须追着 Cursor 版本跑（见 §7.3 / §7.4 与
  [SAND.md](./SAND.md) / [CRSR.md](./CRSR.md)）。
- **不做浏览器自动化**。取 token 走 OAuth：浏览器是用户自己的，应用只做纯 HTTP 轮询（D5）。
- **不做负载均衡**。一个用户一台机器，任何时刻一个号就够（§6.3）。
- **不在主窗口加载远程内容**。需要开网页的地方一律交给系统浏览器。
- **不做 Linux**。不是不能，是 Tauri 在 Linux 上绑 WebKitGTK，双平台 CI 已经够重了。
  CI 里的 Linux job 只跑 lint 与测试，不出包。

`nexus-gateway` 与 `nexus-sand` 都是对更早一版「不做本地网关 / 不做 sand 补丁」的**有意反转**，
两个 crate 的模块注释里各写了一遍理由；`nexus-crsr` 沿用 Sand 那次反转的结论。反转的前提是
它们能被整体拆卸（§3.4）。

---

## 2. 关键决策

### D1 · 桌面框架：Tauri v2

Rust core + 系统 WebView + React/TS 前端。Tauri 是 Rust 桌面栈里打包、签名、更新、深链最成熟的
一个：DMG / NSIS 都有官方 bundler，updater 插件带签名校验。安装包比 Electron 小一大截，
对一个常驻工具很重要。

**代价（接受）**：两个平台的 WebView 不一样（macOS WebKit / Windows WebView2），所以双平台 CI 是
硬要求；Rust 编译慢；Tauri 自身的 bug 率高于 Electron，靠锁版本管住。

纯 Rust GUI（Dioxus / egui / iced / Slint）被否决的原因是打包与平台集成仍然偏 DIY——等于承担了
平台风险却没拿到工具链。

### D2 · 前端：React + TypeScript + Vite

静态 SPA，没有 SSR、没有路由库（`shell/nav.ts` 里一个 hash 路由就够）。设计 token 抽在
`packages/design-tokens`，是一份 CSS 变量。

`apps/desktop/ui-preview/` 把 `@tauri-apps/*` 换成本目录的假实现，不起 Rust 也能在浏览器里跑真页面：

```bash
cd apps/desktop && npm run preview:ui   # http://127.0.0.1:1500/?route=overview
```

这不只是方便，它是前端改动的验收手段——UI 层的行为改动至少要在这里过一遍。

### D3 · 本地存储：SQLite 存数据，也存秘密

`rusqlite` 带 `bundled`，不依赖系统 sqlite 版本。业务表里没有明文凭证，只有 `ref`；秘密全在
`secrets` 表里。为什么不走 OS 钥匙串，见 §9.1——那是一个被推翻过的决定，理由值得完整读一遍。

迁移用 `rusqlite_migration`，版本号单调递增，**只加不改**。

### D4 · Cursor 集成：写它自己的登录态，不逆向不 patch

切号 = 写 `state.vscdb` 里的 `cursorAuth/*` 键 + `storage.json` 里的 `telemetry.*` 机器码。
这是 Cursor **自己**的登录态，官方登录就往这写。

**为什么这是长期主干**：不逆向协议、不改二进制，Cursor 升级不失效。这是全仓库唯一"装完一直能用"
的机制，所以它不能依赖任何会随版本漂移的东西。

### D5 · 取 token：OAuth 深链 + 轮询

`loginDeepControl?challenge=&uuid=` 加 `api2.cursor.sh/auth/poll`：应用生成 PKCE 挑战、用系统
浏览器打开登录页、然后在后台纯 HTTP 轮询结果。浏览器是用户真人真浏览器，天然绕开机器人识别，
Rust 侧只要一个 `reqwest`。

自动填表（Playwright 之类）被否决：脆弱、要拖一个 Node 运行时、而且正面撞机器人防护。

### R1 · 切号与账号池互不依赖

`nexus-switcher` 不 `use nexus_accounts`，反之亦然。**这条约束由 Cargo 依赖图强制执行**，
不是靠自觉——两个 crate 的 `Cargo.toml` 里各写了一行注释说明这件事。

理由是产品层面的：切号本是「我要登进 IDE 的那几个号」，账号池是「我托管了凭证的所有号」，
它们是两个概念，合并会让「删掉一个账号」这种操作的语义变得没法解释。两者之间唯一的数据通路是
`accounts_add_to_switch_book` 命令里那一次显式拷贝，走的是 UI 层。

---

## 3. 架构

### 3.1 分层

```
┌──────────────────────────────────────────────────────────────────┐
│  apps/desktop/src            React + TS 前端                      │
├──────────────────────────────────────────────────────────────────┤
│  apps/desktop/src-tauri      IPC 边界：命令、事件、capabilities    │
├──────────┬──────────┬──────────┬──────────┬──────────┬───────────┤
│ switcher │ accounts │ chatgpt  │ gateway  │ playground│  connect  │  业务 crate
│          │          │ grok kiro│  grokbot │           │           │  （互不依赖）
│          │          │          │sand crsr │           │           │
├──────────┴────┬─────┴──────────┴──────────┴──────────┴───────────┤
│ nexus-cursor  │  nexus-store（SQLite：数据 + 秘密 + 迁移）         │  基础能力
├───────────────┴──────────────────────────────────────────────────┤
│  nexus-core   领域类型 / 错误 / id / 时间 / 机器码（无 IO）         │
└──────────────────────────────────────────────────────────────────┘
```

**依赖只能向下。** 业务 crate 之间原则上零依赖；例外都是显式的、单向的，且写在 `Cargo.toml` 的
注释里（比如 `nexus-gateway` 要从 `nexus-accounts` / `nexus-chatgpt` 取号，`nexus-playground`
借 `nexus_gateway::playground` 解 SSE 而不是写第二份解析器）。

### 3.2 crate 职责

| crate | 管什么 |
|---|---|
| `nexus-core` | `Email`、`AccountId`、`MachineProfile`、`AppError`、`Clock`、`Secret`。纯类型，无 IO。 |
| `nexus-store` | SQLite 连接池、迁移、`SecretStore`、活动日志、设置、整库备份。 |
| `nexus-cursor` | 定位 Cursor 安装、读写 `state.vscdb` 与 `storage.json`、退出与启动进程。 |
| `nexus-switcher` | 切号本、auth 备份、切号编排（热切 / 冷切）。 |
| `nexus-accounts` | Cursor 账号池：OAuth、刷 token、拉用量、导入导出。 |
| `nexus-chatgpt` | ChatGPT 订阅号：登录、刷 token、Codex 后端协议。 |
| `nexus-grok` / `nexus-kiro` | 对应平台的账号、OAuth、协议、额度。 |
| `nexus-grokbot` | Grok Bot 额度凭证的获取与维护。 |
| `nexus-gateway` | 本地网关：方言口、透传口、通道注册表、额度接力、账本。 |
| `nexus-playground` | 游乐场的线程 / 消息 / 图片 / 视频存储与编排。 |
| `nexus-connect` | 一键接入：改 Claude Code / Codex / OpenCode / Grok CLI 的配置文件。 |
| `nexus-sand` | Sand 补丁引擎（本机 + 远程 SSH）。 |
| `nexus-crsr` | CRSR 补丁：原生 Agent 面板改用 `crsr_` API Key 兑的票据。复用 sand 的安装器骨架。 |

### 3.3 IPC 约定

- 命令命名 `<模块>_<动作>`：`switcher_switch_to`、`accounts_start_oauth`、`gateway_channel_set_current`。
- **全部返回 `Result<T, AppError>`。** `AppError` 带三段：`code` 机器可读（前端按它分支）、
  `message` 直接显示给人、`hint` 说下一步该做什么。`hint` 不是装饰——错误发生的那一刻是我们最
  清楚该怎么办的时候，把它写进结构里比让界面去猜要好。`ErrorCode` **只增不改**，改一个既有变体
  的名字等于悄悄改了前端的判断条件。
- 长任务（切号、OAuth 轮询、批量刷用量、装补丁）用 **Tauri events** 推进度，不让前端轮询。
- **秘密永不经过 IPC 进前端**，除非用户显式点「显示明文」——那一次会记进活动日志。
- **命令不能阻塞 UI 线程。** Tauri 的同步命令跑在主线程上，一个几百毫秒的 SQLite 事务就能让
  窗口卡住。凡是会做 IO 的命令都要 `async` 或者 `spawn_blocking`，包括自定义协议处理器
  （曾经有一条图片协议的路藏在这里，表现是滚动资产页时整个窗口冻住）。

### 3.4 可拆卸的边界

`nexus-gateway`、`nexus-sand` 与 `nexus-crsr` 是要跟着第三方协议 / bundle 变化的模块，也就是
**会定期坏掉**的那几个。它们因此被要求：

1. 只向下依赖，不改任何现有 crate（`nexus-crsr` → `nexus-sand` 是一条显式的单向复用）；
2. 默认关闭，用户不开就完全不起作用；
3. 坏了只影响自己——网关挂了，切号与账号池照常；补丁失配了，拒装而不是装坏。

这条约束是那几次「有意反转」的前提。放宽它之前先想清楚坏掉的那天怎么办。

### 3.5 Tauri 安全配置

- `capabilities/` 只给主窗口本应用的命令加 `core:event`。**不给** fs / http 通用能力，
  网络全在 Rust 侧；开系统浏览器也不给前端（需要它的只有 OAuth，从 Rust 侧发起）。
- CSP 严格，`default-src 'self'`。主窗口不加载任何远程 URL。
- 更新包走 minisign 签名校验，公钥内置在 `tauri.conf.json` 里。

---

## 4. Cursor 切号

### 4.1 热切与冷切

**默认热切**：Cursor 在跑、且不换机器码时，把登录态交给 Cursor 自己吃进内存，不退出、
不打断进行中的任务。

```
热切（默认）  备份当前 auth → 深链 cursor://cursorAuth?route=login&… → 轮询确认 accessToken 已变 → 补写展示键
冷切（兜底）  备份 → 退出 Cursor 并等进程消失 → [单事务] 清旧 auth + 写新 auth → [可选] 写机器码 → 启动
```

什么时候冷切：① Cursor 没在跑（只能写盘再启动）；② 用户打开了「切换时同时切机器码」——机器码
启动时读一次就缓存，热切换不了。

几条不显然的细节：

- **备份只备 auth 键。** `state.vscdb` 里有会话历史，能到几十 GB，整文件备份不现实。备份的值
  进 `secrets` 表，索引进 `auth_backups`，只留最近 N 份。
- **热切之后要补写展示键。** 深链那条路只写 token 和订阅档，**不碰** `cachedEmail` /
  `cachedSignUpType` / `cachedScopedProfile`；而 Cursor 读邮箱的函数是「缓存有就直接返回、
  不问服务端」，这几把键只在 logout 时才清。结果是 token 已经换了号、菜单里还是旧号的名字，
  **而且不会自愈**。修法：确认 token 落盘后，按目标号把 `nexus_cursor::DISPLAY_KEYS` 写一遍，
  目标号没有的键要**删掉**（Cursor 命中 miss 会自己重拉）。Cursor 在跑时写这几把键是安全的，
  它内存里没有副本。
- **清旧与写新在同一个事务里**（冷切）。任何时刻进程被杀，库要么是切换前、要么是切换后，
  不存在「已清未写」的未登录态。
- **一机一码是默认。** 本机机器码不随切号改动——同机换号把账号漂到多套指纹上会触发
  `too many computers`。「切换时同时切机器码」留作高级选项。真机的原始机器码在第一次动手前
  就存下来了，**永不覆盖**，随时可还原。
- **只收有 refresh 的号。** 仅会话的号凑不出 Cursor 要的那对 token，写进去等于让它在下一次
  续期时掉登录，而那批号掉了找不回来。判据是 `can_write_cursor_login()`，理由见 §5.1。
- **只由用户点击触发。** 进程内没有任何自动路径会调用切号。

### 4.2 Cursor 的本地状态

| 项 | 位置 |
|---|---|
| 登录态库 | macOS `~/Library/Application Support/Cursor/User/globalStorage/state.vscdb`；Windows `%APPDATA%\Cursor\…`。SQLite，WAL |
| 表结构 | `ItemTable(key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB)`，值是 text |
| auth 键 | `cursorAuth/accessToken`、`refreshToken`、`cachedEmail`、`cachedSignUpType`、`stripeMembershipType`、`stripeSubscriptionStatus`、`stripeMembershipAuthId`、`cachedScopedProfile`、`cachedUserId`、`onboardingDate`（写入白名单） |
| 机器码 | `User/globalStorage/storage.json` 里的 `telemetry.machineId` / `macMachineId` / `devDeviceId` / `sqmId`，外加 `<Cursor>/machineid` 文件 |
| 版本 | `Cursor.app/Contents/Resources/app/product.json` |

**键名漂移是这条路唯一真实的风险。** 启动时做一次 dry-run 自检（预期的键都读得到吗），
不通过就把切号降级成只读并明说原因，而不是等到写坏了再报错。

排障时这几个环境变量很有用：`CURSOR_USER_DIR`、`CURSOR_STATE_DB`（指到副本上可以安全地试切号）、
`CURSOR_APP_PATH`。

---

## 5. 账号池

### 5.1 托管什么

**门槛是邮箱 + 一样能证明身份的东西**：refresh token、Cursor 密码、还活着的 session token、
或者一把 `crsr_` API Key。只有 session token 的号（token 导入、没密码）在有效期内是真能查用量、
真能进网关的，扔掉太浪费；但它能干什么要分清楚，见下面三条判据。

状态只反映**能不能用**：`active` / `needs_login` / `dead`。「这个号是哪来的、给谁了」是备注，
不是状态——把它们混在一起，界面就没法回答「现在有几个号能用」这种最常问的问题。

**三条判据，从宽到严。** 它们不是同义词，混用过一次就赔掉了一批号：

| 判据 | 条件 | 谁在用 |
|---|---|---|
| `can_query_usage()` | refresh \| 活着的 access \| `crsr_` | 刷用量、凭证页 |
| `has_usable_session()` | 非 dead 且（refresh \| 活着的 access） | 网关号池、Grok Bot 换额度 |
| `can_write_cursor_login()` | 非 dead 且**有 refresh** | 切号本、写 Cursor 登录态 |

最严的那条是被一个真实事故校正出来的。Cursor 的登录态要**成对** token（`accessToken` +
`refreshToken`）并且会自己续期，仅会话的号凑不出这一对，早先的做法是把 access 复制一份去占
refresh 那一格。Cursor 拿这个假 refresh 去续期必然 401，然后掉登录——对有密码的号这只是
「重新粘一份」，但对 token 导入、没密码、接不了验证码的号，授权链整条走不通，掉了就再也拿不
回来。切号那一步把一个还有几十天寿命的号当场变成废号。所以规矩是：**绝不拿 access 去占
refresh 那一格**，仅会话的号切号入口直接置灰，界面说清原因和替代路径。网关不受影响，它走
`has_usable_session()`——借一把会话发请求而已，不写任何登录态。

**替代路径：趁 access 还活着铸一把 `crsr_`。** `DashboardService/CreateUserApiKey` 只认
`Bearer access_token`，不要密码、不要验证码、不要重新登录，这是仅会话号唯一的保命出口
（`AccountsService::mint_api_key`，默认 90 天）。铸完这个号的额度就不再挂在一个会死的凭证上：
查用量、进网关、走 CRSR 通道都能接着用。边界必须说清——铸 key **换不回切号能力**，`crsr_`
兑出来的是 `api_key_token` 而不是 `WorkosCursorSessionToken`，登不进 Cursor。

**堵入口不够，还要认存量。** 规矩生效之前收录的档案还躺在切号本里，切过去照样杀号。指纹很好认：
`accessToken` 和 `refreshToken` 两格一模一样（`AuthBundle::refresh_is_placeholder`）。切号本列表
把这类档标成「无 refresh」并置灰，`switch_to` 里还有一道硬闸——**在碰 Cursor 任何状态之前**就拒绝，
失败时 Cursor 的登录态一个字节都没动。

用量从各平台自己的接口拉。Cursor 这边是 dashboard 系列接口，四个桶（Bot / 总量 / Auto / API）
加两个重置时间（Bot 周额与月账期），两个都显示。

### 5.2 一个总库，两份显式使用池

这是整个账号模型里最容易搞混的一点，也是被用户反馈校正过的一点：

> 账号是**总库**；切号与网关都是用户**明确挑出来的子集**。总库统一不等于每一种使用方式都自动全量。

| 页面 | 列什么 | 动作 |
|---|---|---|
| 账号 | 号池本身 | 加 / 授权 / 查用量 / 看凭证 |
| 切号 | 明确加入切号池的账号，正在登的置顶 | 加入 / 移出 / 写进 Cursor IDE |
| 本地网关 | 明确加入网关池的账号，正在用的置顶 | 加入 / 移出 / 指定接力顺序 |

实现上，**`SwitchProfile` 的存在本身就是切号池的成员关系**，没有第二张会与它双写的成员表；
网关那边是 `lane::Roster`（一组小写邮箱，落 `settings` 表的 `gateway.members`），**默认为空**。

网关池默认为空是个明确的决定。第一版让「授权过的号自动进接力队」，被否了：网关背后是
Claude Code、Codex 这类会自己跑很久的客户端，一个号被它悄悄用光、用户回到 IDE 才发现——这个
代价比多点一次「添加」贵得多，Cursor 里正登着的那个号尤其如此。

因此网关的快照里有两栏容易被忽略但很重要的数据：`missing`（名单里有、此刻拿不到凭证——登出了、
丢了 refresh token、被从号池删了）和 `available`（有凭证、没进名单）。前者在列表里留一行
「接力时跳过」加原因，用户明明加过的号不会凭空少一行；后者是「添加号」弹窗里的候选。
一个号都没有时的 503 也分两种说法：名单是空的（去加）和名单里的号都拿不到凭证（去修）。

### 5.3 多平台：展示一套，能力各自适配

四个平台的账号在界面上共用一条链：`accounts/model.ts` 的 `AccountView` →
`accounts/AccountCard.tsx`（唯一的卡片拼装）→ `accounts/AccountInspector.tsx`（统一的详情入口）。
账号库、切号池、网关池、概览都走这条链，所以「看起来差不多但能力随入口变化」的问题不会再出现。

接新平台时有几条边界要守住：

- **持久化身份是 `(platform, external_id)`**，email 只是可变的展示属性。同一个邮箱在 Cursor、
  ChatGPT、Grok 是三个账号。
- **用量是平台可辨识联合，不是一组全局 capability boolean。** Cursor 有四桶；别的平台可能只有
  一个滚动窗口。每个平台把自己的数据转成 `AccountView`，卡片对联合做穷尽处理。
  只知道一个百分比时就只画一个「总额度」，**不伪造另外三桶**；拿不到用量必须写原因，
  不能画成 0%。
- **不扩 Rust 的 `Account` 大结构继续堆可空字段**，也不把总库和使用池合表。

---

## 6. 本地网关

### 6.1 数据流

```
客户端（Claude Code / Codex / SDK / curl）
   │  OpenAI Chat Completions · Anthropic Messages · OpenAI Responses · OpenAI Images
   ▼
server        127.0.0.1 HTTP，校验本地口令
   ▼
inbound       各方言 → 统一中间表示（按 Anthropic Messages 建模）
   ▼
channel       通道注册表选路（§6.2）
   ▼
lane          额度接力挑号（§6.3）
   ▼
upstream      Cursor InferenceService/Stream · ChatGPT Codex · Grok · Kiro
   ▼
inbound       统一表示 → 客户端方言的 SSE（流式原样支持）
   ▼
ledger        记一行账（§6.5）
```

入站先归一再桥接，而不是每种方言各写一条到每个上游的路——四种方言乘四个上游是十六条路，
归一之后是四加四。

**模型名映射**不是可选项：Claude Code 发 `claude-sonnet-4-5`、Codex 发 `gpt-5`，不映射到上游
认识的名字就是 400。`models` 模块管这件事，也支持强制所有请求走某一个模型。

### 6.2 通道是实体

四个平台最初是 `Gateway` 上四个平行的字段、一条手排的 if 链选路，`/v1/models`、账本、状态快照、
Tauri 命令、前端 API 各抄一遍，加一个平台要摸十几处。现在收成一个实体：

```
Channel         = id + 前缀集合 + 一队号（Lane）+ 一个后端（Upstream）+ 门禁（ChannelGate）
ChannelRegistry = 若干通道 + 用户指定的默认通道
```

**对外目录的主键是 `{通道}/{模型}`**（`cursor/claude-opus-5`），**选路只剩两条规则**，对聊天 /
生图 / 生视频一致：显式前缀强制走那条通道，不看它此刻有没有号；裸名或空模型走**用户设的默认
通道**。别名 `codex/` 与 `xai/` 只在请求前缀里认，不进目录。

早先还有中间一条「无前缀时按『声明拥有该模型且此刻有号』反推归属」。它猜错的时候没法解释：
同一个模型名在两条通道上都有，请求落到哪条取决于哪条恰好还有号，用户看到的是「昨天还好好的，
今天换了个上游」。现在归属写在名字里，默认通道写在设置里，两处都是用户能看见、能改的。

默认通道和注册表分开一把锁（`share_default`）：改设置不用重建通道，也不用重启网关，
正在听的那一发下一次就照新的走。

能力按维度建模（`Capability::{Chat, Image, Video}`），不是一组布尔。视频是异步任务，状态轮询
必须回到**创建它的那个号**——任务是账号维度的，换个号去查就是 404。

前端 `gateway/channels.ts` 的摘要全部从快照算，加第五个平台时这里不用改。

### 6.3 额度接力，不是负载均衡

一个用户一台机器，任何时刻一个号就够。所以策略是：**一直用当前号，额度到线才接力下一个。**

这么做的好处是会话粘性天然成立，换号频率极低，不必解析 protobuf 去做 sticky 路由。

错误分类直接决定处置，这一层的精度比看上去重要：

- `ERROR_RATE_LIMITED_CHANGEABLE` 只冷却「这个号 × 这个模型」，不牵连别的模型；
- quota / billing 类才把整个号标成耗尽；
- 未登录标失效，等用户去重新授权；
- 上游明说「指名的这个模型你出不了」时不能按限流处理——按限流每 5 分钟就会在它身上再白撞一次。

### 6.4 透传口

另开一个 h2c 口，把 `cursor-agent` 的原生 Connect 流量（`aiserver.*` → api2、`agent.v1.*` → api5）
**换身份头后原样转发**。它不解析业务内容，只做身份替换，所以协议细节变化对它影响很小。

`NEXUS_PASSTHROUGH_DUMP_DIR` 可以把入站推理请求体原样落盘（会关掉流式转发）。落的是业务明文，
取证完就删。

### 6.5 账本

方言口每处理一次请求记一行：账号、模型、routed 到的模型、成功与否、状态码、错误类别、
输入输出 token、首字延迟、总耗时、走的哪条通道。概览页的「本地用量」读的就是它。

这是**本地**账本，不上报任何地方。

---

## 7. 其余模块

### 7.1 一键接入

接入页原来只给「复制」，让用户把一段 JSON / TOML 抄回自己的配置文件。应用明明就在这台机器上、
拿得到那些文件，让人手抄是把最容易错的一步留给了人。`nexus-connect` 替他改，规矩是：

- **只动我们那几个键**，其余一字不改；文件本来就坏的（不是合法 JSON / TOML）**拒绝改**，
  让人自己去看；
- **动之前先备份**到 `~/.roviix/backups/clients/<tool>/` 并记一份清单；
- **可撤销**：按清单把备份拷回去；文件是我们建的就删掉；清单丢了就退回「只删我们的键」。

它只认三个字串——地址、钥匙、模型。从哪个号源来是 Tauri 层的事，这个 crate 不依赖 gateway。

### 7.2 游乐场

应用内的多轮对话 + 生图 / 生视频工作台，打的是**和客户端完全相同的地址、钥匙与链路**。
它的用处不是「再做一个聊天界面」，是验证这条链路此刻通不通、这个号还能不能用、这个模型输出什么样。

会话、消息、图片、视频都落本地。视频和图共用一张表与 `nexus-image://` 协议口，靠 MIME 区分。

### 7.3 Sand 补丁

把 Cursor IDE 内置的 Agent 面板也改道到本地网关。决策记录、风险与完整的规则清单在
[SAND.md](./SAND.md)。

这里只说结构上的一点：补丁改的是 Cursor 的**代码**，切号写的是 Cursor 的**数据**。前者追着版本跑，
后者升级不失效——性质不同，所以是两个 crate、互不依赖，只共享 `nexus-cursor` 那层「定位 / 退出 /
启动」。锚点是压缩后的精确字符串，**版本不等一律拒装，不做模糊匹配**：拒装的代价是用不了，
模糊匹配的代价是装坏。

### 7.4 CRSR 补丁

第二条补丁通道，和 Sand 并列：只把 `applyAuthorization` 里的 Bearer 换成账号 `crsr_` User API Key
兑出来的短期票据，**不改 client-type、不改 URL、不改路由**，面板走的还是原生
`agent.v1.AgentService/Run`。决策记录在 [CRSR.md](./CRSR.md)。

它和 Sand **占同一个挂点，安装器互相拒绝同时装**，备份目录也分开（`crsr/backups` 与
`sand/backups`）——两条补丁的还原点混在一起，用户还原时就无从判断会回到哪个状态。
`nexus-crsr` 复用 `nexus-sand` 的 `layout` / `backup` / `commit` / `integrity`，自己只写注入体
和凭证，这是业务 crate 之间少数几条显式单向依赖之一。

票据续期在**注入块里自己做**，不在 Nexus 进程里：否则用户关掉 Nexus 之后 IDE 会在某个时刻突然
401，而那时他正在写代码，不会想到是另一个没开着的应用的问题。

---

## 8. 数据模型

全部在应用数据目录下的 `nexus.db` 里。业务表只存 `ref`，秘密在 `secrets` 表。

```sql
CREATE TABLE secrets (ref TEXT PRIMARY KEY, value TEXT NOT NULL, updated_at TEXT NOT NULL);

CREATE TABLE accounts (
  id TEXT PRIMARY KEY, email TEXT UNIQUE NOT NULL,
  source TEXT NOT NULL,             -- 账号从哪来
  status TEXT NOT NULL,             -- active | needs_login | dead
  note TEXT, tags TEXT,             -- tags 是 JSON array
  membership TEXT, signup_type TEXT,
  usage_json TEXT, billing_json TEXT,   -- 额度快照 / 订阅实付快照，各一口径（§8.1）
  last_checked_at TEXT, last_error TEXT,
  has_refresh INTEGER, has_password INTEGER, has_api_key INTEGER,
  created_at TEXT, updated_at TEXT
);
-- ref: acct/<id>/refresh, acct/<id>/access, acct/<id>/api_key, …

CREATE TABLE switch_profiles (      -- 切号本；与 accounts 没有外键（R1）
  id TEXT PRIMARY KEY, email TEXT UNIQUE NOT NULL,
  membership TEXT, signup_type TEXT, note TEXT,
  machine_ids_json TEXT NOT NULL,   -- 专属机器码，不是秘密
  created_at TEXT, updated_at TEXT, last_switched_at TEXT
);
-- ref: switch/<id>/auth（整套 cursorAuth/* 的 JSON）

CREATE TABLE auth_backups (id TEXT PRIMARY KEY, email TEXT, created_at TEXT, reason TEXT);
CREATE TABLE machine_original (singleton INTEGER PRIMARY KEY CHECK (singleton=1), ids_json TEXT, saved_at TEXT);
CREATE TABLE activity (id INTEGER PRIMARY KEY, at TEXT, level TEXT, scope TEXT, email TEXT, message TEXT);
CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT);
```

另有各平台账号表、网关账本、游乐场的线程 / 消息 / 图片表。以实际迁移为准——迁移文件才是 schema
的唯一真相，这里只画个轮廓。

机器码（`MachineProfile`）**不是秘密**：它们是随机数，泄露了换一套就行，所以直接进业务表，
不走 `SecretStore`。

CRSR 通道的凭证是个例外，它**不在库里**：补丁自己要读它，而补丁是 Cursor 进程里的一段 JS，
打不开我们的 SQLite。所以它是应用数据目录下一个 `0600` 的 `crsr-agent-credential.json`
（见 [CRSR.md](./CRSR.md) §3.2）。

### 8.1 用量与账单是两本账

`usage_json` 是**额度**（还剩多少次 / 多少 token、什么时候重置），`billing_json` 是**订阅实付**
（标价、券、下次扣多少、历史发票）。两者不是同一口径，叠成一个大数会把「$0 的 Ultra」和
「花了 $21 的按需用量」说成一句糊涂话，所以抽屉里也是上下两张卡。

字段缺席一律是 `null`（未知），**不写成「没有折扣」「不会续费」**——页面改版、接口换字段的时候，
「没读到」和「确实没有」必须能分开，否则界面会拿一个猜测去骗用户。

---

## 9. 安全模型

| 面 | 做法 |
|---|---|
| 凭证静态 | 全在 `secrets` 表，**明文**；Unix 上库文件 `0600`、目录 `0700`，Windows 上靠 `%APPDATA%` 的默认 ACL（§9.1）；内存里的 token 用 `zeroize` |
| 凭证展示 | 默认打码；「显示明文」是显式动作且记活动日志 |
| IPC | 最小命令集；capabilities 白名单；主窗口不加载远程内容 |
| 切号 | 备份先行、白名单键、单事务、原始机器码永不覆盖 |
| 日志 | 邮箱走 `nexus_core::Email::masked()`，token 只打前后几位 |
| 供应链 | CI 里跑 `cargo audit`；锁 Tauri 与插件版本 |
| 更新 | updater minisign 签名校验；清单只走 HTTPS |
| 分发 | macOS Developer ID + 公证；Windows 代码签名（§10） |

网关只监听 `127.0.0.1`。同一台机器上的其他进程能访问它，这是预期行为而不是漏洞——
网关的意义就是给本机的客户端用。

### 9.1 秘密为什么不进钥匙串

最初的决定是「凭证一律进 OS 钥匙串」，代价那一栏只写了「未签名应用会反复弹授权」。
这个代价被低估了两次。

**第一次是数量。** 钥匙串条目绑定创建它的二进制的**代码身份**，ad-hoc 签名的开发构建每重建
一次身份就变一次；而条目数随账号线性增长——一个号三条凭证，二十几个号就是六七十条，
重建一次就是六七十个授权弹窗。于是它们先挪进了本地库。

**第二次是剩下的那一条。** 只留一条会话 token 在钥匙串里，理由是「就一条，值得让操作系统看着」。
但为这一条 token 养着的东西是：`keyring` 依赖（连带 macOS 的 `security-framework`、Linux 的
secret-service + zbus、Windows 原生后端）、启动时的后端探活、`AppState` 上的状态字段、
IPC 里的三个字段、两处界面横幅、一个专用错误码，以及一整套平台文案。

这笔账算不平，而且**分开放并没有换来相应的安全**：这条 token 保护的东西和同一个库文件里那
六七十条凭证是同一个信任域。能读到库文件的人已经拿走了整个号池，把一条 token 挪去钥匙串挡不住
这个攻击者，只是让代码看起来更负责。它的成本却是真实且每天发生的。

于是钥匙串整条去掉，`SqliteSecrets` 成为唯一后端。

**代价说在明处：库文件就是凭证文件。** `Db::open` 把它和所在目录收到 `0600` / `0700`，
但拿得到这个文件的人就拿得到全部凭证。这里没有加密——密钥若跟密文放在一起只是好看，
真要防得住就得进钥匙串，那就绕回了不走钥匙串的原因。**这是一个明确的取舍，不是疏忽。**

**`0600` / `0700` 只在 Unix 上成立。** 收权限的那几处都是 `#[cfg(unix)]`，Windows 上一律不动
文件——那边靠的是 `%APPDATA%` 在用户配置文件下的默认 ACL（同机其他标准用户读不到，管理员和
本用户自己的任何进程读得到）。结论是一样的「同一台机器上信任本用户」，但**不要以为 Windows 上
有一层等价的 chmod**。CRSR 的凭证文件同理。

将来若要真正加密，正确的做法是用户口令派生的主密钥（KDF），而不是把钥匙串接回来。

导出文件（账号导出、整库备份）同样是明文，应用会持续警告。理由相同。

---

## 10. 构建、签名、发布

本地开发与工程检查见 [README「从源码构建」](../README.md#从源码构建)。

发布由 `v*` 标签触发 [`release.yml`](../.github/workflows/release.yml)：两个平台构建 → 有证书就
签名并公证 → 校验产物（macOS 还要验双架构与 stapler）→ 生成 SHA-256 / MD5 与 updater 用的
`latest.json` → 发到 GitHub Releases。

**updater 的签名密钥对是硬要求**，没有它更新器无法验证来源，workflow 会直接失败。
代码签名证书是可选的：两平台证书齐全时发签名正式版（进入自动更新），否则发 Preview
（只能手工下载）。

macOS 上有一条不显然的依赖：**不签名的包拿不到系统「App 管理」权限，Sand 补丁装不上**。
本地构建脚本 `scripts/build-dmg.sh` 因此会用本机钥匙串里的证书签，没有就造一张自签的。
自签只在本机可用，不等于公证。

CI（[`ci.yml`](../.github/workflows/ci.yml)）里有一个单独的 `check-windows` job 跑同样的 clippy
与测试。这不是冗余：平台分支的代码（`tasklist` / `taskkill`、`%APPDATA%` 路径、只读属性）
在 Linux 上一行都编不到，少了它「CI 绿」只代表 macOS 能用。

---

## 11. 已知取舍

写在这里的都是**有意为之**，报 issue 之前请先读一遍：

- **凭证明文存在本地 SQLite。** 见 §9.1。
- **网关监听 `127.0.0.1`，同机进程可访问。** 这是它存在的意义。
- **账号导出文件含明文凭证。** 应用持续警告，用完就删。
- **Sand 与 CRSR 补丁会修改 Cursor 的应用文件。** 幂等、可逆、带版本护栏，但仍是对第三方软件的
  改动，Cursor 升级后要重装。两条占同一挂点，只能装一条。
- **用的是各平台的非公开客户端接口。** 这可能违反对应平台的服务条款，账号有被限流或封禁的
  风险。不要用在你不能承受损失的账号上。
- **只支持 macOS 与 Windows。**
- **锚点精确匹配，版本不等就拒装。** 宁可用不了，不可装坏。
