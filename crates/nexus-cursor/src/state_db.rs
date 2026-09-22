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
pub const AUTH_KEYS: [&str; 11] = [
    "cursorAuth/accessToken",
    "cursorAuth/refreshToken",
    "cursorAuth/cachedEmail",
    "cursorAuth/cachedSignUpType",
    "cursorAuth/stripeMembershipType",
    "cursorAuth/stripeSubscriptionStatus",
    "cursorAuth/stripeMembershipAuthId",
    "cursorAuth/cachedScopedProfile",
    "cursorAuth/cachedTeam",
    "cursorAuth/cachedUserId",
    "cursorAuth/onboardingDate",
];

/// 没有这两个就切不进去 —— 其余键缺了只是显示不全。
pub const REQUIRED_KEYS: [&str; 2] = ["cursorAuth/accessToken", "cursorAuth/refreshToken"];

/// 「此刻真的登着号」只能由这几把键回答。
///
/// 这一条是 Windows 上踩出来的。`cursorAuth/*` 不是一起生灭的：用户登出之后，
/// `stripeMembershipType` / `cachedSignUpType` / `onboardingDate` 会**留在库里**，
/// 而 token 和邮箱被清掉。于是「库里还有 auth 键」被当成了「登着号」，再一看必需的
/// token 不在 —— 判成键名漂移，切号降级只读。
///
/// `cachedUserId` **不在这张表里**。Cursor 3.21.13 的 bundle 已经不再读写它，
/// `logout` 也不清它；升级过的机器上它会作为旧版本的残留单独留下。把它当成登录证据，
/// 就是把一台已经登出的机器说成「登着号」。
///
/// **这张表只是启发式，不是拦不拦的依据**（见 [`SchemaCheck::writable`]）。
/// 它现在只用来给提示文案措辞。
pub const IDENTITY_KEYS: [&str; 2] = ["cursorAuth/cachedEmail", "cursorAuth/cachedScopedProfile"];

/// 库里所有登录态键的公共前缀。用来找出已知键表**之外**的 `cursorAuth/*` 键。
pub const AUTH_PREFIX: &str = "cursorAuth/";

/// Cursor 3.21.13 的 `workbench.desktop.main.js` 里出现过的全部 `cursorAuth/*` 键，
/// 加上 `cachedUserId`（这个版本已经不读写它，但旧版本会把它留在盘上）。
///
/// 这不是写入白名单。[`AUTH_KEYS`] 才是切号会写、会清的那几把。BYOK 钥匙、团队 id、
/// 引导日期留在这里，是为了让一台登出后还留着它们的机器**不要**被当成「键名改了」。
/// 对照来源是本机 Cursor 3.21.13（`e44a49c17e33`）的 bundle 字符串，不是猜测。
pub const KNOWN_AUTH_KEYS: [&str; 21] = [
    "cursorAuth/accessToken",
    "cursorAuth/refreshToken",
    "cursorAuth/cachedEmail",
    "cursorAuth/cachedSignUpType",
    "cursorAuth/stripeMembershipType",
    "cursorAuth/stripeSubscriptionStatus",
    "cursorAuth/stripeMembershipAuthId",
    "cursorAuth/cachedScopedProfile",
    "cursorAuth/cachedTeam",
    "cursorAuth/cachedUserId",
    "cursorAuth/onboardingDate",
    "cursorAuth/stripeCustomerId",
    "cursorAuth/teamId",
    "cursorAuth/workspaceOpenedDate",
    "cursorAuth/changeManagementCodeSnippets",
    "cursorAuth/openAIKey",
    "cursorAuth/claudeKey",
    "cursorAuth/googleKey",
    "cursorAuth/azureApiKey",
    "cursorAuth/bedrockAccessKey",
    "cursorAuth/bedrockSecretKey",
];

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
pub const DISPLAY_KEYS: [&str; 7] = [
    "cursorAuth/cachedEmail",
    "cursorAuth/cachedSignUpType",
    "cursorAuth/stripeMembershipType",
    "cursorAuth/stripeSubscriptionStatus",
    "cursorAuth/stripeMembershipAuthId",
    "cursorAuth/cachedScopedProfile",
    "cursorAuth/cachedTeam",
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
///
/// 出口形状见 [`SchemaCheckWire`]：序列化时会**额外带上** `writable` 与 `blockedReason`。
/// 界面不该自己拼「能不能写」那套判据 —— 它拼过，而且和这边拼得不一样。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    rename_all = "camelCase",
    into = "SchemaCheckWire",
    from = "SchemaCheckWire"
)]
pub struct SchemaCheck {
    /// 库在不在。
    pub db_present: bool,
    /// `ItemTable` 在不在。
    pub table_present: bool,
    /// 读到了哪些 auth 键。
    pub present_keys: Vec<String>,
    /// 预期有、实际没有的键。
    pub missing_keys: Vec<String>,
    /// 库里有、[`KNOWN_AUTH_KEYS`] 里没有的 `cursorAuth/*` 键。
    ///
    /// 多出来不等于改了名：Cursor 加字段是常态。拦不拦看 [`Self::drift_auth_keys`]。
    #[serde(default)]
    pub unknown_auth_keys: Vec<String>,
    /// 陌生键里**像是把 token 改了名**的那些：名字里带 `token`，或者值是一把 JWT。
    ///
    /// 没有必需 token 时，只有这张表非空才停手。`openAIKey` / `teamId` / `cachedTeam`
    /// 这类已知键，以及 `someNewFlag` 这种不像 token 的新键，都不在这里。
    #[serde(default)]
    pub drift_auth_keys: Vec<String>,
    /// Cursor 版本，用于「未在此版本验证过」的提示。
    pub cursor_version: Option<String>,
}

