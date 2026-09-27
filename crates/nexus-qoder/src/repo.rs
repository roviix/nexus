//! Qoder 账号仓库：`qoder_accounts` 表存元信息，凭证交给 `SecretStore`。

use crate::model::{
    compute_label, hint_of, QoderAccount, QoderBackend, QoderIdentity, QoderStatus,
};
use nexus_core::{now_iso, AppError, ErrorCode, QoderAccountId, Result, Secret};
use nexus_store::keys::{qoder_secret, QoderSecret};
use nexus_store::{Db, SecretStore};
use rusqlite::Row;
use std::sync::Arc;

pub struct QoderAccounts {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
}

#[derive(Debug, Clone)]
pub struct Upserted {
    pub account: QoderAccount,
    pub created: bool,
}

#[derive(Debug, Clone)]
pub struct StoredPat {
    pub backend: QoderBackend,
    pub pat: String,
    pub user_id: String,
    pub email: String,
    pub name: String,
    pub machine_id: String,
    pub job_token: String,
    pub job_refresh: String,
    pub expires_at: String,
}

impl QoderAccounts {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { db, secrets }
    }

    pub fn list(&self) -> Result<Vec<QoderAccount>> {
        self.db.with(|c| {
            let mut stmt = c.prepare(&format!("{SELECT} ORDER BY created_at ASC, id ASC"))?;
            let rows = stmt.query_map([], row_to_account)?;
            rows.collect()
        })
    }

    pub fn get(&self, id: &QoderAccountId) -> Result<QoderAccount> {
        self.find(id)?.ok_or_else(|| not_found(id.as_str()))
    }

    pub fn by_ref(&self, account_ref: &str) -> Result<Option<QoderAccount>> {
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

    /// 接力队把标签收成小写再当键（`RelayLane`）。这里按同一把键找，
    /// 否则 `Qoder` 和 `qoder` 对不上，签名时就说「找不到身份」。
    pub fn by_label(&self, label: &str) -> Result<Option<QoderAccount>> {
        let want = label.trim().to_lowercase();
        Ok(self
            .list()?
            .into_iter()
            .find(|a| a.label().trim().to_lowercase() == want))
    }

    /// 同一把 PAT（同一边、同一个 user id）重复导入是更新，不新开一行。
    pub fn upsert(&self, imported: &StoredPat, note: Option<&str>) -> Result<Upserted> {
        let account_ref = account_ref(imported);
        let email = blank_to_none(imported.email.as_str()).map(|e| e.to_ascii_lowercase());
        let user_id = blank_to_none(imported.user_id.as_str());
        let name = blank_to_none(imported.name.as_str());
        let now = now_iso();
        let (id, created) = match self.by_ref(&account_ref)? {
            Some(existing) => (existing.id, false),
            None => {
                let id = QoderAccountId::new();
                self.db.with(|c| {
                    c.execute(
                        "INSERT INTO qoder_accounts
                           (id, account_ref, backend, user_id, email, display_name, machine_id,
                            status, enabled, note, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 1, ?9, ?10, ?10)",
                        rusqlite::params![
                            id.as_str(),
                            &account_ref,
                            imported.backend.as_str(),
                            user_id,
                            email.as_deref(),
                            name,
                            &imported.machine_id,
                            QoderStatus::NeedsLogin.as_str(),
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
                "UPDATE qoder_accounts SET
                   backend = ?2,
                   user_id = COALESCE(?3, user_id),
                   email = COALESCE(?4, email),
                   display_name = COALESCE(?5, display_name),
                   machine_id = ?6,
                   note = COALESCE(?7, note),
                   updated_at = ?8
                 WHERE id = ?1",
                rusqlite::params![
                    id.as_str(),
                    imported.backend.as_str(),
                    user_id,
                    email.as_deref(),
                    name,
                    &imported.machine_id,
                    note,
                    &now,
                ],
            )
        })?;
        self.store_secrets(&id, imported)?;
        Ok(Upserted {
            account: self.get(&id)?,
            created,
        })
    }

    pub fn store_secrets(&self, id: &QoderAccountId, imported: &StoredPat) -> Result<()> {
        self.secrets.set(
            &qoder_secret(id, QoderSecret::Pat),
            &Secret::new(imported.pat.trim()),
        )?;
        if !imported.job_token.trim().is_empty() {
            self.secrets.set(
                &qoder_secret(id, QoderSecret::JobToken),
                &Secret::new(imported.job_token.trim()),
            )?;
        }
        if !imported.job_refresh.trim().is_empty() {
            self.secrets.set(
                &qoder_secret(id, QoderSecret::JobRefresh),
                &Secret::new(imported.job_refresh.trim()),
            )?;
        }
        let hint = hint_of(imported.pat.trim());
        self.db.with(|c| {
            c.execute(
                "UPDATE qoder_accounts SET
                   has_pat = 1,
                   key_hint = COALESCE(?2, key_hint),
                   access_expires_at = ?3,
                   status = 'active',
                   last_error = NULL,
                   updated_at = ?4
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), hint, &imported.expires_at, now_iso()],
            )
        })?;
        Ok(())
    }

    pub fn secret(&self, id: &QoderAccountId, kind: QoderSecret) -> Result<Option<Secret>> {
        self.secrets.get(&qoder_secret(id, kind))
    }

    pub fn identity_of(&self, account: &QoderAccount) -> QoderIdentity {
        QoderIdentity {
            backend: account.backend,
            user_id: account.user_id.clone().unwrap_or_default(),
            name: account.display_name.clone().unwrap_or_default(),
            email: account.email.clone().unwrap_or_default(),
            machine_id: self
                .db
                .with(|c| {
                    c.query_row(
                        "SELECT machine_id FROM qoder_accounts WHERE id = ?1",
                        [account.id.as_str()],
                        |row| row.get::<_, String>(0),
                    )
                })
                .unwrap_or_default(),
        }
    }

    pub fn remove(&self, id: &QoderAccountId) -> Result<()> {
        let n = self
            .db
            .with(|c| c.execute("DELETE FROM qoder_accounts WHERE id = ?1", [id.as_str()]))?;
        if n == 0 {
            return Err(not_found(id.as_str()));
        }
        for kind in QoderSecret::ALL {
            self.secrets.delete(&qoder_secret(id, kind))?;
        }
        Ok(())
    }

    pub fn set_enabled(&self, id: &QoderAccountId, enabled: bool) -> Result<QoderAccount> {
        self.db.with(|c| {
            c.execute(
                "UPDATE qoder_accounts SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), enabled, now_iso()],
            )
        })?;
        self.get(id)
    }

    pub fn set_note(&self, id: &QoderAccountId, note: Option<&str>) -> Result<QoderAccount> {
        self.db.with(|c| {
            c.execute(
                "UPDATE qoder_accounts SET note = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), note, now_iso()],
            )
        })?;
        self.get(id)
    }

    pub fn record_failure(
        &self,
        id: &QoderAccountId,
        error: &str,
        status: Option<QoderStatus>,
    ) -> Result<()> {
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE qoder_accounts SET
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

    /// 让下一轮 `credential` 重新交换 PAT。job token 先留着，交换成功再盖掉。
    pub fn expire_token(&self, id: &QoderAccountId) -> Result<()> {
        self.db.with(|c| {
            c.execute(
                "UPDATE qoder_accounts SET access_expires_at = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), "1970-01-01T00:00:00Z", now_iso()],
            )
        })?;
        Ok(())
    }

    fn find(&self, id: &QoderAccountId) -> Result<Option<QoderAccount>> {
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

fn account_ref(imported: &StoredPat) -> String {
    let user = imported.user_id.trim();
    if !user.is_empty() {
        return format!("{}:{user}", imported.backend.as_str());
    }
    let mut hasher = sha2::Sha256::new();
    use sha2::Digest;
    hasher.update(imported.pat.trim().as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(16);
    for b in &digest[..8] {
        hex.push(HEX[(*b >> 4) as usize] as char);
        hex.push(HEX[(*b & 0xf) as usize] as char);
    }
    format!("{}:pat:{hex}", imported.backend.as_str())
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn blank_to_none(value: &str) -> Option<&str> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

const SELECT: &str = "SELECT id, account_ref, backend, user_id, email, display_name, machine_id,
        status, enabled, note, key_hint, has_pat, access_expires_at, last_checked_at, last_error,
        created_at, updated_at
 FROM qoder_accounts";

fn row_to_account(row: &Row<'_>) -> rusqlite::Result<QoderAccount> {
    let backend = QoderBackend::parse(&row.get::<_, String>(2)?);
    let status = QoderStatus::parse(&row.get::<_, String>(7)?);
    let email: Option<String> = row.get(4)?;
    let hint: Option<String> = row.get(10)?;
    let user_id: Option<String> = row.get(3)?;
    Ok(QoderAccount {
        id: QoderAccountId::from_raw(row.get::<_, String>(0)?),
        account_ref: row.get(1)?,
        label: compute_label(
            backend,
            email.as_deref(),
            hint.as_deref(),
            user_id.as_deref(),
        ),
        backend,
        plan_type: backend.product().to_string(),
        user_id,
        email,
        display_name: row.get(5)?,
        status,
        enabled: row.get::<_, i64>(8)? != 0,
        note: row.get(9)?,
        key_hint: hint,
        has_token: row.get::<_, i64>(11)? != 0,
        access_expires_at: row.get(12)?,
        last_checked_at: row.get(13)?,
        last_error: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
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
        format!("没有这个 Qoder 账号：{id}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_store::MemorySecrets;

    fn repo() -> QoderAccounts {
        QoderAccounts::new(
            Arc::new(Db::open_in_memory().unwrap()),
            Arc::new(MemorySecrets::new()),
        )
    }

    fn imported(user: &str, pat: &str) -> StoredPat {
        StoredPat {
            backend: QoderBackend::Cn,
            pat: pat.into(),
            user_id: user.into(),
            email: "Me@Example.COM".into(),
            name: "Me".into(),
            machine_id: "machine-1".into(),
            job_token: "jt-1".into(),
            job_refresh: "jrt-1".into(),
            expires_at: "2099-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn the_same_user_updates_in_place_and_the_table_never_holds_the_pat() {
        let r = repo();
        let first = r.upsert(&imported("u-1", "pt-aaaaaaaaaaaa"), None).unwrap();
        let again = r
            .upsert(&imported("u-1", "pt-bbbbbbbbbbbb"), Some("备注"))
            .unwrap();
        assert!(first.created);
        assert!(!again.created);
        assert_eq!(first.account.id, again.account.id);
        assert_eq!(
            r.secret(&again.account.id, QoderSecret::Pat)
                .unwrap()
                .unwrap()
                .expose(),
            "pt-bbbbbbbbbbbb"
        );
        let dump: Vec<String> =
            r.db.with(|c| {
                let mut stmt = c.prepare("SELECT * FROM qoder_accounts")?;
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
        assert!(!dump.join("").contains("pt-bbbbbbbbbbbb"));
        assert!(again.account.has_token);
        assert_eq!(again.account.label(), "me@example.com · Qoder CN");
    }

    #[test]
    fn global_and_cn_of_the_same_user_are_two_accounts() {
        let r = repo();
        let mut cn = imported("u-1", "pt-aaaaaaaaaaaa");
        let mut global = imported("u-1", "pt-cccccccccccc");
        global.backend = QoderBackend::Global;
        assert!(r.upsert(&cn, None).unwrap().created);
        cn.user_id = "u-1".into();
        assert!(r.upsert(&global, None).unwrap().created);
        assert_eq!(r.list().unwrap().len(), 2);
    }

    #[test]
    fn the_lane_looks_accounts_up_by_the_lowercased_label() {
        let r = repo();
        let account = r.upsert(&imported("u-1", "pt-aaaaaaaaaaaa"), None).unwrap();
        let lowered = account.account.label().to_lowercase();
        assert_ne!(lowered, account.account.label());
        assert_eq!(
            r.by_label(&lowered).unwrap().unwrap().id,
            account.account.id
        );
    }
}
