//! `nexus-accounts` —— 我的账号。
//!
//! 管理用户手上的 Cursor 账号：托管凭证、OAuth 取 token、刷用量、维护状态。
//!
//! 三条不变量：
//!   1. **凭证只在 `SecretStore`**。这个 crate 导出的所有结构都可以直接序列化给前端，
//!      因为里面根本没有秘密（只有 `has_refresh` 这类布尔量）。
//!   2. **托管门槛**：邮箱 + (refresh_token | Cursor 密码 | session token | crsr_ API Key)。
//!      只有 API Key 的号能查基础用量，不能切号。
//!   3. **不依赖 `nexus-switcher`**（ARCHITECTURE R1）。「加入切号本」是 UI 层的一次显式拷贝。
//!
//! `import` 与 `export` 是同一种清单文件的两个方向：导出去的能原样粘回来。

pub mod accounts_json;
pub mod billing;
pub mod convert;
pub mod export;
pub mod import;
pub mod model;
pub mod oauth;
pub mod provision;
pub mod repo;
pub mod service;
pub mod sessions;
pub mod token;
pub mod usage;

pub use billing::{AccountBilling, BillingDiscount, BillingInvoice, BillingItem, DiscountState};
pub use export::{Export, FORMAT as EXPORT_FORMAT};
pub use import::{parse_dump, preview as preview_import, ImportPreview, ImportRow, ParsedAccount};
pub use model::{Account, AccountPatch, NewAccount, Source, Status};
pub use oauth::{OauthHandle, OauthSession, OauthState, OauthTokens, DEFAULT_TIMEOUT};
pub use provision::{ProvisionPlan, ProvisionReport, ProvisionStep, StepReport, StepState};
pub use repo::Accounts;
pub use service::AccountsService;
pub use sessions::{ActiveSession, KickOutcome};
pub use token::{extract_user_id, refresh_to_session, MintedApiKeyInfo, RefreshedSession};
pub use usage::{AccountUsage, BotQuota, ModelUsage};