/// `SchemaCheck` 在 IPC 上的形状：探测到的事实，加上由它们算出来的结论。
///
/// 结论只算一次、只在 Rust 这边算。以前界面自己拼一遍：SwitcherPage 拼对了，
/// SettingsPage 漏掉了「没人登着就该放行」那一支，于是一台干净的 Cursor 一进设置页
/// 就顶着「格式与预期不符」的横幅。判据放在两处就一定会分叉，所以这里只留一处。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaCheckWire {
    pub db_present: bool,
    pub table_present: bool,
    pub present_keys: Vec<String>,
    pub missing_keys: Vec<String>,
    #[serde(default)]
    pub unknown_auth_keys: Vec<String>,
    #[serde(default)]
    pub drift_auth_keys: Vec<String>,
    pub cursor_version: Option<String>,
    /// [`SchemaCheck::writable`] 的结果。
    pub writable: bool,
    /// [`SchemaCheck::explain`] 的结果：不通过时给用户的那句话，通过时是 `None`。
    pub blocked_reason: Option<String>,
}

impl From<SchemaCheck> for SchemaCheckWire {
    fn from(c: SchemaCheck) -> Self {
        let writable = c.writable();
        let blocked_reason = c.explain();
        Self {
            db_present: c.db_present,
            table_present: c.table_present,
            present_keys: c.present_keys,
            missing_keys: c.missing_keys,
            unknown_auth_keys: c.unknown_auth_keys,
            drift_auth_keys: c.drift_auth_keys,
            cursor_version: c.cursor_version,
            writable,
            blocked_reason,
        }
    }
}

impl From<SchemaCheckWire> for SchemaCheck {
    /// 回来的路上把结论丢掉：它是算出来的，不是事实，留着就会有人去改它。
    fn from(w: SchemaCheckWire) -> Self {
        Self {
            db_present: w.db_present,
            table_present: w.table_present,
            present_keys: w.present_keys,
            missing_keys: w.missing_keys,
            unknown_auth_keys: w.unknown_auth_keys,
            drift_auth_keys: w.drift_auth_keys,
            cursor_version: w.cursor_version,
        }
    }
}

impl SchemaCheck {
    /// Cursor 现在登着号吗。
    ///
    /// 只看身份键（[`IDENTITY_KEYS`]）。「库里有 auth 键」是个不能用的判据 ——
    /// 登出会留下几把非身份键，那不叫登着号。
    ///
    /// 这只是个**近似**，所以 [`writable`](Self::writable) 不用它：反过来，登出留下了
    /// 身份键也不叫登着号，而哪几把会被留下我们管不着。
    pub fn logged_in(&self) -> bool {
        self.present_keys
            .iter()
            .any(|k| IDENTITY_KEYS.contains(&k.as_str()))
    }

    /// 必需的 token 在不在。
    fn has_tokens(&self) -> bool {
        REQUIRED_KEYS
            .iter()
            .all(|k| self.present_keys.iter().any(|p| p == k))
    }

