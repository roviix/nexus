//! aiserver.v1 的 IDE 请求头集合。
//!
//! api2 的 IDE 端点强校验这一整套头（checksum / client-key / session-id / config-version …），
//! 缺一个就回 `unauthenticated(16)`，而且不说缺的是哪个。这一组照 `protocol.js` 的
//! `buildAiHeaders` 逐项搬：键名、取值规则、常量一个都不改。
//!
//! `x-cursor-client-type` 只是**额度通道标签**（ide / cli / sand），不选端点——所有值都打
//! 同一个 Stream。网关不预设 sand：用户自己的号默认走它本来的通道，蹭 bot 额度是另一个
//! 显式的高风险开关，两件事不绑在一起。

use crate::identity::DeviceIdentity;
use uuid::Uuid;

/// 以 CLI 身份出面时报的版本。
pub const CLI_CLIENT_VERSION: &str = "cli-2026.08.11-e8db854";
/// 以 IDE 身份出面时报的版本。
///
/// **必须跟得上真机在装的 Cursor**：api2 对过旧的 IDE 版本直接拒服务，回的是
/// `ERROR_GPT_4_VISION_PREVIEW_RATE_LIMIT` + "Update Required: Your version of Cursor is
/// no longer supported"——错误码和真实原因毫无关系，照着它排障会一路跑偏。停在 2.6.22 时
/// `ide` 这条通道整条是死的，且和账号、模型、请求内容都无关。
pub const IDE_CLIENT_VERSION: &str = "3.19.7";
pub const CONNECT_USER_AGENT: &str = "connect-es/1.6.1";

/// 一次请求要带的全部头。用 Vec 而不是 map，是为了迭代顺序可预测（日志、测试都省心）。
pub type HeaderList = Vec<(&'static str, String)>;

/// 请求头里仅有的两个非确定量：逐请求随机的 request id，和随时间走的时间戳。
/// 单独拆出来，其余十六个字段就全是账号与 token 的纯函数，测试能逐个钉死。
#[derive(Debug, Clone, Copy)]
pub struct RequestNonce {
    pub request_id: Uuid,
    pub now_ms: u64,
}

impl RequestNonce {
    pub fn now() -> Self {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        Self {
            request_id: Uuid::new_v4(),
            now_ms,
        }
    }
}

/// 版本号跟 client-type 配套：报 ide 就要配 IDE 版本，否则形态对不上。
pub fn client_version_for(client_type: &str) -> &'static str {
    if client_type == "ide" {
        IDE_CLIENT_VERSION
    } else {
        CLI_CLIENT_VERSION
    }
}

