//! `nexus-zcode` —— ZCode（智谱 GLM 编码套餐）账号：从本机官方客户端导入、取凭证。
//!
//! 本地网关的第五种号源。凭证只在这台机器上，请求从家庭宽带直连 `api.z.ai`。
//! 和 Cursor / ChatGPT / Grok / Kiro 分表（ARCHITECTURE §5.3）。
//!
//! 和别的平台不一样的两点：
//!
//! 1. **不做 OAuth，改成导入。** 官方客户端登录后把凭证写在 `~/.zcode/v2/credentials.json`，
//!    加解密方式在它自己的 bundle 里。用户在官方客户端登录一次，这里直接读——比复刻一遍
//!    「device-poll 轮询 + 三跳 biz API 换 key」既短又不会因为对方改流程而烂掉。
//! 2. **不做 token 刷新。** 编码套餐的 API key 是永久的；体验套餐的 JWT 连 `exp` 都没有。
//!    凭证失效只以上游 401 的形式出现，处理方式是重新导入，不是续期。
//!
//! 事实来源：官方 ZCode 桌面客户端 3.12.3（`dev.zcode.app`）。

pub mod import;
pub mod model;
pub mod protocol;
pub mod repo;
pub mod service;

pub use import::ImportedAccount;
pub use model::{ZcodeAccount, ZcodePlan, ZcodeProvider, ZcodeStatus};
pub use protocol::{
    catalog, chat_url, default_max_tokens, is_zcode_model, upstream_model, ModelSpec, MODELS,
    ROUTE_PREFIXES,
};
pub use repo::{Upserted, ZcodeAccounts};
pub use service::{ImportReport, ZcodeService};
