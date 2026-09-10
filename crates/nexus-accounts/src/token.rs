//! refresh_token → session token。
//!
//! `POST api2.cursor.sh/oauth/token`（grant_type=refresh_token），拿到 access_token 后
//! 拼成 `user_xxx::<accessToken>` —— 那正是 Cursor 存在 `WorkosCursorSessionToken`
//! Cookie 里的值，dashboard 接口认它。
//!
//! client_id 是 Cursor 内置的桌面端 id，不是按账号分的。

use nexus_core::{AppError, Result, Secret};
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

const TOKEN_URL: &str = "https://api2.cursor.sh/oauth/token";
pub const DEFAULT_CLIENT_ID: &str = "KbZUR41cY7W6zRSdpSUJ7I7mLYBKOCmB";

/// 一次刷新的产物。
#[derive(Debug)]
pub struct RefreshedSession {
    /// `user_xxx::<jwt>`。查用量用它。
    pub session_token: Secret,
    pub user_id: String,
    pub access_token: Secret,
    /// Cursor 有时会轮换 refresh_token；换了就要存回去，不然下次刷新会失败。
    /// 只靠一份 session token 撑着的号没有它（`None`）：到期就只能重新粘一份。
    pub refresh_token: Option<Secret>,
    pub access_expires_at: Option<String>,
    /// 这把会话是**复用**上次那把、没走 token 交换。
    ///
    /// 调用方需要它是因为：复用的会话被上游拒了，不代表凭证废了，也可能只是这把提前
    /// 失效了。那时候该换一把新的重试一次，而不是直接把号判死。
    pub reused: bool,
}

#[derive(Serialize)]
struct RefreshRequest<'a> {
    grant_type: &'a str,
    client_id: &'a str,
    refresh_token: &'a str,
}

pub async fn refresh_to_session(
    http: &reqwest::Client,
    refresh_token: &str,
) -> Result<RefreshedSession> {
    let rt = refresh_token.trim();
    if rt.is_empty() {
        return Err(AppError::invalid("缺少 refresh_token。"));
    }

    let res = http
        .post(TOKEN_URL)
        .timeout(Duration::from_secs(15))
        .header("accept", "application/json")
        .json(&RefreshRequest {
            grant_type: "refresh_token",
            client_id: DEFAULT_CLIENT_ID,
            refresh_token: rt,
        })
        .send()
        .await
        .map_err(|err| AppError::network(format!("刷新 token 请求失败：{err}")))?;

    let status = res.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(AppError::unauthorized("refresh_token 已过期或无效。"));
    }
    let body = res.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AppError::upstream(format!("刷新接口返回 {status}")));
    }
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|_| AppError::upstream("刷新响应不是 JSON。"))?;

    if json.get("shouldLogout").and_then(|v| v.as_bool()) == Some(true)
        || json.get("should_logout").and_then(|v| v.as_bool()) == Some(true)
    {
        return Err(AppError::unauthorized(
            "Cursor 要求重新登录（shouldLogout）。",
        ));
    }

    let access = pick(&json, &["access_token", "accessToken"])
        .ok_or_else(|| AppError::upstream("刷新响应缺少 access_token。"))?;
    // Cursor 不一定回新的 refresh_token；没回就继续用旧的。
    let next_refresh =
        pick(&json, &["refresh_token", "refreshToken"]).unwrap_or_else(|| rt.to_string());
    let user_id = extract_user_id(&access)
        .or_else(|| extract_user_id(rt))
        .ok_or_else(|| AppError::upstream("access_token 里没有 user_xxx。"))?;

    Ok(RefreshedSession {
        session_token: Secret::new(format!("{user_id}::{access}")),
        access_expires_at: jwt_expiry_iso(&access),
        access_token: Secret::new(access),
        refresh_token: Some(Secret::new(next_refresh)),
        user_id,
        reused: false,
    })
}

/// 用**手上已有的** access token 拼一把会话，不去换新的。
///
/// access token 自带 `exp`，所以「还能不能用」不需要额外存一份过期时间去对，解开 JWT
/// 看一眼就行。过期（或看不出期限）就返回 `None`，让调用方老老实实去换。
pub fn reuse_session(
    user_id: &str,
    access: &Secret,
    refresh: Option<&Secret>,
) -> Option<RefreshedSession> {
    let expires_at = jwt_expiry_iso(access.expose());
    if session_expired(expires_at.as_deref()) {
        return None;
    }
    Some(RefreshedSession {
        session_token: Secret::new(format!("{user_id}::{}", access.expose())),
        user_id: user_id.to_string(),
        access_token: access.clone(),
        refresh_token: refresh.cloned(),
        access_expires_at: expires_at,
        reused: true,
    })
}

