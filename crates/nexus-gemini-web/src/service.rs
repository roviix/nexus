//! Gemini 网页账号：Cookie 只进 secrets，表里只留身份。

use nexus_core::{now_iso, AppError, ErrorCode, GeminiWebAccountId, Result, Secret};
use nexus_store::keys::{gemini_web_secret, GeminiWebSecret};
use nexus_store::{Db, SecretStore};
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GeminiWebStatus {
    Active,
    NeedsLogin,
}

impl GeminiWebStatus {
    fn parse(raw: &str) -> Self {
        match raw {
            "active" => GeminiWebStatus::Active,
            _ => GeminiWebStatus::NeedsLogin,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeminiWebAccount {
    pub id: GeminiWebAccountId,
    pub account_ref: String,
    pub email: Option<String>,
    pub status: GeminiWebStatus,
    pub enabled: bool,
    pub note: Option<String>,
    pub label: String,
    pub has_refresh: bool,
    pub last_error: Option<String>,
    pub last_checked_at: Option<String>,
    pub access_expires_at: Option<String>,
    pub tier: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub struct GeminiWebService {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
}

impl GeminiWebService {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { db, secrets }
    }

    pub fn list(&self) -> Result<Vec<GeminiWebAccount>> {
        self.db.with(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, account_ref, email, status, enabled, note, last_error, last_checked_at, created_at, updated_at
                 FROM gemini_web_accounts ORDER BY created_at ASC, id ASC",
            )?;
            let rows = stmt.query_map([], |row| {
                let id = GeminiWebAccountId::from_raw(row.get::<_, String>(0)?);
                let account_ref: String = row.get(1)?;
                let email: Option<String> = row.get(2)?;
                let status = GeminiWebStatus::parse(&row.get::<_, String>(3)?);
                Ok(GeminiWebAccount {
                    label: email.clone().filter(|e| !e.is_empty()).unwrap_or_else(|| account_ref.clone()),
                    id,
                    account_ref,
                    email,
                    status,
                    enabled: row.get::<_, i64>(4)? != 0,
                    note: row.get(5)?,
                    has_refresh: true,
                    last_error: row.get(6)?,
                    last_checked_at: row.get(7)?,
                    access_expires_at: None,
                    tier: Some("网页".into()),
                    created_at: row.get(8)?,
                    updated_at: row.get(9)?,
                })
            })?;
            rows.collect()
        })
    }

    pub fn import_cookie(&self, cookie: &str, email: Option<String>, note: Option<String>) -> Result<GeminiWebAccount> {
        let cookie = cookie.trim();
        if !cookie.contains("__Secure-1PSID=") {
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                "这不是 Gemini 网页登录 Cookie，缺少 __Secure-1PSID",
            ).with_hint("在 gemini.google.com 已登录的页面里复制请求头 Cookie。"));
        }
        let account_ref = crate::protocol::account_ref_from_cookie(cookie).ok_or_else(|| {
            AppError::new(ErrorCode::InvalidInput, "Cookie 里的 __Secure-1PSID 是空的")
        })?;
        let email = email.map(|e| e.trim().to_string()).filter(|e| !e.is_empty());
        let now = now_iso();
        let existing: Option<String> = self.db.with(|conn| {
            conn.query_row(
                "SELECT id FROM gemini_web_accounts WHERE account_ref = ?1",
                [&account_ref],
                |row| row.get(0),
            )
            .optional()
        })?;
        let id = existing
            .map(GeminiWebAccountId::from_raw)
            .unwrap_or_else(GeminiWebAccountId::new);
        self.db.with(|conn| {
            conn.execute(
                "INSERT INTO gemini_web_accounts
                    (id, account_ref, email, status, enabled, note, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'active', 1, ?4, ?5, ?5)
                 ON CONFLICT(account_ref) DO UPDATE SET
                    email = COALESCE(excluded.email, gemini_web_accounts.email),
                    status = 'active',
                    note = COALESCE(excluded.note, gemini_web_accounts.note),
                    last_error = NULL,
                    updated_at = excluded.updated_at",
                params![id.as_str(), account_ref, email, note, now],
            )?;
            Ok(())
        })?;
        self.secrets.set(
            &gemini_web_secret(&id, GeminiWebSecret::Cookie),
            &Secret::new(cookie),
        )?;
        self.get(&id)
    }

    pub fn get(&self, id: &GeminiWebAccountId) -> Result<GeminiWebAccount> {
        self.list()?
            .into_iter()
            .find(|a| &a.id == id)
            .ok_or_else(|| AppError::new(ErrorCode::AccountNotFound, "没有这个 Gemini 网页账号"))
    }

    pub fn cookie(&self, id: &str) -> Result<Secret> {
        let aid = GeminiWebAccountId::from_raw(id);
        self.secrets
            .require(&gemini_web_secret(&aid, GeminiWebSecret::Cookie))
    }

    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<GeminiWebAccount> {
        let aid = GeminiWebAccountId::from_raw(id);
        let now = now_iso();
        self.db.with(|conn| {
            conn.execute(
                "UPDATE gemini_web_accounts SET enabled = ?1, updated_at = ?2 WHERE id = ?3",
                params![enabled as i64, now, aid.as_str()],
            )?;
            Ok(())
        })?;
        self.get(&aid)
    }

    pub fn set_note(&self, id: &str, note: Option<String>) -> Result<GeminiWebAccount> {
        let aid = GeminiWebAccountId::from_raw(id);
        let now = now_iso();
        self.db.with(|conn| {
            conn.execute(
                "UPDATE gemini_web_accounts SET note = ?1, updated_at = ?2 WHERE id = ?3",
                params![note, now, aid.as_str()],
            )?;
            Ok(())
        })?;
        self.get(&aid)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let aid = GeminiWebAccountId::from_raw(id);
        let _ = self.secrets.delete(&gemini_web_secret(&aid, GeminiWebSecret::Cookie));
        self.db.with(|conn| {
            conn.execute("DELETE FROM gemini_web_accounts WHERE id = ?1", [aid.as_str()])?;
            Ok(())
        })
    }

    pub fn update_cookie(&self, id: &str, cookie: &str) -> Result<()> {
        let aid = GeminiWebAccountId::from_raw(id);
        self.secrets.set(
            &gemini_web_secret(&aid, GeminiWebSecret::Cookie),
            &Secret::new(cookie),
        )
    }

    pub fn mark_auth_failed(&self, id: &str, message: &str) -> Result<()> {
        let now = now_iso();
        self.db.with(|conn| {
            conn.execute(
                "UPDATE gemini_web_accounts SET status = 'needs_login', last_error = ?1, updated_at = ?2 WHERE id = ?3",
                params![message, now, id],
            )?;
            Ok(())
        })
    }
}
