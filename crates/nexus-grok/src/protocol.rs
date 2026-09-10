//! Grok 的公共事实：地址、头、模型清单。
//!
//! 两个上游、两种凭证：
//!
//! | 凭证 | 文本（Responses） | 媒体（Imagine：图 / 视频） |
//! |---|---|---|
//! | OAuth 订阅号 | `cli-chat-proxy.grok.com/v1` + Grok CLI 身份头 | `api.x.ai/v1`（同一把 bearer） |
//! | API Key | `api.x.ai/v1` | `api.x.ai/v1` |
//!
//! 订阅号的媒体走 `api.x.ai` 而不是 chat-proxy，是 sub2api 线上踩出来的：chat-proxy 的请求体上限
//! 小，1–2 MB 的 base64 图生视频会被拒；`api.x.ai` 对同一把 OAuth bearer 是放行的（官方 Grok Build
//! 的 `image_gen` / `video_gen` 也是走 `/v1/images` `/v1/videos` 这组路径）。
//!
//! 版本头是**下限**：chat-proxy 会对低于当前 Grok CLI 版本的客户端回 426（"Your Grok CLI version
//! is outdated"）。426 不是账号的问题——换号无用，要升这里的常量。
//!
//! 推理请求体的清洗在 `nexus-gateway` 的 `grok` 模块。

use std::time::Duration;

pub const CLI_CHAT_PROXY: &str = "https://cli-chat-proxy.grok.com/v1";
pub const XAI_API: &str = "https://api.x.ai/v1";
pub const CLIENT_VERSION: &str = "0.2.114";
pub const TOKEN_AUTH: &str = "xai-grok-cli";
pub const CLIENT_IDENTIFIER: &str = "grok-shell";

/// access token 提前这么久续。xAI 的 access 通常一小时级，按 5 分钟提前。
pub const REFRESH_AHEAD: Duration = Duration::from_secs(5 * 60);

pub const GROK_MODELS: &[&str] = &[
    "grok-4.6",
    "grok-4.5",
    "grok-4.3",
    "grok-4",
    "grok-4-fast",
    "grok-build-0.1",
    "grok-composer-2.5-fast",
    "grok-code",
    "grok-code-fast-1",
    "grok-3",
    "grok-3-mini",
];

/// 生图模型。`grok-imagine` 是官方文档里的别名，实际发 `grok-imagine-image`。
pub const GROK_IMAGE_MODELS: &[&str] = &[
    "grok-imagine-image",
    "grok-imagine-image-quality",
    "grok-imagine-image-2.0",
    "grok-imagine",
];
/// 官方 Grok Build 客户端默认用的生图 / 改图模型。
pub const DEFAULT_IMAGE_MODEL: &str = "grok-imagine-image-quality";
pub const DEFAULT_IMAGE_EDIT_MODEL: &str = "grok-imagine-image-quality";

/// 生视频模型。`-1.5` 支持 1080p 与图生视频；参考图生视频只有不带后缀的那个支持。
pub const GROK_VIDEO_MODELS: &[&str] = &["grok-imagine-video-1.5", "grok-imagine-video"];
pub const DEFAULT_VIDEO_MODEL: &str = "grok-imagine-video-1.5";

pub const ROUTE_PREFIXES: &[&str] = &["grok/", "xai/"];

fn base_of(model: &str) -> String {
    split_route_prefix(model).0.trim().to_ascii_lowercase()
}

/// 任何 `grok-*`（含媒体）。
pub fn is_grok_model(model: &str) -> bool {
    let base = base_of(model);
    base.starts_with("grok-") || GROK_MODELS.iter().any(|m| m.eq_ignore_ascii_case(&base))
}

pub fn is_grok_image_model(model: &str) -> bool {
    let base = base_of(model);
    GROK_IMAGE_MODELS
        .iter()
        .any(|m| m.eq_ignore_ascii_case(&base))
}

pub fn is_grok_video_model(model: &str) -> bool {
    let base = base_of(model);
    GROK_VIDEO_MODELS
        .iter()
        .any(|m| m.eq_ignore_ascii_case(&base))
}

pub fn is_grok_media_model(model: &str) -> bool {
    is_grok_image_model(model) || is_grok_video_model(model)
}

/// 客户端写的生图模型名 → 发给上游的。别名归正；空 / `auto` 给默认。
pub fn upstream_image_model(model: &str) -> String {
    let base = base_of(model);
    if base.is_empty() || base == "auto" || base == "grok-imagine" {
        return if base == "grok-imagine" {
            "grok-imagine-image".into()
        } else {
            DEFAULT_IMAGE_MODEL.into()
        };
    }
    base
}

