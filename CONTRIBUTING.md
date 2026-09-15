# 参与贡献

感谢你的兴趣。先说几条能省大家时间的约定。

## 先开 issue 再动手

新功能、行为改动、协议适配（Cursor / ChatGPT / Grok / Kiro 的接口变了）先开一个 issue 讨论。
纯 bug 修复、文档、测试可以直接提 PR。

## 开发环境

见 [README「从源码构建」](./README.md#从源码构建)。合并前 CI 会跑的检查：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cd apps/desktop && npm run typecheck && npm test
node scripts/test-package-release.mjs
```

本机全过再提。CI 同时在 Linux 与 Windows 上跑 clippy 和测试——平台分支的代码
（`#[cfg(windows)]` / `#[cfg(target_os = "macos")]`）在另一边一行都编不到，请留意。

## 代码约定

- **依赖只向下。** `crates/` 之间的依赖图在 README 里画着；`nexus-switcher` 与 `nexus-accounts`
  互不依赖，这是产品约束不是巧合。
- **注释写「为什么」，不写「做了什么」。** 现有代码与文档以中文为主，接着用中文即可；英文也可以，
  别混在同一段。
- **协议知识要有出处。** 对上游接口的任何断言（请求头、字段、错误码）都要能在注释里指出是从哪个
  真实请求 / 哪份 bundle 里核实的，不要凭记忆写。

  你会在注释里看到形如 `gateway/src/cursor/protocol.js`、`shop/src/lib/cursorUsage.ts`、
  `docs/relay/CODEX-OAUTH.md` 的路径。**它们指向的是本项目的上游私有仓库，不在这里。**
  这些协议事实原本是在那边用真实流量核对出来的，移植过来时把出处一并带上，是为了让「这个
  magic header 哪来的」这类问题有答案，而不是留一个待办。你不需要能打开那些文件——注释里
  都写清了结论本身。新增的断言请引用你自己能公开指出的来源（真机抓包、官方开源客户端、
  版本号明确的 bundle）。
- **不改 Cursor 的字节**——除了 `nexus-sand` 与 `nexus-crsr` 这两条补丁通道。网关、切号、账号池
  对 Cursor 本体只读不写。新增锚点一律**精确字符串匹配、版本不等就拒装**，不做模糊匹配：
  拒装的代价是用不了，模糊匹配的代价是把用户的 Cursor 装坏。
- **测试跟着改。** 纯函数的行为改动要有对应的测试向量；Tauri 命令层的改动至少要过 `ui-preview`。

## 安全红线

- **永远不要提交真实凭证**，包括测试夹具。JWT、refresh token、机器码、邮箱地址、内网主机名
  一律用虚构值。CI 里有 gitleaks 扫全部历史；被扫到的提交会被要求重写。
- 新增日志时想一下会不会打出 token 或邮箱。邮箱用 `nexus_core::Email::masked()`，token 只打前后几位。

## 提交与 PR

- 一个 PR 做一件事。
- 提交信息第一行说清「为什么」，例如 `gateway: Anthropic tool_result 拆成独立消息，否则上游丢参数`。
- PR 描述里写清怎么验证的（真机 / 假后端 / 只跑了单测）。

## 许可

提交的代码默认按本仓库的 [MIT](./LICENSE) 许可发布。
