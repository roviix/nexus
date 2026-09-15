//! Grok Bot 客户端本体：装在哪、登没登录、拉起。

use nexus_core::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const GROK_BOT_APP: &str = "Grok Bot";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub installed: bool,
    /// `desktop-status.json` 里的 `signedIn`；文件不存在（从没启动过）为 `None`。
    pub signed_in: Option<bool>,
    pub app_version: Option<String>,
    /// 进程是否在跑（按 status 文件里的 pid 探一下）。
    pub running: bool,
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Grok Bot 的用户数据目录（Electron `userData`）。
pub fn user_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home_dir()
            .join("Library/Application Support")
            .join(GROK_BOT_APP)
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData/Roaming"))
            .join(GROK_BOT_APP)
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        home_dir().join(".config").join(GROK_BOT_APP)
    }
}

pub fn app_bundle_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        let candidates = [
            PathBuf::from("/Applications/Grok Bot.app"),
            home_dir().join("Applications/Grok Bot.app"),
        ];
        return candidates.into_iter().find(|p| p.is_dir());
    }
    #[allow(unreachable_code)]
    None
}

#[derive(Deserialize)]
struct DesktopStatusFile {
    pid: Option<u32>,
    #[serde(rename = "appVersion")]
    app_version: Option<String>,
    #[serde(rename = "signedIn")]
    signed_in: Option<bool>,
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

pub fn status() -> AppStatus {
    let installed = app_bundle_path().is_some();
    let raw = std::fs::read_to_string(user_data_dir().join("desktop-status.json")).ok();
    let parsed: Option<DesktopStatusFile> = raw.and_then(|r| serde_json::from_str(&r).ok());
    let running = parsed
        .as_ref()
        .and_then(|p| p.pid)
        .map(pid_alive)
        .unwrap_or(false);
    AppStatus {
        installed,
        signed_in: parsed.as_ref().and_then(|p| p.signed_in),
        app_version: parsed.and_then(|p| p.app_version),
        running,
    }
}

/// 拉起 Grok Bot（已在跑就只是前置窗口）。
pub fn launch() -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/usr/bin/open")
            .args(["-a", GROK_BOT_APP])
            .output()?;
        if out.status.success() {
            return Ok(());
        }
        return Err(AppError::new(
            nexus_core::ErrorCode::CursorNotFound,
            "没找到 Grok Bot 应用。",
        )
        .with_hint("到 x.ai/bot 下载并登录一个有额度的账号。"));
    }
    #[allow(unreachable_code)]
    Err(AppError::unsupported_platform("拉起 Grok Bot"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_never_panics_without_the_app() {
        let s = status();
        // 只要求形状合法；有没有装看机器。
        let _ = s.installed;
    }
}
