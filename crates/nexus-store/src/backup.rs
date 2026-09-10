//! 本地备份：一个目录（默认 `~/.roviix/backups`）下的整库快照。
//!
//! 一份快照就是一个完整的 `nexus.db` 副本 —— 账号、凭证、切号本、登录态备份、设置、游乐场
//! —— 拷到另一台机器上还原就是搬家。**它是凭证文件**：目录 0700、文件 0600，界面上也要把
//! 这句话说给用户听。
//!
//! 快照与还原的机制在 [`Db::snapshot_to`] / [`Db::restore_from`]；这里只管目录、命名与清单。
//! 命名 `nexus-<UTC 时间戳>[-pre-restore].db`：按文件名排序就是按时间排序，
//! `pre-restore` 是还原之前自动留的那份 —— 撤销操作本身也得能撤销。

use crate::Db;
use nexus_core::{
    file_stamp, iso_from_system_time, AppError, Clock, ErrorCode, Result, SystemClock,
};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const PREFIX: &str = "nexus-";
const SUFFIX: &str = ".db";
const MANUAL: &str = "manual";
const PRE_RESTORE: &str = "pre-restore";

/// 清单里的一份备份。给前端的，不含任何内容 —— 只是文件的元信息。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFile {
    pub file_name: String,
    pub path: String,
    /// `manual`，或 `pre-restore`（还原前自动留的）。
    pub reason: String,
    pub created_at: String,
    pub size_bytes: u64,
}

/// 一次还原的结果：还原了哪份，以及还原前的状态被存成了哪份。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOutcome {
    pub restored: String,
    pub safety: BackupFile,
}

pub struct Backups {
    dir: PathBuf,
    /// 文件名里的时间戳从这里取。注入是为了测试能把「同一秒」钉死。
    clock: Arc<dyn Clock>,
}

impl Backups {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self::with_clock(dir, Arc::new(SystemClock))
    }

    pub fn with_clock(dir: impl Into<PathBuf>, clock: Arc<dyn Clock>) -> Self {
        Self {
            dir: dir.into(),
            clock,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 目录里所有的备份，最新的在前。目录还没建时是空表，不是错误。
    pub fn list(&self) -> Result<Vec<BackupFile>> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if parse_reason(&name).is_none() || !entry.path().is_file() {
                continue;
            }
            out.push(describe(&entry.path(), &name)?);
        }
        out.sort_by(|a, b| b.file_name.cmp(&a.file_name));
        Ok(out)
    }

    /// 现在备份一份。
    pub fn create(&self, db: &Db) -> Result<BackupFile> {
        self.create_with_reason(db, MANUAL)
    }

    fn create_with_reason(&self, db: &Db, reason: &str) -> Result<BackupFile> {
        let stamp = file_stamp(self.clock.now());
        let name = if reason == MANUAL {
            format!("{PREFIX}{stamp}{SUFFIX}")
        } else {
            format!("{PREFIX}{stamp}-{reason}{SUFFIX}")
        };
        let path = self.dir.join(&name);
        // 文件名精确到秒；SQLite 又要求目标不存在。与其在名字后面挂序号把清单弄乱，
        // 不如把这一秒说清楚。
        if path.exists() {
            return Err(AppError::new(ErrorCode::Busy, "这一秒刚刚备份过一次了。")
                .with_hint("等一秒再点。"));
        }
        db.snapshot_to(&path)?;
        describe(&path, &name)
    }

    /// 删一份备份。只认这个目录里、我们命名的文件。
    pub fn remove(&self, file_name: &str) -> Result<()> {
        let path = self.resolve(file_name)?;
        std::fs::remove_file(path)?;
        Ok(())
    }

    /// 用某份备份覆盖当前库。**先**把当前状态另存一份（`pre-restore`），再还原 ——
    /// 还原错了份也能回到还原之前。
    pub fn restore(&self, db: &Db, file_name: &str) -> Result<RestoreOutcome> {
        let path = self.resolve(file_name)?;
        let safety = self.create_with_reason(db, PRE_RESTORE)?;
        db.restore_from(&path)?;
        Ok(RestoreOutcome {
            restored: file_name.to_string(),
            safety,
        })
    }

    /// 文件名 → 目录里的路径。拒绝一切带路径成分的输入：这个接口从 IPC 进来，
    /// 「删掉 `../../nexus.db`」不该是一个可能的请求。
    fn resolve(&self, file_name: &str) -> Result<PathBuf> {
        let bad = file_name.is_empty()
            || file_name.contains('/')
            || file_name.contains('\\')
            || file_name.contains("..")
            || parse_reason(file_name).is_none();
        if bad {
            return Err(AppError::invalid(format!(
                "不是一个备份文件名：{file_name}"
            )));
        }
        let path = self.dir.join(file_name);
        if !path.is_file() {
            return Err(AppError::new(
                ErrorCode::BackupNotFound,
                format!("没有这份备份（{file_name}）。"),
            )
            .with_hint("它可能已被删除；刷新清单看看。"));
        }
        Ok(path)
    }
}

