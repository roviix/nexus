//! CRSR 补丁命令。
//!
//! 给本机 Cursor 的 `applyAuthorization` 换成账号里的 `crsr_` User API Key 兑出来的
//! `api_key_token`，让原生 Agent 面板走 ide/cli 额度。和 Sand 互斥。
//!
//! install / uninstall / restore 都会关掉用户正在用的编辑器并改 Cursor 的文件，所以每一条都是
//! **用户点击**直接触发。

use crate::commands::events;
use crate::commands::switcher::run_blocking;
use crate::state::AppState;
use nexus_core::{AccountId, Result};
use nexus_crsr::{CrsrBackup, CrsrCredentialInfo, CrsrOutcome, CrsrProgress, CrsrStatus};
use tauri::{AppHandle, Emitter, State};

/// 只读：版本 / 已装标记 / 凭证摘要 / 备份数。
#[tauri::command(async)]
pub fn crsr_status(state: State<'_, AppState>) -> Result<CrsrStatus> {
    state.crsr.status()
}

#[tauri::command]
pub async fn crsr_install(
    app: AppHandle,
    state: State<'_, AppState>,
    relaunch: Option<bool>,
) -> Result<CrsrOutcome> {
    let crsr = state.crsr.clone();
    let relaunch = relaunch.unwrap_or(true);
    run_blocking(move || {
        crsr.install(relaunch, &|p: CrsrProgress| {
            let _ = app.emit(events::CRSR_PROGRESS, p);
        })
    })
    .await
}

#[tauri::command]
pub async fn crsr_uninstall(
    app: AppHandle,
    state: State<'_, AppState>,
    relaunch: Option<bool>,
) -> Result<CrsrOutcome> {
    let crsr = state.crsr.clone();
    let relaunch = relaunch.unwrap_or(true);
    run_blocking(move || {
        crsr.uninstall(relaunch, &|p: CrsrProgress| {
            let _ = app.emit(events::CRSR_PROGRESS, p);
        })
    })
    .await
}

#[tauri::command(async)]
pub fn crsr_backups(state: State<'_, AppState>) -> Result<Vec<CrsrBackup>> {
    state.crsr.backups()
}

#[tauri::command(async)]
pub fn crsr_remove_backup(state: State<'_, AppState>, id: String) -> Result<()> {
    state.crsr.remove_backup(&id)
}

#[tauri::command]
pub async fn crsr_restore_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    relaunch: Option<bool>,
) -> Result<CrsrOutcome> {
    let crsr = state.crsr.clone();
    let relaunch = relaunch.unwrap_or(true);
    run_blocking(move || {
        crsr.restore_backup(&id, relaunch, &|p: CrsrProgress| {
            let _ = app.emit(events::CRSR_PROGRESS, p);
        })
    })
    .await
}

/// 用账号库里这个号的 `crsr_` 兑票写成凭证。不必重装补丁。
#[tauri::command]
pub async fn crsr_mint_for_account(
    state: State<'_, AppState>,
    id: AccountId,
) -> Result<CrsrCredentialInfo> {
    let info = state.crsr.mint_for_account(&id).await?;
    Ok(info)
}

#[tauri::command(async)]
pub fn crsr_clear_credential(state: State<'_, AppState>) -> Result<()> {
    state.crsr.clear_credential()
}

#[tauri::command(async)]
pub fn crsr_set_gateway_url(
    state: State<'_, AppState>,
    gateway_url: Option<String>,
) -> Result<Option<CrsrCredentialInfo>> {
    state.crsr.set_gateway_url(gateway_url)
}

/// 把扩展自定义模型写入 Cursor 的 `state.vscdb`，使其直接出现在 Cursor 界面模型选择器下拉中。
#[tauri::command(async)]
pub fn crsr_sync_custom_model(state: State<'_, AppState>, model: String) -> Result<bool> {
    let state_db = nexus_cursor::StateDb::new(&state.switcher.cursor().paths.state_db);
    state_db.add_custom_model(&model)
}

/// 读取当前 Cursor 库中已添加的扩展模型列表。
#[tauri::command(async)]
pub fn crsr_get_custom_models(state: State<'_, AppState>) -> Result<Vec<String>> {
    let state_db = nexus_cursor::StateDb::new(&state.switcher.cursor().paths.state_db);
    state_db.read_custom_models()
}


