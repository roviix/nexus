//! `nexus-grok` —— Grok Build（xAI CLI 订阅）账号：device code 授权、导入、续期。
//!
//! 本地网关的第三种号源（Cursor、ChatGPT 之后）。凭证只在这台机器上，请求从家庭宽带直连
//! `cli-chat-proxy.grok.com`。和 ChatGPT 分表（ARCHITECTURE §5.3）：持久化身份是 JWT `sub`。
//!
//! **不是** `nexus-grokbot`。那个是 Cursor 里借 Grok Bot 额度的桥。

pub mod model;
pub mod oauth;
pub mod protocol;
pub mod quota;
pub mod repo;
pub mod service;

pub use model::{GrokAccount, GrokAuthKind, GrokStatus};
pub use oauth::{Identity, TokenSet};
pub use protocol::{
    api_key_chat_headers, chat_headers, is_grok_image_model, is_grok_media_model, is_grok_model,
    is_grok_video_model, media_headers, split_route_prefix, upstream_image_model,
    upstream_video_model, CLI_CHAT_PROXY, DEFAULT_IMAGE_EDIT_MODEL, DEFAULT_IMAGE_MODEL,
    DEFAULT_VIDEO_MODEL, GROK_IMAGE_MODELS, GROK_MODELS, GROK_VIDEO_MODELS, REFRESH_AHEAD, XAI_API,
};
pub use quota::{media_eligibility_from_tier, GrokQuota};
pub use repo::{GrokAccounts, Upserted};
pub use service::{
    looks_like_api_key, parse_import_text, parse_models_v2, GrokManifestModel, GrokService,
    LoginHandle, LoginState,
};
