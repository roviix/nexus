//! 本地 SQLite。
//!
//! 业务表（账号元信息、切号本、备份索引、活动日志、设置）里**不存秘密**——它们只有
//! ref。秘密集中在独立的 `secrets` 表里，由 [`crate::secrets::SqliteSecrets`] 独占读写
//! （为什么不走 OS 钥匙串，见那里）。这条边界仍然值钱：`SELECT * FROM accounts`
//! 永远不该出现 token，那是有测试盯着的。
//!
//! 因此库文件本身就是凭证文件，`open` 会把它连同所在目录的权限收到 0600 / 0700。
//!
//! 迁移用 `rusqlite_migration`：版本号单调递增，**只加不改**。改一条已发布的迁移
//! 意味着老用户的库和新代码对不上，而这种错要到用户机器上才发作。

use nexus_core::{AppError, ErrorCode, Result};
use rusqlite::{Connection, Transaction};
use rusqlite_migration::{Migrations, M};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// rusqlite 的错误统一翻成 `AppError`。孤儿规则不让我们在这里实现 `From`，
/// 所以给一个显式函数 + 扩展 trait。
pub fn sql_error(err: rusqlite::Error) -> AppError {
    AppError::new(ErrorCode::Database, format!("本地数据库错误：{err}"))
}

pub trait SqlExt<T> {
    /// `stmt.query_row(..).sql()?` —— 比每处写 `.map_err(sql_error)` 短且不易漏。
    fn sql(self) -> Result<T>;
}

impl<T> SqlExt<T> for rusqlite::Result<T> {
    fn sql(self) -> Result<T> {
        self.map_err(sql_error)
    }
}

