//! 网关号池的名单：哪些号被用户**明确**放进了接力队。
//!
//! 号池里有号、Cursor 里登着号，不等于它们该被网关拿去出流量。网关背后是 Claude Code、Codex
//! 这类会自己跑很久的客户端，一个号被它悄悄用光，用户回到 IDE 才发现——所以进队必须是
//! 用户自己点的：没点过的号，[`RelayLane`](super::RelayLane) 既不列也不选。
//!
//! 名单按小写邮箱记（和 Lane 内部的键一致），落在 `settings` 表的一个 JSON 数组里；
//! 号从哪个来源来（Cursor 正登着 / 托管）不影响它在不在名单里。

use nexus_core::Result;
use nexus_store::{settings, Db};
use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

pub const SETTING_MEMBERS: &str = "gateway.members";

pub struct Roster {
    /// `None` = 只在内存里（测试、探针）。
    db: Option<Arc<Db>>,
    members: RwLock<BTreeSet<String>>,
    /// 「来源给什么就要什么」的名单：不过滤。给 ChatGPT 通道用——那些号在这个应用里只有
    /// 网关这一个用途，进不进队由账号自己的 `enabled` 开关说了算，不必再点一次「添加」。
    open: bool,
}

fn key(email: &str) -> String {
    email.trim().to_lowercase()
}

impl Roster {
    /// 从库里读出来；没有就是空名单——**默认谁也不在队里**。
    pub fn load(db: Arc<Db>) -> Self {
        let stored: Vec<String> = settings::get_or(&db, SETTING_MEMBERS, Vec::new());
        Self {
            db: Some(db),
            members: RwLock::new(
                stored
                    .iter()
                    .map(|e| key(e))
                    .filter(|e| !e.is_empty())
                    .collect(),
            ),
            open: false,
        }
    }

    /// 不落库的名单。
    pub fn in_memory<S: AsRef<str>>(members: &[S]) -> Self {
        Self {
            db: None,
            members: RwLock::new(members.iter().map(|e| key(e.as_ref())).collect()),
            open: false,
        }
    }

    /// 不过滤的名单：来源给出的候选都算在队里。
    pub fn open() -> Self {
        Self {
            db: None,
            members: RwLock::new(BTreeSet::new()),
            open: true,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn contains(&self, email: &str) -> bool {
        self.open || self.members.read().expect("roster").contains(&key(email))
    }

    pub fn is_empty(&self) -> bool {
        !self.open && self.members.read().expect("roster").is_empty()
    }

    pub fn list(&self) -> Vec<String> {
        self.members
            .read()
            .expect("roster")
            .iter()
            .cloned()
            .collect()
    }

    /// 放进名单。返回是不是新加的。
    pub fn add(&self, email: &str) -> Result<bool> {
        let k = key(email);
        if k.is_empty() {
            return Ok(false);
        }
        let mut m = self.members.write().expect("roster");
        let added = m.insert(k);
        if added {
            self.persist(&m)?;
        }
        Ok(added)
    }

    /// 移出名单。返回原来在不在。
    pub fn remove(&self, email: &str) -> Result<bool> {
        let mut m = self.members.write().expect("roster");
        let removed = m.remove(&key(email));
        if removed {
            self.persist(&m)?;
        }
        Ok(removed)
    }

    fn persist(&self, members: &BTreeSet<String>) -> Result<()> {
        if let Some(db) = &self.db {
            let list: Vec<&String> = members.iter().collect();
            settings::set(db, SETTING_MEMBERS, &list)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_empty_and_persists_across_loads() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let r = Roster::load(db.clone());
        assert!(r.is_empty(), "默认谁也不在队里");
        assert!(r.add(" A@X.com ").unwrap());
        assert!(!r.add("a@x.com").unwrap(), "同一个号第二次不算新加");
        assert!(r.contains("A@x.COM"), "按小写邮箱比");

        let again = Roster::load(db);
        assert_eq!(again.list(), vec!["a@x.com".to_string()]);
        assert!(again.remove("a@x.com").unwrap());
        assert!(!again.remove("a@x.com").unwrap());
        assert!(again.is_empty());
    }

    #[test]
    fn in_memory_roster_ignores_blank_entries_and_needs_no_db() {
        let r = Roster::in_memory(&["b@x.com", "A@x.com"]);
        assert_eq!(r.list(), vec!["a@x.com".to_string(), "b@x.com".to_string()]);
        assert!(!r.add("   ").unwrap());
        assert!(r.add("c@x.com").unwrap());
        assert!(r.contains("c@x.com"));
    }

    #[test]
    fn corrupt_setting_degrades_to_an_empty_roster() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        settings::set_raw(&db, SETTING_MEMBERS, "not json").unwrap();
        assert!(Roster::load(db).is_empty());
    }
}
