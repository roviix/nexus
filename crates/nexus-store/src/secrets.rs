//! 秘密存放。
//!
//! 一个 trait、两个实现：秘密一律落本地库 [`SqliteSecrets`]，[`MemorySecrets`] 给测试用。
//!
//! 曾经分两处放，一半在 OS 钥匙串里。钥匙串那一半已经全部去掉 —— 理由和代价都写在
//! [`SqliteSecrets`] 上。
//!
//! 留着 trait 是因为跨 crate 传的是 `Arc<dyn SecretStore>`：业务 crate 不必认识 `Db`，
//! 测试也不必为了存一条 token 搭一个库。

use crate::db::Db;
use crate::keys::SecretRef;
use nexus_core::{now_iso, AppError, ErrorCode, Result, Secret};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

pub trait SecretStore: Send + Sync + 'static {
    fn get(&self, key: &SecretRef) -> Result<Option<Secret>>;
    fn set(&self, key: &SecretRef, value: &Secret) -> Result<()>;
    fn delete(&self, key: &SecretRef) -> Result<()>;

    /// 存进去的东西能不能活过进程退出。
    ///
    /// 这不是一个可有可无的元信息：切号会**清掉** Cursor 里当前那个号的 token，此前存的
    /// 备份是它唯一的另一份。备份只在内存里的话，切一次号就等于把上一个号弄丢了。
    /// 所以有破坏性的操作必须先问这一句（见 `nexus_switcher::Switcher::switch_to`）。
    fn durable(&self) -> bool {
        true
    }

    fn exists(&self, key: &SecretRef) -> bool {
        matches!(self.get(key), Ok(Some(_)))
    }

    /// 有就返回，没有就报 `SecretMissing`。取「本该在的」秘密时用它，省掉每处 unwrap。
    fn require(&self, key: &SecretRef) -> Result<Secret> {
        self.get(key)?.ok_or_else(|| {
            AppError::new(ErrorCode::SecretMissing, format!("没有 {key} 这条秘密。"))
                .with_hint("这个凭证可能没保存成功，重新授权或重新填一次。")
        })
    }
}

/// 本地库里的 `secrets` 表 —— 所有秘密都在这儿。
///
/// **为什么不走 OS 钥匙串。** macOS 把每个钥匙串条目绑在创建它的那个二进制的代码身份
/// 上，而开发期应用是 ad-hoc 签名的 —— 每重建一次身份就变一次，于是每一条都要重新
/// 授权一遍。池子号的条目数还随号线性增长：一个号三条凭证，二十二个号六十七条，
/// 重建一次就是六十七个弹窗。这些是**可丢弃的池子号**（它们的来源本来就把同一批
/// token 明文摊在一份 JSON 里），护到那个份上不成比例。
///
/// 用户自己的 Nexus 登录态一度是例外，就一条，留在钥匙串里。后来也收了回来：为一条
/// token 养一整套降级路径（启动探活、状态字段、两处界面横幅、平台文案、一个错误码），
/// 而这条 token 最终躺在同一个库里那六十七条凭证旁边 —— 分开放挡不住任何真实攻击者，
/// 只是让代码看起来更负责。
///
/// 代价说明白：库文件从此就是凭证文件。`Db::open` 会把它和所在目录收到 0600 / 0700，
/// 但**拿得到这个文件的人就拿得到这些号，连同用户自己的 Nexus 会话** —— 这里没有加密。
/// 加一层密钥就搁在密文旁边的加密只会让这段注释好看些：那个密钥文件会跟着库一起进备份。
pub struct SqliteSecrets {
    db: Arc<Db>,
}

impl SqliteSecrets {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }
}

