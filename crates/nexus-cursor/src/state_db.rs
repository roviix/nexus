//! 读写 Cursor 的登录态库 `state.vscdb`。
//!
//! Cursor 把当前登录凭证存在 SQLite 的 `ItemTable(key,value)` 里；切号 = 把某个号的
//! `cursorAuth/*` 写进去再重启。本机实测（Cursor 3.18.9）：
//!
//! ```text
//! CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
//! journal_mode = wal;  auth 值全是 text
//! ```
//!
//! 这个文件动的是用户**正在用的**登录态，所以处处往「不弄坏」做：
//!   - 只写白名单里的键，库里其它几万个键一个不碰；
//!   - 参数化写入，token 里的特殊字符不会拼坏 SQL；
//!   - 清旧 + 写新在同一事务里，不存在「已清未写」的未登录中间态；
//!   - 写库要求 Cursor 已退出（它在跑会用内存状态覆盖回来）——由 `nexus-switcher` 保证。

use nexus_core::{AppError, ErrorCode, Result};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 切换要覆盖的键。**顺序即写入顺序，内容即白名单。**
///
/// 缺 membership / signupType 那几个，Cursor 界面会显示上一个号的订阅档 —— 这不是
/// 美观问题，是用户会据此以为切错了号。
pub const AUTH_KEYS: [&str; 10] = [
    "cursorAuth/accessToken",
    "cursorAuth/refreshToken",
    "cursorAuth/cachedEmail",
    "cursorAuth/cachedSignUpType",
    "cursorAuth/stripeMembershipType",
    "cursorAuth/stripeSubscriptionStatus",
    "cursorAuth/stripeMembershipAuthId",
    "cursorAuth/cachedScopedProfile",
    "cursorAuth/cachedUserId",
    "cursorAuth/onboardingDate",
];

/// 没有这两个就切不进去 —— 其余键缺了只是显示不全。
pub const REQUIRED_KEYS: [&str; 2] = ["cursorAuth/accessToken", "cursorAuth/refreshToken"];

/// 热切之后要**补写**的那几个键：账号在界面上「叫什么、什么档」的缓存。
///
/// 热切走的是 Cursor 自己的 `cursor://cursorAuth?route=login` 深链。读 3.19.7 的
/// `workbench.desktop.main.js`（`handleAuth` → `storeAccessRefreshToken` → `refreshMembership`）
/// 可以确认：这条路只写 `accessToken` / `refreshToken` 和 `stripeMembershipType` /
/// `stripeSubscriptionStatus`，**不碰** `cachedEmail` / `cachedSignUpType` / `cachedScopedProfile`；而
/// 界面读邮箱的 `getEmailAndSignUpType()` 是「缓存里有就直接返回，不再向服务端要」，显示名的
/// `getCachedAccountProfile()` 同样只读缓存。于是 token 已经是新号、菜单里的名字还是旧号——
/// 它们只在登出（`logout` → `clearStoredEmailAndSignUpType` / `clearCachedScopedAccount`）时才被清。
///
/// 所以热切确认 token 落盘后，我们把这几把键**按目标号**写一遍：目标号有值就写值；没有的
/// （比如收录时还没有 `cachedScopedProfile`）就**删掉**——删掉之后 Cursor 那边的 getter
/// 命中 miss，会自己去服务端拉一份新的。这两种都比留着上一个号的值正确。
///
/// `accessToken` / `refreshToken` 不在这里：那两把是 Cursor 自己刚写的、内存里也是新的，我们再
/// 写一遍没意义；机器码更不碰（热切的前提就是不换指纹）。
pub const DISPLAY_KEYS: [&str; 6] = [
    "cursorAuth/cachedEmail",
    "cursorAuth/cachedSignUpType",
    "cursorAuth/stripeMembershipType",
    "cursorAuth/stripeSubscriptionStatus",
    "cursorAuth/stripeMembershipAuthId",
    "cursorAuth/cachedScopedProfile",
];

