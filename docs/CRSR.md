# CRSR 通道 · 设计与决策记录

> 状态：**锚点适配 Cursor 3.19.13**，两处 `applyAuthorization`（`cursor-agent-host` 与
> `cursor-always-local`）上 apply / remove 往返幂等，注入体过 `node --check`。版本不等一律拒装。
>
> 这条通道和 [Sand](./SAND.md) 一样，是对 `ARCHITECTURE.md §1.2`「不改 Cursor 的字节」的**有意反转**。
> 两条补丁占同一个挂点、互相拒绝同时安装。

---

## 1. 它做什么

把 Cursor IDE 原生 Agent 面板发出的请求，鉴权头换成你某个账号的 `crsr_` User API Key 兑出来的
短期票据。**只换 Bearer，别的什么都不动。**

一句话对比：

| | Sand 通道 | CRSR 通道 |
|---|---|---|
| 改什么 | `client-type: sand` + 路由到 `InferenceService/Stream` | 只改 `applyAuthorization` 的 Bearer |
| 走哪条服务 | Cursor 的 bot 额度通道 | 原生 `agent.v1.AgentService/Run` |
| 用什么身份 | Grok Bot 凭证 | 账号里的 `crsr_` User API Key |
| 改 URL / client-type | 改 | **不改** |
| 覆盖范围 | Agent 面板的推理 | `agent.v1.*` 与 `BackgroundComposerService` |

为什么值得单列一条：Sand 把面板整条链路搬去了另一个服务，能力集跟着那个服务走；CRSR 什么都
不搬，面板还是原来那个面板、还是原生协议，只是**换了一个人在付账**。两者解决的不是同一个问题，
所以没有做成一个开关的两档，而是两条并列的通道。

## 2. 为什么是 `applyAuthorization`

这是 Cursor 自己给所有出站 Connect 请求统一贴鉴权头的地方。挂在这里有三个好处：

1. **一处覆盖全部。** 不用逐个接口去认 URL，服务名在 `e.service.typeName` 上现成。
2. **不碰协议。** 请求体、header 的其余部分、URL 全是原样，上游看到的是一个正常客户端。
3. **和 Sand 共用已经验证过的锚点定位。** `nexus-sand` 的 `layout` / `backup` / `commit` /
   `integrity` 直接复用，这个 crate 只写自己的注入体和凭证。

注入块的作用域收得很窄——只拦 `agent.v1.` 前缀和 `aiserver.v1.BackgroundComposerService`，
其余服务（补全、索引、遥测）原路返回，不受影响。

## 3. 架构

```text
nexus-crsr
  service      编排：预检 → 备份 → 退出 Cursor → 写 → 校验 → 启动
    ├─ inject      写入 bundle 的 JS 块
    ├─ credential  crsr_ 兑票、落盘（0600）
    └─ 复用 nexus-sand 的 layout / backup / commit / integrity
```

依赖只有 `nexus-core` / `nexus-store` / `nexus-cursor` / `nexus-accounts` / `nexus-sand`，
和分层规则一致（ARCHITECTURE §3.1）。

### 3.1 IPC

`crsr_status` / `crsr_install` / `crsr_uninstall` / `crsr_backups` / `crsr_remove_backup` /
`crsr_restore_backup` / `crsr_mint_for_account` / `crsr_clear_credential`。
安装与卸载的进度走 `crsr://progress` 事件，载荷复用 `nexus_sand::SandProgress`。

### 3.2 数据

- 凭证文件 `crsr-agent-credential.json` 落在应用数据目录，权限 `0600`。
  里面是 `apiKey`（长期 `crsr_…`）、`accessToken`（兑出来的短期 JWT）、`expiresAtMs`。
- 备份落在 `crsr/backups`，**和 Sand 的 `sand/backups` 分开**——两条补丁的还原点混在一起，
  用户还原时就无从判断会回到哪个状态。保留最近 10 份。
- IPC 只回 `CrsrCredentialInfo`（邮箱、过期时间、能不能续），不含任何秘密。

## 4. 关键机制

### 4.1 顺序（硬约束，与 Sand、切号同构）

**预检 → 备份 → 退出 Cursor → 写入 → 校验 → 启动。** 预检不过就一个字节都不碰；写入失败
`commit_plan` 回滚。同一时刻只允许一个操作在跑（`ErrorCode::Busy`）。

### 4.2 版本护栏

