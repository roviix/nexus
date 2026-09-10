//! `nexus-kiro` —— Kiro（Amazon Q / AWS Builder ID）账号：device code 授权、导入、续期。
//!
//! 本地网关的第四种号源。凭证只在这台机器上，请求从家庭宽带直连 `q.us-east-1.amazonaws.com`。
//! 和 Cursor / ChatGPT / Grok 分表（ARCHITECTURE §5.3）。第一版只做 Builder ID device code +
//! 导入 `kiro-auth-token.json`；社交登录刷新有 client 对就走 OIDC，没有就走 Kiro 桌面端点。

pub mod model;
pub mod oauth;
pub mod protocol;
pub mod repo;
pub mod service;

pub use model::{KiroAccount, KiroStatus};
pub use oauth::{Identity, TokenSet};
pub use protocol::{is_kiro_model, split_route_prefix, upstream_model, KIRO_MODELS, REFRESH_AHEAD};
pub use repo::{KiroAccounts, Upserted};
pub use service::{parse_import_text, KiroService, LoginHandle, LoginState};
