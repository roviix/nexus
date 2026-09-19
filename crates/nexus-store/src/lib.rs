//! `nexus-store` —— 本地持久化：一个 SQLite 库，数据和凭证都在里面。
//!
//! 不碰 OS 钥匙串。曾经碰过，最后一条（用户自己的 Nexus 登录态）也收了回来 ——
//! 理由和代价写在 `secrets::SqliteSecrets` 上。
//!
//! - 业务表（accounts / switch_profiles / …）里一行秘密也没有，只有 ref。这条没变，
//!   有测试盯着 —— 列表、导出、日志都直接读这些表。
//! - 秘密本身在同一个库的 `secrets` 表里，明文。
//!
//! 于是库文件本身成了凭证文件，`Db::open` 把它和目录收到 0600 / 0700。
//!
//! 整库的本地备份（快照到 `~/.roviix/backups`、从快照还原）在 `backup`。

pub mod activity;
pub mod backup;
pub mod db;
pub mod keys;
pub mod secrets;
pub mod settings;

pub use backup::{BackupFile, Backups, RestoreOutcome};
pub use db::{sql_error, Db, SqlExt};
pub use keys::{AccountSecret, ChatGptSecret, GrokSecret, KiroSecret, SecretRef, ZcodeSecret};
pub use secrets::{MemorySecrets, SecretStore, SqliteSecrets};
