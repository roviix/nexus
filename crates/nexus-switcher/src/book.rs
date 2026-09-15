//! 切号本：可切的账号列表。
//!
//! 一档 = `switch_profiles` 里一行元信息 + 秘密存储里一份整套 `cursorAuth/*`。
//!
//! 机器码的规矩（§5.1）：**首次建档时分配一套，之后固定。** 每次切号都换一套新的，
//! 等于每次登录都是一台新设备，比固定指纹更可疑。

use crate::model::SwitchProfile;
use nexus_core::{now_iso, AppError, ErrorCode, MachineProfile, ProfileId, Result, Secret};
use nexus_cursor::AuthBundle;
use nexus_store::{keys, Db, SecretStore};
use rusqlite::Row;
use std::sync::Arc;

pub struct SwitchBook {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
}

impl SwitchBook {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self { db, secrets }
    }

    /// 全部档，最近切过的在前，没切过的按建档时间。
    pub fn list(&self, current_email: Option<&str>) -> Result<Vec<SwitchProfile>> {
        let rows: Vec<ProfileRow> = self.db.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, email, membership, signup_type, note, machine_ids_json,
                        created_at, updated_at, last_switched_at
                 FROM switch_profiles
                 ORDER BY last_switched_at DESC NULLS LAST, created_at ASC",
            )?;
            let rows = stmt.query_map([], ProfileRow::from_row)?;
            rows.collect()
        })?;
        Ok(rows
            .into_iter()
            .map(|r| self.hydrate(r, current_email))
            .collect())
    }

    pub fn get(&self, id: &ProfileId, current_email: Option<&str>) -> Result<SwitchProfile> {
        let row = self.row(id)?;
        Ok(self.hydrate(row, current_email))
    }

    /// 取一档的登录态（**含明文 token**）。只给切号编排用。
    pub fn auth(&self, id: &ProfileId) -> Result<AuthBundle> {
        let secret = self.secrets.get(&keys::profile_auth(id))?.ok_or_else(|| {
            AppError::new(
                ErrorCode::ProfileIncomplete,
                "这一档的登录态没存下来，切不进去。",
            )
            .with_hint("重新「收录当前登录」或删掉这一档。")
        })?;
        let bundle: AuthBundle = serde_json::from_str(secret.expose())?;
        Ok(bundle)
    }

    /// 按邮箱建档或更新。
    ///
    /// 已存在时：更新登录态和显示字段，**保留原有机器码**——那是这个号一直在用的
    /// 设备指纹，换掉等于制造一次「换设备登录」。
    pub fn upsert(
        &self,
        auth: &AuthBundle,
        note: Option<&str>,
        machine_ids: Option<MachineProfile>,
    ) -> Result<SwitchProfile> {
        let email = auth.email().ok_or_else(|| {
            AppError::new(
                ErrorCode::ProfileIncomplete,
                "这套登录态里没有邮箱，收录不了。",
            )
        })?;
        let now = now_iso();
        let summary = auth.summary();

        let existing: Option<ProfileRow> = self.db.with(|c| {
            c.query_row(
                "SELECT id, email, membership, signup_type, note, machine_ids_json,
                        created_at, updated_at, last_switched_at
                 FROM switch_profiles WHERE email = ?1",
                [&email],
                ProfileRow::from_row,
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
        })?;

        let (id, ids) = match &existing {
            Some(row) => (ProfileId::from_raw(row.id.clone()), row.machine_ids.clone()),
            // 新档：给一套专属机器码。调用方传了就用传的（「收录当前登录」传的是
            // 这个号此刻正在用的那套），否则新造。
            None => (
                ProfileId::new(),
                machine_ids.unwrap_or_else(MachineProfile::generate),
            ),
        };

        // 先写秘密再写元信息：反过来的话，元信息说有、秘密没有，界面会显示一个
        // 切不进去的档。
        self.secrets.set(
            &keys::profile_auth(&id),
            &Secret::new(serde_json::to_string(auth)?),
        )?;

        let ids_json = serde_json::to_string(&ids)?;
        self.db.with(|c| {
            c.execute(
                "INSERT INTO switch_profiles
                   (id, email, membership, signup_type, note, machine_ids_json, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 ON CONFLICT(email) DO UPDATE SET
                   membership  = excluded.membership,
                   signup_type = excluded.signup_type,
                   note        = COALESCE(excluded.note, switch_profiles.note),
                   updated_at  = excluded.updated_at",
                rusqlite::params![
                    id.as_str(),
                    &email,
                    summary.membership,
                    summary.signup_type,
                    note,
                    ids_json,
                    now,
                ],
            )
        })?;

        self.get(&id, None)
    }

    pub fn set_note(&self, id: &ProfileId, note: Option<&str>) -> Result<()> {
        let n = self.db.with(|c| {
            c.execute(
                "UPDATE switch_profiles SET note = ?2, updated_at = ?3 WHERE id = ?1",
                rusqlite::params![id.as_str(), note, now_iso()],
            )
        })?;
        if n == 0 {
            return Err(not_found(id));
        }
        Ok(())
    }

    /// 删一档：元信息那行和存下来的登录态一起删，不留孤儿。
    pub fn remove(&self, id: &ProfileId) -> Result<()> {
        let n = self
            .db
            .with(|c| c.execute("DELETE FROM switch_profiles WHERE id = ?1", [id.as_str()]))?;
        if n == 0 {
            return Err(not_found(id));
        }
        self.secrets.delete(&keys::profile_auth(id))?;
        Ok(())
    }

    pub(crate) fn mark_switched(&self, id: &ProfileId) -> Result<()> {
        let now = now_iso();
        self.db.with(|c| {
            c.execute(
                "UPDATE switch_profiles SET last_switched_at = ?2, updated_at = ?2 WHERE id = ?1",
                rusqlite::params![id.as_str(), now],
            )
        })?;
        Ok(())
    }

    /// 哪一档拥有这套机器码。用于「当前机器码属于谁」。
    pub(crate) fn owner_of_machine(&self, machine_id: &str) -> Result<Option<String>> {
        if machine_id.is_empty() {
            return Ok(None);
        }
        let rows: Vec<ProfileRow> = self.db.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, email, membership, signup_type, note, machine_ids_json,
                        created_at, updated_at, last_switched_at FROM switch_profiles",
            )?;
            let rows = stmt.query_map([], ProfileRow::from_row)?;
            rows.collect()
        })?;
        Ok(rows
            .into_iter()
            .find(|r| r.machine_ids.machine_id == machine_id)
            .map(|r| r.email))
    }

    fn row(&self, id: &ProfileId) -> Result<ProfileRow> {
        self.db
            .with(|c| {
                c.query_row(
                    "SELECT id, email, membership, signup_type, note, machine_ids_json,
                            created_at, updated_at, last_switched_at
                     FROM switch_profiles WHERE id = ?1",
                    [id.as_str()],
                    ProfileRow::from_row,
                )
                .map(Some)
                .or_else(|e| match e {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
            })?
            .ok_or_else(|| not_found(id))
    }

    /// 这一档存的 refresh 是不是 access 占位（毒档案，见 `AuthBundle::refresh_is_placeholder`）。
    ///
    /// 要解一次密才知道，比 `exists` 贵；但切号本只有几十档，而代价是切过去就报废一个
    /// 找不回来的号——值得。解不开就当它没问题，让 `switch_to` 那道闸去拦。
    fn refresh_is_placeholder(&self, id: &ProfileId) -> bool {
        self.secrets
            .get(&keys::profile_auth(id))
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<AuthBundle>(s.expose()).ok())
            .is_some_and(|b| b.refresh_is_placeholder())
    }

    fn hydrate(&self, row: ProfileRow, current_email: Option<&str>) -> SwitchProfile {
        let id = ProfileId::from_raw(row.id);
        SwitchProfile {
            has_auth: self.secrets.exists(&keys::profile_auth(&id)),
            refresh_is_placeholder: self.refresh_is_placeholder(&id),
            is_current: current_email.is_some_and(|e| e.eq_ignore_ascii_case(&row.email)),
            id,
            email: row.email,
            membership: row.membership,
            signup_type: row.signup_type,
            note: row.note,
            machine_ids: row.machine_ids,
            created_at: row.created_at,
            updated_at: row.updated_at,
            last_switched_at: row.last_switched_at,
        }
    }
}