const KEY_EMAIL: &str = "cursorAuth/cachedEmail";
const KEY_MEMBERSHIP: &str = "cursorAuth/stripeMembershipType";
const KEY_SIGNUP: &str = "cursorAuth/cachedSignUpType";
const KEY_SUBSCRIPTION: &str = "cursorAuth/stripeSubscriptionStatus";
const KEY_ACCESS: &str = "cursorAuth/accessToken";
const KEY_REFRESH: &str = "cursorAuth/refreshToken";

/// 一整套 `cursorAuth/*` 键值。**含 token 明文。**
///
/// 它必须能序列化 —— 整包 JSON 就是交给 `SecretStore` 的那个值。所以规矩是：
/// **只能写进 `SecretStore`，不许进业务表、日志、IPC 或 Tauri 事件。** 给出去的一律是
/// `summary()`。`Debug` 已经打码，随手 log 一下不会漏。
#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct AuthBundle(BTreeMap<String, String>);

impl AuthBundle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).map(String::as_str)
    }

    /// 只接受白名单内的键，空值当作「没有」。挡住上游脏数据顺着切号写进用户的库。
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<String>) -> &mut Self {
        let (key, value) = (key.into(), value.into());
        if AUTH_KEYS.contains(&key.as_str()) && !value.is_empty() {
            self.0.insert(key, value);
        }
        self
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn email(&self) -> Option<String> {
        self.get(KEY_EMAIL).map(|e| e.trim().to_ascii_lowercase())
    }

    pub fn access_token(&self) -> Option<&str> {
        self.get(KEY_ACCESS)
    }

    pub fn refresh_token(&self) -> Option<&str> {
        self.get(KEY_REFRESH)
    }

    /// 能不能拿它切号。
    pub fn is_switchable(&self) -> bool {
        REQUIRED_KEYS
            .iter()
            .all(|k| self.get(k).is_some_and(|v| !v.is_empty()))
    }

    /// 不含秘密的视图。**所有出口都走它。**
    pub fn summary(&self) -> AuthSummary {
        AuthSummary {
            email: self.email(),
            membership: self.get(KEY_MEMBERSHIP).map(str::to_string),
            signup_type: self.get(KEY_SIGNUP).map(str::to_string),
            subscription_status: self.get(KEY_SUBSCRIPTION).map(str::to_string),
            has_access_token: self.get(KEY_ACCESS).is_some(),
            has_refresh_token: self.get(KEY_REFRESH).is_some(),
            key_count: self.0.len(),
        }
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }
}

impl std::fmt::Debug for AuthBundle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AuthBundle({} keys, email={:?}, values hidden)",
            self.0.len(),
            self.email()
        )
    }
}

/// 登录态的公开视图：能显示的都在这儿，秘密一个没有。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AuthSummary {
    pub email: Option<String>,
    pub membership: Option<String>,
    pub signup_type: Option<String>,
    pub subscription_status: Option<String>,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
    pub key_count: usize,
}

/// 启动自检的结果（§5.2「键名漂移是唯一真实风险」）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaCheck {
    /// 库在不在。
    pub db_present: bool,
    /// `ItemTable` 在不在。
    pub table_present: bool,
    /// 读到了哪些 auth 键。
    pub present_keys: Vec<String>,
    /// 预期有、实际没有的键。
    pub missing_keys: Vec<String>,
    /// Cursor 版本，用于「未在此版本验证过」的提示。
    pub cursor_version: Option<String>,
}

impl SchemaCheck {
    /// Cursor 现在登着号吗。
    pub fn logged_in(&self) -> bool {
        !self.present_keys.is_empty()
    }

    /// 能不能安全地写。
    ///
    /// 关键的区分：**「一个 auth 键都没有」和「有几个但缺了必需的」是两回事。**
    ///   - 一个都没有 = Cursor 从没登录过（新装的机器，或用户登出了）。往里写正是
    ///     我们要做的事，不该拦 —— 拦了的话「买号 → 切入」在干净机器上根本走不通（§2.4）。
    ///   - 有几个、却缺了必需的 = 键名可能变了。这才是该降级只读的情形（§11）。
    ///
    /// 漏网之鱼是「Cursor 把十个键全改了名」：那时 `present_keys` 也是空的，我们会照旧
    /// 写老键名。写进去是无害的（Cursor 忽略不认识的键），用户会看到它依然未登录
    /// ——比一上来就把功能锁死好。
    pub fn writable(&self) -> bool {
        if !self.db_present || !self.table_present {
            return false;
        }
        if !self.logged_in() {
            return true;
        }
        REQUIRED_KEYS
            .iter()
            .all(|k| self.present_keys.iter().any(|p| p == k))
    }

