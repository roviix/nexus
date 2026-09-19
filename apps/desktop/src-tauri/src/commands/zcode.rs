//! ZCode（智谱 GLM 编码套餐）账号命令。凭证从不经过 IPC。
//!
//! 没有「授权登录」：官方 ZCode 客户端登录后把凭证写在 `~/.zcode/v2/credentials.json`，
//! 用户在那边登录一次，这里直接读。所以入口只有两个——从本机客户端导入，或者粘贴一把 key。

use crate::state::AppState;
use nexus_core::{Result, ZcodeAccountId};
use nexus_store::activity;
use nexus_zcode::service::ImportReport;
use nexus_zcode::{ZcodeAccount, ZcodeProvider};
use serde::Serialize;
use tauri::State;

#[tauri::command(async)]
pub fn zcode_list(state: State<'_, AppState>) -> Result<Vec<ZcodeAccount>> {
    state.zcode.list()
}

/// 本机官方客户端的状态。界面据此决定「从 ZCode 导入」这个按钮能不能点，
/// 以及找不到时告诉用户该去看哪个文件。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientProbe {
    pub present: bool,
    pub path: String,
}

#[tauri::command(async)]
pub fn zcode_probe_client(state: State<'_, AppState>) -> Result<ClientProbe> {
    Ok(ClientProbe {
        present: state.zcode.client_credentials_present(),
        path: state.zcode.client_credentials_path().display().to_string(),
    })
}

#[tauri::command(async)]
pub fn zcode_import_client(
    state: State<'_, AppState>,
    note: Option<String>,
) -> Result<ImportReport> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let report = state.zcode.import_from_client(note.as_deref())?;
    log_import(&state, &report, "从本机 ZCode 客户端导入");
    Ok(report)
}

#[tauri::command(async)]
pub fn zcode_import_text(
    state: State<'_, AppState>,
    text: String,
    provider: Option<String>,
    note: Option<String>,
) -> Result<ImportReport> {
    let provider = provider
        .as_deref()
        .map(ZcodeProvider::parse)
        .unwrap_or(ZcodeProvider::Zai);
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    let report = state.zcode.import_text(&text, provider, note.as_deref())?;
    log_import(&state, &report, "已导入 ZCode 账号");
    Ok(report)
}

fn log_import(state: &State<'_, AppState>, report: &ImportReport, what: &str) {
    for a in &report.accounts {
        activity::info(&state.db, "zcode", Some(&a.label()), what);
    }
    for why in &report.skipped {
        activity::warn(&state.db, "zcode", None, format!("有一条没导进来：{why}"));
    }
}

#[tauri::command(async)]
pub fn zcode_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    let id = ZcodeAccountId::from_raw(id);
    let account = state.zcode.get(&id)?;
    // 先让网关忘掉这个号，再删——顺序反了的话接力队会拿着一条已经没有凭证的 label 去发请求。
    state.gateway.channel_forget("zcode", &account.label());
    state.zcode.remove(&id)?;
    activity::info(
        &state.db,
        "zcode",
        Some(&account.label()),
        "已删除 ZCode 账号",
    );
    Ok(())
}

#[tauri::command(async)]
pub fn zcode_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<ZcodeAccount> {
    let id = ZcodeAccountId::from_raw(id);
    let account = state.zcode.set_enabled(&id, enabled)?;
    if !enabled {
        state.gateway.channel_forget("zcode", &account.label());
    }
    activity::info(
        &state.db,
        "zcode",
        Some(&account.label()),
        if enabled {
            "ZCode 账号已加入网关"
        } else {
            "ZCode 账号已暂停"
        },
    );
    Ok(account)
}

#[tauri::command(async)]
pub fn zcode_set_note(
    state: State<'_, AppState>,
    id: String,
    note: Option<String>,
) -> Result<ZcodeAccount> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    state
        .zcode
        .set_note(&ZcodeAccountId::from_raw(id), note.as_deref())
}