impl SecretStore for SqliteSecrets {
    fn get(&self, key: &SecretRef) -> Result<Option<Secret>> {
        self.db.with(|c| {
            c.query_row(
                "SELECT value FROM secrets WHERE ref = ?1",
                [key.as_str()],
                |row| row.get::<_, String>(0),
            )
            .map(|v| Some(Secret::new(v)))
            .or_else(|err| match err {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
        })
    }

    fn set(&self, key: &SecretRef, value: &Secret) -> Result<()> {
        self.db.with(|c| {
            c.execute(
                "INSERT INTO secrets (ref, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(ref) DO UPDATE SET value = ?2, updated_at = ?3",
                rusqlite::params![key.as_str(), value.expose(), now_iso()],
            )
        })?;
        Ok(())
    }

    fn delete(&self, key: &SecretRef) -> Result<()> {
        // 删一个本来就不存在的条目是成功 —— 删除要幂等，否则清理路径上到处是特判。
        self.db
            .with(|c| c.execute("DELETE FROM secrets WHERE ref = ?1", [key.as_str()]))?;
        Ok(())
    }

    /// 落在库文件里，进程退出照样在。
    fn durable(&self) -> bool {
        true
    }

    /// 走 SQL 的 `EXISTS`，不必把明文读出来再扔掉。
    fn exists(&self, key: &SecretRef) -> bool {
        self.db
            .with(|c| {
                c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM secrets WHERE ref = ?1)",
                    [key.as_str()],
                    |row| row.get::<_, i64>(0),
                )
            })
            .map(|n| n == 1)
            .unwrap_or(false)
    }
}

/// 进程内存实现。只给测试用。
#[derive(Debug, Default)]
pub struct MemorySecrets {
    inner: Mutex<HashMap<String, String>>,
    /// 谎报自己能持久化。**只给测试用**：切号一类的破坏性操作会先问 `durable()`，
    /// 不谎报的话就没法在内存存储上测那些路径了。
    claim_durable: bool,
}

impl MemorySecrets {
    /// 诚实版：`durable()` 返回 false。
    pub fn new() -> Self {
        Self::default()
    }

    /// 行为等同落盘的存储，好让破坏性路径能被测到。
    pub fn durable_for_tests() -> Self {
        Self {
            claim_durable: true,
            ..Self::default()
        }
    }

    pub fn len(&self) -> usize {
        self.inner.lock().expect("secrets lock").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl SecretStore for MemorySecrets {
    /// 退出即失。调用方据此拒绝有破坏性的操作。
    fn durable(&self) -> bool {
        self.claim_durable
    }

    fn get(&self, key: &SecretRef) -> Result<Option<Secret>> {
        Ok(self
            .inner
            .lock()
            .expect("secrets lock")
            .get(key.as_str())
            .map(Secret::new))
    }

    fn set(&self, key: &SecretRef, value: &Secret) -> Result<()> {
        self.inner
            .lock()
            .expect("secrets lock")
            .insert(key.as_str().to_string(), value.expose().to_string());
        Ok(())
    }

    fn delete(&self, key: &SecretRef) -> Result<()> {
        self.inner
            .lock()
            .expect("secrets lock")
            .remove(key.as_str());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::{account_secret, AccountSecret};
    use nexus_core::AccountId;

    #[test]
    fn round_trips_and_deletes_idempotently() {
        let store = MemorySecrets::new();
        let key = account_secret(&AccountId::from_raw("a"), AccountSecret::Refresh);

        assert!(store.get(&key).unwrap().is_none());
        assert!(!store.exists(&key));

        store.set(&key, &Secret::new("rt-123")).unwrap();
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "rt-123");
        assert!(store.exists(&key));

        store.delete(&key).unwrap();
        assert!(store.get(&key).unwrap().is_none());
        // 再删一次仍然成功。
        store.delete(&key).unwrap();
    }

    #[test]
    fn require_reports_a_missing_secret_with_a_code() {
        let store = MemorySecrets::new();
        let key = account_secret(&AccountId::from_raw("a"), AccountSecret::Access);
        let err = store.require(&key).unwrap_err();
        assert_eq!(err.code, ErrorCode::SecretMissing);
        assert!(err.hint.is_some());
    }

    #[test]
    fn the_memory_store_declares_itself_non_durable() {
        // 切号会依赖这一位来决定敢不敢动用户的登录态。
        assert!(!MemorySecrets::new().durable());
        assert!(MemorySecrets::durable_for_tests().durable());
    }

    #[test]
    fn overwriting_replaces_rather_than_appends() {
        let store = MemorySecrets::new();
        let key = account_secret(&AccountId::from_raw("a"), AccountSecret::Refresh);
        store.set(&key, &Secret::new("one")).unwrap();
        store.set(&key, &Secret::new("two")).unwrap();
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "two");
        assert_eq!(store.len(), 1);
    }

    // ── SqliteSecrets ────────────────────────────────────────────────────────

    fn sqlite_store() -> SqliteSecrets {
        SqliteSecrets::new(Arc::new(Db::open_in_memory().unwrap()))
    }

    #[test]
    fn sqlite_round_trips_and_deletes_idempotently() {
        let store = sqlite_store();
        let key = account_secret(&AccountId::from_raw("a"), AccountSecret::Refresh);

        assert!(store.get(&key).unwrap().is_none());
        assert!(!store.exists(&key));

        store.set(&key, &Secret::new("rt-123")).unwrap();
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "rt-123");
        assert!(store.exists(&key));

        store.delete(&key).unwrap();
        assert!(store.get(&key).unwrap().is_none());
        // 再删一次仍然成功。
        store.delete(&key).unwrap();
    }