    /// 能不能安全地写。
    ///
    /// 只有**拿得出 token 被改名的实证**时才停手。实证是 [`Self::drift_auth_keys`]：
    /// 陌生的 `cursorAuth/*` 键，并且名字里带 `token`，或者值本身是一把 JWT。
    ///
    /// 「库里有任何白名单之外的 `cursorAuth/*` 键」不够格。Cursor 3.21 的 bundle 里
    /// 就有 `openAIKey`、`claudeKey`、`teamId`、`stripeCustomerId`、`cachedTeam`、
    /// 引导日期这十几把，登出不会把它们清干净。把它们当成改名，等于在用户最该往里写的
    /// 时候再锁一次 —— 和 Windows 上已经发生过的两次误判是同一类错。
    ///
    /// 更早的判据「有身份键却缺 token = 改名了」也废了：它假定登出会把
    /// `cachedEmail` / `cachedScopedProfile` 一并清干净，而这取决于那一版 `logout`
    /// 清了什么。
    ///
    /// 真正要防的是：token 键改了名，我们照旧写老名字，Cursor 仍登着旧号，而界面报
    /// 「切换成功」。那种新名字要么还叫 token，要么里面躺着一把 JWT。
    pub fn writable(&self) -> bool {
        if !self.db_present || !self.table_present {
            return false;
        }
        if self.has_tokens() {
            return true;
        }
        self.drift_auth_keys.is_empty()
    }

    /// 不通过时给用户的一句话。
    ///
    /// 只点名**必需的**那几把缺了的键，外加那几把陌生键 —— 后者就是判断的依据，
    /// 也是我们适配新版本时唯一需要的信息，所以要让用户能直接抄走。
    /// 十把键全列出来是一屏键名，用户读完仍然不知道该干什么。
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
        let missing: Vec<&str> = REQUIRED_KEYS
            .iter()
            .copied()
            .filter(|k| !self.present_keys.iter().any(|p| p == k))
            .collect();
        Some(format!(
            "Cursor 读不到 {}，库里却有{}这样没见过的键 —— 它升级后改了键名。\
             照旧写老键名的话，Cursor 会仍然登着原来的号，而这边报「切换成功」，\
             所以切号先降级为只读。把这句话连同 Cursor 版本（{}）报给我们即可适配。",
            missing.join("、"),
            preview(&self.drift_auth_keys),
            self.cursor_version.as_deref().unwrap_or("未知")
        ))
    }
}

