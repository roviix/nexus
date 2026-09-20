//! Tauri 命令 —— IPC 边界。
//!
//! 约定（ARCHITECTURE §3.3）：
//!   - 命名 `<模块>_<动作>`；
//!   - **全部返回 `Result<T, AppError>`**，前端拿到的错误永远是同一个形状
//!     （`code` 分支、`message` 显示、`hint` 告诉用户下一步）；
//!   - 长任务用事件推进度，不让前端轮询；
//!   - **秘密永不经过 IPC**，除用户显式点「显示明文」的那一次（并记活动日志）。

pub mod accounts;
pub mod app;
pub mod backup;
pub mod chatgpt;
pub mod connect;
pub mod crsr;
pub mod gateway;
pub mod grok;
pub mod grokbot;
pub mod kiro;
pub mod perms;
pub mod playground;
pub mod sand;
pub mod sand_remote;
pub mod switcher;
pub mod zcode;

/// 事件名。集中在这里，免得前后端各写各的字符串。
pub mod events {
    /// 切号进度。载荷是 `nexus_switcher::SwitchProgress`。
    pub const SWITCH_PROGRESS: &str = "switcher://progress";
    /// OAuth 轮询状态。载荷是 `nexus_accounts::OauthState`。
    pub const OAUTH_STATE: &str = "oauth://state";
    /// ChatGPT 授权（等本机 1455 回调）的状态。载荷是 `nexus_chatgpt::LoginState`。
    pub const CHATGPT_LOGIN: &str = "chatgpt://login";
    pub const GROK_LOGIN: &str = "grok://login";
    pub const KIRO_LOGIN: &str = "kiro://login";
    /// 批量刷用量的逐个结果。
    pub const ACCOUNT_REFRESHED: &str = "accounts://refreshed";
    /// 批量自动配置的逐个结果。载荷是 `nexus_accounts::ProvisionReport`。
    pub const ACCOUNT_PROVISIONED: &str = "accounts://provisioned";
    /// Sand 补丁安装 / 卸载 / 还原进度。载荷是 `nexus_sand::SandProgress`。
    pub const SAND_PROGRESS: &str = "sand://progress";
    /// CRSR 补丁安装 / 卸载 / 还原进度。载荷同上（`SandProgress` 的步骤枚举）。
    pub const CRSR_PROGRESS: &str = "crsr://progress";
    /// 远程 Sand 补丁安装 / 卸载进度。载荷同上（`nexus_sand::SandProgress`）。
    pub const SAND_REMOTE_PROGRESS: &str = "sand://remote-progress";
    /// 「试一下」的流式回字。载荷是 `commands::gateway::TryFrame`。
    pub const GATEWAY_TRY: &str = "gateway://try";
    /// 游乐场对话的流式回字。载荷同上（`TryFrame`），按 `id` 认是哪一次发送。
    pub const PLAYGROUND_CHAT: &str = "playground://chat";
}
