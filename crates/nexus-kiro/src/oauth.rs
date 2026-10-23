//! AWS Builder ID 的 OIDC：注册 client、device code、刷新。
//!
//! AWS SSO OIDC 用 JSON 体，不是 form。
//!
//! 协议事实靠真机探针钉住，不靠文档：
//! `cargo run -p nexus-gateway --example kiro_matrix -- status | usage | profiles`。

use nexus_core::{AppError, ErrorCode, Secret};
use serde_json::{json, Value};
use std::time::Duration;
use time::OffsetDateTime;

use crate::protocol::{CLIENT_NAME, OIDC_BASE, SCOPES, SOCIAL_REFRESH, START_URL};

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
const FATAL_CODES: &[&str] = &[
    "invalid_grant",
    "invalid_client",
    "unauthorized_client",
    "expired_token",
    "access_denied",
];

#[derive(Debug, Clone)]
pub struct RegisteredClient {
    pub client_id: String,
    pub client_secret: String,
}

#[derive(Debug, Clone)]
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
    pub client_id: Option<Secret>,
    pub client_secret: Option<Secret>,
    pub expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    pub subject: String,
    pub email: Option<String>,
    pub auth_method: Option<String>,
    /// 社交登录的 `profileArn`。Builder ID 留空。
    pub profile_arn: Option<String>,
}

pub struct TokenClient {
    http: reqwest::Client,
    oidc_base: String,
}

impl TokenClient {
    pub fn new(http: reqwest::Client) -> Self {
        Self {
            http,
            oidc_base: OIDC_BASE.to_string(),
        }
    }