    /// 不通过时给用户的一句话。
    pub fn explain(&self) -> Option<String> {
        if self.writable() {
            return None;
        }
        if !self.db_present {
            return Some("没找到 Cursor 的登录态库，切号功能不可用。".to_string());
        }
        if !self.table_present {
            return Some("Cursor 的登录态库里没有 ItemTable，结构与预期不符。".to_string());
        }
        Some(format!(
            "Cursor 的登录态键名与预期不符（缺 {}）。它可能升级后改了存储结构；\
             切号已降级为只读，避免写坏你的登录态。",
            self.missing_keys.join("、")
        ))
    }
}

/// 一个 `state.vscdb` 的句柄。只持路径，每次操作现开现关 —— 长期持连接会在
/// Cursor 启动时和它抢 WAL 锁。
#[derive(Debug, Clone)]
pub struct StateDb {
    path: PathBuf,
}

impl StateDb {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.path.is_file()
    }

    fn open(&self, read_only: bool) -> Result<Connection> {
        if !self.exists() {
            return Err(AppError::new(
                ErrorCode::CursorNotFound,
                format!("没找到 Cursor 状态库：{}", self.path.display()),
            )
            .with_hint("确认 Cursor 已安装并至少登录过一次。"));
        }
        let flags = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        let conn = Connection::open_with_flags(&self.path, flags).map_err(|err| {
            AppError::new(
                ErrorCode::Database,
                format!("打开 Cursor 状态库失败：{err}"),
            )
            .with_hint("如果 Cursor 正在运行，先退出它再试。")
        })?;
        // Cursor 的 helper 进程退出后还会攥着 WAL 锁一小会儿。默认行为是立刻返回
        // SQLITE_BUSY，那会让切号在最后一步毫无必要地失败；等两秒足够它放手。
        let _ = conn.busy_timeout(std::time::Duration::from_secs(2));
        Ok(conn)
    }

    /// 读整套 auth 键（**含明文 token**）。备份、以及「收录当前登录的号」用它。
    pub fn read_auth(&self) -> Result<AuthBundle> {
        let conn = self.open(true)?;
        let mut stmt = conn
            .prepare("SELECT value FROM ItemTable WHERE key = ?1")
            .map_err(|err| schema_error(err, &self.path))?;
        let mut bundle = AuthBundle::new();
        for key in AUTH_KEYS {
            let value: Option<String> = stmt
                .query_row([key], |row| row.get::<_, Option<String>>(0))
                .or_else(|err| match err {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
                .map_err(|err| schema_error(err, &self.path))?;
            if let Some(v) = value {
                bundle.insert(key, v);
            }
        }
        Ok(bundle)
    }

    /// 当前登录的号，只要能公开的部分。没登录返回 `None`。
    pub fn current_account(&self) -> Result<Option<AuthSummary>> {
        let bundle = self.read_auth()?;
        Ok(bundle.email().map(|_| bundle.summary()))
    }

    /// 自检：结构和键都在不在。
    pub fn check(&self, cursor_version: Option<String>) -> SchemaCheck {
        let mut check = SchemaCheck {
            db_present: self.exists(),
            table_present: false,
            present_keys: Vec::new(),
            missing_keys: AUTH_KEYS.iter().map(|k| k.to_string()).collect(),
            cursor_version,
        };
        if !check.db_present {
            return check;
        }
        let Ok(conn) = self.open(true) else {
            return check;
        };
        check.table_present = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type='table' AND name='ItemTable'",
                [],
                |_| Ok(()),
            )
            .is_ok();
        if !check.table_present {
            return check;
        }
        if let Ok(bundle) = self.read_auth() {
            check.present_keys = bundle.keys().map(str::to_string).collect();
            check.missing_keys = AUTH_KEYS
                .iter()
                .filter(|k| !check.present_keys.iter().any(|p| p == *k))
                .map(|k| k.to_string())
                .collect();
        }
        check
    }

    /// 把一套 auth 键写进去。**调用方必须先确保 Cursor 已退出。**
    ///
    /// `clear_first`：先删掉白名单里的全部键再写。切号必须开 —— 目标号没有的字段
    /// 不该残留上一个号的值。清和写在**同一事务**里：中途进程被杀，库要么是切换前、
    /// 要么是切换后，不存在未登录的中间态（§5.1）。
    pub fn write_auth(&self, bundle: &AuthBundle, clear_first: bool) -> Result<usize> {
        if bundle.is_empty() {
            return Err(AppError::new(
                ErrorCode::ProfileIncomplete,
                "没有可写入的登录态键。",
            ));
        }
        let mut conn = self.open(false)?;
        let tx = conn
            .transaction()
            .map_err(|err| schema_error(err, &self.path))?;
        {
            if clear_first {
                let mut del = tx
                    .prepare("DELETE FROM ItemTable WHERE key = ?1")
                    .map_err(|err| schema_error(err, &self.path))?;
                for key in AUTH_KEYS {
                    del.execute([key])
                        .map_err(|err| schema_error(err, &self.path))?;
                }
            }
            // schema 自带 UNIQUE ON CONFLICT REPLACE，INSERT OR REPLACE 正对上它的语义。
            let mut up = tx
                .prepare("INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?1, ?2)")
                .map_err(|err| schema_error(err, &self.path))?;
            for key in AUTH_KEYS {
                if let Some(value) = bundle.get(key) {
                    up.execute(rusqlite::params![key, value])
                        .map_err(|err| schema_error(err, &self.path))?;
                }
            }
        }
        tx.commit().map_err(|err| schema_error(err, &self.path))?;
        Ok(bundle.len())
    }

    /// 热切收尾：把目标号的展示键写进去（见 [`DISPLAY_KEYS`]）。
    ///
    /// 目标号有的键写值，没有的键**删掉**，同一事务。不清也不写 token / 机器码。
    /// Cursor 在跑也可以调：这些键 Cursor 自己只在读的时候查一次缓存，不会用内存状态覆盖回来
    /// （它内存里根本没有这几把键的副本——`getEmailAndSignUpType` 每次都是 `storageService.get`）。
    /// 返回写了几把、删了几把。
    pub fn write_display_keys(&self, bundle: &AuthBundle) -> Result<(usize, usize)> {
        let mut conn = self.open(false)?;
        let tx = conn
            .transaction()
            .map_err(|err| schema_error(err, &self.path))?;
        let mut written = 0usize;
        let mut removed = 0usize;
        {
            let mut up = tx
                .prepare("INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?1, ?2)")
                .map_err(|err| schema_error(err, &self.path))?;
            let mut del = tx
                .prepare("DELETE FROM ItemTable WHERE key = ?1")
                .map_err(|err| schema_error(err, &self.path))?;
            for key in DISPLAY_KEYS {
                match bundle.get(key) {
                    Some(value) => {
                        up.execute(rusqlite::params![key, value])
                            .map_err(|err| schema_error(err, &self.path))?;
                        written += 1;
                    }
                    None => {
                        removed += del
                            .execute([key])
                            .map_err(|err| schema_error(err, &self.path))?;
                    }
                }
            }
        }
        tx.commit().map_err(|err| schema_error(err, &self.path))?;
        Ok((written, removed))
    }

    /// 清掉 auth 键 = 回到未登录态。
    pub fn clear_auth(&self) -> Result<()> {
        let mut conn = self.open(false)?;
        let tx = conn
            .transaction()
            .map_err(|err| schema_error(err, &self.path))?;
        {
            let mut del = tx
                .prepare("DELETE FROM ItemTable WHERE key = ?1")
                .map_err(|err| schema_error(err, &self.path))?;
            for key in AUTH_KEYS {
                del.execute([key])
                    .map_err(|err| schema_error(err, &self.path))?;
            }
        }
        tx.commit().map_err(|err| schema_error(err, &self.path))
    }
}