/// 全部迁移。**追加**新版本时在 vec 末尾加一条，不要动已有的。
fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(
            r#"
CREATE TABLE accounts (
  id                    TEXT PRIMARY KEY,
  email                 TEXT UNIQUE NOT NULL,
  source                TEXT NOT NULL,              -- local | purchased | synced
  status                TEXT NOT NULL,              -- active | needs_login | dead
  note                  TEXT,
  tags                  TEXT NOT NULL DEFAULT '[]', -- JSON array
  membership            TEXT,
  signup_type           TEXT,
  workos_user_id        TEXT,
  usage_json            TEXT,
  last_checked_at       TEXT,
  last_error            TEXT,
  code_channel          TEXT NOT NULL DEFAULT 'auto',
  code_channel_resolved TEXT,
  last_code_at          TEXT,
  has_refresh           INTEGER NOT NULL DEFAULT 0,
  has_password          INTEGER NOT NULL DEFAULT 0,
  has_email_password    INTEGER NOT NULL DEFAULT 0,
  has_recovery_email    INTEGER NOT NULL DEFAULT 0,
  server_id             TEXT,
  created_at            TEXT NOT NULL,
  updated_at            TEXT NOT NULL
);
CREATE INDEX accounts_updated_at ON accounts(updated_at DESC);

-- 切号本。**与 accounts 无外键**：两个模块互不依赖（R1），数据只在 UI 层显式拷贝一次。
CREATE TABLE switch_profiles (
  id                TEXT PRIMARY KEY,
  email             TEXT UNIQUE NOT NULL,
  membership        TEXT,
  signup_type       TEXT,
  note              TEXT,
  machine_ids_json  TEXT NOT NULL,   -- 专属机器码（非秘密）
  created_at        TEXT NOT NULL,
  updated_at        TEXT NOT NULL,
  last_switched_at  TEXT
);

CREATE TABLE auth_backups (
  id          TEXT PRIMARY KEY,
  email       TEXT,
  created_at  TEXT NOT NULL,
  reason      TEXT NOT NULL   -- pre-switch | pre-restore | manual
);
CREATE INDEX auth_backups_created_at ON auth_backups(created_at DESC);

-- 这台真机的原始机器码。singleton：只有一行，永不覆盖。
CREATE TABLE machine_original (
  singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
  ids_json  TEXT NOT NULL,
  saved_at  TEXT NOT NULL
);

CREATE TABLE activity (
  id       INTEGER PRIMARY KEY AUTOINCREMENT,
  at       TEXT NOT NULL,
  level    TEXT NOT NULL,   -- info | warn | error
  scope    TEXT NOT NULL,   -- switcher | accounts | shop | app
  email    TEXT,
  message  TEXT NOT NULL
);
CREATE INDEX activity_at ON activity(at DESC);

CREATE TABLE settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
"#,
        ),
        // v2：秘密从钥匙串搬进本地库。单独一张表，不塞进业务表 —— 业务表「一行也没有
        // 秘密」这条性质本身还有用（列表、导出、日志都直接读业务表）。
        M::up(
            r#"
CREATE TABLE secrets (
  ref        TEXT PRIMARY KEY,   -- keys.rs 里构造的那个 ref
  value      TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
"#,
        ),
        // v3：游乐场。会话（对话 / 图片各一种）→ 消息 → 生成的图片，三层都是级联删除。
        // 图片**字节不进库**：落在 app_data/playground/images 下，这里只记文件名与元信息——
        // 一张图一两 MB，塞进 SQLite 会让备份和 WAL 都跟着胖。
        // 消息里没有任何凭证：模型名、路由结果、用量、耗时都是给人看的。
        M::up(
            r#"
CREATE TABLE playground_threads (
  id          TEXT PRIMARY KEY,
  kind        TEXT NOT NULL,            -- chat | image
  title       TEXT NOT NULL,
  source      TEXT NOT NULL,            -- local | cloud
  model       TEXT NOT NULL,
  token_id    TEXT,                     -- 云端密钥 id；本地号源为空
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE INDEX playground_threads_recent ON playground_threads(kind, updated_at DESC);

CREATE TABLE playground_messages (
  id          TEXT PRIMARY KEY,
  thread_id   TEXT NOT NULL REFERENCES playground_threads(id) ON DELETE CASCADE,
  seq         INTEGER NOT NULL,         -- 会话内顺序号，从 1 起
  role        TEXT NOT NULL,            -- user | assistant
  content     TEXT NOT NULL DEFAULT '',
  thinking    TEXT,
  model       TEXT,                     -- 产出这条回复时请求的模型
  routed      TEXT,                     -- 上游实际路由到的模型
  usage_json  TEXT,                     -- {"promptTokens":..,"completionTokens":..}
  error       TEXT,
  duration_ms INTEGER,
  ttft_ms     INTEGER,
  created_at  TEXT NOT NULL
);
CREATE INDEX playground_messages_thread ON playground_messages(thread_id, seq);

CREATE TABLE playground_images (
  id          TEXT PRIMARY KEY,
  message_id  TEXT NOT NULL REFERENCES playground_messages(id) ON DELETE CASCADE,
  thread_id   TEXT NOT NULL REFERENCES playground_threads(id) ON DELETE CASCADE,
  file        TEXT NOT NULL,            -- images 目录下的文件名
  mime        TEXT NOT NULL,
  width       INTEGER,
  height      INTEGER,
  bytes       INTEGER NOT NULL,
  size        TEXT,                     -- 请求时要的规格，如 1024x1024
  created_at  TEXT NOT NULL
);
CREATE INDEX playground_images_thread ON playground_images(thread_id, created_at);
"#,
        ),
        // v4：账号来源只剩 local / purchased。`synced` 那批本来就是用户自己的号，并入 local；
        // `server_id` 没有所指了，删掉。
        M::up(
            r#"
UPDATE accounts SET source = 'local' WHERE source = 'synced';
ALTER TABLE accounts DROP COLUMN server_id;
"#,
        ),
        // v5：本地网关的请求账本。每条方言口请求结束记一行——概览上的「本地用量」全从这里算。
        // 只记数字与名字：账号邮箱、模型、状态、token 数、耗时；**没有对话内容，没有凭证**。
        // `at_ms` 存 Unix 毫秒而不是 RFC 3339：按天聚合要做整除，字符串做不了。
        M::up(
            r#"
CREATE TABLE gateway_requests (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  at_ms         INTEGER NOT NULL,
  account       TEXT NOT NULL,            -- 小写邮箱
  model         TEXT NOT NULL,            -- 客户端要的模型
  routed        TEXT,                     -- 上游实际路由到的模型
  dialect       TEXT NOT NULL,            -- openai | anthropic | responses
  ok            INTEGER NOT NULL,
  status        INTEGER NOT NULL,         -- HTTP 语义的状态码
  kind          TEXT,                     -- 失败时的分类（quota / auth / rate_limit…）
  input_tokens  INTEGER NOT NULL DEFAULT 0,
  output_tokens INTEGER NOT NULL DEFAULT 0,
  cache_read    INTEGER NOT NULL DEFAULT 0,
  cache_write   INTEGER NOT NULL DEFAULT 0,
  measured      INTEGER NOT NULL DEFAULT 1, -- usage 是上游报的还是我们估的
  ttft_ms       INTEGER,
  duration_ms   INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX gateway_requests_at ON gateway_requests(at_ms DESC);
"#,
        ),
        // v6：接码渠道只剩 nexus / manual。指到已拿掉的第三方渠道的号退回 auto，
        // 「上次命中」里的也清掉 —— 否则界面会一直显示一条已经不存在的渠道名。
        M::up(
            r#"
UPDATE accounts SET code_channel = 'auto' WHERE code_channel IN ('panel3100', 'shiyi');
UPDATE accounts SET code_channel_resolved = NULL WHERE code_channel_resolved IN ('panel3100', 'shiyi');
"#,
        ),
        // v7：ChatGPT 账号。**独立一张表，不扩 accounts**（ARCHITECTURE §5.3）：持久化身份是
        // `(platform, external_id)`——这里 external_id 就是 `chatgpt_account_id`，同一个邮箱在
        // Cursor 和 ChatGPT 是两个账号。凭证（refresh / access / id_token）照旧只在 secrets 表，
        // 这里只有 has_refresh 这类投影。用量是 ChatGPT 自己的形状（5 小时 / 7 天两个滚动窗口），
        // 存成 JSON，不试图套进 Cursor 的四桶。
        M::up(
            r#"
CREATE TABLE chatgpt_accounts (
  id               TEXT PRIMARY KEY,
  account_ref      TEXT UNIQUE NOT NULL,        -- chatgpt_account_id：上游侧的账号身份
  email            TEXT,
  plan_type        TEXT,                        -- plus | pro | team | free …
  status           TEXT NOT NULL,               -- active | needs_login | dead
  enabled          INTEGER NOT NULL DEFAULT 1,  -- 进不进网关接力队
  note             TEXT,
  usage_json       TEXT,                        -- 5 小时 / 7 天窗口快照
  last_checked_at  TEXT,
  last_error       TEXT,
  has_refresh      INTEGER NOT NULL DEFAULT 0,
  access_expires_at TEXT,                       -- access token 的过期时刻（RFC 3339）
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL
);
"#,
        ),
        // v8：Grok Build / Kiro 订阅号。和 chatgpt_accounts 同一形状（独立表、凭证在 secrets），
        // 不扩 accounts（ARCHITECTURE §5.3）。Grok 的 external_id 是 JWT `sub`；Kiro 的是
        // access token 的 subject / 文件里的 profileArn 尾巴，导入时再定。
        M::up(
            r#"
CREATE TABLE grok_accounts (
  id               TEXT PRIMARY KEY,
  account_ref      TEXT UNIQUE NOT NULL,
  email            TEXT,
  plan_type        TEXT,
  status           TEXT NOT NULL,
  enabled          INTEGER NOT NULL DEFAULT 1,
  note             TEXT,
  last_checked_at  TEXT,
  last_error       TEXT,
  has_refresh      INTEGER NOT NULL DEFAULT 0,
  access_expires_at TEXT,
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL
);
CREATE TABLE kiro_accounts (
  id               TEXT PRIMARY KEY,
  account_ref      TEXT UNIQUE NOT NULL,
  email            TEXT,
  plan_type        TEXT,
  status           TEXT NOT NULL,
  enabled          INTEGER NOT NULL DEFAULT 1,
  note             TEXT,
  auth_method      TEXT,
  last_checked_at  TEXT,
  last_error       TEXT,
  has_refresh      INTEGER NOT NULL DEFAULT 0,
  access_expires_at TEXT,
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL
);
"#,
        ),
        // v9：网关按「通道」建模（ARCHITECTURE §6.2）。
        //  - 账本记通道：事后要分得清一条请求走的是 Cursor 还是 Grok；老行按当时唯一的通道补成 cursor。
        //  - 异步媒体任务（生视频）登记簿：request_id → 通道 / 账号，状态轮询要回到创建它的号。
        //  - Grok 账号补：账号类型（OAuth 订阅 / API Key）、额度快照、媒体资格（自动探测 + 手动覆盖）、档位。
        M::up(
            r#"
ALTER TABLE gateway_requests ADD COLUMN channel TEXT NOT NULL DEFAULT 'cursor';
UPDATE gateway_requests SET channel = 'chatgpt'
  WHERE channel = 'cursor' AND (model LIKE 'gpt-%' OR model LIKE 'codex-%' OR model LIKE 'chatgpt/%');
CREATE TABLE gateway_media_jobs (
  request_id    TEXT PRIMARY KEY,
  channel       TEXT NOT NULL,
  account       TEXT NOT NULL,            -- 小写标签
  model         TEXT NOT NULL,
  op            TEXT NOT NULL,            -- generate | edit | extend
  status        TEXT NOT NULL,            -- pending | done | failed | expired
  video_url     TEXT,
  duration_secs INTEGER,
  resolution    TEXT,
  created_ms    INTEGER NOT NULL,
  updated_ms    INTEGER NOT NULL
);
CREATE INDEX gateway_media_jobs_created ON gateway_media_jobs(created_ms DESC);
ALTER TABLE grok_accounts ADD COLUMN auth_kind TEXT NOT NULL DEFAULT 'oauth';
ALTER TABLE grok_accounts ADD COLUMN usage_json TEXT;
ALTER TABLE grok_accounts ADD COLUMN media_probe INTEGER;      -- NULL 未探测 / 0 不能 / 1 能
ALTER TABLE grok_accounts ADD COLUMN media_override INTEGER;   -- 用户手动：NULL 跟探测 / 0 / 1
ALTER TABLE grok_accounts ADD COLUMN subscription_tier TEXT;
"#,
        ),
        // v10：Cursor 账号可以只靠一份 session token 撑着（没有 refresh）。
        //  - has_access / access_expires_at 是 Access 那条秘密的投影：列表页判「仅会话的号还活着没」
        //    不用解密。老库里有 refresh 的号这两列先是 0 / NULL，下一次写凭证时补齐，不影响判断。
        M::up(
            r#"
ALTER TABLE accounts ADD COLUMN has_access INTEGER NOT NULL DEFAULT 0;
ALTER TABLE accounts ADD COLUMN access_expires_at TEXT;
"#,
        ),
        // v11：Cursor 个人订阅的 Stripe 账单快照（标价 / 折扣 / 发票）。
        // 门户 URL 和 ephemeral key 不进这一列——那是一次性账单钥匙，只活在当次请求的栈上。
        M::up(
            r#"
ALTER TABLE accounts ADD COLUMN billing_json TEXT;
"#,
        ),
        // v12：ChatGPT 账号把 JWT / `/wham/usage` 里已经有的身份留下来。
        // 用户 id、工作区名登录时就在 token 里，以前解析完就扔了。
        M::up(
            r#"
ALTER TABLE chatgpt_accounts ADD COLUMN user_id TEXT;
ALTER TABLE chatgpt_accounts ADD COLUMN organization_id TEXT;
ALTER TABLE chatgpt_accounts ADD COLUMN organization_title TEXT;
"#,
        ),
        // v13：ChatGPT 订阅账单快照（档位 / 到期 / 会不会续）。
        // 没有标价和发票——Codex OAuth 打不开 ChatGPT 网页的 Stripe 门户。
        M::up(
            r#"
ALTER TABLE chatgpt_accounts ADD COLUMN billing_json TEXT;
"#,
        ),
        // v14：Cursor 账号可以另存一把 `crsr_` User API Key。
        // session / refresh 过期时还能兑短期 access，拉逐条用量（没有额度百分比）。
        M::up(
            r#"
ALTER TABLE accounts ADD COLUMN has_api_key INTEGER NOT NULL DEFAULT 0;
"#,
        ),
        // v15：Cursor 账号可以归档。归档 = 先收起来：默认不列、不刷、不进网关候选，
        // 凭证原样留着，取消归档就回来。是一列时间而不是布尔，「什么时候收起来的」以后有用。
        M::up(
            r#"
ALTER TABLE accounts ADD COLUMN archived_at TEXT;
"#,
        ),
        // v16：Cursor access JWT 的 `type` claim（session | web | …）。
        //
        // `type=session` 可以写进 Cursor IDE；`type=web` 只是一把网站 WorkOS 会话。把 web JWT
        // 同时写进 accessToken / refreshToken 后，Cursor 会拿它请求 `/oauth/token`，服务端回
        // `shouldLogout: true` 并注销这把网站会话（2026-09-16 真机事故）。只看 exp 分不出二者，
        // 所以把非秘密的 claim 投影出来，列表页不用解密就能正确禁用「切号」。
        M::up(
            r#"
ALTER TABLE accounts ADD COLUMN access_token_type TEXT;
"#,
        ),
    ])
}

