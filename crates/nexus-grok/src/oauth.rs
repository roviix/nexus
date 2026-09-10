//! Grok CLI 的 OAuth：OIDC discovery + device code + 刷新。
//!
//! 走 Grok CLI 公开 client（`b1a00492-…`）。协议事实见 `docs/relay/GROK-BUILD.md`。

use nexus_core::{AppError, ErrorCode, Secret};
use serde::Serialize;
use std::time::Duration;
use time::OffsetDateTime;

pub const CLIENT_ID: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub const ISSUER: &str = "https://auth.x.ai";
pub const DISCOVERY_URL: &str = "https://auth.x.ai/.well-known/openid-configuration";
pub const SCOPE: &str = "openid profile email offline_access grok-cli:access api:access";
pub const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const FATAL_CODES: &[&str] = &["invalid_grant", "invalid_client", "unauthorized_client"];

#[derive(Debug, Clone)]
pub struct Discovery {
    pub device_authorization_endpoint: String,
    pub token_endpoint: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub interval_secs: u64,
    pub expires_in_secs: u64,
}

#[derive(Debug, Clone)]
pub struct TokenSet {
    pub access_token: Secret,
    pub refresh_token: Option<Secret>,
    pub id_token: Option<Secret>,
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub subject: String,
    pub email: Option<String>,
}

pub struct TokenClient {
    http: reqwest::Client,
    discovery_url: String,
}

impl TokenClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            discovery_url: DISCOVERY_URL.to_string(),
        }
    }

    #[cfg(test)]
    pub fn with_discovery(http: reqwest::Client, discovery_url: String) -> Self {
        Self {
            http,
            discovery_url,
        }
    }

    pub async fn discover(&self) -> Result<Discovery, AppError> {
        let res = self
            .http
            .get(&self.discovery_url)
            .timeout(HTTP_TIMEOUT)
            .send()
            .await
            .map_err(net)?;
        let body: serde_json::Value = read_json(res).await?;
        let device = body
            .get("device_authorization_endpoint")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                AppError::upstream("xAI OIDC discovery 没有 device_authorization_endpoint")
            })?;
        let token = body
            .get("token_endpoint")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| AppError::upstream("xAI OIDC discovery 没有 token_endpoint"))?;
        Ok(Discovery {
            device_authorization_endpoint: device.to_string(),
            token_endpoint: token.to_string(),
        })
    }

    pub async fn start_device(&self, device_endpoint: &str) -> Result<DeviceCode, AppError> {
        let form = [("client_id", CLIENT_ID), ("scope", SCOPE)];
        let res = self
            .http
            .post(device_endpoint)
            .timeout(HTTP_TIMEOUT)
            .header("accept", "application/json")
            .form(&form)
            .send()
            .await
            .map_err(net)?;
        let body: serde_json::Value = read_json(res).await?;
        let device_code = req_str(&body, "device_code")?;
        let user_code = req_str(&body, "user_code")?;
        let verification_uri = req_str(&body, "verification_uri")?;
        let complete = body
            .get("verification_uri_complete")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(verification_uri)
            .to_string();
        let interval = body
            .get("interval")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .max(1);
        let expires = body
            .get("expires_in")
            .and_then(|v| v.as_u64())
            .unwrap_or(900);
        Ok(DeviceCode {
            device_code: device_code.to_string(),
            user_code: user_code.to_string(),
            verification_uri: verification_uri.to_string(),
            verification_uri_complete: complete,
            interval_secs: interval,
            expires_in_secs: expires,
        })
    }

    /// 轮询一次。`Ok(None)` = 还在等用户；`Err` 里 `authorization_pending` / `slow_down` 不会走到这里。
    pub async fn poll_once(
        &self,
        token_endpoint: &str,
        device_code: &str,
    ) -> Result<Option<TokenSet>, AppError> {
        let form = [
            ("grant_type", DEVICE_GRANT),
            ("device_code", device_code),
            ("client_id", CLIENT_ID),
        ];
        let res = self
            .http
            .post(token_endpoint)
            .timeout(HTTP_TIMEOUT)
            .header("accept", "application/json")
            .form(&form)
            .send()
            .await
            .map_err(net)?;
        let status = res.status().as_u16();
        let body: serde_json::Value = res.json().await.map_err(net)?;
        if status == 200 {
            return Ok(Some(tokens_from(&body)?));
        }
        let err = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
        match err {
            "authorization_pending" => Ok(None),
            "slow_down" => Ok(None),
            "expired_token" | "access_denied" => Err(oauth_err(&body, true)),
            _ => Err(oauth_err(&body, FATAL_CODES.contains(&err))),
        }
    }

    pub async fn refresh(
        &self,
        token_endpoint: &str,
        refresh_token: &str,
    ) -> Result<TokenSet, AppError> {
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ];
        let res = self
            .http
            .post(token_endpoint)
            .timeout(HTTP_TIMEOUT)
            .header("accept", "application/json")
            .form(&form)
            .send()
            .await
            .map_err(net)?;
        let status = res.status().as_u16();
        let body: serde_json::Value = res.json().await.map_err(net)?;
        if status == 200 {
            return tokens_from(&body);
        }
        let err = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
        Err(oauth_err(&body, FATAL_CODES.contains(&err)))
    }
}

