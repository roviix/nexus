//! Kiro 账号仓库：`kiro_accounts` 表存元信息，凭证交给 `SecretStore`。

use crate::model::{KiroAccount, KiroStatus};
use crate::oauth::{Identity, TokenSet};
use nexus_core::{now_iso, AppError, ErrorCode, KiroAccountId, Result, Secret};
use nexus_store::keys::{kiro_secret, KiroSecret};
use nexus_store::{Db, SecretStore};
use rusqlite::Row;
use std::sync::Arc;
use time::format_description::well_known::Rfc3339;

pub struct KiroAccounts {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
}

#[derive(Debug, Clone)]
pub struct Upserted {
    pub account: KiroAccount,
    pub created: bool,
}

impl KiroAccounts {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { db, secrets }
    }

    pub fn list(&self) -> Result<Vec<KiroAccount>> {
        self.db.with(|c| {
            let mut stmt = c.prepare(&format!("{SELECT} ORDER BY created_at ASC, id ASC"))?;
            let rows = stmt.query_map([], row_to_account)?;
            rows.collect()
        })
    }

    pub fn get(&self, id: &KiroAccountId) -> Result<KiroAccount> {
        self.find(id)?.ok_or_else(|| not_found(id.as_str()))
    }

    pub fn by_ref(&self, account_ref: &str) -> Result<Option<KiroAccount>> {
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

    pub fn upsert(
        &self,
        identity: &Identity,
        tokens: &TokenSet,
        note: Option<&str>,
    ) -> Result<Upserted> {
        let account_ref = identity.subject.trim();
        if account_ref.is_empty() {
            return Err(
                AppError::invalid("这组 token 里没有稳定身份（JWT sub / profileArn）。")
                    .with_hint("换一份 Kiro IDE 的 kiro-auth-token.json，或重新授权。"),
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
                let id = KiroAccountId::new();
                self.db.with(|c| {
                    c.execute(
                        "INSERT INTO kiro_accounts (id, account_ref, email, status, enabled, note, auth_method, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, ?7)",
                        rusqlite::params![
                            id.as_str(),
                            account_ref,
                            email.as_deref(),
                            KiroStatus::NeedsLogin.as_str(),
                            note,
                            identity.auth_method.as_deref(),
                            &now,
                        ],
                    )
                })?;
                (id, true)
            }
        };

        self.db.with(|c| {
            c.execute(
                "UPDATE kiro_accounts SET
                   email = COALESCE(?2, email),
                   note = COALESCE(?3, note),
                   auth_method = COALESCE(?4, auth_method),
                   updated_at = ?5
                 WHERE id = ?1",
                rusqlite::params![
                    id.as_str(),
                    email.as_deref(),
                    note,
                    identity.auth_method.as_deref(),
                    &now
                ],
            )
        })?;
        self.store_tokens(&id, tokens)?;
        Ok(Upserted {
            account: self.get(&id)?,
            created,
        })
    }

    pub fn store_tokens(&self, id: &KiroAccountId, tokens: &TokenSet) -> Result<()> {
        self.secrets
            .set(&kiro_secret(id, KiroSecret::Access), &tokens.access_token)?;
        if let Some(rt) = &tokens.refresh_token {
            self.secrets
                .set(&kiro_secret(id, KiroSecret::Refresh), rt)?;
        }
        if let Some(cid) = &tokens.client_id {
            self.secrets
                .set(&kiro_secret(id, KiroSecret::ClientId), cid)?;
        }
        if let Some(cs) = &tokens.client_secret {
            self.secrets
                .set(&kiro_secret(id, KiroSecret::ClientSecret), cs)?;
        }
        let has_refresh = self.secrets.exists(&kiro_secret(id, KiroSecret::Refresh));
        let expires = tokens.expires_at.and_then(|t| t.format(&Rfc3339).ok());
        self.db.with(|c| {
            c.execute(
                "UPDATE kiro_accounts SET
                   has_refresh = ?2, access_expires_at = ?3, status = 'active',
                   last_error = NULL, updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), has_refresh, expires, now_iso()],
            )
        })?;
        Ok(())
    }

    pub fn secret(&self, id: &KiroAccountId, kind: KiroSecret) -> Result<Option<Secret>> {
        self.secrets.get(&kiro_secret(id, kind))
    }

    pub fn remove(&self, id: &KiroAccountId) -> Result<()> {
        let n = self
            .db
            .with(|c| c.execute("DELETE FROM kiro_accounts WHERE id = ?1", [id.as_str()]))?;
        if n == 0 {
            return Err(not_found(id.as_str()));
        }
        for kind in KiroSecret::ALL {
            self.secrets.delete(&kiro_secret(id, kind))?;
        }
        Ok(())
    }

    pub fn set_enabled(&self, id: &KiroAccountId, enabled: bool) -> Result<KiroAccount> {
        self.db.with(|c| {
            c.execute(
                "UPDATE kiro_accounts SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), enabled, now_iso()],
            )
        })?;
        self.get(id)
    }

    pub fn update_identity(&self, id: &KiroAccountId, identity: &Identity) -> Result<()> {
        let email = identity
            .email
            .as_deref()
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty());
        self.db.with(|c| {
            c.execute(
                "UPDATE kiro_accounts SET
                   email = COALESCE(?2, email),
                   auth_method = COALESCE(?3, auth_method),
                   updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![
                    id.as_str(),
                    email.as_deref(),
                    identity.auth_method.as_deref(),
                    now_iso()
                ],
            )
        })?;
        Ok(())
    }

    pub fn set_note(&self, id: &KiroAccountId, note: Option<&str>) -> Result<KiroAccount> {
        self.db.with(|c| {
            c.execute(
                "UPDATE kiro_accounts SET note = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), note, now_iso()],
            )
        })?;
        self.get(id)
    }

    pub fn record_failure(
        &self,
        id: &KiroAccountId,
        error: &str,
        status: Option<KiroStatus>,
    ) -> Result<()> {
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE kiro_accounts SET
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

    fn find(&self, id: &KiroAccountId) -> Result<Option<KiroAccount>> {
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

const SELECT: &str = "SELECT id, account_ref, email, plan_type, status, enabled, note, auth_method,
        last_checked_at, last_error, has_refresh, access_expires_at, created_at, updated_at
 FROM kiro_accounts";

fn row_to_account(row: &Row<'_>) -> rusqlite::Result<KiroAccount> {
    let status: String = row.get(4)?;
    Ok(KiroAccount {
        id: KiroAccountId::from_raw(row.get::<_, String>(0)?),
        account_ref: row.get(1)?,
        email: row.get(2)?,
        plan_type: row.get(3)?,
        status: KiroStatus::parse(&status),
        enabled: row.get::<_, i64>(5)? != 0,
        note: row.get(6)?,
        auth_method: row.get(7)?,
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
        format!("没有这个 Kiro 账号：{id}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_store::MemorySecrets;
    use time::OffsetDateTime;

    fn repo() -> KiroAccounts {
        KiroAccounts::new(
            Arc::new(Db::open_in_memory().unwrap()),
            Arc::new(MemorySecrets::new()),
        )
    }

    fn tokens(access: &str, refresh: Option<&str>) -> TokenSet {
        TokenSet {
            access_token: Secret::new(access),
            refresh_token: refresh.map(Secret::new),
            client_id: Some(Secret::new("cid")),
            client_secret: Some(Secret::new("csec")),
            expires_at: Some(OffsetDateTime::from_unix_timestamp(4_000_000_000).unwrap()),
        }
    }

    #[test]
    fn upsert_keeps_client_pair_out_of_the_table() {
        let r = repo();
        let first = r
            .upsert(
                &Identity {
                    subject: "user/a".into(),
                    email: Some("A@X.com".into()),
                    auth_method: Some("builder-id".into()),
                },
                &tokens("at-1", Some("rt-1")),
                None,
            )
            .unwrap();
        assert!(first.created);
        assert_eq!(first.account.email.as_deref(), Some("a@x.com"));
        assert_eq!(first.account.auth_method.as_deref(), Some("builder-id"));
        assert_eq!(
            r.secret(&first.account.id, KiroSecret::ClientId)
                .unwrap()
                .unwrap()
                .expose(),
            "cid"
        );
        let dump: Vec<String> =
            r.db.with(|c| {
                let mut stmt = c.prepare("SELECT * FROM kiro_accounts")?;
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
        assert!(!dump.iter().any(|s| s.contains("at-") || s.contains("csec")));
    }
}
