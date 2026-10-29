//! Gemini 网页协议通道。和反重力（Cloud Code / OAuth）分开。
//!
//! 账号是浏览器 Cookie。出站打 `gemini.google.com` 的 `StreamGenerate`，
//! 模型由 `x-goog-ext-525001261-jspb` 决定，不走 Google API Key。

pub mod protocol;
pub mod service;
pub mod session;

pub use protocol::{catalog_ids, owns_model, resolve_model, ModelSpec, MODELS, ROUTE_PREFIXES};
pub use service::{GeminiWebAccount, GeminiWebService, GeminiWebStatus};
