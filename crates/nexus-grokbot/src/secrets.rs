//! `sand-secrets.json`：Grok Bot 里的 Cursor 账号槽与设备 machineId。
//!
//! ```json
//! { "cursor-machine-id": "<enc>", "cursor-accounts": "<json: {active, accounts:{slot:{cursor-access-token, cursor-refresh-token, cursor-account-profile}}}>" }
//! ```
//! 只解**活跃**槽。其它槽可能是历史账号，口令换过就解不开，不要因此整体失败。

use crate::app::user_data_dir;
use crate::keychain;
use nexus_core::{AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveAccount {
    pub slot: String,
    pub email: Option<String>,
    pub name: Option<String>,
    /// WorkOS `sub`（`auth0|user_…`），从 access token 里解出来；不是秘密。
    pub subject: Option<String>,
}

/// 解出来的秘密。**含 token**，不进日志、不进事件。
pub struct GrokBotSecrets {
    pub machine_id: String,
    pub session_token: String,
    pub refresh_token: Option<String>,
    pub account: ActiveAccount,
}

impl std::fmt::Debug for GrokBotSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrokBotSecrets")
            .field(
                "machine_id",
                &format!("{}…", &self.machine_id[..self.machine_id.len().min(8)]),
            )
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

#[derive(Deserialize)]
struct SecretsFile {
    #[serde(rename = "cursor-machine-id")]
    machine_id: Option<String>,
    #[serde(rename = "cursor-accounts")]
    accounts: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct AccountsBlob {
    active: Option<String>,
    #[serde(default)]
    accounts: BTreeMap<String, AccountSlot>,
}

#[derive(Deserialize)]
struct AccountSlot {
    #[serde(rename = "cursor-access-token")]
    access_token: Option<String>,
    #[serde(rename = "cursor-refresh-token")]
    refresh_token: Option<String>,
    #[serde(rename = "cursor-account-profile")]
    profile: Option<String>,
}

#[derive(Deserialize)]
struct Profile {
    email: Option<String>,
    name: Option<String>,
}

pub fn secrets_path() -> std::path::PathBuf {
    user_data_dir().join("sand-secrets.json")
}

/// 读活跃账号 + machineId。需要钥匙串口令。
pub fn load() -> Result<GrokBotSecrets> {
    let path = secrets_path();
    let raw = std::fs::read_to_string(&path).map_err(|_| {
        AppError::new(
            ErrorCode::CursorNotFound,
            "没找到 Grok Bot 的 sand-secrets.json。",
        )
        .with_hint("先打开 Grok Bot 并登录一个账号。")
    })?;
    let file: SecretsFile = serde_json::from_str(&raw)?;
    let password = keychain::safe_storage_password()?;
    let key = keychain::derive_key(&password);

    let machine_id = file
        .machine_id
        .as_deref()
        .ok_or_else(|| AppError::internal("sand-secrets.json 里没有 cursor-machine-id。"))
        .and_then(|v| keychain::decrypt(v, &key))?;

    let accounts_raw = file
        .accounts
        .ok_or_else(|| AppError::new(ErrorCode::NotLoggedIn, "Grok Bot 还没登录任何账号。"))?;
    let blob: AccountsBlob = match accounts_raw {
        serde_json::Value::String(s) => serde_json::from_str(&s)?,
        other => serde_json::from_value(other)?,
    };
    let slot_id = blob
        .active
        .clone()
        .ok_or_else(|| AppError::new(ErrorCode::NotLoggedIn, "Grok Bot 没有活跃账号。"))?;
    let slot = blob
        .accounts
        .get(&slot_id)
        .ok_or_else(|| AppError::internal("Grok Bot 的活跃账号槽不在账号表里。"))?;
    let session_token = slot
        .access_token
        .as_deref()
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::NotLoggedIn,
                "Grok Bot 活跃账号没有 access token。",
            )
        })
        .and_then(|v| keychain::decrypt(v, &key))?;
    let refresh_token = slot
        .refresh_token
        .as_deref()
        .and_then(|v| keychain::decrypt(v, &key).ok());
    let profile: Option<Profile> = slot
        .profile
        .as_deref()
        .and_then(|v| keychain::decrypt(v, &key).ok())
        .and_then(|s| serde_json::from_str(&s).ok());

    Ok(GrokBotSecrets {
        machine_id: machine_id.trim().to_string(),
        account: ActiveAccount {
            slot: slot_id,
            email: profile.as_ref().and_then(|p| p.email.clone()),
            name: profile.and_then(|p| p.name),
            subject: jwt_subject(&session_token),
        },
        session_token,
        refresh_token,
    })
}

