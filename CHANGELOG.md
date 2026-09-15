# 变更记录

本文件记录每个版本之间用户能感知到的变化。格式参照
[Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循
[语义化版本](https://semver.org/lang/zh-CN/)。

分节用「新增 / 变更 / 修复 / 移除 / 安全」。只影响仓库内部的重构与测试不单列。

## [未发布]

（下一版的条目写在这里。）

## [0.5.1] — 2026-09-15

**建议尽快升级。** 本版修的是一个会**永久弄丢账号**的 bug：只有 session token 的号一旦被切进
Cursor，下一次续期就会掉登录，而这类号没密码、接不了验证码，掉了就再也拿不回来。

### 新增

- **给账号铸一把长期 `crsr_` API Key。** 抽屉的凭证页多了「铸一把 crsr_ Key」：只拿这个号
  手上那把 access 去换，不需要密码、不需要验证码、不需要重新登录，默认 90 天。对只有一把
  session token 的号，这是它唯一的保命动作——铸完之后查用量、进网关、走 CRSR 通道就都不再
  挂在一个会过期的凭证上，所以界面上对这类号会加重成主按钮。铸出来的 key **换不回切号能力**：
  它兑出来的是 `api_key_token`，登不进 Cursor。

### 修复

- **只有 session token 的号不再能加进切号本**，此前加进去会把号弄丢。Cursor 的登录态要成对
  token 并且会自己续期，这类号凑不出这一对，原先的做法是拿 access 复制一份去占 refresh 那一格；
  Cursor 用这个假 refresh 续期必然失败并掉登录，而它们没密码、接不了验证码，掉了就再也找不回来。
  现在切号入口对这类号直接置灰并说明替代路径（铸 `crsr_`、或走网关 / CRSR 通道）。
  **网关不受影响**：它只是借一把会话发请求，不写任何登录态，仅会话的号在有效期内照常参与接力。
- **切号本里此前存下的这类档案也不再能切过去。** 堵住入口救不了已经收录的——它们的指纹是
  `accessToken` 与 `refreshToken` 两格完全相同。现在列表里会标成「无 refresh」并置灰，切号时
  还有一道硬闸在碰 Cursor 任何状态**之前**就拒绝，失败时本机 Cursor 的登录态一个字节都不会动。

## [0.5.0] — 2026-09-15

**首个公开发布版本。** 0.1.0–0.4.0 都是开源之前的内部版本，摘要保留在下面，但**没有任何一个
有对应的公开 Release**——仓库此前只推过代码骨架。本版的功能基线见 0.4.0 那一节，这里只列它之后
的变化。

### 新增

- **CRSR 通道**（可选，进阶）。第二条 Cursor 补丁通道，和 Sand 并列：只把 Agent 面板请求的
  `Authorization` 换成账号 `crsr_` User API Key 兑出来的短期票据，**不改 client-type、不改 URL、
  不改路由**，面板走的还是原生 `agent.v1.AgentService/Run`。票据续期由注入块自己完成，关掉
  Nexus 也不会在写代码写到一半时 401。和 Sand 占同一挂点、互相拒绝同时安装，备份目录分开。
  决策记录见 [`docs/CRSR.md`](./docs/CRSR.md)。
- **账号的订阅账单。** 抽屉里新增账单页：标价、券与持续方式、下次应付、历史发票。
  它和用量是两本账、两张卡，不叠成一个数。ChatGPT 账号同样能看到订阅档位与到期日。
- **ChatGPT 账号页对齐 Cursor**：同一副卡片扫 + 右侧抽屉看；卡上不再放开关和删除。
- **粘贴导入认更多格式**：`crsr_` API Key、`auth.json`、Codex session JSON（数组 / 多行 /
  `credentials` 包一层）、`access----refresh`、单个 refresh token，可以一次贴多个。
- Grok Bot 通道在经网关安装时自动开启，并补上 Grok Bot 的 sand Stream 规则。

### 变更

- **模型目录的主键改成 `{通道}/{模型}`**（`cursor/claude-opus-5`），**默认通道由用户指定**，
  不再写死 Cursor。带前缀强制走那条通道；裸名走默认通道。
  早先「无前缀时按『声明拥有该模型且此刻有号』反推归属」那条规则去掉了——它猜错的时候没法
  解释：同一个模型名在两条通道上都有时，请求落到哪条取决于哪条恰好还有号。
  写进客户端配置文件的仍然是短名（Codex 等客户端会按白名单校验，带前缀直接 400）。
- Sand 规则表跟进 Cursor 版本；ChatGPT 用量多读 `/wham/usage` 的附加桶与点数。

### 修复

- 非 macOS 平台上 `nexus-grokbot` 的未使用导入与常量过不了 `clippy -D warnings`。
- 视频测试里的锁作用域过宽。
- Sand 的 Grok 认证 marker 与 Python 参考实现对齐；中继端到端测试只在 Unix 上跑。

### 迁移

- v11 `accounts.billing_json`（Cursor 个人订阅的 Stripe 账单快照）
- v12 `chatgpt_accounts.user_id` / `organization_id` / `organization_title`
- v13 `chatgpt_accounts.billing_json`
- v14 `accounts.has_api_key`

### 安全

- 补 `.gitleaks.toml`：默认规则抓不到 `crsr_` User API Key（本项目自己处理的那种凭证），
  现在有一条专门的规则；同时逐条放行已知的合成测试夹具，让「扫描红了」重新成为一件值得看的事。

---

## [0.4.0] — 2026-09-11（内部）

功能基线在这一版成型。它**没有公开 Release**——见 0.5.0。

### 新增

- **本地网关。** `127.0.0.1` 上一个端口同时讲四种方言：OpenAI Chat Completions、
  Anthropic Messages（含 `count_tokens`）、OpenAI Responses、OpenAI Images。入站统一解析成
  一份中间表示再桥接到上游，流式 SSE 原样支持。默认端口 `8787`，默认关闭。
- **四个上游平台**：Cursor、ChatGPT 订阅号（Codex 后端）、Grok Build、Kiro。通道是一等实体，
  按前缀与「此刻有没有号」选路；`/v1/models` 按上游能力自动汇总，也支持生图与生视频。
- **额度接力。** 一直用当前号，额度到线才切下一个。限流只冷却「该号 × 该模型」，
  quota / billing 才整号标耗尽。
- **账号池。** 四个平台的账号统一管理：OAuth 走系统浏览器登录、粘 refresh token 批量导入、
  查订阅档与额度与重置时间。凭证存在权限收紧（`0700` / `0600`）的本地 SQLite 里。
- **Cursor 切号。** 直接写 Cursor 自己的登录态库，不逆向、不 patch、升级不失效。默认热切
  （不打断 IDE 里进行中的任务），一号一套专属机器码，切前自动备份、可还原。
- **一键接入。** 直接改 Claude Code、Codex CLI、OpenCode、Grok CLI 的配置文件，只动我们那几个
  键，改前备份、一键还原。
- **游乐场。** 应用内的多轮对话 / 生图 / 生视频工作台，走的是和客户端完全相同的地址与链路。
- **Sand 补丁（可选）。** 把 Cursor IDE 的 Agent 面板改道到本地网关；支持通过 SSH 装在远程
  开发机上，隧道走 ssh 会话里的多路复用中继。锚点精确匹配 Cursor 3.19.13，版本不等一律拒装。
- **请求账本与本地用量。** 每次请求记账号 / 模型 / token / 耗时 / 通道，概览页按时间窗看。
- **透传口。** 另开一个 h2c 口把 `cursor-agent` 的原生 Connect 流量换身份头后原样转发。
- **签名发布链。** `v*` 标签触发双平台构建、签名与公证、校验和与 updater 清单，
  发到 GitHub Releases；配齐证书时应用内自动更新可用。

### 安全

- 秘密一条也不进系统钥匙串，`keyring` 依赖下线。这是一个**明确的取舍**：库文件就是凭证文件，
  靠 `0700` / `0600` 与「一用户一机器」的假设保护。完整理由见
  [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) §9.1。
- 凭证默认打码；「显示明文」是显式动作并记活动日志。日志里邮箱走 `Email::masked()`，
  token 只打前后几位。

### 已知取舍

发 issue 之前请先读 [`docs/ARCHITECTURE.md`](./docs/ARCHITECTURE.md) §11——凭证明文、网关对
同机进程开放、导出文件含明文凭证等都是有意为之，并已在文档里说明理由。

---

以下是开源之前的内部版本，只作为背景保留。

## [0.3.0] — 2026-09-08（内部）

- 账号页按平台分 Cursor / ChatGPT 两组；「IDE 面板拦截」从网关页搬到 Sand 页。
- 远程隧道从 `ssh -R` 换成 ssh 会话里的多路复用中继——`-R` 会被企业网关静默吞掉，
  不报错但工作区里根本没有监听。
- 远程重装认得盘上已装的端点；关掉改道时不再原地装回去。
- 本机签名构建脚本；打包时同时保留 `.app` 与 `.dmg`（Tauri 打完 dmg 会清掉 app）。

## [0.2.1] — 2026-09-07（内部）

- Sand 远程出网多一条「经本机代理」的路；隧道可以挂在 Cursor 自己那条 ssh 连接上
  （ssh config `RemoteForward`），与常驻转发二选一。

## [0.2.0] — 2026-09-06（内部）

- 概览首屏改成状态带；账单按时间窗看、加节奏统计。
- 账号按档位筛选、切号排序、artifact 收进右侧预览区。
- `h2` 0.4.15 → 0.4.19，消掉 RUSTSEC-2026-0258（空 DATA 帧无上限）。

## [0.1.0] — 2026-09-04（内部，Preview）

- 第一个能装的包：切号、账号池、本地网关、游乐场、接入向导、Sand 补丁的首个完整形态。

[未发布]: https://github.com/roviix/nexus/compare/v0.5.0...HEAD
[0.5.0]: https://github.com/roviix/nexus/releases/tag/v0.5.0
