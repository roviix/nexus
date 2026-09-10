//! ChatGPT 账号仓库：`chatgpt_accounts` 表存元信息，凭证交给 `SecretStore`。
//!
//! 同一条不变量：**表里没有任何秘密。** `has_refresh` 是秘密存储状态的投影，写凭证和写标记
//! 必须一起发生。持有库锁期间不碰 `SecretStore`（两者是同一个 SQLite 连接，里外嵌套就是自锁）。

use crate::model::{ChatGptAccount, ChatGptStatus};
use crate::oauth::{Identity, TokenSet};
use crate::protocol::CodexUsage;
use nexus_core::{now_iso, AppError, ChatGptAccountId, ErrorCode, Result, Secret};
use nexus_store::keys::{chatgpt_secret, ChatGptSecret};
use nexus_store::{Db, SecretStore};
use rusqlite::Row;
use std::sync::Arc;
use time::format_description::well_known::Rfc3339;

pub struct ChatGptAccounts {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
}

/// `upsert` 的结果：是新建的还是更新了已有的（同一个 `chatgpt_account_id` 再授权一次是更新）。
#[derive(Debug, Clone)]
pub struct Upserted {
    pub account: ChatGptAccount,
    pub created: bool,
}

impl ChatGptAccounts {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { db, secrets }
    }

    /// 全部账号，按加入先后（理由同 Cursor 账号仓库：顺序不能随刷新漂）。
    pub fn list(&self) -> Result<Vec<ChatGptAccount>> {
        self.db.with(|c| {
            let mut stmt = c.prepare(&format!("{SELECT} ORDER BY created_at ASC, id ASC"))?;
            let rows = stmt.query_map([], row_to_account)?;
            rows.collect()
        })
    }

    pub fn get(&self, id: &ChatGptAccountId) -> Result<ChatGptAccount> {
        self.find(id)?.ok_or_else(|| not_found(id.as_str()))
    }

    pub fn by_ref(&self, account_ref: &str) -> Result<Option<ChatGptAccount>> {
        self.db.with(|c| {
            c.query_row(
                &format!("{SELECT} WHERE account_ref = ?1"),
                [account_ref.trim()],
                row_to_account,
            )
            .map(Some)
            .or_else(no_rows)
        })
    }

    /// 新增或按 `chatgpt_account_id` 合并，并把这组 token 落库。
    ///
    /// 合并时**只覆盖带值的凭证**：刷新响应可能不带新 refresh token（沿用旧的），
    /// 导入一份只有 access_token 的清单也不该把本地的 refresh 抹掉。
    pub fn upsert(
        &self,
        identity: &Identity,
        tokens: &TokenSet,
        note: Option<&str>,
    ) -> Result<Upserted> {
        let account_ref = identity
            .account_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::invalid("这组 token 里没有 chatgpt_account_id：不是 ChatGPT 订阅账号，用不了 Codex。")
                    .with_hint("纯平台 API 账号（没有 ChatGPT Plus / Pro / Team）的 token 就是这样。换一个有订阅的账号登录。")
            })?;
        let email = identity
            .email
            .as_deref()
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty());
        let now = now_iso();

        let (id, created) = match self.by_ref(account_ref)? {
            Some(existing) => (existing.id, false),
            None => {
                let id = ChatGptAccountId::new();
                self.db.with(|c| {
                    c.execute(
                        "INSERT INTO chatgpt_accounts (id, account_ref, email, plan_type, status, enabled, note, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?7, ?7)",
                        rusqlite::params![
                            id.as_str(),
                            account_ref,
                            email.as_deref(),
                            identity.plan_type.as_deref(),
                            ChatGptStatus::NeedsLogin.as_str(),
                            note,
                            &now,
                        ],
                    )
                })?;
                (id, true)
            }
        };

        // 再授权 / 刷新带来的新身份信息覆盖旧的（邮箱、套餐可能变）。
        self.db.with(|c| {
            c.execute(
                "UPDATE chatgpt_accounts SET
                   email     = COALESCE(?2, email),
                   plan_type = COALESCE(?3, plan_type),
                   note      = COALESCE(?4, note),
                   updated_at = ?5
                 WHERE id = ?1",
                rusqlite::params![
                    id.as_str(),
                    email.as_deref(),
                    identity.plan_type.as_deref(),
                    note,
                    &now,
                ],
            )
        })?;
        self.store_tokens(&id, tokens)?;
        Ok(Upserted {
            account: self.get(&id)?,
            created,
        })
    }

    /// 落一组 token。刷新之后调它：refresh token 轮换了不马上存，这个号就再也刷不动了。
    /// 存完这个号一定是 `active`——刚拿到一组新鲜 token 就是「能用」的最强证据。
    pub fn store_tokens(&self, id: &ChatGptAccountId, tokens: &TokenSet) -> Result<()> {
        self.secrets.set(
            &chatgpt_secret(id, ChatGptSecret::Access),
            &tokens.access_token,
        )?;
        if let Some(rt) = &tokens.refresh_token {
            self.secrets
                .set(&chatgpt_secret(id, ChatGptSecret::Refresh), rt)?;
        }
        if let Some(idt) = &tokens.id_token {
            self.secrets
                .set(&chatgpt_secret(id, ChatGptSecret::IdToken), idt)?;
        }
        let has_refresh = self
            .secrets
            .exists(&chatgpt_secret(id, ChatGptSecret::Refresh));
        let expires = tokens.expires_at.format(&Rfc3339).unwrap_or_default();
        self.db.with(|c| {
            c.execute(
                "UPDATE chatgpt_accounts SET
                   has_refresh = ?2, access_expires_at = ?3, status = 'active',
                   last_error = NULL, updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), has_refresh, expires, now_iso()],
            )
        })?;
        Ok(())
    }

    /// 读一条凭证。**唯一**的明文出口。
    pub fn secret(&self, id: &ChatGptAccountId, kind: ChatGptSecret) -> Result<Option<Secret>> {
        self.secrets.get(&chatgpt_secret(id, kind))
    }

    pub fn remove(&self, id: &ChatGptAccountId) -> Result<()> {
        let n = self
            .db
            .with(|c| c.execute("DELETE FROM chatgpt_accounts WHERE id = ?1", [id.as_str()]))?;
        if n == 0 {
            return Err(not_found(id.as_str()));
        }
        for kind in ChatGptSecret::ALL {
            self.secrets.delete(&chatgpt_secret(id, kind))?;
        }
        Ok(())
    }

    pub fn set_enabled(&self, id: &ChatGptAccountId, enabled: bool) -> Result<ChatGptAccount> {
        self.db.with(|c| {
            c.execute(
                "UPDATE chatgpt_accounts SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), enabled, now_iso()],
            )
        })?;
        self.get(id)
    }

    /// 刷新响应里带回来的身份（邮箱、套餐可能变）。只更新带值的字段。
    pub fn update_identity(&self, id: &ChatGptAccountId, identity: &Identity) -> Result<()> {
        let email = identity
            .email
            .as_deref()
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty());
        self.db.with(|c| {
            c.execute(
                "UPDATE chatgpt_accounts SET
                   email = COALESCE(?2, email), plan_type = COALESCE(?3, plan_type), updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![
                    id.as_str(),
                    email.as_deref(),
                    identity.plan_type.as_deref(),
                    now_iso()
                ],
            )
        })?;
        Ok(())
    }

    pub fn set_note(&self, id: &ChatGptAccountId, note: Option<&str>) -> Result<ChatGptAccount> {
        self.db.with(|c| {
            c.execute(
                "UPDATE chatgpt_accounts SET note = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), note, now_iso()],
            )
        })?;
        self.get(id)
    }

    /// 记一次额度快照（来自响应头或 `/wham/usage`）。快照里带套餐就一并更新。
    pub fn record_usage(&self, id: &ChatGptAccountId, usage: &CodexUsage) -> Result<()> {
        let json = serde_json::to_string(usage)?;
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE chatgpt_accounts SET
                   usage_json = ?2, plan_type = COALESCE(?3, plan_type),
                   last_checked_at = ?4, updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), json, usage.plan_type.as_deref(), now],
            )
        })?;
        Ok(())
    }

    /// 记一次失败。`status` 给 `Some` 时同时改状态：refresh token 作废 → `NeedsLogin`，
    /// 账号被停用 → `Dead`；网络抖动这类不动状态。
    pub fn record_failure(
        &self,
        id: &ChatGptAccountId,
        error: &str,
        status: Option<ChatGptStatus>,
    ) -> Result<()> {
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE chatgpt_accounts SET
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

    fn find(&self, id: &ChatGptAccountId) -> Result<Option<ChatGptAccount>> {
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

const SELECT: &str = "SELECT id, account_ref, email, plan_type, status, enabled, note, usage_json,
        last_checked_at, last_error, has_refresh, access_expires_at, created_at, updated_at
 FROM chatgpt_accounts";

fn row_to_account(row: &Row<'_>) -> rusqlite::Result<ChatGptAccount> {
    let usage_json: Option<String> = row.get(7)?;
    let status: String = row.get(4)?;
    Ok(ChatGptAccount {
        id: ChatGptAccountId::from_raw(row.get::<_, String>(0)?),
        account_ref: row.get(1)?,
        email: row.get(2)?,
        plan_type: row.get(3)?,
        status: ChatGptStatus::parse(&status),
        enabled: row.get::<_, i64>(5)? != 0,
        note: row.get(6)?,
        usage: usage_json.and_then(|j| serde_json::from_str(&j).ok()),
        last_checked_at: row.get(8)?,
        last_error: row.get(9)?,
        has_refresh: row.get::<_, i64>(10)? != 0,
        access_expires_at: row.get(11)?,
        created_at: row.get(12)?,
        updated_at: row.get(13)?,
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
        format!("没有这个 ChatGPT 账号：{id}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_store::MemorySecrets;
    use time::OffsetDateTime;

    fn repo() -> ChatGptAccounts {
        ChatGptAccounts::new(
            Arc::new(Db::open_in_memory().unwrap()),
            Arc::new(MemorySecrets::new()),
        )
    }

    fn tokens(access: &str, refresh: Option<&str>) -> TokenSet {
        TokenSet {
            access_token: Secret::new(access),
            refresh_token: refresh.map(Secret::new),
            id_token: None,
            expires_at: OffsetDateTime::from_unix_timestamp(4_000_000_000).unwrap(),
        }
    }

    fn who(account_id: &str, email: &str) -> Identity {
        Identity {
            email: Some(email.into()),
            account_id: Some(account_id.into()),
            plan_type: Some("plus".into()),
            ..Default::default()
        }
    }

    #[test]
    fn upsert_creates_once_per_account_ref_and_never_puts_secrets_in_the_table() {
        let r = repo();
        let first = r
            .upsert(
                &who("acct_1", "Alice@Example.com"),
                &tokens("at-1", Some("rt-1")),
                None,
            )
            .unwrap();
        assert!(first.created);
        assert_eq!(
            first.account.email.as_deref(),
            Some("alice@example.com"),
            "邮箱统一小写"
        );
        assert_eq!(first.account.status, ChatGptStatus::Active);
        assert!(first.account.has_refresh);
        assert!(first.account.enabled, "默认进网关");
        assert!(first
            .account
            .access_expires_at
            .as_deref()
            .unwrap()
            .starts_with("2096-"));

        // 同一个号再授权：更新不新建；只有 access 的那次不抹掉 refresh。
        let again = r
            .upsert(
                &who("acct_1", "alice@example.com"),
                &tokens("at-2", None),
                Some("主力"),
            )
            .unwrap();
        assert!(!again.created);
        assert_eq!(again.account.id, first.account.id);
        assert_eq!(again.account.note.as_deref(), Some("主力"));
        assert_eq!(r.list().unwrap().len(), 1);
        assert_eq!(
            r.secret(&first.account.id, ChatGptSecret::Refresh)
                .unwrap()
                .unwrap()
                .expose(),
            "rt-1"
        );
        assert_eq!(
            r.secret(&first.account.id, ChatGptSecret::Access)
                .unwrap()
                .unwrap()
                .expose(),
            "at-2"
        );

        // 表里一个 token 都不能有。
        let dump: Vec<String> =
            r.db.with(|c| {
                let mut stmt = c.prepare("SELECT * FROM chatgpt_accounts")?;
                let n = stmt.column_count();
                let rows = stmt.query_map([], |row| {
                    let mut out = String::new();
                    for i in 0..n {
                        if let Ok(v) = row.get::<_, String>(i) {
                            out.push_str(&v);
                        }
                    }
                    Ok(out)
                })?;
                rows.collect()
            })
            .unwrap();
        assert!(!dump.iter().any(|s| s.contains("at-") || s.contains("rt-")));
    }

    #[test]
    fn tokens_without_an_account_id_are_refused() {
        let r = repo();
        let err = r
            .upsert(
                &Identity {
                    email: Some("noplan@example.com".into()),
                    ..Default::default()
                },
                &tokens("at", Some("rt")),
                None,
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.message.contains("chatgpt_account_id"));
        assert!(r.list().unwrap().is_empty());
    }

    #[test]
    fn usage_failures_and_removal_are_recorded_and_secrets_go_with_the_row() {
        let r = repo();
        let a = r
            .upsert(&who("acct_1", "a@x.com"), &tokens("at", Some("rt")), None)
            .unwrap()
            .account;
        let usage = CodexUsage {
            plan_type: Some("pro".into()),
            checked_at: "t".into(),
            source: "wham/usage".into(),
            ..Default::default()
        };
        r.record_usage(&a.id, &usage).unwrap();
        let got = r.get(&a.id).unwrap();
        assert_eq!(got.usage.unwrap().source, "wham/usage");
        assert_eq!(got.plan_type.as_deref(), Some("pro"), "快照里的套餐覆盖");

        r.record_failure(&a.id, "网络抖动", None).unwrap();
        assert_eq!(
            r.get(&a.id).unwrap().status,
            ChatGptStatus::Active,
            "不动状态"
        );
        r.record_failure(
            &a.id,
            "refresh_token_reused",
            Some(ChatGptStatus::NeedsLogin),
        )
        .unwrap();
        assert_eq!(r.get(&a.id).unwrap().status, ChatGptStatus::NeedsLogin);
        // 新 token 落库 → 复活。
        r.store_tokens(&a.id, &tokens("at-3", Some("rt-3")))
            .unwrap();
        let back = r.get(&a.id).unwrap();
        assert_eq!(back.status, ChatGptStatus::Active);
        assert!(back.last_error.is_none());

        assert!(!r.set_enabled(&a.id, false).unwrap().enabled);
        r.remove(&a.id).unwrap();
        assert!(r.list().unwrap().is_empty());
        assert!(r.secret(&a.id, ChatGptSecret::Refresh).unwrap().is_none());
        assert_eq!(
            r.remove(&a.id).unwrap_err().code,
            ErrorCode::AccountNotFound
        );
    }
}
