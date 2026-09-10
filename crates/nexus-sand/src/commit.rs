//! 把一组计划好的改动写进磁盘：**先备份、逐文件原子写、写完校验、任一步失败全部回滚。**
//!
//! 和切号的「单事务清写」是同一种谨慎，只是对象从 SQLite 变成了一组文件。文件系统没有事务，
//! 所以用「写前 sha 核对 + 写后 sha 核对 + 失败按原字节写回」凑一个：任何时刻进程被杀，
//! 每个文件要么是改前的、要么是改后的；即便回滚也失败，备份目录里还有原字节，界面会把
//! 目录路径告诉用户。移植自 Python `_commit_plan` / `_atomic_write`。

use nexus_core::{AppError, ErrorCode, Result};
use std::path::Path;

use crate::backup::{Backups, PlannedFile};
use crate::integrity::sha256_hex;
use crate::model::Operation;

/// 原子写：临时文件 → fsync → rename。只读文件会临时加写权限再改回去。
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_file_name(format!(
        ".{}.nexus-sand-{}.tmp",
        path.file_name()
            .map(|s| s.to_string_lossy())
            .unwrap_or_default(),
        std::process::id()
    ));
    // Windows 上带只读属性的目标会让 rename 直接 `ACCESS_DENIED`。先摘掉属性，
    // 写完再按原样贴回去 —— Cursor 自己的安装包偶尔会把 bundle 标成只读。
    let was_readonly = take_readonly(path);
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        copy_mode(path, &tmp)?;
        std::fs::rename(&tmp, path)
    };
    let result = write();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    // 成功时贴回新文件，失败时贴回原文件：两条路都要还原用户看到的属性。
    if was_readonly {
        set_readonly(path);
    }
    Ok(result?)
}

/// 写之前先确认这些文件真的能写。
///
/// 存在的理由是代价不对称：`commit_plan` 要先退出 Cursor、再备份、才开始写，等到那时
/// 才撞上「装在 Program Files、当前用户没权限」，用户已经白白被关掉了编辑器。
pub fn ensure_writable(paths: &[std::path::PathBuf]) -> Result<()> {
    for path in paths {
        if let Err(err) = writable_probe(path) {
            if err.kind() != std::io::ErrorKind::PermissionDenied {
                continue; // 不是权限问题（比如文件刚被挪走）就交给真正的写入去报。
            }
            return Err(
                AppError::new(ErrorCode::Io, format!("没有写入权限：{}", path.display()))
                    .with_hint(permission_hint()),
            );
        }
    }
    Ok(())
}

/// 权限不足时该怎么办。两个平台的出路完全不同，别给一句放之四海的废话。
fn permission_hint() -> &'static str {
    if cfg!(target_os = "windows") {
        "这份 Cursor 装在需要管理员权限的目录（如 Program Files）。\
         用管理员身份重开 Nexus，或把 Cursor 改装到用户目录（安装器默认的 %LOCALAPPDATA%\\Programs\\cursor）。"
    } else {
        "当前用户改不动这个 Cursor 安装。检查它的属主与权限，或重新装到你自己的用户目录下。"
    }
}

// clippy 反对 `set_readonly(false)` 是因为它在 Unix 上等于放成 0666；这两个函数只在 Windows
// 编译，那里「只读」就是一个文件属性位，摘掉它正是本意。
#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn writable_probe(path: &Path) -> std::io::Result<()> {
    // 只读属性本身不等于没权限：能摘掉它就说明能写。摘完立刻贴回去，不留副作用。
    let perms = std::fs::metadata(path)?.permissions();
    if perms.readonly() {
        let mut relaxed = perms.clone();
        relaxed.set_readonly(false);
        std::fs::set_permissions(path, relaxed)?;
        let _ = std::fs::set_permissions(path, perms);
        return Ok(());
    }
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map(|_| ())
}

#[cfg(not(windows))]
fn writable_probe(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map(|_| ())
}

/// 摘掉只读属性，返回「原来是不是只读」。Windows 专有：Unix 上用的是 mode 位，
/// 由 [`copy_mode`] 负责，这里动它反而会把 0444 放成 0666。
#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn take_readonly(path: &Path) -> bool {
    let Ok(perms) = std::fs::metadata(path).map(|m| m.permissions()) else {
        return false;
    };
    if !perms.readonly() {
        return false;
    }
    let mut relaxed = perms;
    relaxed.set_readonly(false);
    std::fs::set_permissions(path, relaxed).is_ok()
}

#[cfg(not(windows))]
fn take_readonly(_path: &Path) -> bool {
    false
}

#[cfg(windows)]
fn set_readonly(path: &Path) {
    if let Ok(mut perms) = std::fs::metadata(path).map(|m| m.permissions()) {
        perms.set_readonly(true);
        let _ = std::fs::set_permissions(path, perms);
    }
}

#[cfg(not(windows))]
fn set_readonly(_path: &Path) {}

