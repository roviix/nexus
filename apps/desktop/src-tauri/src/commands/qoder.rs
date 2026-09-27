//! Qoder 账号命令。PAT 和 job token 都不经过 IPC。
//!
//! 没有设备码登录：聊天要的是 Personal Access Token 换成的 job token。
//! 用户在 Qoder 的集成页创建一把 PAT，贴进来。国际版和国内版是两条号。

use crate::state::AppState;
use nexus_core::{QoderAccountId, Result};
use nexus_qoder::service::ImportReport;
use nexus_qoder::{QoderAccount, QoderBackend};
use nexus_store::activity;
use tauri::State;

#[tauri::command(async)]
pub fn qoder_list(state: State<'_, AppState>) -> Result<Vec<QoderAccount>> {
    state.qoder.list()
}

#[tauri::command(async)]
pub async fn qoder_import_text(
    state: State<'_, AppState>,
    text: String,
    backend: Option<String>,
    note: Option<String>,
) -> Result<ImportReport> {
    let backend = backend
        .as_deref()
        .map(QoderBackend::parse)
        .unwrap_or(QoderBackend::Global);
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let report = state
        .qoder
        .import_text(&text, backend, note.as_deref())
        .await?;
    for a in &report.accounts {
        activity::info(&state.db, "qoder", Some(&a.label()), "已导入 Qoder 账号");
    }
    for why in &report.skipped {
        activity::warn(&state.db, "qoder", None, format!("有一条没导进来：{why}"));
    }
    Ok(report)
}

#[tauri::command(async)]
pub fn qoder_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    let id = QoderAccountId::from_raw(id);
    let account = state.qoder.get(&id)?;
    state.gateway.channel_forget("qoder", &account.label());
    state.qoder.remove(&id)?;
    activity::info(
        &state.db,
        "qoder",
        Some(&account.label()),
        "已删除 Qoder 账号",
    );
    Ok(())
}

#[tauri::command(async)]
pub fn qoder_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<QoderAccount> {
    let id = QoderAccountId::from_raw(id);
    let account = state.qoder.set_enabled(&id, enabled)?;
    if !enabled {
        state.gateway.channel_forget("qoder", &account.label());
    }
    activity::info(
        &state.db,
        "qoder",
        Some(&account.label()),
        if enabled {
            "Qoder 账号已加入网关"
        } else {
            "Qoder 账号已暂停"
        },
    );
    Ok(account)
}

#[tauri::command(async)]
pub fn qoder_set_note(
    state: State<'_, AppState>,
    id: String,
    note: Option<String>,
) -> Result<QoderAccount> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    state
        .qoder
        .set_note(&QoderAccountId::from_raw(id), note.as_deref())
}
