//! OAuth 取 token：deep-link + 服务端轮询。
//!
//! Cursor 的 `loginDeepControl?challenge=&uuid=` 配 `api2.cursor.sh/auth/poll` 这条路：
//! **浏览器是用户自己的系统浏览器，应用只做纯 HTTP 轮询。** 好处是三重的——
//! 零浏览器依赖（不用拖 Node/Playwright sidecar）、天然绕开 Cloudflare（真人真浏览器
//! 不会被判机器人）、弹窗关掉也不影响收 token（轮询与窗口无关）。
//!
//! 自动填表（Playwright/patchright）那条路产品不做，理由见 ARCHITECTURE D5。

use nexus_core::{AppError, ErrorCode, Result, Secret};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const LOGIN_URL: &str = "https://cursor.com/loginDeepControl";
const POLL_URL: &str = "https://api2.cursor.sh/auth/poll";

/// 默认给用户五分钟走完登录（含收验证码）。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// 一次 OAuth 会话。`verifier` 是秘密，只留在 Rust 侧。
pub struct OauthSession {
    pub uuid: String,
    /// 丢给系统浏览器打开的地址。**不含 verifier**，所以给前端看是安全的。
    pub login_url: String,
    verifier: Secret,
    cancelled: Arc<AtomicBool>,
}

/// 交给前端的部分。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OauthHandle {
    pub uuid: String,
    pub login_url: String,
}

/// 轮询过程中的状态，通过事件推给界面。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "state",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum OauthState {
    /// 已打开浏览器，等用户完成登录。
    Waiting {
        uuid: String,
        elapsed_secs: u64,
    },
    Succeeded {
        uuid: String,
        email: Option<String>,
    },
    Failed {
        uuid: String,
        message: String,
    },
    Cancelled {
        uuid: String,
    },
}

/// 收到的 token。
#[derive(Debug)]
pub struct OauthTokens {
    pub access_token: Secret,
    pub refresh_token: Secret,
    pub auth_id: Option<String>,
}

impl OauthSession {
    /// 生成 PKCE 对与 uuid，拼出登录地址。
    pub fn start() -> Self {
        use base64::Engine;
        use rand::Rng;
        use sha2::{Digest, Sha256};

        let mut raw = [0u8; 32];
        rand::rng().fill_bytes(&mut raw);
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let verifier = b64.encode(raw);
        let challenge = b64.encode(Sha256::digest(verifier.as_bytes()));
        let uuid = uuid::Uuid::new_v4().to_string();

        Self {
            login_url: format!(
                "{LOGIN_URL}?challenge={}&uuid={}&mode=login",
                urlencode(&challenge),
                urlencode(&uuid)
            ),
            verifier: Secret::new(verifier),
            cancelled: Arc::new(AtomicBool::new(false)),
            uuid,
        }
    }

    pub fn handle(&self) -> OauthHandle {
        OauthHandle {
            uuid: self.uuid.clone(),
            login_url: self.login_url.clone(),
        }
    }