#[cfg(unix)]
fn copy_mode(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Ok(meta) = std::fs::metadata(from) {
        std::fs::set_permissions(to, meta.permissions())?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn copy_mode(_from: &Path, _to: &Path) -> std::io::Result<()> {
    Ok(())
}

/// 写入结果。
#[derive(Debug, Clone)]
pub struct Committed {
    pub backup_id: String,
    pub files_written: u32,
}

/// 执行一份计划。`validate` 在全部文件写完后跑（比如重新 inspect、校验 checksum）；
/// 它一报错就整体回滚。
pub fn commit_plan(
    backups: &Backups,
    app_root: &Path,
    cursor_version: &str,
    operation: Operation,
    plan: &[PlannedFile],
    validate: &dyn Fn() -> Result<()>,
) -> Result<Committed> {
    if plan.is_empty() {
        return Err(AppError::internal("内部错误：提交计划为空。"));
    }
    // 计划是按「当时读到的内容」算的；写之前再核一次，防止中途有人动过文件。
    for f in plan {
        if sha256_hex(&std::fs::read(&f.path)?) != sha256_hex(&f.original) {
            return Err(AppError::new(
                ErrorCode::SandIntegrity,
                format!("文件在计划生成后被改动，已停止：{}", f.path.display()),
            )
            .with_hint("Cursor 没有被改动。重新打开 Sand 页刷新状态后再试。"));
        }
    }
    let backup_id = backups.create(app_root, cursor_version, operation, plan)?;

    // `written` 记录已经动过的文件（下标）；写到一半失败时只回滚这些。
    let mut written: Vec<usize> = Vec::with_capacity(plan.len());
    let run = |written: &mut Vec<usize>| -> Result<()> {
        for (i, f) in plan.iter().enumerate() {
            if sha256_hex(&std::fs::read(&f.path)?) != sha256_hex(&f.original) {
                return Err(AppError::new(
                    ErrorCode::SandIntegrity,
                    format!("文件在写入前被改动，已停止：{}", f.path.display()),
                ));
            }
            // 先登记再写：写到一半崩了，回滚时才知道要看这个文件。
            written.push(i);
            atomic_write(&f.path, &f.next)?;
        }
        validate()?;
        for f in plan {
            if sha256_hex(&std::fs::read(&f.path)?) != sha256_hex(&f.next) {
                return Err(AppError::new(
                    ErrorCode::SandIntegrity,
                    format!("写入后哈希校验失败：{}", f.path.display()),
                ));
            }
        }
        Ok(())
    };

    match run(&mut written) {
        Ok(()) => {
            backups.finish(&backup_id, "committed", None)?;
            Ok(Committed {
                backup_id,
                files_written: plan.len() as u32,
            })
        }
        Err(err) => {
            let mut rollback_errors = Vec::new();
            for f in written.iter().rev().map(|&i| &plan[i]) {
                match std::fs::read(&f.path) {
                    Ok(cur) => {
                        let cur = sha256_hex(&cur);
                        if cur == sha256_hex(&f.original) {
                            continue;
                        }
                        if cur != sha256_hex(&f.next) {
                            rollback_errors
                                .push(format!("{}: 已被外部修改，未覆盖", f.path.display()));
                            continue;
                        }
                        if let Err(e) = atomic_write(&f.path, &f.original) {
                            rollback_errors.push(format!("{}: {e}", f.path.display()));
                        }
                    }
                    Err(e) => rollback_errors.push(format!("{}: {e}", f.path.display())),
                }
            }
            let mut message = err.message.clone();
            if !rollback_errors.is_empty() {
                message.push_str("；回滚错误：");
                message.push_str(&rollback_errors.join(" | "));
            }
            let _ = backups.finish(&backup_id, "rolled_back", Some(message.clone()));
            if rollback_errors.is_empty() {
                Err(err.with_hint("已自动回滚，Cursor 文件恢复为改动前。"))
            } else {
                Err(AppError::new(
                    ErrorCode::SandRollbackIncomplete,
                    format!(
                        "补丁失败且有文件未能自动回滚。原始文件在备份目录：{}",
                        backups.root().join(&backup_id).display()
                    ),
                )
                .with_hint(message))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Fixture {
        _dir: tempfile::TempDir,
        app_root: std::path::PathBuf,
        backups: Backups,
        plan: Vec<PlannedFile>,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let app_root = dir.path().join("app");
        std::fs::create_dir_all(app_root.join("out")).unwrap();
        let a = app_root.join("out/a.js");
        let b = app_root.join("out/b.js");
        std::fs::write(&a, b"a-orig").unwrap();
        std::fs::write(&b, b"b-orig").unwrap();
        let backups = Backups::new(dir.path(), &app_root);
        let plan = vec![
            PlannedFile {
                path: a,
                original: b"a-orig".to_vec(),
                next: b"a-new".to_vec(),
            },
            PlannedFile {
                path: b,
                original: b"b-orig".to_vec(),
                next: b"b-new".to_vec(),
            },
        ];
        Fixture {
            _dir: dir,
            app_root,
            backups,
            plan,
        }
    }

    #[test]
    fn writes_all_files_and_marks_backup_committed() {
        let f = fixture();
        let c = commit_plan(
            &f.backups,
            &f.app_root,
            "3.18.9",
            Operation::Install,
            &f.plan,
            &|| Ok(()),
        )
        .unwrap();
        assert_eq!(c.files_written, 2);
        assert_eq!(std::fs::read(&f.plan[0].path).unwrap(), b"a-new");
        assert_eq!(std::fs::read(&f.plan[1].path).unwrap(), b"b-new");
        assert_eq!(f.backups.manifest(&c.backup_id).unwrap().state, "committed");
        assert_eq!(
            f.backups.original_bytes(&c.backup_id, "out/a.js").unwrap(),
            b"a-orig"
        );
    }

    #[test]
    fn validator_failure_rolls_every_file_back() {
        let f = fixture();
        let err = commit_plan(
            &f.backups,
            &f.app_root,
            "3.18.9",
            Operation::Install,
            &f.plan,
            &|| Err(AppError::new(ErrorCode::SandIntegrity, "假装校验失败")),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::SandIntegrity);
        assert!(err.hint.unwrap().contains("已自动回滚"));
        assert_eq!(std::fs::read(&f.plan[0].path).unwrap(), b"a-orig");
        assert_eq!(std::fs::read(&f.plan[1].path).unwrap(), b"b-orig");
        let list = f.backups.list().unwrap();
        assert_eq!(list[0].state, "rolled_back");
    }

    #[test]
    fn refuses_when_a_file_changed_since_planning() {
        let f = fixture();
        std::fs::write(&f.plan[1].path, b"tampered").unwrap();
        let err = commit_plan(
            &f.backups,
            &f.app_root,
            "3.18.9",
            Operation::Install,
            &f.plan,
            &|| Ok(()),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::SandIntegrity);
        assert!(err.hint.unwrap().contains("没有被改动"));
        // 第一个文件也没被碰
        assert_eq!(std::fs::read(&f.plan[0].path).unwrap(), b"a-orig");
        assert!(
            f.backups.list().unwrap().is_empty(),
            "核对失败在备份之前，不该留备份"
        );
    }

    #[test]
    fn external_edit_during_rollback_is_reported_not_overwritten() {
        let f = fixture();
        let path_a = f.plan[0].path.clone();
        let calls = Cell::new(0);
        let err = commit_plan(
            &f.backups,
            &f.app_root,
            "3.18.9",
            Operation::Install,
            &f.plan,
            &|| {
                // 校验期间有人把 a 改成第三种内容，然后校验失败
                calls.set(calls.get() + 1);
                std::fs::write(&path_a, b"someone-else").unwrap();
                Err(AppError::new(ErrorCode::SandIntegrity, "校验失败"))
            },
        )
        .unwrap_err();
        assert_eq!(calls.get(), 1);
        assert_eq!(err.code, ErrorCode::SandRollbackIncomplete);
        assert!(err.message.contains("备份目录"));
        // 外部改的不覆盖；另一个正常回滚
        assert_eq!(std::fs::read(&path_a).unwrap(), b"someone-else");
        assert_eq!(std::fs::read(&f.plan[1].path).unwrap(), b"b-orig");
    }

    #[test]
    fn atomic_write_replaces_content_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.js");
        std::fs::write(&p, b"old").unwrap();
        atomic_write(&p, b"new").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
    }

    /// Windows 上 Cursor 的 bundle 可能带只读属性。写要成功，属性还要留在原样 ——
    /// 否则我们「顺手」改了用户安装的形状。
    #[cfg(windows)]
    #[test]
    fn atomic_write_handles_a_readonly_target_and_restores_the_flag() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("f.js");
        std::fs::write(&p, b"old").unwrap();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&p, perms).unwrap();

        atomic_write(&p, b"new").unwrap();

        assert_eq!(std::fs::read(&p).unwrap(), b"new");
        assert!(
            std::fs::metadata(&p).unwrap().permissions().readonly(),
            "只读属性该被贴回去"
        );
    }

    #[test]
    fn writable_targets_pass_the_preflight() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.js");
        std::fs::write(&a, b"x").unwrap();
        ensure_writable(&[a]).unwrap();
    }

    /// 只读 ≠ 不可写：属性能摘掉就说明有权限，预检不该在这里拦人。
    #[cfg(windows)]
    #[test]
    fn a_readonly_but_owned_file_still_passes_the_preflight() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.js");
        std::fs::write(&p, b"x").unwrap();
        let mut perms = std::fs::metadata(&p).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&p, perms).unwrap();

        ensure_writable(std::slice::from_ref(&p)).unwrap();
        assert!(
            std::fs::metadata(&p).unwrap().permissions().readonly(),
            "预检不该留下副作用"
        );
    }
}
