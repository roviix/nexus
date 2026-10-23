# 通道真机测试

单元测试只能证明「发出去的字节是我们以为的样子」。这份清单证明另一半：真账号、真客户端、真上游，请求通不通，缓存接不接得住。

2026-10-17 跑过 ChatGPT 通道的第一刀。别的通道用同一张记分卡，还没跑。

## 要回答的两件事

1. 这个通道对外的协议，客户端接上去能不能完成一轮真实使用。
2. 同一会话往后的轮次，缓存读占输入的比例。这个比例用来看协议有没有把前缀弄丢。

不设及格百分比。第一次跑只建基线。以后同一条用例的比例明显掉下去，才算退步。OpenAI 公开文档写提示词缓存从大约 1024 token 起才可能命中；短于这个长度的请求、以及每张图都换新会话键的出图，不进缓存率。

## 什么叫通过

- 客户端拿到正文或一次完整的工具往返，HTTP 成功。
- 网关账本有对应的一行：通道、模型、方言、输入、缓存读，和客户端自己报的用量一致。
- 同一会话、同一账号、同一模型，第 2 轮起缓存读大于 0。第 1 轮本来就是 0，不进分子分母。
- 中途换了号的样本作废，重测。换号会换缓存键，命中归零是预期。

## 怎么测

不改用户日常的 `~/.codex`、`~/.claude`、`~/.config/opencode`。每个客户端用单独的配置目录，指到本机网关。

三条入口都要打到：

| 客户端讲的话 | 网关路径 | ChatGPT 通道出去的样子 |
|---|---|---|
| Responses | `/v1/responses` | 原样透传，只改上游会拒的字段和身份 |
| OpenAI Chat | `/v1/chat/completions` | 桥接成 Responses |
| Anthropic | `/v1/messages` | 桥接成 Responses |

ChatGPT 订阅号的上游只有 `chatgpt.com/backend-api/codex/responses`。`chatgpt/` 和 `codex/` 都是选这条通道的名字，不是两条上游。

每个客户端两轮就够看出缓存键稳不稳：第一轮一句固定回复，同一会话再问一句。桥接路径再加第三轮，因为开场只有一条 user 时，第二轮多出来的追问曾经会把键改掉。

对照请求不用真客户端：固定一段超过 1024 token 的 system，三轮只在末尾加一句。这样把「客户端每轮改写了前缀」和「网关自己把键改了」分开。

测完对账本 `gateway_requests`：`cache_read / input_tokens`。这条通道没有缓存写，那一列是 0 是预期。

## 记分卡

通道 × 协议 × 客户端 × 用例。用例至少这些：

- 短回复能回来
- 同一会话第二轮、第三轮的缓存率
- 工具声明、调用、把结果送回去、拿到最终正文
- 流式和非流式
- 连续几次请求落在同一个号上
- 出图只看能不能回来，不进缓存率

客户端按接入页那几个：Claude Code、Codex CLI、OpenCode、Grok CLI、Cline、SDK。某台机器没装的记「未测」，不算通过。

## 2026-10-17 · ChatGPT · `gpt-5.6-luna` · 一个 Plus 号

| 客户端 | 结果 | 缓存 |
|---|---|---|
| Codex CLI，Responses 透传 | 两轮都回到指定的词 | 第 2 轮 7296/7601 = 96.0% |
| Claude Code，Anthropic 桥接 | 三轮都回到指定的词。客户端日志有 `unrecognized_model`，请求仍发出去了 | 第 2 轮 0/15177；第 3 轮 14848/15222 = 97.5% |
| 桥接对照（固定前缀，无客户端） | 三轮都回到指定的词 | 第 2 轮 0/1561；第 3 轮 1408/1584 = 88.9% |
| OpenCode 2.0.30，实际走了 Responses | 两轮都回到指定的词。带斜杠的模型名它的目录不收。每次运行先打一笔 `gpt-6-luna`，上游拒绝 | 第 1 轮 2048/6000 = 34.1%；第 2 轮 5888/6032 = 97.6% |

`gpt-6-luna` 的拒绝原文：`The 'gpt-6-luna' model is not supported when using Codex with a ChatGPT account.` `gpt-5.6-luna` 这次是认的。

第 2 轮桥接缓存为 0 的原因已经改掉：Claude Code 从第一轮就带 `metadata.user_id` 里的 `session_id`，缓存键用它；没有会话号时，只哈希第一条回复之前的开场，追问不再改键。正在进行的旧会话会冷启动一次。不同会话不能只靠第一条 user，那段经常是所有会话共用的 caveat。

## 2026-10-17 续 · 同一个 Plus 号目录里其余的能力

这个号从上游拉到的对话目录只有四条：`gpt-6-astra`、`gpt-5.6-sol`、`gpt-5.6-terra`、`gpt-5.6-luna`。没有 `gpt-6-luna`。

| 用例 | 结果 |
|---|---|
| `gpt-6-astra` 短回复 | PONG，200 |
| `gpt-5.6-sol` 短回复 | PONG，200 |
| `gpt-5.6-terra` 短回复 | PONG，200 |
| `gpt-5.6-luna` 工具往返 | 先调用 `echo`，参数 `{"text":"PONG"}`；把结果送回后回复 PONG |
| `gpt-image-2` 出图 | 400。网关用 `gpt-5.4-mini` 去代调出图工具，上游拒绝：`The 'gpt-5.4-mini' model is not supported when using Codex with a ChatGPT account.` |

`gpt-6-luna` 单独打过，同样是 400：`The 'gpt-6-luna' model is not supported when using Codex with a ChatGPT account.` 网关的静态目录里仍有这个名字，所以客户端点了它，请求会发出去，再被上游拒。这个号的实时目录不会列出它。

换号仍然没测：库里只有这一个可用的 ChatGPT 号。思考凭据回放、流式中断、ChatGPT 以外的通道也还没测。

## 重跑时别踩的坑

- 系统代理如果把 `127.0.0.1` 转走，Codex 会拿到 502，账本里没有这一行。那不是网关拒的。
- 测完不要从带测试环境变量的 shell 里 `open -a Nexus`。进程会继承 `ANTHROPIC_*`、`CLAUDE_CONFIG_DIR`、`CODEX_HOME`，接入页会把它们列出来，客户端也会不读刚写的配置。
- 从 Dock 重新打开一次，这些进程环境变量就没了。用户自己的 shell 启动文件里如果有同名变量，会继续显示，来源是 `~/.zshrc:行号` 而不是「环境变量」。