/// 还原时**不**灌回去的表：`machine_original` 是这台真机的原始机器码，一份从别的机器
/// 带过来的备份不该改写它（§5.2 的「原始机器码保护」）。其余所有表都整体还原。
const MACHINE_BOUND_TABLES: &[&str] = &["machine_original"];

/// 把路径的权限收紧。失败只记一笔：Windows 上没有这套位，而权限收不上
/// 不该让应用起不来。
#[cfg(unix)]
fn restrict(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    if let Err(err) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
        tracing::warn!(path = %path.display(), %err, "收紧权限失败");
    }
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) {}

/// 一个进程一个连接，用 `Mutex` 串行化。
///
/// 不做连接池：这是单用户桌面应用，并发度是「一个人点鼠标」，池子只会带来
/// WAL 之外额外的锁竞争面。
pub struct Db {
    conn: Mutex<Connection>,
    /// 库文件的位置；内存库是 `None`。
    ///
    /// 留着它只为一件事：备份要能**另开一条连接**去跑 `VACUUM INTO`，而不是占着上面那把
    /// 锁做整库重写 —— 那期间全应用每一次读写都得排队，界面就是在这种时候僵住的。
    path: Option<PathBuf>,
}

impl std::fmt::Debug for Db {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Db")
    }
}

impl Db {
    /// 打开（必要时创建）库文件并迁移到最新版本。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            restrict(dir, 0o700);
        }
        let conn = Connection::open(path).sql()?;
        // 必须赶在建 WAL 之前：SQLite 按主库文件的权限去创建 `-wal` / `-shm`，
        // 而刚写下的凭证正躺在 WAL 里。老库每次打开也会被重新收紧。
        restrict(path, 0o600);
        Self::prepare(conn, Some(path.to_path_buf()))
    }

    /// 内存库。测试用。
    pub fn open_in_memory() -> Result<Self> {
        Self::prepare(Connection::open_in_memory().sql()?, None)
    }

    fn prepare(mut conn: Connection, path: Option<PathBuf>) -> Result<Self> {
        // WAL：读不挡写。桌面端会边看列表边后台刷用量。
        conn.pragma_update(None, "journal_mode", "WAL").sql()?;
        conn.pragma_update(None, "foreign_keys", "ON").sql()?;
        // 崩溃时最多丢最后一次事务，换来切号时明显更快的写入。
        conn.pragma_update(None, "synchronous", "NORMAL").sql()?;
        migrations().to_latest(&mut conn).map_err(|err| {
            AppError::new(ErrorCode::Database, format!("数据库迁移失败：{err}"))
                .with_hint("这通常意味着库文件来自更新版本的应用。")
        })?;
        Ok(Self {
            conn: Mutex::new(conn),
            path,
        })
    }

    /// 只读 / 单语句写。闭包里直接用 rusqlite 的 `Result`。
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<T>) -> Result<T> {
        let conn = self.conn.lock().map_err(|_| lock_poisoned())?;
        f(&conn).sql()
    }

    /// 事务。闭包返回 `Err` 就整体回滚 —— 「清旧 + 写新」这类不允许出现中间态的
    /// 操作必须走这里（§5.1）。
    pub fn tx<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.conn.lock().map_err(|_| lock_poisoned())?;
        let tx = conn.transaction().sql()?;
        let out = f(&tx)?;
        tx.commit().sql()?;
        Ok(out)
    }

    // ── 快照与还原 ──────────────────────────────────────────────────────────

    /// 把整库快照到 `target`（`VACUUM INTO`）。
    ///
    /// 一条语句、事务一致、顺手压实。不做文件级拷贝：WAL 模式下主文件 + `-wal` 两个文件
    /// 才是完整状态，只拷一个会拷出半截事务。SQLite 要求目标文件不存在，调用方用带时间戳
    /// 的名字保证这一点。快照里有 `secrets` 表 —— 它和库本体一样是凭证文件，权限同样收到 0600。
    pub fn snapshot_to(&self, target: &Path) -> Result<()> {
        if let Some(dir) = target.parent() {
            std::fs::create_dir_all(dir)?;
            restrict(dir, 0o700);
        }
        let target_str = utf8_path(target)?;
        match &self.path {
            // **另开一条连接**，不走 `self.with`。整库重写按库的大小可以跑上几秒，
            // 而那把连接锁是全应用共用的 —— 占着它做备份，期间任何一次读写都得排队，
            // 界面就在那几秒里僵住。WAL 模式下这条新连接自己拿一个读快照：不挡写，
            // 也拿得到一份事务一致的镜像，正是备份要的语义。
            Some(path) => {
                let c = Connection::open(path).sql()?;
                c.execute("VACUUM INTO ?1", [target_str]).sql()?;
            }
            // 内存库没有第二条路：另开一条连接开出来的是另一个空库。测试才会走到这儿。
            None => self.with(|c| c.execute("VACUUM INTO ?1", [target_str]).map(|_| ()))?,
        }
        restrict(target, 0o600);
        Ok(())
    }

    /// 用一份快照覆盖当前库的内容。
    ///
    /// **不换文件。** 进程里每个模块攥着的都是这同一个连接，偷换它脚下的文件谁也不知道。
    /// 做法是把快照 ATTACH 进来，在一个事务里逐表「清空 → 灌入」；中途任何一步失败整体
    /// 回滚，库里要么全是旧的、要么全是新的。`MACHINE_BOUND_TABLES` 里的表不动。
    ///
    /// 快照可能来自旧版本的应用（表少几张、列少几列），所以先拷一份到临时文件、把那份
    /// 迁到当前版本，再从它灌 —— 用户的备份文件本身一个字节都不动。来自**更新**版本的
    /// 快照迁不下来，会在这一步被拒绝，而不是灌进半套认不出来的 schema。
    pub fn restore_from(&self, snapshot: &Path) -> Result<()> {
        let staging = snapshot.with_extension("db.restoring");
        std::fs::copy(snapshot, &staging)?;
        restrict(&staging, 0o600);
        let outcome = self.restore_from_staging(&staging);
        // 临时文件里有凭证，无论成败都清掉（连同 SQLite 可能留下的 -wal / -shm）。
        for suffix in ["", "-wal", "-shm", "-journal"] {
            let mut p = staging.clone().into_os_string();
            p.push(suffix);
            let _ = std::fs::remove_file(p);
        }
        outcome
    }

    fn restore_from_staging(&self, staging: &Path) -> Result<()> {
        {
            let mut c = Connection::open(staging).sql()?;
            // 先认一下：一份从没经过我们迁移的文件（空库、别的软件的库）user_version 是 0。
            // 不认的话，迁移会在它上面白手起家建出一套空表，然后把用户的数据「还原」成空。
            let version: i64 = c.query_row("PRAGMA user_version", [], |r| r.get(0)).sql()?;
            if version == 0 {
                return Err(AppError::invalid("这不是 Nexus 的备份文件。")
                    .with_hint("备份文件是「设置 → 本地备份」里生成的 nexus-*.db。"));
            }
            migrations().to_latest(&mut c).map_err(|err| {
                AppError::new(
                    ErrorCode::Database,
                    format!("这份备份无法迁移到当前版本：{err}"),
                )
                .with_hint("它可能来自更新版本的应用。")
            })?;
            // 迁移的结果要落回主文件、不留 -wal，ATTACH 时才读得到。
            c.pragma_update(None, "journal_mode", "DELETE").sql()?;
        }

        let staging_str = utf8_path(staging)?;
        let mut conn = self.conn.lock().map_err(|_| lock_poisoned())?;
        conn.execute("ATTACH DATABASE ?1 AS src", [staging_str])
            .sql()?;
        let copied = copy_tables_from_src(&mut conn);
        // DETACH 得在事务外；灌成灌败都要摘掉，不能让下一次操作还看见 src。
        let detached = conn.execute("DETACH DATABASE src", []).map(|_| ()).sql();
        copied.and(detached)
    }
}

