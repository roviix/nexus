//! Gemini 网页账号。Cookie 不从这里读出明文，只负责导入和开关。

use crate::state::AppState;
use nexus_core::Result;
use nexus_gemini_web::GeminiWebAccount;
use tauri::State;

#[tauri::command]
pub fn gemini_web_list(state: State<'_, AppState>) -> Result<Vec<GeminiWebAccount>> {
    state.gemini_web.list()
}

#[tauri::command]
pub fn gemini_web_import(
    state: State<'_, AppState>,
    cookie: String,
    email: Option<String>,
    note: Option<String>,
) -> Result<GeminiWebAccount> {
    state.gemini_web.import_cookie(&cookie, email, note)
}

#[tauri::command]
pub fn gemini_web_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    state.gemini_web.remove(&id)
}

#[tauri::command]
pub fn gemini_web_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<GeminiWebAccount> {
    state.gemini_web.set_enabled(&id, enabled)
}

#[tauri::command]
pub fn gemini_web_set_note(
    state: State<'_, AppState>,
    id: String,
    note: Option<String>,
) -> Result<GeminiWebAccount> {
    state.gemini_web.set_note(&id, note)
}
