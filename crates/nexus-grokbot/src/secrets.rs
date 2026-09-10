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
}