/// 组一次请求的头。
///
/// os / arch / os-version / timezone 目前是和 protocol.js 一致的常量——那是线上验证过的形态。
/// 网关跑在用户真机上，以后可以换成真值，但要先确认 api2 对这几个字段的态度，别顺手改。
pub fn ai_headers(
    access_token: &str,
    identity: &DeviceIdentity,
    client_type: &str,
    nonce: RequestNonce,
) -> HeaderList {
    let req_id = nonce.request_id.to_string();
    vec![
        ("authorization", format!("Bearer {access_token}")),
        ("connect-protocol-version", "1".into()),
        ("user-agent", CONNECT_USER_AGENT.into()),
        ("x-amzn-trace-id", format!("Root={req_id}")),
        ("x-client-key", identity.client_key()),
        ("x-cursor-checksum", identity.checksum(nonce.now_ms)),
        (
            "x-cursor-client-version",
            client_version_for(client_type).into(),
        ),
        ("x-cursor-client-type", client_type.into()),
        ("x-cursor-client-os", "darwin".into()),
        ("x-cursor-client-arch", "arm64".into()),
        ("x-cursor-client-os-version", "25.2.0".into()),
        ("x-cursor-client-device-type", "desktop".into()),
        ("x-cursor-config-version", identity.config_version()),
        ("x-cursor-timezone", "Asia/Shanghai".into()),
        ("x-ghost-mode", "false".into()),
        ("x-new-onboarding-completed", "true".into()),
        ("x-request-id", req_id),
        ("x-session-id", identity.session_id()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const JWT: &str =
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhdXRoa2l0fHVzZXJfVEVTVDEyMyIsImV4cCI6OTk5OTk5OTk5OX0.sig";

    fn fixed_nonce() -> RequestNonce {
        RequestNonce {
            request_id: Uuid::nil(),
            now_ms: 1_700_000_000_000,
        }
    }

    fn get<'a>(h: &'a HeaderList, k: &str) -> &'a str {
        h.iter()
            .find(|(name, _)| *name == k)
            .map(|(_, v)| v.as_str())
            .unwrap_or_else(|| panic!("缺少头 {k}"))
    }

    #[test]
    fn sends_the_full_ide_header_set_with_no_duplicates() {
        let h = ai_headers(JWT, &DeviceIdentity::derived(JWT), "cli", fixed_nonce());
        assert_eq!(h.len(), 18);
        let mut names: Vec<_> = h.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 18, "有重复的头");
        for (_, v) in &h {
            assert!(!v.is_empty(), "不该有空值");
        }
    }

    #[test]
    fn deterministic_fields_match_protocol_js_output() {
        // 期望值来自 /tmp/vec.mjs 跑 buildAiHeaders(jwt, "sand")。
        let h = ai_headers(JWT, &DeviceIdentity::derived(JWT), "sand", fixed_nonce());
        assert_eq!(
            get(&h, "x-client-key"),
            "14bf27c057322f2115b7e183b2df2bded0fb6fac73537af7624bf98793f8a293"
        );
        assert_eq!(
            get(&h, "x-session-id"),
            "c4bbe3fc-3b68-5816-a637-881411c33b49"
        );
        assert_eq!(
            get(&h, "x-cursor-config-version"),
            "1f91df2b-ed39-592c-9b0c-1ad75928fea3"
        );
        assert_eq!(get(&h, "x-cursor-client-type"), "sand");
        assert_eq!(get(&h, "x-cursor-client-version"), "cli-2026.08.11-e8db854");
        assert!(get(&h, "x-cursor-checksum")
            .ends_with("472863d1b0bf416aec181ad7721c730801a3c10ec4418cbfdb7e494b2e6045db"));
        assert_eq!(get(&h, "x-cursor-checksum").len(), 8 + 64);
    }

    #[test]
    fn client_version_follows_client_type() {
        let id = DeviceIdentity::derived(JWT);
        assert_eq!(
            get(
                &ai_headers(JWT, &id, "ide", fixed_nonce()),
                "x-cursor-client-version"
            ),
            IDE_CLIENT_VERSION
        );
        assert_eq!(
            get(
                &ai_headers(JWT, &id, "cli", fixed_nonce()),
                "x-cursor-client-version"
            ),
            CLI_CLIENT_VERSION
        );
        assert_eq!(
            get(
                &ai_headers(JWT, &id, "sand", fixed_nonce()),
                "x-cursor-client-version"
            ),
            CLI_CLIENT_VERSION
        );
    }

    #[test]
    fn request_id_and_trace_id_share_one_uuid() {
        let nonce = RequestNonce {
            request_id: Uuid::new_v4(),
            now_ms: 0,
        };
        let h = ai_headers(JWT, &DeviceIdentity::derived(JWT), "cli", nonce);
        let rid = get(&h, "x-request-id");
        assert_eq!(rid, nonce.request_id.to_string());
        assert_eq!(get(&h, "x-amzn-trace-id"), format!("Root={rid}"));
    }

    #[test]
    fn authorization_is_a_bearer_of_the_raw_token() {
        let h = ai_headers(JWT, &DeviceIdentity::derived(JWT), "cli", fixed_nonce());
        assert_eq!(get(&h, "authorization"), format!("Bearer {JWT}"));
        assert_eq!(get(&h, "connect-protocol-version"), "1");
        assert_eq!(get(&h, "user-agent"), CONNECT_USER_AGENT);
    }

    #[test]
    fn pinned_identity_changes_only_the_checksum_suffix() {
        let derived = ai_headers(JWT, &DeviceIdentity::derived(JWT), "cli", fixed_nonce());
        let pinned = ai_headers(
            JWT,
            &DeviceIdentity::pinned(JWT, "realmachine"),
            "cli",
            fixed_nonce(),
        );
        assert!(get(&pinned, "x-cursor-checksum").ends_with("realmachine"));
        for k in ["x-client-key", "x-session-id", "x-cursor-config-version"] {
            assert_eq!(get(&derived, k), get(&pinned, k), "{k} 只该看账号");
        }
    }

    #[test]
    fn nonce_now_is_in_the_present() {
        let n = RequestNonce::now();
        assert!(n.now_ms > 1_700_000_000_000);
        assert!(!n.request_id.is_nil());
    }
}
