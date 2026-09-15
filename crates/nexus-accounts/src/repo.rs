//! 账号仓库：`accounts` 表存元信息，凭证交给 `SecretStore`。
//!
//! 一条不变量贯穿全文件：**`accounts` 表里没有任何秘密。** `has_refresh` 这类布尔量
//! 是秘密存储状态的投影，写凭证和写标记必须一起发生，否则界面会显示一个「有 token」
//! 却取不出 token 的号。

use crate::billing::{self, AccountBilling};
use crate::model::{status_from_credentials, Account, AccountPatch, NewAccount, Source, Status};
use crate::token;
use crate::usage::AccountUsage;
use nexus_core::{now_iso, AccountId, AppError, Email, ErrorCode, Result, Secret};
use nexus_store::keys::{account_secret, AccountSecret};
use nexus_store::{Db, SecretStore};
use rusqlite::Row;
use std::sync::Arc;

pub struct Accounts {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
}

impl Accounts {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { db, secrets }
    }

    /// 列出全部账号，**按加入的先后**。
    ///
    /// 曾经是 `ORDER BY updated_at DESC`，那是个坑：刷一次用量就写一次 `updated_at`，
    /// 于是每刷新一轮，整个列表的顺序都变了 —— 用户刚记住「第三个是主力号」，一刷新
    /// 它就跑到第一个去了。顺序要跟着「什么时候加进来的」这种不会变的事实走。
    /// 同一毫秒加进来的（批量导入）再按 id 兜底，保证每次查询结果完全一致。
    pub fn list(&self) -> Result<Vec<Account>> {
        self.db.with(|c| {
            let mut stmt = c.prepare(&format!("{SELECT} ORDER BY created_at ASC, id ASC"))?;
            let rows = stmt.query_map([], row_to_account)?;
            rows.collect()
        })
    }

    pub fn get(&self, id: &AccountId) -> Result<Account> {
        self.find(id)?.ok_or_else(|| not_found(id.as_str()))
    }

    pub fn by_email(&self, email: &str) -> Result<Option<Account>> {
        let key = email.trim().to_ascii_lowercase();
        self.db.with(|c| {
            c.query_row(
                &format!("{SELECT} WHERE email = ?1"),
                [&key],
                row_to_account,
            )
            .map(Some)
            .or_else(no_rows)
        })
    }

    /// 新增或按邮箱合并。
    ///
    /// 合并时**只覆盖带值的凭证**：再导一份只有 refresh_token 的清单进来，不该把本地
    /// 存着的密码抹掉。这与 shop 侧 `ingestCursorAccounts` 的语义一致。
    pub fn upsert(&self, incoming: NewAccount) -> Result<Account> {
        incoming.qualify()?;
        let email = Email::parse(&incoming.email)?.into_string();
        let now = now_iso();

        let id = match self.by_email(&email)? {
            Some(existing) => existing.id,
            None => {
                let id = AccountId::new();
                self.db.with(|c| {
                    c.execute(
                        "INSERT INTO accounts (id, email, source, status, note, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                        rusqlite::params![
                            id.as_str(),
                            &email,
                            incoming.source.unwrap_or(Source::Local).as_str(),
                            Status::NeedsLogin.as_str(),
                            incoming.note.as_deref(),
                            &now,
                        ],
                    )
                })?;
                id
            }
        };

        self.put_secret(
            &id,
            AccountSecret::Refresh,
            incoming.refresh_token.as_deref(),
        )?;
        if let Some(raw) = incoming
            .access_token
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            self.put_access(&id, raw)?;
        }
        self.put_secret(
            &id,
            AccountSecret::CursorPassword,
            incoming.cursor_password.as_deref(),
        )?;
        self.put_secret(
            &id,
            AccountSecret::EmailPassword,
            incoming.email_password.as_deref(),
        )?;
        self.put_secret(
            &id,
            AccountSecret::RecoveryEmail,
            incoming.recovery_email.as_deref(),
        )?;
        self.put_secret(&id, AccountSecret::ApiKey, incoming.api_key.as_deref())?;
        if let Some(note) = incoming.note.as_deref() {
            self.db.with(|c| {
                c.execute(
                    "UPDATE accounts SET note = ?2 WHERE id = ?1",
                    rusqlite::params![id.as_str(), note],
                )
            })?;
        }
        self.sync_credential_flags(&id)?;
        self.get(&id)
    }

    pub fn patch(&self, id: &AccountId, patch: AccountPatch) -> Result<Account> {
        let account = self.get(id)?;
        let tags = match &patch.tags {
            Some(t) => serde_json::to_string(t)?,
            None => serde_json::to_string(&account.tags)?,
        };
        self.db.with(|c| {
            c.execute(
                "UPDATE accounts SET
                   note         = COALESCE(?2, note),
                   tags         = ?3,
                   code_channel = COALESCE(?4, code_channel),
                   status       = COALESCE(?5, status),
                   updated_at   = ?6
                 WHERE id = ?1",
                rusqlite::params![
                    id.as_str(),
                    patch.note.as_deref(),
                    tags,
                    patch.code_channel.as_deref(),
                    patch.status.map(|s| s.as_str()),
                    now_iso(),
                ],
            )
        })?;
        self.get(id)
    }

    /// 删账号：库里的行和这个号**全部**凭证一起清。
    pub fn remove(&self, id: &AccountId) -> Result<()> {
        let n = self
            .db
            .with(|c| c.execute("DELETE FROM accounts WHERE id = ?1", [id.as_str()]))?;
        if n == 0 {
            return Err(not_found(id.as_str()));
        }
        for kind in AccountSecret::ALL {
            self.secrets.delete(&account_secret(id, kind))?;
        }
        Ok(())
    }

    // ── 凭证 ────────────────────────────────────────────────────────────────

    /// 读一条凭证。**唯一**的明文出口，调用点应当少而显眼。
    pub fn secret(&self, id: &AccountId, kind: AccountSecret) -> Result<Option<Secret>> {
        self.secrets.get(&account_secret(id, kind))
    }

    pub fn require_secret(&self, id: &AccountId, kind: AccountSecret) -> Result<Secret> {
        self.secrets.require(&account_secret(id, kind))
    }

    /// 写一条凭证并同步标记位。`None` / 空串 = 不动（不是删除）——
    /// 「这次没提供」和「要清掉」是两件事，混淆会在合并一份不完整的清单时丢凭证。
    pub fn put_secret(
        &self,
        id: &AccountId,
        kind: AccountSecret,
        value: Option<&str>,
    ) -> Result<()> {
        let Some(v) = value.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(());
        };
        if kind == AccountSecret::ApiKey && !token::looks_like_user_api_key(v) {
            return Err(AppError::invalid("不是有效的 crsr_ API Key。").with_hint("形如 crsr_…"));
        }
        self.secrets
            .set(&account_secret(id, kind), &Secret::new(v))?;
        self.sync_credential_flags(id)
    }

    pub fn clear_secret(&self, id: &AccountId, kind: AccountSecret) -> Result<()> {
        self.secrets.delete(&account_secret(id, kind))?;
        self.sync_credential_flags(id)
    }

    /// 存一把用户粘进来的 session / access token。
    ///
    /// 和 `put_secret(Access, ..)` 的区别：这里先经 `normalize_access` 把 `user_xxx::` 前缀、URL 编码
    /// 剥掉，只存裸 JWT；前缀里的 `user_xxx` 顺手落到 `workos_user_id`——拼 cookie 要它，而只有
    /// session token 的号没有别的地方能拿到它。
    pub fn put_access(&self, id: &AccountId, raw: &str) -> Result<()> {
        let (user_id, jwt) = token::normalize_access(raw)?;
        self.secrets.set(
            &account_secret(id, AccountSecret::Access),
            &Secret::new(jwt),
        )?;
        if let Some(uid) = user_id {
            self.set_workos_user_id(id, &uid)?;
        }
        self.sync_credential_flags(id)
    }

    /// 把秘密存储的实际状态投影到表里的布尔列。凭证一变就调它。
    fn sync_credential_flags(&self, id: &AccountId) -> Result<()> {
        // 几个 exists / get 必须**全部**在进 `db.with` 之前问完。
        //
        // `db.with` 攥着那把 `Mutex<Connection>`，而秘密就落在同一个库上（`SqliteSecrets`），
        // 在闭包里再问一次就是自己等自己，进程直接挂死。秘密还在钥匙串里的时候这么写
        // 没事，所以这是一条换了后端才发作的规矩：**持有库锁期间不碰 SecretStore。**
        let has = |kind| self.secrets.exists(&account_secret(id, kind));
        let has_refresh = has(AccountSecret::Refresh);
        let has_password = has(AccountSecret::CursorPassword);
        let has_email_password = has(AccountSecret::EmailPassword);
        let has_recovery_email = has(AccountSecret::RecoveryEmail);
        let has_api_key = has(AccountSecret::ApiKey);
        // access 的过期时刻从 JWT 里读出来落成一列：列表页判「仅会话的号还活着没」不用再解密。
        let access = self.secret(id, AccountSecret::Access)?;
        let has_access = access.is_some();
        let access_expires_at = access.and_then(|a| token::jwt_expiry_iso(a.expose()));
        let live_access = has_access && !token::session_expired(access_expires_at.as_deref());

        self.db.with(|c| {
            c.execute(
                "UPDATE accounts SET
                   has_refresh        = ?2,
                   has_password       = ?3,
                   has_email_password = ?4,
                   has_recovery_email = ?5,
                   has_access         = ?8,
                   access_expires_at  = ?9,
                   has_api_key        = ?10,
                   -- 已判死的号不因为补了凭证就自动复活：那要走一次真正的刷新。
                   status = CASE WHEN status = 'dead' THEN status ELSE ?6 END,
                   updated_at = ?7
                 WHERE id = ?1",
                rusqlite::params![
                    id.as_str(),
                    has_refresh,
                    has_password,
                    has_email_password,
                    has_recovery_email,
                    status_from_credentials(has_refresh, live_access, has_api_key).as_str(),
                    now_iso(),
                    has_access,
                    access_expires_at,
                    has_api_key,
                ],
            )
        })?;
        Ok(())
    }

    // ── 用量与状态回写 ───────────────────────────────────────────────────────

    /// 记一次成功的用量拉取。
    ///
    /// dashboard 那条路成功 = 会话还活着，号标成 active。`crsr_` 兑票拉到的花费
    /// **不算**会话复活：refresh 已经被拒的号不该因为还能查基础用量就重新变成可切号。
    pub fn record_usage(&self, id: &AccountId, usage: &AccountUsage) -> Result<()> {
        let now = now_iso();
        let json = serde_json::to_string(usage)?;
        let via_api_key = usage.via.as_deref() == Some("apiKey");
        self.db.with(|c| {
            c.execute(
                "UPDATE accounts SET
                   usage_json = ?2, membership = ?3, last_checked_at = ?4,
                   last_error = NULL,
                   status = CASE WHEN ?5 AND status = 'dead' THEN status ELSE 'active' END,
                   updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), json, usage.plan.as_deref(), now, via_api_key],
            )
        })?;
        Ok(())
    }

    /// 记一次成功的订阅账单拉取。
    ///
    /// 不碰 `last_checked_at` / `last_error`：那两格是用量刷新的口径。门户读失败
    /// 不该把一张好的用量快照标成「出错」。密钥进不了这一列——`snapshot_is_clean`
    /// 是最后一道门。
    pub fn record_billing(&self, id: &AccountId, billing: &AccountBilling) -> Result<()> {
        if !billing::snapshot_is_clean(billing) {
            return Err(AppError::internal(
                "账单快照里出现了不该留下的门户密钥，已丢弃。",
            ));
        }
        let json = serde_json::to_string(billing)?;
        self.db.with(|c| {
            c.execute(
                "UPDATE accounts SET billing_json = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), json, now_iso()],
            )
        })?;
        Ok(())
    }

    /// 记一次失败。
    ///
    /// `fatal`（凭证本身废了）时：有密码 → 退回待登录（还能 OAuth 救回来）；只靠 session token
    /// 撑着的号也退回待登录（那把 token 到期是预期内的事，粘一份新的就活）；有 refresh 而 refresh
    /// 被拒、又没密码的才判死。这与 shop 侧 `saveCursorUsage` 的语义一致。
    pub fn record_failure(&self, id: &AccountId, error: &str, fatal: bool) -> Result<()> {
        let account = self.get(id)?;
        let status = if fatal {
            Some(if account.has_password || !account.has_refresh {
                Status::NeedsLogin
            } else {
                Status::Dead
            })
        } else {
            None
        };
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE accounts SET
                   last_error = ?2, last_checked_at = ?3,
                   status = COALESCE(?4, status), updated_at = ?3
                 WHERE id = ?1",
                rusqlite::params![
                    id.as_str(),
                    error.chars().take(300).collect::<String>(),
                    now,
                    status.map(|s| s.as_str()),
                ],
            )
        })?;
        Ok(())
    }

    pub fn set_workos_user_id(&self, id: &AccountId, user_id: &str) -> Result<()> {
        self.db.with(|c| {
            c.execute(
                "UPDATE accounts SET workos_user_id = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), user_id, now_iso()],
            )
        })?;
        Ok(())
    }

    /// 记住 auto 模式下真正命中的那条渠道，下次直奔它。
    pub fn record_code_hit(&self, id: &AccountId, channel: &str) -> Result<()> {
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE accounts SET code_channel_resolved = ?2, last_code_at = ?3, updated_at = ?3
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), channel, now],
            )
        })?;
        Ok(())
    }

    fn find(&self, id: &AccountId) -> Result<Option<Account>> {
        self.db.with(|c| {
            c.query_row(
                &format!("{SELECT} WHERE id = ?1"),
                [id.as_str()],
                row_to_account,
            )
            .map(Some)
            .or_else(no_rows)
        })
    }
}

