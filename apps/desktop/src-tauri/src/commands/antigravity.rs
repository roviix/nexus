//! Google 反重力 (Antigravity) 账号管理与 IPC 命令。
//!
//! 遵循 Nexus 架构：
//! - 秘密永不通过 IPC 暴露；
//! - 授权采用 Google OAuth 2.0 PKCE 流程（监听 51121 端口或支持手动粘贴回跳）；
//! - 支持本机 CLI 探测与导入；
//! - 支持单个账号的状态启停、备注设置与删除。

use crate::commands::events;
use crate::state::AppState;
use nexus_antigravity::{AntigravityAccount, ClientProbe, LoginStart, LoginState};
use nexus_core::{ErrorCode, Result};
use nexus_store::activity;
use tauri::{AppHandle, Emitter, State};

#[tauri::command(async)]
pub fn antigravity_list(state: State<'_, AppState>) -> Result<Vec<AntigravityAccount>> {
    state.antigravity.list()
}

#[tauri::command]
pub async fn antigravity_login_start(
    app: AppHandle,
    state: State<'_, AppState>,
    note: Option<String>,
) -> Result<LoginStart> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let started = state.antigravity.start_login(note).await?;
    activity::info(
        &state.db,
        "antigravity",
        None,
        "发起 Google Antigravity 授权",
    );
    if started.callback_listening {
        let antigravity = state.antigravity.clone();
        let db = state.db.clone();
        let emitter = app.clone();
        let session_id = started.login_id.clone();
        tauri::async_runtime::spawn(async move {
            let result = antigravity
                .wait_login(&session_id, &|st: LoginState| {
                    let _ = emitter.emit(events::ANTIGRAVITY_LOGIN, st);
                })
                .await;
            match result {
                Ok(account) => {
                    activity::info(
                        &db,
                        "antigravity",
                        Some(&account.label),
                        "已通过授权添加 Google Antigravity 账号",
                    );
                }
                Err(err) if err.code != ErrorCode::Cancelled => activity::warn(
                    &db,
                    "antigravity",
                    None,
                    format!("Google Antigravity 授权未完成：{}", err.message),
                ),
                Err(_) => {}
            }
        });
    }
    Ok(started)
}

#[tauri::command(async)]
pub fn antigravity_login_cancel(state: State<'_, AppState>) -> Result<()> {
    state.antigravity.cancel_login();
    Ok(())
}

#[tauri::command(async)]
pub async fn antigravity_import_text(
    state: State<'_, AppState>,
    text: String,
    note: Option<String>,
) -> Result<AntigravityAccount> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let account = state.antigravity.import_text(&text, note).await?;
    activity::info(
        &state.db,
        "antigravity",
        Some(&account.label),
        "已添加 Google Antigravity 账号",
    );
    Ok(account)
}

#[tauri::command(async)]
pub async fn antigravity_probe_local(state: State<'_, AppState>) -> Result<ClientProbe> {
    state.antigravity.probe_local().await
}

#[tauri::command(async)]
pub async fn antigravity_import_local(
    state: State<'_, AppState>,
    note: Option<String>,
) -> Result<AntigravityAccount> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let account = state.antigravity.import_local(note).await?;
    activity::info(
        &state.db,
        "antigravity",
        Some(&account.label),
        "已从本机导入 Google Antigravity 账号",
    );
    Ok(account)
}

#[tauri::command(async)]
pub fn antigravity_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    let label = state
        .antigravity
        .list()?
        .into_iter()
        .find(|a| a.id.as_str() == id)
        .map(|a| a.label);
    state.antigravity.remove(&id)?;
    activity::info(
        &state.db,
        "antigravity",
        label.as_deref(),
        "已删除 Google Antigravity 账号",
    );
    Ok(())
}

#[tauri::command(async)]
pub fn antigravity_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<AntigravityAccount> {
    state.antigravity.set_enabled(&id, enabled)
}

#[tauri::command(async)]
pub fn antigravity_set_note(
    state: State<'_, AppState>,
    id: String,
    note: Option<String>,
) -> Result<AntigravityAccount> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    state.antigravity.set_note(&id, note)
}

#[tauri::command(async)]
pub async fn antigravity_refresh_quota(
    state: State<'_, AppState>,
    id: String,
) -> Result<AntigravityAccount> {
    let account = state.antigravity.refresh_quota(&id).await?;
    activity::info(
        &state.db,
        "antigravity",
        Some(&account.label),
        "已刷新 Google Antigravity 账号状态与配额",
    );
    Ok(account)
}
