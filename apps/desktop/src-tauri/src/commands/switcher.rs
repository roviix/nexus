//! 切号命令。
//!
//! 切号默认热切（不退出 Cursor）；只有打开「切换时同时切机器码」或 Cursor 没在跑时
//! 才走冷切。每一条都是**用户点击**直接触发的；这里没有任何定时器或后台任务。

use crate::commands::events;
use crate::state::AppState;
use nexus_core::{BackupId, ProfileId, Result};
use nexus_store::settings;
use nexus_switcher::{
    AuthBackup, Overview, SwitchOptions, SwitchOutcome, SwitchProfile, SwitchProgress,
};
use tauri::{AppHandle, Emitter, State};

// 这两条要开 state.vscdb 跑十来个查询、再读三个文件。同步 command 跑在主线程上，
// 库一被占住整个界面就卡住 —— 标 `async` 让 Tauri 把它们挪到线程池。
#[tauri::command(async)]
pub fn switcher_overview(state: State<'_, AppState>) -> Result<Overview> {
    state.switcher.overview()
}

#[tauri::command(async)]
pub fn switcher_list(state: State<'_, AppState>) -> Result<Vec<SwitchProfile>> {
    state.switcher.list()
}

/// 把 Cursor 里当前登录的号收进切号本。
#[tauri::command(async)]
pub fn switcher_capture_current(
    state: State<'_, AppState>,
    note: Option<String>,
) -> Result<SwitchProfile> {
    state.switcher.capture_current(note.as_deref())
}

#[tauri::command(async)]
pub fn switcher_set_note(
    state: State<'_, AppState>,
    id: String,
    note: Option<String>,
) -> Result<()> {
    state
        .switcher
        .book
        .set_note(&ProfileId::from_raw(id), note.as_deref())
}

#[tauri::command(async)]
pub fn switcher_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    state.switcher.book.remove(&ProfileId::from_raw(id))
}

/// 切到某一档。
///
/// 整段跑在阻塞线程上：冷切里有等进程退出这种真的会睡上十几秒的操作，放在异步执行器上
/// 会把整个 runtime 卡住。进度通过 `switcher://progress` 事件实时推给界面。
#[tauri::command]
pub async fn switcher_switch_to(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    relaunch: Option<bool>,
) -> Result<SwitchOutcome> {
    let switcher = state.switcher.clone();
    let id = ProfileId::from_raw(id);
    let options = SwitchOptions {
        relaunch: relaunch.unwrap_or(true),
        switch_machine_ids: settings::get_or(&state.db, settings::SWITCH_MACHINE_IDS, false),
        prefer_hot: true,
    };

    run_blocking(move || {
        switcher.switch_to(&id, options, &|progress: SwitchProgress| {
            let _ = app.emit(events::SWITCH_PROGRESS, progress);
        })
    })
    .await
}

#[tauri::command(async)]
pub fn switcher_backups(state: State<'_, AppState>) -> Result<Vec<AuthBackup>> {
    state.switcher.backups.list()
}

#[tauri::command(async)]
pub fn switcher_backup_now(state: State<'_, AppState>) -> Result<Option<AuthBackup>> {
    state.switcher.backup_now()
}

#[tauri::command(async)]
pub fn switcher_remove_backup(state: State<'_, AppState>, id: String) -> Result<()> {
    state.switcher.backups.remove(&BackupId::from_raw(id))
}

/// 从某份备份还原登录态（不动机器码）。
#[tauri::command]
pub async fn switcher_restore_backup(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
    relaunch: Option<bool>,
) -> Result<SwitchOutcome> {
    let switcher = state.switcher.clone();
    let id = BackupId::from_raw(id);
    let relaunch = relaunch.unwrap_or(true);

    run_blocking(move || {
        switcher.restore_backup(&id, relaunch, &|progress: SwitchProgress| {
            let _ = app.emit(events::SWITCH_PROGRESS, progress);
        })
    })
    .await
}

/// 把机器码还原成这台真机的原始值。
#[tauri::command]
pub async fn switcher_restore_machine(
    state: State<'_, AppState>,
    relaunch: Option<bool>,
) -> Result<String> {
    let switcher = state.switcher.clone();
    let relaunch = relaunch.unwrap_or(true);
    run_blocking(move || {
        switcher
            .restore_original_machine(relaunch)
            .map(|ids| ids.short())
    })
    .await
}

/// 把会阻塞的活儿挪到线程池，并把 join 失败翻成一个说得清的错误。
pub(crate) async fn run_blocking<T, F>(f: F) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|err| {
            nexus_core::AppError::internal(format!("后台任务异常结束：{err}"))
                .with_hint("重启应用后重试；你的登录态没有被改动。")
        })?
}
