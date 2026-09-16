//! web → session 转换：把一把还活着的**网站** web token 换成真正的**桌面** session token。
//!
//! ## 为什么需要它
//!
//! `type=web` 的 token（网站 WorkOS 会话，`WorkosCursorSessionToken` cookie 里那把）**不能**
//! 直接写进 Cursor 的登录态：写进去 Cursor 一续期就收到服务端的 `shouldLogout: true`，走登出
//! 流程把这个 WorkOS 会话终止，号就掉了（2026-09-16 真机：joshua / jessica）。判据见
//! [`crate::model::Account::can_write_cursor_login`]。
//!
//! 但这把 web token 的网站会话此刻是活的，可以**无密码、无验证码**地完成一次 Cursor 官方的
//! `loginDeepControl` 深链登录，服务端就发一把真正的桌面 `type=session` access **加一把 refresh**
//! 回来。真机验证过三件事：
//! - 带着 web cookie 打开登录页，服务端直接认出账号，不需要再输密码 / 验证码；
//! - 换出来的 access 是 `type=session`、refresh 能反复 `/oauth/token` 续期（durable）；
//! - **转换不作废原来的 web 会话**（换完原 token 还能读 api2）。
//!
//! 转换之后这个号就带上了 refresh，从「仅会话（到期就废）」升级成长期号，可以一直切。
//!
//! ## 边界
//!
//! 只在切号入口按需调用（`accounts_add_to_switch_book` 遇到 web-only 号时），**不进任何自动
//! 路径**——它毕竟是拿用户的网站会话去官方登录端点走一遭，该由用户点「切号」这个动作触发。

use crate::oauth::OauthTokens;
use nexus_core::{AppError, ErrorCode, Result, Secret};
use std::sync::Arc;
use std::time::{Duration, Instant};

const WEBSITE: &str = "https://cursor.com";
const LOGIN_DEEP: &str = "https://cursor.com/loginDeepControl";
const CALLBACK: &str = "https://cursor.com/api/auth/loginDeepCallbackControl";
const POLL: &str = "https://api2.cursor.sh/auth/poll";
const UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0 Safari/537.36";

/// 拿 `user_xxx` + 一把活着的 web JWT，换出桌面 session + refresh。
///
/// `user_id` 是 `WorkosCursorSessionToken` cookie 里 `::` 前面那截；`jwt` 是后面那截裸 JWT。
pub async fn web_to_session(user_id: &str, jwt: &str) -> Result<OauthTokens> {
    let user_id = user_id.trim();
    let jwt = jwt.trim();
    if user_id.is_empty() || jwt.is_empty() {
        return Err(AppError::invalid("缺 user_id 或 web token，换不了。")
            .with_hint("这个号没有可用的网站会话；粘一份新的 session token，或用密码授权。"));
    }

    // PKCE：verifier 留在本进程，challenge = sha256(verifier)。
    use base64::Engine;
    use rand::Rng;
    use sha2::{Digest, Sha256};
    let mut raw = [0u8; 32];
    rand::rng().fill_bytes(&mut raw);
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let verifier = b64.encode(raw);
    let challenge = b64.encode(Sha256::digest(verifier.as_bytes()));
    let uuid = uuid::Uuid::new_v4().to_string();

    // 专用带 cookie jar 的客户端：把 web token 放进 jar，打开登录页时服务端还会 set 一个
    // `cursor-web-target-*` cookie，后面 callbackControl 要连着这两个一起带。手动拼 Cookie 头
    // 和 jar 混用容易互相覆盖，所以统一交给 jar。
    let jar = Arc::new(reqwest::cookie::Jar::default());
    let site: reqwest::Url = WEBSITE
        .parse()
        .map_err(|e| AppError::internal(format!("URL 解析失败：{e}")))?;
    jar.add_cookie_str(
        &format!("WorkosCursorSessionToken={user_id}%3A%3A{jwt}; Domain=.cursor.com; Path=/"),
        &site,
    );
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent(UA)
        .cookie_provider(jar)
        .build()
        .map_err(|e| AppError::internal(format!("构造 HTTP 客户端失败：{e}")))?;

    // 1) 带 web cookie 打开 loginDeepControl，拿 bootstrap cookie（跟随重定向即可）。
    let login_url = format!("{LOGIN_DEEP}?challenge={challenge}&uuid={uuid}&mode=login");
    http.get(&login_url)
        .send()
        .await
        .map_err(|e| AppError::network(format!("打开 Cursor 登录页失败：{e}")))?;

    // 2) 完成登录（浏览器里点「继续」做的事）。服务端认出会话就 200；认不出会 3xx 重定向到
    //    workos 授权页——那说明这把 web 会话已经不被认了。
    let body = serde_json::json!({
        "uuid": uuid,
        "challenge": challenge,
        "redirectTarget": "cursor",
        "mobile": false,
    });
    let cb = http
        .post(CALLBACK)
        .header("origin", WEBSITE)
        .header("referer", &login_url)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::network(format!("完成 Cursor 登录失败：{e}")))?;
    if !cb.status().is_success() {
        return Err(AppError::new(
            ErrorCode::Unauthorized,
            "Cursor 没认出这个号的网站会话，换不出桌面登录。",
        )
        .with_hint(
            "这把 web token 的网站会话可能已经过期或被登出。到凭证页粘一份新的 session token，\
             或用密码授权一次拿到 refresh_token。",
        ));
    }

    // 3) 轮询 auth/poll 拿桌面 token（api2，凭 verifier，不需要 cookie）。
    poll_tokens(&http, &uuid, &verifier).await
}

