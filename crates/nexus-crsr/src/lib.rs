//! `nexus-crsr` —— 给本机 Cursor 打独立的 CRSR 鉴权补丁。
//!
//! **和 Sand 并列、互不依赖语义**：Sand 把 Agent 面板改去 bot 额度通道
//! （`client-type: sand` + `InferenceService/Stream`）；这条只换
//! `applyAuthorization` 的 Bearer，让原生 `agent.v1.AgentService/Run` 用账号里的
//! `crsr_` User API Key 兑出来的 `api_key_token`。不改 client-type、不改路由。
//!
//! 两条补丁占同一挂点，安装器互相拒绝同时装。备份目录分开（`crsr/backups` vs `sand/backups`）。
//!
//! 结构：
//!
//! ```text
//! service     编排：预检 → 备份 → 退出 Cursor → 写 → 校验 → 启动
//!   ├─ inject     写入 bundle 的 JS 块（concat!，禁止 format!("{}")）
//!   ├─ credential crsr_ 兑票、落盘（0o600）；补丁自己会再兑
//!   └─ 复用 nexus-sand 的 layout / backup / commit / integrity
//! ```

pub mod credential;
pub mod inject;
pub mod service;

pub use credential::{
    CrsrCredential, CrsrCredentialInfo, CREDENTIAL_FILENAME, CREDENTIAL_FILE_ENV, RENEW_LEEWAY_MS,
};
pub use inject::AUTH_MARKER;
pub use service::{CrsrOutcome, CrsrService, CrsrStatus};

pub use nexus_sand::{
    Operation, SandBackup as CrsrBackup, SandProgress as CrsrProgress, SandStep as CrsrStep,
    SUPPORTED_CURSOR_VERSION,
};