const SELECT: &str = "SELECT id, email, source, status, note, tags, membership, signup_type,
        workos_user_id, usage_json, last_checked_at, last_error, code_channel,
        code_channel_resolved, last_code_at, has_refresh, has_password,
        has_email_password, has_recovery_email, created_at, updated_at,
        has_access, access_expires_at, billing_json, has_api_key
 FROM accounts";

fn row_to_account(row: &Row<'_>) -> rusqlite::Result<Account> {
    let tags: String = row.get(5)?;
    let usage_json: Option<String> = row.get(9)?;
    Ok(Account {
        id: AccountId::from_raw(row.get::<_, String>(0)?),
        email: row.get(1)?,
        source: Source::parse(&row.get::<_, String>(2)?),
        status: Status::parse(&row.get::<_, String>(3)?),
        note: row.get(4)?,
        // 标签坏了就当没有：不该因为一个装饰字段让整行读不出来。
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        membership: row.get(6)?,
        signup_type: row.get(7)?,
        workos_user_id: row.get(8)?,
        usage: usage_json.and_then(|j| serde_json::from_str(&j).ok()),
        last_checked_at: row.get(10)?,
        last_error: row.get(11)?,
        code_channel: row.get(12)?,
        code_channel_resolved: row.get(13)?,
        last_code_at: row.get(14)?,
        has_refresh: row.get(15)?,
        has_password: row.get(16)?,
        has_email_password: row.get(17)?,
        has_recovery_email: row.get(18)?,
        created_at: row.get(19)?,
        updated_at: row.get(20)?,
        has_access: row.get(21)?,
        access_expires_at: row.get(22)?,
        billing: row
            .get::<_, Option<String>>(23)?
            .and_then(|j| serde_json::from_str(&j).ok()),
        has_api_key: row.get(24)?,
    })
}

