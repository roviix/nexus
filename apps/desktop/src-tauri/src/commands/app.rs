//! 应用级命令：自检、设置、活动日志。

use crate::state::AppState;
use nexus_core::{AppError, ErrorCode, Result};
use nexus_cursor::{is_cursor_install, normalize_app_dir, Cursor, SchemaCheck};
use nexus_store::{activity, settings};
use serde::Serialize;
use tauri::{AppHandle, State};

/// 启动后第一屏要知道的一切。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub version: String,
    /// Cursor 的自检结果。不通过时切号降级为只读（§5.2）。
    pub cursor: SchemaCheck,
    pub cursor_user_dir: String,
    /// **生效的**安装目录。探不到、或者用户填的那个里面没有 Cursor，都是 `None`
    /// —— 只影响启动 Cursor 和 Sand，不影响读写登录态。
    pub cursor_app_dir: Option<String>,
    /// 用户在设置里填的安装目录**原文**。和上面那个分开给：填了却没生效时，
    /// 界面要能说「这个目录里没有 Cursor」而不是含糊的「未检测到」，
    /// 而输入框里还得留着用户填的那行字，不能因为没认出来就抹掉。
    pub cursor_app_dir_setting: String,
    pub cursor_version: Option<String>,
    /// 切号时是否连机器码一起切。
    pub switch_machine_ids: bool,
    pub backup_keep: u32,
}

// 要读 state.vscdb、storage.json、product.json —— 别在主线程上做。
#[tauri::command(async)]
pub fn app_status(state: State<'_, AppState>) -> Result<AppStatus> {
    let paths = &state.switcher.cursor().paths;
    Ok(AppStatus {
        version: env!("CARGO_PKG_VERSION").to_string(),
        cursor: state.switcher.cursor().check(),
        cursor_user_dir: paths.user_dir.display().to_string(),
        cursor_app_dir: paths.app.as_ref().map(|p| p.display().to_string()),
        cursor_app_dir_setting: settings::get_raw(&state.db, settings::CURSOR_APP_DIR)?
            .unwrap_or_default(),
        cursor_version: paths.version(),
        switch_machine_ids: settings::get_or(&state.db, settings::SWITCH_MACHINE_IDS, false),
        backup_keep: settings::get_or(
            &state.db,
            settings::BACKUP_KEEP,
            nexus_switcher::DEFAULT_KEEP,
        ),
    })
}

#[tauri::command(async)]
pub fn app_activity(
    state: State<'_, AppState>,
    limit: Option<u32>,
) -> Result<Vec<activity::Entry>> {
    activity::recent(&state.db, limit.unwrap_or(100).min(500))
}

/// 设置页能改的东西。每一项都能单独改，`None` = 不动。
///
/// 改 Cursor 目录会立刻换上：切号、Sand、CRSR、网关一起指到新路径，不用重启客户端。
/// 切号或补丁正在跑时会拒绝，目录也不会落库。
#[tauri::command(async)]
pub fn app_update_settings(
    state: State<'_, AppState>,
    switch_machine_ids: Option<bool>,
    backup_keep: Option<u32>,
    cursor_user_dir: Option<String>,
    cursor_app_dir: Option<String>,
) -> Result<AppStatus> {
    if let Some(v) = switch_machine_ids {
        settings::set(&state.db, settings::SWITCH_MACHINE_IDS, &v)?;
    }
    if let Some(v) = backup_keep {
        settings::set(&state.db, settings::BACKUP_KEEP, &v.clamp(1, 200))?;
    }
    if cursor_user_dir.is_some() || cursor_app_dir.is_some() {
        let mut user = settings::get_raw(&state.db, settings::CURSOR_USER_DIR)?.unwrap_or_default();
        let mut app = settings::get_raw(&state.db, settings::CURSOR_APP_DIR)?.unwrap_or_default();
        if let Some(v) = cursor_user_dir {
            user = persist_user_dir(&v);
        }
        if let Some(v) = cursor_app_dir {
            app = persist_app_dir(&v);
        }
        let cursor = AppState::assemble_cursor(
            if user.is_empty() {
                None
            } else {
                Some(user.as_str())
            },
            if app.is_empty() {
                None
            } else {
                Some(app.as_str())
            },
        )?;
        state.retarget_cursor(cursor)?;
        settings::set_raw(&state.db, settings::CURSOR_USER_DIR, &user)?;
        settings::set_raw(&state.db, settings::CURSOR_APP_DIR, &app)?;
        activity::info(&state.db, "app", None, "已更新 Cursor 目录");
    }
    app_status(state)
}

/// 系统文件夹选择器。必须在主线程上弹，否则 macOS 上对话框出不来。
#[tauri::command]
pub async fn app_pick_dir(app: AppHandle) -> Result<Option<String>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || {
        let picked = rfd::FileDialog::new()
            .set_title("选择目录")
            .pick_folder()
            .map(|p| p.display().to_string());
        let _ = tx.send(picked);
    })
    .map_err(|err| AppError::new(ErrorCode::Internal, format!("打不开文件夹选择器：{err}")))?;
    rx.await
        .map_err(|_| AppError::new(ErrorCode::Internal, "文件夹选择器没有返回。"))
}

fn persist_user_dir(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        String::new()
    } else {
        Cursor::at(t).paths.user_dir.display().to_string()
    }
}

fn persist_app_dir(raw: &str) -> String {
    let t = raw.trim();
    if t.is_empty() {
        return String::new();
    }
    let norm = normalize_app_dir(t);
    if is_cursor_install(&norm) {
        norm.display().to_string()
    } else {
        t.to_string()
    }
}