/// 逐表「清空 → 从 `src` 灌入」，一个事务。
///
/// 按**主库**的表清单走：主库和 `src` 都在当前版本，表和列理应一致；主库有而 `src` 没有的
/// 表，INSERT 会报「no such table」，整体回滚 —— 那是迁移出了岔子，不该静默跳过。
/// 列名显式列出而不用 `SELECT *`：两边列顺序万一不同，位置对齐会把数据灌错列。
fn copy_tables_from_src(conn: &mut Connection) -> Result<()> {
    let tables: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM main.sqlite_master
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .sql()?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0)).sql()?;
        rows.collect::<rusqlite::Result<_>>().sql()?
    };

    let tx = conn.transaction().sql()?;
    // 表之间有外键（游乐场三层级联）。灌的顺序不按依赖排，把检查推迟到提交那一刻。
    tx.pragma_update(None, "defer_foreign_keys", true).sql()?;
    for table in tables
        .iter()
        .filter(|t| !MACHINE_BOUND_TABLES.contains(&t.as_str()))
    {
        let columns: Vec<String> = {
            let mut stmt = tx
                .prepare(&format!("PRAGMA main.table_info(\"{table}\")"))
                .sql()?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1)).sql()?;
            rows.collect::<rusqlite::Result<_>>().sql()?
        };
        let list = columns
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        tx.execute(&format!("DELETE FROM main.\"{table}\""), [])
            .sql()?;
        tx.execute(
            &format!("INSERT INTO main.\"{table}\" ({list}) SELECT {list} FROM src.\"{table}\""),
            [],
        )
        .sql()?;
    }
    tx.commit().sql()
}

