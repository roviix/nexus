//! `nexus-grokbot` —— Grok Bot 桥。
//!
//! **它不是账号系统**。Cursor 账号仍由 `nexus-accounts` 管；这里只按需从本机装着的 Grok Bot
//! 客户端读取几样东西，供 Sand 补丁 / 本机网关用 Grok Bot 的额度跑 Cursor 模型：
//!
//! - `app`：装没装、登没登录、拉起它。
//! - `secrets`：`sand-secrets.json`（machineId、活跃账号的 email / session）—— Electron
//!   safeStorage 加密，macOS 上钥匙串取口令解 AES-128-CBC。
//! - `descriptor`：`gateway-descriptor.json` → Box gateway 地址 + 短期 token
//!   （Box Relay 模式直接用；直连模式借它到 pod 里读续期种子）。
//! - `pod`：exec daemon（Connect RPC）读 `SAND_INFERENCE_RENEWAL_CREDENTIAL`（`sbi_*`）。
//! - `credential`：`sbi_*` → `POST /sand-box/inference-credential` → `grokBotToken`（约 10 分钟，
//!   续期不要任何鉴权）；落成 `grokbot-stream-credential.json`，Sand 直连补丁与网关都读它。
//!
//! 全部纯 Rust：早先靠 `node -e` 解密，GUI 版应用拿不到 shell 的 PATH（nvm）就直接失败了。

pub mod app;
pub mod credential;
pub mod descriptor;
pub mod pod;
pub mod secrets;
pub mod service;

mod keychain;
mod sync;

pub use app::{AppStatus, GROK_BOT_APP};
pub use credential::{StreamCredential, STREAM_CREDENTIAL_FILENAME};
pub use descriptor::{BoxRelayDescriptor, BOX_RELAY_PATH};
pub use secrets::{ActiveAccount, GrokBotSecrets};
pub use service::{
    relay_config_path, CuaProbe, DirectCredentialInfo, ExportedAccount, GrokBotIdentity,
    GrokBotService, GrokBotStatus, RelayInfo, CUA_PROBE_MODEL,
};
pub use sync::run_sync;
