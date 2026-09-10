//! 文件层：读、原子写、备份。
//!
//! 这些文件里有钥匙，所以写出来的一律 0600；用户原来的文件在动之前先拷一份到
//! `~/.roviix/backups/clients/<tool>/`，「撤销」就是把它拷回去。

use nexus_core::{AppError, ErrorCode, Result};
use std::path::{Path, PathBuf};

/// 读文本；不存在返回 `None`，其余错误原样报。
pub fn read_opt(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path, e)),
    }
}

/// 原子写：临时文件 → fsync → rename。目录不存在就建（`~/.claude` 在没装过 Claude Code
/// 的机器上不存在，接入就是它第一次出现的理由）。
pub fn write_atomic(path: &Path, data: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.nexus-{}.tmp",
        path.file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default(),
        std::process::id()
    ));
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        restrict(&f)?;
        f.write_all(data.as_bytes())?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, path)
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(io_err(path, e));
    }
    Ok(())
}

/// 把 `path` 拷到备份目录，文件名带时间戳。返回备份路径；源文件不存在返回 `None`。
pub fn backup(path: &Path, dir: &Path) -> Result<Option<PathBuf>> {
    let Some(bytes) = (match std::fs::read(path) {
        Ok(b) => Some(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io_err(path, e)),
    }) else {
        return Ok(None);
    };
    std::fs::create_dir_all(dir).map_err(|e| io_err(dir, e))?;
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config".into());
    let stamp = nexus_core::clock::file_stamp(time::OffsetDateTime::now_utc());
    let mut target = dir.join(format!("{name}.{stamp}"));
    // 同一秒内备份两次（连点两下「接入」）不该覆盖前一份。
    let mut n = 1;
    while target.exists() {
        target = dir.join(format!("{name}.{stamp}-{n}"));
        n += 1;
    }
    let mut f = std::fs::File::create(&target).map_err(|e| io_err(&target, e))?;
    restrict(&f).map_err(|e| io_err(&target, e))?;
    std::io::Write::write_all(&mut f, &bytes).map_err(|e| io_err(&target, e))?;
    Ok(Some(target))
}

/// 用备份覆盖回去（原子）。
pub fn restore(backup: &Path, to: &Path) -> Result<()> {
    let data = std::fs::read_to_string(backup).map_err(|e| io_err(backup, e))?;
    write_atomic(to, &data)
}

pub fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(path, e)),
    }
}

#[cfg(unix)]
fn restrict(f: &std::fs::File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    f.set_permissions(std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict(_f: &std::fs::File) -> std::io::Result<()> {
    Ok(())
}

/// IO 错误翻成给人看的话。权限不够单独说 —— 那是唯一一种用户自己能修的情形。
pub fn io_err(path: &Path, e: std::io::Error) -> AppError {
    if e.kind() == std::io::ErrorKind::PermissionDenied {
        return AppError::new(ErrorCode::Io, format!("没有权限写入 {}", path.display()))
            .with_hint("检查这个文件（和它所在目录）的属主与权限；它应当属于当前用户。");
    }
    AppError::new(ErrorCode::Io, format!("{}：{e}", path.display()))
}
