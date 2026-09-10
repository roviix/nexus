//! `nexus-chatgpt` —— ChatGPT 订阅账号：授权登录、导入、续期、额度。
//!
//! 这是本地网关的第二种号源（第一种是 Cursor，在 `nexus-accounts`）。它让 Codex CLI、Claude Code、
//! OpenAI SDK 这些客户端经本机网关用**用户自己的 ChatGPT 订阅**跑 Codex 模型——凭证只在这台机器上，
//! 请求从这台机器（家庭宽带）直接到 `chatgpt.com`，从上游看过去就是一个 Codex CLI 用户。
//!
//! 和 Cursor 账号是两个平台、两张表，互不认识（ARCHITECTURE §5.3）：持久化身份是
//! `(platform, external_id)`，这里的 external_id 是 `chatgpt_account_id`；用量是自己的形状
//! （5 小时 / 7 天两个滚动窗口）。
//!
//! 模块：
//! - [`oauth`]：协议本身——PKCE、授权地址、换 token、刷新、从 JWT 读身份；
//! - [`callback`]：本机 1455 端口的一次性回调服务器（Codex 的 client 把回调地址登记死在那）；
//! - [`protocol`]：账号侧与网关侧共用的 Codex 事实——身份头、额度形状；
//! - [`model`] / [`repo`]：账号与仓库（表里没有秘密，凭证在 `SecretStore`）；
//! - [`service`]：用例层，Tauri 命令与网关号源只跟它打交道。
//!
//! 推理路径本身（请求体怎么改、身份怎么按账号收敛、错误怎么分类）不在这里，在
//! `nexus-gateway` 的 `codex` 模块——那是网关的事。

pub mod callback;
pub mod model;
pub mod oauth;
pub mod protocol;
pub mod repo;
pub mod service;

pub use model::{ChatGptAccount, ChatGptStatus};
pub use oauth::{Identity, TokenSet, CALLBACK_PORT, REDIRECT_URI};
pub use protocol::{
    select_models, CodexUsage, Exhausted, ManifestModel, UsageWindow, CLIENT_VERSION,
    DEFAULT_BACKEND_URL, ORIGINATOR,
};
pub use repo::{ChatGptAccounts, Upserted};
pub use service::{parse_import_text, ChatGptService, LoginHandle, LoginState, SETTING_MODELS};
