//! Sand 补丁命令。
//!
//! install / uninstall / restore 都会关掉用户正在用的编辑器并改 Cursor 的文件，所以每一条都是
//! **用户点击**直接触发；这里没有定时器、没有后台任务、没有「顺手帮你装上」。

use crate::commands::events;
use crate::commands::switcher::run_blocking;
use crate::state::AppState;
use nexus_core::{AppError, Result};
use nexus_sand::{
    supported_cursor_release, CursorDownloadPlatform, CursorRelease, InstallOptions, SandBackup,
    SandOutcome, SandProgress, SandStatus,
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
/// 推理端点改道由界面上「推理经本机网关」开关决定（`options.inference_endpoint`）；
/// `SAND_INFERENCE_ENDPOINT` 环境变量仍留作排障口子，设了就覆盖界面选的值。
#[tauri::command]
pub async fn sand_install(
    app: AppHandle,
    state: State<'_, AppState>,
    options: Option<InstallOptions>,
) -> Result<SandOutcome> {
    let sand = state.sand.clone();
    let options = nexus_sand::with_inference_endpoint(
        options.unwrap_or_default(),
        std::env::var("SAND_INFERENCE_ENDPOINT").ok().as_deref(),
    )?;
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