    /// 取消开关。前端点「取消」时置上，轮询在下一拍退出。
    pub fn canceller(&self) -> Arc<AtomicBool> {
        self.cancelled.clone()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn poll_url(&self) -> String {
        format!(
            "{POLL_URL}?uuid={}&verifier={}",
            urlencode(&self.uuid),
            urlencode(self.verifier.expose())
        )
    }

    /// 轮询直到拿到 token、超时或被取消。
    ///
    /// 网络抖动不算失败（登录页可能开着好几分钟，中间断个网很正常），只放慢节奏继续等。
    pub async fn poll(
        &self,
        http: &reqwest::Client,
        timeout: Duration,
        on_state: &(dyn Fn(OauthState) + Sync),
    ) -> Result<OauthTokens> {
        let url = self.poll_url();
        let started = Instant::now();
        let mut last_status: Option<u16> = None;

        while started.elapsed() < timeout {
            if self.cancelled.load(Ordering::SeqCst) {
                on_state(OauthState::Cancelled {
                    uuid: self.uuid.clone(),
                });
                return Err(AppError::new(ErrorCode::Cancelled, "已取消这次授权。"));
            }
            on_state(OauthState::Waiting {
                uuid: self.uuid.clone(),
                elapsed_secs: started.elapsed().as_secs(),
            });

            match http
                .get(&url)
                .header("accept", "application/json")
                .send()
                .await
            {
                Ok(res) => {
                    let status = res.status();
                    last_status = Some(status.as_u16());
                    // 404 = 用户还没走完，这是最常见的响应，不是错误。
                    if status.is_success() {
                        if let Ok(json) = res.json::<serde_json::Value>().await {
                            if let Some(tokens) = read_tokens(&json) {
                                on_state(OauthState::Succeeded {
                                    uuid: self.uuid.clone(),
                                    email: json
                                        .get("email")
                                        .and_then(|v| v.as_str())
                                        .map(str::to_string),
                                });
                                return Ok(tokens);
                            }
                        }
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
                // 断网不该终止一次进行中的登录。
                Err(_) => tokio::time::sleep(POLL_INTERVAL * 2).await,
            }
        }

        let message = format!(
            "等待授权超时（最后状态 {}）。",
            last_status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "无响应".into())
        );
        on_state(OauthState::Failed {
            uuid: self.uuid.clone(),
            message: message.clone(),
        });
        Err(AppError::new(ErrorCode::OauthTimeout, message)
            .with_hint("登录可能没走完，或验证码那一步卡住了。重新发起一次授权即可。"))
    }
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

fn urlencode(raw: &str) -> String {
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

    #[test]
    fn start_produces_a_valid_pkce_login_url() {
        use base64::Engine;
        use sha2::{Digest, Sha256};

        let s = OauthSession::start();
        assert!(s
            .login_url
            .starts_with("https://cursor.com/loginDeepControl?"));
        assert!(s.login_url.contains(&format!("uuid={}", s.uuid)));
        assert!(s.login_url.contains("mode=login"));
        assert!(uuid::Uuid::parse_str(&s.uuid).is_ok());

        // challenge 必须真的是 verifier 的 sha256。
        let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(s.verifier.expose().as_bytes()));
        assert!(
            s.login_url.contains(&urlencode(&expected)),
            "challenge 应当是 verifier 的 SHA-256"
        );
    }

    #[test]
    fn the_verifier_never_appears_in_anything_the_frontend_sees() {
        let s = OauthSession::start();
        let verifier = s.verifier.expose().to_string();
        assert!(
            !s.login_url.contains(&verifier),
            "登录地址里不能带 verifier"
        );
        let handle = serde_json::to_string(&s.handle()).unwrap();
        assert!(!handle.contains(&verifier));
        // 只有轮询地址带它，而那个地址不出 Rust。
        assert!(s.poll_url().contains(&urlencode(&verifier)));
    }

    #[test]
    fn each_session_is_independent() {
        let a = OauthSession::start();
        let b = OauthSession::start();
        assert_ne!(a.uuid, b.uuid);
        assert_ne!(a.verifier.expose(), b.verifier.expose());
    }

    #[test]
    fn tokens_are_read_from_either_casing() {
        let snake = serde_json::json!({
            "access_token": "at", "refresh_token": "rt", "auth_id": "aid"
        });
        let t = read_tokens(&snake).unwrap();
        assert_eq!(t.access_token.expose(), "at");
        assert_eq!(t.refresh_token.expose(), "rt");
        assert_eq!(t.auth_id.as_deref(), Some("aid"));

        let camel = serde_json::json!({ "accessToken": "at", "refreshToken": "rt" });
        assert!(read_tokens(&camel).is_some());
    }

    #[test]
    fn a_half_filled_response_is_not_treated_as_success() {
        // 只有 access 没有 refresh = 拿不到长期凭证，等于没成功。
        assert!(read_tokens(&serde_json::json!({ "access_token": "at" })).is_none());
        assert!(read_tokens(&serde_json::json!({ "refresh_token": "rt" })).is_none());
        assert!(
            read_tokens(&serde_json::json!({ "access_token": "", "refresh_token": "rt" }))
                .is_none()
        );
        assert!(read_tokens(&serde_json::json!({})).is_none());
    }

    #[tokio::test]
    async fn cancelling_stops_the_poll_promptly() {
        let s = OauthSession::start();
        s.cancel();
        let states = std::sync::Mutex::new(Vec::new());
        let err = s
            .poll(&reqwest::Client::new(), Duration::from_secs(60), &|st| {
                states.lock().unwrap().push(st)
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Cancelled);
        assert!(matches!(
            states.lock().unwrap().first(),
            Some(OauthState::Cancelled { .. })
        ));
    }

    #[tokio::test]
    async fn a_zero_timeout_reports_oauth_timeout_with_a_next_step() {
        let s = OauthSession::start();
        let err = s
            .poll(&reqwest::Client::new(), Duration::ZERO, &|_| {})
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::OauthTimeout);
        assert!(err.hint.unwrap().contains("重新发起"));
    }

    #[test]
    fn oauth_state_serializes_with_camel_case_fields() {
        let v = serde_json::to_value(OauthState::Waiting {
            uuid: "u".into(),
            elapsed_secs: 12,
        })
        .unwrap();
        assert_eq!(v["state"], "waiting");
        assert_eq!(v["elapsedSecs"], 12);
    }
}
