<!--
一个 PR 做一件事。功能与行为改动请先开 issue 谈拢。
-->

## 为什么

<!-- 这个改动解决什么问题。相关 issue 用 "Fixes #123" 关联。 -->

## 改了什么

<!-- 按模块说清动了哪几处，以及为什么是这个改法而不是别的。 -->

## 怎么验证的

<!--
写清楚验证到了哪一层，不要只写「测过了」：
- 真机：对真实上游跑通了，说明是哪个平台、什么版本
- 假后端：走 examples/ 里的假 upstream 或 ui-preview
- 只跑了单测
-->

## 协议知识的出处

<!--
只在这个 PR 断言了上游接口的行为（请求头、字段、错误码、bundle 锚点）时才需要填。
写清结论是从哪来的：某次真机抓包、某个版本的 bundle、官方开源客户端的哪个文件。
不要凭记忆写。没有涉及可以删掉这一节。
-->

## 自检

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace`
- [ ] `cd apps/desktop && npm run typecheck && npm test`
- [ ] `node scripts/test-package-release.mjs`
- [ ] 改了纯函数的行为，补了对应的测试向量
- [ ] **没有提交任何真实凭证**——JWT、refresh token、机器码、邮箱、内网主机名一律用虚构值
- [ ] 新增的日志不会打出 token 或完整邮箱
- [ ] 涉及平台分支（`#[cfg(windows)]` / `#[cfg(target_os = "macos")]`）的改动，我确认另一边也能编过
