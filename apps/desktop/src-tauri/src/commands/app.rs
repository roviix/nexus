//! 应用级命令：自检、设置、活动日志。

use crate::state::AppState;
use nexus_core::Result;
use nexus_cursor::SchemaCheck;
use nexus_store::{activity, settings};
use serde::Serialize;
use tauri::State;

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
    if let Some(v) = cursor_user_dir {
        // 改 Cursor 目录要重启才生效：`Cursor` 是在启动时装配好的，热替换它会让
        // 正在跑的切号看到半新半旧的路径。宁可让用户重开一次。
        settings::set_raw(&state.db, settings::CURSOR_USER_DIR, v.trim())?;
        activity::info(&state.db, "app", None, "已修改 Cursor 数据目录，重启后生效");
    }
    if let Some(v) = cursor_app_dir {
        // 同上：`Cursor` 是启动时装配的，安装目录也要重启才换得干净。
        settings::set_raw(&state.db, settings::CURSOR_APP_DIR, v.trim())?;
        activity::info(&state.db, "app", None, "已修改 Cursor 安装目录，重启后生效");
    }
    app_status(state)
}
