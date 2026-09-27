//! Claude 订阅号。token 不经过 IPC。
//!
//! 授权是 PKCE：在本机 54545 上听 `http://localhost:54545/callback`，和 Claude Code /
//! CLIProxyAPI 同一条路。端口被占时退回「把地址栏贴回来」。也可以直接导入
//! `~/.claude/.credentials.json`、macOS 钥匙串里的 Claude Code 凭证、setup-token 或 Console API Key。

use crate::commands::events;
use crate::state::AppState;
use nexus_claude::{ClaudeAccount, ClientProbe, ImportReport, LoginStart, LoginState};
use nexus_core::{ClaudeAccountId, ErrorCode, Result};
use nexus_store::activity;
use tauri::{AppHandle, Emitter, State};

#[tauri::command(async)]
pub fn claude_list(state: State<'_, AppState>) -> Result<Vec<ClaudeAccount>> {
    state.claude.list()
}

#[tauri::command]
pub async fn claude_login_start(
    app: AppHandle,
    state: State<'_, AppState>,
    note: Option<String>,
) -> Result<LoginStart> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let started = state.claude.start_login(note).await?;
    activity::info(&state.db, "claude", None, "发起 Claude 授权");
    if started.callback_listening {
        let claude = state.claude.clone();
        let db = state.db.clone();
        let emitter = app.clone();
        let session_id = started.login_id.clone();
        tauri::async_runtime::spawn(async move {
            let result = claude
                .wait_login(&session_id, &|st: LoginState| {
                    let _ = emitter.emit(events::CLAUDE_LOGIN, st);
                })
                .await;
            match result {
                Ok(account) => {
                    activity::info(
                        &db,
                        "claude",
                        Some(&account.label),
                        "已通过授权添加 Claude 账号",
                    );
                }
                Err(err) if err.code != ErrorCode::Cancelled => activity::warn(
                    &db,
                    "claude",
                    None,
                    format!("Claude 授权未完成：{}", err.message),
                ),
                Err(_) => {}
            }
        });
    }
    Ok(started)
}

#[tauri::command(async)]
pub fn claude_login_cancel(state: State<'_, AppState>) -> Result<()> {
    state.claude.cancel_login();
    Ok(())
}

#[tauri::command(async)]
pub async fn claude_import_text(
    state: State<'_, AppState>,
    text: String,
    note: Option<String>,
) -> Result<ImportReport> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let report = state.claude.import_text(&text, note.as_deref()).await?;
    for a in &report.accounts {
        activity::info(&state.db, "claude", Some(&a.label), "已添加 Claude 账号");
    }
    Ok(report)
}

#[tauri::command(async)]
pub fn claude_probe_local(state: State<'_, AppState>) -> Result<ClientProbe> {
    Ok(state.claude.probe_local())
}

#[tauri::command(async)]
pub async fn claude_import_local(
    state: State<'_, AppState>,
    note: Option<String>,
) -> Result<ImportReport> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let report = state.claude.import_local(note.as_deref()).await?;
    for a in &report.accounts {
        activity::info(
            &state.db,
            "claude",
            Some(&a.label),
            "已从本机 Claude Code 导入账号",
        );
    }
    Ok(report)
}

#[tauri::command(async)]
pub async fn claude_refresh_quota(
    state: State<'_, AppState>,
    id: String,
) -> Result<ClaudeAccount> {
    state
        .claude
        .refresh_quota(&ClaudeAccountId::from_raw(id))
        .await
}

#[tauri::command(async)]
pub fn claude_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    let id = ClaudeAccountId::from_raw(id);
    let label = state.claude.list()?.into_iter().find(|a| a.id == id).map(|a| a.label);
    state.claude.remove(&id)?;
    activity::info(
        &state.db,
        "claude",
        label.as_deref(),
        "已删除 Claude 账号",
    );
    Ok(())
}

#[tauri::command(async)]
pub fn claude_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<ClaudeAccount> {
    state
        .claude
        .set_enabled(&ClaudeAccountId::from_raw(id), enabled)
}

#[tauri::command(async)]
pub fn claude_set_note(
    state: State<'_, AppState>,
    id: String,
    note: Option<String>,
) -> Result<ClaudeAccount> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    state
        .claude
        .set_note(&ClaudeAccountId::from_raw(id), note.as_deref())
}
