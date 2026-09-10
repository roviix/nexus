//! `gateway-descriptor.json`：Grok Bot 桌面端连自己 Box（pod 网关 :1340）用的地址与 token。
//!
//! 按账号作用域（`sha256(sub)`）索引，每个条目是 safeStorage 加密的 JSON
//! `{ baseUrl, token, headers }`。Box Relay 模式把 Stream 改道到 `baseUrl + BOX_RELAY_PATH`，
//! 直连模式借它到 pod 里读续期种子（exec daemon 就是同一台 pod 的 :1337）。

use crate::app::user_data_dir;
use crate::keychain;
use crate::secrets::GrokBotSecrets;
use nexus_core::{AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const BOX_RELAY_PATH: &str = "/sand-stream-relay/aiserver.v1.InferenceService/Stream";
/// Grok Bot 网关端口；exec daemon 是同一台 pod 上的 1337。
const GATEWAY_PORT_SEGMENT: &str = "-1340.";
const EXEC_PORT_SEGMENT: &str = "-1337.";

/// v135 脚本写进 `grok-box-relay.json` 的形状（补丁 runtime 读它）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BoxRelayDescriptor {
    #[serde(default = "one")]
    pub version: u32,
    pub base_url: String,
    pub token: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_relay_path")]
    pub relay_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_fingerprint: Option<String>,
}

fn one() -> u32 {
    1
}

fn default_relay_path() -> String {
    BOX_RELAY_PATH.into()
}

impl BoxRelayDescriptor {
    pub fn validate(&self) -> Result<()> {
        if !self.base_url.starts_with("https://") {
            return Err(AppError::invalid("Box gateway 地址必须是 https://。"));
        }
        if self.token.trim().is_empty() {
            return Err(AppError::invalid("Box gateway token 为空。"));
        }
        Ok(())
    }

    /// exec daemon 地址：同 pod，端口段 1340 → 1337。
    pub fn exec_daemon_url(&self) -> Option<String> {
        if self.base_url.contains(GATEWAY_PORT_SEGMENT) {
            Some(
                self.base_url
                    .replacen(GATEWAY_PORT_SEGMENT, EXEC_PORT_SEGMENT, 1)
                    .trim_end_matches('/')
                    .to_string(),
            )
        } else {
            None
        }
    }

    /// pod 侧统一要求的网络 token 头。
    pub fn network_token(&self) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("x-anyrun-network-token"))
            .map(|(_, v)| v.as_str())
    }

    pub fn relay_url(&self) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), self.relay_path)
    }
}

#[derive(Deserialize)]
struct DescriptorFile {
    #[serde(default)]
    entries: BTreeMap<String, DescriptorEntry>,
}

#[derive(Deserialize)]
struct DescriptorEntry {
    encrypted: Option<String>,
}

#[derive(Deserialize)]
struct Connection {
    #[serde(rename = "baseUrl")]
    base_url: Option<String>,
    token: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, serde_json::Value>,
}

pub fn descriptor_path() -> std::path::PathBuf {
    user_data_dir().join("gateway-descriptor.json")
}

/// 读当前活跃账号的 descriptor。
pub fn load(secrets: &GrokBotSecrets) -> Result<BoxRelayDescriptor> {
    let raw = std::fs::read_to_string(descriptor_path()).map_err(|_| {
        AppError::new(
            ErrorCode::CursorNotFound,
            "没找到 Grok Bot 的 gateway-descriptor.json。",
        )
        .with_hint("打开 Grok Bot 并等它连上 Box（左下角状态变绿）后重试。")
    })?;
    let file: DescriptorFile = serde_json::from_str(&raw)?;
    let scope = crate::secrets::account_scope(&secrets.session_token);
    let entry = file
        .entries
        .get(&scope)
        .and_then(|e| e.encrypted.as_deref())
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::NotLoggedIn,
                "gateway-descriptor 里没有当前 Grok Bot 账号的条目。",
            )
            .with_hint("Grok Bot 可能刚换了账号还没连上 Box；等它连上再刷新。")
        })?;
    let password = keychain::safe_storage_password()?;
    let key = keychain::derive_key(&password);
    let conn: Connection = serde_json::from_str(&keychain::decrypt(entry, &key)?)?;
    let base_url = conn
        .base_url
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::internal("descriptor 缺 baseUrl。"))?;
    let token = conn
        .token
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::internal("descriptor 缺 token。"))?;
    let headers = conn
        .headers
        .into_iter()
        .filter_map(|(k, v)| {
            v.as_str()
                .filter(|s| !s.is_empty())
                .map(|s| (k, s.to_string()))
        })
        .collect();
    let fingerprint = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(scope.as_bytes());
        h.finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()[..16]
            .to_string()
    };
    let d = BoxRelayDescriptor {
        version: 1,
        base_url,
        token,
        headers,
        relay_path: BOX_RELAY_PATH.into(),
        account_fingerprint: Some(fingerprint),
    };
    d.validate()?;
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BoxRelayDescriptor {
        BoxRelayDescriptor {
            version: 1,
            base_url: "https://abc-pod-xyz-1340.us11.cursorvm.com".into(),
            token: "t".into(),
            headers: [("x-anyrun-network-token".to_string(), "nto-1".to_string())].into(),
            relay_path: BOX_RELAY_PATH.into(),
            account_fingerprint: None,
        }
    }

    #[test]
    fn exec_daemon_url_swaps_the_port_segment() {
        assert_eq!(
            sample().exec_daemon_url().as_deref(),
            Some("https://abc-pod-xyz-1337.us11.cursorvm.com")
        );
        let mut d = sample();
        d.base_url = "https://other.example".into();
        assert_eq!(d.exec_daemon_url(), None);
    }

    #[test]
    fn relay_url_and_network_token() {
        let d = sample();
        assert_eq!(
            d.relay_url(),
            "https://abc-pod-xyz-1340.us11.cursorvm.com/sand-stream-relay/aiserver.v1.InferenceService/Stream"
        );
        assert_eq!(d.network_token(), Some("nto-1"));
    }

    #[test]
    fn json_shape_matches_v135_file() {
        let d = sample();
        let v: serde_json::Value = serde_json::to_value(&d).unwrap();
        assert_eq!(v["baseUrl"], d.base_url);
        assert_eq!(v["relayPath"], BOX_RELAY_PATH);
        assert!(v.get("accountFingerprint").is_none());
    }
}
