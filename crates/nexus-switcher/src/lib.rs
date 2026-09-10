//! `nexus-switcher` —— 切号本与切号编排。
//!
//! 只做一件事：让用户在自己电脑的 Cursor 里，十秒内从一个号切到另一个号，且**不弄坏
//! 任何东西**。为此付出的代价（备份先行、退出后写、单事务、原始机器码保护）都在
//! `switch.rs` 里逐条写明。
//!
//! **本 crate 不依赖 `nexus-accounts`，永远不要加这条依赖**（ARCHITECTURE R1）：切号本与
//! 我的账号是两个独立模块，数据只在 UI 层通过 `Switcher::adopt` 显式拷贝一次。
//! 依赖关系是这条约束唯一靠得住的执行者——文档里的约定会被忘掉，编译错误不会。

pub mod backup;
pub mod book;
pub mod model;
pub mod switch;

pub use backup::{Backups, DEFAULT_KEEP};
pub use book::SwitchBook;
pub use model::{
    AuthBackup, BackupReason, Overview, SwitchOptions, SwitchOutcome, SwitchProfile, SwitchProgress,
};
pub use nexus_cursor::QuitOutcome;
pub use switch::{ignore_progress, ProgressSink, Switcher};
