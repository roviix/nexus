//! Kiro 订阅号命令。凭证从不经过 IPC。

use crate::commands::events;
use crate::state::AppState;
use nexus_core::{AppError, ErrorCode, KiroAccountId, Result};
use nexus_kiro::{KiroAccount, LoginHandle, LoginState};
use nexus_store::activity;
use tauri::{AppHandle, Emitter, State};

#[tauri::command(async)]
pub fn kiro_list(state: State<'_, AppState>) -> Result<Vec<KiroAccount>> {
    state.kiro.list()
}

#[tauri::command]
pub async fn kiro_login_start(
    app: AppHandle,
    state: State<'_, AppState>,
    note: Option<String>,
) -> Result<LoginHandle> {
    let handle = state.kiro.start_login(note).await?;
    open_default_browser(&app, &handle.authorize_url)?;
    activity::info(&state.db, "kiro", None, "发起 Kiro 授权");
    let kiro = state.kiro.clone();
    let db = state.db.clone();
    let emitter = app.clone();
    let session_id = handle.session_id.clone();
    tauri::async_runtime::spawn(async move {
        let result = kiro
            .wait_login(&session_id, &|st: LoginState| {
                let _ = emitter.emit(events::KIRO_LOGIN, st);
            })
            .await;
        match result {
            Ok(up) => {
                activity::info(
                    &db,
                    "kiro",
                    Some(&up.account.label()),
                    if up.created {
                        "已通过授权添加 Kiro 账号"
                    } else {
                        "已重新授权 Kiro 账号"
                    },
                );
            }
            Err(err) if err.code != ErrorCode::Cancelled => {
                activity::warn(
                    &db,
                    "kiro",
                    None,
                    format!("Kiro 授权未完成：{}", err.message),
                );
            }
            Err(_) => {}
        }
    });
    Ok(handle)
}

#[tauri::command(async)]
pub fn kiro_login_cancel(state: State<'_, AppState>, session_id: String) -> Result<()> {
    state.kiro.cancel_login(&session_id);
    Ok(())
}

#[tauri::command]
pub async fn kiro_import_cli(state: State<'_, AppState>) -> Result<KiroAccount> {
    let path = state
        .home_dir
        .join(".aws")
        .join("sso")
        .join("cache")
        .join("kiro-auth-token.json");
    let up = state.kiro.import_kiro_cli(&path).await?;
    activity::info(
        &state.db,
        "kiro",
        Some(&up.account.label()),
        "从本机 Kiro 导入了账号",
    );
    Ok(up.account)
}

#[tauri::command]
pub async fn kiro_import_text(
    state: State<'_, AppState>,
    text: String,
    note: Option<String>,
) -> Result<KiroAccount> {
    let up = state.kiro.import_text(&text, note.as_deref()).await?;
    activity::info(
        &state.db,
        "kiro",
        Some(&up.account.label()),
        "已导入 Kiro 账号",
    );
    Ok(up.account)
}

#[tauri::command(async)]
pub fn kiro_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    let id = KiroAccountId::from_raw(id);
    let account = state.kiro.get(&id)?;
    state.gateway.channel_forget("kiro", &account.label());
    state.kiro.remove(&id)?;
    activity::info(
        &state.db,
        "kiro",
        Some(&account.label()),
        "已删除 Kiro 账号",
    );
    Ok(())
}

#[tauri::command(async)]
pub fn kiro_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<KiroAccount> {
    let id = KiroAccountId::from_raw(id);
    let account = state.kiro.set_enabled(&id, enabled)?;
    if !enabled {
        state.gateway.channel_forget("kiro", &account.label());
    }
    activity::info(
        &state.db,
        "kiro",
        Some(&account.label()),
        if enabled {
            "Kiro 账号已加入网关"
        } else {
            "Kiro 账号已暂停"
        },
    );
    Ok(account)
}

#[tauri::command(async)]
pub fn kiro_set_note(
    state: State<'_, AppState>,
    id: String,
    note: Option<String>,
) -> Result<KiroAccount> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    state
        .kiro
        .set_note(&KiroAccountId::from_raw(id), note.as_deref())
}

fn open_default_browser(app: &AppHandle, url: &str) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(url, None::<&str>).map_err(|err| {
        AppError::internal(format!("打不开系统浏览器：{err}"))
            .with_hint("手动复制授权链接到浏览器里打开也可以。")
    })
}