/// 用户手上的「session token」长什么样都认：`user_xxx::<jwt>`、URL 编码过的 `user_xxx%3A%3A<jwt>`、
/// 或者一枚裸 access JWT。统一拆成 `(user_id, 裸 jwt)`；user_id 优先取前缀，没有前缀就从 JWT 的
/// `sub` 里读。库里只存裸 JWT——前缀是可以从 JWT 算回来的，存两份只会在某天对不上。
///
/// 认不出是 JWT 的一律拒收：一串随便什么字符存进 Access 那一格，之后每次拼 cookie 都会失败，
/// 而失败的原因用户看不见。
pub fn normalize_access(raw: &str) -> Result<(Option<String>, String)> {
    let s = raw.trim().replace("%3A%3A", "::");
    let (prefix, jwt) = match s.split_once("::") {
        Some((p, j)) if p.starts_with("user_") => (Some(p.to_string()), j.trim().to_string()),
        Some(_) | None => (None, s.clone()),
    };
    if jwt.split('.').count() != 3 || decode_payload(&jwt).is_none() {
        return Err(
            AppError::invalid("这不是 Cursor 的 session / access token。")
                .with_hint("形如 user_xxx::eyJ… 或者一枚 eyJ… 开头的 JWT。"),
        );
    }
    let user_id = prefix.or_else(|| extract_user_id(&jwt));
    Ok((user_id, jwt))
}

fn pick(json: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        let s = json.get(*k)?.as_str()?.trim();
        (!s.is_empty()).then(|| s.to_string())
    })
}

/// Cursor 的 JWT 里 `sub` 形如 `auth0|user_xxx`；取 `user_` 那一段。
pub fn extract_user_id(jwt: &str) -> Option<String> {
    let sub = decode_payload(jwt)?.get("sub")?.as_str()?.to_string();
    let id = sub.rsplit('|').next().unwrap_or(&sub).to_string();
    id.starts_with("user_").then_some(id)
}

/// access JWT 里的 WorkOS 会话 id。`ListActiveSessions` 返回的 `sessionId` 就对它。
pub fn extract_session_id(jwt: &str) -> Option<String> {
    let payload = decode_payload(jwt)?;
    let id = payload
        .get("workosSessionId")
        .or_else(|| payload.get("workos_session_id"))
        .and_then(Value::as_str)?
        .trim();
    (!id.is_empty()).then(|| id.to_string())
}

pub fn jwt_expiry_iso(jwt: &str) -> Option<String> {
    let exp = decode_payload(jwt)?.get("exp")?.as_f64()?;
    nexus_core::clock::iso_from_millis((exp * 1000.0) as i64)
}

