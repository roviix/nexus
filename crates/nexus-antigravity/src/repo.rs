//! Google 反重力 (Antigravity) 账号数据访问层。
//!
//! 业务元信息入 SQLite `antigravity_accounts` 表，明文凭证入 `secrets` 表。

use crate::model::{assign_labels, AntigravityAccount, AntigravityStatus, AntigravityUsage};
use crate::oauth::TokenSet;
use nexus_core::{now_iso, AntigravityAccountId, AppError, ErrorCode, Result, Secret};
use nexus_store::{antigravity_secret, AntigravitySecret, Db, SecretStore};
use rusqlite::{params, OptionalExtension, Row};
use std::sync::Arc;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

pub struct AntigravityAccounts {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
}

impl AntigravityAccounts {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { db, secrets }
    }

    pub fn list(&self) -> Result<Vec<AntigravityAccount>> {
        let mut rows = self.db.with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, account_ref, email, project_id, status, enabled, note, tier,
                        usage_json, last_checked_at, last_error, has_refresh, access_expires_at,
                        created_at, updated_at
                 FROM antigravity_accounts
                 ORDER BY created_at ASC",
            )?;
            let mapped = stmt.query_map([], Self::map_row)?;
            let mut out = Vec::new();
            for r in mapped {
                out.push(r?);
            }
            Ok(out)
        })?;
        assign_labels(&mut rows);
        Ok(rows)
    }

    pub fn get(&self, id: &AntigravityAccountId) -> Result<Option<AntigravityAccount>> {
        let list = self.list()?;
        Ok(list.into_iter().find(|a| &a.id == id))
    }

    pub fn get_by_ref(&self, account_ref: &str) -> Result<Option<AntigravityAccount>> {
        let id_str: Option<String> = self.db.with(|conn| {
            conn.query_row(
                "SELECT id FROM antigravity_accounts WHERE account_ref = ?1",
                params![account_ref],
                |r| r.get(0),
            )
            .optional()
        })?;
        let Some(id_str) = id_str else {
            return Ok(None);
        };
        self.get(&AntigravityAccountId::from_raw(id_str))
    }

    pub fn upsert_oauth(
        &self,
        email: &str,
        tokens: &TokenSet,
        note: Option<String>,
    ) -> Result<AntigravityAccount> {
        let now = now_iso();
        let expires_at = OffsetDateTime::now_utc()
            .checked_add(Duration::seconds(tokens.expires_in))
            .and_then(|t| t.format(&Rfc3339).ok());

        let existing = self.get_by_ref(email)?;
        let id = existing
            .as_ref()
            .map(|a| a.id.clone())
            .unwrap_or_else(AntigravityAccountId::new);

        let has_refresh = tokens.refresh_token.is_some()
            || existing.as_ref().map(|a| a.has_refresh).unwrap_or(false);

        // 写入 secrets
        self.secrets.set(
            &antigravity_secret(&id, AntigravitySecret::Access),
            &Secret::new(&tokens.access_token),
        )?;
        if let Some(ref rt) = tokens.refresh_token {
            self.secrets.set(
                &antigravity_secret(&id, AntigravitySecret::Refresh),
                &Secret::new(rt),
            )?;
        }

        self.db.with(|conn| {
            conn.execute(
                "INSERT INTO antigravity_accounts (
                    id, account_ref, email, status, enabled, note,
                    has_refresh, access_expires_at, created_at, updated_at
                ) VALUES (?1, ?2, ?3, 'active', 1, ?4, ?5, ?6, ?7, ?7)
                ON CONFLICT(account_ref) DO UPDATE SET
                    email = excluded.email,
                    status = 'active',
                    has_refresh = excluded.has_refresh,
                    access_expires_at = excluded.access_expires_at,
                    updated_at = excluded.updated_at",
                params![
                    id.as_str(),
                    email,
                    email,
                    note,
                    if has_refresh { 1 } else { 0 },
                    expires_at,
                    now,
                ],
            )?;
            Ok(())
        })?;

        self.get(&id)?
            .ok_or_else(|| AppError::new(ErrorCode::Database, "读取刚写入的 Antigravity 账号失败"))
    }

    pub fn save_tokens(
        &self,
        id: &AntigravityAccountId,
        access: &str,
        refresh: Option<&str>,
        expires_at: Option<&str>,
    ) -> Result<()> {
        let now = now_iso();
        self.secrets.set(
            &antigravity_secret(id, AntigravitySecret::Access),
            &Secret::new(access),
        )?;
        let has_refresh = if let Some(rt) = refresh {
            self.secrets.set(
                &antigravity_secret(id, AntigravitySecret::Refresh),
                &Secret::new(rt),
            )?;
            true
        } else {
            self.secrets
                .get(&antigravity_secret(id, AntigravitySecret::Refresh))?
                .is_some()
        };

        self.db.with(|conn| {
            conn.execute(
                "UPDATE antigravity_accounts SET
                    has_refresh = ?1,
                    access_expires_at = ?2,
                    updated_at = ?3
                 WHERE id = ?4",
                params![
                    if has_refresh { 1 } else { 0 },
                    expires_at,
                    now,
                    id.as_str()
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn get_access_token(&self, id: &AntigravityAccountId) -> Result<Option<Secret>> {
        self.secrets
            .get(&antigravity_secret(id, AntigravitySecret::Access))
    }

    pub fn get_refresh_token(&self, id: &AntigravityAccountId) -> Result<Option<Secret>> {
        self.secrets
            .get(&antigravity_secret(id, AntigravitySecret::Refresh))
    }

    pub fn set_status(
        &self,
        id: &AntigravityAccountId,
        status: AntigravityStatus,
        last_error: Option<String>,
    ) -> Result<()> {
        let now = now_iso();
        self.db.with(|conn| {
            conn.execute(
                "UPDATE antigravity_accounts SET
                    status = ?1,
                    last_error = ?2,
                    last_checked_at = ?3,
                    updated_at = ?3
                 WHERE id = ?4",
                params![status.as_str(), last_error, now, id.as_str()],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn set_project_id(&self, id: &AntigravityAccountId, project_id: &str) -> Result<()> {
        let now = now_iso();
        self.db.with(|conn| {
            conn.execute(
                "UPDATE antigravity_accounts SET
                    project_id = ?1,
                    updated_at = ?2
                 WHERE id = ?3",
                params![project_id, now, id.as_str()],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn update_metadata(
        &self,
        id: &AntigravityAccountId,
        tier: Option<&str>,
        project_id: Option<&str>,
    ) -> Result<()> {
        let now = now_iso();
        self.db.with(|conn| {
            conn.execute(
                "UPDATE antigravity_accounts SET
                    tier = COALESCE(?1, tier),
                    project_id = COALESCE(?2, project_id),
                    last_checked_at = ?3,
                    updated_at = ?3
                 WHERE id = ?4",
                params![tier, project_id, now, id.as_str()],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn set_usage(
        &self,
        id: &AntigravityAccountId,
        usage: &AntigravityUsage,
        tier: Option<&str>,
    ) -> Result<()> {
        let now = now_iso();
        let usage_json = serde_json::to_string(usage).ok();
        self.db.with(|conn| {
            conn.execute(
                "UPDATE antigravity_accounts SET
                    usage_json = ?1,
                    tier = COALESCE(?2, tier),
                    status = 'active',
                    last_error = NULL,
                    last_checked_at = ?3,
                    updated_at = ?3
                 WHERE id = ?4",
                params![usage_json, tier, now, id.as_str()],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn set_enabled(
        &self,
        id: &AntigravityAccountId,
        enabled: bool,
    ) -> Result<AntigravityAccount> {
        let now = now_iso();
        self.db.with(|conn| {
            conn.execute(
                "UPDATE antigravity_accounts SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
                params![if enabled { 1 } else { 0 }, now, id.as_str()],
            )?;
            Ok(())
        })?;
        self.get(id)?
            .ok_or_else(|| AppError::new(ErrorCode::AccountNotFound, "未找到该 Antigravity 账号"))
    }

    pub fn set_note(
        &self,
        id: &AntigravityAccountId,
        note: Option<String>,
    ) -> Result<AntigravityAccount> {
        let now = now_iso();
        self.db.with(|conn| {
            conn.execute(
                "UPDATE antigravity_accounts SET note = ?1, updated_at = ?2 WHERE id = ?3",
                params![note, now, id.as_str()],
            )?;
            Ok(())
        })?;
        self.get(id)?
            .ok_or_else(|| AppError::new(ErrorCode::AccountNotFound, "未找到该 Antigravity 账号"))
    }

    pub fn remove(&self, id: &AntigravityAccountId) -> Result<()> {
        for secret in AntigravitySecret::ALL {
            let _ = self.secrets.delete(&antigravity_secret(id, secret));
        }
        self.db.with(|conn| {
            conn.execute(
                "DELETE FROM antigravity_accounts WHERE id = ?1",
                params![id.as_str()],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    fn map_row(row: &Row<'_>) -> rusqlite::Result<AntigravityAccount> {
        let id_raw: String = row.get(0)?;
        let status_raw: String = row.get(4)?;
        let usage_json: Option<String> = row.get(8)?;
        let usage = usage_json.and_then(|s| serde_json::from_str(&s).ok());

        Ok(AntigravityAccount {
            id: AntigravityAccountId::from_raw(id_raw),
            account_ref: row.get(1)?,
            email: row.get(2)?,
            project_id: row.get(3)?,
            status: AntigravityStatus::parse(&status_raw),
            enabled: row.get::<_, i64>(5)? != 0,
            note: row.get(6)?,
            tier: row.get(7)?,
            usage,
            last_checked_at: row.get(9)?,
            last_error: row.get(10)?,
            has_refresh: row.get::<_, i64>(11)? != 0,
            access_expires_at: row.get(12)?,
            label: String::new(),
            created_at: row.get(13)?,
            updated_at: row.get(14)?,
        })
    }
}
