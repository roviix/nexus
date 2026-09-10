//! Grok Build 订阅号命令。凭证从不经过 IPC。

use crate::commands::events;
use crate::state::AppState;
use nexus_core::{AppError, ErrorCode, GrokAccountId, Result};
use nexus_grok::{GrokAccount, GrokManifestModel, LoginHandle, LoginState};
use nexus_store::activity;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, State};

#[tauri::command(async)]
pub fn grok_list(state: State<'_, AppState>) -> Result<Vec<GrokAccount>> {
    state.grok.list()
}

#[tauri::command]
pub async fn grok_login_start(
    app: AppHandle,
    state: State<'_, AppState>,
    note: Option<String>,
) -> Result<LoginHandle> {
    let handle = state.grok.start_login(note).await?;
    open_default_browser(&app, &handle.authorize_url)?;
    activity::info(&state.db, "grok", None, "发起 Grok Build 授权");
    let grok = state.grok.clone();
    let db = state.db.clone();
    let emitter = app.clone();
    let session_id = handle.session_id.clone();
    tauri::async_runtime::spawn(async move {
        let result = grok
            .wait_login(&session_id, &|st: LoginState| {
                let _ = emitter.emit(events::GROK_LOGIN, st);
            })
            .await;
        match result {
            Ok(up) => {
                activity::info(
                    &db,
                    "grok",
                    Some(&up.account.label()),
                    if up.created {
                        "已通过授权添加 Grok 账号"
                    } else {
                        "已重新授权 Grok 账号"
                    },
                );
            }
            Err(err) if err.code != ErrorCode::Cancelled => {
                activity::warn(
                    &db,
                    "grok",
                    None,
                    format!("Grok 授权未完成：{}", err.message),
                );
            }
            Err(_) => {}
        }
    });
    Ok(handle)
}

#[tauri::command(async)]
pub fn grok_login_cancel(state: State<'_, AppState>, session_id: String) -> Result<()> {
    state.grok.cancel_login(&session_id);
    Ok(())
}

#[tauri::command]
pub async fn grok_import_cli(state: State<'_, AppState>) -> Result<GrokAccount> {
    let home = std::env::var_os("GROK_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| state.home_dir.join(".grok"));
    let up = state.grok.import_grok_cli(&home).await?;
    activity::info(
        &state.db,
        "grok",
        Some(&up.account.label()),
        "从本机 Grok CLI 导入了账号",
    );
    Ok(up.account)
}

#[tauri::command]
pub async fn grok_import_text(
    state: State<'_, AppState>,
    text: String,
    note: Option<String>,
) -> Result<GrokAccount> {
    let up = state.grok.import_text(&text, note.as_deref()).await?;
    activity::info(
        &state.db,
        "grok",
        Some(&up.account.label()),
        "已导入 Grok 账号",
    );
    Ok(up.account)
}

#[tauri::command(async)]
pub fn grok_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    let id = GrokAccountId::from_raw(id);
    let account = state.grok.get(&id)?;
    state.gateway.channel_forget("grok", &account.label());
    state.grok.remove(&id)?;
    activity::info(
        &state.db,
        "grok",
        Some(&account.label()),
        "已删除 Grok 账号",
    );
    Ok(())
}

#[tauri::command(async)]
pub fn grok_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<GrokAccount> {
    let id = GrokAccountId::from_raw(id);
    let account = state.grok.set_enabled(&id, enabled)?;
    if !enabled {
        state.gateway.channel_forget("grok", &account.label());
    }
    activity::info(
        &state.db,
        "grok",
        Some(&account.label()),
        if enabled {
            "Grok 账号已加入网关"
        } else {
            "Grok 账号已暂停"
        },
    );
    Ok(account)
}

#[tauri::command(async)]
pub fn grok_set_note(
    state: State<'_, AppState>,
    id: String,
    note: Option<String>,
) -> Result<GrokAccount> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    state
        .grok
        .set_note(&GrokAccountId::from_raw(id), note.as_deref())
}

/// 现在拉一次额度 / 档位（`/v1/billing?format=credits`）。平时不用点：网关取号前会按需刷。
#[tauri::command]
pub async fn grok_refresh_quota(state: State<'_, AppState>, id: String) -> Result<GrokAccount> {
    state.grok.refresh_quota(&GrokAccountId::from_raw(id)).await
}

/// 手动覆盖媒体资格：`Some(true)` 强开、`Some(false)` 强关、`None` 回到自动探测。
#[tauri::command(async)]
pub fn grok_set_media_override(
    state: State<'_, AppState>,
    id: String,
    value: Option<bool>,
) -> Result<GrokAccount> {
    let account = state
        .grok
        .set_media_override(&GrokAccountId::from_raw(id), value)?;
    activity::info(
        &state.db,
        "grok",
        Some(&account.label()),
        match value {
            Some(true) => "Grok 媒体资格：手动开启",
            Some(false) => "Grok 媒体资格：手动关闭",
            None => "Grok 媒体资格：回到自动探测",
        },
    );
    Ok(account)
}

/// 上次拉到的模型目录（`/v1/models-v2`）。
#[tauri::command(async)]
pub fn grok_models(state: State<'_, AppState>) -> Vec<GrokManifestModel> {
    state.grok.manifest()
}

#[tauri::command]
pub async fn grok_refresh_models(state: State<'_, AppState>) -> Result<Vec<GrokManifestModel>> {
    let models = state.grok.refresh_models_any().await?;
    activity::info(
        &state.db,
        "grok",
        None,
        format!("拉到 Grok 模型目录：{} 个", models.len()),
    );
    Ok(models)
}

/// 加一个 xAI API Key 号（按 token 计费，走 api.x.ai）。key 只进 Rust。
#[tauri::command]
pub async fn grok_add_api_key(
    state: State<'_, AppState>,
    api_key: String,
    note: Option<String>,
) -> Result<GrokAccount> {
    let up = state.grok.add_api_key(&api_key, note.as_deref()).await?;
    activity::info(
        &state.db,
        "grok",
        Some(&up.account.label()),
        if up.created {
            "已添加 xAI API Key 账号"
        } else {
            "已更新 xAI API Key"
        },
    );
    Ok(up.account)
}

fn open_default_browser(app: &AppHandle, url: &str) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(url, None::<&str>).map_err(|err| {
        AppError::internal(format!("打不开系统浏览器：{err}"))
            .with_hint("手动复制授权链接到浏览器里打开也可以。")
    })
}
