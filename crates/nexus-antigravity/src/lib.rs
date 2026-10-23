//! `nexus-antigravity` —— Google 反重力 (Antigravity / AGY) 平台支持。
//!
//! 提供：
//! - Google OAuth 2.0 PKCE 授权与静默 Token 自动续期（内置单飞锁）；
//! - 确定性 Session ID 派生以保护上游 KV-Cache / Prompt Caching；
//! - Thinking / Reasoning 思维链流式提取与解析；
//! - Gemini 3 / Claude 3 & 4 跨模型家族统一转换与路由；
//! - 本地配置探测与批量导入；
//! - 满足生产级网关调度与故障自动接力的账号池模型。

pub mod callback;
pub mod model;
pub mod oauth;
pub mod protocol;
pub mod repo;
pub mod service;
pub mod signatures;
pub mod usage;

pub use model::{
    AntigravityAccount, AntigravityStatus, AntigravityUsage, ClientProbe, ImportReport, LoginStart,
    LoginState,
};
pub use oauth::{TokenSet, OAUTH_CLIENT_ID, OAUTH_SCOPES};
pub use protocol::{
    derive_session_id, owns_model, upstream_model, GenerateContentRequest, GenerateContentResponse,
    ThinkingTagParser, ANTIGRAVITY_MODELS, DEFAULT_ENDPOINT, ROUTE_PREFIXES,
};
pub use repo::AntigravityAccounts;
pub use service::{AntigravityService, REFRESH_AHEAD};
pub use signatures::SignatureLedger;
pub use usage::{is_auth_expired, is_quota_exhausted, model_cooldown_ms, quota_hint};
