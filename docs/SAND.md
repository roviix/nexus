# Sand 通道 · 设计与决策记录

> **2026-09-16 收缩（§13）**：网关的透传口整条拆掉，随之消失的有——本机「推理经本机网关」（§11 整节）、
> 网关侧的 Grok Bot 开关（§12.4）、远程的「经本机网关」出网路（§9 里三条路只剩代理 / 直连）、
> Grok 鉴权的「关」档。§9 / §11 / §12 里凡是提到透传口、端点改道、`client_type=sand` 的段落都是
> **历史记录**，按当时的真实情况保留，不再对应今天的代码。今天的形态见 §13。
> 界面上 Sand 也不再是独立一页，而是「Cursor 面板」页（原生 / CRSR / Sand）里的一档。
>
> 状态：**规则表已适配 Cursor 3.21.13**（2026-09-19）。本机 `/Applications/Cursor.app`（已升到 3.21.13）上
> `profile_probe` 每类命中等于 `expected()`、`remaining_ide` / `foreign` / `legacy` 全 0，9 个目标文件上
> Rust apply 与 Python 参考实现逐字节一致，live 幂等 / 可逆金标准通过。**这轮不是纯改名**，有三处语义
> 变更（Direct 注入体清空、task tool 改成包官方工厂、move_exec 的 gate 进了 `Promise.all`），见 §1.1。
> **未验的是运行时行为**：task tool 解钉、managed-local / action route 要有 bot 账号跑真会话才能确认。
> 远程 `LayoutProfile::Server` 的期望值还停在 3.19.7 量的，**没在 3.21.13 的 remote bundle 上重量过**
> （数字不对的后果是拒装而不是装坏，见 §9.2）。
>
> 2026-09-06：适配 Cursor 3.19.13；纯压缩符号改名，10 个目标文件上 Rust 与 Python 逐字节一致。
>
> 2026-09-04：适配 Cursor 3.19.7；真机 11 个目标文件上 Rust 与 Python 逐字节一致。
>
> 2026-09-03：**远程主机（remote SSH）落地**，同一份规则表打远程 `~/.cursor-server`，推理默认改道回本机网关，
> 反向隧道受监管。真远程 bundle 镜像上端到端通过；真机安装待远程链路恢复后点（§9）。
>
> 这份文档是对 `ARCHITECTURE.md §1.2`「不改 Cursor 的字节」的**有意反转**。改了要改这里，
> 也要回去改架构文档那一处。
>
> **另有一条并列的补丁通道 [CRSR](./CRSR.md)**：它只换 `applyAuthorization` 的 Bearer，不改
> client-type、不改路由。两条占同一个挂点，安装器互相拒绝同时安装，备份目录也分开。
>
> **关于文中的 `gateway/…`、`docs/relay/…` 路径**：它们指向本项目的上游私有仓库（逆向研究笔记与
> 那份 Python 参考安装器），**不在本仓库内**。引它们是为了让每条协议断言都有出处可考，结论本身
> 都已写在正文里，不打开原文也能读懂。详见 [CONTRIBUTING](../CONTRIBUTING.md#代码约定)。

---

## 1. 决策：推翻「不做 sand 补丁」

### 1.1 当初为什么否决

ARCHITECTURE §1.2 原话：「不做 sand 补丁 / 改 Cursor 二进制：Cursor 一升级即可能全部失配。这是军备竞赛，
不适合作为要长期维护的产品主干。」

这个担忧**今天依然成立**，而且刚被验证了一次：3.18.9 → 3.18.25，16 类补丁里有 7 处压缩后的符号名变了
（`En→In`、`Cre→xre`、`toe→soe`、`hre→gre`、`Joe→tre`、`nre→are`、`Mn→Bn`），一个 chunk 改了编号
（`657.js→61.js`），Direct Stream 注入体多了十来个模型标志。

3.19.7 → 3.19.13（2026-09-06）又验证了一次，但这次**代价小得多**：全部是压缩符号改名，没有一处语义变更。
改名表：路由 `v→A` / `h→f`（外层 `i→o`、`o→s`、`s→i`）、local loop gate 常量 `qs→Ms`、move_exec
`f=…(Ls)→h=…(Js)`、action route `i.xy→o.xy` / `T→y`、resume `xn.FL→Gn.FL` / `we.xy→Ee.xy`、
bubble `hr→Ar` / `_t.w3→wt.w3`、attempt 工厂 `ve→me`、task tool 工厂 `Ae→Ne`、
端点改道 `J.getHttp2PingConfig→R.`、`w.OriginService→E.`、`T.RemoteAgentHostPresenceService→y.`。
**打包方式变了**：3.19.7 的 `9909.js`（1.1 MB）整块内联进 agent-host 的 `main.js`（1.8 MB → 11.7 MB），
`4883.js` 改号 `4884.js`，`TARGET_SPECS` 从 11 个目标减到 10 个。Direct 注入体一个字符都没改——它依赖的
`J` / `o.sXH` / `o.got` / `o.Ycw` / `oe` 两版字节相同。整轮实际改动：两个版本常量 + 一行 `TARGET_SPECS`
+ 12 处锚点字符串，引擎 / 完整性 / 备份 / 回滚 / 编排 / 界面照旧零改动。

⚠️ 「零语义变更」只对**我们这 10 个文件**成立。同日对全 bundle 做了四层 diff
（`gateway/scripts/diff-cursor-bundles.py`，结论在 LEDGER 7.18–7.24），新东西不少，其中三条跟我们有关：
①  agent-host 新增 featureFlags **白名单**（`oq` 131 个 bool + `sq` 7 个数值，`iq()` 只认名单内的键）。
我们注入的 `enableBrowserSubagent` 在名单内，暂时无碍；但这是「featureFlags 从字面量走向受控构造」的
信号——哪天我们打补丁的 `,useClientSideSubagent:!0};` 字面量被 `iq()` 取代，那条规则会同时失锚且失效。
②  子代理面正在我们改的同一批文件里扩张：`UpdateCurrentStep` / `sendFinalSummary` 进度上报工具、
`InterruptAgentHostTurnRequest.session_tree` 停止级联、六种新子代理类型。
③  换了 undici（`EnvHttpProxyAgent` / `EventSource` / `Dispatcher#compose`），我们自建 transport 贴着这层跑，
下一版若继续动它，`INFERENCE_TRANSPORT_ANCHOR` 一带是首要复核点。
反过来，`sand_*` gate 集合（101 条）与签名 / 认证 / 设备指纹面**两版完全一致**，桌面端仍无任何 IAP 实现。

3.19.13 → 3.21.13（2026-09-19）是**上面那个「代价小得多」的反例**：跨了两个小版本，除了全 bundle 改名，
还有三处真语义变更，是继 3.18.25 之后第一次需要重写注入体而不只是换符号。

1. **Direct 注入体清空。** 3.21.13 的 `RunInference` 直接收服务端下发的 `promptModelMetadata`，
   旧注入体手工补的 `promptModelInfo` / `agentTokenLimit` 已被它取代；且旧写法依赖的三个局部量与
   `J` / `oe` 等压缩符号在新转译形态里全没了（§10.5 那套逐条复核随之作废）。锚点改成新的异步生成器形态
   `function kRe(e){return t=>CRe(this,void 0,void 0,function*(){`，注入体降为**空块**
   `{/*SAND_DIRECT_INFERENCE_STREAM_V1*/}`，让 Cursor 自己的 `RunInference` 原样跑。
   写成空块而不是裸 marker 是有讲究的：历史注入体都是「`{` + marker + 逻辑」，裸 marker 会成为它们的
   子串，legacy 识别 / 迁移 / 卸载会互相咬。
   顺带说明「改 `modelId` 去蹭高级模型额度」那条路**本来就不成立**（早前实测拿不到），不是这轮丢的。
2. **task tool 改成包官方工厂。** 工厂 `Ne→zRe`，且 patched 体不再自己拼整个 props——改成调 `zRe()`
   拿到官方对象后只覆盖钉模型的那几项（`isModelBlocked` / `isModelValid` / `forceModelId` /
   `subagentModelForcePolicy` / `getTaskToolConfig` / `subagentModels`）。包一层比重写整体抗改名。
3. **move_exec 的 gate 进了 `await Promise.all([...])` 数组**，成了解构出来的第一个元素。整段
   `Promise.resolve(a.cursor.checkFeatureGate(bYe)).catch(()=>!1)` 换成 `Promise.resolve(!0)` 把它钉成真。

另有两处结构性的：agent-host 的 `4884.js` 整块并进 `main.js`，`TARGET_SPECS` 10 → 9（**「chunk 会整块消失」
这条第二次应验**）；client-type 的 `set_header` 多了一处三元回退，且回退值是 `"cli"` 而非 `"ide"`，
为可逆性新增了 `SAND_CLIENT_CLI_MARKER`（这也是预检从 22/23 差一条的原因）。

**`subagent route` 这条规则本版起不再新装。** 上游自己把 `subagentTypeName` / `parentAgentToolCallId`
从 unsupported run options 里摘了出去——gate 改名 `hasUnsupportedRunOptions` → `Ns()` /
`unsupportedRunOptionReason`，那两项转而用于算 `isHostedSubagentChild`——正是这条规则以前干的事。
所以它在 `profile_probe` 里命中 0 是**正确状态**（`expected()` 为 `None`、不进硬校验），
留着只为卸载 3.19.7 之前装的旧补丁。

### 1.2 为什么现在做

三个变化，缺一个都不该做：

1. **需求真实且独占**。切号 / 账号 / 商城，竞品（cc switch、Sirocco）都有类似物；sand 补丁没人做得出来
   ——它依赖我们在 `gateway` 里积累的完整协议逆向。作为引流点，它的吸引力远大于切号。
2. **不再是「主干」，而是「一个隔离的专区」**。补丁与切号是两个 crate、互不依赖、两套备份、两套状态。
   补丁失配只影响 Sand 页；切号那条「装完一直能用」的主路径不受牵连。这是编译期约束（§3）。
3. **有一条把「军备竞赛」变成「运营节拍」的路**：锚点做成**数据表**（`rules.rs`），引擎不认识任何具体锚点。
   这次 3.18.25 升级在 Rust 侧只动了三处：`SUPPORTED_CURSOR_VERSION`、`TARGET_SPECS` 一行、一个预检锚点；
   引擎、完整性、备份、回滚、编排、界面**零改动**，全部测试原样通过。这就是设计要证明的事。

### 1.3 明确不做的（本轮）

- **不做云端下发补丁**。曾经考虑过「锚点 JSON 放云端，客户端拉了就改」——响应从「发版几小时」缩到
  「几分钟」，但代价是**亲手造一条云端到用户机器的代码注入通道**（注入体是可执行 JS，如几千字符的
  Direct Stream 片段）。这是 desktop 现在完全没有的量级的攻击面，签名与信任域设计是生死线。而分发快十倍
  并不改变总周期——瓶颈在**人肉重新逆向新版锚点**（本次约半天），不在分发。
  结论：走 App 自动更新（M4 的 updater）。补丁内容和其它 Rust 代码一样受同一套签名保护，**零新增攻击面**。
  `rules.rs` 保持数据化是为了将来若真需要热更时不必重构，不是为了现在上云。
- ~~**不做 remote SSH 的补丁**。`gateway/scripts/sand-remote-server.py` 另有一套，不在 desktop 范围。~~
  **2026-09-03 反转，见 §9。** 反转的直接原因恰恰是「另有一套」：那套只有 5 类规则，漏掉的正是
  `extensionHostProcess.js` 上的 client-type，远程于是一直以 `ide` 身份发请求——第二套规则本身就是 bug。
- **不签名 / 不公证**（用户决定：面向开发者，拿到 DMG 就能用）。后果如实写在 §7。

---

## 2. 它做什么

给本机已安装的 Cursor（`/Applications/Cursor.app` 里的 JS bundle）打一组字符串级补丁，让 IDE 的 Agent 面板
把推理从「云端 api5 编排」改道到「本机 managed-local 本地 loop + api2 `InferenceService`」，从而走 sand/bot 额度通道。
原理与全部补丁清单见 `docs/relay/CURSOR-FULL-ARCHITECTURE.md`；Python 参考实现是
`gateway/scripts/sand-stream-installer.py`（v1.3.0-cursor-3.21.13 ↔ Cursor 3.21.13）。

16 类补丁、9 个目标文件。用户可选三项：上下文自动摘要 / 模式放行档位（Agent · Agent+Plan · 全部）/ 完成后重启。

**模式放行默认「全部」**（2026-09-04 起，与上游安装器不同）。理由在补丁自身：`managed_local_route_patched`
把「本地循环处理不了就退回云端」那条路封了（`if(!1)return{runtime:"connect"}`），所以档位拦下来的 turn
不是绕道云端、而是 `{runtime:"fail"}` **硬失败**。收窄在原版里只是"这类 turn 走云端"，在补丁下等于
"这类 turn 直接报错"——`cursor-guide` 子代理就是这么挂的（`Local loop cannot run this turn:
mode-not-supported`）。已知边界照旧：managed-local 主循环的 query 处理器只执行
web_search / web_fetch / generate_image，`create_plan` 这类交互查询不被执行。

**`subagent session` 这条规则在 3.19.7 换了用途。** 3.19.7 的 managed-local featureFlags 对象自带
`useClientSideSubagent` / `enableExploreSubagent` / `enableDebugSubagent`（原来那条 `const xre={…}` 锚点
随之消失，规则只剩卸旧装的用途、不进硬校验），但**没开 `enableBrowserSubagent`**。子代理目录是按 flag
拼的（`includeBrowserUseSubagent: t?.enableBrowserSubagent ?? false`），不开就是
`Local loop cannot resolve subagent type "browser-use"`。所以同一个 `RuleId` 下挂了第二条规则，锚点是
3.19.7 那个对象的收尾 `,useClientSideSubagent:!0};`，只追加这一个 flag。浏览器机能本身在本地循环里是齐的
（它加载的 chunk 清单含 9341 / 2337 / 5371，`browser_take_screenshot` / `browser_tools` /
`browser_use_enabled` 都在那几块）。
推理引擎只有 Direct（直连 `InferenceService/Stream`，§10）；曾短暂提供的 Session（走官方 `RunInference`）
在 2026-09-04 被服务端对 sand 身份封掉后下线。

**上下文自动摘要默认开**（2026-09-03 起，与安装器 v1.2.6.7 一致；此前默认关）。理由见
`docs/relay/CURSOR-FULL-ARCHITECTURE.md` §I.5b：Stream 模式下 managed-local 没配
`backgroundSummarizationProps` 阈值，后台外部摘要永远不会主动触发，自摘要是**唯一**能在撞上限前压缩
历史的机制；关掉后长会话到上限只会一直 `InputTokenLimitError`，撞墙后的阻塞摘要把同一份超限对话再发
一遍也必然失败。开着的下行风险小：sand 下摘要请求若失败，Background 模式丢弃结果、会话照常继续。
`SandStatus.selfSummary` 报盘上实际的开关（`rules::installed_self_summary`），与界面开关不同时安装
会**原地切换**——`AnchoredInsert` 的 apply 见到 `legacy_injections` 里的变体就换成当前注入体（计
`migrated`），不必先卸载；此前 marker 已在就跳过，「改了选项点重新安装」是静默 no-op。

---

## 3. 架构

```
crates/nexus-sand
  service    编排：预检 → 备份 → 退出 Cursor → 写 → 校验 → 启动；单飞闸（Busy）
    ├─ rules      知识：锚点 / 注入体 / marker / 期望命中数   ← Cursor 升级只改这里（+ layout 的 chunk 名）
    ├─ engine     引擎：apply / remove / inspect（纯函数，不认识具体锚点）
    ├─ integrity  Cursor 自己的完整性：扩展内嵌 sha256、product.json checksums
    ├─ commit     原子写 + 写后校验 + 失败回滚
    ├─ backup     改动前字节快照 + manifest
    └─ layout     目标文件在哪（复用 nexus_cursor::CursorPaths.app）
```

**依赖**：`nexus-core`、`nexus-store`（activity 日志）、`nexus-cursor`（定位 / 退出 / 启动）。
**零依赖** `nexus-switcher` / `nexus-accounts`。理由与 ARCHITECTURE R1 同构：切号写 Cursor 的**数据**（登录态），
补丁改 Cursor 的**代码**（bundle）——性质、风险、失效方式都不同，不该互相拖累。两者只共享 `nexus-cursor`
那层「Cursor 在哪、怎么安全退出重启」。

**为什么不扩 `nexus-cursor`**：它的契约是「读写 Cursor 自己的数据，升级无关」。补丁塞进去会污染这份
「干净、长期主干」的定位。

### 3.1 IPC

命令（全部 `Result<T, AppError>`，ARCHITECTURE §3.3）：

| 命令 | 说明 |
|---|---|
| `sand_release` | 不读本机安装：返回 Sand 适配版本及当前平台的 Cursor 官方不可变下载直链 |
| `sand_status` | 只读：版本 / 已装 marker / dry-run 预演 / 备份数。`async`（读十个 bundle） |
| `sand_install(options?)` | 装。阻塞线程 + `sand://progress` 事件 |
| `sand_uninstall(relaunch?)` | 卸载，恢复原版 |
| `sand_backups` / `sand_remove_backup(id)` | 备份列表 / 删 |
| `sand_restore_backup(id, relaunch?)` | **紧急刹车**：按字节写回，不认锚点 |

新增 `ErrorCode`（只增不改）：`SandUnsupportedVersion`（等待适配）、`SandAnchorMismatch`（锚点不齐，未改动）、
`SandForeignMarkers`（别人的补丁在，拒接管）、`SandIntegrity`（写后校验失败，已回滚）、
`SandRollbackIncomplete`（最坏情况，message 带备份目录）。

### 3.2 数据

- **不进 `SecretStore`**：这里没有秘密。
- **不进 SQLite**：单个 bundle 几 MB。备份落 `<app_data>/sand/backups/<install-hash>/<stamp>-<op>/`
  （`manifest.json` + `files/<相对路径>`），`install-hash` 是 app 根路径 sha256 前 16 位，两套 Cursor 不串。
- 活动日志走 `nexus_store::activity`（scope `sand`）。

---

## 4. 关键机制

### 4.1 顺序（硬约束，与切号同构）

```
预检：版本精确匹配 · 预检锚点 · 无外部 marker · dry-run 锚点齐全     ← 不过就什么都不碰
  → 备份改动前字节 → 退出 Cursor 并等进程消失 → 逐文件原子写
  → 重新 inspect + 扩展 hash + product checksums 全对 → 启动 Cursor
任一步失败：commit_plan 按原字节回滚；回滚也失败 → SandRollbackIncomplete + 备份目录路径
```

### 4.2 版本护栏

`SUPPORTED_CURSOR_VERSION` 精确匹配，**不做模糊匹配**——锚点错配写坏 bundle 比不装糟糕得多。
不匹配时界面不给补丁安装按钮，只给两条真实出路：安装页顶提供的已适配 Cursor，或等待 Nexus 跟进当前版本。
这是引流场景下最重要的一条：新用户第一次用就装坏 Cursor，整个产品的信任就没了，连带切号。

下载入口也必须跟补丁版本硬绑。`model::supported_cursor_release` 用
`SUPPORTED_CURSOR_VERSION + SUPPORTED_CURSOR_RELEASE_ID` 生成 `downloads.cursor.com/production/...` 官方不可变直链，
`sand_release` 只按 Rust 编译目标返回当前系统的包；即使 Cursor 未安装、`sand_status` 失败，入口仍可用。**禁止**
直接链接 `releaseTrack=stable`：它会在 Cursor 发版后漂到补丁尚未适配的新版本。每次升级适配时必须同时从官方
下载 API 取得新 release ID，并对 macOS Universal、Windows x64/ARM64 user setup、Linux x64/ARM64 AppImage
逐一做 HEAD 200 校验。

### 4.3 幂等与可逆（`engine` 的不变量，测试钉住）

`remove(apply(x)) == x`；`apply(apply(x)) == apply(x)`。旧版本 / 其它档位的变体（V1–V6 task tool、
无 explore 的 subagent session、十二种 Direct Stream 注入体 + 已下线 Session 引擎的空 marker、三档 action route）
安装时原地迁移、卸载时全部认。

### 4.4 与 Python 版的两处刻意差异

- **product.json 做文本级值替换**而不是重新序列化：保住原文件键序 / 缩进 / BOM，diff 只剩被改的值。
- **正则的负向前瞻改成显式 `skip_if_followed_by` 字段**：Rust `regex` 不支持前瞻；引擎在每个匹配的
  结束位置试 guard。这也是 inspect 数「还剩几处没打」的依据。

---

## 5. 界面

「Cursor 面板」页（`pages/CursorPanelPage.tsx`）的 Sand 那一档（`pages/SandPage.tsx`，`embedded`）。一屏四件事：**哪里拿到匹配版本**（页顶 Cursor 官方直链）、
**现在是什么状态**（版本 / 已装 / 完整）、**装了会发生什么**（dry-run：改几个文件、16 行 marker 各自
`已装 +待打 / 需`）、**出问题能回到哪**（备份）。
措辞按 `outcome.wrote` 分两种：写盘前失败说「Cursor 没有被改动」，写盘后不许这么说——
错误文案不能承诺我们并不知道的事。

---

## 6. 规则表：与 Python 的对照与验收

`rules::catalog(&InstallOptions) -> Result<Vec<PatchRule>>` 返回 28+ 条规则（19 类；client-type 有 4 条正则、
eligibility 6 条字面量共用各自的 `RuleId`），顺序与 Python `apply_patch_to_content` 一致。
**Cursor 升级时改这里**，逐条对照新版 Python 文件（压缩符号名每版都变，只有 marker 与期望计数稳定）：

| # | RuleId | Python 常量 / 函数 | RuleKind |
|---|---|---|---|
| 1 | ClientType | `CLIENT_RULES`（三条正则）+ `legacy_client_re`（KC 迁移） | Regex ×4，guard = `CLIENT_MARKER_GUARD_PATTERN` |
| 2 | Eligibility | `ELIGIBILITY_PREFIXES` ×6 | Literal ×6 |
| 3 | ManagedLocalRoute | `MANAGED_LOCAL_ROUTE_*` | Literal |
| 4 | LocalRuntimeLoad | `LOCAL_RUNTIME_LOAD_*` | Literal |
| 5 | AgentHostMoveExec | `AGENT_HOST_MOVE_EXEC_*` | Literal |
| 6 | ManagedSubagentRoute | `MANAGED_SUBAGENT_ROUTE_*` | Literal |
| 7 | ManagedActionRoute | `_managed_action_route_patched(level)` 三档 | Literal，legacy = 另两档 |
| 8 | SubagentResumeMode | `SUBAGENT_RESUME_MODE_*` | Literal |
| 9 | SubagentInteractionBubble | `SUBAGENT_INTERACTION_BUBBLE_*` | Literal |
| 10 | SubagentModelVariants | `SUBAGENT_MODEL_VARIANTS_RE` / `_PATCH_RE` | Regex（desktop + glass 总 2） |
| 11 | ContextWindow | `MAX_TOKENS_*` | Literal |
| 12 | SubagentCompletionWake | `SUBAGENT_COMPLETION_WAKE_RE` / `_PATCH_RE` | Regex（总 2） |
| 13 | ManagedSubagentSession | `MANAGED_SUBAGENT_SESSION_*` + `_PATCHED_V1` | Literal，legacy ×1 |
| 14 | ManagedTaskTool | `_managed_task_tool_patched()`（V6）+ v5/v4/v3/v2/v125/v124 + `SUBAGENT_MODEL_CATALOG_JS`（V6，两把父模型键）/ `_V5` | Literal，legacy ×6 |
| 15 | AgentHostIdentity | `AGENT_HOST_IDENTITY_*` | Literal |
| 16 | InferenceStream | `DIRECT_STREAM_ANCHOR` + `_direct_stream_injection(self_summary, ctx)` | AnchoredInsert（marker 需 1）；`legacy_injections` = 其余 11 种 Direct 变体 + 已下线 Session 引擎的空 marker `LEGACY_SESSION_STREAM_MARKER`，install 见到即原地换成当前形态，inspect 把 Session marker 计入 `legacy`（§10） |
| 17 | AgentHostEnablement | `AGENT_HOST_ENABLEMENT_RE` / `_PATCH_RE` | Regex，`per_file_limit = 1` |
| 18 | InferenceEndpoint | `INFERENCE_TRANSPORT_*` + `INFERENCE_ROUTE_*`（可选，远程 / 本机网关） | Literal ×2，legacy = 盘上旧 URL |
| 19 | GrokBotStreamAuth | `applyAuthorization` 两处（`GROK_RUNTIME_AUTH_VAR_DECLS`）+ `_grok_runtime_auth_patched(vars, mode)` | Literal（可选，默认 `box_relay`）；三形态互为 legacy 原地切；凭证由 `nexus-grokbot` 准备，见 §12 |

**顺序有意义**（与 Python `apply_patch_to_content` 一致）：legacy 迁移先于同类新装。

**子代理模型目录为什么是动态的（V5）。** V4 曾往 `modelsBySlug` 塞一张硬编码 slug 表，Cursor 原生
userinfo 把它原样念给模型（"you may ONLY use model slugs from this list"），而 `isModelValid` 又是
`()=>!0` 一路放行——实测 14 个里 5 个是**客户端别名或不存在的 id**（`opus-5`、`fable-5`、`sonnet-4.5`、
`gpt-5.6`、`claude-4.5-opus`），到服务端才报 "AI Model Not Found"。逆向发现注入点作用域里
`e.runOptions.selectedSubagentModels` 就是 workbench 每轮下发的「用户勾选且支持 agent 的模型」
（Cursor 自己的 connect 路径在服务端用的正是它），V5 起目录直接取它 + 父模型，**没有任何硬编码**，
永远与用户的模型选择器一致。

**父模型名为什么从 `i` 换成 `e.requestedModel.modelId`（V6，2026-09-03）。** `i = e.resolvedModel?.modelId ??
e.modelId`。Direct 引擎下 `resolvedModel: n` 就是客户端请求，`i` 与请求 id 相同；Session 引擎下 `resolvedModel`
是服务端 `runReady` 解析过的，可能是 `claude-opus-5-thinking-high` 这类**变体 slug**——拿它当 `parentRequestedModelName`
再配上 `parentModelParameters`（thinking / effort 等参数）等于把变体编码两遍。V6 父模型名取客户端 id，目录末尾同时
追加客户端 id 与 `i` 两把键（Direct 下重复键无害），`Task` 的 `model` 参数写哪个都解析得到。V1–V5 装过的机器
install 时原地迁移到 V6（真机金标准已按「迁移等价」在盘上 V5 的真实字节上验证）。

**变体（fast / max / thinking / effort）怎么进目录（SubagentModelVariants）。** 新版 Cursor 里这些不是
slug 而是 `RequestedModel.maxMode` + `parameters`（`{id:"fast",value:"true"}`、`effort=max`…）；Task 的
`model` 参数只解析成一个字符串，客户端起子代理时 `maxMode` 直接继承父会话、`parameters` 去查该
modelId 的**已保存**选择器配置——所以"opus 5 fast max"根本没法通过 `model` 参数表达。agent host 拿不到
变体目录（`AiService.availableModels` 只在 workbench 侧、且是异步的），但 workbench 组
`selectedSubagentModels` 的 `rRf()` 手里就有每个模型的 `legacySlugs`（服务端 `AvailableModel.legacy_slugs`，
如 `claude-opus-5-thinking-max-fast` = thinking + effort=max + fast）。于是补丁改 `rRf()`：把 legacySlugs
也各造一条 RequestedModel 一并下发，V5 目录按 modelId 建 key 便自然含全部变体；解析出的 legacy slug 原样进
`InferenceRequestedModel.model_id`，服务端自己解释——Cursor 子代理默认模型 `composer-2.5-fast` 就是这样送
的，本仓库 gateway 也一直这样送 `claude-opus-5-thinking-max-fast`。desktop 与 glass 两个 workbench bundle
标识符不同（`l/u/h/s/o/vA/p/AP` vs `c/u/d/s/o/ZR/h/E3`），故为正则、期望 2。
已知边界：目录条目随勾选模型的变体数增长（本机 12 模型 +87 行系统提示）；解析器对目录外的 `*-fast`
名字仍按 Cursor 原生逻辑落到 `composer-2.5-fast`；`rRf()` 前置门 `explicit_subagent_models`（Statsig）
关着且父会话非 max 时整个列表不下发——这是原生行为，本补丁不改。

**Rust `regex` 与 Python `re` 的两处硬差异**（移植时必须改写，不能照抄）：
- 无反向引用 `\1`：引号用显式交替 `("sand"|'sand')`；变量名相等的约束移到 `repl` 闭包里判，不等则原样返回。
- 无前瞻 `(?!…)`：变成 `RuleKind::Regex.skip_if_followed_by`（逐位置 guard，client-type 用）。另有
  `skip_file_if_marked`（整文件幂等，enablement / completion-wake 用）对应 Python 的 `if MARKER not in content`。

**验收**（每次改规则表都跑；`cargo test -p nexus-sand --test live_cursor -- --ignored --nocapture`）：
- 对适配版本的 10 个文件：`baseline = remove(disk)` 全部 marker 为 0 → `apply(baseline)` 的 `inspect` 17 项计数
  等于 `RuleId::expected()`、`remaining_ide == 0` → **金标准 `apply(baseline) == disk` 逐字节**（Rust 复现 Python
  装在盘上的字节）→ `remove(apply(baseline)) == baseline` 逐字节。2026-09-02 对 3.18.25 全部通过。
- 规则表刚改、盘上还是旧版时金标准必然不等；改跑 `apply_matches_python_reference_in_memory`：让 Python
  在内存里对同一基线 `apply(remove(disk))`，与 Rust 逐字节比（2026-09-02 加 SubagentModelVariants 时 11 文件全等）。
- **升级适配时本机通常还停在旧版**（补丁装着、故意不让它自动更新，好留住 diff 基线），新版只有一份从官方
  dmg 抽出来的副本。`SAND_LIVE_APP=<摆成 .app 形状的目录>` 让上面两条对着那一份跑；盘上是原版时金标准
  自动跳过（没安装就无从「复现」），此时以 in-memory 对拍为准。2026-09-06 对 3.19.13 全部通过。
  这一轮它抓到一个真 bug：Python 缺 browser 子代理 flag 那条规则（Rust 有），因为 3.19.13 的 featureFlags
  对象插了 `longRunningJobs` / `outputNotificationLimit`，Python 那条要求 `nalLoopDetection:!0,useClientSideSubagent:!0`
  连续的旧锚点不再匹配、又没有 Rust 那条独立的 `,useClientSideSubagent:!0};` 兜底。已补齐。
- `tests/python_parity.rs`：版本 / 19 个 marker / 期望计数 / `TARGET_SPECS` 顺序 与 Python 双向一致。
  **Python 与 Rust 任一方单独改版本都会挂。**

  注意这道门在**公开仓库里是关着的**：那两份 Python 脚本在上游私有仓库里，没随开源发布，
  测试因此整组跳过（跳过时会打一行说明，不会静悄悄地绿）。有上游仓库时用
  `SAND_PYTHON_REF=…/gateway/scripts` 指过去就会真的跑。跳过的代价是「先改 Python 验证、
  再同步 Rust」这条约定在公开仓库里只剩人工遵守 —— 真值仍然是 `rules.rs` 自己，
  不依赖 Python 的那条 `rust_marker_table_and_rule_markers_cover_each_other` 任何时候都跑。
- 之后才允许在真机 `install`（会退出 Cursor）。

---

## 7. 已知代价（如实）

| 代价 | 后果 | 对策 |
|---|---|---|
| 版本硬绑 | Cursor 升级即失配，需要重逆向 + 发版 | 改 `rules.rs` / `TARGET_SPECS` / 版本常量与下载 release ID；界面可下载匹配版或等待适配。3.18.25 那轮约半天；3.19.13 那轮（纯改名）约一小时 |
| 未签名 / 未公证 | Gatekeeper 拦 DMG；改 `/Applications` 可能要提权 | 用户决定接受（面向开发者）；文案写清右键打开 |
| 改的是官方二进制 | 账号级风险自负；Cursor 自更新会把补丁覆盖掉（需重装） | 界面明示；`status` 每次进页重读 |
| 灰度粗 | App 更新无法只放给 5% 的人 | 本地硬校验（锚点不齐 = 装不上而不是装坏）比灰度更实在 |

---

## 8. 与 Python 脚本的关系

`gateway/scripts/sand-stream-installer.py` **继续保留**，定位变成：① 运营在新版 Cursor 上先行验证锚点的
探针（它的 `status` dry-run 就是干这个的）；② `rules.rs` 的移植规格来源。两边的 marker 字符串、期望计数、
补丁语义必须一致。

顺序上原本的规矩是「先改 Python 验证通过，再同步 Rust」（3.19.7 那轮就是这么走的）。3.19.13 这轮**反过来
先改的 Rust**，因为 `examples/profile_probe` 能直接对一份从 dmg 抽出来的 bundle 报每类命中数，比 Python 的
`status` dry-run 更快定位哪些锚点飘了。这么走的前提是**收尾必须补上 in-memory 对拍**
（`apply_matches_python_reference_in_memory`）——正是它抓到 Python 少了 browser flag 那条规则（§6 验收）。
换句话说：谁先改都行，两边逐字节对齐这一步不能省。

`gateway/scripts/sand-remote-server.py` 也保留，但只剩一个角色：它的两个端点改道 marker
（`SAND_INFERENCE_ENDPOINT_V1` / `SAND_REMOTE_INFERENCE_ROUTE_V1`）是 Rust 侧同名常量的对账来源
（`tests/python_parity.rs::remote_only_markers_match_the_remote_python_script`）。**它不再是远程补丁的
规则来源**——远程走 §9 的统一规则表。那个脚本装过的远程机器，desktop 能认出是自己人并正常接管 / 卸载。

---

## 9. 远程主机（remote SSH）—— 对 §1.3 的第二次反转（2026-09-03）

### 9.1 为什么反转

remote SSH 下 Agent 的编排与推理跑在**远程** `~/.cursor-server` 上，本机 Cursor.app 只是 UI，本机补丁够不着。
九月初那轮用 `sand-remote-server.py` + `ssh -R` + hosts/iptables 劫持 api2 试过，**从头到尾没有出过字**，
文档还把卡点错定性成「sand 账号对外网 provider 没额度」。今天复盘出来的真相是两个叠着的坑：

1. **身份**：`x-cursor-client-type` 出在 `extensionHostProcess.js`（`_??"ide"`），而那个脚本只有 5 类规则、
   从不碰这个文件。远程每一发推理都是 `ide` 身份、记在账号自己的额度上，撞上一个没有普通额度的号就是
   `resource_exhausted`。第二套规则集本身就是这个 bug——所以反转的方式不是「把 Python 集成进来」，而是
   **让远程走同一份 `rules::catalog`**。
2. **网络**：远程默认出不去网，或出口地区拿不到 claude / gpt（实测某公司代理走东京出口：grok 正常、
   claude 与 gpt 被 api2 以 region 拒绝——这正是当时「grok 能用、claude/gpt 不能」的原因，与额度无关）。

Cursor 侧没有干净路子：agent host 从 `cursor-agent-exec` 拿的是进程内的活对象句柄、按相对路径 require
兄弟扩展的原生 `.node` 模块、文件操作直接走 node `fs`，它只能和 exec 待在同一台机器上；Cursor 自己的反向
通道（agentBidi / bcProxy）是 service 专线，两条都实测不通。**推理必然从远程出网**——于是要不要隧道由
远程的网络性质决定，与架构无关。产品默认按「远程出不去网」处理。

### 9.2 怎么做到几乎零改造

远程 server 根（`~/.cursor-server/bin/<arch>/<commit>/`）本来就是一个合法的 `SandLayout`：有 `product.json`
（无 `checksums`）、`out/`、`extensions/`，`TARGET_SPECS` 里 Electron/UI 侧的几个文件缺席，其余命中。
3.19.7 上是 11 中 7；3.19.13 目标减到 10，**还没在 3.19.13 的 remote bundle 上重量过**。
现有代码对这些差异**本来就是正确降级的**（跳过不存在的目标、无 checksums 则 no-op、扩展 hash 只同步有内嵌
条目的）。所以：

- **暂存镜像**，不抽远程文件系统。把远程那 7 个文件拉到本地临时目录、按 `<staging>/resources/app/<rel>` 摆好，
  `SandLayout::from_root` 探测 app 根的第二个候选形状正好接得住——engine / integrity / commit / backup 全部
  原样复用，包括本地这一侧完整的「备份 → 原子写 → 写后校验 → 失败回滚」。改完把变动的文件推回去。
- **`LayoutProfile::{Desktop, Server}`**：唯一需要动脑的改造。期望命中数是硬校验的依据，远程少 4 个文件数目必然
  不同，不能「按实际算」（那会让版本护栏退化成恒真），所以是两套写死的 profile。Server 的数字是拿原版远程
  bundle 用 `examples/profile_probe` 量出来的：client-type **2**、agent host enable / completion wake /
  subagent model variants **0**（锚点全在远程不存在的 workbench 文件里），其余与 desktop 相同。测试钉住。
- **推理端点改道**是第 18 类规则（`RuleId::InferenceEndpoint`，可选项，两处：在 transport 装配处
  （3.19.7 是 `9909.js`，3.19.13 起并进 agent-host `main.js`）建一条指向
  `http://127.0.0.1:<port>` 的 transport + 挂进 `InferenceService` 的路由表；两处必须同进同出，查表是 `e in map`，
  只挂路由不建 transport 会命中 `undefined` 而非回退）。用 `Literal` 而不是 `AnchoredInsert`，把锚点后面紧邻的
  片段一并纳入 original 使其成为非前缀，apply 天然幂等。端点用 `127.0.0.1`，**不能**写 `localhost` /
  `lclhst.build`（`isDebug()` 认这两个串）。真远程 bundle 上实测命中 2。
- **推回去**自己保证原子与可退：先在远程把原文件备份到 `~/.nexus-sand-backup/<commit>/`（只备第一次，重装不
  覆盖），再解到**同一文件系统**上的临时目录、逐个 `mv`。远程备份留在远程——笔记本丢了远程也得能自己还原。
- **选 commit**：远程常堆着好几份 server，只改与本机 Cursor 同 commit 的那份（server 版本由客户端决定，其余是
  历史残留）；对不上直接拒绝而不是挨个试。
- **系统 `ssh`，不用 Rust SSH 库**：用户的 `~/.ssh/config` 里有别名、`ProxyCommand`、跳板、agent；换库要把这些
  全部重写，而且 `-R` 也得自己实现。代价是 GUI 没有 TTY，一律 `BatchMode=yes`，要求「终端里 `ssh <host>`
  能免密进去」，应用只校验不接管认证。`ControlMaster` 复用连接是必需不是优化（走跳板每条命令重新握手能慢
  到十几秒）；`ControlPath` 是 unix socket、路径有 104 字节上限，**不能**拿应用数据目录拼，放 `/tmp/nxs-<hash>/%C`。
- **隧道是受监管子进程**（`remote/tunnel.rs`）：第一次手工验证用 `ssh -f -N -R`，它悄悄死掉、端口随之消失，
  表现和补丁没打对一模一样。现在是 tokio 任务循环 spawn `ssh -N -v -R`，用 ssh 自己的 `remote forward success`
  行判定「已连接」（比进程活着准），退出退避重连（2s→30s，撑过一分钟归零），状态放 `watch` 给界面，应用退出
  时显式收掉。

### 9.3 组装与界面

`nexus-sand` 与 `nexus-gateway` 互不认识；隧道端口要跟着网关的透传端口走，这个依赖在 Tauri 层的
`commands::sand_remote::RemoteSandHub` 里完成（同 ARCHITECTURE §3.1「组装只在 AppState」）。主机列表存 settings
（`sand.remote_hosts`），**不存任何凭证**。

界面是 Sand 页的第二张卡「远程主机」而不是第四个 UseTab：同一个心智模型，而且本地补丁与远程连接的相互
影响能摆在同一屏。每台主机三个灯——补丁、隧道、网关——都亮才算通；「网关没在跑」单独横幅提示，不替用户
开（网关的开关有自己的语义）。装完远程 server 进程已重启，但 Cursor 那个窗口要用户自己 Reload Window，
文案如实写。

### 9.4 验收状态

- 单元：profile 表、端点规则幂等 / 可逆、脚本生成（备份先于覆盖、同 fs 临时目录）、ControlPath 长度、隧道
  失败分类与停止。
- 端到端（`tests/remote_roundtrip.rs`，`#[ignore]`，需 `SAND_SERVER_MIRROR` 指向一份原版远程 bundle 镜像）：
  只把 ssh 换成假的，其余全是真代码真文件。装 → 4 个文件改动、Server profile 完整、端点在、远程备份是原版
  字节；重装无事发生且不覆盖首次备份；卸载逐字节回到原样并清掉远程备份；外部 marker 拒装且不重启远程。
  **真远程 bundle 镜像上全部通过。**
- 真机（2026-09-04，`devbox-01`，公司内网代理 + 跳板，远程 3.19.7 `90de2327`）：**整条链路逐段验过**。
  远程盘上 13 处 marker 里 12 处在（缺的那处见下），端点写着 `http://127.0.0.1:8688`；远程
  `curl 127.0.0.1:8688` 0.23s 拿到本机网关响应；远程打 `aiserver.v1.InferenceService/Stream` 回
  `requested_model is required`——**准入过了**（同一条链路打已下线的 `RunInference` 回
  `Sand traffic is not supported on this endpoint`，与 §10 记的一致，是服务端拦 sand，不是链路问题）。

### 9.5 真机上暴露的两件事（2026-09-04）

1. **隧道的生命周期必须跟着应用**，否则 remote SSH 会**静默**失效。端点是写进远程文件的永久状态，隧道
   只活在应用进程里：17:50 换包重启后隧道没了，到 22:33 用户手动打开之前，远程 Agent Host 日志里
   12 次 `ECONNREFUSED 127.0.0.1:8688` → 重试，而桌面端这边「补丁已装」「网关在跑」两个灯都是绿的，
   用户在 Cursor 那边只看得到一直转圈。修法两条：启动时 `RemoteSandHub::restore_tunnels()` 把
   `routeViaLocal` 的主机的隧道拉回来（不走 ssh 探测——每台一次往返会拖住启动，而多起一条闲置
   `ssh -N` 的代价远小于静默连不上）；界面上「盘上有端点 + 隧道不通」不再只是一个中性的灰标签，
   而是一条写明后果的红横幅（`tunnelBlocker`）。
   顺带把 `Tunnel::start` 的「已经在跑就什么都不做」改成认**任务还活着**：只看手柄还在的话，监管任务
   一旦 panic，开关就永远点不动了，症状同样是「界面说在连、系统里一条 ssh 都没有」。
2. **规则表更新后，远程要重装一次**。16:56 那次安装用的是加 browser flag 规则之前的构建，远程
   4883.js 上 `,useClientSideSubagent:!0};` 的锚点还是原样 → 12/13，界面报「补丁不完整」。
   远程盘上的补丁不会跟着桌面端升级走，规则表一变就得重点一次「安装到远程」。

### 9.6 出网方式变成三条路；从远程实地验一遍（2026-09-06）

同族的 Python 工具（`sand_stream_installer_tools_grokbot_direct_v131`，锁死 3.18.9）在 remote 这一段用的是
另一条思路，其中两样值得照搬，一样值得**不**照搬。

**照搬一：代理出网，成为第二条路（[`RemoteRoute::Proxy`]）。**
`ssh -R` 送过去的不是我们的网关，而是**用户本机的 HTTP 代理**；远程照旧打官方 `api2.cursor.sh`，
端点一个字节都不改。远程会话怎么知道要走代理，机制不是我们发明的，是 `anysphere.remote-ssh`
扩展自己的：它把 `remote.SSH.httpProxy` / `httpsProxy` / `noProxy` 三个设置的值
`export HTTP_PROXY=…` 写进**远程 server 的启动脚本**（同时进 `SendEnv`）。这一点是当场核实过的
（扩展 1.1.14 的 `dist/main.js` 里那几处 `Fe.HTTP_PROXY = …` / `export HTTP_PROXY="${e}"`，
CHANGELOG v0.0.24 写着「and during the remote sessions」），**不是**推测。

顺带排掉一个更干净的方案：远程侧没有 `server-env-setup` 这类钩子可用 —— 扩展生成的 bootstrap
脚本里根本没有它（`grep -c` 得 0）。所以「让远程经代理出网」只能落在那三个设置上，没有别的入口。

两条路的取舍要写在选项旁边，因为它是用户真正要做的权衡：

| | 经本机网关 | 经本机代理 |
|---|---|---|
| 改端点 | 改（第 18 类规则） | **不改**，走官方端点 |
| 隧道那头 | 本机网关透传口 | 本机 HTTP 代理 |
| 号池接力 / 记账 / 面板拦截 | 有 | 没有，用远程当前登录那个号 |
| 额外要求 | 网关在跑、client-type 是 sand | 本机代理在跑；要写 Cursor 的用户设置 |

**代理设置是定点文本编辑，不是 JSON 往返**（`remote/proxy.rs`）。`settings.json` 是 JSONC，
用户在里面写注释是常态 —— 本机那份就有一条「sand 补丁按版本硬绑，自动更新会把补丁覆盖掉」，
正是关于这个功能的备忘。解析再 `to_string_pretty` 写回会把注释和缩进全部抹掉：改一个端口的代价
是毁掉用户的文件，这个交换任何时候都不成立。所以只做三件小事 —— 顶层插一个键、在已有对象里
插 / 换一个 host 条目、把那条删掉 —— 其余字节原样不动；扫描器认字符串转义和两种注释，所以
`{` / `"` 出现在注释里不会把它带偏。另外**只写按主机的对象形式、只碰自己那一台**：发现那一项
已经是个对所有远程生效的字符串就报错而不是覆盖（替掉它会静默改变别的主机的出网方式）。
写之前把原文备份进我们自己的数据目录，不在用户那边留垃圾。

**照搬二：端到端探针（`RemoteSand::probe`）。** 这是「三个灯都绿但用户那边一直转圈」唯一的解药。
之前能查的只有各段的**状态**（补丁装了、隧道说已连接、网关在跑），而状态全绿仍然可能不通：
远程端口被别的进程占着、本机代理只听 `::1` 不听 `127.0.0.1`、公司代理拒绝 `CONNECT`……
这些都要真发一次请求才看得见。做法：一条**临时**高位端口的 `ssh -R`（不碰常驻那条 —— 它可能正被
一个开着的 Cursor 窗口合法占着，拿它去测会把「别人在正常用」误读成「端口被占」），远程用
**server 自带的 node**（每份 server 根下都一定有，而 curl 在精简镜像里经常没有）跑一段脚本：
网关模式 `GET /` 到转发端口；代理模式 `CONNECT api2.cursor.sh:443` → TLS → `GET /`。结果**分阶段**
（`tunnel` / `proxy` / `tls` / `http`），失败时那个阶段就是断点 —— 同样一句「不通」，断在隧道要去看
端口占用，断在代理要去看是不是填了 SOCKS 口，断在 TLS 基本是代理在做中间人，三种下一步完全不同。
几百的状态码都算通：验的是链路，不是鉴权。

**照搬三：把 `RemoteForward` 写进 `~/.ssh/config`（`remote/sshcfg.rs`）。**
隧道挂在用户自己那条 Cursor Remote-SSH 连接上：Cursor 连着它就在、断了跟着走，**Nexus 不开也成立**。
转发存在的区间恰好等于远程会话存在的区间 —— §9.5 那种「端点是永久的、隧道只活在应用进程里」的
生命周期错位，从根上没有了，不再需要 `restore_tunnels()` 去追。

一开始判它「不搬」，理由是它拿走了 §9.5 换来的**可见**（状态点 + 重连次数 + ssh 原因）与**自愈**
（退避重连）。那个判断只在「没有别的办法知道通没通」时成立 —— 而探针（照搬二）恰好把这一半补上了。
所以两种并存，做成**每台主机二选一**（`TunnelMode`），因为它们真的互斥：同一个远程端口只能被一条
连接绑住，两边同时开，后起的那条拿 `remote port forwarding failed`。

| | 应用管（`Supervised`，默认） | 挂在 Cursor 的连接上（`SshConfig`） |
|---|---|---|
| 谁维持 | 我们的 `ssh -N -R` 子进程 | 用户自己那条 Cursor 连接 |
| 状态从哪来 | 进程 + ssh 的 `-v` 输出（相位 / 重连数 / 原因） | 远程那个端口有人听吗（`remote_port_open`）+ 探针 |
| 自愈 | 有（退避重连） | 由 ssh 自己带（Cursor 重连就重建） |
| 应用不开时 | 没有隧道 | 照旧有 |

ssh-config 模式下**界面不能照 supervised 那套判警**：我们这边的相位永远是 `stopped`（隧道不是我们
起的），照旧判会让一台其实通着的主机天天挂红横幅 —— 那种「天天误报」比不报更糟，用户很快就不看它了。
所以那个模式下只在一种情况报警：远程已经指着我们，而配置里那行转发不在（没有任何东西承接流量）。

编辑纪律和 Cursor 设置那边同一条，另外三处是这份文件特有的：

1. **`Host *` 不能碰。** 本机那份 config 第 2 行就是 `Host *`（通用设置），它也匹配目标主机 ——
   往里插一条 `RemoteForward` 等于给**每一台**主机都开这个反向转发，连别人的跳板机都会被塞一个
   监听端口。所以只认「模式恰好就是这个别名」的块；共享块（`Host a b c`）也不碰，另起一个专属块
   （`RemoteForward` 是可累加指令，另起一块与插在原块里等效）。
2. **转发要显式绑 `127.0.0.1`。** 只写端口号时，sshd 按 `GatewayPorts` 决定绑哪个地址 —— 配了
   `GatewayPorts yes` 的机器会绑到 `0.0.0.0`，那就把本机的网关 / 代理暴露给整个内网了。
3. **写完用 `ssh -G <host>` 验一次解析得过，不过就立刻还原。** 这份文件坏掉的后果不止是我们的功能
   不通，而是用户所有的 ssh 都连不上。

`user@host` 那种目标不支持这个模式（ssh 的 `Host` 模式里不能有 `@`），报错并指路「起一个别名，
或改用应用管那种」，而不是写出一个永远匹配不上的块。

其余变化：`TunnelSpec` 分出 `remote_port` / `local_port`（代理模式两端不同号 —— 远程那台上 7890
常常已经被别的东西占着）；`RemoteHost.route_via_local: bool` 变成三值的 `route`，老配置里的布尔
按 `true → Gateway` / `false → Direct` 归一（读丢了会让这些主机装出一个不改道的补丁，而远程出不去网，
症状是装完就不通）；`tunnel_mode` 缺省是 `Supervised` = 升级前的行为。

两处永久状态（Cursor 的代理设置、ssh config 里的转发行）由 `sync_persistent_state` **整体过一遍**，
而不是各处零敲碎打：「切走某个模式时忘了摘掉上一个模式留下的东西」是这类联动最容易漏的地方，
而漏掉的表现是远程去连一个早就不存在的端口。改设置 / 装补丁走它，移出主机 / 卸载补丁走
`clear_persistent_state`。写失败（代理没开、有个全局代理、ssh config 认不出）时**先落盘再存主机**
的顺序保证列表与盘上始终一致。

### 9.7 `ssh -R` 在容器平台上是死路；隧道改成 ssh 会话里的多路复用中继（2026-09-08）

用户报「乱七八糟，而且无法使用」。截图那台 `devbox-01`（公司内网的开发机，经跳板机 ssh 进去，
再由网关转进容器）上，§9.5 / §9.6 那一整套的三个前提**都不成立**，真机逐条核过：

1. **`-R` 被网关吞了。** `ssh -R 127.0.0.1:47123:… devbox-01 'ss -ltn'`：ssh 不报错、命令照跑，工作区里
   却没有 47123 在听。`-L` 正常（Cursor Remote 本身能连，就是靠它）。这类平台的 ssh 网关只把命令转进
   工作区，反向端口转发要么落在网关那台机器上、要么直接丢。`ExitOnForwardFailure` 也救不了——请求是被
   **接受**的。所以「应用管」的隧道永远连不上，「挂在 ssh config 上」的 `RemoteForward` 每次 Cursor 连接都
   报一句 `remote port forwarding failed`（用户看不见）。
2. **「远程端口有人听」全是误判。** 工作区里 `0.0.0.0:7897` 确实有人听——但那是平台自带的出网代理
   （进程不在容器的 pid namespace 里，`curl -x http://127.0.0.1:7897 https://api2.cursor.sh` 回 200，直连 000）。
   `remote_port_open` 于是报「常驻转发在（远程 7897 有人听）」，其实和我们没有一点关系。同一个事实还
   解释了为什么第一版「两端同口」（远程端口 = 本机代理端口 7897）永远撞端口。
3. **用户看到的「无法使用」是第三样东西。** 远程 Agent 走的是平台那个代理（`HTTP_PROXY=127.0.0.1:7897`，
   remote-ssh 扩展注进去的），对 api2 `[aborted] read ECONNRESET`，重试 3 次后放弃——既不是补丁、也不是
   我们的隧道，是那条代理不稳。可我们的界面对此一个字没说，反而摆着「隧道由谁维持」「远程监听端口（可选）」
   这些在这台机器上没有任何一项能成立的选项。

**替代路径当场验过可行**：ssh 的 exec 通道对二进制透明（300 字节随机数据经 `node -e 'process.stdin.pipe(process.stdout)'`
原样回来），远程有 cursor-server 自带的 `node`。于是隧道改成 **stdio 多路复用中继**（`remote/relay.js` +
`remote/tunnel.rs`）：远程用那个 node 在 `127.0.0.1:<remote_port>` 监听，每条连接编号后经这条 ssh 的
stdin/stdout 回到本机，本机这头把每一路接到网关透传口 / 本机代理口。帧是 `1 字节类型 | 4 字节流号 | 4 字节长度 | 载荷`
（OPEN / DATA / EOF / CLOSE），双向都有背压（写不动就 pause 对应 socket / stdin）。就绪以中继打出的
`NEXUS-RELAY 1 READY <port>` 为准；起不来打 `ERROR <code>`（`EADDRINUSE` 翻成「远程端口被别的程序占着，换一个」，
远程没 node 翻成「先用 Cursor 连一次这台机器」）。只依赖「ssh 能执行命令」——Cursor Remote 自己就靠它活着。
不改 `~/.ssh/config`、不受 sshd 转发策略影响、端口由我们选由我们验。

两个真机上踩到的细节：远程侧 `net.createServer` 要 `allowHalfOpen: true`，否则客户端发完请求半关（FIN）时
Node 把写侧也关了，应答尾巴丢失（表现为 curl `Empty reply`）；本机侧那一路要等远程回 CLOSE 才收尾，本机
接收方不在（网关没开）时立刻发 CLOSE 让远程客户端被拒，别吊着。`tunnel.rs` 里有拿本机 `sh` + 本机 `node` 跑
同一份启动器的端到端测试（三路并发各 200K 往返、HTTP 式一问一答、接收方缺席、远程端口被占），远程会发生的事
两头都在这台机器上复现。`cargo run -p nexus-sand --example relay -- <host> <远程端口> <本机端口>` 是命令行版，
`--example probe_remote -- <host> <远程端口> [api2.cursor.sh]` 是「验一遍」的命令行版，排障不用开界面。

**装完中继之后用户报「还是不行：Connection failed」（2026-09-08 14:32）**——远程 Agent Host 日志是
`ECONNREFUSED 127.0.0.1:8688`：bundle 里还留着早期「两端同口」时写进去的旧端点，界面上点了「重新安装」却回
「已经是目标状态」。根因是远程那条 `RemoteSand::install` 只按选项组规则表（`rules::catalog`），不知道盘上装着
什么：新端点的 `original` 锚点早已不在，计划为空。本机那条路（`SandService::install`）一直是
`catalog_with_installed` + strip，远程漏了。修法就是对齐：读盘上端点、旧端点当 legacy 原地迁移、plan 为空时
只有「补丁齐 + 端点是选项要的」才算无事发生、写后校验也数端点。顺带抓到 strip 的一个 bug：剥掉旧端点后
Apply 的规则表里仍带着那条端点规则（为了 status / uninstall 认得它），刚还原出来的 original 又被装回去——
「关掉推理经本机网关再重装」在真 bundle 上根本关不掉；现在 Apply 时排除 strip 里同 id 的规则。
`tests/remote_roundtrip.rs` 在 3.19.13 的真 bundle 镜像上多了「换端口重装 → 关掉改道重装 → 卸载」三步
（镜像用 `~/.nexus-sand-backup/<commit>/` 里的原版字节还原出来）；那条 ignored 测试的假 ssh 之前把每个数都翻倍
（脚本里每个目标出现两次），静默失效了几个月，一起修了。

**devbox-01 真机（2026-09-08 13:41–13:50）**：中继就绪约 15 秒（走跳板）；远程 `curl http://127.0.0.1:41782/` 到本机
http.server 200，5 路并发全 200，3 MB 文件 sha256 两头一致、约 1.1 MB/s；代理模式 `curl -x http://127.0.0.1:41783
https://api2.cursor.sh/` 经本机 Clash 7897 出去 200（≈0.95s，与平台自带代理持平）；探针网关模式 / 代理模式各报
`stage=http, status=200`，打一个没人听的口报 `stage=tunnel, ECONNREFUSED`；在远程 `kill -9` 中继进程后本机
状态走 Reconnecting → Connected（重连 1 次，约 20 秒），之后 curl 照常 200。

跟着删掉的：`TunnelMode`（只剩一种隧道）、`remoteProxyPort` + 「两端同口」（变成每台主机一个独立的
`remote_port`，默认 `41777`——刻意避开 7890 / 7897 / 8080 这些远程上常有人的号；老配置的
`remoteProxyPort` 读进来、`tunnelMode` 忽略）、`remote_port_open`、`run_with_forward`（探针不再另起临时
`ssh -R`，直接打常驻中继的远程口——远程 Agent 用的正是它，验别的口验不到真问题）。`sshcfg.rs` 只留 read /
remove：启动时和移除主机时把早期版本写进用户 `~/.ssh/config` 的 `RemoteForward` 块清掉。

界面：一台主机一张卡，三盏灯（补丁 / 隧道 / 本机那头）+ 一句结论；隧道那格显示活着的连接数（有数就是真在用）；
网关模式多一条 `endpointDrift`——盘上写的端点和现在设置该写的对不上（改过远程端口、或早期「两端同口」装的）
就点名说出来并给「重新安装」，否则隧道全绿、推理照样打在旧端口上。

---

## 10. 推理引擎：Direct 唯一；Session 已下线（2026-09-03 提出，2026-09-04 下线）

> 状态（2026-09-04，Cursor 3.19.7）：**Session 引擎在 sand 身份下已被服务端封掉**——`RunInference` 对
> `x-cursor-client-type: sand` 一律回 connect code 3（InvalidArgument）`Sand traffic is not supported on this endpoint`，
> 同一请求换 `ide` 身份能进到计费环节（`ERROR_RATE_LIMITED`），说明接口本身没变，是专门拦 sand（探针
> `gateway/scripts/probe-run-inference.mjs`）。Agent Host 日志里 Session 会话每次推理都是 `connectCode: 3` →
> `decision=RETRY`，界面表现为一直 Reconnecting。**Direct（`Stream`）仍然通**，是 sand 唯一能用的引擎；
> 3.19.7 上 Direct 的注入体见 §10.5。
>
> **产品决定：Session 从 Sand 下掉**（安装器 v1.2.9-direct-only.1，Rust / 前端同步）。不再有「推理引擎」选项，
> `InstallOptions.stream_engine` / `SandStatus.stream_engine` / 前端 `StreamEngine` 全部删除；Session 留在盘上的
> 空 marker 改名 `LEGACY_SESSION_STREAM_MARKER`（值不变），status 计入 `legacy_markers`、install 原地迁成 Direct、
> uninstall 剥掉。§10.1–10.2 保留作历史记录：它解释了 Session 曾经为什么值得做，以及服务端哪天放开时怎么回来。

### 10.1 Direct Stream 是基于一个误判造出来的绕路

CURSOR-FULL-ARCHITECTURE.md §B.2 说 attempt 工厂（3.18.25 的 `gre`）原版 `yield` 的那条 `runInference`
「连到 connect runtime（= api5 的 agent.v1）」。**3.18.25 的 `61.js` 逐字读出来不是这样**：

- `RunInference` 是 **`aiserver.v1.InferenceService`** 上的方法（`kind: BiDiStreaming`，与 `Stream` 同一个 service）；
- 它有一条 **method 级** 路由 override：`_overrideMethodNameToTransportMap["RunInference"] = e.agenticComposerTransport`，
  优先级高于 service 级；
- `agenticComposerTransport = bidiTransportFactory.createTransport({baseUrl: o, useHttp2: !r, …})`，而 `o` 就是
  `cursorCreds.backendUrl` —— **api2**。

所以官方 local-loop 的推理路径是：`runRequest{conversationId, requestedModel, routingConversation, agentMode}` →
服务端回 `runReady{resolved_model, supports_self_summary, routed_model_display_name, prompt_model_metadata}` →
之后每次推理用 `invokeModel{invocationId, request: InferenceStreamRequest}` 在同一条流上复用 → `finishRun`。
`RunInferencePromptModelMetadata` 有 33 个字段（`vendor` / `prompt_version` / `is_opus5` … `use_dsv3_harness` /
`agent_token_limit` / `estimated_cache_ttl_ms`）——Direct 注入体里那一大段 `isOpus5 / isGpt56 / vendor / agentTokenLimit`
就是在客户端手工复刻这个结构，还缺最后两个字段。当时被封的只是 `agent.v1.AgentService/Run @ api5`，`InferenceService`
在 api2 上对 sand 是开的（`probe-inference.mjs`）。**2026-09-04 复测：`InferenceService/Stream` 仍开，
`InferenceService/RunInference` 对 sand 已关**（见本节开头的状态行）——「误判」这个结论只对 2026-09-03 之前成立，
Direct 从绕路变成了唯一路。

### 10.2 Session 引擎曾经做什么：什么都不做（历史）

`managed-local route` + `local runtime load` + `move_exec` 三条补丁已经把 Cursor 摆进了它自己出货过的
「本地 loop + 同进程 exec」组合（§B.5）。这个组合的推理原本就走 `RunInference`。Session 引擎因此**不注入任何逻辑**，
只在锚点后放一个空注释 `/*SAND_SESSION_INFERENCE_STREAM_V1*/`（幂等 / 状态 / 互斥用），让官方路径原样跑。

相对 Direct 多出来的能力全是服务端给的：Auto 路由（`routingConversation`）、按模式的提示词元数据（`agentMode` 随
runRequest 上送）、`supports_self_summary`、`estimated_cache_ttl_ms`、以后新增的任何字段。Cursor 升级时不用再逐版本
补模型标志。——这些都以服务端肯给 sand 开 `RunInference` 为前提；它只开了不到一天。

marker 字符串与同类工具 v1.2.7 "session-stream" 逐字一致——那类工具装过的机器现在也认得出（计 legacy）、能精确迁移。

### 10.3 落地（Direct-only）

- `InstallOptions { self_summary, mode_gate, relaunch, inference_endpoint }`，没有引擎字段；serde `default` 让老前端
  传来的 `streamEngine` 被忽略而不是报错。
- `RuleId::InferenceStream.markers()` 只有 `SAND_DIRECT_STREAM_MARKER`，期望 1；`inference_stream_rule(self_summary)`
  的 `legacy_injections` = 其余 11 种 Direct 变体 + `LEGACY_SESSION_STREAM_MARKER`。
- `engine::apply` 的 `AnchoredInsert` 顺序：当前体在 → 不动；任一 legacy 在 → 原地换（计 migrated）；本类 marker 在
  → 不叠加；否则锚点后插入。legacy 检查排在 marker 检查前面是因为 Session 的 marker 不在本类 marker 表里，反过来会
  把 Direct 体叠在它后面。`engine::inspect` 把 `LEGACY_SESSION_STREAM_MARKER` 计入 `legacy`（同 task tool V1–V6）。
- `SandStatus` 只剩 `self_summary` 报盘上实际取值；前端 Sand 页去掉「推理引擎」下拉，自摘要开关常显，
  `MARKER_ROWS.inferenceStream.desc = 推理引擎（直连）`。盘上是 Session 的机器由既有的 `legacyMarkers > 0` 横幅提示
  「点安装原地升级」。
- Python v1.2.9-direct-only.1：删 `SAND_STREAM_ENGINE`；`LEGACY_SAND_SESSION_STREAM_MARKER`；
  `PatchStatus.legacy_session_stream_markers`；`stream_mode_installed` 要求 direct 1 且 legacy session 0；install 硬校验
  `(after_direct, after_session) == (1, 0)`；`RemoveStats.legacy_session_stream`。
- 验证（2026-09-04）：`cargo test -p nexus-sand` 76 + 7；真机 in-memory 对拍自摘要开 / 关两遍 11 文件全等；Python 在
  真机 4883.js 上：Session marker → Direct 与全新装逐字节一致、旧裸调体 → 带中间件体一致、幂等、可卸；前端
  `tsc` / vitest 262 通过；`cargo test --workspace` 全绿。

### 10.4 边界与后续

- **端点改道只对 Direct 生效（现在只有 Direct，问题消失）。** §9 的端点改道（`InferenceEndpoint`）挂在 service 级
  `_overrideServiceNameToTransportMapLowerPriorityThanMethodOverrides["aiserver.v1.InferenceService"]` 上，
  而 `RunInference` 有 method 级 override 抢先——Session 引擎下远程的推理不会进我们的 passthrough，直接去 api2。
  Session 在的时候靴子是 `remote::effective_remote_options` / `service::with_inference_endpoint` 强制 Direct；
  Session 下线后两处强制随之删除。若服务端哪天放开 `RunInference` 想把 Session 加回来，得同时补
  `_overrideMethodNameToTransportMap["RunInference"] = e.sandInferenceTransport`，且那条 transport 用 h2
  （BiDi 不能走 HTTP/1.1；passthrough 的 auto 接受循环已支持 h2c BiDi）。
- ~~Task 工具 V5 的父模型取 `i`……做 V6~~ **已做**（§6「父模型名为什么从 `i` 换成 `e.requestedModel.modelId`」）。
- Composer 系模型在本地 loop 下会被 `yre("dsv3-harness-not-supported")` 明确拒掉（"Local loop can't run this
  model"）——与引擎无关。sand 的目标模型（grok / claude / gpt）不受影响。
- 若服务端哪天对 sand 放开 `RunInference`、想把 Session 加回来，真机验收清单：① 高级模型出字、`Routed to …`
  显示服务端给的名字；② shell / 读写文件 / MCP；③ Task / Explore / 自定义子代理 / 指定变体 / Resume；④ 长会话
  自摘要触发；⑤ Auto 模型能路由；⑥ 网关日志或抓包确认请求落在 `api2 /aiserver.v1.InferenceService/RunInference`，
  没有任何 `agent.v1` 流量。（2026-09-04 起 sand 身份下 ① 就过不了——见本节状态行。）

### 10.5 3.19.7 上 Direct 链路的逐条复核（2026-09-04）

按 3.19.7 实际调用链（`main.js` → `9909.js` 路由 → `4883.js` attempt 工厂 / AgentConfig）逐条对过，不只看字面量：

- **路由（`SAND_MANAGED_LOCAL_ROUTE_V1`）**：`f(e)` 里 `checkFeatureGate("agent_host_local_loop")` 仍执行但结果被
  `if(!1)` 忽略；之后 `v(o,e,r)` 返回非 undefined 时走 `h()` → `{runtime:"fail"}`。3.19.7 不再回退 connect，所以
  action 放行档位直接决定成败。`v` 新增前置 `isManagedInferenceHttp2Available()`（查 `runInference` 的 transport
  是否 h2）与 privacy-mode 检查，都在放行之前，与我们无关。
- **action 放行（`SAND_MANAGED_ACTION_ROUTE_V1`）**：官方只放 `userMessageAction`（AGENT 档 / hosted 子代理
  UNSPECIFIED 档）+ `backgroundTaskCompletionAction`；补丁加 `summarizeAction` / `resumeAction` 走同一条
  `T(e,r)`（模型必须有、私有凭据拒、customSystemPrompt / harness / excludeWorkspaceContext / directMetaSubagent 拒）。
- **attempt 工厂（`SAND_DIRECT_INFERENCE_STREAM_V1`）**：官方 `ve` 返回
  `{promptSession, promptToolSession:{getExecutor:e=>new o.Ycw(...)}, attempt:{resolvedModel, supportsSelfSummary,
  routedModelDisplayName, resolvedModelMetadata:{promptModelInfo, useDsv3Harness, agentTokenLimit, estimatedCacheTtlMs,
  persona, featureFlags, promptConfig}, finish}}`。`Fe(e,t)` 读 `resolvedModelMetadata.promptModelInfo`（没有就
  `unrecognized-model-family`）、`.useDsv3Harness`（我们不设 → 走普通分支）、`.featureFlags ?? be`、
  `.promptConfig?.enableLineNumbers`（缺省 true）、`.agentTokenLimit` / `.estimatedCacheTtlMs`（可选）。注入体给的
  `{promptModelInfo:oe(a,d), agentTokenLimit}` 覆盖全部必需字段；`oe()` 3.19.7 多读 `isGrok46ProductPrompt`，已补。
  **本轮补的缺口**：官方 `getSession(...)` 带 `(0,o.sXH)((0,o.got)({imageResizing:{webpWithoutCodec:"passthrough"},
  …, supportsAssistantMessagePrefill:!0},{}))`——`got` 组装执行器中间件：图片缩放（超限截图缩到供应商能收的尺寸，
  缩不动的换占位文本，`@anysphere/chat-inference/image-resizing-middleware`）+ 请求指标；loopNudge /
  progressReminder / effortLevel 三段只在服务端 `promptConfig` 给了才挂，Direct 没有 runReady 所以和官方一样不挂。
  之前注入体是 `getSession()` 裸调，带图会话直接把原图送出去。现在注入体挂同一条链，Python / Rust 逐字节一致，
  旧的裸调体（3.19.7 首版）与 3.19.7 之前的扁平体都作 legacy 认、install 原地迁移、uninstall 都认（12 种变体）。
- **Task 工具（`SAND_MANAGED_TASK_TOOL_V7`）**：官方 `Ae()` 3.19.7 返回 `parentRequestedModelName / getTaskToolConfig
  (抛 "managed local loop does not build in-process child AgentConfig") / modelInfo / isModelBlocked / isModelValid /
  forceModelId / subagentModelForcePolicy:"parent_pin" / subagentModelOverrides:{} / …`。我们的字面量少 `modelInfo`
  （只有 dsv3 harness 工具集读它，我们不走）、少 `forceModelId` 与 `subagentModelOverrides`（消费方 `QE/JE` 对
  `forceModelId===undefined` 返回 false → 用默认子代理集合；`Zm` 对 overrides 做 `e?.[t] ?? Object.entries(e??{})`，
  缺省安全）。`e.requestedModel` / `e.runOptions.selectedSubagentModels` / `p` / `n.modelName` / `v` 都在 `Fe` 普通分支
  的作用域里。
- **上下文窗口（`SAND_CONTEXT_WINDOW_V1`）**：落在 `class W`（Direct 实际用的执行器，`constructor(e,t,n,o,r)` 里
  `this.requestedModel=n`）的 `extendedUsage` 分支上，`this.requestedModel?.parameters` 可达。
- **`main.js` 三条**：`SAND_LOCAL_RUNTIME_LOAD_V1` 让 `t=!0` 绕过 `agent_host_local_loop`（3.19.7 同一函数里新增
  private-inference 分支在它之前 return，不冲突）；`SAND_AGENT_HOST_MOVE_EXEC_V1` `f=!0` → `y=f||g`；
  `SAND_AGENT_HOST_IDENTITY_V1` `clientType:"sand"`——启动日志 `Selected Agent Host turn runtime
  {"runtime":"managed-local","reason":"sand-client"}` / `move_exec ON` 佐证三条在真机生效。
- **`9909.js` 子代理两条**：resume 把 `resumeAgentId && mode===UNSPECIFIED && !readonly` 提成 AGENT（过路由的档位
  检查）；bubble 让 `hr()` 把 UNSPECIFIED 当 BUBBLE_TO_PARENT——3.19.7 的 `vr` 策略表（webSearch/webFetch
  bubble、askQuestion/createPlan surface、其余 auto-reject）是在这之后按类型再分，语义不冲突。
- **workbench 四条**（enablement / client-type ×8 / completion wake 认 `source==="subagent"` / model variants 补
  `legacySlugs`）落点与 3.18.25 同构，`profile_probe` 计数全等。
- **3.19.7 新增、与我们无关**：private inference（`AGENT_HOST_PRIVATE_INFERENCE_GATE`，只在 `privateInference:true`
  时走）、`RemoteAgentHostPresenceService`、`RunInferenceRunRequest.subagent_type_name`（Session 才用）。
- **没在真机验过的**：Direct 下带图会话（这次补的中间件正是为它）、Task / Explore 子代理在 3.19.7 Direct 下的完整
  一轮。前者补丁重装后即可验，后者路由上 `isHostedSubagentChild` 档位已放行。

---

## 11. 本机也走网关：IDE 面板拦截（2026-09-05）

> 状态：代码与单测齐（`nexus-sand` 84、`nexus-gateway` 249、前端 270 全绿），**真机未验**。第一次验收步骤在 §11.4。

### 11.1 为什么本机也要改道

§9 的端点改道（`RuleId::InferenceEndpoint`）原本只给远程用：远程出不去网，把 `InferenceService` 改到
`http://127.0.0.1:<port>` 再用 `ssh -R` 接回本机网关。`model.rs` 当时写死「本机安装永远是 `None`」。

现在反转：本机也可以把推理改道到本机网关的**透传口**。动机不是网络，是**看见**——sand 路径的每一次模型调用都是
一发无状态的 `InferenceService/Stream`，整段上下文在 `messages` 里、用量在响应流末尾的 `extended_usage` 里
（`docs/relay/CURSOR-FULL-ARCHITECTURE.md` §A.2 / §I.7）。谁拿到这一发的字节，谁就能记 Cursor 的
用量、也能在发出去之前改上下文。三种拿法（bundle 再打一类补丁 / 本机终止型代理 / 离线改 `state.vscdb`）
选了代理：`conversation_id` 就在请求里，proto 比压缩符号名稳，passthrough 本来就在。

### 11.2 Sand 侧：端点改道补齐生命周期（`service.rs` / `rules.rs`）

- **status / uninstall 从盘上读端点**（`installed_inference_endpoint` → `catalog_with_installed`）。此前 uninstall 用默认
  选项组规则表，Remove 反向不了那两处，写后校验又按 `RuleId::ALL` 常量数出 2 处残留 → 回滚。
- **换 URL 原地迁移**：新 URL 的 transport 规则把盘上旧 URL 当 `legacy`，与一开始就装新 URL 逐字节一致。
- **关掉再装 = 剥掉**：`build_plan_with_strip` 在 Apply 前先 remove 旧端点规则，同一次写入、同一份备份。
- install 前后都校验端点命中数（要装 2 / 要关 0）；它仍不进通用硬校验，两种状态都合法。
- `SandStatus.inference_endpoint` 报盘上实际值；界面开关**跟盘上初始化**（与自摘要相反：这不是要迁移的默认值，
  静默剥掉会让 Agent 面板当场断线），读不到网关状态时退回盘上现值。

### 11.3 网关侧：透传口上唯一「看懂内容」的路径（`nexus-gateway/src/intercept.rs`、`wire.rs`）

只对 `/aiserver.v1.InferenceService/Stream` 解 body，其余路径照旧盲转发（`passthrough.rs` 模块文档的「做不到」对它们仍成立）。

- **请求侧**：ServerStreaming 的请求就是一个信封，收满再转不伤流式。改写在 protobuf **线格式顶层逐字段**做（`wire.rs`），
  只重编码被改的那一条 user 消息，其余字段字节不动。理由：`proto.rs` 按 3.18.9 生成，3.19.7 的 sand 请求带它没有的字段
  （WATCHLIST §1.5 的 75 / 77 / 79），整包 prost decode → encode 会把它们**静默抹掉**。测试
  `unknown_top_level_and_message_fields_survive_a_rewrite` 钉住这一点。
- **改写规则**（`RewriteRule`，落库 `gateway.ide_rewrite`，热改即生效）：默认关；哨兵原样插入最后一条 user 末尾（`tail`）
  或第一条 user 开头（`head`，Cursor 把 system 折在那里）；user 消息没有文本（tool 结果）就往前 / 后找下一条。
- **响应侧**：`TeeBody` 帧原样转给 IDE，同时喂只读解码器挑 `extended_usage` / `response_info.model`（实际路由模型）/
  流内与流尾错误；流走完、出错、客户端断开（Drop）都结一次账，只结一次。只读解码不怕 proto 旧。
- **账本**：一次 Stream = 一行，`dialect` 列写 `ide-agent`；默认 `summary` **排除**它、`summary_source` 单看它——方言口是标准
  API 客户端的请求，这边是 Cursor 每一轮的模型调用（几十万 token 一行是常态），混着谁也看不懂。
- **界面**：Sand 页的「IDE 面板拦截」卡（开关 / 哨兵位置 / 哨兵文本 / 今天与 7 天用量 / 最近 50 条）。只有名字和数字，
  没有对话内容；要看内容只在 `NEXUS_PASSTHROUGH_DUMP_DIR` 取证时落盘。2026-09-08 从本地网关页搬到这里：它拦的是
  **Sand 改道过来的** Agent 面板流量，没有补丁 + 改道就没有东西可拦，网关只是它借的那条管子；摆在网关页上，
  用户在网关页看到一张永远空着的卡、到 Sand 页又找不到「改写在哪」。

### 11.4 第一次真机验收（未做）

1. 本地网关页开网关（client-type 建议 `sand`）。Sand 页打开「推理经本机网关」→ 重新安装（退出并重启 Cursor）。
   期望：status 里 `inference endpoint` 2 处，「盘上：经网关」。
2. Agent 面板随便问一句。期望：Sand 页「IDE 面板拦截」出现一行，`conversation_id` / 模型 / token 齐；网关日志有
   `IDE 拦截：转发推理请求`。这一步同时证明改道在本机生效、真实请求能被解开。
3. 开「上下文改写」，哨兵默认 `[nexus-mark]`。Agent 面板问「把我这条消息的最后一段原样复述」。期望：模型复述出哨兵，
   **且**那一行标「已改写」——两个证据缺一不可（playbook §3.6）。换 `head` 再来一遍。
4. 关掉改写，再问一句：那一行不再标「已改写」，字节与官方一致（可临时开 `NEXUS_PASSTHROUGH_DUMP_DIR` 用
   `gateway/scripts/decode-inference-dump.mjs` 解开看）。顺手确认 dump 里有没有 75 / 77 / 79——有，就该按 playbook §6
   重跑 `gen-proto.py` 对齐 3.19.7。
5. Sand 页关掉「推理经本机网关」→ 重新安装。期望：端点 0 处、「盘上：直连」，Agent 面板直连 api2 正常。

预计失效条件：Cursor 升级改了 transport 组装处（端点规则的两个锚点；3.19.13 就把它从 `9909.js` 挪进了
agent-host `main.js`，锚点符号也跟着换了一轮），或服务端对 Stream 也开始拒 sand。

---

## 12. Grok Bot 额度：三种鉴权形态（2026-09-09）

> 状态：**三种形态都在真机 3.19.13 的 `main.js` 上过了 `node --check`**（`examples/check_main_syntax.rs`），
> Box Relay 已在真机 Agent 面板跑通；直连与「经本机网关」的协议链路分别用 `gateway/scripts/probe-grokbot-header-tolerance.mjs`
> 与 `nexus-grokbot/examples/live_mint.rs` 对真上游验证过，Agent 面板端到端待用户验收。

### 12.1 动机与两个坑

Mac session JWT 直连 `InferenceService/Stream` 回 code 16；renewal 拿到的 **`grokBotToken`**（`type: grok_bot`）可以。
第一版（`/*SAND_GROKBOT_STREAM_AUTH_V1*/`）把 Connect interceptor 塞进 `originTransport` 的 `createTransport({… interceptors:[…]})`，
**从未生效**，两个原因都是读 bundle 时想当然了：

1. `TransportFactory.createTransport(e)` 只解构固定几个字段，`interceptors` 全由内部 `createInterceptors()` 生成——传进去的被静默丢弃。
2. `originTransport` 只服务 `OriginService`；`InferenceService` 走的是 `_backendTransport`。

正确的挂点是 **`TransportFactory.applyAuthorization`**（所有 transport 共用，v135 社区脚本也选它）：在变量声明后、
`if(t.overrideAuthToken){` 前插一块按 `InferenceService/Stream` 守卫的代码，命中就 `return`。外层是 `__awaiter` 包着的
generator，块里可以 `yield` promise（直连块的续期就靠这个）。3.19.13 有两处（agent-host / always-local，只差变量声明顺序），两处都打。

头容忍度矩阵（`probe-grokbot-header-tolerance.mjs`）：IDE 多余头、`x-cursor-client-version: 3.19.13`、别的 machineId 的
checksum、没 `x-sand-box-namespace` 都放行；**只有 `x-cursor-client-type` 必须是 `sand`**（`ide` → code 8）。所以注入块只补头、不删头。

### 12.2 规则 `GrokBotStreamAuth`（第 19 类，可选）：`GrokBotAuthMode`

| 形态 | Marker | 做什么 | 依赖 | 与「经本机网关」 |
|---|---|---|---|---|
| `off` | — | 不动 `applyAuthorization` | — | 可同开（由网关换 token，见 §12.4） |
| `box_relay`（默认） | `/*SAND_GROK_BOX_RELAY_AUTH_V1*/` | `e.url` 改到 Box 内 `/sand-stream-relay/…/Stream`，Bearer 换 descriptor 的短期 token；grokBotToken 留在 Box。**字节级与 v135 一致**，两边互认 | pod 在线；Bot 端装过 relay（教程第 3 步） | **互斥**（URL 被改到 Box，到不了网关；`service.install` 拒） |
| `direct` | `/*SAND_GROKBOT_DIRECT_AUTH_V1*/` | 读 `grokbot-stream-credential.json`，快过期凭 `sbi_*` `yield fetch` 续期（**免鉴权**），Bearer 换 grokBotToken，补 sand 头 | 本机有 Nexus 生成的凭证 | 可同开（补丁换一次、网关再换一次，无害） |

三种互为 Literal `legacy`：盘上装着一种、界面选另一种重装 → 原地换块（计 `migrated`），不必先卸；两种规则表都能卸掉任一形态。
旧 interceptor（`LEGACY_SAND_GROKBOT_STREAM_AUTH_MARKER`）由 `grokbot_legacy_migration_rules()` 在 install 表最前面还原成 stock
锚点（它占的正是端点改道的锚点），`off` 时走 strip；**不进 uninstall 的 catalog**（Literal remove 会把 stock 反向改回它）。

**写法约束**：注入块里满是 `{}`，绝不能过 `format!` 的位置参数——第一版 `,,pingConfig` 语法错误（Agent Host 扩展直接加载失败、
Agent 面板 70 秒超时）就是 `format!("…{}…", iife)` 把 IIFE 里的 `catch(e){}` 当占位符吃掉的产物。只用命名参数或 `concat!`。

Python 侧：`SAND_GROKBOT_AUTH=box_relay|direct|off`（旧 `SAND_GROKBOT_STREAM_AUTH=0` 等价 off）。

### 12.3 凭证：`nexus-grokbot`（纯 Rust，去掉 node 依赖）

Grok Bot 是 Electron 应用，`sand-secrets.json` / `gateway-descriptor.json` 里的字段是 safeStorage 加密：钥匙串取口令 →
pbkdf2-sha1(1003, 16) → AES-128-CBC（IV 16 个空格）。早先靠 `node -e` 解，GUI 版拿不到 shell 的 PATH（nvm）就直接失败——现在全在 Rust 里。

| 模块 | 做什么 |
|---|---|
| `app` | 装没装（`/Applications/Grok Bot.app`）、`desktop-status.json` 的 `signedIn` / pid、`open -a` 拉起 |
| `secrets` | 活跃账号槽：machineId、session / refresh token、email。只解活跃槽 |
| `descriptor` | 按 `sha256(sub)` 取本账号的 Box gateway 地址 + token；落成 **与 v135 同路径** 的 `grok-box-relay.json` |
| `pod` | exec daemon（Connect RPC，`prost` 手写消息）读 `SAND_INFERENCE_RENEWAL_CREDENTIAL`；地址从 descriptor 推（`-1340.` → `-1337.`，`Bearer local` + `x-anyrun-network-token`），推不出退回 `EnsureSandBox(wake)` |
| `credential` | `sbi_*` → `POST /sand-box/inference-credential` → grokBotToken（**无需任何鉴权**，2026-09-09 实测三种头组合都 200）；文件 `grokbot-stream-credential.json` |
| `service` | 门面：`status()`（不碰钥匙串）、`refresh_relay()`、`mint_direct()`、`fresh_stream_credential()`（快过期就续） |

关键事实：**只凭 Box descriptor 就能读到 `sbi_*`**，不需要 EnsureSandBox、不需要 session token 参与任何 api2 调用；续期又免鉴权——
所以直连模式的凭证链完全由 Nexus 自主完成，Bot 端零改动。`sbi_*` 在 pod 生命周期内稳定，grokBotToken 约 10 分钟。

**它不是账号系统**：Cursor 账号仍在 `nexus-accounts`。一份 `GrokBotService` 实例由 AppState / Sand / 网关共用（钥匙串口令只弹一次）。
`service.install` 选了某形态而前提不在（relay 文件 / 直连凭证）会当场去拿，拿不到才报错——不让用户装完发现 Agent 面板报 16。

### 12.4 与端点改道的组合：网关侧的 Grok 开关

`nexus-gateway::grokbot::GrokBotStreamAuth`：透传口对 **`InferenceService/Stream`** 这一条路径，开关开着就不用接力队里的号，改用
`fresh_stream_credential()` 的 grokBotToken（`client_type` 钉 `sand`，`machine_id` 钉 Grok Bot 的），不回报 Lane、不参与接力 / 冷却；
别的路径（`agent.v1.*`、`/auth/*`、其它 aiserver 方法）照旧。设置 `gateway.grokbot_stream`，热生效；开的瞬间没凭证会 `mint_direct()`。

于是「经本机网关」这条路的推荐组合是：Sand 页 Grok 鉴权 = `off`，网关页「Stream 用 Grok Bot 额度」= 开。token 只在网关进程与磁盘文件之间，
不进 Cursor 补丁；顺带把记账 / 改写都接上。

研究细节见 `gateway/scripts/POD-STREAM-RESEARCH.md`。

---

## 13. 收缩：透传口拆掉之后 Sand 长什么样（2026-09-16）

### 13.1 拆了什么、为什么

网关的 h2c 透传口曾经服务两个客户：`cursor-agent` CLI 和 Sand 的「推理经本机网关」（§11）。两条链路
都**没有端到端验收过**（§11 的状态行写着「真机未验」，透传口自己的模块文档写着 CLI 完整一轮没跑通）。
它们却在产品面上长出一堆东西：网关设置里 CLI / IDE / Sand 三选一的 `client_type`（`sand` 那一档实际是
「换成 Grok Bot 凭证、旁路整个号池」，和 Sand 补丁同名却不是一回事）、第二个端口、Sand 页的改道开关、
「IDE 面板拦截」卡、网关页的 Grok Bot 开关、远程主机的「经本机网关」路和 `GatewayGuard` 横幅。
用户面对的是三个叫「Sand」的东西（一条补丁、一个请求头、一种凭证），谁影响谁没人说得清。

决定：**网关只干一件事——拿号池的号，对本机客户端讲标准方言；面板补丁是另一件事，跟网关解耦。**
上面那一串全部拆掉。

### 13.2 今天的形态

| | 现在 |
|---|---|
| 推理端点 | 永远直连 api2。`InstallOptions::inference_endpoint` 字段保留，应用层恒传 `None`；它只剩两个用途：认出老机器盘上残留的改道并在 install / uninstall 时剥掉，以及 `examples/install_local` 这类研究用途 |
| Grok Bot 鉴权 | 只有 **Box Relay / 直连** 两档。`GrokBotAuthMode::Off` 在模型里保留（卸载 / 剥掉时的目标形态），界面不提供，`sand_install` 命令拒绝——`sand` 头配会话 JWT 上游一律 401，以前靠透传口换 token 才成立 |
| 远程出网 | `RemoteRoute::{Proxy, Direct}`，默认代理。老配置里的 `routeViaLocal: true` / `"gateway"` 读成 `Direct`。远程盘上若还有当年写的改道，卡片上红横幅点名，重新安装剥掉 |
| 界面 | 「Cursor 面板」一页三档：原生 / CRSR / Sand。盘上装着哪一档打「盘上」标签 |
| 账本 | 面板拦截写过的 `ide-agent` 行在网关启动 prune 时清掉 |

### 13.3 一个没解决的问题（如实）

远程 Sand 装的是同一份规则表，里面**没有** Grok 鉴权那一类（凭证文件在本机、Box relay 描述符也在本机）。
以前远程能用，靠的是「经本机网关」那条路上网关把 token 换成 grokBotToken。那条路没了之后，远程以
`sand` 身份、拿远程自己登着的会话 JWT 打 `InferenceService/Stream`，按今天的服务端行为会被 401。
也就是说**远程 Sand 目前没有一条验证过能出字的鉴权路径**。留着它是用户的决定（隧道、中继、探针
这套基础设施本身是好的）；要让它重新能用，得把 Grok 直连凭证推到远程、或在远程装 Box Relay 规则
并让 Box 可达——两件事都还没做。