async fn poll_tokens(http: &reqwest::Client, uuid: &str, verifier: &str) -> Result<OauthTokens> {
    let url = format!("{POLL}?uuid={}&verifier={}", enc(uuid), enc(verifier));
    let started = Instant::now();
    // callbackControl 已经 200，token 通常立刻就在；给 20 秒兜住服务端落库的短暂延迟。
    let deadline = Duration::from_secs(20);
    let mut last: Option<u16> = None;
    while started.elapsed() < deadline {
        match http
            .get(&url)
            .header("accept", "application/json")
            .send()
            .await
        {
            Ok(res) => {
                last = Some(res.status().as_u16());
                if res.status().is_success() {
                    if let Ok(json) = res.json::<serde_json::Value>().await {
                        if let Some(t) = read_tokens(&json) {
                            return Ok(t);
                        }
                    }
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
        }
    }
    Err(AppError::new(
        ErrorCode::OauthTimeout,
        format!(
            "换桌面 token 超时（最后状态 {}）。",
            last.map(|s| s.to_string())
                .unwrap_or_else(|| "无响应".into())
        ),
    )
    .with_hint("网络抖动，稍后再点一次切号即可。"))
}

fn read_tokens(json: &serde_json::Value) -> Option<OauthTokens> {
    let pick = |keys: [&str; 2]| -> Option<String> {
        keys.iter().find_map(|k| {
            let s = json.get(*k)?.as_str()?.trim();
            (!s.is_empty()).then(|| s.to_string())
        })
    };
    Some(OauthTokens {
        access_token: Secret::new(pick(["access_token", "accessToken"])?),
        refresh_token: Secret::new(pick(["refresh_token", "refreshToken"])?),
        auth_id: pick(["auth_id", "authId"]),
    })
}

fn enc(raw: &str) -> String {
    raw.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_credentials_are_rejected_before_any_network() {
        let err = web_to_session("", "jwt").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        let err = web_to_session("user_1", "  ").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn read_tokens_takes_either_casing() {
        let camel = serde_json::json!({"accessToken":"at","refreshToken":"rt","authId":"aid"});
        let t = read_tokens(&camel).unwrap();
        assert_eq!(t.access_token.expose(), "at");
        assert_eq!(t.refresh_token.expose(), "rt");
        assert_eq!(t.auth_id.as_deref(), Some("aid"));
        let snake = serde_json::json!({"access_token":"at","refresh_token":"rt"});
        assert!(read_tokens(&snake).is_some());
        // 缺 refresh 就不算成功（web 转换必须同时拿到两把）。
        assert!(read_tokens(&serde_json::json!({"access_token":"at"})).is_none());
    }
}
