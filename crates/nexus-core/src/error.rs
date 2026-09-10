//! 全应用统一的错误形状。
//!
//! 每个 Tauri command 都返回 `Result<T, AppError>`，前端因此永远拿到同一个结构：
//! `code` 用来分支（机器可读、稳定、不随文案变），`message` 直接显示给人看，
//! `hint` 是「下一步该做什么」。
//!
//! 为什么不省掉 `hint`：只有一句话的错误反复坑人——用户看到「刷新失败」却不知道
//! 该重新授权还是等一会儿重试。既然错误发生的那一刻我们最清楚该怎么办，就把它写进
//! 结构里，别让界面去猜。

use serde::{Deserialize, Serialize};
use std::fmt;

/// 机器可读的错误分类。前端按它分支，所以**只增不改**——改一个既有变体的名字
/// 等于悄悄改了前端的判断条件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 输入不合法（邮箱格式、空字段…）。
    InvalidInput,
    /// 找不到 Cursor 安装或它的状态库。
    CursorNotFound,
    /// 状态库里缺预期的键 —— Cursor 可能改了 schema，见 §5.2 的自检。
    CursorSchemaDrift,
    /// 需要 Cursor 退出才能继续，但它还在跑。
    CursorRunning,
    /// 退出 / 启动 Cursor 失败。
    CursorControl,
    /// 当前平台不支持这个动作（进程控制目前只实现了 macOS / Windows）。
    UnsupportedPlatform,
    /// 切号本里没有这个档。
    ProfileNotFound,
    /// 档存在但凭证不全，切不进去。
    ProfileIncomplete,
    /// 备份不存在。
    BackupNotFound,
    /// 账号不存在。
    AccountNotFound,
    /// 账号已存在（唯一键冲突）。
    AccountExists,
    /// 秘密存储里没有这条秘密。
    SecretMissing,
    /// 本地数据库错误。
    Database,
    /// 本地文件 IO 错误。
    Io,
    /// 网络不通 / 超时。
    Network,
    /// 上游返回了非 2xx，或响应形状不对。
    Upstream,
    /// 凭证失效，需要重新授权。
    Unauthorized,
    /// 凭证有效，但这个号没有这项权益（订阅档位不够、没开通）。重新授权没用。
    Forbidden,
    /// OAuth 轮询超时。
    OauthTimeout,
    /// 用户取消了正在进行的流程。
    Cancelled,
    /// 同类操作已经在跑了。切号这类会动 Cursor 文件的动作一次只能有一个。
    Busy,
    /// 这个平台还没有登录过的账号，动作没有可用的身份。
    NotLoggedIn,
    /// Sand 补丁：本机 Cursor 版本不是适配版本，或没有可识别的目标文件。前端据此显示
    /// 「等待适配」而不是「失败」。
    SandUnsupportedVersion,
    /// Sand 补丁：锚点命中数与预期不符（bundle 被改过、或版本细节有差），install 中止，
    /// 文件未被改动。
    SandAnchorMismatch,
    /// Sand 补丁：检测到其它同类工具的 marker，拒绝接管。
    SandForeignMarkers,
    /// Sand 补丁：写入后校验失败（checksum / 内嵌 hash / marker 数），已自动回滚。
    SandIntegrity,
    /// Sand 补丁：最坏情况——回滚也没能全部完成。message 里带备份目录，用户须手动处理。
    SandRollbackIncomplete,
    /// 兜底：不该发生的内部错误。
    Internal,
}

impl ErrorCode {
    /// 这个错误重试有没有意义。前端用它决定要不要给「重试」按钮。
    pub fn retryable(self) -> bool {
        matches!(
            self,
            ErrorCode::Network | ErrorCode::Upstream | ErrorCode::CursorRunning | ErrorCode::Busy
        )
    }
}

/// 跨 IPC 的错误。`serde` 出来就是前端 `catch` 到的对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Network, message).with_hint("检查网络后重试。")
    }

    pub fn upstream(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Upstream, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Unauthorized, message).with_hint("重新授权这个账号即可恢复。")
    }

    pub fn unsupported_platform(action: &str) -> Self {
        Self::new(
            ErrorCode::UnsupportedPlatform,
            format!("当前系统还不支持{action}。"),
        )
        .with_hint("进程控制只实现了 macOS、Windows 和 Linux。")
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)?;
        if let Some(hint) = &self.hint {
            write!(f, "（{hint}）")?;
        }
        Ok(())
    }
}

impl std::error::Error for AppError {}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::new(ErrorCode::Io, err.to_string())
    }
}

impl From<serde_json::Error> for AppError {
    fn from(err: serde_json::Error) -> Self {
        AppError::new(ErrorCode::Internal, format!("JSON 解析失败：{err}"))
    }
}

pub type Result<T> = std::result::Result<T, AppError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_the_shape_the_frontend_expects() {
        let err =
            AppError::new(ErrorCode::CursorRunning, "Cursor 还在运行。").with_hint("先退出。");
        let v: serde_json::Value = serde_json::to_value(&err).unwrap();
        assert_eq!(v["code"], "cursor_running");
        assert_eq!(v["message"], "Cursor 还在运行。");
        assert_eq!(v["hint"], "先退出。");
    }

    #[test]
    fn omits_hint_when_absent() {
        let err = AppError::invalid("邮箱不对");
        let v: serde_json::Value = serde_json::to_value(&err).unwrap();
        assert!(v.get("hint").is_none());
    }
}
