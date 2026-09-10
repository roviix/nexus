//! Grok 账号仓库：`grok_accounts` 表存元信息，凭证交给 `SecretStore`。
//!
//! 同一条不变量：**表里没有任何秘密。** `has_refresh` 是秘密存储状态的投影。

use crate::model::{GrokAccount, GrokAuthKind, GrokStatus};
use crate::oauth::{Identity, TokenSet};
use crate::quota::{media_eligibility_from_tier, GrokQuota};
use nexus_core::{now_iso, AppError, ErrorCode, GrokAccountId, Result, Secret};
use nexus_store::keys::{grok_secret, GrokSecret};
use nexus_store::{Db, SecretStore};
use rusqlite::Row;
use std::sync::Arc;
use time::format_description::well_known::Rfc3339;

pub struct GrokAccounts {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
}

#[derive(Debug, Clone)]
pub struct Upserted {
    pub account: GrokAccount,
    pub created: bool,
}

impl GrokAccounts {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { db, secrets }
    }

    pub fn list(&self) -> Result<Vec<GrokAccount>> {
        self.db.with(|c| {
            let mut stmt = c.prepare(&format!("{SELECT} ORDER BY created_at ASC, id ASC"))?;
            let rows = stmt.query_map([], row_to_account)?;
            rows.collect()
        })
    }

    pub fn get(&self, id: &GrokAccountId) -> Result<GrokAccount> {
        self.find(id)?.ok_or_else(|| not_found(id.as_str()))
    }

    pub fn by_ref(&self, account_ref: &str) -> Result<Option<GrokAccount>> {
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

    /// 新增或按 JWT `sub` 合并。刷新响应可能不带新 refresh，导入只有 access 时也不抹旧 refresh。
    pub fn upsert(
        &self,
        identity: &Identity,
        tokens: &TokenSet,
        note: Option<&str>,
    ) -> Result<Upserted> {
        let account_ref = identity.subject.trim();
        if account_ref.is_empty() {
            return Err(
                AppError::invalid("这组 token 里没有 sub：不是 xAI 订阅账号。")
                    .with_hint("换一组 Grok CLI / grok login 拿到的凭证。"),
            );
        }
        let email = identity
            .email
            .as_deref()
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty());
        let now = now_iso();

        let (id, created) = match self.by_ref(account_ref)? {
            Some(existing) => (existing.id, false),
            None => {
                let id = GrokAccountId::new();
                self.db.with(|c| {
                    c.execute(
                        "INSERT INTO grok_accounts (id, account_ref, email, status, enabled, note, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?6)",
                        rusqlite::params![
                            id.as_str(),
                            account_ref,
                            email.as_deref(),
                            GrokStatus::NeedsLogin.as_str(),
                            note,
                            &now,
                        ],
                    )
                })?;
                (id, true)
            }
        };

        self.db.with(|c| {
            c.execute(
                "UPDATE grok_accounts SET
                   email = COALESCE(?2, email),
                   note = COALESCE(?3, note),
                   updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), email.as_deref(), note, &now],
            )
        })?;
        self.store_tokens(&id, tokens)?;
        Ok(Upserted {
            account: self.get(&id)?,
            created,
        })
    }

    pub fn store_tokens(&self, id: &GrokAccountId, tokens: &TokenSet) -> Result<()> {
        self.secrets
            .set(&grok_secret(id, GrokSecret::Access), &tokens.access_token)?;
        if let Some(rt) = &tokens.refresh_token {
            self.secrets
                .set(&grok_secret(id, GrokSecret::Refresh), rt)?;
        }
        if let Some(idt) = &tokens.id_token {
            self.secrets
                .set(&grok_secret(id, GrokSecret::IdToken), idt)?;
        }
        let has_refresh = self.secrets.exists(&grok_secret(id, GrokSecret::Refresh));
        let expires = tokens.expires_at.and_then(|t| t.format(&Rfc3339).ok());
        self.db.with(|c| {
            c.execute(
                "UPDATE grok_accounts SET
                   has_refresh = ?2, access_expires_at = ?3, status = 'active',
                   last_error = NULL, updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), has_refresh, expires, now_iso()],
            )
        })?;
        Ok(())
    }

    pub fn secret(&self, id: &GrokAccountId, kind: GrokSecret) -> Result<Option<Secret>> {
        self.secrets.get(&grok_secret(id, kind))
    }

    pub fn remove(&self, id: &GrokAccountId) -> Result<()> {
        let n = self
            .db
            .with(|c| c.execute("DELETE FROM grok_accounts WHERE id = ?1", [id.as_str()]))?;
        if n == 0 {
            return Err(not_found(id.as_str()));
        }
        for kind in GrokSecret::ALL {
            self.secrets.delete(&grok_secret(id, kind))?;
        }
        Ok(())
    }

    pub fn set_enabled(&self, id: &GrokAccountId, enabled: bool) -> Result<GrokAccount> {
        self.db.with(|c| {
            c.execute(
                "UPDATE grok_accounts SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), enabled, now_iso()],
            )
        })?;
        self.get(id)
    }

    pub fn update_identity(&self, id: &GrokAccountId, identity: &Identity) -> Result<()> {
        let email = identity
            .email
            .as_deref()
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty());
        self.db.with(|c| {
            c.execute(
                "UPDATE grok_accounts SET
                   email = COALESCE(?2, email), updated_at = ?3
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), email.as_deref(), now_iso()],
            )
        })?;
        Ok(())
    }

    pub fn set_note(&self, id: &GrokAccountId, note: Option<&str>) -> Result<GrokAccount> {
        self.db.with(|c| {
            c.execute(
                "UPDATE grok_accounts SET note = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), note, now_iso()],
            )
        })?;
        self.get(id)
    }

    /// 新增一个 API Key 号。`account_ref` 是 `GET /v1/api-key` 回的 key id（拿不到就用 key 的指纹）。
    /// key 只进 secrets；`has_refresh` 在这类号上的意思是「key 在」。
    pub fn upsert_api_key(
        &self,
        account_ref: &str,
        display: Option<&str>,
        api_key: &Secret,
        note: Option<&str>,
    ) -> Result<Upserted> {
        let account_ref = account_ref.trim();
        if account_ref.is_empty() {
            return Err(AppError::invalid("API Key 账号缺少身份标识。"));
        }
        let now = now_iso();
        let (id, created) = match self.by_ref(account_ref)? {
            Some(existing) => (existing.id, false),
            None => {
                let id = GrokAccountId::new();
                self.db.with(|c| {
                    c.execute(
                        "INSERT INTO grok_accounts (id, account_ref, email, status, enabled, note, auth_kind, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, 1, ?5, 'api_key', ?6, ?6)",
                        rusqlite::params![
                            id.as_str(),
                            account_ref,
                            display,
                            GrokStatus::Active.as_str(),
                            note,
                            &now,
                        ],
                    )
                })?;
                (id, true)
            }
        };
        self.secrets
            .set(&grok_secret(&id, GrokSecret::ApiKey), api_key)?;
        self.db.with(|c| {
            c.execute(
                "UPDATE grok_accounts SET
                   email = COALESCE(?2, email), note = COALESCE(?3, note),
                   auth_kind = 'api_key', has_refresh = 1, status = 'active', last_error = NULL,
                   media_probe = 1, updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), display, note, &now],
            )
        })?;
        Ok(Upserted {
            account: self.get(&id)?,
            created,
        })
    }

    /// 写入一份额度快照；档位能推出媒体资格时顺手记下探测结果。
    pub fn store_quota(&self, id: &GrokAccountId, quota: &GrokQuota) -> Result<GrokAccount> {
        let json = serde_json::to_string(quota)?;
        let tier = quota.subscription_tier.clone();
        let probe = media_eligibility_from_tier(tier.as_deref());
        self.db.with(|c| {
            c.execute(
                "UPDATE grok_accounts SET
                   usage_json = ?2, subscription_tier = COALESCE(?3, subscription_tier),
                   plan_type = COALESCE(?3, plan_type),
                   media_probe = COALESCE(?4, media_probe),
                   last_checked_at = ?5, updated_at = ?5
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), json, tier, probe.map(|b| b as i64), now_iso()],
            )
        })?;
        self.get(id)
    }

    /// 媒体请求撞回来的结果：402 / 403 记「不能」，成功记「能」。
    pub fn record_media_probe(&self, id: &GrokAccountId, eligible: bool) -> Result<()> {
        self.db.with(|c| {
            c.execute(
                "UPDATE grok_accounts SET media_probe = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), eligible as i64, now_iso()],
            )
        })?;
        Ok(())
    }

    pub fn set_media_override(
        &self,
        id: &GrokAccountId,
        value: Option<bool>,
    ) -> Result<GrokAccount> {
        self.db.with(|c| {
            c.execute(
                "UPDATE grok_accounts SET media_override = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), value.map(|b| b as i64), now_iso()],
            )
        })?;
        self.get(id)
    }

    pub fn record_failure(
        &self,
        id: &GrokAccountId,
        error: &str,
        status: Option<GrokStatus>,
    ) -> Result<()> {
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE grok_accounts SET
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

    fn find(&self, id: &GrokAccountId) -> Result<Option<GrokAccount>> {
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

const SELECT: &str = "SELECT id, account_ref, email, plan_type, status, enabled, note,
        last_checked_at, last_error, has_refresh, access_expires_at, created_at, updated_at,
        auth_kind, usage_json, media_probe, media_override, subscription_tier
 FROM grok_accounts";

fn row_to_account(row: &Row<'_>) -> rusqlite::Result<GrokAccount> {
    let status: String = row.get(4)?;
    let auth_kind: String = row.get(13)?;
    let usage: Option<GrokQuota> = row
        .get::<_, Option<String>>(14)?
        .and_then(|s| serde_json::from_str(&s).ok());
    let media_probe = row.get::<_, Option<i64>>(15)?.map(|v| v != 0);
    let media_override = row.get::<_, Option<i64>>(16)?.map(|v| v != 0);
    Ok(GrokAccount {
        id: GrokAccountId::from_raw(row.get::<_, String>(0)?),
        account_ref: row.get(1)?,
        email: row.get(2)?,
        plan_type: row.get(3)?,
        status: GrokStatus::parse(&status),
        enabled: row.get::<_, i64>(5)? != 0,
        note: row.get(6)?,
        auth_kind: GrokAuthKind::parse(&auth_kind),
        subscription_tier: row.get(17)?,
        last_checked_at: row.get(7)?,
        last_error: row.get(8)?,
        has_refresh: row.get::<_, i64>(9)? != 0,
        access_expires_at: row.get(10)?,
        usage,
        media_probe,
        media_override,
        media_eligible: media_override.or(media_probe),
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
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
        format!("没有这个 Grok 账号：{id}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_store::MemorySecrets;
    use time::OffsetDateTime;

    fn repo() -> GrokAccounts {
        GrokAccounts::new(
            Arc::new(Db::open_in_memory().unwrap()),
            Arc::new(MemorySecrets::new()),
        )
    }

    fn tokens(access: &str, refresh: Option<&str>) -> TokenSet {
        TokenSet {
            access_token: Secret::new(access),
            refresh_token: refresh.map(Secret::new),
            id_token: None,
            expires_at: Some(OffsetDateTime::from_unix_timestamp(4_000_000_000).unwrap()),
        }
    }

    fn who(sub: &str, email: &str) -> Identity {
        Identity {
            subject: sub.into(),
            email: Some(email.into()),
        }
    }

    #[test]
    fn upsert_creates_once_per_sub_and_never_puts_secrets_in_the_table() {
        let r = repo();
        let first = r
            .upsert(
                &who("user_1", "Alice@X.AI"),
                &tokens("at-1", Some("rt-1")),
                None,
            )
            .unwrap();
        assert!(first.created);
        assert_eq!(first.account.email.as_deref(), Some("alice@x.ai"));
        assert_eq!(first.account.status, GrokStatus::Active);
        assert!(first.account.has_refresh);

        let again = r
            .upsert(
                &who("user_1", "alice@x.ai"),
                &tokens("at-2", None),
                Some("主力"),
            )
            .unwrap();
        assert!(!again.created);
        assert_eq!(again.account.id, first.account.id);
        assert_eq!(again.account.note.as_deref(), Some("主力"));
        assert_eq!(
            r.secret(&first.account.id, GrokSecret::Refresh)
                .unwrap()
                .unwrap()
                .expose(),
            "rt-1"
        );

        let dump: Vec<String> =
            r.db.with(|c| {
                let mut stmt = c.prepare("SELECT * FROM grok_accounts")?;
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
    fn tokens_without_sub_are_refused() {
        let r = repo();
        let err = r
            .upsert(&Identity::default(), &tokens("at", Some("rt")), None)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(r.list().unwrap().is_empty());
    }

    #[test]
    fn api_key_accounts_are_usable_and_media_capable_without_a_refresh_token() {
        let r = repo();
        let up = r
            .upsert_api_key("key_1", Some("team-a"), &Secret::new("xai-secret"), None)
            .unwrap();
        assert!(up.created);
        assert_eq!(up.account.auth_kind, GrokAuthKind::ApiKey);
        assert!(up.account.usable());
        assert_eq!(up.account.media_eligible, Some(true));
        assert!(r
            .secret(&up.account.id, GrokSecret::Refresh)
            .unwrap()
            .is_none());
        assert_eq!(
            r.secret(&up.account.id, GrokSecret::ApiKey)
                .unwrap()
                .unwrap()
                .expose(),
            "xai-secret"
        );
    }

    #[test]
    fn quota_snapshot_updates_tier_and_media_probe_but_override_wins() {
        let r = repo();
        let up = r
            .upsert(&who("u", "u@x.ai"), &tokens("at", Some("rt")), None)
            .unwrap();
        let id = up.account.id.clone();
        let q = GrokQuota {
            credit_usage_percent: Some(12.0),
            subscription_tier: Some("Free".into()),
            ..Default::default()
        };
        let a = r.store_quota(&id, &q).unwrap();
        assert_eq!(a.usage.as_ref().unwrap().credit_usage_percent, Some(12.0));
        assert_eq!(a.subscription_tier.as_deref(), Some("Free"));
        assert_eq!(a.media_eligible, Some(false));
        let a = r.set_media_override(&id, Some(true)).unwrap();
        assert_eq!(a.media_eligible, Some(true));
        assert_eq!(a.media_probe, Some(false));
        let a = r.set_media_override(&id, None).unwrap();
        assert_eq!(a.media_eligible, Some(false));
    }
}
