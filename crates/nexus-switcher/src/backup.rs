//! 登录态备份。
//!
//! **只备 auth 那十个键**，不备整个 `state.vscdb`：本机那份库有 20+GB（会话历史都在
//! 里面），整文件备份既不现实也没必要——切号只动那十个键。
//!
//! 值进秘密存储（含明文 token），索引进 `auth_backups` 表。留最近 N 份，超出的两边一起清。

use crate::model::{AuthBackup, BackupReason};
use nexus_core::{now_iso, AppError, BackupId, ErrorCode, Result, Secret};
use nexus_cursor::AuthBundle;
use nexus_store::{keys, Db, SecretStore};
use rusqlite::Row;
use std::sync::Arc;

/// 默认保留份数。够回溯几天的切换，又不会让备份无限堆着。
pub const DEFAULT_KEEP: u32 = 30;

pub struct Backups {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
}

impl Backups {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { db, secrets }
    }

    /// 存一份。空的登录态（Cursor 从没登录过）不存 —— 存了也还原不出东西，
    /// 只会在列表里堆一堆没用的条目。返回 `None` 表示跳过。
    pub fn create(
        &self,
        auth: &AuthBundle,
        reason: BackupReason,
        keep: u32,
    ) -> Result<Option<AuthBackup>> {
        if auth.is_empty() {
            return Ok(None);
        }
        let id = BackupId::new();
        let email = auth.email();
        let created_at = now_iso();

        self.secrets.set(
            &keys::backup_auth(&id),
            &Secret::new(serde_json::to_string(auth)?),
        )?;
        self.db.with(|c| {
            c.execute(
                "INSERT INTO auth_backups (id, email, created_at, reason) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id.as_str(), &email, &created_at, reason.as_str()],
            )
        })?;

        self.prune(keep)?;
        Ok(Some(AuthBackup {
            id,
            email,
            created_at,
            reason,
        }))
    }

    pub fn list(&self) -> Result<Vec<AuthBackup>> {
        self.db.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, email, created_at, reason FROM auth_backups ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map([], row_to_backup)?;
            rows.collect()
        })
    }

    /// 取一份备份的登录态（**含明文 token**）。
    pub fn read(&self, id: &BackupId) -> Result<AuthBundle> {
        // 先确认索引在：有值但没索引行，属于清理没做干净，不该还能还原。
        let exists: bool = self.db.with(|c| {
            c.query_row(
                "SELECT 1 FROM auth_backups WHERE id = ?1",
                [id.as_str()],
                |_| Ok(()),
            )
            .map(|_| true)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(false),
                other => Err(other),
            })
        })?;
        if !exists {
            return Err(AppError::new(
                ErrorCode::BackupNotFound,
                format!("没有这份备份（{id}）。"),
            ));
        }
        let secret = self.secrets.get(&keys::backup_auth(id))?.ok_or_else(|| {
            AppError::new(
                ErrorCode::BackupNotFound,
                "这份备份只剩索引，内容已经没了。",
            )
            .with_hint("它可能已被清理，换一份更早的备份试试。")
        })?;
        Ok(serde_json::from_str(secret.expose())?)
    }

    pub fn remove(&self, id: &BackupId) -> Result<()> {
        let n = self
            .db
            .with(|c| c.execute("DELETE FROM auth_backups WHERE id = ?1", [id.as_str()]))?;
        if n == 0 {
            return Err(AppError::new(
                ErrorCode::BackupNotFound,
                format!("没有这份备份（{id}）。"),
            ));
        }
        self.secrets.delete(&keys::backup_auth(id))?;
        Ok(())
    }

    /// 只留最近 `keep` 份。
    pub fn prune(&self, keep: u32) -> Result<usize> {
        let keep = keep.max(1);
        let stale: Vec<String> = self.db.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id FROM auth_backups ORDER BY created_at DESC LIMIT -1 OFFSET ?1",
            )?;
            let rows = stmt.query_map([keep], |r| r.get::<_, String>(0))?;
            rows.collect()
        })?;
        for id in &stale {
            self.db
                .with(|c| c.execute("DELETE FROM auth_backups WHERE id = ?1", [id]))?;
            // 删秘密失败不该让整个清理失败 —— 下次还会再试到它。
            if let Err(err) = self
                .secrets
                .delete(&keys::backup_auth(&BackupId::from_raw(id)))
            {
                tracing::warn!(%err, id, "清理旧备份时删秘密失败");
            }
        }
        Ok(stale.len())
    }
}

