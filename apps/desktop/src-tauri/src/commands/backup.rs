//! 本地备份命令：`~/.roviix/backups` 下的整库快照（ARCHITECTURE §1.1）。
//!
//! 机制在 `nexus-store::backup`；这里只把它接到 IPC 上、记活动日志、把「在 Finder 里
//! 显示」这种系统动作留在 Rust 侧。前端拿到的永远只是文件的元信息（名字、时间、大小），
//! 备份的内容不经过 IPC。

use crate::state::AppState;
use nexus_core::{AppError, Result};
use nexus_store::{activity, BackupFile, RestoreOutcome};
use std::path::Path;
use tauri::{AppHandle, State};

#[tauri::command(async)]
pub fn backup_list(state: State<'_, AppState>) -> Result<Vec<BackupFile>> {
    state.backups.list()
}

/// 现在备份一份。快照走 `VACUUM INTO`，读库不挡写；几十个号的库不到一秒。
#[tauri::command(async)]
pub fn backup_create(state: State<'_, AppState>) -> Result<BackupFile> {
    let made = state.backups.create(&state.db)?;
    activity::info(&state.db, "app", None, format!("已备份到 {}", made.path));
    Ok(made)
}

/// 用一份备份覆盖当前库。还原前会自动把当前状态另存一份（`pre-restore`）。
///
/// 还原之后**应用要重启**：库是在同一个连接里整体换掉的，但网关的接力名单、Sand 的远程
/// 主机这些模块在启动时读过一次就攥在内存里，只有重启才能让它们看见还原后的数据。
/// 前端拿到结果后调 `relaunch()`；这里只把日志写进**还原后**的库，重启后能看到。
#[tauri::command(async)]
pub fn backup_restore(state: State<'_, AppState>, file_name: String) -> Result<RestoreOutcome> {
    let outcome = state.backups.restore(&state.db, &file_name)?;
    activity::warn(
        &state.db,
        "app",
        None,
        format!(
            "已从备份 {} 还原（还原前的状态另存为 {}）",
            outcome.restored, outcome.safety.file_name
        ),
    );
    Ok(outcome)
}

#[tauri::command(async)]
pub fn backup_remove(state: State<'_, AppState>, file_name: String) -> Result<()> {
    state.backups.remove(&file_name)?;
    activity::info(&state.db, "app", None, format!("已删除备份 {file_name}"));
    Ok(())
}

/// 在 Finder / 资源管理器里显示某个文件；不给路径就打开备份目录。
///
/// 只认 `~/.roviix` 底下的路径。前端拿到的路径都是我们自己回给它的，但「显示文件」
/// 这个能力本身不该对任意路径开放 —— 所以能力留在 Rust 侧，并在这里校验一次。
///
/// **这一个必须留在主线程，不要加 `(async)`。** `reveal_item_in_dir` 在 macOS 上直接调
/// `NSWorkspace::activateFileViewerSelectingURLs`（`tauri-plugin-opener` 没有自己往主队列
/// 派发），AppKit 那套东西在别的线程上调是未定义行为。它本身只做路径校验，留在主线程
/// 也不花时间。
#[tauri::command]
pub fn backup_reveal(
    app: AppHandle,
    state: State<'_, AppState>,
    path: Option<String>,
) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;

    let root = state
        .backups
        .dir()
        .parent()
        .ok_or_else(|| AppError::internal("备份目录没有上级目录。"))?
        .to_path_buf();
    let target = match path {
        Some(p) => {
            let p = Path::new(&p);
            if !p.starts_with(&root) || p.components().any(|c| c.as_os_str() == "..") {
                return Err(AppError::invalid(format!(
                    "只能显示 {} 下的文件。",
                    root.display()
                )));
            }
            p.to_path_buf()
        }
        None => {
            std::fs::create_dir_all(state.backups.dir())?;
            state.backups.dir().to_path_buf()
        }
    };
    app.opener()
        .reveal_item_in_dir(&target)
        .map_err(|err| AppError::internal(format!("打不开文件位置：{err}")))
}

/// 写一个只有本人可读的文件：先建目录（0700），再写内容，再把文件收到 0600。
///
/// 导出的清单与备份一样是凭证文件，落盘那一刻权限就得是对的 —— 不能先写再收，
/// 那一瞬间别的用户读得到。
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        restrict(dir, 0o700);
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes)?;
    }
    Ok(())
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(err) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
        tracing::warn!(path = %path.display(), %err, "收紧权限失败");
    }
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) {}