    #[cfg(test)]
    pub fn with_oidc_base(http: reqwest::Client, oidc_base: String) -> Self {
        Self { http, oidc_base }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.oidc_base.trim_end_matches('/'), path)
    }

    pub async fn register(&self) -> Result<RegisteredClient, AppError> {
        let body = json!({
            "clientName": CLIENT_NAME,
            "clientType": "public",
            "grantTypes": [DEVICE_GRANT, "refresh_token"],
            "issuerUrl": self.oidc_base.trim_end_matches('/'),
            "scopes": SCOPES,
        });
        let res = self
            .http
            .post(self.url("/client/register"))
            .timeout(HTTP_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(net)?;
        let v = read_json(res).await?;
        let client_id = req_str(&v, "clientId")?;
        let client_secret = req_str(&v, "clientSecret")?;
        Ok(RegisteredClient {
            client_id: client_id.to_string(),
            client_secret: client_secret.to_string(),
        })
    }

    pub async fn start_device(&self, client: &RegisteredClient) -> Result<DeviceCode, AppError> {
        let body = json!({
            "clientId": client.client_id,
            "clientSecret": client.client_secret,
            "startUrl": START_URL,
        });
        let res = self
            .http
            .post(self.url("/device_authorization"))
            .timeout(HTTP_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(net)?;
        let v = read_json(res).await?;
        let device_code = req_str(&v, "deviceCode")?;
        let user_code = req_str(&v, "userCode")?;
        let verification_uri = v
            .get("verificationUri")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("https://device.sso.us-east-1.amazonaws.com/");
        let complete = v
            .get("verificationUriComplete")
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(verification_uri)
            .to_string();
        Ok(DeviceCode {
            device_code: device_code.to_string(),
            user_code: user_code.to_string(),
            verification_uri: verification_uri.to_string(),
            verification_uri_complete: complete,
            interval_secs: v
                .get("interval")
                .and_then(|x| x.as_u64())
                .unwrap_or(5)
                .max(1),
            expires_in_secs: v.get("expiresIn").and_then(|x| x.as_u64()).unwrap_or(600),
        })
    }

    pub async fn poll_once(
        &self,
        client: &RegisteredClient,
        device_code: &str,
    ) -> Result<Option<TokenSet>, AppError> {
        let body = json!({
            "clientId": client.client_id,
            "clientSecret": client.client_secret,
            "deviceCode": device_code,
            "grantType": DEVICE_GRANT,
        });
        let res = self
            .http
            .post(self.url("/token"))
            .timeout(HTTP_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(net)?;
        let status = res.status().as_u16();
        let v: Value = res.json().await.map_err(net)?;
        if (200..300).contains(&status) {
            return Ok(Some(tokens_from(
                &v,
                Some(&client.client_id),
                Some(&client.client_secret),
            )?));
        }
        let err = error_code(&v);
        match err.as_str() {
            "authorization_pending" | "AuthorizationPendingException" => Ok(None),
            "slow_down" | "SlowDownException" => Ok(None),
            other
                if FATAL_CODES.iter().any(|c| other.eq_ignore_ascii_case(c))
                    || other.contains("ExpiredToken")
                    || other.contains("AccessDenied") =>
            {
                Err(oauth_err(&v, true))
            }
            _ => Err(oauth_err(&v, false)),
        }
    }

    pub async fn refresh_oidc(
        &self,
        client_id: &str,
        client_secret: &str,
        refresh_token: &str,
    ) -> Result<TokenSet, AppError> {
        let body = json!({
            "clientId": client_id,
            "clientSecret": client_secret,
            "refreshToken": refresh_token,
            "grantType": "refresh_token",
        });
        let res = self
            .http
            .post(self.url("/token"))
            .timeout(HTTP_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(net)?;
        let status = res.status().as_u16();
        let v: Value = res.json().await.map_err(net)?;
        if (200..300).contains(&status) {
            return tokens_from(&v, Some(client_id), Some(client_secret));
        }
        let err = error_code(&v);
        Err(oauth_err(
            &v,
            FATAL_CODES.iter().any(|c| err.eq_ignore_ascii_case(c)),
        ))
    }

    /// Kiro 桌面社交登录通过授权码换票。
    pub async fn exchange_social_code(
        &self,
        code: &str,
        code_verifier: &str,
    ) -> Result<(TokenSet, Option<String>), AppError> {
        let body = json!({
            "code": code,
            "code_verifier": code_verifier,
            "redirect_uri": crate::protocol::SOCIAL_REDIRECT_URI,
        });
        let res = self
            .http
            .post(format!("{}/oauth/token", crate::protocol::SOCIAL_AUTH_BASE))
            .timeout(HTTP_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(net)?;
        let status = res.status().as_u16();
        let v: Value = res.json().await.map_err(net)?;
        if !(200..300).contains(&status) {
            return Err(oauth_err(&v, status == 401 || status == 403));
        }
        let profile_arn = v.get("profileArn").and_then(|x| x.as_str()).map(str::to_string);
        let tokens = tokens_from(&v, None, None)?;
        Ok((tokens, profile_arn))
    }

    /// Kiro 桌面社交登录没有 SSO client 对，刷新走桌面端点。
    pub async fn refresh_social(&self, refresh_token: &str) -> Result<TokenSet, AppError> {
        let body = json!({ "refreshToken": refresh_token });
        let res = self
            .http
            .post(SOCIAL_REFRESH)
            .timeout(HTTP_TIMEOUT)
            .json(&body)
            .send()
            .await
            .map_err(net)?;
        let status = res.status().as_u16();
        let v: Value = res.json().await.map_err(net)?;
        if (200..300).contains(&status) {
            return tokens_from(&v, None, None);
        }
        Err(oauth_err(&v, status == 401 || status == 403))
    }

    /// Enterprise / IdC 的真实 profileArn 不在刷新响应里，只能问 `ListAvailableProfiles`。
    /// 成功但列表为空返回 `Ok(None)`（纯 Builder ID）。两端点都失败才是 `Err`。
    pub async fn list_profiles(
        &self,
        access_token: &str,
        token_type: Option<&str>,
    ) -> Result<Option<String>, AppError> {
        let mut saw_empty = false;
        let mut last = None;
        for region in ["us-east-1", "eu-central-1"] {
            let host = format!("q.{region}.amazonaws.com");
            let mut req = self
                .http
                .post(format!("https://{host}/"))
                .timeout(HTTP_TIMEOUT)
                .header("content-type", "application/x-amz-json-1.0")
                .header(
                    "x-amz-target",
                    "AmazonCodeWhispererService.ListAvailableProfiles",
                )
                .header(
                    "user-agent",
                    "aws-sdk-js/1.0.0 KiroIDE-0.11.132 ua/2.1 api/codewhispererruntime#1.0.0 m/N,E",
                )
                .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
                .header("amz-sdk-request", "attempt=1; max=1")
                .header("authorization", format!("Bearer {access_token}"))
                .body(r#"{"maxResults":10}"#);
            if let Some(token_type) = token_type.map(str::trim).filter(|s| !s.is_empty()) {
                req = req.header("tokentype", token_type);
            }
            let res = match req.send().await {
                Ok(res) => res,
                Err(err) => {
                    last = Some(err.to_string());
                    continue;
                }
            };
            let status = res.status().as_u16();
            if !(200..300).contains(&status) {
                let text = res.text().await.unwrap_or_default();
                last = Some(format!("{status} {text}"));
                continue;
            }
            let v: Value = res.json().await.map_err(net)?;
            let arn = v
                .get("profiles")
                .and_then(Value::as_array)
                .and_then(|profiles| {
                    profiles.iter().find_map(|profile| {
                        profile
                            .get("arn")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                    })
                });
            if let Some(arn) = arn {
                return Ok(Some(arn));
            }
            saw_empty = true;
        }
        if saw_empty {
            return Ok(None);
        }
        Err(AppError::upstream(
            last.unwrap_or_else(|| "获取可用 profile 失败".to_string()),
        ))
    }

    /// `GET /getUsageLimits`。真实 profileArn 先带上试，400/403 再退回不带。
    /// Builder ID 的占位 ARN 不送。401 直接失败，不再换区域。
    pub async fn usage_limits(
        &self,
        access_token: &str,
        token_type: Option<&str>,
        profile_arn: Option<&str>,
    ) -> Result<crate::usage::KiroUsage, AppError> {
        let arn = profile_arn
            .map(str::trim)
            .filter(|s| s.starts_with("arn:") && !crate::protocol::is_placeholder_profile_arn(s));
        let mut last = String::from("获取 Kiro 用量失败");
        for region in ["us-east-1", "eu-central-1"] {
            let passes: Vec<Option<&str>> = if arn.is_some() {
                vec![arn, None]
            } else {
                vec![None]
            };
            for profile in passes {
                match self
                    .usage_once(region, access_token, token_type, profile)
                    .await
                {
                    Ok(body) => {
                        return crate::usage::parse_usage(&body).map_err(AppError::upstream)
                    }
                    Err(err) if err.code == ErrorCode::Unauthorized => return Err(err),
                    Err(err) => {
                        let retry_without = profile.is_some()
                            && (err.message.contains("403")
                                || err.message.contains("400")
                                || err.message.contains("Improperly formed request")
                                || err.message.contains("Invalid profileArn"));
                        last = err.message;
                        if retry_without {
                            continue;
                        }
                        break;
                    }
                }
            }
        }
        Err(AppError::upstream(last))
    }

    async fn usage_once(
        &self,
        region: &str,
        access_token: &str,
        token_type: Option<&str>,
        profile_arn: Option<&str>,
    ) -> Result<Value, AppError> {
        let host = format!("q.{region}.amazonaws.com");
        let mut url = format!(
            "https://{host}/getUsageLimits?origin=AI_EDITOR&resourceType=AGENTIC_REQUEST&isEmailRequired=true"
        );
        if let Some(arn) = profile_arn {
            url.push_str("&profileArn=");
            url.push_str(&query_escape(arn));
        }
        let mut req = self
            .http
            .get(url)
            .timeout(HTTP_TIMEOUT)
            .header(
                "user-agent",
                "aws-sdk-js/1.0.0 KiroIDE-0.6.18 ua/2.1 api/codewhispererruntime#1.0.0 m/N,E",
            )
            .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
            .header("amz-sdk-request", "attempt=1; max=1")
            .header("authorization", format!("Bearer {access_token}"));
        if let Some(token_type) = token_type.map(str::trim).filter(|s| !s.is_empty()) {
            req = req.header("tokentype", token_type);
        }
        let res = req.send().await.map_err(net)?;
        let status = res.status().as_u16();
        if (200..300).contains(&status) {
            return res.json().await.map_err(net);
        }
        let text = res.text().await.unwrap_or_default();
        let head: String = text.chars().take(240).collect();
        if status == 401 {
            return Err(AppError::new(
                ErrorCode::Unauthorized,
                format!("Kiro 登录失效：{head}"),
            ));
        }
        Err(AppError::upstream(format!("{status} {head}")))
    }
}

fn query_escape(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn tokens_from(
    body: &Value,
    client_id: Option<&str>,
    client_secret: Option<&str>,
) -> Result<TokenSet, AppError> {
    let access = pick(body, &["accessToken", "access_token"])
        .ok_or_else(|| AppError::upstream("Kiro token 响应没有 accessToken"))?;
    let refresh = pick(body, &["refreshToken", "refresh_token"]);
    let expires_at = expires_from(body, &access);
    Ok(TokenSet {
        access_token: Secret::new(access),
        refresh_token: refresh.map(Secret::new),
        client_id: client_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| pick(body, &["clientId", "client_id"]))
            .map(Secret::new),
        client_secret: client_secret
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| pick(body, &["clientSecret", "client_secret"]))
            .map(Secret::new),
        expires_at,
    })
}

fn expires_from(body: &Value, access: &str) -> Option<OffsetDateTime> {
    if let Some(s) = pick(body, &["expiresAt", "expires_at"]) {
        if let Ok(t) = OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339) {
            return Some(t);
        }
    }
    let expires_in = body
        .get("expiresIn")
        .or_else(|| body.get("expires_in"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if expires_in > 0 {
        return Some(OffsetDateTime::now_utc() + time::Duration::seconds(expires_in));
    }
    jwt_expiry(access)
}

/// 这个 `account_ref` 指不指得到一个人。
///
/// profileArn 不行：社交登录所有 Google / GitHub 号共用一个，IdC 是一个组织一个，Builder ID
/// 的占位符人人都一样——拿它当身份，导第二个号就会把第一个的凭证盖掉。`client:` 是某次
/// 设备授权注册出来的 client，重新授权就换一个。这两种要等问到 `userInfo.userId` 再改记。
pub fn names_one_user(account_ref: &str) -> bool {
    let r = account_ref.trim();
    !r.is_empty() && !r.starts_with("arn:") && !r.starts_with("client:")
}

pub fn identity_from_tokens(tokens: &TokenSet, fallback_ref: Option<&str>) -> Identity {
    let mut id = Identity::default();
    if let Some(claims) = decode_jwt_claims(tokens.access_token.expose()) {
        if let Some(sub) = claims
            .get("sub")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            id.subject = sub.to_string();
        }
        id.email = claims
            .get("email")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase);
    }
    if id.subject.is_empty() {
        if let Some(r) = fallback_ref.map(str::trim).filter(|s| !s.is_empty()) {
            id.subject = r.to_string();
        }
    }
    if id.subject.is_empty() {
        if let Some(cid) = tokens.client_id.as_ref() {
            id.subject = format!("client:{}", cid.expose());
        }
    }
    if let Some(arn) = fallback_ref
        .map(str::trim)
        .filter(|s| s.starts_with("arn:"))
    {
        id.profile_arn = Some(arn.to_string());
    }
    id
}

pub fn decode_jwt_claims(jwt: &str) -> Option<Value> {
    use base64::Engine;
    let mut parts = jwt.trim().split('.');
    let (_h, payload, _s) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
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

fn pick(body: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        body.get(*k)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    })
}

fn req_str<'a>(body: &'a Value, key: &str) -> Result<&'a str, AppError> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::upstream(format!("Kiro 响应缺少 {key}")))
}

