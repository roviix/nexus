//! 活动日志。
//!
//! 记「谁在什么时候动了什么」，给用户看的那份账。两个用途：切号出问题时能回溯是哪一步，
//! 以及「显示明文凭证」这类敏感动作留痕（§8）。
//!
//! **不记秘密**：邮箱可以，token 绝不。这条靠 `Secret` 不实现 `Display` 挡住大半，
//! 剩下的靠 review。

use crate::db::Db;
use nexus_core::{now_iso, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    fn parse(raw: &str) -> Self {
        match raw {
            "warn" => Level::Warn,
            "error" => Level::Error,
            _ => Level::Info,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: i64,
    pub at: String,
    pub level: Level,
    pub scope: String,
    pub email: Option<String>,
    pub message: String,
}

/// 写一条。失败只 warn 不返回错误 —— 日志写不进去不该让业务动作失败。
pub fn log(db: &Db, level: Level, scope: &str, email: Option<&str>, message: impl AsRef<str>) {
    let message = message.as_ref();
    let result = db.with(|c| {
        c.execute(
            "INSERT INTO activity (at, level, scope, email, message) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![now_iso(), level.as_str(), scope, email, message],
        )
    });
    if let Err(err) = result {
        tracing::warn!(%err, "活动日志写入失败");
    }
}

pub fn info(db: &Db, scope: &str, email: Option<&str>, message: impl AsRef<str>) {
    log(db, Level::Info, scope, email, message);
}

pub fn warn(db: &Db, scope: &str, email: Option<&str>, message: impl AsRef<str>) {
    log(db, Level::Warn, scope, email, message);
}

pub fn error(db: &Db, scope: &str, email: Option<&str>, message: impl AsRef<str>) {
    log(db, Level::Error, scope, email, message);
}

/// 最近 N 条，新的在前。
pub fn recent(db: &Db, limit: u32) -> Result<Vec<Entry>> {
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, at, level, scope, email, message FROM activity ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit], |r| {
            Ok(Entry {
                id: r.get(0)?,
                at: r.get(1)?,
                level: Level::parse(&r.get::<_, String>(2)?),
                scope: r.get(3)?,
                email: r.get(4)?,
                message: r.get(5)?,
            })
        })?;
        rows.collect()
    })
}

/// 只留最近 `keep` 条。启动时跑一次，日志不会无限长。
pub fn prune(db: &Db, keep: u32) -> Result<usize> {
    db.with(|c| {
        c.execute(
            "DELETE FROM activity WHERE id NOT IN (SELECT id FROM activity ORDER BY id DESC LIMIT ?1)",
            [keep],
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_reads_back_newest_first() {
        let db = Db::open_in_memory().unwrap();
        info(&db, "switcher", Some("a@example.com"), "已备份当前登录");
        warn(&db, "switcher", None, "Cursor 没在限时内退出");
        let rows = recent(&db, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].level, Level::Warn);
        assert_eq!(rows[1].level, Level::Info);
        assert_eq!(rows[1].email.as_deref(), Some("a@example.com"));
    }

    #[test]
    fn prune_keeps_only_the_newest() {
        let db = Db::open_in_memory().unwrap();
        for i in 0..20 {
            info(&db, "app", None, format!("事件 {i}"));
        }
        prune(&db, 5).unwrap();
        let rows = recent(&db, 100).unwrap();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].message, "事件 19");
    }

    #[test]
    fn limit_is_honoured() {
        let db = Db::open_in_memory().unwrap();
        for i in 0..10 {
            info(&db, "app", None, format!("{i}"));
        }
        assert_eq!(recent(&db, 3).unwrap().len(), 3);
    }
}
