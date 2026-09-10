//! 设置：一张 key/value 表。
//!
//! 值一律是 JSON 文本，所以加一个带结构的设置项不需要迁移。桌面端的设置就那么几条
//! （切号时是否同时切机器码、备份保留份数、Cursor 目录…），一张表足够。

use crate::db::Db;
use nexus_core::Result;
use serde::{de::DeserializeOwned, Serialize};

/// 切号时是否连机器码一起切。**默认关**（R6 一机一码，避免 too many computers）；
/// 打开则强制冷切并换指纹。万一某个环境必须隔离设备指纹，用户可以打开（§11）。
pub const SWITCH_MACHINE_IDS: &str = "switcher.switch_machine_ids";
/// 备份保留份数。
pub const BACKUP_KEEP: &str = "switcher.backup_keep";
/// 用户手动指定的 Cursor 用户目录（自动探测失败时）。
pub const CURSOR_USER_DIR: &str = "cursor.user_dir";
/// 用户手动指定的 Cursor **安装**目录。
///
/// 和上面那条是两件事：用户目录放登录态（切号读写它），安装目录放程序本体
/// （启动 Cursor 和 Sand 补丁要它）。Windows 上安装目录可以在任意盘符，探测更容易落空，
/// 所以它必须能单独指定。
pub const CURSOR_APP_DIR: &str = "cursor.app_dir";

pub fn get_raw(db: &Db, key: &str) -> Result<Option<String>> {
    db.with(|c| {
        c.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| {
            r.get(0)
        })
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
    })
}

pub fn set_raw(db: &Db, key: &str, value: &str) -> Result<()> {
    db.with(|c| {
        c.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )
    })?;
    Ok(())
}

/// 读一个带类型的设置。**解析不了就返回默认值**而不是报错：一条设置坏掉不该让
/// 整个界面打不开。
pub fn get_or<T: DeserializeOwned>(db: &Db, key: &str, fallback: T) -> T {
    match get_raw(db, key) {
        Ok(Some(raw)) => serde_json::from_str(&raw).unwrap_or_else(|err| {
            tracing::warn!(key, %err, "设置项解析失败，回退到默认值");
            fallback
        }),
        _ => fallback,
    }
}

pub fn set<T: Serialize>(db: &Db, key: &str, value: &T) -> Result<()> {
    set_raw(db, key, &serde_json::to_string(value)?)
}

pub fn remove(db: &Db, key: &str) -> Result<()> {
    db.with(|c| c.execute("DELETE FROM settings WHERE key = ?1", [key]))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_key_falls_back() {
        let db = Db::open_in_memory().unwrap();
        assert!(get_raw(&db, "nope").unwrap().is_none());
        assert!(!get_or(&db, SWITCH_MACHINE_IDS, false));
        assert_eq!(get_or(&db, BACKUP_KEEP, 30u32), 30);
    }

    #[test]
    fn set_then_get_round_trips_typed_values() {
        let db = Db::open_in_memory().unwrap();
        set(&db, SWITCH_MACHINE_IDS, &false).unwrap();
        assert!(!get_or(&db, SWITCH_MACHINE_IDS, true));
        set(&db, BACKUP_KEEP, &5u32).unwrap();
        assert_eq!(get_or(&db, BACKUP_KEEP, 30u32), 5);
        set(&db, CURSOR_USER_DIR, &"/tmp/cursor").unwrap();
        assert_eq!(get_or(&db, CURSOR_USER_DIR, String::new()), "/tmp/cursor");
    }

    #[test]
    fn set_overwrites_rather_than_conflicting() {
        let db = Db::open_in_memory().unwrap();
        set_raw(&db, "k", "1").unwrap();
        set_raw(&db, "k", "2").unwrap();
        assert_eq!(get_raw(&db, "k").unwrap().unwrap(), "2");
    }

    #[test]
    fn corrupt_value_degrades_to_default_instead_of_failing() {
        let db = Db::open_in_memory().unwrap();
        set_raw(&db, BACKUP_KEEP, "not json").unwrap();
        assert_eq!(get_or(&db, BACKUP_KEEP, 30u32), 30);
    }

    #[test]
    fn remove_deletes() {
        let db = Db::open_in_memory().unwrap();
        set_raw(&db, "k", "1").unwrap();
        remove(&db, "k").unwrap();
        assert!(get_raw(&db, "k").unwrap().is_none());
    }
}