fn error_code(body: &Value) -> String {
    body.get("error")
        .or_else(|| body.get("__type"))
        .or_else(|| body.get("errorCode"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

async fn read_json(res: reqwest::Response) -> Result<Value, AppError> {
    let status = res.status().as_u16();
    let body: Value = res.json().await.map_err(net)?;
    if !(200..300).contains(&status) {
        return Err(oauth_err(&body, false));
    }
    Ok(body)
}

fn oauth_err(body: &Value, fatal: bool) -> AppError {
    let msg = body
        .get("error_description")
        .or_else(|| body.get("message"))
        .or_else(|| body.get("error"))
        .or_else(|| body.get("__type"))
        .and_then(|v| v.as_str())
        .unwrap_or("Kiro OAuth 失败");
    AppError::new(
        if fatal {
            ErrorCode::Unauthorized
        } else {
            ErrorCode::Upstream
        },
        msg.to_string(),
    )
}

fn net(err: reqwest::Error) -> AppError {
    AppError::new(ErrorCode::Network, format!("连 AWS / Kiro 失败：{err}"))
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
    fn identity_from_jwt_sub() {
        let access = jwt(r#"{"sub":"user/alice","email":"A@AMAZON.COM"}"#);
        let tokens = TokenSet {
            access_token: Secret::new(access),
            refresh_token: None,
            client_id: None,
            client_secret: None,
            expires_at: None,
        };
        let id = identity_from_tokens(&tokens, None);
        assert_eq!(id.subject, "user/alice");
        assert_eq!(id.email.as_deref(), Some("a@amazon.com"));
    }
}