/// 陌生键太多时只列前几把。它是给人看的，不是给机器解析的。
fn preview(keys: &[String]) -> String {
    const SHOWN: usize = 3;
    let head = keys
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join("、");
    if keys.len() > SHOWN {
        format!("{head} 等 {} 把", keys.len())
    } else {
        head
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
        // 读也用读写方式打开，再立刻钉上 `query_only`。
        //
        // `state.vscdb` 是 WAL。`SQLITE_OPEN_READ_ONLY` 在 Windows 上经常映射不了
        // `-shm`（共享内存要写权限），于是连接退回去只看见上次 checkpoint 的主库：
        // 身份键在主库里、token 还在 WAL 里，读出来就是「登着号却没有 token」。
        // 热切的确认轮询也走这条读路径，Cursor 还开着、刚把新 token 写进 WAL 的时候
        // 最容易踩中。读写打开才能加入 WAL 索引；`query_only` 保证这条连接改不了任何一行。
        // 文件本身不可写时退回只读，总比完全读不到强。
        if read_only {
            if let Ok(conn) = self.open_flags(OpenFlags::SQLITE_OPEN_READ_WRITE) {
                if conn.pragma_update(None, "query_only", true).is_ok() {
                    return Ok(conn);
                }
            }
        }
        let flags = if read_only {
            OpenFlags::SQLITE_OPEN_READ_ONLY
        } else {
            OpenFlags::SQLITE_OPEN_READ_WRITE
        };
        self.open_flags(flags)
    }

    fn open_flags(&self, flags: OpenFlags) -> Result<Connection> {
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
                .query_row([key], |row| row.get_ref(0).map(text_of))
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
            unknown_auth_keys: Vec::new(),
            drift_auth_keys: Vec::new(),
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
        let (unknown, drift) = classify_auth_keys(&conn);
        check.unknown_auth_keys = unknown;
        check.drift_auth_keys = drift;
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

/// 已知键表之外的 `cursorAuth/*` 键，以及其中像是「token 被改了名」的子集。
///
/// 查不动时两边都是空的：**拿不到实证就不拦**。
fn classify_auth_keys(conn: &Connection) -> (Vec<String>, Vec<String>) {
    let Ok(mut stmt) =
        conn.prepare("SELECT key, value FROM ItemTable WHERE key LIKE ?1 ORDER BY key")
    else {
        return (Vec::new(), Vec::new());
    };
    // `/` 在 LIKE 里没有特殊含义，前缀里也没有 `%` / `_`，不需要 ESCAPE。
    let pattern = format!("{AUTH_PREFIX}%");
    let Ok(rows) = stmt.query_map([pattern], |row| {
        let key: String = row.get(0)?;
        let value = row.get_ref(1).map(text_of)?;
        Ok((key, value))
    }) else {
        return (Vec::new(), Vec::new());
    };
    let mut unknown = Vec::new();
    let mut drift = Vec::new();
    for row in rows.flatten() {
        let (key, value) = row;
        if KNOWN_AUTH_KEYS.contains(&key.as_str()) {
            continue;
        }
        if is_renamed_token_key(&key, value.as_deref()) {
            drift.push(key.clone());
        }
        unknown.push(key);
    }
    (unknown, drift)
}

/// 这把陌生键是不是「token 换了个名字」。
///
/// 两种样子都算：名字里还有 `token`（`accessTokenV2`），或者值是一把 JWT
/// （`eyJ…` 三段）。`openAIKey`、`teamId`、`someNewFlag=1` 都不是。
fn is_renamed_token_key(key: &str, value: Option<&str>) -> bool {
    if key.to_ascii_lowercase().contains("token") {
        return true;
    }
    value.is_some_and(looks_like_jwt)
}

/// 三段、头一段以 `eyJ` 开头。不验签名，只用来认出「这格里躺着一把 JWT」。
fn looks_like_jwt(value: &str) -> bool {
    let value = value.trim();
    let mut parts = value.split('.');
    let (Some(header), Some(payload), Some(sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    header.starts_with("eyJ") && !payload.is_empty() && !sig.is_empty()
}

/// 把一格取成字符串。
///
/// `ItemTable` 的 `value` 列声明的是 `BLOB`，sqlite 的列类型只是建议，一格存什么类型
/// 由写它的人决定。本机实测 Cursor 写的是 text，但只认 text 的话，哪天它改用字节写入，
/// **整次读取**就会失败 —— 而读取失败在 [`StateDb::check`] 里会被当成「没登录」，
/// 于是我们会兴高采烈地往一台其实登着号的机器上写。宁可多认一种存法。
fn text_of(value: rusqlite::types::ValueRef<'_>) -> Option<String> {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => String::from_utf8(bytes.to_vec()).ok(),
        // token 不可能是数字或 NULL。真碰上就是「这一格不是我们要的东西」，当作没有。
        _ => None,
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
    fn the_write_whitelist_is_inside_the_known_key_table() {
        for key in AUTH_KEYS {
            assert!(
                KNOWN_AUTH_KEYS.contains(&key),
                "{key} 会写入，却不在已知键表里，登出后会被当成改名"
            );
        }
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
        assert!(printed.contains(&format!("{} keys", AUTH_KEYS.len())));
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
        // 7 把展示键里 5 把目标号没有：signUpType / subscriptionStatus / membershipAuthId /
        // scopedProfile / cachedTeam。留下上一个号的团队名，菜单会显示成切错了号。
        assert_eq!(removed, 5);

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

    /// Windows 实测：用户登出后库里还剩 `stripeMembershipType` / `cachedSignUpType` /
    /// `onboardingDate`，token 和邮箱都已被清。这是「没人登着」，不是键名漂移 ——
    /// 它曾经把切号禁成只读，而那台机器恰恰正等着我们往里写。
    #[test]
    fn leftovers_from_a_logout_are_not_mistaken_for_a_login() {
        let (_dir, db) = fixture();
        let mut leftovers = AuthBundle::new();
        leftovers.insert("cursorAuth/stripeMembershipType", "pro");
        leftovers.insert("cursorAuth/cachedSignUpType", "google");
        leftovers.insert("cursorAuth/onboardingDate", "2026-08-01");
        db.write_auth(&leftovers, false).unwrap();

        let check = db.check(None);
        assert!(!check.present_keys.is_empty(), "残留键确实在库里");
        assert!(!check.logged_in(), "残留的非身份键不算登着号");
        assert!(check.writable(), "没人登着就该放行，这正是要写进去的时候");
        assert!(check.explain().is_none());
    }

    /// 往库里塞一把我们不认识的 `cursorAuth/*` 键，模拟 Cursor 改名。
    fn put_raw(db: &StateDb, key: &str, value: &str) {
        Connection::open(db.path())
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO ItemTable (key, value) VALUES (?1, ?2)",
                rusqlite::params![key, value],
            )
            .unwrap();
    }

    /// 结论只在 Rust 这边算一次，序列化时一起交给界面。
    #[test]
    fn the_wire_shape_carries_the_verdict_so_the_ui_never_re_derives_it() {
        let (_dir, db) = fixture();
        let mut partial = AuthBundle::new();
        partial.insert("cursorAuth/cachedEmail", "a@example.com");
        db.write_auth(&partial, false).unwrap();
        put_raw(&db, "cursorAuth/accessTokenV2", "new-name");

        let json = serde_json::to_value(db.check(Some("9.9.9".into()))).unwrap();
        assert_eq!(json["writable"], serde_json::json!(false));
        let reason = json["blockedReason"].as_str().unwrap();
        assert!(reason.contains("降级为只读"));
        // 只点名卡住切号的那两把，不是把十把键全倒出来。
        assert!(reason.contains("cursorAuth/accessToken"));
        assert!(
            !reason.contains("onboardingDate"),
            "别把不相干的键也列上：{reason}"
        );
        // 判断的依据和适配要用的信息都得能直接抄走。
        assert!(reason.contains("cursorAuth/accessTokenV2"), "{reason}");
        assert!(reason.contains("9.9.9"), "{reason}");
        assert_eq!(
            json["unknownAuthKeys"],
            serde_json::json!(["cursorAuth/accessTokenV2"])
        );
        assert_eq!(
            json["driftAuthKeys"],
            serde_json::json!(["cursorAuth/accessTokenV2"])
        );
    }

    /// 真的改名了：新名字就在库里摆着。这一种有实证，该停手（§11）。
    #[test]
    fn a_renamed_key_degrades_to_read_only() {
        let (_dir, db) = fixture();
        let mut partial = AuthBundle::new();
        partial.insert("cursorAuth/cachedEmail", "a@example.com");
        partial.insert("cursorAuth/stripeMembershipType", "ultra");
        db.write_auth(&partial, false).unwrap();
        put_raw(&db, "cursorAuth/accessTokenV2", "header.payload.sig");
        put_raw(&db, "cursorAuth/refreshTokenV2", "header.payload.sig");

        let check = db.check(None);
        assert_eq!(
            check.unknown_auth_keys,
            vec![
                "cursorAuth/accessTokenV2".to_string(),
                "cursorAuth/refreshTokenV2".to_string()
            ]
        );
        assert!(!check.writable());
        assert!(check.explain().unwrap().contains("降级为只读"));
    }

    /// Windows 实测（第二次）：库里剩着身份键、却没有 token，而**没有任何**陌生的
    /// `cursorAuth/*` 键。上一版把这一种也当成改名，于是那台机器上切号整块锁死。
    /// 没有实证就不该拦 —— 何况读不到 token 本来就意味着没有登录态可写坏。
    #[test]
    fn leftover_identity_keys_without_evidence_of_a_rename_still_allow_writing() {
        let (_dir, db) = fixture();
        let mut leftovers = AuthBundle::new();
        leftovers.insert("cursorAuth/cachedEmail", "a@example.com");
        leftovers.insert("cursorAuth/cachedUserId", "auth0|user_01");
        leftovers.insert("cursorAuth/stripeMembershipType", "pro");
        db.write_auth(&leftovers, false).unwrap();

        let check = db.check(None);
        assert!(check.logged_in(), "身份键确实在库里");
        assert!(check.unknown_auth_keys.is_empty(), "但没有任何改名的迹象");
        assert!(check.writable(), "没有实证就不该把切号锁死");
        assert!(check.explain().is_none());
    }

    /// token 齐全时，多出几把没见过的键不算事：Cursor 加字段是常态，
    /// 我们要写的那几把都在原处。
    #[test]
    fn extra_keys_alongside_working_tokens_are_not_a_block() {
        let (_dir, db) = fixture();
        db.write_auth(&full_bundle("a@example.com"), true).unwrap();
        put_raw(&db, "cursorAuth/someNewFlag", "1");

        let check = db.check(None);
        assert_eq!(check.unknown_auth_keys, vec!["cursorAuth/someNewFlag"]);
        assert!(check.writable());
        assert!(check.explain().is_none());
    }

    /// Cursor 3.21 登出后仍可能留着的键：BYOK、团队、账单客户、引导日期。
    /// 它们在 bundle 里，不是 token 改名。没 token 的机器必须还能写。
    #[test]
    fn known_extra_keys_on_a_logged_out_database_do_not_block() {
        let (_dir, db) = fixture();
        for key in [
            "cursorAuth/openAIKey",
            "cursorAuth/claudeKey",
            "cursorAuth/googleKey",
            "cursorAuth/azureApiKey",
            "cursorAuth/bedrockAccessKey",
            "cursorAuth/bedrockSecretKey",
            "cursorAuth/stripeCustomerId",
            "cursorAuth/cachedTeam",
            "cursorAuth/teamId",
            "cursorAuth/workspaceOpenedDate",
            "cursorAuth/changeManagementCodeSnippets",
            "cursorAuth/cachedUserId",
        ] {
            put_raw(&db, key, "leftover");
        }
        let check = db.check(None);
        assert!(
            check.unknown_auth_keys.is_empty(),
            "{:?}",
            check.unknown_auth_keys
        );
        assert!(check.drift_auth_keys.is_empty());
        assert!(!check.logged_in(), "cachedUserId 单独留下不算登着号");
        assert!(check.writable());
        assert!(check.explain().is_none());
    }

    /// 没见过、但也不像 token 的新键，缺 token 时仍然放行。
    /// 上一版把「任何陌生 cursorAuth/*」都当成改名，这一种会被锁死。
    #[test]
    fn an_unrecognized_flag_without_tokens_still_allows_writing() {
        let (_dir, db) = fixture();
        put_raw(&db, "cursorAuth/someNewFlag", "1");
        let check = db.check(None);
        assert_eq!(check.unknown_auth_keys, vec!["cursorAuth/someNewFlag"]);
        assert!(check.drift_auth_keys.is_empty());
        assert!(check.writable());
        assert!(check.explain().is_none());
    }

    /// 新键的名字不带 token，但值是一把 JWT：token 被挪走了，照旧写老名字会报成功、号却没换。
    #[test]
    fn an_unknown_key_holding_a_jwt_is_a_rename() {
        let (_dir, db) = fixture();
        put_raw(
            &db,
            "cursorAuth/sessionBlob",
            "eyJhbGciOiJIUzI1NiJ9.eyJ0eXBlIjoic2Vzc2lvbiJ9.sig",
        );
        let check = db.check(None);
        assert_eq!(check.drift_auth_keys, vec!["cursorAuth/sessionBlob"]);
        assert!(!check.writable());
        assert!(check.explain().unwrap().contains("cursorAuth/sessionBlob"));
    }

    /// 另一个连接已经提交、还没 checkpoint 的写入，读路径必须看得见。
    /// 热切确认轮询靠的就是这个：Cursor 还开着，新 token 在 WAL 里。
    #[test]
    fn a_live_writers_commit_is_visible() {
        let (_dir, db) = fixture();
        db.write_auth(&full_bundle("a@example.com"), true).unwrap();
        let writer = Connection::open(db.path()).unwrap();
        writer
            .execute(
                "UPDATE ItemTable SET value = 'from-wal' WHERE key = 'cursorAuth/accessToken'",
                [],
            )
            .unwrap();
        assert_eq!(db.read_auth().unwrap().access_token(), Some("from-wal"));
        // 写者还活着：读路径没有把这格盖掉。
        let still: String = writer
            .query_row(
                "SELECT value FROM ItemTable WHERE key = 'cursorAuth/accessToken'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(still, "from-wal");
    }

    /// `value` 列声明的是 BLOB。哪天 Cursor 真按字节写，也不能让**整次读取**失败 ——
    /// 那会在 `check` 里被当成「没登录」，然后往一台登着号的机器上写。
    #[test]
    fn a_token_stored_as_bytes_is_still_read() {
        let (_dir, db) = fixture();
        db.write_auth(&full_bundle("a@example.com"), true).unwrap();
        Connection::open(db.path())
            .unwrap()
            .execute(
                "UPDATE ItemTable SET value = ?1 WHERE key = 'cursorAuth/accessToken'",
                rusqlite::params![b"blob-access".to_vec()],
            )
            .unwrap();

        let read = db.read_auth().unwrap();
        assert_eq!(read.access_token(), Some("blob-access"));
        assert!(db.check(None).writable());
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