fn utf8_path(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| AppError::invalid(format!("路径不是合法的 UTF-8：{}", path.display())))
}

fn lock_poisoned() -> AppError {
    AppError::new(
        ErrorCode::Database,
        "数据库锁已损坏（有线程在持锁时 panic）。",
    )
    .with_hint("重启应用即可恢复。")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrations_are_valid() {
        migrations().validate().expect("迁移定义本身有问题");
    }

    #[test]
    fn opening_twice_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/nexus.db");
        {
            let db = Db::open(&path).unwrap();
            db.with(|c| c.execute("INSERT INTO settings (key, value) VALUES ('a', '1')", []))
                .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let v: String = db
            .with(|c| {
                c.query_row("SELECT value FROM settings WHERE key = 'a'", [], |r| {
                    r.get(0)
                })
            })
            .unwrap();
        assert_eq!(v, "1");
    }

    #[test]
    fn transactions_roll_back_on_error() {
        let db = Db::open_in_memory().unwrap();
        let err = db.tx(|tx| {
            tx.execute("INSERT INTO settings (key, value) VALUES ('k', 'v')", [])
                .sql()?;
            Err::<(), _>(AppError::internal("boom"))
        });
        assert!(err.is_err());
        let n: i64 = db
            .with(|c| c.query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 0, "回滚后不该留下任何行");
    }

    #[test]
    fn transactions_commit_on_success() {
        let db = Db::open_in_memory().unwrap();
        db.tx(|tx| {
            tx.execute("INSERT INTO settings (key, value) VALUES ('k', 'v')", [])
                .sql()?;
            Ok(())
        })
        .unwrap();
        let n: i64 = db
            .with(|c| c.query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn sql_errors_carry_the_database_code() {
        let db = Db::open_in_memory().unwrap();
        let err = db
            .with(|c| c.execute("SELECT * FROM nope", []))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Database);
    }

    #[test]
    fn switch_profiles_and_accounts_have_no_foreign_key_between_them() {
        // R1 落成 schema 约束：往切号本插一个 accounts 里没有的邮箱必须成功。
        let db = Db::open_in_memory().unwrap();
        db.with(|c| {
            c.execute(
                "INSERT INTO switch_profiles (id, email, machine_ids_json, created_at, updated_at)
                 VALUES ('p1', 'nobody@example.com', '{}', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                [],
            )
        })
        .unwrap();
    }

    #[test]
    fn synced_accounts_become_local_and_lose_their_server_id() {
        // v4：老库里 source='synced' 的行要归为自有号，server_id 列要消失。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nexus.db");
        {
            let mut c = Connection::open(&path).unwrap();
            migrations().to_version(&mut c, 3).unwrap();
            c.execute(
                "INSERT INTO accounts (id, email, source, status, server_id, created_at, updated_at)
                 VALUES ('a', 'a@example.com', 'synced', 'active', 'srv-1', 't', 't')",
                [],
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let source: String = db
            .with(|c| {
                c.query_row("SELECT source FROM accounts WHERE id = 'a'", [], |r| {
                    r.get(0)
                })
            })
            .unwrap();
        assert_eq!(source, "local");
        let columns: Vec<String> = db
            .with(|c| {
                let mut stmt = c.prepare("PRAGMA table_info(accounts)")?;
                let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
                rows.collect()
            })
            .unwrap();
        assert!(!columns.iter().any(|c| c == "server_id"));
    }

    #[test]
    fn accounts_pointed_at_removed_code_channels_fall_back_to_auto() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nexus.db");
        {
            let mut c = Connection::open(&path).unwrap();
            migrations().to_version(&mut c, 5).unwrap();
            c.execute_batch(
                "INSERT INTO accounts (id, email, source, status, code_channel, code_channel_resolved, created_at, updated_at)
                 VALUES ('a', 'a@example.com', 'local', 'active', 'shiyi', 'panel3100', 't', 't'),
                        ('b', 'b@example.com', 'local', 'active', 'nexus', 'nexus', 't', 't'),
                        ('c', 'c@example.com', 'local', 'active', 'manual', NULL, 't', 't');",
            )
            .unwrap();
        }
        let db = Db::open(&path).unwrap();
        let rows: Vec<(String, String, Option<String>)> = db
            .with(|c| {
                let mut stmt = c.prepare(
                    "SELECT id, code_channel, code_channel_resolved FROM accounts ORDER BY id",
                )?;
                let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
                rows.collect()
            })
            .unwrap();
        assert_eq!(
            rows,
            vec![
                ("a".into(), "auto".into(), None),
                ("b".into(), "nexus".into(), Some("nexus".into())),
                ("c".into(), "manual".into(), None),
            ]
        );
    }

    // ── 快照与还原 ──────────────────────────────────────────────────────────

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

    fn machine_original(db: &Db) -> Option<String> {
        db.with(|c| {
            c.query_row("SELECT ids_json FROM machine_original", [], |r| r.get(0))
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
        })
        .unwrap()
    }

    #[test]
    fn a_snapshot_restores_everything_except_this_machines_identity() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("nexus.db")).unwrap();
        put_setting(&db, "k", "before");
        db.with(|c| {
            c.execute(
                "INSERT INTO secrets (ref, value, updated_at) VALUES ('acct/1/refresh', 'rt-old', 't')",
                [],
            )?;
            c.execute(
                "INSERT INTO machine_original (singleton, ids_json, saved_at) VALUES (1, '{\"m\":\"old\"}', 't')",
                [],
            )
        })
        .unwrap();

        let snapshot = dir.path().join("backups/nexus-1.db");
        db.snapshot_to(&snapshot).unwrap();
        assert!(snapshot.is_file());

        // 快照之后继续改：设置改了、秘密换了、还换了一台机器（机器码不同）。
        put_setting(&db, "k", "after");
        put_setting(&db, "only-after", "x");
        db.with(|c| {
            c.execute("UPDATE secrets SET value = 'rt-new'", [])?;
            c.execute(
                "UPDATE machine_original SET ids_json = '{\"m\":\"this-machine\"}'",
                [],
            )
        })
        .unwrap();

        db.restore_from(&snapshot).unwrap();

        assert_eq!(setting(&db, "k").as_deref(), Some("before"));
        assert_eq!(setting(&db, "only-after"), None, "快照里没有的行要被清掉");
        let secret: String = db
            .with(|c| c.query_row("SELECT value FROM secrets", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(secret, "rt-old");
        assert_eq!(
            machine_original(&db).as_deref(),
            Some("{\"m\":\"this-machine\"}"),
            "这台机器的原始机器码不该被别处的备份改写"
        );
        // 临时文件不能留下：它和库一样是凭证文件。
        assert!(!dir.path().join("backups/nexus-1.db.restoring").exists());
        assert!(snapshot.is_file(), "用户的备份文件本身不该被动");
    }

    #[test]
    fn a_snapshot_from_an_older_schema_is_migrated_before_restoring() {
        let dir = tempfile::tempdir().unwrap();
        // 手工造一份只跑到 v1 的库：没有 secrets 表、accounts 还带 server_id。
        let old = dir.path().join("nexus-old.db");
        {
            let mut c = Connection::open(&old).unwrap();
            migrations().to_version(&mut c, 1).unwrap();
            c.execute(
                "INSERT INTO accounts (id, email, source, status, server_id, created_at, updated_at)
                 VALUES ('a', 'a@example.com', 'synced', 'active', 'srv', 't', 't')",
                [],
            )
            .unwrap();
        }

        let db = Db::open(dir.path().join("nexus.db")).unwrap();
        put_setting(&db, "k", "current");
        db.restore_from(&old).unwrap();

        let (source, n): (String, i64) = db
            .with(|c| {
                let s = c.query_row("SELECT source FROM accounts", [], |r| r.get(0))?;
                let n = c.query_row("SELECT COUNT(*) FROM settings", [], |r| r.get(0))?;
                Ok((s, n))
            })
            .unwrap();
        assert_eq!(source, "local", "老备份里的 synced 在迁移时归为 local");
        assert_eq!(n, 0, "老备份里没有设置，当前的设置要被清掉");
    }

    #[test]
    fn a_snapshot_from_a_newer_app_is_refused_and_nothing_changes() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("nexus.db")).unwrap();
        put_setting(&db, "k", "keep");
        let snapshot = dir.path().join("nexus-future.db");
        db.snapshot_to(&snapshot).unwrap();
        {
            let c = Connection::open(&snapshot).unwrap();
            c.pragma_update(None, "user_version", 999).unwrap();
        }

        let err = db.restore_from(&snapshot).unwrap_err();
        assert_eq!(err.code, ErrorCode::Database);
        assert_eq!(setting(&db, "k").as_deref(), Some("keep"));
    }

    #[test]
    fn a_file_that_is_not_ours_is_refused_and_nothing_changes() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("nexus.db")).unwrap();
        put_setting(&db, "k", "keep");

        // 一份合法但空白的 SQLite 文件：user_version 是 0。
        let foreign = dir.path().join("something-else.db");
        {
            let c = Connection::open(&foreign).unwrap();
            c.execute("CREATE TABLE whatever (x)", []).unwrap();
        }
        let err = db.restore_from(&foreign).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert_eq!(setting(&db, "k").as_deref(), Some("keep"));

        // 根本不是 SQLite。
        let garbage = dir.path().join("garbage.db");
        std::fs::write(&garbage, b"hello").unwrap();
        assert!(db.restore_from(&garbage).is_err());
        assert_eq!(setting(&db, "k").as_deref(), Some("keep"));
    }

    #[test]
    fn the_database_stays_usable_after_a_restore() {
        // ATTACH 过的 src 要被摘掉，否则之后的 ATTACH / 事务会撞上它。
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("nexus.db")).unwrap();
        let snapshot = dir.path().join("s.db");
        db.snapshot_to(&snapshot).unwrap();
        db.restore_from(&snapshot).unwrap();
        db.restore_from(&snapshot).unwrap();
        db.tx(|tx| {
            tx.execute("INSERT INTO settings (key, value) VALUES ('k', 'v')", [])
                .sql()?;
            Ok(())
        })
        .unwrap();
    }
}
