//! `nexus-claude` —— Claude 订阅号：OAuth、setup-token、Console API Key。
//!
//! 本地网关的一条号源。凭证只在这台机器上，请求打到 `api.anthropic.com` 的
//! Messages。客户端本来就讲 Messages 时，请求体和 SSE 原样转发，缓存断点、
//! thinking 签名才留得住。
//!
//! OAuth / setup-token 出站按真实 Claude Code CLI 的指纹补齐（计费头、身份句、
//! beta、工具名、`metadata.user_id`）。否则上游把流量划进 extra usage。
//! 真实 Claude Code 客户端自己已经带齐的，只换认证和账号身份，不重写它的 system。

mod callback;
pub mod fingerprint;
mod http1;
pub mod model;
pub mod oauth;
mod prompt;
pub mod protocol;
pub mod repo;
pub mod service;
pub mod transport;

pub use model::{
    ClaudeAccount, ClaudeAuthMode, ClaudeStatus, ClientProbe, ImportReport, LoginStart, LoginState,
};
pub use protocol::{owns_model, upstream_model, MODELS, OAUTH_BETA, ROUTE_PREFIXES};
pub use service::ClaudeService;
