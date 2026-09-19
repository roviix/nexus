//! ChatGPT 订阅号直连：`chatgpt.com/backend-api/codex/responses`，Codex CLI 自己走的那条路。
//!
//! 这是网关的第二个后端（第一个是 Cursor 的 `inference`）。两条路进来，处理不同：
//!
//! - **透传**：客户端讲的就是 Responses（Codex CLI）。原始请求体只改上游会拒收的地方、把身份
//!   标识换成这个账号的面孔，然后**逐帧原样**把上游的 SSE 事件交给客户端（[`Delta::Raw`]）——
//!   reasoning 的回放凭据、`compaction_summary`、`web_search_call` 这些中间表示放不下的东西
//!   全都不丢。到达上游的东西和真实 Codex 客户端几乎无法区分。
//! - **桥接**：客户端讲 Chat / Anthropic。从中间表示构造 Responses 请求体，读流时只交
//!   文本 / 思考增量与工具调用，走通用序列化。采样参数上游不收，会丢——这是订阅号通道的能力
//!   边界，不是 bug。
//!
//! 号的凭证由 [`crate::lane`] 给（`ChatGptSource` 负责续期），这里只认 access token；
//! `chatgpt_account_id` 从 token 里解出来。额度随每次响应头回来，写回账号。
//!
//! 协议事实（请求头、拒收表、身份收敛、错误分类）全在 [`protocol`]，都是纯函数。

pub mod protocol;
#[cfg(test)]
mod tests;

use crate::error::{UpstreamError, UpstreamKind};
use crate::inference::{DEFAULT_IDLE_TIMEOUT, DEFAULT_MAX_TURN};
use crate::lane::{BoxFuture, Credential};
use crate::normalized::{ChatRequest, Completion, Delta, FinishReason, ToolCall, Usage};
use crate::sse::{SseDecoder, SseEvent};
use crate::upstream::{DeltaSink, Upstream};
use nexus_chatgpt::{ChatGptService, CodexUsage, DEFAULT_BACKEND_URL};
use protocol::{IdentityMode, Prepared, Verdict};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 同一个号上最多修几次再发（上游 400 指着字段不认 / 别的号签的 reasoning 凭据）。
const MAX_REPAIRS: usize = 3;

#[derive(Debug, Clone)]
pub struct CodexConfig {
    /// `…/backend-api`。测试指向假上游。
    pub backend_url: String,
    pub identity_mode: IdentityMode,
    pub idle_timeout: Duration,
    pub max_turn: Duration,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            backend_url: DEFAULT_BACKEND_URL.to_string(),
            identity_mode: IdentityMode::Scope,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
        }
    }
}

pub struct CodexUpstream {
    client: reqwest::Client,
    cfg: CodexConfig,
    /// 额度与凭证状态写回账号。`None` = 不回写（测试、examples）。
    accounts: Option<Arc<ChatGptService>>,
}

impl CodexUpstream {
    pub fn new(cfg: CodexConfig, accounts: Option<Arc<ChatGptService>>) -> Self {
        Self {
            client: crate::inference::http_client(),
            cfg,
            accounts,
        }
    }

    fn responses_url(&self) -> String {
        format!(
            "{}/codex/responses",
            self.cfg.backend_url.trim_end_matches('/')
        )
    }

    fn account_id_of(&self, account_ref: &str) -> Option<nexus_core::ChatGptAccountId> {
        let svc = self.accounts.as_ref()?;
        svc.repo.by_ref(account_ref).ok().flatten().map(|a| a.id)
    }

    fn note_usage(&self, account_ref: &str, headers: &reqwest::header::HeaderMap) {
        let Some(svc) = &self.accounts else { return };
        let Some(usage) = CodexUsage::from_headers(headers, time::OffsetDateTime::now_utc()) else {
            return;
        };
        if let Some(id) = self.account_id_of(account_ref) {
            if let Err(err) = svc.record_usage(&id, &usage) {
                tracing::warn!(%err, "写回 Codex 额度失败");
            }
        }
    }