fn decode_payload(jwt: &str) -> Option<serde_json::Value> {
    use base64::Engine;
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 会话 token 只是拿来查用量的短期物；过期就重新刷。
pub fn session_expired(access_expires_at: Option<&str>) -> bool {
    let Some(raw) = access_expires_at else {
        return true;
    };
    let Some(at) = nexus_core::clock::parse_iso(raw) else {
        return true;
    };
    // 留 60 秒余量，别在临界点上发一个注定被拒的请求。
    at <= time::OffsetDateTime::now_utc() + time::Duration::seconds(60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use nexus_core::ErrorCode;

    fn jwt(payload: serde_json::Value) -> String {
        let enc = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        format!(
            "{}.{}.{}",
            enc(br#"{"alg":"HS256"}"#),
            enc(payload.to_string().as_bytes()),
            enc(b"sig")
        )
    }

    #[test]
    fn extracts_the_workos_session_id() {
        let t = jwt(serde_json::json!({
            "sub": "auth0|user_01JABCDEF",
            "workosSessionId": "session_01XYZ"
        }));
        assert_eq!(extract_session_id(&t).as_deref(), Some("session_01XYZ"));
        assert!(extract_session_id(&jwt(serde_json::json!({ "sub": "user_1" }))).is_none());
    }

    #[test]
    fn extracts_the_workos_user_id_from_an_auth0_subject() {
        let t = jwt(serde_json::json!({ "sub": "auth0|user_01JABCDEF" }));
        assert_eq!(extract_user_id(&t).as_deref(), Some("user_01JABCDEF"));
    }

    #[test]
    fn accepts_a_bare_user_subject() {
        let t = jwt(serde_json::json!({ "sub": "user_01JABCDEF" }));
        assert_eq!(extract_user_id(&t).as_deref(), Some("user_01JABCDEF"));
    }

    #[test]
    fn rejects_subjects_that_are_not_workos_users() {
        assert!(extract_user_id(&jwt(serde_json::json!({ "sub": "auth0|abc" }))).is_none());
        assert!(extract_user_id(&jwt(serde_json::json!({ "nope": 1 }))).is_none());
        assert!(extract_user_id("not-a-jwt").is_none());
        assert!(extract_user_id("").is_none());
        assert!(extract_user_id("a.!!!not-base64!!!.c").is_none());
    }

    #[test]
    fn reads_the_expiry_out_of_the_token() {
        let t = jwt(serde_json::json!({ "sub": "user_1", "exp": 1_788_307_200i64 }));
        assert_eq!(jwt_expiry_iso(&t).as_deref(), Some("2026-09-02T00:00:00Z"));
        assert!(jwt_expiry_iso(&jwt(serde_json::json!({ "sub": "user_1" }))).is_none());
    }

    #[test]
    fn a_session_with_no_known_expiry_counts_as_expired() {
        assert!(session_expired(None));
        assert!(session_expired(Some("garbage")));
        assert!(session_expired(Some("2020-01-01T00:00:00Z")));
        assert!(!session_expired(Some("2099-01-01T00:00:00Z")));
    }

    #[test]
    fn a_live_access_token_is_reused_instead_of_traded_for_a_new_one() {
        let far = time::OffsetDateTime::now_utc() + time::Duration::hours(2);
        let access = jwt(serde_json::json!({ "sub": "user_1", "exp": far.unix_timestamp() }));
        let rt = Secret::new("rt");
        let reused = reuse_session("user_1", &Secret::new(access.clone()), Some(&rt))
            .expect("还没过期的会话应当可以直接复用");
        assert_eq!(reused.session_token.expose(), format!("user_1::{access}"));
        assert!(
            reused.reused,
            "复用的会话要标出来，被拒时才知道该换一把重试"
        );
    }

    #[test]
    fn an_expired_or_unreadable_access_token_forces_a_real_exchange() {
        let rt = Secret::new("rt");
        let stale = jwt(serde_json::json!({ "sub": "user_1", "exp": 1_600_000_000i64 }));
        assert!(reuse_session("user_1", &Secret::new(stale), Some(&rt)).is_none());
        // 看不出期限的一律当过期：宁可多换一次，也不要拿一把不知道死没死的会话去发请求。
        let no_exp = jwt(serde_json::json!({ "sub": "user_1" }));
        assert!(reuse_session("user_1", &Secret::new(no_exp), Some(&rt)).is_none());
        assert!(reuse_session("user_1", &Secret::new("not-a-jwt"), Some(&rt)).is_none());
    }

    #[test]
    fn a_session_only_account_reuses_its_access_token_without_a_refresh() {
        let far = time::OffsetDateTime::now_utc() + time::Duration::hours(2);
        let access = jwt(serde_json::json!({ "sub": "user_1", "exp": far.unix_timestamp() }));
        let s = reuse_session("user_1", &Secret::new(access), None).expect("有效期内就能用");
        assert!(s.refresh_token.is_none());
    }

    #[test]
    fn normalizes_every_shape_a_pasted_session_token_comes_in() {
        let access = jwt(serde_json::json!({ "sub": "auth0|user_9", "exp": 1i64 }));
        for raw in [
            format!("user_9::{access}"),
            format!("user_9%3A%3A{access}"),
            format!("  {access}  "),
        ] {
            let (uid, jwt) = normalize_access(&raw).unwrap();
            assert_eq!(uid.as_deref(), Some("user_9"), "{raw}");
            assert_eq!(jwt, access);
        }
        // 前缀说 user_7、JWT 里说 user_9：前缀是用户明确给的，听它的。
        let (uid, _) = normalize_access(&format!("user_7::{access}")).unwrap();
        assert_eq!(uid.as_deref(), Some("user_7"));
        assert!(normalize_access("user_9::not-a-jwt").is_err());
        assert!(normalize_access("crsr_apikey").is_err());
    }

    #[test]
    fn a_session_expiring_within_the_minute_is_treated_as_expired() {
        let soon = time::OffsetDateTime::now_utc() + time::Duration::seconds(30);
        let iso = soon
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap();
        assert!(
            session_expired(Some(&iso)),
            "临界点上不该再发注定被拒的请求"
        );
    }

    #[tokio::test]
    async fn an_empty_refresh_token_fails_before_any_request() {
        let http = reqwest::Client::new();
        let err = refresh_to_session(&http, "   ").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn picks_either_snake_or_camel_case_keys() {
        let snake = serde_json::json!({ "access_token": "a" });
        let camel = serde_json::json!({ "accessToken": "b" });
        assert_eq!(
            pick(&snake, &["access_token", "accessToken"]).as_deref(),
            Some("a")
        );
        assert_eq!(
            pick(&camel, &["access_token", "accessToken"]).as_deref(),
            Some("b")
        );
        assert!(pick(
            &serde_json::json!({ "access_token": "  " }),
            &["access_token"]
        )
        .is_none());
    }
}
