//! Stream 凭证文件 `grokbot-stream-credential.json`：Sand 直连补丁与本机网关都读它。
//!
//! - `grokBotToken`：JWT（`type: grok_bot`），约 10 分钟；Stream 的 Bearer。
//! - `renewalCredential`：`sbi_*`，pod 生命周期内稳定；凭它 `POST /sand-box/inference-credential`
//!   就能拿新 token，**不需要任何鉴权**（2026-09-09 实测：无 Authorization、无 sand 头都 200）。
//! - `machineId`：Grok Bot 的 cursor-machine-id，拼 `x-cursor-checksum`。实测 checksum 用别的
//!   machineId 也放行，带上只是形态一致。
//!
//! 字段名是补丁注入体读的键，改了两边一起改。

use nexus_core::{AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const STREAM_CREDENTIAL_FILENAME: &str = "grokbot-stream-credential.json";
pub const RENEWAL_URL: &str = "https://api2.cursor.sh/sand-box/inference-credential";
pub const DEFAULT_CLIENT_VERSION: &str = "0.44.0";
pub const DEFAULT_NAMESPACE: &str = "prod";
/// 过期前多久就算「该续了」。补丁注入体用同一个数（12e4）。
pub const RENEW_LEEWAY_MS: u64 = 120_000;
/// renewal 响应没给 expiresAtMs 时的兜底 TTL。
const FALLBACK_TTL_MS: u64 = 600_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamCredential {
    pub grok_bot_token: String,
    pub machine_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewal_credential: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
    #[serde(default = "default_client_version")]
    pub client_version: String,
    #[serde(default = "default_namespace")]
    pub namespace: String,
    /// 下面几项只给界面 / 换号检测看，注入体不读。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_email: Option<String>,
    /// 生成时 Grok Bot 的活跃账号槽 id。与当前活跃槽不同 = Grok Bot 换过号，这份凭证在花旧号的额度。
    /// 从「我的账号」直接生成的凭证没有这一项（不绑 Grok Bot 客户端登着谁）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_slot: Option<String>,
    /// 凭证来自哪条路：`grokbot_app`（读 Grok Bot 客户端）/ `library`（账号库里的号自己换的）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CredentialSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minted_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renewed_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    GrokbotApp,
    Library,
}

fn default_client_version() -> String {
    DEFAULT_CLIENT_VERSION.into()
}

fn default_namespace() -> String {
    DEFAULT_NAMESPACE.into()
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(STREAM_CREDENTIAL_FILENAME)
}

impl StreamCredential {
    pub fn validate(&self) -> Result<()> {
        if self.grok_bot_token.split('.').count() != 3 {
            return Err(AppError::invalid("grokBotToken 不是 JWT。"));
        }
        if self.machine_id.trim().is_empty() {
            return Err(AppError::invalid("machineId 为空。"));
        }
        Ok(())
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

    /// 有效期：优先文件里的 `expiresAtMs`，没有就解 JWT `exp`。
    pub fn expires_at_ms(&self) -> Option<u64> {
        self.expires_at_ms
            .or_else(|| crate::secrets::jwt_exp_ms(&self.grok_bot_token))
    }

    pub fn is_expiring(&self, now_ms: u64) -> bool {
        match self.expires_at_ms() {
            Some(exp) => now_ms + RENEW_LEEWAY_MS >= exp,
            None => true,
        }
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_at_ms()
            .map(|exp| now_ms >= exp)
            .unwrap_or(true)
    }

    pub fn can_renew(&self) -> bool {
        self.renewal_credential
            .as_deref()
            .map(|s| s.starts_with("sbi_"))
            .unwrap_or(false)
    }

    /// 快过期且有种子就续；返回是否续了。
    pub async fn renew_if_needed(&mut self) -> Result<bool> {
        let now = now_ms();
        if !self.is_expiring(now) {
            return Ok(false);
        }
        let Some(sbi) = self.renewal_credential.clone() else {
            return Err(AppError::new(
                ErrorCode::Unauthorized,
                "grokBotToken 已过期且没有续期种子。",
            )
            .with_hint("在 Sand 页重新「生成直连凭证」。"));
        };
        let renewed = renew(&sbi).await?;
        self.grok_bot_token = renewed.grok_bot_token;
        self.expires_at_ms = Some(renewed.expires_at_ms);
        self.renewed_at_ms = Some(now);
        Ok(true)
    }
}

pub struct Renewed {
    pub grok_bot_token: String,
    pub access_token: String,
    pub expires_at_ms: u64,
}

#[derive(Deserialize)]
struct RenewalBody {
    #[serde(rename = "grokBotToken")]
    grok_bot_token: Option<String>,
    #[serde(rename = "accessToken")]
    access_token: Option<String>,
    #[serde(rename = "expiresAtMs")]
    expires_at_ms: Option<u64>,
}

/// `sbi_*` → 新 token。免鉴权。
pub async fn renew(renewal_credential: &str) -> Result<Renewed> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .no_proxy()
        .http2_keep_alive_interval(Duration::from_secs(10))
        .http2_keep_alive_timeout(Duration::from_secs(10))
        .http2_keep_alive_while_idle(true)
        .build()
        .map_err(|e| AppError::internal(format!("建 HTTP 客户端失败：{e}")))?;
    let resp = client
        .post(RENEWAL_URL)
        .header("content-type", "application/json")
        .json(&serde_json::json!({ "credential": renewal_credential }))
        .send()
        .await
        .map_err(|e| AppError::network(format!("续期请求失败：{e}")))?;
    let status = resp.status();
    let text = resp
        .text()
        .await
        .map_err(|e| AppError::network(format!("读续期响应失败：{e}")))?;
    if !status.is_success() {
        let code = if status.as_u16() == 401 {
            ErrorCode::Unauthorized
        } else {
            ErrorCode::Upstream
        };
        return Err(AppError::new(
            code,
            format!("续期 HTTP {status}：{}", &text[..text.len().min(160)]),
        )
        .with_hint("续期种子失效多半是 pod 重建了；在 Sand 页重新「生成直连凭证」。"));
    }
    let body: RenewalBody =
        serde_json::from_str(&text).map_err(|_| AppError::upstream("续期响应不是 JSON。"))?;
    let grok_bot_token = body
        .grok_bot_token
        .filter(|t| t.split('.').count() == 3)
        .ok_or_else(|| AppError::upstream("续期响应里没有 grokBotToken。"))?;
    Ok(Renewed {
        expires_at_ms: body
            .expires_at_ms
            .unwrap_or_else(|| now_ms() + FALLBACK_TTL_MS),
        access_token: body.access_token.unwrap_or_default(),
        grok_bot_token,
    })
}

/// `x-cursor-checksum`：与 `nexus-gateway::identity::checksum` 同一形态（含 JS 位移截断怪癖）。
pub fn checksum(machine_id: &str, now_ms: u64) -> String {
    use base64::Engine;
    let ts = (now_ms / 1_000_000) as u32;
    let b = [
        (ts >> 8) as u8,
        ts as u8,
        (ts >> 24) as u8,
        (ts >> 16) as u8,
        (ts >> 8) as u8,
        ts as u8,
    ];
    let mut out = Vec::with_capacity(6);
    let mut prev: u8 = 165;
    for (i, &x) in b.iter().enumerate() {
        let v = (x ^ prev).wrapping_add(i as u8);
        out.push(v);
        prev = v;
    }
    let prefix = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(out);
    format!("{prefix}{machine_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(exp: Option<u64>) -> StreamCredential {
        StreamCredential {
            grok_bot_token: "a.b.c".into(),
            machine_id: "m".into(),
            renewal_credential: Some("sbi_x".into()),
            expires_at_ms: exp,
            client_version: DEFAULT_CLIENT_VERSION.into(),
            namespace: DEFAULT_NAMESPACE.into(),
            account_email: None,
            account_slot: None,
            source: None,
            minted_at_ms: None,
            renewed_at_ms: None,
        }
    }

    #[test]
    fn expiring_window_uses_leeway() {
        let c = cred(Some(1_000_000));
        assert!(!c.is_expiring(1_000_000 - RENEW_LEEWAY_MS - 1));
        assert!(c.is_expiring(1_000_000 - RENEW_LEEWAY_MS));
        assert!(!c.is_expired(999_999));
        assert!(c.is_expired(1_000_000));
    }

    #[test]
    fn file_round_trip_keeps_runtime_keys() {
        let dir = tempfile::tempdir().unwrap();
        let c = cred(Some(5));
        c.save(dir.path()).unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path(dir.path())).unwrap()).unwrap();
        for k in [
            "grokBotToken",
            "machineId",
            "renewalCredential",
            "expiresAtMs",
            "clientVersion",
            "namespace",
        ] {
            assert!(raw.get(k).is_some(), "缺 {k}");
        }
        assert_eq!(StreamCredential::load(dir.path()).unwrap(), Some(c));
        StreamCredential::remove(dir.path()).unwrap();
        assert_eq!(StreamCredential::load(dir.path()).unwrap(), None);
    }

    #[test]
    fn checksum_matches_gateway_vector() {
        // 与 nexus-gateway identity.rs 的向量一致：Vfb45Bi9 + machine_id。
        assert_eq!(checksum("deadbeef", 1_700_000_000_000), "Vfb45Bi9deadbeef");
    }
}