fn row_to_backup(row: &Row<'_>) -> rusqlite::Result<AuthBackup> {
    Ok(AuthBackup {
        id: BackupId::from_raw(row.get::<_, String>(0)?),
        email: row.get(1)?,
        created_at: row.get(2)?,
        reason: BackupReason::parse(&row.get::<_, String>(3)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_store::MemorySecrets;

    fn setup() -> (Backups, Arc<MemorySecrets>) {
        let secrets = Arc::new(MemorySecrets::new());
        let db = Arc::new(Db::open_in_memory().unwrap());
        (Backups::new(db, secrets.clone()), secrets)
    }

    fn auth(email: &str) -> AuthBundle {
        let mut b = AuthBundle::new();
        b.insert("cursorAuth/accessToken", "at");
        b.insert("cursorAuth/refreshToken", "rt");
        b.insert("cursorAuth/cachedEmail", email);
        b
    }

    #[test]
    fn creates_and_reads_back() {
        let (backups, _) = setup();
        let made = backups
            .create(
                &auth("a@example.com"),
                BackupReason::PreSwitch,
                DEFAULT_KEEP,
            )
            .unwrap()
            .unwrap();
        assert_eq!(made.email.as_deref(), Some("a@example.com"));
        assert_eq!(made.reason, BackupReason::PreSwitch);

        let restored = backups.read(&made.id).unwrap();
        assert_eq!(restored.get("cursorAuth/refreshToken"), Some("rt"));
        assert_eq!(backups.list().unwrap().len(), 1);
    }

    #[test]
    fn empty_auth_is_skipped_rather_than_stored() {
        let (backups, secrets) = setup();
        assert!(backups
            .create(&AuthBundle::new(), BackupReason::PreSwitch, DEFAULT_KEEP)
            .unwrap()
            .is_none());
        assert!(backups.list().unwrap().is_empty());
        assert_eq!(secrets.len(), 0);
    }

    #[test]
    fn prune_keeps_the_newest_and_clears_their_secrets() {
        let (backups, secrets) = setup();
        for i in 0..10 {
            // created_at 用 now_iso()，同一毫秒内可能撞；显式错开保证顺序可断言。
            std::thread::sleep(std::time::Duration::from_millis(2));
            backups
                .create(
                    &auth(&format!("u{i}@example.com")),
                    BackupReason::Manual,
                    100,
                )
                .unwrap();
        }
        assert_eq!(secrets.len(), 10);

        assert_eq!(backups.prune(3).unwrap(), 7);
        let left = backups.list().unwrap();
        assert_eq!(left.len(), 3);
        assert_eq!(left[0].email.as_deref(), Some("u9@example.com"));
        assert_eq!(secrets.len(), 3, "旧备份的秘密必须一起清掉");
    }

    #[test]
    fn create_enforces_the_retention_limit() {
        let (backups, _) = setup();
        for i in 0..5 {
            std::thread::sleep(std::time::Duration::from_millis(2));
            backups
                .create(
                    &auth(&format!("u{i}@example.com")),
                    BackupReason::PreSwitch,
                    2,
                )
                .unwrap();
        }
        assert_eq!(backups.list().unwrap().len(), 2);
    }

    #[test]
    fn keep_zero_still_retains_one() {
        let (backups, _) = setup();
        backups
            .create(&auth("a@example.com"), BackupReason::Manual, 0)
            .unwrap();
        assert_eq!(backups.list().unwrap().len(), 1, "再抠也得留住刚存的那份");
    }

    #[test]
    fn missing_backup_reports_backup_not_found() {
        let (backups, _) = setup();
        let ghost = BackupId::from_raw("nope");
        assert_eq!(
            backups.read(&ghost).unwrap_err().code,
            ErrorCode::BackupNotFound
        );
        assert_eq!(
            backups.remove(&ghost).unwrap_err().code,
            ErrorCode::BackupNotFound
        );
    }

    #[test]
    fn an_index_without_its_secret_is_reported_not_silently_empty() {
        let (backups, secrets) = setup();
        let made = backups
            .create(&auth("a@example.com"), BackupReason::Manual, DEFAULT_KEEP)
            .unwrap()
            .unwrap();
        secrets.delete(&keys::backup_auth(&made.id)).unwrap();
        let err = backups.read(&made.id).unwrap_err();
        assert_eq!(err.code, ErrorCode::BackupNotFound);
        assert!(err.hint.is_some());
    }

    #[test]
    fn remove_deletes_both_sides() {
        let (backups, secrets) = setup();
        let made = backups
            .create(&auth("a@example.com"), BackupReason::Manual, DEFAULT_KEEP)
            .unwrap()
            .unwrap();
        backups.remove(&made.id).unwrap();
        assert!(backups.list().unwrap().is_empty());
        assert_eq!(secrets.len(), 0);
    }
}
