//! `claude_accounts` 表。token 在 `secrets`。

use crate::model::{ClaudeAccount, ClaudeAuthMode, ClaudeStatus, UsageCard};
use crate::protocol::{ClaudeQuota, REFRESH_AHEAD_SECS};
use nexus_core::{now_iso, AppError, ClaudeAccountId, ErrorCode, Result, Secret};
use nexus_store::keys::{claude_secret, ClaudeSecret};
use nexus_store::{Db, SecretStore};
use rusqlite::OptionalExtension;
use std::sync::Arc;

const SELECT: &str = "SELECT id, account_ref, auth_mode, email, plan_type, status, enabled, note, \
     key_hint, access_expires_at, usage_json, last_checked_at, last_error, created_at, updated_at \
     FROM claude_accounts";

pub struct ClaudeAccounts {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
}

pub struct SavedOauth {
    pub account_ref: String,
    pub email: Option<String>,
    pub plan: Option<String>,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<String>,
    pub note: Option<String>,
}

pub struct SavedToken {
    pub account_ref: String,
    pub mode: ClaudeAuthMode,
    pub token: String,
    pub note: Option<String>,
}

impl ClaudeAccounts {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { db, secrets }
    }

    pub fn list(&self) -> Result<Vec<ClaudeAccount>> {
        let rows = self.db.with(|c| {
            let mut stmt = c.prepare(&format!("{SELECT} ORDER BY created_at ASC, id ASC"))?;
            let rows = stmt.query_map([], row_parts)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
        })?;
        rows.into_iter().map(|row| self.finish(row)).collect()
    }

    pub fn get(&self, id: &ClaudeAccountId) -> Result<ClaudeAccount> {
        self.find(id)?.ok_or_else(|| missing(id.as_str()))
    }

    pub fn by_ref(&self, account_ref: &str) -> Result<Option<ClaudeAccount>> {
        let row = self.db.with(|c| {
            c.query_row(
                &format!("{SELECT} WHERE account_ref = ?1"),
                [account_ref],
                row_parts,
            )
            .optional()
        })?;
        row.map(|row| self.finish(row)).transpose()
    }

    pub fn by_label(&self, label: &str) -> Result<Option<ClaudeAccount>> {
        Ok(self.list()?.into_iter().find(|a| a.label == label))
    }

    pub fn save_oauth(&self, saved: SavedOauth) -> Result<ClaudeAccount> {
        let now = now_iso();
        let existing = self.by_ref(&saved.account_ref)?;
        let id = match &existing {
            Some(a) => a.id.clone(),
            None => {
                let id = ClaudeAccountId::new();
                self.db.with(|c| {
                    c.execute(
                        "INSERT INTO claude_accounts
                         (id, account_ref, auth_mode, email, plan_type, status, enabled, note, key_hint,
                          access_expires_at, last_checked_at, last_error, created_at, updated_at)
                         VALUES (?1, ?2, 'oauth', ?3, ?4, 'active', 1, ?5, ?6, ?7, NULL, NULL, ?8, ?8)",
                        rusqlite::params![
                            id.as_str(),
                            saved.account_ref,
                            saved.email,
                            saved.plan,
                            saved.note,
                            hint(&saved.access_token),
                            saved.expires_at,
                            now,
                        ],
                    )
                })?;
                id
            }
        };
        self.db.with(|c| {
            c.execute(
                "UPDATE claude_accounts SET
                   email = COALESCE(?2, email),
                   plan_type = COALESCE(?3, plan_type),
                   status = 'active',
                   note = COALESCE(?4, note),
                   key_hint = ?5,
                   access_expires_at = ?6,
                   last_error = NULL,
                   updated_at = ?7
                 WHERE id = ?1",
                rusqlite::params![
                    id.as_str(),
                    saved.email,
                    saved.plan,
                    saved.note,
                    hint(&saved.access_token),
                    saved.expires_at,
                    now,
                ],
            )
        })?;
        self.secrets.set(
            &claude_secret(&id, ClaudeSecret::Access),
            &Secret::new(saved.access_token),
        )?;
        if let Some(refresh) = saved.refresh_token {
            self.secrets.set(
                &claude_secret(&id, ClaudeSecret::Refresh),
                &Secret::new(refresh),
            )?;
        }
        self.get(&id)
    }

    pub fn save_static(&self, saved: SavedToken) -> Result<ClaudeAccount> {
        let now = now_iso();
        let mode = saved.mode.as_str();
        let existing = self.by_ref(&saved.account_ref)?;
        let id = match &existing {
            Some(a) => a.id.clone(),
            None => {
                let id = ClaudeAccountId::new();
                self.db.with(|c| {
                    c.execute(
                        "INSERT INTO claude_accounts
                         (id, account_ref, auth_mode, status, enabled, note, key_hint, created_at, updated_at)
                         VALUES (?1, ?2, ?3, 'active', 1, ?4, ?5, ?6, ?6)",
                        rusqlite::params![
                            id.as_str(),
                            saved.account_ref,
                            mode,
                            saved.note,
                            hint(&saved.token),
                            now,
                        ],
                    )
                })?;
                id
            }
        };
        self.db.with(|c| {
            c.execute(
                "UPDATE claude_accounts SET auth_mode = ?2, status = 'active', note = COALESCE(?3, note),
                   key_hint = ?4, last_error = NULL, updated_at = ?5 WHERE id = ?1",
                rusqlite::params![id.as_str(), mode, saved.note, hint(&saved.token), now],
            )
        })?;
        let kind = match saved.mode {
            ClaudeAuthMode::ApiKey => ClaudeSecret::ApiKey,
            _ => ClaudeSecret::Access,
        };
        self.secrets
            .set(&claude_secret(&id, kind), &Secret::new(saved.token))?;
        self.get(&id)
    }

    pub fn store_access(
        &self,
        id: &ClaudeAccountId,
        access: &str,
        refresh: Option<&str>,
        expires_at: Option<&str>,
    ) -> Result<()> {
        self.secrets.set(
            &claude_secret(id, ClaudeSecret::Access),
            &Secret::new(access.to_string()),
        )?;
        if let Some(refresh) = refresh {
            self.secrets.set(
                &claude_secret(id, ClaudeSecret::Refresh),
                &Secret::new(refresh.to_string()),
            )?;
        }
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE claude_accounts SET status = 'active', key_hint = ?2, access_expires_at = ?3,
                   last_error = NULL, updated_at = ?4 WHERE id = ?1",
                rusqlite::params![id.as_str(), hint(access), expires_at, now],
            )
        })?;
        Ok(())
    }

    /// 刷新票之后补邮箱和套餐。空值不覆盖已经写上的。
    pub fn touch_profile(
        &self,
        id: &ClaudeAccountId,
        email: Option<&str>,
        plan: Option<&str>,
    ) -> Result<()> {
        if email.is_none() && plan.is_none() {
            return Ok(());
        }
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE claude_accounts SET
                   email = COALESCE(?2, email),
                   plan_type = COALESCE(?3, plan_type),
                   updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), email, plan, now],
            )
        })?;
        Ok(())
    }

    pub fn access(&self, id: &ClaudeAccountId) -> Result<Option<Secret>> {
        self.secrets.get(&claude_secret(id, ClaudeSecret::Access))
    }

    pub fn refresh_token(&self, id: &ClaudeAccountId) -> Result<Option<Secret>> {
        self.secrets.get(&claude_secret(id, ClaudeSecret::Refresh))
    }

    pub fn api_key(&self, id: &ClaudeAccountId) -> Result<Option<Secret>> {
        self.secrets.get(&claude_secret(id, ClaudeSecret::ApiKey))
    }

    pub fn set_usage(&self, id: &ClaudeAccountId, quota: &ClaudeQuota) -> Result<ClaudeAccount> {
        let now = now_iso();
        let json = serde_json::to_string(quota).map_err(|e| AppError::invalid(e.to_string()))?;
        self.db.with(|c| {
            c.execute(
                "UPDATE claude_accounts SET usage_json = ?2, last_checked_at = ?3, last_error = NULL, updated_at = ?3
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), json, now],
            )
        })?;
        self.get(id)
    }

    pub fn set_error(
        &self,
        id: &ClaudeAccountId,
        status: ClaudeStatus,
        message: &str,
    ) -> Result<()> {
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE claude_accounts SET status = ?2, last_error = ?3, last_checked_at = ?4, updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), status.as_str(), message, now],
            )
        })?;
        Ok(())
    }

    pub fn set_enabled(&self, id: &ClaudeAccountId, enabled: bool) -> Result<ClaudeAccount> {
        let now = now_iso();
        let n = self.db.with(|c| {
            c.execute(
                "UPDATE claude_accounts SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), enabled as i64, now],
            )
        })?;
        if n == 0 {
            return Err(missing(id.as_str()));
        }
        self.get(id)
    }

    pub fn set_note(&self, id: &ClaudeAccountId, note: Option<&str>) -> Result<ClaudeAccount> {
        let now = now_iso();
        let n = self.db.with(|c| {
            c.execute(
                "UPDATE claude_accounts SET note = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), note, now],
            )
        })?;
        if n == 0 {
            return Err(missing(id.as_str()));
        }
        self.get(id)
    }

    pub fn remove(&self, id: &ClaudeAccountId) -> Result<()> {
        let n = self
            .db
            .with(|c| c.execute("DELETE FROM claude_accounts WHERE id = ?1", [id.as_str()]))?;
        if n == 0 {
            return Err(missing(id.as_str()));
        }
        for kind in ClaudeSecret::ALL {
            self.secrets.delete(&claude_secret(id, kind))?;
        }
        Ok(())
    }

    pub fn access_expires_at(&self, id: &ClaudeAccountId) -> Result<Option<String>> {
        self.db.with(|c| {
            c.query_row(
                "SELECT access_expires_at FROM claude_accounts WHERE id = ?1",
                [id.as_str()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map(|found| found.flatten())
        })
    }

    pub fn expires_soon(expires_at: Option<&str>, now_unix: i64) -> bool {
        let Some(raw) = expires_at else {
            return false;
        };
        let Ok(dt) =
            time::OffsetDateTime::parse(raw, &time::format_description::well_known::Rfc3339)
        else {
            return true;
        };
        dt.unix_timestamp() <= now_unix + REFRESH_AHEAD_SECS
    }

    fn find(&self, id: &ClaudeAccountId) -> Result<Option<ClaudeAccount>> {
        let row = self.db.with(|c| {
            c.query_row(&format!("{SELECT} WHERE id = ?1"), [id.as_str()], row_parts)
                .optional()
        })?;
        row.map(|row| self.finish(row)).transpose()
    }

    fn finish(&self, row: Parts) -> Result<ClaudeAccount> {
        let has_refresh = self
            .secrets
            .get(&claude_secret(&row.id, ClaudeSecret::Refresh))?
            .is_some();
        let has_token = self
            .secrets
            .get(&claude_secret(&row.id, ClaudeSecret::Access))?
            .is_some();
        let has_api_key = self
            .secrets
            .get(&claude_secret(&row.id, ClaudeSecret::ApiKey))?
            .is_some();
        let quota = row
            .usage_json
            .as_deref()
            .and_then(|s| serde_json::from_str::<ClaudeQuota>(s).ok());
        let usage = match (&quota, row.last_checked_at.as_deref()) {
            (Some(q), Some(at)) => Some(UsageCard::from_quota(q, row.plan_type.as_deref(), at)),
            _ => None,
        };
        let label = row
            .email
            .clone()
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| format!("claude·{}", tail(&row.account_ref)));
        Ok(ClaudeAccount {
            auth_kind: if row.auth_mode == ClaudeAuthMode::ApiKey {
                "api_key"
            } else {
                "oauth"
            },
            label,
            has_refresh,
            has_token,
            has_api_key,
            usage,
            id: row.id,
            account_ref: row.account_ref,
            email: row.email,
            plan_type: row.plan_type,
            auth_mode: row.auth_mode,
            status: row.status,
            enabled: row.enabled,
            note: row.note,
            key_hint: row.key_hint,
            last_checked_at: row.last_checked_at,
            last_error: row.last_error,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

struct Parts {
    id: ClaudeAccountId,
    account_ref: String,
    auth_mode: ClaudeAuthMode,
    email: Option<String>,
    plan_type: Option<String>,
    status: ClaudeStatus,
    enabled: bool,
    note: Option<String>,
    key_hint: Option<String>,
    usage_json: Option<String>,
    last_checked_at: Option<String>,
    last_error: Option<String>,
    created_at: String,
    updated_at: String,
}

fn row_parts(row: &rusqlite::Row<'_>) -> rusqlite::Result<Parts> {
    Ok(Parts {
        id: ClaudeAccountId::from_raw(row.get::<_, String>(0)?),
        account_ref: row.get(1)?,
        auth_mode: ClaudeAuthMode::parse(&row.get::<_, String>(2)?),
        email: row.get(3)?,
        plan_type: row.get(4)?,
        status: ClaudeStatus::parse(&row.get::<_, String>(5)?),
        enabled: row.get::<_, i64>(6)? != 0,
        note: row.get(7)?,
        key_hint: row.get(8)?,
        usage_json: row.get(10)?,
        last_checked_at: row.get(11)?,
        last_error: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

fn hint(token: &str) -> String {
    let t = token.trim();
    let n = t.chars().count();
    if n <= 4 {
        "••••".into()
    } else {
        format!("••••{}", t.chars().skip(n - 4).collect::<String>())
    }
}

fn tail(id: &str) -> String {
    let n = id.chars().count();
    id.chars().skip(n.saturating_sub(6)).collect()
}

fn missing(id: &str) -> AppError {
    AppError::new(
        ErrorCode::AccountNotFound,
        format!("找不到这个 Claude 账号（{id}）"),
    )
}