    fn note_verdict(&self, account_ref: &str, verdict: &Verdict) {
        let Some(svc) = &self.accounts else { return };
        if verdict.kind != UpstreamKind::Auth {
            return;
        }
        let Some(id) = self.account_id_of(account_ref) else {
            return;
        };
        let res = if verdict.message.contains("停用") {
            svc.mark_dead(&id, &verdict.message)
        } else {
            svc.mark_unauthorized(&id, &verdict.message)
        };
        if let Err(err) = res {
            tracing::warn!(%err, "写回账号状态失败");
        }
    }

    /// 发一次请求，2xx 就把响应交出来；上游 400 指着字段不认 / 别的号签的 reasoning 凭据时
    /// 修一下再发（同一个号上最多 `MAX_REPAIRS` 次，同一个修法不重复试）。其余错误分类后返回，
    /// 并把额度头 / 凭证状态写回账号。返回值里带修过什么，给调用方记日志。
    async fn send(
        &self,
        credential: &Credential,
        account_ref: &str,
        headers: &[(String, String)],
        mut body: serde_json::Map<String, Value>,
    ) -> Result<(reqwest::Response, Vec<String>), UpstreamError> {
        let mut repairs: Vec<String> = Vec::new();
        let mut seen_bodies: Vec<String> = Vec::new();
        loop {
            let payload = serde_json::to_vec(&Value::Object(body.clone())).map_err(|e| {
                UpstreamError::new(
                    UpstreamKind::BadRequest,
                    400,
                    format!("请求体无法序列化：{e}"),
                )
            })?;
            let mut req = self
                .client
                .post(self.responses_url())
                .timeout(self.cfg.max_turn)
                .body(payload);
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }
            let res = match req.send().await {
                Ok(r) => r,
                Err(e) if e.is_timeout() => {
                    return Err(UpstreamError::new(
                        UpstreamKind::Timeout,
                        504,
                        format!("上游连接超时：{e}"),
                    ));
                }
                Err(e) => {
                    return Err(UpstreamError::new(
                        UpstreamKind::Upstream,
                        502,
                        format!("上游连接失败：{e}"),
                    ));
                }
            };
            let status = res.status().as_u16();
            if (200..300).contains(&status) {
                return Ok((res, repairs));
            }
            let res_headers = res.headers().clone();
            let text = res.text().await.unwrap_or_default();
            if repairs.len() < MAX_REPAIRS {
                if protocol::is_invalid_reasoning_signature(status, &text) {
                    let mut next = body.clone();
                    if protocol::strip_encrypted_reasoning(&mut next) {
                        tracing::info!(account = %credential.label, "回带了别的号签的 reasoning 凭据，剔掉重发");
                        repairs.push("剔除 reasoning 凭据".into());
                        body = next;
                        continue;
                    }
                }
                if let Some(repair) = protocol::repair_rejected_request(&body, status, &text) {
                    let fingerprint = Value::Object(repair.body.clone()).to_string();
                    if !seen_bodies.contains(&fingerprint) {
                        tracing::info!(account = %credential.label, reason = %repair.reason, "上游 400 后修正请求重发");
                        seen_bodies.push(fingerprint);
                        repairs.push(repair.reason);
                        body = repair.body;
                        continue;
                    }
                }
            }
            let verdict = protocol::classify_http_error(status, &text, &res_headers, now_ms());
            self.note_usage(account_ref, &res_headers);
            self.note_verdict(account_ref, &verdict);
            let mut err = UpstreamError::new(verdict.kind, status, verdict.message);
            if let Some(reset) = verdict.reset_at_ms {
                err = err.with_reset_at_ms(reset);
            }
            return Err(err);
        }
    }

    /// `/v1/images/generations` 的一张：Responses 请求 + `image_generation` 工具，图片在
    /// `image_generation_call` 项的 `result` 里（base64）。
    async fn run_image(
        &self,
        credential: &Credential,
        request: &crate::images::ImageRequest,
    ) -> Result<crate::images::GeneratedImage, UpstreamError> {
        let account_ref = account_ref_from_token(&credential.access_token).ok_or_else(|| {
            UpstreamError::new(
                UpstreamKind::Auth,
                401,
                format!(
                    "{}：access token 里没有 chatgpt_account_id，不是 ChatGPT 订阅账号",
                    credential.label
                ),
            )
        })?;
        let namespace = protocol::identity_namespace(&account_ref);
        // 出图没有会话可粘：每张一个新会话键，别让几张图共用一个缓存键。
        let session_key = format!("image:{}", uuid::Uuid::new_v4().simple());
        let body = protocol::build_image_body(
            &request.prompt,
            &request.model,
            &protocol::ImageOptions {
                size: request.size.as_deref(),
                quality: request.quality.as_deref(),
                background: request.background.as_deref(),
                output_format: request.output_format.as_deref(),
                references: &request.references,
                mask: request.mask.as_deref(),
            },
            &namespace,
            &session_key,
        );
        let session_id = body
            .get("prompt_cache_key")
            .and_then(|v| v.as_str())
            .unwrap_or(&session_key)
            .to_string();
        let client_headers = protocol::forward_client_headers(
            &HashMap::new(),
            &namespace,
            HashMap::new(),
            self.cfg.identity_mode,
        );
        let headers = protocol::request_headers(
            &credential.access_token,
            &account_ref,
            &session_id,
            &client_headers,
        );

        let (res, _repairs) = self.send(credential, &account_ref, &headers, body).await?;
        self.note_usage(&account_ref, res.headers());
        let outcome = ImageCollector::default()
            .read(res, self.cfg.idle_timeout)
            .await;
        if let Err(e) = &outcome {
            if e.kind == UpstreamKind::Auth {
                self.note_verdict(
                    &account_ref,
                    &Verdict {
                        kind: e.kind,
                        message: e.message.clone(),
                        reset_at_ms: None,
                    },
                );
            }
        }
        outcome
    }

    async fn run(
        &self,
        credential: &Credential,
        request: &ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<Completion, UpstreamError> {
        let account_ref = account_ref_from_token(&credential.access_token).ok_or_else(|| {
            UpstreamError::new(
                UpstreamKind::Auth,
                401,
                format!(
                    "{}：access token 里没有 chatgpt_account_id，不是 ChatGPT 订阅账号",
                    credential.label
                ),
            )
        })?;
        let namespace = protocol::identity_namespace(&account_ref);
        let session_key = request
            .conversation_id
            .clone()
            .unwrap_or_else(|| credential.label.clone());
        let opts = protocol::PrepareOptions {
            model: &request.model,
            session_key: &session_key,
            namespace: &namespace,
            client_headers: &request.client_headers,
            identity_mode: self.cfg.identity_mode,
        };
        let passthrough = request.raw_responses.is_some();
        let Prepared {
            body,
            headers: client_headers,
            tool_aliases,
            model: upstream_model,
        } = match &request.raw_responses {
            Some(raw) => match raw.as_object() {
                Some(o) => protocol::build_passthrough_body(o, &opts),
                None => protocol::build_bridge_body(request, &opts),
            },
            None => protocol::build_bridge_body(request, &opts),
        };
        let session_id = body
            .get("prompt_cache_key")
            .and_then(|v| v.as_str())
            .unwrap_or(&session_key)
            .to_string();
        let headers = protocol::request_headers(
            &credential.access_token,
            &account_ref,
            &session_id,
            &client_headers,
        );
        let started = Instant::now();

        let (res, repairs) = self.send(credential, &account_ref, &headers, body).await?;
        self.note_usage(&account_ref, res.headers());
        // 上游的头到了就立刻告诉 server（哪怕没什么可转的）：客户端那一侧的响应头等的就是这一刻，
        // 不能等到流结束。只有透传客户端认得这些头，桥接的给空。
        let relay = if passthrough {
            protocol::relay_response_headers(res.headers(), &namespace)
        } else {
            Vec::new()
        };
        on_delta(Delta::Headers(relay));
        let mut reader = StreamReader::new(passthrough, tool_aliases, upstream_model, started);
        let outcome = reader.read(res, self.cfg.idle_timeout, on_delta).await;
        if let Err(e) = &outcome {
            if e.kind == UpstreamKind::Auth {
                self.note_verdict(
                    &account_ref,
                    &Verdict {
                        kind: e.kind,
                        message: e.message.clone(),
                        reset_at_ms: None,
                    },
                );
            }
        }
        if !repairs.is_empty() {
            tracing::debug!(repairs = %repairs.join("; "), "本次请求经过修正");
        }
        outcome
    }
}

