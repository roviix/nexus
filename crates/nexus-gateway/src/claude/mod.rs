//! Claude 订阅号直连 `api.anthropic.com/v1/messages`。
//!
//! 客户端讲 Messages 时走供应商那条原样转发：缓存断点、thinking 签名留在请求体里。
//! 别的方言先收成中间表示再写成 Messages。
//!
//! OAuth / setup-token 出站按 [`nexus_claude::fingerprint`] 补齐 Claude Code CLI
//! 的指纹（计费头、身份句、beta、工具名、`metadata.user_id`）。真实 Claude Code
//! 已经带齐的只换认证和账号身份。Console API Key 不走这套，按钥匙直连。

use crate::error::{UpstreamError, UpstreamKind};
use crate::inbound::Dialect;
use crate::lane::{BoxFuture, Credential};
use crate::normalized::ChatRequest;
use crate::provider::wire::{self, ClaudeOauthTune, Target};
use crate::upstream::{DeltaSink, Upstream};
use nexus_claude::fingerprint;
use nexus_claude::ClaudeAuthMode;
use nexus_store::key_providers::ApiFormat;
use nexus_store::key_providers::AuthField;
use std::sync::Arc;

pub trait ClaudeRouting: Send + Sync {
    fn route(&self, label: &str) -> Option<(ClaudeAuthMode, String)>;
}

pub struct ClaudeUpstream {
    routing: Arc<dyn ClaudeRouting>,
}

impl ClaudeUpstream {
    pub fn new(routing: Arc<dyn ClaudeRouting>) -> Self {
        Self { routing }
    }
}

impl Upstream for ClaudeUpstream {
    fn stream<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ChatRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<crate::normalized::Completion, UpstreamError>> {
        Box::pin(async move {
            let (target, req) = outbound(self.routing.as_ref(), credential, request)?;
            wire::execute(&target, &req, on_delta).await
        })
    }

    fn count_tokens<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ChatRequest,
    ) -> BoxFuture<'a, Result<u64, UpstreamError>> {
        Box::pin(async move {
            let (target, req) = outbound(self.routing.as_ref(), credential, request)?;
            wire::count_input_tokens(&target, &req).await
        })
    }
}

/// 拼出这一次打 `api.anthropic.com` 的目标和请求。推理和 `count_tokens` 共用，
/// 指纹、beta、身份头只在这里决定一次。
fn outbound(
    routing: &dyn ClaudeRouting,
    credential: &Credential,
    request: &ChatRequest,
) -> Result<(Target, ChatRequest), UpstreamError> {
    let (mode, account_ref) = routing.route(&credential.label).ok_or_else(|| {
        UpstreamError::new(
            UpstreamKind::Auth,
            401,
            "找不到这个 Claude 账号。重新授权一次。",
        )
    })?;
    let oauth = mode.uses_bearer();
    let native = oauth
        && request.raw_inbound.as_ref().is_some_and(|raw| {
            raw.dialect == Dialect::AnthropicMessages
                && fingerprint::looks_like_claude_code(&raw.headers, raw.body.as_ref())
        });
    let requested_betas = request
        .raw_inbound
        .as_ref()
        .and_then(|raw| {
            raw.headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("anthropic-beta"))
                .map(|(_, v)| {
                    v.split(',')
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                })
        })
        .unwrap_or_default();
    let wants_1m = nexus_claude::protocol::wants_one_million(&request.model)
        || requested_betas
            .iter()
            .any(|b| b.eq_ignore_ascii_case(fingerprint::BETA_CONTEXT_1M));
    let mut req = request.clone();
    let extra_headers = if oauth {
        if !native {
            if let Some(raw) = req.raw_inbound.as_mut() {
                raw.headers
                    .retain(|(k, _)| !fingerprint::is_identity_header(k));
            }
            fingerprint::identity_headers()
        } else {
            let mut h = vec![("anthropic-beta".into(), nexus_claude::OAUTH_BETA.into())];
            if wants_1m {
                h.push(("anthropic-beta".into(), fingerprint::BETA_CONTEXT_1M.into()));
            }
            h
        }
    } else {
        Vec::new()
    };
    let target = Target {
        name: credential.label.clone(),
        base_url: nexus_claude::protocol::MESSAGES_ORIGIN.into(),
        api_key: credential.access_token.clone(),
        format: ApiFormat::Anthropic,
        auth: if oauth {
            AuthField::AuthToken
        } else {
            AuthField::ApiKey
        },
        upstream_model: nexus_claude::upstream_model(&request.model),
        extra_headers,
        claude_oauth: oauth.then(|| ClaudeOauthTune {
            account_ref,
            native,
            requested_betas,
            wants_1m,
        }),
    };
    Ok((target, req))
}
