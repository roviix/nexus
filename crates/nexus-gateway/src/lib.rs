//! `nexus-gateway` —— 本机 `127.0.0.1` 上的推理网关。
//!
//! 对外是 OpenAI / Anthropic / Responses 兼容口，对上游讲 Cursor 的
//! `aiserver.v1.InferenceService/Stream`；另开一条透传口把 `cursor-agent` 的原生 Connect
//! 流量（`aiserver.*` → api2、`agent.v1.*` → api5）换身份头后原样转发。号从用户自己的账号
//! （`nexus-accounts`）或本机 Cursor 登录态来。网关本身**不改 Cursor 一个字节**。
//!
//! 客户端入口：
//! - 标准 SDK / Claude Code / Codex（已通，真机 curl）
//! - `cursor-agent -e … --agent-endpoint …`（透传口已通，h2c BiDi；CLI 本身还需 `agent login`）
//! - Cursor IDE local mode：**正式否决**——零售版 `buildFlags.localMode=false`，硬开会关 Tab
//!   （`isAllowedCpp` 恒 false），代价太大
//!
//! 这是对 ARCHITECTURE §1.2「不做本地网关」的有意反转；边界仍按 §4.6：只向下依赖
//! `nexus-accounts` / `nexus-cursor` / `nexus-core`，不改任何现有 crate。它是 desktop 里第一个
//! 要跟着 Cursor 协议变化的模块，所以默认关闭、可整体拆卸——坏了只影响网关，切号与账号照常。
//!
//! 核心策略是**额度接力**而不是负载均衡：一个用户一台机器，任何时刻一个号就够；一直用当前号，
//! 额度到线才接力下一个。这样会话粘性天然成立、换号频率极低、不必解析 protobuf 做 sticky。
//!
//! 结构（依赖只向下）：
//!
//! ```text
//! service     编排：起停 / 设置 / 口令 / 状态（Tauri 只认它；方言口 + 透传口各自独立起停）
//! server      127.0.0.1 HTTP：/v1/chat/completions /v1/messages /v1/responses /v1/models
//!             /v1/images/generations（+ 别名）
//! passthrough 127.0.0.1 h2c/h1：/aiserver.* → api2、/agent.v1.* → api5、/auth/* → api2（只换身份头）
//!   ├─ grokbot    透传口的 Grok Bot 额度开关：仅 InferenceService/Stream 换成 grokBotToken（nexus-grokbot 维护）
//!   ├─ inbound    OpenAI Chat / Responses / Anthropic Messages ⇄ 统一表示（parse + SSE serialize）
//!   ├─ models     客户端模型名 → Cursor 模型名（不映射的话 Claude Code 直连必 400）+ force_model
//!   ├─ lane       号从哪来：RelayLane 额度接力 + 三个 Source（Cursor 登录号 / nexus-accounts 托管号 /
//!   │             nexus-chatgpt 的 ChatGPT 订阅号——后者走自己的一条 lane）
//!   ├─ ledger     请求账本：每次方言口请求记一行（账号 / 模型 / token / 耗时），概览的「本地用量」
//!   ├─ upstream   推理后端 trait + CursorUpstream（server 用假后端可不联网全测）
//!   ├─ codex      第二个后端：ChatGPT 订阅号直连 chatgpt.com/backend-api/codex/responses
//!   │             （Responses 入站原样透传，其余方言桥接；协议纯函数在 codex::protocol）
//!   ├─ inference  InferenceService/Stream 客户端：请求构造 + Collector 流驱动 + 错误映射
//!   ├─ images     AiService/RunGenerateImage 客户端：一次一张的生图 + OpenAI images 形状
//!   ├─ normalized 统一中间表示（按 Anthropic Messages 建模）
//!   ├─ error      上游错误分类（每一类对应一种处置）
//!   ├─ connect    ConnectRPC 信封分帧 + HTTP/2 服务端流
//!   ├─ proto      aiserver.v1.Inference* 类型（scripts/gen-proto.py 从 bundle 机器生成）
//!   ├─ headers    aiserver IDE 请求头集合
//!   └─ identity   账号锚点 + 设备身份 + checksum（纯函数）
//! ```
//!
//! 以上全部已落地并对真上游验证（`examples/probe.rs` / `serve.rs` / `serve_passthrough.rs`）。
//! Tauri 侧在 `apps/desktop/src-tauri/src/commands/gateway.rs` 与 `pages/GatewayPage.tsx`。
//!
//! 协议知识移植自 `gateway/src/cursor/protocol.js`——那份 JS 是线上验证过的真值：纯函数的
//! 测试向量由它直接跑出来（`/tmp/vec.mjs`），消息映射拿它的编码器字节做跨实现对拍
//! （`/tmp/vec-proto.mjs`），不凭记忆、不凭"理解"重写。有意偏离它的地方只有两处，都在代码
//! 注释里说明了理由：上游 OUTPUT_TOKEN_LIMIT 在已有输出时算 `finish=length` 而不是报错；
//! Anthropic user 消息里的 tool_result 在解析层拆成独立 tool 消息（JS 那边会把它们丢掉）。

pub mod channel;
pub mod codex;
pub mod connect;
pub mod error;
pub mod grok;
pub mod grokbot;
pub mod headers;
pub mod identity;
pub mod images;
pub mod inbound;
pub mod inference;
pub mod intercept;
pub mod kiro;
pub mod lane;
pub mod ledger;
pub mod media;
pub mod models;
pub mod normalized;
pub mod passthrough;
pub mod playground;
pub mod proto;
#[cfg(test)]
mod proto_tests;
pub mod server;
pub mod service;
pub mod subscriptions;
pub mod upstream;
pub mod wire;

pub use channel::{Capability, Channel, ChannelGate, ChannelId, ChannelRegistry, OpenGate};
pub use codex::{CodexConfig, CodexUpstream};
pub use error::{UpstreamError, UpstreamKind};
pub use grok::GrokUpstream;
pub use grokbot::{GrokBotStreamAuth, GrokBotStreamSnapshot};
pub use headers::{ai_headers, HeaderList, RequestNonce};
pub use identity::DeviceIdentity;
pub use images::{GeneratedImage, ImageRequest};
pub use inference::{stream, StreamConfig};
pub use intercept::{
    InterceptHub, InterceptRecord, InterceptSnapshot, MarkerPosition, RewriteRule,
};
pub use kiro::KiroUpstream;
pub use lane::{
    Credential, CursorLoginSource, Lane, LaneSnapshot, Outcome, RelayLane, Roster, StaticLane,
    StoredAccountsSource, SubscriptionAccounts, SubscriptionSource,
};
pub use ledger::{Ledger, RequestRecord, UsageSummary};
pub use media::{MediaJob, MediaJobs, VideoJob, VideoRequest, VideoStatus};
pub use normalized::{
    ChatRequest, Completion, Delta, FinishReason, ImageInput, Message, Role, Sampling, ToolCall,
    ToolChoice, ToolDef, ToolResult, Usage,
};
pub use server::Gateway;
pub use service::{ChannelSnapshot, GatewayService, GatewaySettings, GatewayStatus, SettingsPatch};
pub use upstream::{CursorUpstream, Upstream};