fn not_found(id: &ProfileId) -> AppError {
    AppError::new(
        ErrorCode::ProfileNotFound,
        format!("切号本里没有这一档（{id}）。"),
    )
}

struct ProfileRow {
    id: String,
    email: String,
    membership: Option<String>,
    signup_type: Option<String>,
    note: Option<String>,
    machine_ids: MachineProfile,
    created_at: String,
    updated_at: String,
    last_switched_at: Option<String>,
}

impl ProfileRow {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let ids_json: String = row.get(5)?;
        Ok(Self {
            id: row.get(0)?,
            email: row.get(1)?,
            membership: row.get(2)?,
            signup_type: row.get(3)?,
            note: row.get(4)?,
            // 机器码解析不了就当空的：这一档还能显示和删除，只是切的时候会重新分配。
            machine_ids: serde_json::from_str(&ids_json).unwrap_or_default(),
            created_at: row.get(6)?,
            updated_at: row.get(7)?,
            last_switched_at: row.get(8)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_store::MemorySecrets;

    fn book() -> SwitchBook {
        SwitchBook::new(
            Arc::new(Db::open_in_memory().unwrap()),
            Arc::new(MemorySecrets::new()),
        )
    }

    fn auth_for(email: &str) -> AuthBundle {
        let mut b = AuthBundle::new();
        b.insert("cursorAuth/accessToken", "at");
        b.insert("cursorAuth/refreshToken", "rt");
        b.insert("cursorAuth/cachedEmail", email);
        b.insert("cursorAuth/stripeMembershipType", "ultra");
        b
    }

    #[test]
    fn upsert_creates_then_updates_by_email() {
        let book = book();
        let a = book
            .upsert(&auth_for("a@example.com"), Some("自用"), None)
            .unwrap();
        assert_eq!(a.email, "a@example.com");
        assert_eq!(a.membership.as_deref(), Some("ultra"));
        assert_eq!(a.note.as_deref(), Some("自用"));
        assert!(a.has_auth);

        let b = book.upsert(&auth_for("a@example.com"), None, None).unwrap();
        assert_eq!(a.id, b.id, "同一个邮箱不该建出第二档");
        assert_eq!(book.list(None).unwrap().len(), 1);
    }

    #[test]
    fn a_profile_keeps_its_machine_ids_across_updates() {
        let book = book();
        let first = book.upsert(&auth_for("a@example.com"), None, None).unwrap();
        let again = book.upsert(&auth_for("a@example.com"), None, None).unwrap();
        assert_eq!(
            first.machine_ids, again.machine_ids,
            "机器码建档时定，之后固定"
        );
        assert!(!first.machine_ids.machine_id.is_empty());
    }

    #[test]
    fn different_profiles_get_different_machine_ids() {
        let book = book();
        let a = book.upsert(&auth_for("a@example.com"), None, None).unwrap();
        let b = book.upsert(&auth_for("b@example.com"), None, None).unwrap();
        assert_ne!(a.machine_ids.machine_id, b.machine_ids.machine_id);
    }

    #[test]
    fn capture_can_supply_the_accounts_existing_machine_ids() {
        let book = book();
        let existing = MachineProfile::generate();
        let p = book
            .upsert(&auth_for("a@example.com"), None, Some(existing.clone()))
            .unwrap();
        assert_eq!(p.machine_ids, existing);
    }

    #[test]
    fn auth_round_trips_through_the_secret_store() {
        let book = book();
        let p = book.upsert(&auth_for("a@example.com"), None, None).unwrap();
        let back = book.auth(&p.id).unwrap();
        assert_eq!(back.get("cursorAuth/refreshToken"), Some("rt"));
        assert!(back.is_switchable());
    }

    #[test]
    fn remove_clears_the_stored_secret_too() {
        let secrets = Arc::new(MemorySecrets::new());
        let book = SwitchBook::new(Arc::new(Db::open_in_memory().unwrap()), secrets.clone());
        let p = book.upsert(&auth_for("a@example.com"), None, None).unwrap();
        assert_eq!(secrets.len(), 1);
        book.remove(&p.id).unwrap();
        assert_eq!(secrets.len(), 0, "秘密存储里不该留孤儿");
        assert!(book.list(None).unwrap().is_empty());
    }

    #[test]
    fn missing_profile_reports_profile_not_found() {
        let book = book();
        let ghost = ProfileId::from_raw("nope");
        assert_eq!(
            book.get(&ghost, None).unwrap_err().code,
            ErrorCode::ProfileNotFound
        );
        assert_eq!(
            book.remove(&ghost).unwrap_err().code,
            ErrorCode::ProfileNotFound
        );
        assert_eq!(
            book.set_note(&ghost, Some("x")).unwrap_err().code,
            ErrorCode::ProfileNotFound
        );
    }

    #[test]
    fn a_profile_without_a_stored_secret_is_flagged_not_hidden() {
        let secrets = Arc::new(MemorySecrets::new());
        let book = SwitchBook::new(Arc::new(Db::open_in_memory().unwrap()), secrets.clone());
        let p = book.upsert(&auth_for("a@example.com"), None, None).unwrap();
        secrets.delete(&keys::profile_auth(&p.id)).unwrap();

        let listed = book.list(None).unwrap();
        assert_eq!(listed.len(), 1, "档还在列表里，只是标成不可切");
        assert!(!listed[0].has_auth);
        assert_eq!(
            book.auth(&p.id).unwrap_err().code,
            ErrorCode::ProfileIncomplete
        );
    }

    /// 旧版本给「仅会话」号收录时留下的毒档案：refresh 那一格里其实是 access。
    #[test]
    fn a_profile_whose_refresh_is_really_the_access_token_is_flagged() {
        let book = book();
        let mut poisoned = AuthBundle::new();
        poisoned.insert("cursorAuth/accessToken", "same-jwt");
        poisoned.insert("cursorAuth/refreshToken", "same-jwt");
        poisoned.insert("cursorAuth/cachedEmail", "session-only@example.com");
        let p = book.upsert(&poisoned, None, None).unwrap();

        // 登录态确实在，看起来「能切」——正是它危险的地方。
        assert!(p.has_auth);
        assert!(book.auth(&p.id).unwrap().is_switchable());
        // 但必须被认出来，否则切过去就报废一个找不回来的号。
        assert!(p.refresh_is_placeholder);
        assert!(book.list(None).unwrap()[0].refresh_is_placeholder);

        // 成对的真 token 不该被误伤。
        let ok = book
            .upsert(&auth_for("normal@example.com"), None, None)
            .unwrap();
        assert!(!ok.refresh_is_placeholder);
    }

    #[test]
    fn current_account_is_marked_case_insensitively() {
        let book = book();
        book.upsert(&auth_for("a@example.com"), None, None).unwrap();
        book.upsert(&auth_for("b@example.com"), None, None).unwrap();
        let list = book.list(Some("A@Example.COM")).unwrap();
        assert_eq!(list.iter().filter(|p| p.is_current).count(), 1);
        assert!(
            list.iter()
                .find(|p| p.email == "a@example.com")
                .unwrap()
                .is_current
        );
    }

    #[test]
    fn most_recently_switched_sorts_first() {
        let book = book();
        let a = book.upsert(&auth_for("a@example.com"), None, None).unwrap();
        let b = book.upsert(&auth_for("b@example.com"), None, None).unwrap();
        book.mark_switched(&b.id).unwrap();
        let list = book.list(None).unwrap();
        assert_eq!(list[0].id, b.id);
        assert_eq!(list[1].id, a.id);
        assert!(list[0].last_switched_at.is_some());
    }

    #[test]
    fn machine_owner_lookup_finds_the_profile() {
        let book = book();
        let p = book.upsert(&auth_for("a@example.com"), None, None).unwrap();
        assert_eq!(
            book.owner_of_machine(&p.machine_ids.machine_id)
                .unwrap()
                .as_deref(),
            Some("a@example.com")
        );
        assert!(book.owner_of_machine("nobody").unwrap().is_none());
        assert!(book.owner_of_machine("").unwrap().is_none());
    }

    #[test]
    fn auth_without_an_email_is_refused() {
        let book = book();
        let mut b = AuthBundle::new();
        b.insert("cursorAuth/accessToken", "at");
        assert_eq!(
            book.upsert(&b, None, None).unwrap_err().code,
            ErrorCode::ProfileIncomplete
        );
    }

    #[test]
    fn note_survives_an_update_that_does_not_carry_one() {
        let book = book();
        let p = book
            .upsert(&auth_for("a@example.com"), Some("买家甲"), None)
            .unwrap();
        book.upsert(&auth_for("a@example.com"), None, None).unwrap();
        assert_eq!(
            book.get(&p.id, None).unwrap().note.as_deref(),
            Some("买家甲")
        );
    }
}