pub fn upstream_video_model(model: &str) -> String {
    let base = base_of(model);
    if base.is_empty() || base == "auto" {
        return DEFAULT_VIDEO_MODEL.into();
    }
    base
}

pub fn split_route_prefix(model: &str) -> (&str, bool) {
    let t = model.trim();
    let lower = t.to_ascii_lowercase();
    for p in ROUTE_PREFIXES {
        if let Some(rest) = lower.strip_prefix(p) {
            let start = t.len() - rest.len();
            return (&t[start..], true);
        }
    }
    (t, false)
}

pub fn user_agent() -> String {
    format!("xai-grok-workspace/{CLIENT_VERSION}")
}

/// Grok CLI 的身份头。chat-proxy 靠这几个认出「官方客户端」；缺一个就 426 / 402。
pub fn cli_headers(access_token: &str) -> Vec<(String, String)> {
    vec![
        ("authorization".into(), format!("Bearer {access_token}")),
        ("x-xai-token-auth".into(), TOKEN_AUTH.into()),
        ("x-grok-client-version".into(), CLIENT_VERSION.into()),
        ("x-grok-client-identifier".into(), CLIENT_IDENTIFIER.into()),
        (
            "x-authenticateresponse".into(),
            "authenticate-response".into(),
        ),
        ("user-agent".into(), user_agent()),
    ]
}

/// 订阅号的聊天头：CLI 身份头 + SSE + 会话。
pub fn chat_headers(access_token: &str, session_id: &str) -> Vec<(String, String)> {
    let mut h = cli_headers(access_token);
    h.push(("content-type".into(), "application/json".into()));
    h.push(("accept".into(), "text/event-stream".into()));
    if !session_id.trim().is_empty() {
        h.push(("x-grok-conv-id".into(), session_id.trim().to_string()));
        h.push(("x-grok-session-id".into(), session_id.trim().to_string()));
    }
    h
}

/// API Key 号的聊天头：普通 bearer，不装 CLI。
pub fn api_key_chat_headers(api_key: &str) -> Vec<(String, String)> {
    vec![
        ("authorization".into(), format!("Bearer {api_key}")),
        ("content-type".into(), "application/json".into()),
        ("accept".into(), "text/event-stream".into()),
        ("user-agent".into(), user_agent()),
    ]
}

/// 媒体头：两种凭证都是 bearer 到 `api.x.ai`；订阅号顺带把 CLI 身份带上（官方客户端如此，
/// 服务端据此认订阅额度）。
pub fn media_headers(token: &str, oauth: bool, session_id: Option<&str>) -> Vec<(String, String)> {
    let mut h = if oauth {
        cli_headers(token)
    } else {
        vec![
            ("authorization".into(), format!("Bearer {token}")),
            ("user-agent".into(), user_agent()),
        ]
    };
    h.push(("content-type".into(), "application/json".into()));
    if let Some(s) = session_id.filter(|s| !s.trim().is_empty()) {
        h.push(("x-grok-session-id".into(), s.trim().to_string()));
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_prefix_and_bare_names() {
        assert!(is_grok_model("grok-4.5"));
        assert!(is_grok_model("grok/grok-4"));
        assert!(split_route_prefix("xai/grok-3").1);
        assert!(!is_grok_model("gpt-5.6-sol"));
        assert!(!is_grok_model("claude-sonnet-5"));
    }

    #[test]
    fn media_models_are_told_apart_from_chat() {
        assert!(is_grok_image_model("grok-imagine"));
        assert!(is_grok_image_model("xai/grok-imagine-image-2.0"));
        assert!(is_grok_video_model("grok-imagine-video-1.5"));
        assert!(!is_grok_image_model("grok-4.5"));
        assert!(is_grok_media_model("grok-imagine-video"));
        assert_eq!(upstream_image_model("grok-imagine"), "grok-imagine-image");
        assert_eq!(upstream_image_model(""), DEFAULT_IMAGE_MODEL);
        assert_eq!(upstream_video_model("auto"), DEFAULT_VIDEO_MODEL);
    }

    #[test]
    fn cli_headers_carry_the_identity_the_proxy_checks() {
        let h = chat_headers("tok", "s1");
        let get = |k: &str| h.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str());
        assert_eq!(get("x-xai-token-auth"), Some(TOKEN_AUTH));
        assert_eq!(get("x-grok-client-version"), Some(CLIENT_VERSION));
        assert_eq!(get("x-grok-conv-id"), Some("s1"));
        let k = api_key_chat_headers("xai-1");
        assert!(k.iter().all(|(kk, _)| kk != "x-xai-token-auth"));
    }
}