fn tokens_from(body: &serde_json::Value) -> Result<TokenSet, AppError> {
    let access = body
        .get("access_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::upstream("xAI token 响应没有 access_token"))?;
    let refresh = body
        .get("refresh_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Secret::new(s.to_string()));
    let id_token = body
        .get("id_token")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| Secret::new(s.to_string()));
    let expires_in = body.get("expires_in").and_then(|v| v.as_i64()).unwrap_or(0);
    let expires_at = if expires_in > 0 {
        Some(OffsetDateTime::now_utc() + time::Duration::seconds(expires_in))
    } else {
        jwt_expiry(access)
    };
    Ok(TokenSet {
        access_token: Secret::new(access.to_string()),
        refresh_token: refresh,
        id_token,
        expires_at,
    })
}

pub fn identity_from_tokens(tokens: &TokenSet) -> Identity {
    let mut id = Identity::default();
    for raw in [
        tokens.id_token.as_ref().map(|s| s.expose()),
        Some(tokens.access_token.expose()),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(claims) = decode_jwt_claims(raw) {
            if id.subject.is_empty() {
                if let Some(sub) = claims
                    .get("sub")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                {
                    id.subject = sub.to_string();
                }
            }
            if id.email.is_none() {
                id.email = claims
                    .get("email")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_ascii_lowercase);
            }
        }
    }
    id
}

pub fn decode_jwt_claims(jwt: &str) -> Option<serde_json::Value> {
    use base64::Engine;
    let mut parts = jwt.trim().split('.');
    let (_h, payload, _s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.is_object().then_some(v)
}

pub fn looks_like_jwt(value: &str) -> bool {
    let v = value.trim();
    let parts: Vec<&str> = v.split('.').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        })
        && decode_jwt_claims(v).is_some()
}

pub fn jwt_expiry(token: &str) -> Option<OffsetDateTime> {
    let exp = decode_jwt_claims(token)?
        .get("exp")
        .and_then(|v| v.as_i64())?;
    OffsetDateTime::from_unix_timestamp(exp).ok()
}

fn req_str<'a>(body: &'a serde_json::Value, key: &str) -> Result<&'a str, AppError> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::upstream(format!("xAI 响应缺少 {key}")))
}

async fn read_json(res: reqwest::Response) -> Result<serde_json::Value, AppError> {
    let status = res.status().as_u16();
    let body: serde_json::Value = res.json().await.map_err(net)?;
    if !(200..300).contains(&status) {
        return Err(oauth_err(&body, false));
    }
    Ok(body)
}

fn oauth_err(body: &serde_json::Value, fatal: bool) -> AppError {
    let msg = body
        .get("error_description")
        .and_then(|v| v.as_str())
        .or_else(|| body.get("error").and_then(|v| v.as_str()))
        .unwrap_or("xAI OAuth 失败");
    let code = if fatal {
        ErrorCode::Unauthorized
    } else {
        ErrorCode::Upstream
    };
    AppError::new(code, msg.to_string())
}

fn net(err: reqwest::Error) -> AppError {
    AppError::new(ErrorCode::Network, format!("连 xAI 失败：{err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: &str) -> String {
        use base64::Engine;
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes());
        format!("{}.{}.sig", b64("{}"), b64(payload))
    }

    #[test]
    fn identity_prefers_id_token_email_and_sub() {
        let id = jwt(r#"{"sub":"user_1","email":"A@X.AI"}"#);
        let access = jwt(r#"{"sub":"user_1"}"#);
        let tokens = TokenSet {
            access_token: Secret::new(access),
            refresh_token: None,
            id_token: Some(Secret::new(id)),
            expires_at: None,
        };
        let ident = identity_from_tokens(&tokens);
        assert_eq!(ident.subject, "user_1");
        assert_eq!(ident.email.as_deref(), Some("a@x.ai"));
    }
}
