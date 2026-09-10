//! `nexus-core` —— 领域类型、错误、id、时间。**不做任何 IO。**
//!
//! 分层最底下的一层（见 ARCHITECTURE §3.1）。上面所有 crate 都依赖它，它谁也不依赖，
//! 所以这里出现 `std::fs` / `reqwest` / `rusqlite` 都是设计被破坏的信号。

pub mod clock;
pub mod email;
pub mod error;
pub mod ids;
pub mod machine;
pub mod secret;

pub use clock::{file_stamp, iso_from_system_time, now_iso, Clock, SystemClock};
pub use email::Email;
pub use error::{AppError, ErrorCode, Result};
pub use ids::{AccountId, BackupId, ChatGptAccountId, GrokAccountId, KiroAccountId, ProfileId};
pub use machine::{MachineProfile, TELEMETRY_KEYS};
pub use secret::Secret;
