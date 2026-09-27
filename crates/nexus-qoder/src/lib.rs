//! `nexus-qoder` —— Qoder 账号，以及它的聊天协议。
//!
//! 本地网关的下一种号源。凭证只在这台机器上，请求从家庭宽带直连 Qoder 的网关。
//! 和 Cursor / ChatGPT / Grok / Kiro / ZCode 分表。
//!
//! 聊天**不走 CLI**。官方客户端（`qodercli` / IDE）自己打的是一条 HTTP SSE：
//!
//! 1. PAT 到 `openapi` 换一把短命的 job token（`POST /api/v1/jobToken/exchange`）；
//! 2. 请求体按官方的 `Encode=1` 编码；
//! 3. 带 COSY 签名头，`POST {gateway}/algo/api/v2/service/pro/sse/agent_chat_generation`；
//! 4. 响应是 OpenAI 形状的增量，包在 SSE 信封里。
//!
//! 国际版网关是 `api3.qoder.sh`，国内版是 `gateway.qoder.com.cn`。账号上的
//! `backend` 决定打哪边。模型的公开名字一样时，两边的上游 key 可能不同。
//!
//! Cockpit Tools 里的 Qoder 模块只做到设备登录和额度（`openapi.qoder.sh` 的
//! userinfo / plan / quota），没有这条聊天出站。协议形状对齐官方 CLI 现在用的
//! COSY 网关，不经过 `qodercli` 子进程。

pub mod auth;
pub mod chat;
pub mod cosy;
pub mod model;
pub mod protocol;
pub mod repo;
pub mod service;

pub use model::{QoderAccount, QoderBackend, QoderIdentity, QoderStatus};
pub use protocol::{catalog, resolve_model, ResolvedModel, ROUTE_PREFIXES};
pub use repo::{QoderAccounts, Upserted};
pub use service::{ImportReport, QoderService};
