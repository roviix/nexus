//! Sand 补丁命令。
//!
//! install / uninstall / restore 都会关掉用户正在用的编辑器并改 Cursor 的文件，所以每一条都是
//! **用户点击**直接触发；这里没有定时器、没有后台任务、没有「顺手帮你装上」。

use crate::commands::events;
use crate::commands::switcher::run_blocking;
use crate::state::AppState;
use nexus_core::{AppError, Result};
use nexus_sand::{
    supported_cursor_release, CursorDownloadPlatform, CursorRelease, GrokBotAuthMode,
    InstallOptions, SandBackup, SandOutcome, SandProgress, SandStatus,
};
use tauri::{AppHandle, Emitter, State};

/// Sand 明确适配的 Cursor 发行包。只按编译目标选平台，不依赖 WebView 的 UA；
/// 不读本机安装，所以还没装 Cursor 或安装目录损坏时也能拿到下载链接。
#[tauri::command(async)]
pub fn sand_release() -> Result<CursorRelease> {
    let platform = match std::env::consts::OS {
        "macos" => CursorDownloadPlatform::Macos,
        "windows" => CursorDownloadPlatform::Windows,
        "linux" => CursorDownloadPlatform::Linux,
        _ => return Err(AppError::unsupported_platform("下载 Sand 适配的 Cursor")),
    };
    Ok(supported_cursor_release(platform))
}

/// 只读：版本 / 已装标记 / dry-run 预检 / 备份数。要读十一个 bundle（几十 MB），挪到线程池。
#[tauri::command(async)]
pub fn sand_status(state: State<'_, AppState>) -> Result<SandStatus> {
    state.sand.status()
}

/// 安装。整段跑在阻塞线程上（里面有等 Cursor 退出这种会睡十几秒的操作），
/// 进度经 `sand://progress` 推给界面。
///
/// 应用层不再提供「推理经本机网关」：`inference_endpoint` 恒为 `None`，盘上若还有早期版本写的
/// 改道，这次安装把它剥掉。Grok 鉴权也不能选「关」——`sand` 头配会话 JWT 上游一律 401，
/// 以前靠网关透传口换 token 才成立，那条口子已经没有了。
#[tauri::command]
pub async fn sand_install(
    app: AppHandle,
    state: State<'_, AppState>,
    options: Option<InstallOptions>,
) -> Result<SandOutcome> {
    let sand = state.sand.clone();
    let mut options = options.unwrap_or_default();
    options.inference_endpoint = None;
    if options.grokbot_auth == GrokBotAuthMode::Off {
        return Err(
            AppError::invalid("Sand 通道必须选一种 Grok Bot 鉴权方式。").with_hint(
                "选「Box Relay」或「直连」。不带 Grok Bot 凭证的 sand 请求会被上游拒绝（401）。",
            ),
        );
    }
    run_blocking(move || {
        sand.install(options, &|p: SandProgress| {
            let _ = app.emit(events::SAND_PROGRESS, p);
        })
    })
    .await
}

#[tauri::command]
pub async fn sand_uninstall(
    app: AppHandle,
    state: State<'_, AppState>,
    relaunch: Option<bool>,
) -> Result<SandOutcome> {
    let sand = state.sand.clone();
    let relaunch = relaunch.unwrap_or(true);
    run_blocking(move || {
        sand.uninstall(relaunch, &|p: SandProgress| {
            let _ = app.emit(events::SAND_PROGRESS, p);
        })
    })
    .await
}

#[tauri::command(async)]
pub fn sand_backups(state: State<'_, AppState>) -> Result<Vec<SandBackup>> {
    state.sand.backups()
}

#[tauri::command(async)]
pub fn sand_remove_backup(state: State<'_, AppState>, id: String) -> Result<()> {
    state.sand.remove_backup(&id)
}

/// 紧急刹车：按字节把某份备份写回，不认锚点。
#[tauri::command]
pub async fn sand_restore_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    relaunch: Option<bool>,
) -> Result<SandOutcome> {
    let sand = state.sand.clone();
    let relaunch = relaunch.unwrap_or(true);
    run_blocking(move || {
        sand.restore_backup(&id, relaunch, &|p: SandProgress| {
            let _ = app.emit(events::SAND_PROGRESS, p);
        })
    })
    .await
}