/// 账号作用域：descriptor 按 `sha256(sub 或整段 token)` 索引。
pub fn account_scope(session_token: &str) -> String {
    use sha2::{Digest, Sha256};
    let principal = jwt_subject(session_token).unwrap_or_else(|| session_token.to_string());
    let mut h = Sha256::new();
    h.update(principal.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn jwt_subject(token: &str) -> Option<String> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("sub")?.as_str().map(str::to_string)
}

/// JWT `exp`（秒）。
/// 要写进 Grok Bot 活跃槽的登录态。`refresh_token` 可以和 `access_token` 是同一把
/// 桌面 session JWT：客户端续期走 `/oauth/token`，和 Cursor 一样认这把当 refresh。
pub struct ClientLogin {
    pub access_token: String,
    pub refresh_token: String,
    pub email: String,
}

const ACCESS_FIELD: &str = "cursor-access-token";
const REFRESH_FIELD: &str = "cursor-refresh-token";
const PROFILE_FIELD: &str = "cursor-account-profile";
const ACCOUNTS_FIELD: &str = "cursor-accounts";
const MACHINE_FIELD: &str = "cursor-machine-id";

/// 把这个号写成 Grok Bot 的活跃登录，其它槽保留。文件必须已经存在（里面有 machine id）。
///
/// `cursor-accounts` 必须是**字符串**里的 JSON：客户端读盘时丢掉非字符串字段。
pub fn write_active_login(
    path: &std::path::Path,
    key: &[u8; 16],
    login: &ClientLogin,
) -> Result<String> {
    let raw = std::fs::read_to_string(path).map_err(|_| {
        AppError::new(
            ErrorCode::CursorNotFound,
            "没找到 Grok Bot 的 sand-secrets.json。",
        )
        .with_hint("先打开一次 Grok Bot，让它生成设备号。")
    })?;
    let mut root: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&raw)
        .map_err(|_| AppError::internal("sand-secrets.json 不是 JSON。"))?;
    let machine = root.get(MACHINE_FIELD).and_then(|v| v.as_str());
    if machine.is_none() || machine.is_some_and(|s| s.is_empty()) {
        return Err(
            AppError::new(ErrorCode::CursorNotFound, "Grok Bot 还没有设备号。")
                .with_hint("先打开一次 Grok Bot 并登录任意账号。"),
        );
    }

    let mut record = match root.get(ACCOUNTS_FIELD) {
        Some(serde_json::Value::String(s)) => serde_json::from_str::<serde_json::Value>(s)
            .unwrap_or_else(|_| serde_json::json!({"active": null, "accounts": {}})),
        Some(other) => other.clone(),
        None => serde_json::json!({"active": null, "accounts": {}}),
    };
    let slot = account_scope(&login.access_token);
    let profile = serde_json::json!({ "email": login.email }).to_string();
    {
        let accounts = record
            .as_object_mut()
            .and_then(|o| o.get_mut("accounts"))
            .and_then(|a| a.as_object_mut())
            .ok_or_else(|| AppError::internal("Grok Bot 账号表损坏。"))?;
        let mut fields = accounts
            .get(&slot)
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        fields.insert(
            ACCESS_FIELD.into(),
            serde_json::Value::String(keychain::encrypt(&login.access_token, key)),
        );
        fields.insert(
            REFRESH_FIELD.into(),
            serde_json::Value::String(keychain::encrypt(&login.refresh_token, key)),
        );
        fields.insert(
            PROFILE_FIELD.into(),
            serde_json::Value::String(keychain::encrypt(&profile, key)),
        );
        accounts.insert(slot.clone(), serde_json::Value::Object(fields));
    }
    if let Some(obj) = record.as_object_mut() {
        obj.insert("active".into(), serde_json::Value::String(slot.clone()));
    }
    root.insert(
        ACCOUNTS_FIELD.into(),
        serde_json::Value::String(serde_json::to_string(&record)?),
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(&root)?))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(slot)
}

/// 写本机 Grok Bot 的 `sand-secrets.json`。要钥匙串口令。
pub fn install_active_login(login: &ClientLogin) -> Result<String> {
    let password = keychain::safe_storage_password()?;
    let key = keychain::derive_key(&password);
    write_active_login(&secrets_path(), &key, login)
}

pub fn jwt_exp_ms(token: &str) -> Option<u64> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("exp")?.as_u64().map(|s| s * 1000)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JWT: &str =
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhdXRoMHx1c2VyXzAxIiwiZXhwIjoxNzAwMDAwMDAwfQ.sig";

    #[test]
    fn subject_and_exp_parse() {
        assert_eq!(jwt_subject(JWT).as_deref(), Some("auth0|user_01"));
        assert_eq!(jwt_exp_ms(JWT), Some(1_700_000_000_000));
    }

    #[test]
    fn scope_hashes_the_subject() {
        let s = account_scope(JWT);
        assert_eq!(s.len(), 64);
        assert_eq!(s, account_scope(JWT));
    }

    #[test]
    fn active_login_keeps_machine_id_and_other_slots() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sand-secrets.json");
        let key = keychain::derive_key("test-key");
        let existing = serde_json::json!({
            "cursor-machine-id": "enc-machine",
            "local-exec-file-key": "enc-file",
            "cursor-accounts": serde_json::json!({
                "active": "old",
                "accounts": { "old": { "cursor-access-token": "keep" } }
            }).to_string()
        });
        std::fs::write(&path, existing.to_string()).unwrap();
        let slot = write_active_login(
            &path,
            &key,
            &ClientLogin {
                access_token: JWT.into(),
                refresh_token: JWT.into(),
                email: "a@b.c".into(),
            },
        )
        .unwrap();
        assert_eq!(slot, account_scope(JWT));
        let root: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(root["cursor-machine-id"], "enc-machine");
        assert_eq!(root["local-exec-file-key"], "enc-file");
        let accounts: serde_json::Value =
            serde_json::from_str(root["cursor-accounts"].as_str().unwrap()).unwrap();
        assert_eq!(accounts["active"], slot);
        assert!(accounts["accounts"]["old"]["cursor-access-token"].is_string());
        let fields = &accounts["accounts"][&slot];
        let access =
            keychain::decrypt(fields["cursor-access-token"].as_str().unwrap(), &key).unwrap();
        let refresh =
            keychain::decrypt(fields["cursor-refresh-token"].as_str().unwrap(), &key).unwrap();
        let profile =
            keychain::decrypt(fields["cursor-account-profile"].as_str().unwrap(), &key).unwrap();
        assert_eq!(access, JWT);
        assert_eq!(refresh, JWT);
        assert!(profile.contains("a@b.c"));
    }
}