锚点是逐字节匹配的字符串，含 `applyAuthorization` 的方法前缀、那一行变量声明、以及
`if(t.overrideAuthToken){`。3.19.13 上两个 bundle 的变量声明顺序不同（`var n,r,o,s,…` 与
`var n,r,s,o,…`），两条都认。版本不是适配版本时报「等待适配」而不是「失败」——
数字不对的后果是**拒装**，不是装坏。

### 4.3 与 Sand 互斥

装之前先扫盘上的 Sand marker：Grok 鉴权那三个（`SAND_GROK_BOX_RELAY_AUTH_V1` 等）占的是同一个
挂点；`SAND_CLIENT_MODE_V1` 这类则意味着面板根本不走原生 `Run`，装了 CRSR 也不会被执行。
两种情况都拒装并说清原因。反向同理。

### 4.4 续期在补丁里，不在 Nexus 进程里

`crsr_` 兑出来的 JWT 大约一小时过期。如果靠 Nexus 定时去续，用户关掉 Nexus 之后 IDE 就会在某个
时刻突然 401——而那时他正在写代码，不会想到是另一个没开着的应用的问题。

所以注入块自己会兑：发现 `accessToken` 缺失或距过期不足 2 分钟，就地
`yield fetch("https://api2.cursor.sh/auth/exchange_user_api_key")` 换一张，解 JWT 的 `exp` 写回
文件。**IDE 在跑，这条链路就自洽**，不依赖 Nexus 是否运行。

Nexus 侧的 `crsr_mint_for_account` 只负责「第一次把哪个号的 key 写进文件」。

### 4.5 注入体的写法约束

注入块里满是 `{}`（JS 的对象与代码块），**绝不能经过 `format!` 的位置参数**，所以整块用
`concat!` 拼死。测试里有一条 `node --check`，把注入块包进一个 generator 函数里做语法校验——
往 bundle 里写进一段语法错误的 JS，后果是 Cursor 直接起不来。

## 5. 界面

侧栏「补丁」组只有一页「Cursor 面板」，CRSR 是它三档（原生 / CRSR / Sand）里的一档。这一档回答四件事：版本认不认、装没装（几处命中 / 几个锚点）、
当前用的是哪个号的 key、有几个还原点。

挑号在账号抽屉里：凭证区有 `crsr_` API Key 的号会多出一行「Agent 面板用这个号」。
这是因为「哪个号」本质上是账号的属性，不是补丁的属性——补丁只认那个文件。

## 6. 已知代价（如实）

- **Cursor 升级后补丁失效**，需要重装；新版本的锚点要重新适配。这是补丁类方案的固有代价，
  和 Sand 一样。
- **需要 `crsr_` User API Key。** 没有这个 key 的号用不了这条通道。
- **API Key 走的是 Cursor 的 API 计费口径**，和订阅额度不是同一本账。用之前先确认你清楚
  自己在花什么。
- **注入块里硬编码了 `api2.cursor.sh` 的兑票端点。** 上游改这个接口，补丁就要跟着改。
- 和 Sand 只能二选一。

## 7. 排障

| 现象 | 多半是 |
|---|---|
| IDE 报 `[CRSR_AUTH_NOT_SELECTED]` | 还没挑号，凭证文件里没有 `apiKey` |
| IDE 报 `[CRSR_AUTH_TOKEN_MISSING]` | 兑票失败：key 失效、或到 `api2.cursor.sh` 的网络不通 |
| 状态卡显示「等待适配」 | 本机 Cursor 不是适配版本 |
| 装不上，说 Sand 占用 | 先卸载 Sand |
| 重装之后无事发生 | Cursor 没真正退干净，旧进程还在跑老 bundle（同 SAND.md §9.5） |

环境变量 `NEXUS_CRSR_CREDENTIAL_FILE` 可以把凭证文件指到别处，排障时用。**安装器和注入体读的是
同一个变量**（`credential::resolve_path` 与 `inject::auth_block`，有测试钉住）。但它们是两个进程：
要让它真正生效，变量得设在两边都看得见的地方（macOS `launchctl setenv`、Windows 用户级环境变量），
只在终端里 `export` 之后启动 Nexus 是不够的——那样 Nexus 写一处、Cursor 读另一处，界面会显示
「已选好号」而 IDE 一直报 `CRSR_AUTH_NOT_SELECTED`。