impl Upstream for CodexUpstream {
    fn stream<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ChatRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
        Box::pin(self.run(credential, request, on_delta))
    }

    fn image<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a crate::images::ImageRequest,
    ) -> BoxFuture<'a, Result<crate::images::GeneratedImage, UpstreamError>> {
        Box::pin(self.run_image(credential, request))
    }
}

// ---------------------------------------------------------------------------
// 出图：收 image_generation_call
// ---------------------------------------------------------------------------

/// 读一次出图请求的流。只关心三样：`image_generation_call` 项（图在 `result`）、模型回的文本
/// （有文本没图 = 工具没被调用，得判断是拒绝还是没干活）、以及 `response.incomplete` 的原因。
#[derive(Default)]
struct ImageCollector {
    image: Option<crate::images::GeneratedImage>,
    text: String,
    incomplete_reason: Option<String>,
    completed: bool,
}

impl ImageCollector {
    async fn read(
        mut self,
        mut res: reqwest::Response,
        idle_timeout: Duration,
    ) -> Result<crate::images::GeneratedImage, UpstreamError> {
        let mut decoder = SseDecoder::default();
        loop {
            let chunk = match tokio::time::timeout(idle_timeout, res.chunk()).await {
                Ok(Ok(Some(c))) => c,
                Ok(Ok(None)) => break,
                Ok(Err(e)) => {
                    if self.completed {
                        break;
                    }
                    return Err(UpstreamError::new(
                        UpstreamKind::Upstream,
                        502,
                        format!("上游流中断：{e}"),
                    ));
                }
                Err(_) => {
                    return Err(UpstreamError::new(
                        UpstreamKind::Timeout,
                        504,
                        format!("上游 {} 秒没有动静", idle_timeout.as_secs()),
                    ));
                }
            };
            for ev in decoder.push(&chunk) {
                self.handle(ev)?;
            }
            if self.completed {
                break;
            }
        }
        for ev in decoder.finish() {
            self.handle(ev)?;
        }
        self.finish()
    }