/// `nexus-<stamp>.db` → `manual`；`nexus-<stamp>-pre-restore.db` → `pre-restore`；
/// 别的形状 → `None`（不是我们的文件，清单里不显示、也不许删）。
fn parse_reason(file_name: &str) -> Option<&'static str> {
    let body = file_name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let (stamp, reason) = match body.split_once('-') {
        Some((stamp, reason)) => (stamp, Some(reason)),
        None => (body, None),
    };
    let stamp_ok = !stamp.is_empty() && stamp.chars().all(|c| c.is_ascii_alphanumeric());
    if !stamp_ok {
        return None;
    }
    match reason {
        None => Some(MANUAL),
        Some(PRE_RESTORE) => Some(PRE_RESTORE),
        Some(_) => None,
    }
}

fn describe(path: &Path, name: &str) -> Result<BackupFile> {
    let meta = std::fs::metadata(path)?;
    let created_at = meta
        .modified()
        .ok()
        .and_then(iso_from_system_time)
        .unwrap_or_default();
    Ok(BackupFile {
        file_name: name.to_string(),
        path: path.display().to_string(),
        reason: parse_reason(name).unwrap_or(MANUAL).to_string(),
        created_at,
        size_bytes: meta.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    /// 每问一次「现在」就往后走一秒：文件名精确到秒，连着两次备份才不会撞名。
    struct SteppingClock(AtomicI64);

    impl SteppingClock {
        fn new() -> Arc<Self> {
            Arc::new(Self(AtomicI64::new(1_756_900_000)))
        }
    }

    impl Clock for SteppingClock {
        fn now(&self) -> time::OffsetDateTime {
            let secs = self.0.fetch_add(1, Ordering::SeqCst);
            time::OffsetDateTime::from_unix_timestamp(secs).unwrap()
        }
    }

    /// 时间不走的钟。
    struct FrozenClock;

    impl Clock for FrozenClock {
        fn now(&self) -> time::OffsetDateTime {
            time::OffsetDateTime::from_unix_timestamp(1_756_900_000).unwrap()
        }
    }

    fn backups_in(dir: &Path) -> Backups {
        Backups::with_clock(dir.join("backups"), SteppingClock::new())
    }

    fn setting(db: &Db, key: &str) -> Option<String> {
        db.with(|c| {
            c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
                r.get(0)
            })
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
        })
        .unwrap()
    }

    fn put_setting(db: &Db, key: &str, value: &str) {
        db.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                [key, value],
            )
        })
        .unwrap();
    }

    #[test]
    fn file_names_encode_the_reason() {
        assert_eq!(parse_reason("nexus-20260903T062233Z.db"), Some("manual"));
        assert_eq!(
            parse_reason("nexus-20260903T062233Z-pre-restore.db"),
            Some("pre-restore")
        );
        assert_eq!(parse_reason("nexus-20260903T062233Z-whatever.db"), None);
        assert_eq!(parse_reason("nexus.db"), None);
        assert_eq!(parse_reason("nexus-20260903T062233Z.db.restoring"), None);
        assert_eq!(parse_reason("notes.txt"), None);
    }

    #[test]
    fn an_empty_or_missing_directory_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let backups = Backups::new(dir.path().join("does-not-exist"));
        assert!(backups.list().unwrap().is_empty());
    }

    #[test]
    fn creating_a_backup_puts_a_readable_snapshot_in_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("nexus.db")).unwrap();
        put_setting(&db, "k", "v");
        let backups = backups_in(dir.path());

        let made = backups.create(&db).unwrap();
        // 1_756_900_000 = 2025-09-03T11:46:40Z。
        assert_eq!(made.file_name, "nexus-20250903T114640Z.db");
        assert_eq!(made.reason, "manual");
        assert!(made.size_bytes > 0);
        assert!(nexus_core::clock::parse_iso(&made.created_at).is_some());
        assert!(Path::new(&made.path).starts_with(backups.dir()));

        let listed = backups.list().unwrap();
        assert_eq!(listed, vec![made.clone()]);

        // 快照本身是一份能打开的库，里面有那条设置。
        let copy = Db::open(&made.path).unwrap();
        assert_eq!(setting(&copy, "k").as_deref(), Some("v"));
    }

    #[test]
    fn two_backups_in_the_same_second_are_refused_rather_than_renamed() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("nexus.db")).unwrap();
        let backups = Backups::with_clock(dir.path().join("backups"), Arc::new(FrozenClock));
        backups.create(&db).unwrap();
        let err = backups.create(&db).unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
        assert_eq!(backups.list().unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn backups_are_only_readable_by_the_owner() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("nexus.db")).unwrap();
        let backups = backups_in(dir.path());
        let made = backups.create(&db).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(Path::new(&made.path)), 0o600, "备份就是凭证文件");
        assert_eq!(mode(backups.dir()), 0o700);
    }

    #[test]
    fn restoring_keeps_a_pre_restore_copy_of_what_was_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("nexus.db")).unwrap();
        let backups = backups_in(dir.path());

        put_setting(&db, "k", "old");
        let old = backups.create(&db).unwrap();
        put_setting(&db, "k", "new");
        let outcome = backups.restore(&db, &old.file_name).unwrap();

        assert_eq!(outcome.restored, old.file_name);
        assert_eq!(outcome.safety.reason, "pre-restore");
        assert_eq!(setting(&db, "k").as_deref(), Some("old"));

        // 还原前的状态在 safety 里，能再回去。
        backups.restore(&db, &outcome.safety.file_name).unwrap();
        assert_eq!(setting(&db, "k").as_deref(), Some("new"));

        let names: Vec<String> = backups
            .list()
            .unwrap()
            .into_iter()
            .map(|b| b.file_name)
            .collect();
        assert!(names.contains(&old.file_name));
        assert!(names.contains(&outcome.safety.file_name));
    }

    #[test]
    fn removing_only_touches_our_own_files_inside_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("nexus.db")).unwrap();
        let backups = backups_in(dir.path());
        let made = backups.create(&db).unwrap();

        for bad in [
            "../nexus.db",
            "nexus.db",
            "",
            "sub/nexus-1.db",
            "nexus-1.db.restoring",
        ] {
            let err = backups.remove(bad).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidInput, "{bad:?} 不该被接受");
        }
        assert_eq!(
            backups
                .remove("nexus-19700101T000000Z.db")
                .unwrap_err()
                .code,
            ErrorCode::BackupNotFound
        );
        // 主库还在。
        assert!(dir.path().join("nexus.db").is_file());

        backups.remove(&made.file_name).unwrap();
        assert!(backups.list().unwrap().is_empty());
    }

    #[test]
    fn foreign_files_in_the_directory_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let backups = backups_in(dir.path());
        std::fs::create_dir_all(backups.dir()).unwrap();
        std::fs::write(backups.dir().join("README.txt"), "hi").unwrap();
        std::fs::write(backups.dir().join("nexus-x.db.restoring"), "half").unwrap();
        assert!(backups.list().unwrap().is_empty());
    }
}