/// 库结构不对时给一个能行动的错误，而不是把 sqlite 的原文丢给用户。
fn schema_error(err: rusqlite::Error, path: &Path) -> AppError {
    let text = err.to_string();
    if text.contains("no such table") || text.contains("no such column") {
        return AppError::new(
            ErrorCode::CursorSchemaDrift,
            format!("Cursor 状态库的结构与预期不符：{text}"),
        )
        .with_hint("Cursor 可能升级后改了存储结构。切号功能已停用，等待适配。");
    }
    if text.contains("locked") || text.contains("busy") {
        return AppError::new(ErrorCode::CursorRunning, "Cursor 状态库被占用。")
            .with_hint("先完全退出 Cursor 再重试。");
    }
    AppError::new(
        ErrorCode::Database,
        format!("读写 Cursor 状态库失败（{}）：{text}", path.display()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 造一个和真实 Cursor 库同构的临时库（schema 取自本机 3.18.9 实测）。
    fn fixture() -> (tempfile::TempDir, StateDb) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.vscdb");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);
             PRAGMA journal_mode=WAL;",
        )
        .unwrap();
        // 库里还有几万个与登录无关的键，放一个进去，验证我们不碰它。
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES ('workbench.panel.state', 'keep-me')",
            [],
        )
        .unwrap();
        drop(conn);
        (dir, StateDb::new(path))
    }

    fn full_bundle(email: &str) -> AuthBundle {
        let mut b = AuthBundle::new();
        for key in AUTH_KEYS {
            b.insert(key, format!("{key}-value"));
        }
        b.insert("cursorAuth/cachedEmail", email);
        b
    }

    #[test]
    fn writes_and_reads_back_every_whitelisted_key() {
        let (_dir, db) = fixture();
        let bundle = full_bundle("a@example.com");
        assert_eq!(db.write_auth(&bundle, true).unwrap(), AUTH_KEYS.len());
        let read = db.read_auth().unwrap();
        assert_eq!(read, bundle);
        assert_eq!(read.email().unwrap(), "a@example.com");
    }

    #[test]
    fn leaves_unrelated_keys_untouched() {
        let (_dir, db) = fixture();
        db.write_auth(&full_bundle("a@example.com"), true).unwrap();
        let conn = Connection::open(db.path()).unwrap();
        let kept: String = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key = 'workbench.panel.state'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, "keep-me");
    }

    #[test]
    fn clear_first_removes_fields_the_new_account_lacks() {
        let (_dir, db) = fixture();
        db.write_auth(&full_bundle("old@example.com"), true)
            .unwrap();

        // 新号只有必需的两个键 + 邮箱。旧号的订阅档必须消失，否则界面会显示上一个号的档位。
        let mut lean = AuthBundle::new();
        lean.insert("cursorAuth/accessToken", "new-access");
        lean.insert("cursorAuth/refreshToken", "new-refresh");
        lean.insert("cursorAuth/cachedEmail", "new@example.com");
        db.write_auth(&lean, true).unwrap();

        let read = db.read_auth().unwrap();
        assert_eq!(read.len(), 3);
        assert_eq!(read.email().unwrap(), "new@example.com");
        assert!(read.get("cursorAuth/stripeMembershipType").is_none());
    }

    #[test]
    fn without_clear_first_old_fields_survive() {
        let (_dir, db) = fixture();
        db.write_auth(&full_bundle("old@example.com"), true)
            .unwrap();
        let mut lean = AuthBundle::new();
        lean.insert("cursorAuth/cachedEmail", "new@example.com");
        db.write_auth(&lean, false).unwrap();
        let read = db.read_auth().unwrap();
        assert_eq!(read.email().unwrap(), "new@example.com");
        assert!(read.get("cursorAuth/stripeMembershipType").is_some());
    }

    #[test]
    fn bundle_rejects_keys_outside_the_whitelist() {
        let mut b = AuthBundle::new();
        b.insert("cursorAuth/accessToken", "ok");
        b.insert("evil/key", "nope");
        b.insert("cursorAuth/refreshToken", ""); // 空值当没有
        assert_eq!(b.len(), 1);
        assert!(b.get("evil/key").is_none());
    }

    #[test]
    fn debug_never_prints_token_values() {
        let b = full_bundle("a@example.com");
        let printed = format!("{b:?}");
        assert!(printed.contains("10 keys"));
        assert!(!printed.contains("accessToken-value"));
    }

    #[test]
    fn summary_carries_no_secrets() {
        let mut b = full_bundle("a@example.com");
        b.insert("cursorAuth/accessToken", "SECRET-ACCESS");
        b.insert("cursorAuth/refreshToken", "SECRET-REFRESH");
        let s = b.summary();
        let json = serde_json::to_string(&s).unwrap();

        // 订阅档、注册方式这些是要显示的，本来就该在摘要里。
        assert!(json.contains("a@example.com"));
        assert!(s.membership.is_some() && s.signup_type.is_some());
        // token 只能以「有没有」的形式出现，绝不能带值。
        assert!(!json.contains("SECRET"), "摘要里漏了 token：{json}");
        assert!(s.has_access_token && s.has_refresh_token);
    }

    #[test]
    fn switchable_requires_both_tokens() {
        let mut b = AuthBundle::new();
        b.insert("cursorAuth/cachedEmail", "a@example.com");
        assert!(!b.is_switchable());
        b.insert("cursorAuth/accessToken", "x");
        assert!(!b.is_switchable());
        b.insert("cursorAuth/refreshToken", "y");
        assert!(b.is_switchable());
    }

    #[test]
    fn empty_bundle_is_refused_rather_than_wiping_the_login() {
        let (_dir, db) = fixture();
        db.write_auth(&full_bundle("a@example.com"), true).unwrap();
        let err = db.write_auth(&AuthBundle::new(), true).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProfileIncomplete);
        // 原来的登录还在。
        assert_eq!(db.read_auth().unwrap().len(), AUTH_KEYS.len());
    }

    /// 热切收尾：展示键按目标号覆盖，目标号没有的删掉；token 和无关键一个不碰。
    #[test]
    fn write_display_keys_replaces_names_removes_missing_and_leaves_tokens_alone() {
        let (_dir, db) = fixture();
        // 盘上是 old 号的全套（含 scoped profile），token 已经被 Cursor 深链换成 new 号的。
        let mut on_disk = full_bundle("old@example.com");
        on_disk.insert("cursorAuth/accessToken", "new-access");
        on_disk.insert("cursorAuth/refreshToken", "new-refresh");
        db.write_auth(&on_disk, true).unwrap();

        // 目标号收录时只有邮箱 + 档位，没有 scoped profile / signup type。
        let mut target = AuthBundle::new();
        target.insert("cursorAuth/accessToken", "new-access");
        target.insert("cursorAuth/refreshToken", "new-refresh");
        target.insert("cursorAuth/cachedEmail", "new@example.com");
        target.insert("cursorAuth/stripeMembershipType", "pro");

        let (written, removed) = db.write_display_keys(&target).unwrap();
        assert_eq!(written, 2);
        // 6 把展示键里 4 把目标号没有：signUpType / subscriptionStatus / membershipAuthId / scopedProfile。
        assert_eq!(removed, 4);

        let read = db.read_auth().unwrap();
        assert_eq!(read.email().unwrap(), "new@example.com");
        assert_eq!(read.get("cursorAuth/stripeMembershipType"), Some("pro"));
        assert_eq!(
            read.get("cursorAuth/cachedScopedProfile"),
            None,
            "旧号的显示名必须被清掉"
        );
        assert_eq!(read.get("cursorAuth/cachedSignUpType"), None);
        // token 原样；不属于展示键的其它 auth 键（onboardingDate / cachedUserId）也原样。
        assert_eq!(read.access_token(), Some("new-access"));
        assert_eq!(read.refresh_token(), Some("new-refresh"));
        assert_eq!(
            read.get("cursorAuth/onboardingDate"),
            Some("cursorAuth/onboardingDate-value")
        );
        let conn = Connection::open(db.path()).unwrap();
        let kept: String = conn
            .query_row(
                "SELECT value FROM ItemTable WHERE key = 'workbench.panel.state'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kept, "keep-me");
    }

    #[test]
    fn check_passes_on_a_logged_in_database() {
        let (_dir, db) = fixture();
        db.write_auth(&full_bundle("a@example.com"), true).unwrap();
        let check = db.check(Some("3.18.9".into()));
        assert!(check.db_present && check.table_present);
        assert!(check.writable());
        assert!(check.missing_keys.is_empty());
        assert!(check.explain().is_none());
    }

    #[test]
    fn a_logged_out_cursor_is_writable_not_degraded() {
        // 干净机器上 Cursor 从没登录过：一个 auth 键都没有。这时**必须**能写，
        // 否则「买号 → 切入使用」在新机器上根本走不通（§2.4）。
        let (_dir, db) = fixture();
        let check = db.check(None);
        assert!(check.db_present && check.table_present);
        assert!(!check.logged_in());
        assert!(check.writable(), "没登录不等于结构不对，不该拦着不让写");
        assert!(check.explain().is_none());
    }

    #[test]
    fn a_renamed_key_degrades_to_read_only() {
        // 有几个 auth 键、却缺了必需的 —— 这才是键名漂移，该停手（§11）。
        let (_dir, db) = fixture();
        let mut partial = AuthBundle::new();
        partial.insert("cursorAuth/cachedEmail", "a@example.com");
        partial.insert("cursorAuth/stripeMembershipType", "ultra");
        db.write_auth(&partial, false).unwrap();

        let check = db.check(None);
        assert!(check.logged_in());
        assert!(!check.writable());
        assert!(check.explain().unwrap().contains("降级为只读"));
    }

    #[test]
    fn check_reports_a_missing_database() {
        let db = StateDb::new("/tmp/definitely-not-here-9f3a/state.vscdb");
        let check = db.check(None);
        assert!(!check.db_present);
        assert!(!check.writable());
        assert!(check.explain().unwrap().contains("没找到"));
    }

    #[test]
    fn check_reports_a_renamed_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.vscdb");
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE SomethingElse (k TEXT, v BLOB);")
            .unwrap();
        let check = StateDb::new(path).check(None);
        assert!(check.db_present);
        assert!(!check.table_present);
        assert!(check.explain().unwrap().contains("ItemTable"));
    }

    #[test]
    fn clear_auth_leaves_no_login_but_keeps_other_keys() {
        let (_dir, db) = fixture();
        db.write_auth(&full_bundle("a@example.com"), true).unwrap();
        db.clear_auth().unwrap();
        assert!(db.read_auth().unwrap().is_empty());
        assert!(db.current_account().unwrap().is_none());
        let conn = Connection::open(db.path()).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM ItemTable", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1, "无关键必须还在");
    }

    #[test]
    fn tokens_with_sql_metacharacters_round_trip() {
        let (_dir, db) = fixture();
        let nasty = "a'b\"c;--\n\0x DROP TABLE ItemTable; %s";
        let mut b = AuthBundle::new();
        b.insert("cursorAuth/accessToken", nasty);
        b.insert("cursorAuth/refreshToken", "r");
        b.insert("cursorAuth/cachedEmail", "a@example.com");
        db.write_auth(&b, true).unwrap();
        assert_eq!(
            db.read_auth()
                .unwrap()
                .get("cursorAuth/accessToken")
                .unwrap(),
            nasty
        );
    }

    #[test]
    fn missing_database_reports_cursor_not_found() {
        let db = StateDb::new("/tmp/definitely-not-here-9f3a/state.vscdb");
        assert_eq!(db.read_auth().unwrap_err().code, ErrorCode::CursorNotFound);
    }
}