fn no_rows<T>(err: rusqlite::Error) -> rusqlite::Result<Option<T>> {
    match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    }
}

fn not_found(id: &str) -> AppError {
    AppError::new(
        ErrorCode::AccountNotFound,
        format!("没有这个账号（{id}）。"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_store::MemorySecrets;

    fn setup() -> (Accounts, Arc<MemorySecrets>) {
        let secrets = Arc::new(MemorySecrets::new());
        let db = Arc::new(Db::open_in_memory().unwrap());
        (Accounts::new(db, secrets.clone()), secrets)
    }

    fn with_refresh(email: &str) -> NewAccount {
        NewAccount {
            email: email.into(),
            refresh_token: Some("rt-1".into()),
            ..Default::default()
        }
    }

    #[test]
    fn a_refresh_token_makes_an_account_active() {
        let (accounts, _) = setup();
        let a = accounts.upsert(with_refresh("a@example.com")).unwrap();
        assert_eq!(a.status, Status::Active);
        assert!(a.has_refresh && !a.has_password);
        assert!(a.can_query_usage() && a.can_switch());
        assert_eq!(a.code_channel, "auto");
        assert_eq!(a.source, Source::Local);
    }

    #[test]
    fn a_password_only_account_waits_for_login() {
        let (accounts, _) = setup();
        let a = accounts
            .upsert(NewAccount {
                email: "a@example.com".into(),
                cursor_password: Some("pw".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(a.status, Status::NeedsLogin);
        assert!(a.has_password && !a.has_refresh);
        assert!(!a.can_query_usage());
    }

    fn jwt_expiring_at(exp: i64) -> String {
        use base64::Engine;
        let enc = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        format!(
            "{}.{}.{}",
            enc(br#"{"alg":"HS256"}"#),
            enc(format!(r#"{{"sub":"auth0|user_42","exp":{exp}}}"#).as_bytes()),
            enc(b"sig")
        )
    }

    #[test]
    fn a_session_token_alone_is_stored_bare_and_makes_the_account_usable_until_expiry() {
        let (accounts, secrets) = setup();
        let far = (time::OffsetDateTime::now_utc() + time::Duration::hours(3)).unix_timestamp();
        let jwt = jwt_expiring_at(far);
        let a = accounts
            .upsert(NewAccount {
                email: "s@example.com".into(),
                access_token: Some(format!("user_42%3A%3A{jwt}")),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(a.status, Status::Active);
        assert!(a.session_only() && a.can_query_usage() && a.can_switch());
        assert_eq!(a.workos_user_id.as_deref(), Some("user_42"));
        assert!(a.access_expires_at.is_some());
        // 库里只有裸 JWT：前缀能从 JWT 算回来，存两份迟早对不上。
        let stored = secrets
            .get(&account_secret(&a.id, AccountSecret::Access))
            .unwrap()
            .unwrap();
        assert_eq!(stored.expose(), jwt);
    }

    #[test]
    fn an_expired_session_token_lands_the_account_in_needs_login_not_dead() {
        let (accounts, _) = setup();
        let a = accounts
            .upsert(NewAccount {
                email: "s@example.com".into(),
                access_token: Some(jwt_expiring_at(1_600_000_000)),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(a.status, Status::NeedsLogin);
        assert!(!a.can_query_usage());
        // 之后刷用量失败判 fatal，也只是待登录：粘一份新 token 就活，不该判死。
        accounts.record_failure(&a.id, "expired", true).unwrap();
        assert_eq!(accounts.get(&a.id).unwrap().status, Status::NeedsLogin);
        // 粘一份新的进来就复活。
        let far = (time::OffsetDateTime::now_utc() + time::Duration::hours(3)).unix_timestamp();
        accounts.put_access(&a.id, &jwt_expiring_at(far)).unwrap();
        assert_eq!(accounts.get(&a.id).unwrap().status, Status::Active);
    }

    #[test]
    fn an_api_key_alone_can_query_usage_but_cannot_switch() {
        let (accounts, secrets) = setup();
        let a = accounts
            .upsert(NewAccount {
                email: "k@example.com".into(),
                api_key: Some("crsr_abc123DEF".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(a.status, Status::Active);
        assert!(a.has_api_key && a.can_query_usage());
        assert!(!a.can_switch());
        let stored = secrets
            .get(&account_secret(&a.id, AccountSecret::ApiKey))
            .unwrap()
            .unwrap();
        assert_eq!(stored.expose(), "crsr_abc123DEF");
        accounts
            .put_secret(&a.id, AccountSecret::ApiKey, Some("not-a-key"))
            .unwrap_err();
        assert!(accounts.get(&a.id).unwrap().has_api_key);
    }

    #[test]
    fn api_key_usage_does_not_revive_a_dead_refresh() {
        let (accounts, _) = setup();
        let a = accounts.upsert(with_refresh("a@example.com")).unwrap();
        accounts
            .put_secret(&a.id, AccountSecret::ApiKey, Some("crsr_abc123DEF"))
            .unwrap();
        accounts
            .record_failure(&a.id, "refresh 被拒", true)
            .unwrap();
        assert_eq!(accounts.get(&a.id).unwrap().status, Status::Dead);

        accounts
            .record_usage(
                &a.id,
                &AccountUsage {
                    fetched_at: "t".into(),
                    via: Some("apiKey".into()),
                    spend_cents: Some(12.0),
                    ..Default::default()
                },
            )
            .unwrap();
        let got = accounts.get(&a.id).unwrap();
        assert_eq!(
            got.status,
            Status::Dead,
            "crsr_ 查到花费不该把已死的 refresh 复活成可切号"
        );
        assert!(!got.can_switch());
        assert_eq!(got.usage.as_ref().and_then(|u| u.spend_cents), Some(12.0));
    }

    #[test]
    fn accounts_below_the_bar_are_refused() {
        let (accounts, secrets) = setup();
        let err = accounts
            .upsert(NewAccount {
                email: "a@example.com".into(),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(accounts.list().unwrap().is_empty());
        assert_eq!(secrets.len(), 0);
    }

    #[test]
    fn listing_keeps_the_order_accounts_were_added_in() {
        // 顺序是用户的肌肉记忆（「第三个是我那个主力号」）。刷一次用量会写 updated_at，
        // 以前照它排，于是每刷新一轮列表就重排一次 —— 这里把顺序钉在「什么时候加进来的」。
        let (accounts, _) = setup();
        let first = accounts.upsert(with_refresh("first@example.com")).unwrap();
        let second = accounts.upsert(with_refresh("second@example.com")).unwrap();
        let third = accounts.upsert(with_refresh("third@example.com")).unwrap();

        let before: Vec<String> = accounts
            .list()
            .unwrap()
            .into_iter()
            .map(|a| a.email)
            .collect();
        assert_eq!(
            before,
            [
                "first@example.com",
                "second@example.com",
                "third@example.com"
            ]
        );

        // 给最早那个刷一次用量（会写 updated_at），顺序不能变。
        accounts
            .record_usage(&first.id, &AccountUsage::default())
            .unwrap();
        // 给中间那个记一次失败，同样不该换位置。
        accounts.record_failure(&second.id, "boom", false).unwrap();

        let after: Vec<String> = accounts
            .list()
            .unwrap()
            .into_iter()
            .map(|a| a.email)
            .collect();
        assert_eq!(after, before, "刷新用量 / 记录错误都不该改变列表顺序");
        assert_eq!(third.email, "third@example.com");
    }

    #[test]
    fn emails_are_normalised_so_one_account_stays_one_account() {
        let (accounts, _) = setup();
        let a = accounts.upsert(with_refresh("A@Example.COM")).unwrap();
        assert_eq!(a.email, "a@example.com");
        let b = accounts.upsert(with_refresh(" a@example.com ")).unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(accounts.list().unwrap().len(), 1);
    }

    #[test]
    fn merging_never_erases_a_credential_the_new_copy_lacks() {
        // 再导一份没有密码的清单进来，不该把本地存着的密码抹掉。
        let (accounts, _) = setup();
        let a = accounts
            .upsert(NewAccount {
                email: "a@example.com".into(),
                cursor_password: Some("pw".into()),
                email_password: Some("epw".into()),
                ..Default::default()
            })
            .unwrap();

        let merged = accounts.upsert(with_refresh("a@example.com")).unwrap();
        assert_eq!(merged.id, a.id);
        assert!(merged.has_refresh, "新凭证要落进来");
        assert!(merged.has_password, "旧密码必须还在");
        assert!(merged.has_email_password);
        assert_eq!(merged.status, Status::Active);
    }

    #[test]
    fn credential_flags_track_the_secret_store() {
        let (accounts, secrets) = setup();
        let a = accounts.upsert(with_refresh("a@example.com")).unwrap();
        assert_eq!(secrets.len(), 1);

        accounts
            .put_secret(&a.id, AccountSecret::RecoveryEmail, Some("r@example.com"))
            .unwrap();
        assert!(accounts.get(&a.id).unwrap().has_recovery_email);

        accounts
            .clear_secret(&a.id, AccountSecret::Refresh)
            .unwrap();
        let after = accounts.get(&a.id).unwrap();
        assert!(!after.has_refresh);
        assert_eq!(
            after.status,
            Status::NeedsLogin,
            "没了 refresh 就退回待登录"
        );
    }

    #[test]
    fn blank_secret_writes_are_ignored_not_treated_as_deletes() {
        let (accounts, _) = setup();
        let a = accounts.upsert(with_refresh("a@example.com")).unwrap();
        accounts
            .put_secret(&a.id, AccountSecret::Refresh, Some("  "))
            .unwrap();
        accounts
            .put_secret(&a.id, AccountSecret::Refresh, None)
            .unwrap();
        assert!(accounts.get(&a.id).unwrap().has_refresh);
        assert_eq!(
            accounts
                .require_secret(&a.id, AccountSecret::Refresh)
                .unwrap()
                .expose(),
            "rt-1"
        );
    }

    #[test]
    fn credentials_still_land_when_the_secret_store_shares_the_database() {
        // 应用里就是这么接的：秘密后端落在同一个 `Db` 上。
        //
        // 这个用例来自一次真实的死锁：`sync_credential_flags` 曾在攥着库锁的闭包里
        // 回头问 SecretStore「有没有这条」，秘密在钥匙串时无事，一换成 SQLite 就是
        // 自己等自己，导入 22 个号只写进去一条就永远挂住。
        //
        // 必须带看门狗：死锁的表现是永远不返回，直接断言是等不到失败的。
        let db = Arc::new(Db::open_in_memory().unwrap());
        let secrets: Arc<dyn SecretStore> = Arc::new(nexus_store::SqliteSecrets::new(db.clone()));
        let accounts = Accounts::new(db, secrets);

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = accounts
                .upsert(NewAccount {
                    email: "a@example.com".into(),
                    refresh_token: Some("rt".into()),
                    cursor_password: Some("pw".into()),
                    ..Default::default()
                })
                .map(|a| (a.status, a.has_refresh, a.has_password));
            let _ = tx.send(outcome);
        });

        match rx.recv_timeout(std::time::Duration::from_secs(10)) {
            Ok(Ok((status, has_refresh, has_password))) => {
                assert_eq!(status, Status::Active);
                assert!(has_refresh && has_password, "标记位要跟着凭证一起落");
            }
            Ok(Err(err)) => panic!("落库失败：{}", err.message),
            Err(_) => panic!("十秒没返回 —— 多半又在持有库锁时回头碰了 SecretStore"),
        }
    }

    #[test]
    fn a_dead_account_is_not_revived_by_merely_adding_a_credential() {
        let (accounts, _) = setup();
        let a = accounts.upsert(with_refresh("a@example.com")).unwrap();
        accounts
            .record_failure(&a.id, "refresh 被拒", true)
            .unwrap();
        assert_eq!(accounts.get(&a.id).unwrap().status, Status::Dead);

        accounts
            .put_secret(&a.id, AccountSecret::Refresh, Some("rt-2"))
            .unwrap();
        assert_eq!(
            accounts.get(&a.id).unwrap().status,
            Status::Dead,
            "复活要靠一次真正成功的刷新，不是靠补一个字符串"
        );
    }

    #[test]
    fn a_fatal_failure_falls_back_to_needs_login_when_a_password_exists() {
        let (accounts, _) = setup();
        let a = accounts
            .upsert(NewAccount {
                email: "a@example.com".into(),
                refresh_token: Some("rt".into()),
                cursor_password: Some("pw".into()),
                ..Default::default()
            })
            .unwrap();
        accounts
            .record_failure(&a.id, "refresh 被拒", true)
            .unwrap();
        let after = accounts.get(&a.id).unwrap();
        assert_eq!(
            after.status,
            Status::NeedsLogin,
            "有密码就还能 OAuth 救回来"
        );
        assert_eq!(after.last_error.as_deref(), Some("refresh 被拒"));
    }

    #[test]
    fn a_transient_failure_leaves_the_status_alone() {
        let (accounts, _) = setup();
        let a = accounts.upsert(with_refresh("a@example.com")).unwrap();
        accounts.record_failure(&a.id, "网络超时", false).unwrap();
        let after = accounts.get(&a.id).unwrap();
        assert_eq!(after.status, Status::Active);
        assert!(after.last_error.is_some());
        assert!(after.last_checked_at.is_some());
    }

    #[test]
    fn long_error_messages_are_truncated() {
        let (accounts, _) = setup();
        let a = accounts.upsert(with_refresh("a@example.com")).unwrap();
        accounts
            .record_failure(&a.id, &"错".repeat(500), false)
            .unwrap();
        assert_eq!(
            accounts
                .get(&a.id)
                .unwrap()
                .last_error
                .unwrap()
                .chars()
                .count(),
            300
        );
    }

    #[test]
    fn recording_usage_clears_the_previous_error() {
        let (accounts, _) = setup();
        let a = accounts.upsert(with_refresh("a@example.com")).unwrap();
        accounts.record_failure(&a.id, "网络超时", false).unwrap();

        let usage = AccountUsage {
            fetched_at: now_iso(),
            plan: Some("ultra".into()),
            total_percent_used: Some(42.0),
            ..Default::default()
        };
        accounts.record_usage(&a.id, &usage).unwrap();

        let after = accounts.get(&a.id).unwrap();
        assert!(after.last_error.is_none());
        assert_eq!(after.membership.as_deref(), Some("ultra"));
        assert_eq!(after.usage.unwrap().total_percent_used, Some(42.0));
        assert_eq!(after.status, Status::Active);
    }

    #[test]
    fn recording_billing_does_not_touch_the_usage_error() {
        let (accounts, _) = setup();
        let a = accounts.upsert(with_refresh("a@example.com")).unwrap();
        accounts.record_failure(&a.id, "网络超时", false).unwrap();

        let billing = AccountBilling {
            fetched_at: now_iso(),
            discount_state: crate::billing::DiscountState::None,
            list_price: Some(2000),
            current_amount: Some(2000),
            ..Default::default()
        };
        accounts.record_billing(&a.id, &billing).unwrap();

        let after = accounts.get(&a.id).unwrap();
        assert_eq!(after.last_error.as_deref(), Some("网络超时"));
        assert_eq!(after.billing.as_ref().unwrap().list_price, Some(2000));
        assert_eq!(
            after.billing.as_ref().unwrap().discount_state,
            crate::billing::DiscountState::None
        );
    }

    #[test]
    fn patch_updates_only_what_it_carries() {
        let (accounts, _) = setup();
        let a = accounts.upsert(with_refresh("a@example.com")).unwrap();
        accounts
            .patch(
                &a.id,
                AccountPatch {
                    note: Some("买家甲".into()),
                    tags: Some(vec!["自用".into(), "ultra".into()]),
                    ..Default::default()
                },
            )
            .unwrap();
        let after = accounts.patch(&a.id, AccountPatch::default()).unwrap();
        assert_eq!(after.note.as_deref(), Some("买家甲"));
        assert_eq!(after.tags, vec!["自用", "ultra"]);
        assert_eq!(after.code_channel, "auto");
    }

    #[test]
    fn remembering_a_code_channel_survives_a_reload() {
        let (accounts, _) = setup();
        let a = accounts.upsert(with_refresh("a@example.com")).unwrap();
        accounts.record_code_hit(&a.id, "nexus").unwrap();
        let after = accounts.get(&a.id).unwrap();
        assert_eq!(after.code_channel_resolved.as_deref(), Some("nexus"));
        assert!(after.last_code_at.is_some());
    }

    #[test]
    fn removing_an_account_clears_every_secret_it_owned() {
        let (accounts, secrets) = setup();
        let a = accounts
            .upsert(NewAccount {
                email: "a@example.com".into(),
                refresh_token: Some("rt".into()),
                cursor_password: Some("pw".into()),
                email_password: Some("epw".into()),
                recovery_email: Some("r@example.com".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(secrets.len(), 4);
        accounts.remove(&a.id).unwrap();
        assert_eq!(secrets.len(), 0, "秘密存储里不能留孤儿凭证");
        assert!(accounts.list().unwrap().is_empty());
    }

    #[test]
    fn missing_accounts_report_account_not_found() {
        let (accounts, _) = setup();
        let ghost = AccountId::from_raw("nope");
        assert_eq!(
            accounts.get(&ghost).unwrap_err().code,
            ErrorCode::AccountNotFound
        );
        assert_eq!(
            accounts.remove(&ghost).unwrap_err().code,
            ErrorCode::AccountNotFound
        );
        assert!(accounts.by_email("nobody@example.com").unwrap().is_none());
    }

    #[test]
    fn purchased_accounts_keep_their_provenance() {
        let (accounts, _) = setup();
        let a = accounts
            .upsert(NewAccount {
                email: "bought@example.com".into(),
                refresh_token: Some("rt".into()),
                source: Some(Source::Purchased),
                note: Some("订单 #123".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(a.source, Source::Purchased);
        assert_eq!(a.note.as_deref(), Some("订单 #123"));
    }

    #[test]
    fn the_accounts_table_never_holds_a_secret() {
        let (accounts, _) = setup();
        let a = accounts
            .upsert(NewAccount {
                email: "a@example.com".into(),
                refresh_token: Some("SUPER-SECRET-RT".into()),
                cursor_password: Some("SUPER-SECRET-PW".into()),
                ..Default::default()
            })
            .unwrap();
        accounts
            .record_usage(
                &a.id,
                &AccountUsage {
                    fetched_at: now_iso(),
                    ..Default::default()
                },
            )
            .unwrap();

        // 把整张表 dump 成文本，逐字搜。
        let dumped: Vec<String> = accounts
            .db
            .with(|c| {
                let mut stmt = c.prepare("SELECT * FROM accounts")?;
                let n = stmt.column_count();
                let rows = stmt.query_map([], |row| {
                    let mut cells = Vec::new();
                    for i in 0..n {
                        cells.push(format!("{:?}", row.get_ref(i)?));
                    }
                    Ok(cells.join("|"))
                })?;
                rows.collect()
            })
            .unwrap();
        let text = dumped.join("\n");
        assert!(
            !text.contains("SUPER-SECRET"),
            "凭证泄漏进了 SQLite：{text}"
        );
        // 序列化给前端的那份同样干净。
        let json = serde_json::to_string(&accounts.list().unwrap()).unwrap();
        assert!(!json.contains("SUPER-SECRET"));
    }
}
