//! PAT 换成 job token，再用 job token 问用户是谁。
//!
//! 交换接口不需要 COSY 签名。聊天才要。`expires_in` 在交换接口上是毫秒；
//! 大得不像毫秒的才按秒解释。

use crate::model::QoderBackend;
use crate::protocol::{self, CLIENT_TYPE, OPENAPI_COSY_VERSION};
use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::{Duration, OffsetDateTime};

const UA: &str = "QoderCLI/1.1.38";

#[derive(Debug, Clone)]
pub struct Exchanged {
    pub job_token: String,
    pub job_refresh: String,
    pub expires_at: String,
    pub user_id: String,
    pub email: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
struct ExchangeBody {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct UserInfoBody {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    username: Option<String>,
}

pub async fn exchange_pat(
    http: &reqwest::Client,
    backend: QoderBackend,
    pat: &str,
) -> Result<Exchanged, String> {
    let res = http
        .post(protocol::exchange_url(backend))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, UA)
        .header("Cosy-Version", OPENAPI_COSY_VERSION)
        .header("Cosy-ClientType", CLIENT_TYPE)
        .json(&serde_json::json!({ "personal_token": pat }))
        .send()
        .await
        .map_err(|e| format!("连不上 Qoder：{e}"))?;
    let status = res.status().as_u16();
    let text = res.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("PAT 交换失败（{status}）：{}", clip(&text)));
    }
    let body: ExchangeBody =
        serde_json::from_str(&text).map_err(|e| format!("PAT 交换的响应不是预期的 JSON：{e}"))?;
    let job_token = body
        .token
        .filter(|t| !t.trim().is_empty())
        .ok_or_else(|| "PAT 交换没有返回 job token。".to_string())?;
    let mut exchanged = Exchanged {
        job_token,
        job_refresh: body.refresh_token.unwrap_or_default(),
        expires_at: format_expiry(body.expires_at.as_deref(), body.expires_in),
        user_id: String::new(),
        email: String::new(),
        name: String::new(),
    };
    if let Ok(info) = user_info(http, backend, &exchanged.job_token).await {
        exchanged.user_id = info.0;
        exchanged.email = info.1;
        exchanged.name = info.2;
    }
    Ok(exchanged)
}

async fn user_info(
    http: &reqwest::Client,
    backend: QoderBackend,
    job_token: &str,
) -> Result<(String, String, String), String> {
    let res = http
        .get(protocol::user_info_url(backend))
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {job_token}"),
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .header(reqwest::header::USER_AGENT, UA)
        .header("Cosy-Version", OPENAPI_COSY_VERSION)
        .header("Cosy-ClientType", CLIENT_TYPE)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !res.status().is_success() {
        return Err(format!("userinfo {}", res.status()));
    }
    let body: UserInfoBody = res.json().await.map_err(|e| e.to_string())?;
    Ok((
        body.id.unwrap_or_default(),
        body.email.unwrap_or_default(),
        body.name.or(body.username).unwrap_or_default(),
    ))
}

fn format_expiry(expires_at: Option<&str>, expires_in: Option<i64>) -> String {
    let when = if let Some(raw) = expires_at.map(str::trim).filter(|s| !s.is_empty()) {
        OffsetDateTime::parse(raw, &Rfc3339)
            .ok()
            .or_else(|| raw.parse::<i64>().ok().and_then(from_unix))
    } else {
        None
    };
    let when = when.unwrap_or_else(|| match expires_in {
        Some(n) if n > 0 => {
            let ms = if n > 1_000_000 {
                n
            } else {
                n.saturating_mul(1000)
            };
            OffsetDateTime::now_utc() + Duration::milliseconds(ms)
        }
        _ => OffsetDateTime::now_utc() + Duration::hours(24),
    });
    // 提前五分钟换票，别卡在过期那一秒。
    let when = when - Duration::minutes(5);
    when.format(&Rfc3339).unwrap_or_else(|_| String::new())
}

fn from_unix(n: i64) -> Option<OffsetDateTime> {
    if n > 1_000_000_000_000 {
        OffsetDateTime::from_unix_timestamp(n / 1000).ok()
    } else {
        OffsetDateTime::from_unix_timestamp(n).ok()
    }
}

pub fn expired(expires_at: Option<&str>) -> bool {
    let Some(raw) = expires_at.map(str::trim).filter(|s| !s.is_empty()) else {
        return true;
    };
    let Ok(when) = OffsetDateTime::parse(raw, &Rfc3339) else {
        return true;
    };
    OffsetDateTime::now_utc() >= when
}

fn clip(text: &str) -> String {
    text.chars().take(180).collect()
}
