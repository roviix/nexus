//! CRSR 凭证文件 `crsr-agent-credential.json`：补丁注入体与本机安装器都读它。
//!
//! - `apiKey`：长期 `crsr_…`。补丁快过期时自己拿它兑票（不经过 Nexus 进程）。
//! - `accessToken`：兑出来的短期 JWT（`type=api_key_token`，大约一小时）。
//! - 文件权限 0o600。IPC 只回 [`CrsrCredentialInfo`]，不含任何秘密。

use nexus_accounts::token::{self, looks_like_user_api_key};
use nexus_core::{AccountId, AppError, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

pub const CREDENTIAL_FILENAME: &str = "crsr-agent-credential.json";
/// 覆盖凭证文件位置。安装器与注入体都读它，见 [`path`]。
pub const CREDENTIAL_FILE_ENV: &str = "NEXUS_CRSR_CREDENTIAL_FILE";
pub const RENEW_LEEWAY_MS: u64 = 120_000;
const FALLBACK_TTL_MS: u64 = 3_600_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrsrCredential {
    pub api_key: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minted_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewed_at_ms: Option<u64>,
}

/// 给界面看的、不含秘密的摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrsrCredentialInfo {
    pub account_email: Option<String>,
    pub account_id: Option<String>,
    pub expires_at_ms: Option<u64>,
    pub expired: bool,
    pub can_renew: bool,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 凭证文件在哪。
///
/// `NEXUS_CRSR_CREDENTIAL_FILE` 优先——**注入体也认同一个变量**（`inject::auth_block`）。
/// 两边必须用同一套规则算路径，否则安装器写一处、补丁读另一处，界面显示「已选好号」而 IDE 一直
/// 报 `CRSR_AUTH_NOT_SELECTED`。要让它真正生效，变量得设在两个进程都看得见的地方
/// （macOS `launchctl setenv`、Windows 用户级环境变量），只在终端里 export 是不够的。
pub fn path(data_dir: &Path) -> PathBuf {
    resolve_path(std::env::var_os(CREDENTIAL_FILE_ENV).as_deref(), data_dir)
}

/// [`path`] 的纯函数内核。单独拎出来是为了能测——直接在测试里改进程环境变量会和并行跑的
/// 其它测试抢同一份全局状态。
pub fn resolve_path(override_value: Option<&OsStr>, data_dir: &Path) -> PathBuf {
    match override_value {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => data_dir.join(CREDENTIAL_FILENAME),
    }
}

impl CrsrCredential {
    pub fn validate(&self) -> Result<()> {
        if !looks_like_user_api_key(&self.api_key) {
            return Err(AppError::invalid("apiKey 不是 crsr_ API Key。").with_hint("形如 crsr_…"));
        }
        if self.access_token.split('.').count() != 3 {
            return Err(AppError::invalid("accessToken 不是 JWT。"));
        }
        Ok(())
    }

    pub fn is_expired(&self, now: u64) -> bool {
        match self.expires_at_ms {
            Some(exp) => now + RENEW_LEEWAY_MS >= exp,
            None => true,
        }
    }

    pub fn info(&self) -> CrsrCredentialInfo {
        let now = now_ms();
        CrsrCredentialInfo {
            account_email: self.account_email.clone(),
            account_id: self.account_id.clone(),
            expires_at_ms: self.expires_at_ms,
            expired: self.is_expired(now),
            can_renew: looks_like_user_api_key(&self.api_key),
        }
    }

    pub fn load(data_dir: &Path) -> Result<Option<Self>> {
        let p = path(data_dir);
        if !p.is_file() {
            return Ok(None);
        }
        let raw = std::fs::read_to_string(&p)?;
        let c: Self = serde_json::from_str(&raw)?;
        Ok(Some(c))
    }

    pub fn save(&self, data_dir: &Path) -> Result<PathBuf> {
        self.validate()?;
        let p = path(data_dir);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&p, format!("{text}\n"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
        Ok(p)
    }

    pub fn remove(data_dir: &Path) -> Result<()> {
        let p = path(data_dir);
        if p.is_file() {
            std::fs::remove_file(p)?;
        }
        Ok(())
    }
}

pub fn expires_at_ms_of(access_token: &str) -> u64 {
    token::jwt_expiry_iso(access_token)
        .and_then(|iso| nexus_core::clock::parse_iso(&iso))
        .map(|t| (t.unix_timestamp().max(0) as u64).saturating_mul(1000))
        .unwrap_or_else(|| now_ms() + FALLBACK_TTL_MS)
}

pub async fn mint(
    http: &reqwest::Client,
    api_key: &str,
    email: &str,
    id: &AccountId,
) -> Result<CrsrCredential> {
    let access = token::exchange_api_key(http, api_key).await?;
    let now = now_ms();
    Ok(CrsrCredential {
        api_key: api_key.trim().to_string(),
        expires_at_ms: Some(expires_at_ms_of(&access)),
        access_token: access,
        account_email: Some(email.to_string()),
        account_id: Some(id.as_str().to_string()),
        minted_at_ms: Some(now),
        renewed_at_ms: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_core::AccountId;

    fn jwt() -> String {
        use base64::Engine;
        let enc = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        format!(
            "{}.{}.sig",
            enc(b"{\"alg\":\"none\"}"),
            enc(br#"{"exp":4102444800}"#)
        )
    }

    /// 安装器和注入体必须算出同一个路径，否则界面显示「已选好号」而 IDE 一直报
    /// `CRSR_AUTH_NOT_SELECTED`——这是最难查的那种不一致，两边看着都对。
    #[test]
    fn the_env_override_wins_over_the_data_dir() {
        let data_dir = Path::new("/data");
        assert_eq!(
            resolve_path(None, data_dir),
            data_dir.join(CREDENTIAL_FILENAME)
        );
        // 空字符串当没设：`export NEXUS_CRSR_CREDENTIAL_FILE=` 不该把路径变成当前目录。
        assert_eq!(
            resolve_path(Some(OsStr::new("")), data_dir),
            data_dir.join(CREDENTIAL_FILENAME)
        );
        assert_eq!(
            resolve_path(Some(OsStr::new("/tmp/elsewhere.json")), data_dir),
            PathBuf::from("/tmp/elsewhere.json")
        );
        // 注入体读的是同一个变量名；改一边就得改另一边。
        assert!(crate::inject::auth_block().contains(CREDENTIAL_FILE_ENV));
    }

    #[test]
    fn save_load_roundtrip_and_info_hides_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let c = CrsrCredential {
            api_key: "crsr_abc123DEF".into(),
            access_token: jwt(),
            expires_at_ms: Some(now_ms() + 3_600_000),
            account_email: Some("a@example.com".into()),
            account_id: Some(AccountId::new().as_str().to_string()),
            minted_at_ms: Some(now_ms()),
            renewed_at_ms: None,
        };
        c.save(dir.path()).unwrap();
        let loaded = CrsrCredential::load(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.api_key, "crsr_abc123DEF");
        let info = loaded.info();
        let v = serde_json::to_value(&info).unwrap();
        assert!(v.get("apiKey").is_none());
        assert!(v.get("accessToken").is_none());
        assert_eq!(info.account_email.as_deref(), Some("a@example.com"));
        assert!(!info.expired);
        assert!(info.can_renew);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path(dir.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    #[test]
    fn rejects_a_non_crsr_key() {
        let c = CrsrCredential {
            api_key: "not-a-key".into(),
            access_token: jwt(),
            expires_at_ms: None,
            account_email: None,
            account_id: None,
            minted_at_ms: None,
            renewed_at_ms: None,
        };
        assert!(c.validate().is_err());
    }
}
