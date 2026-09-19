//! ZCode 账号仓库：`zcode_accounts` 表存元信息，凭证交给 `SecretStore`。

use crate::import::ImportedAccount;
use crate::model::{ZcodeAccount, ZcodePlan, ZcodeProvider, ZcodeStatus};
use nexus_core::{now_iso, AppError, ErrorCode, Result, Secret, ZcodeAccountId};
use nexus_store::keys::{zcode_secret, ZcodeSecret};
use nexus_store::{Db, SecretStore};
use rusqlite::Row;
use std::sync::Arc;

pub struct ZcodeAccounts {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
}

#[derive(Debug, Clone)]
pub struct Upserted {
    pub account: ZcodeAccount,
    pub created: bool,
}

/// API key 的前 8 位。给人在列表里认哪张是哪张，不足以复原这把 key。
fn hint_of(api_key: &str) -> Option<String> {
    let id = api_key.split('.').next().unwrap_or_default();
    (id.len() >= 8).then(|| format!("{}…", &id[..8]))
}

impl ZcodeAccounts {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { db, secrets }
    }

    pub fn list(&self) -> Result<Vec<ZcodeAccount>> {
        self.db.with(|c| {
            let mut stmt = c.prepare(&format!("{SELECT} ORDER BY created_at ASC, id ASC"))?;
            let rows = stmt.query_map([], row_to_account)?;
            rows.collect()
        })
    }

    pub fn get(&self, id: &ZcodeAccountId) -> Result<ZcodeAccount> {
        self.find(id)?.ok_or_else(|| not_found(id.as_str()))
    }

    pub fn by_ref(&self, account_ref: &str) -> Result<Option<ZcodeAccount>> {
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

    /// 导入一条。已存在的按 `account_ref` 更新凭证，不新建 —— 重复导入是常态操作，
    /// 每次都新建会让列表越滚越长。
    pub fn upsert(&self, imported: &ImportedAccount, note: Option<&str>) -> Result<Upserted> {
        let account_ref = imported.account_ref();
        if imported.api_key.is_none() && imported.jwt.is_none() {
            return Err(AppError::invalid(
                "这条 ZCode 凭证里既没有 API key 也没有 JWT。",
            ));
        }
        let email = imported
            .email
            .as_deref()
            .map(|e| e.trim().to_ascii_lowercase())
            .filter(|e| !e.is_empty());
        let now = now_iso();

        let (id, created) = match self.by_ref(&account_ref)? {
            Some(existing) => (existing.id, false),
            None => {
                let id = ZcodeAccountId::new();
                self.db.with(|c| {
                    c.execute(
                        "INSERT INTO zcode_accounts
                           (id, account_ref, provider, plan, family, email, status, enabled,
                            note, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9, ?9)",
                        rusqlite::params![
                            id.as_str(),
                            &account_ref,
                            imported.provider.as_str(),
                            imported.plan.as_str(),
                            imported.family.as_deref(),
                            email.as_deref(),
                            ZcodeStatus::NeedsLogin.as_str(),
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
                "UPDATE zcode_accounts SET
                   provider = ?2,
                   plan = ?3,
                   family = COALESCE(?4, family),
                   email = COALESCE(?5, email),
                   note = COALESCE(?6, note),
                   updated_at = ?7
                 WHERE id = ?1",
                rusqlite::params![
                    id.as_str(),
                    imported.provider.as_str(),
                    imported.plan.as_str(),
                    imported.family.as_deref(),
                    email.as_deref(),
                    note,
                    &now,
                ],
            )
        })?;

        self.store_credentials(&id, imported.api_key.as_deref(), imported.jwt.as_deref())?;
        Ok(Upserted {
            account: self.get(&id)?,
            created,
        })
    }

    pub fn store_credentials(
        &self,
        id: &ZcodeAccountId,
        api_key: Option<&str>,
        jwt: Option<&str>,
    ) -> Result<()> {
        if let Some(key) = api_key.map(str::trim).filter(|k| !k.is_empty()) {
            self.secrets
                .set(&zcode_secret(id, ZcodeSecret::ApiKey), &Secret::new(key))?;
        }
        if let Some(jwt) = jwt.map(str::trim).filter(|j| !j.is_empty()) {
            self.secrets
                .set(&zcode_secret(id, ZcodeSecret::Jwt), &Secret::new(jwt))?;
        }
        let has_key = self.secrets.exists(&zcode_secret(id, ZcodeSecret::ApiKey));
        let has_jwt = self.secrets.exists(&zcode_secret(id, ZcodeSecret::Jwt));
        let hint = api_key.and_then(hint_of);
        self.db.with(|c| {
            c.execute(
                "UPDATE zcode_accounts SET
                   has_api_key = ?2, has_jwt = ?3,
                   key_hint = COALESCE(?4, key_hint),
                   status = 'active', last_error = NULL, updated_at = ?5
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), has_key, has_jwt, hint, now_iso()],
            )
        })?;
        Ok(())
    }

    pub fn secret(&self, id: &ZcodeAccountId, kind: ZcodeSecret) -> Result<Option<Secret>> {
        self.secrets.get(&zcode_secret(id, kind))
    }

    pub fn remove(&self, id: &ZcodeAccountId) -> Result<()> {
        let n = self
            .db
            .with(|c| c.execute("DELETE FROM zcode_accounts WHERE id = ?1", [id.as_str()]))?;
        if n == 0 {
            return Err(not_found(id.as_str()));
        }
        for kind in ZcodeSecret::ALL {
            self.secrets.delete(&zcode_secret(id, kind))?;
        }
        Ok(())
    }

    pub fn set_enabled(&self, id: &ZcodeAccountId, enabled: bool) -> Result<ZcodeAccount> {
        self.db.with(|c| {
            c.execute(
                "UPDATE zcode_accounts SET enabled = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), enabled, now_iso()],
            )
        })?;
        self.get(id)
    }

    pub fn set_note(&self, id: &ZcodeAccountId, note: Option<&str>) -> Result<ZcodeAccount> {
        self.db.with(|c| {
            c.execute(
                "UPDATE zcode_accounts SET note = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), note, now_iso()],
            )
        })?;
        self.get(id)
    }

    pub fn record_failure(
        &self,
        id: &ZcodeAccountId,
        error: &str,
        status: Option<ZcodeStatus>,
    ) -> Result<()> {
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE zcode_accounts SET
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

    pub fn record_success(&self, id: &ZcodeAccountId) -> Result<()> {
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE zcode_accounts SET
                   last_checked_at = ?2, last_error = NULL, status = 'active', updated_at = ?2
                 WHERE id = ?1",
                rusqlite::params![id.as_str(), now],
            )
        })?;
        Ok(())
    }

    fn find(&self, id: &ZcodeAccountId) -> Result<Option<ZcodeAccount>> {
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

const SELECT: &str = "SELECT id, account_ref, provider, plan, family, email, status, enabled,
        note, key_hint, has_api_key, has_jwt, last_checked_at, last_error, created_at, updated_at
 FROM zcode_accounts";

fn row_to_account(row: &Row<'_>) -> rusqlite::Result<ZcodeAccount> {
    let provider: String = row.get(2)?;
    let plan: String = row.get(3)?;
    let status: String = row.get(6)?;
    let plan = ZcodePlan::parse(&plan);
    let account_ref: String = row.get(1)?;
    let family: Option<String> = row.get(4)?;
    let email: Option<String> = row.get(5)?;
    Ok(ZcodeAccount {
        id: ZcodeAccountId::from_raw(row.get::<_, String>(0)?),
        label: crate::model::compute_label(email.as_deref(), family.as_deref(), plan, &account_ref),
        account_ref,
        provider: ZcodeProvider::parse(&provider),
        plan,
        family,
        email,
        status: ZcodeStatus::parse(&status),
        enabled: row.get::<_, i64>(7)? != 0,
        note: row.get(8)?,
        key_hint: row.get(9)?,
        has_api_key: row.get::<_, i64>(10)? != 0,
        has_jwt: row.get::<_, i64>(11)? != 0,
        last_checked_at: row.get(12)?,
        last_error: row.get(13)?,
        created_at: row.get(14)?,
        updated_at: row.get(15)?,
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
        format!("没有这个 ZCode 账号：{id}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_store::MemorySecrets;

    fn repo() -> ZcodeAccounts {
        ZcodeAccounts::new(
            Arc::new(Db::open_in_memory().unwrap()),
            Arc::new(MemorySecrets::new()),
        )
    }

    fn coding(family: &str, key: &str) -> ImportedAccount {
        ImportedAccount {
            provider: ZcodeProvider::Zai,
            plan: ZcodePlan::CodingPlan,
            family: Some(family.into()),
            account_uuid: Some("u-1".into()),
            email: Some("Me@Example.COM".into()),
            api_key: Some(key.into()),
            jwt: None,
        }
    }

    #[test]
    fn individual_and_team_are_two_accounts_not_one() {
        // 同一个邮箱、同一个 uuid，只有套餐族不同 —— 它们的额度是分开的，必须是两条。
        let r = repo();
        let a = r
            .upsert(&coding("zai-individual-coding-plan", "aaaaaaaa11.k1"), None)
            .unwrap();
        let b = r
            .upsert(&coding("zai-team-coding-plan", "bbbbbbbb22.k2"), None)
            .unwrap();
        assert!(a.created && b.created);
        assert_ne!(a.account.id, b.account.id);
        assert_eq!(r.list().unwrap().len(), 2);
    }

    #[test]
    fn re_importing_updates_in_place_instead_of_piling_up() {
        let r = repo();
        let first = r
            .upsert(&coding("zai-individual-coding-plan", "aaaaaaaa11.k1"), None)
            .unwrap();
        let again = r
            .upsert(&coding("zai-individual-coding-plan", "cccccccc33.k9"), None)
            .unwrap();
        assert!(!again.created);
        assert_eq!(first.account.id, again.account.id);
        assert_eq!(r.list().unwrap().len(), 1);
        // 凭证要换成新的那把。
        assert_eq!(
            r.secret(&again.account.id, ZcodeSecret::ApiKey)
                .unwrap()
                .unwrap()
                .expose(),
            "cccccccc33.k9"
        );
        assert_eq!(again.account.key_hint.as_deref(), Some("cccccccc…"));
    }

    #[test]
    fn the_table_never_holds_the_credential() {
        let r = repo();
        let up = r
            .upsert(
                &coding("zai-individual-coding-plan", "aaaaaaaa11.supersecret"),
                None,
            )
            .unwrap();
        assert!(up.account.has_api_key);
        let dump: Vec<String> =
            r.db.with(|c| {
                let mut stmt = c.prepare("SELECT * FROM zcode_accounts")?;
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
        assert!(!dump.iter().any(|s| s.contains("supersecret")));
    }

    #[test]
    fn removing_an_account_takes_both_of_its_secrets_with_it() {
        let r = repo();
        let mut imported = coding("zai-individual-coding-plan", "aaaaaaaa11.k1");
        imported.jwt = Some("a.b.c".into());
        let up = r.upsert(&imported, None).unwrap();
        assert!(up.account.has_api_key && up.account.has_jwt);
        r.remove(&up.account.id).unwrap();
        for kind in ZcodeSecret::ALL {
            assert!(r.secret(&up.account.id, kind).unwrap().is_none());
        }
        assert!(r.get(&up.account.id).is_err());
    }

    #[test]
    fn an_import_with_neither_credential_is_refused() {
        let r = repo();
        let mut empty = coding("zai-individual-coding-plan", "x");
        empty.api_key = None;
        assert!(r.upsert(&empty, None).is_err());
    }
}