    #[test]
    fn sqlite_overwrites_rather_than_erroring_on_a_duplicate_ref() {
        let store = sqlite_store();
        let key = account_secret(&AccountId::from_raw("a"), AccountSecret::Refresh);
        store.set(&key, &Secret::new("one")).unwrap();
        store.set(&key, &Secret::new("two")).unwrap();
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "two");
    }

    #[test]
    fn sqlite_secrets_outlive_the_process() {
        // durable() 说自己扛得住进程退出，这里真去关掉再打开验一次。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nexus.db");
        let key = account_secret(&AccountId::from_raw("a"), AccountSecret::Refresh);
        {
            let store = SqliteSecrets::new(Arc::new(Db::open(&path).unwrap()));
            assert!(store.durable());
            store.set(&key, &Secret::new("rt-123")).unwrap();
        }
        let store = SqliteSecrets::new(Arc::new(Db::open(&path).unwrap()));
        assert_eq!(store.get(&key).unwrap().unwrap().expose(), "rt-123");
    }

    #[cfg(unix)]
    #[test]
    fn the_database_file_is_not_world_readable() {
        // 库里现在躺着凭证，权限就是最后一道门。
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/nexus.db");
        let store = SqliteSecrets::new(Arc::new(Db::open(&path).unwrap()));
        store
            .set(
                &account_secret(&AccountId::from_raw("a"), AccountSecret::Refresh),
                &Secret::new("rt"),
            )
            .unwrap();

        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600, "库文件");
        assert_eq!(mode(path.parent().unwrap()), 0o700, "所在目录");
        // WAL 是 SQLite 自己建的，按主库文件的权限走 —— 它里面同样有凭证。
        let wal = path.with_file_name("nexus.db-wal");
        if wal.exists() {
            assert_eq!(
                mode(&wal) & 0o077,
                0,
                "WAL 不能对别人可读：{:o}",
                mode(&wal)
            );
        }
    }

    #[test]
    fn every_kind_of_secret_lands_in_the_one_store() {
        // 曾经有一类前缀会被路由去钥匙串，别的才进库。现在没有那条岔路了 ——
        // 每一类凭证都必须从同一个库里读得回来。
        let store = sqlite_store();
        for key in [
            account_secret(&AccountId::from_raw("a"), AccountSecret::Refresh),
            crate::keys::profile_auth(&nexus_core::ProfileId::from_raw("p1")),
            crate::keys::backup_auth(&nexus_core::BackupId::from_raw("b1")),
        ] {
            store.set(&key, &Secret::new("x")).unwrap();
            assert_eq!(store.get(&key).unwrap().unwrap().expose(), "x", "{key}");
        }
    }
}