    fn absorb_item(&mut self, item: &Value) {
        match item.get("type").and_then(|t| t.as_str()).unwrap_or("") {
            "image_generation_call" => {
                let Some(b64) = item
                    .get("result")
                    .and_then(|r| r.as_str())
                    .map(str::trim)
                    .filter(|r| !r.is_empty())
                else {
                    return;
                };
                if self.image.is_some() {
                    return;
                }
                let output_format = item.get("output_format").and_then(|f| f.as_str());
                let size = protocol::parse_size(item.get("size").and_then(|s| s.as_str()))
                    .or_else(|| crate::images::probe_dimensions_b64(b64));
                self.image = Some(crate::images::GeneratedImage {
                    b64: b64.to_string(),
                    mime: protocol::image_mime(output_format).to_string(),
                    size,
                    revised_prompt: item
                        .get("revised_prompt")
                        .and_then(|p| p.as_str())
                        .map(str::trim)
                        .filter(|p| !p.is_empty())
                        .map(str::to_string),
                });
            }
            "message" => {
                if let Some(parts) = item.get("content").and_then(|c| c.as_array()) {
                    for p in parts {
                        if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                            if !self.text.is_empty() {
                                self.text.push(' ');
                            }
                            self.text.push_str(t.trim());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle(&mut self, ev: SseEvent) -> Result<(), UpstreamError> {
        let data: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(_) => return Ok(()),
        };
        let kind = data
            .get("type")
            .and_then(|t| t.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| ev.event.clone());
        match kind.as_str() {
            "response.output_item.done" => {
                if let Some(item) = data.get("item") {
                    self.absorb_item(item);
                }
            }
            "response.completed" | "response.incomplete" => {
                if let Some(Value::Array(output)) = data.pointer("/response/output") {
                    for item in output {
                        self.absorb_item(item);
                    }
                }
                if kind == "response.incomplete" {
                    self.incomplete_reason = data
                        .pointer("/response/incomplete_details/reason")
                        .and_then(|r| r.as_str())
                        .map(str::to_string);
                }
                self.completed = true;
            }
            "response.failed" | "error" => {
                let code = data
                    .pointer("/response/error/code")
                    .or_else(|| data.get("code"))
                    .or_else(|| data.pointer("/error/code"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                let message = data
                    .pointer("/response/error/message")
                    .or_else(|| data.get("message"))
                    .or_else(|| data.pointer("/error/message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("");
                let v = protocol::classify_stream_error(code, message);
                return Err(UpstreamError::new(v.kind, status_for(v.kind), v.message));
            }
            _ => {}
        }
        Ok(())
    }

    fn finish(self) -> Result<crate::images::GeneratedImage, UpstreamError> {
        if let Some(img) = self.image {
            return Ok(img);
        }
        if self.incomplete_reason.as_deref() == Some("content_filter") {
            return Err(UpstreamError::new(
                UpstreamKind::BadRequest,
                400,
                "上游按内容策略拒绝了这次出图",
            ));
        }
        if !self.text.is_empty() {
            let shown: String = self.text.chars().take(300).collect();
            if protocol::looks_like_content_refusal(&self.text) {
                return Err(UpstreamError::new(
                    UpstreamKind::BadRequest,
                    400,
                    format!("上游按内容策略拒绝了这次出图：{shown}"),
                ));
            }
            // 模型答了话却没调工具：换号多半没用，但也不是请求的错——这个号的套餐可能没开出图。
            return Err(UpstreamError::new(
                UpstreamKind::ModelUnsupported,
                404,
                format!("上游没有执行出图，只回了文字：{shown}"),
            ));
        }
        if !self.completed {
            return Err(UpstreamError::new(
                UpstreamKind::Upstream,
                502,
                "上游流在 response.completed 之前结束",
            ));
        }
        Err(UpstreamError::new(
            UpstreamKind::Upstream,
            502,
            "上游没有返回图片",
        ))
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// access token 里的 `chatgpt_account_id`。
pub fn account_ref_from_token(token: &str) -> Option<String> {
    let claims = nexus_chatgpt::oauth::decode_jwt_claims(token)?;
    claims
        .get("https://api.openai.com/auth")?
        .get("chatgpt_account_id")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// 读流
// ---------------------------------------------------------------------------

struct StreamReader {
    passthrough: bool,
    tool_aliases: HashMap<String, String>,
    upstream_model: String,
    started: Instant,
    text: String,
    thinking: String,
    tool_calls: Vec<ToolCall>,
    /// 透传时每个 output item 是否已经交给客户端过（有些 item 只有 done 没有 delta）。
    text_deltas_seen: bool,
    usage: Option<Usage>,
    routed_model: Option<String>,
    finish: FinishReason,
    completed: bool,
    raw_response: Option<Value>,
    ttft_ms: Option<u64>,
}

impl StreamReader {
    fn new(
        passthrough: bool,
        tool_aliases: HashMap<String, String>,
        upstream_model: String,
        started: Instant,
    ) -> Self {
        Self {
            passthrough,
            tool_aliases,
            upstream_model,
            started,
            text: String::new(),
            thinking: String::new(),
            tool_calls: Vec::new(),
            text_deltas_seen: false,
            usage: None,
            routed_model: None,
            finish: FinishReason::Stop,
            completed: false,
            raw_response: None,
            ttft_ms: None,
        }
    }

    async fn read(
        &mut self,
        mut res: reqwest::Response,
        idle_timeout: Duration,
        on_delta: DeltaSink<'_>,
    ) -> Result<Completion, UpstreamError> {
        let mut decoder = SseDecoder::default();
        loop {
            let chunk = match tokio::time::timeout(idle_timeout, res.chunk()).await {
                Ok(Ok(Some(c))) => c,
                Ok(Ok(None)) => break,
                Ok(Err(e)) => {
                    if self.completed {
                        break;
                    }
                    return Err(UpstreamError::new(
                        UpstreamKind::Upstream,
                        502,
                        format!("上游流中断：{e}"),
                    ));
                }
                Err(_) => {
                    return Err(UpstreamError::new(
                        UpstreamKind::Timeout,
                        504,
                        format!("上游 {} 秒没有动静", idle_timeout.as_secs()),
                    ));
                }
            };
            for ev in decoder.push(&chunk) {
                self.handle(ev, &mut *on_delta)?;
            }
            if self.completed {
                break;
            }
        }
        for ev in decoder.finish() {
            self.handle(ev, &mut *on_delta)?;
        }
        self.finish_completion()
    }

    fn mark_first_byte(&mut self) {
        if self.ttft_ms.is_none() {
            self.ttft_ms = Some(self.started.elapsed().as_millis() as u64);
        }
    }

    fn restore_item_names(&self, item: &mut Value) {
        let Some(o) = item.as_object_mut() else {
            return;
        };
        let ty = o.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty == "function_call" || ty == "custom_tool_call" {
            if let Some(Value::String(name)) = o.get("name").cloned() {
                let restored = protocol::restore_tool_name(&name, &self.tool_aliases);
                if restored != name {
                    o.insert("name".into(), Value::String(restored));
                }
            }
        }
    }

    fn handle(&mut self, ev: SseEvent, on_delta: DeltaSink<'_>) -> Result<(), UpstreamError> {
        let mut data: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(_) => {
                // 非 JSON 的帧（不该有）：透传原样，桥接忽略。
                if self.passthrough {
                    on_delta(Delta::Raw {
                        event: ev.event,
                        data: ev.data,
                    });
                }
                return Ok(());
            }
        };
        let kind = data
            .get("type")
            .and_then(|t| t.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| ev.event.clone());

        match kind.as_str() {
            "response.created" | "response.in_progress" => {
                if let Some(m) = data.pointer("/response/model").and_then(|m| m.as_str()) {
                    self.routed_model = Some(m.to_string());
                }
            }
            "response.output_text.delta" => {
                let delta = data
                    .get("delta")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                if !delta.is_empty() {
                    self.mark_first_byte();
                    self.text_deltas_seen = true;
                    self.text.push_str(&delta);
                    if !self.passthrough {
                        on_delta(Delta::Text(delta));
                    }
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let delta = data
                    .get("delta")
                    .and_then(|d| d.as_str())
                    .unwrap_or("")
                    .to_string();
                if !delta.is_empty() {
                    self.mark_first_byte();
                    self.thinking.push_str(&delta);
                    if !self.passthrough {
                        on_delta(Delta::Thinking(delta));
                    }
                }
            }
            "response.output_item.added" | "response.output_item.done" => {
                if let Some(item) = data.get_mut("item") {
                    self.restore_item_names(item);
                }
                if kind == "response.output_item.done" {
                    if let Some(item) = data.get("item").cloned() {
                        self.absorb_item(&item, &mut *on_delta);
                    }
                }
            }
            "response.completed" | "response.incomplete" => {
                if let Some(u) = data.pointer("/response/usage") {
                    self.usage = Some(parse_usage(u));
                }
                if let Some(m) = data.pointer("/response/model").and_then(|m| m.as_str()) {
                    self.routed_model = Some(m.to_string());
                }
                if kind == "response.incomplete" {
                    let reason = data
                        .pointer("/response/incomplete_details/reason")
                        .and_then(|r| r.as_str())
                        .unwrap_or("");
                    self.finish = if reason == "content_filter" {
                        FinishReason::ContentFilter
                    } else {
                        FinishReason::Length
                    };
                }
                if let Some(Value::Array(output)) = data.pointer_mut("/response/output") {
                    for item in output.iter_mut() {
                        self.restore_item_names(item);
                    }
                }
                self.completed = true;
                self.raw_response = data.get("response").cloned();
            }
            "response.failed" => {
                let code = data
                    .pointer("/response/error/code")
                    .and_then(|c| c.as_str())
                    .unwrap_or("");
                let message = data
                    .pointer("/response/error/message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("");
                let v = protocol::classify_stream_error(code, message);
                return Err(UpstreamError::new(v.kind, status_for(v.kind), v.message));
            }
            "error" => {
                let code = data
                    .get("code")
                    .and_then(|c| c.as_str())
                    .or_else(|| data.pointer("/error/code").and_then(|c| c.as_str()))
                    .unwrap_or("");
                let message = data
                    .get("message")
                    .and_then(|m| m.as_str())
                    .or_else(|| data.pointer("/error/message").and_then(|m| m.as_str()))
                    .unwrap_or("");
                let v = protocol::classify_stream_error(code, message);
                return Err(UpstreamError::new(v.kind, status_for(v.kind), v.message));
            }
            _ => {}
        }

        if self.passthrough {
            on_delta(Delta::Raw {
                event: if ev.event.is_empty() { kind } else { ev.event },
                data: data.to_string(),
            });
        }
        Ok(())
    }

    /// `output_item.done` 的整项：工具调用在这里收；桥接时没有增量的短回答也从这里补。
    fn absorb_item(&mut self, item: &Value, on_delta: DeltaSink<'_>) {
        let ty = item.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match ty {
            "function_call" | "custom_tool_call" => {
                let name = item
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("tool")
                    .to_string();
                let id = item
                    .get("call_id")
                    .and_then(|c| c.as_str())
                    .or_else(|| item.get("id").and_then(|c| c.as_str()))
                    .unwrap_or("")
                    .to_string();
                let arguments = if ty == "custom_tool_call" {
                    let input = item
                        .get("input")
                        .cloned()
                        .unwrap_or(Value::String(String::new()));
                    serde_json::json!({ "input": input }).to_string()
                } else {
                    match item.get("arguments") {
                        Some(Value::String(s)) => s.clone(),
                        Some(other) => other.to_string(),
                        None => "{}".to_string(),
                    }
                };
                self.mark_first_byte();
                self.tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments,
                });
            }
            "message" => {
                let full: String = item
                    .get("content")
                    .and_then(|c| c.as_array())
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                            .collect::<Vec<_>>()
                            .join("")
                    })
                    .unwrap_or_default();
                // 短回答可能不发增量只在 done 里给整段。
                if !full.is_empty() && !self.text_deltas_seen && self.text.is_empty() {
                    self.mark_first_byte();
                    self.text.push_str(&full);
                    if !self.passthrough {
                        on_delta(Delta::Text(full));
                    }
                }
            }
            _ => {}
        }
    }

    fn finish_completion(&mut self) -> Result<Completion, UpstreamError> {
        if !self.completed
            && self.text.is_empty()
            && self.thinking.is_empty()
            && self.tool_calls.is_empty()
        {
            return Err(UpstreamError::new(
                UpstreamKind::Upstream,
                502,
                "上游流在 response.completed 之前结束",
            ));
        }
        let usage = self.usage.unwrap_or_default();
        let measured = self.usage.is_some();
        if !self.tool_calls.is_empty() && self.finish == FinishReason::Stop {
            self.finish = FinishReason::ToolCalls;
        }
        if self.finish == FinishReason::Stop
            && self.text.is_empty()
            && self.thinking.is_empty()
            && self.tool_calls.is_empty()
        {
            let has_tokens = usage.input_tokens > 0 || usage.output_tokens > 0;
            let has_items = self
                .raw_response
                .as_ref()
                .and_then(|r| r.get("output"))
                .and_then(|o| o.as_array())
                .is_some_and(|a| !a.is_empty());
            if !has_tokens && !has_items {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    502,
                    "上游返回了空流：无内容、无工具调用、无用量",
                ));
            }
        }
        Ok(Completion {
            text: std::mem::take(&mut self.text),
            thinking: std::mem::take(&mut self.thinking),
            tool_calls: std::mem::take(&mut self.tool_calls),
            finish_reason: self.finish,
            usage,
            usage_measured: measured,
            routed_model: self
                .routed_model
                .take()
                .or_else(|| Some(self.upstream_model.clone())),
            ttft_ms: self.ttft_ms,
            turn_ms: self.started.elapsed().as_millis() as u64,
            raw_response: if self.passthrough {
                self.raw_response.take()
            } else {
                None
            },
        })
    }
}

fn status_for(kind: UpstreamKind) -> u16 {
    match kind {
        UpstreamKind::Auth => 401,
        UpstreamKind::Forbidden => 403,
        UpstreamKind::Quota => 402,
        UpstreamKind::RateLimit | UpstreamKind::Provider => 429,
        UpstreamKind::BadRequest => 400,
        UpstreamKind::ModelUnsupported => 404,
        UpstreamKind::Canceled => 499,
        UpstreamKind::Timeout => 504,
        UpstreamKind::Upstream => 502,
    }
}

fn parse_usage(u: &Value) -> Usage {
    let n = |p: &str| u.pointer(p).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    Usage {
        input_tokens: n("/input_tokens"),
        output_tokens: n("/output_tokens"),
        cache_read_tokens: n("/input_tokens_details/cached_tokens"),
        cache_write_tokens: 0,
        reasoning_tokens: n("/output_tokens_details/reasoning_tokens"),
    }
}

#[cfg(test)]
mod unit {
    use super::*;

    #[test]
    fn sse_decoder_splits_frames_and_joins_multiline_data() {
        let mut d = SseDecoder::default();
        let evs = d.push(b"event: a\ndata: {\"x\":1}\n\nevent: b\ndata: line1\ndata: line2\n\n: keepalive\n\ndata: tail");
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].event, "a");
        assert_eq!(evs[0].data, "{\"x\":1}");
        assert_eq!(evs[1].data, "line1\nline2");
        let rest = d.finish();
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].data, "tail");
        let mut d2 = SseDecoder::default();
        let evs = d2.push(b"event: x\r\ndata: 1\r\n\r\n");
        assert_eq!(evs[0].data, "1");
    }

    #[test]
    fn account_ref_comes_from_the_auth_claim() {
        use base64::Engine;
        let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes());
        let tok = format!(
            "{}.{}.sig",
            b64("{}"),
            b64(r#"{"https://api.openai.com/auth":{"chatgpt_account_id":" acct_9 "}}"#)
        );
        assert_eq!(account_ref_from_token(&tok).as_deref(), Some("acct_9"));
        assert_eq!(account_ref_from_token("not-a-jwt"), None);
    }

    #[test]
    fn usage_is_read_from_the_responses_shape() {
        let u = parse_usage(&serde_json::json!({
            "input_tokens": 120, "output_tokens": 30,
            "input_tokens_details": { "cached_tokens": 100 },
            "output_tokens_details": { "reasoning_tokens": 12 }
        }));
        assert_eq!(u.input_tokens, 120);
        assert_eq!(u.cache_read_tokens, 100);
        assert_eq!(u.reasoning_tokens, 12);
    }
}
