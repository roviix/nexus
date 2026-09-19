//! Grok 通道的后端：聊天（Responses）、生图 / 改图、生视频。
//!
//! 两种凭证走两个地方（见 `nexus_grok::protocol`）：订阅号的文本走 `cli-chat-proxy.grok.com`
//! 带 Grok CLI 身份头，API Key 的走 `api.x.ai`；媒体两种都走 `api.x.ai`。凭证是哪种从 token
//! 本身看：`xai-` 开头是 key，其余是 JWT。
//!
//! 聊天比 Codex 后端瘦：没有身份收敛、没有 compaction。Responses 入站尽量原样透传（只删上游会拒
//! 的字段）；Chat / Anthropic 入站从中间表示拼一份完整的 Responses 体——**带工具、带图、带
//! 推理档位**，和 Codex 桥接是同一套映射。回程认 `function_call` / `reasoning` / `usage` /
//! `response.completed`，非流式把上游最终的 response 对象原样交出去。
//!
//! 每次响应把 `x-ratelimit-*` / `retry-after` 喂回账号服务的额度快照；媒体请求撞回 402 / 403
//! 就把这个号记成「出不了媒体」（聊天不受影响）；426 是我们的客户端版本旧了，换号无用，直接
//! 把上游原话给用户。

use crate::error::{UpstreamError, UpstreamKind};
use crate::images::{measure, GeneratedImage, ImageRequest};
use crate::inference::{DEFAULT_IDLE_TIMEOUT, DEFAULT_MAX_TURN};
use crate::lane::{BoxFuture, Credential};
use crate::media::{VideoJob, VideoOp, VideoRequest, VideoStatus};
use crate::normalized::{
    estimate_tokens, ChatRequest, Completion, Delta, FinishReason, Role, ToolCall, ToolChoice,
    Usage,
};
use crate::sse::{SseDecoder, SseEvent};
use crate::upstream::{DeltaSink, Upstream};
use base64::Engine as _;
use nexus_grok::{
    api_key_chat_headers, chat_headers, looks_like_api_key, media_headers, split_route_prefix,
    upstream_image_model, upstream_video_model, GrokService, CLI_CHAT_PROXY, XAI_API,
};
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

const STRIP: &[&str] = &[
    "previous_response_id",
    "prompt_cache_retention",
    "safety_identifier",
    "stream_options",
    "service_tier",
    "user",
];

/// 出一张图的等待上限。Imagine 一般十几秒；官方客户端给 300 秒。
const IMAGE_TIMEOUT: Duration = Duration::from_secs(300);
const VIDEO_START_TIMEOUT: Duration = Duration::from_secs(60);
const VIDEO_POLL_TIMEOUT: Duration = Duration::from_secs(30);

pub struct GrokUpstream {
    client: reqwest::Client,
    chat_base: String,
    api_base: String,
    idle_timeout: Duration,
    max_turn: Duration,
    /// 有账号服务时把额度头、媒体判决写回去；测试可不带。
    accounts: Option<Arc<GrokService>>,
}

impl GrokUpstream {
    pub fn new(accounts: Option<Arc<GrokService>>) -> Self {
        Self {
            client: crate::inference::http_client(),
            chat_base: CLI_CHAT_PROXY.to_string(),
            api_base: XAI_API.to_string(),
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
            accounts,
        }
    }

    #[cfg(test)]
    pub fn with_bases(chat_base: String, api_base: String) -> Self {
        Self {
            client: crate::inference::http_client(),
            chat_base,
            api_base,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
            accounts: None,
        }
    }

    fn is_api_key(credential: &Credential) -> bool {
        looks_like_api_key(&credential.access_token)
    }

    fn chat_url(&self, credential: &Credential) -> String {
        let base = if Self::is_api_key(credential) {
            &self.api_base
        } else {
            &self.chat_base
        };
        format!("{}/responses", base.trim_end_matches('/'))
    }

    fn media_url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.api_base.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    fn absorb(&self, credential: &Credential, headers: &[(String, String)]) {
        if let Some(acc) = &self.accounts {
            acc.absorb_headers(&credential.label, headers);
        }
    }

    fn media_verdict(&self, credential: &Credential, eligible: bool) {
        if let Some(acc) = &self.accounts {
            acc.record_media_outcome(&credential.label, eligible);
        }
    }

    async fn run(
        &self,
        credential: &Credential,
        request: &ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<Completion, UpstreamError> {
        let passthrough = request.raw_responses.is_some();
        let (name, _) = split_route_prefix(&request.model);
        let model = name.trim().to_string();
        let session = request
            .conversation_id
            .clone()
            .unwrap_or_else(|| credential.label.clone());
        let body = match &request.raw_responses {
            Some(raw) => match raw.as_object() {
                Some(o) => prepare_passthrough(o, &model, &session),
                None => build_bridge(request, &model, &session),
            },
            None => build_bridge(request, &model, &session),
        };
        let payload = serde_json::to_vec(&Value::Object(body)).map_err(|e| {
            UpstreamError::new(
                UpstreamKind::BadRequest,
                400,
                format!("请求体无法序列化：{e}"),
            )
        })?;
        let mut req = self
            .client
            .post(self.chat_url(credential))
            .timeout(self.max_turn)
            .body(payload);
        let headers = if Self::is_api_key(credential) {
            api_key_chat_headers(&credential.access_token)
        } else {
            chat_headers(&credential.access_token, &session)
        };
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let res = match req.send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                return Err(UpstreamError::new(
                    UpstreamKind::Timeout,
                    504,
                    format!("连 Grok 超时：{e}"),
                ));
            }
            Err(e) => {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    502,
                    format!("连不上 Grok：{e}"),
                ));
            }
        };
        let status = res.status().as_u16();
        let relayed = quota_headers(res.headers());
        self.absorb(credential, &relayed);
        if !(200..300).contains(&status) {
            let text = res.text().await.unwrap_or_default();
            return Err(map_status(status, &text));
        }
        on_delta(Delta::Headers(relayed));
        let started = Instant::now();
        read_sse(res, passthrough, started, self.idle_timeout, on_delta).await
    }

    // ---------- 生图 ----------

    async fn draw(
        &self,
        credential: &Credential,
        request: &ImageRequest,
    ) -> Result<GeneratedImage, UpstreamError> {
        let editing = !request.references.is_empty();
        let model = if editing
            && (request.model.trim().is_empty() || request.model.eq_ignore_ascii_case("auto"))
        {
            nexus_grok::DEFAULT_IMAGE_EDIT_MODEL.to_string()
        } else {
            upstream_image_model(&request.model)
        };
        let body = build_image_body(request, &model);
        let path = if editing {
            "images/edits"
        } else {
            "images/generations"
        };
        let mut req = self
            .client
            .post(self.media_url(path))
            .timeout(IMAGE_TIMEOUT)
            .json(&body);
        for (k, v) in media_headers(
            &credential.access_token,
            !Self::is_api_key(credential),
            None,
        ) {
            req = req.header(k, v);
        }
        let res = match req.send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                return Err(UpstreamError::new(
                    UpstreamKind::Timeout,
                    504,
                    format!("Grok 出图超时：{e}"),
                ));
            }
            Err(e) => {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    502,
                    format!("连不上 xAI：{e}"),
                ));
            }
        };
        let status = res.status().as_u16();
        let relayed = quota_headers(res.headers());
        self.absorb(credential, &relayed);
        let text = res.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            let err = map_media_status(status, &text);
            if matches!(err.kind, UpstreamKind::Forbidden | UpstreamKind::Quota) {
                self.media_verdict(credential, false);
            }
            return Err(err);
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| {
            UpstreamError::new(
                UpstreamKind::Upstream,
                502,
                format!("xAI 出图响应不是 JSON：{e}"),
            )
        })?;
        let first = v
            .get("data")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .cloned();
        let Some(item) = first else {
            // 空 200：sub2api 线上见过——付费判断错了的号会「成功」却什么也不给。当作这个号出不了媒体，换号。
            self.media_verdict(credential, false);
            return Err(UpstreamError::new(
                UpstreamKind::Forbidden,
                403,
                "xAI 回了 200 但没有图片；这个号可能没有 Imagine 权益",
            ));
        };
        let revised = item
            .get("revised_prompt")
            .and_then(Value::as_str)
            .map(str::to_string);
        let (b64, mime) = match item.get("b64_json").and_then(Value::as_str) {
            Some(b) if !b.is_empty() => (b.to_string(), "image/png".to_string()),
            _ => match item.get("url").and_then(Value::as_str) {
                Some(url) if !url.is_empty() => fetch_as_b64(&self.client, url).await?,
                _ => {
                    self.media_verdict(credential, false);
                    return Err(UpstreamError::new(
                        UpstreamKind::Forbidden,
                        403,
                        "xAI 出图响应里既没有 b64_json 也没有 url",
                    ));
                }
            },
        };
        self.media_verdict(credential, true);
        let size = base64::engine::general_purpose::STANDARD
            .decode(&b64)
            .ok()
            .and_then(|bytes| measure(&bytes));
        Ok(GeneratedImage {
            b64,
            mime,
            size,
            revised_prompt: revised,
        })
    }

    // ---------- 生视频 ----------

    async fn start_video(
        &self,
        credential: &Credential,
        request: &VideoRequest,
    ) -> Result<VideoJob, UpstreamError> {
        let op = request.op.unwrap_or(VideoOp::Generate);
        let path = match op {
            VideoOp::Generate => "videos/generations",
            VideoOp::Edit => "videos/edits",
            VideoOp::Extend => "videos/extensions",
        };
        let body = build_video_body(request);
        let mut req = self
            .client
            .post(self.media_url(path))
            .timeout(VIDEO_START_TIMEOUT)
            .json(&body);
        for (k, v) in media_headers(
            &credential.access_token,
            !Self::is_api_key(credential),
            None,
        ) {
            req = req.header(k, v);
        }
        let res = req.send().await.map_err(|e| {
            UpstreamError::new(UpstreamKind::Upstream, 502, format!("连不上 xAI：{e}"))
        })?;
        let status = res.status().as_u16();
        let relayed = quota_headers(res.headers());
        self.absorb(credential, &relayed);
        let text = res.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            let err = map_media_status(status, &text);
            if matches!(err.kind, UpstreamKind::Forbidden | UpstreamKind::Quota) {
                self.media_verdict(credential, false);
            }
            return Err(err);
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| {
            UpstreamError::new(
                UpstreamKind::Upstream,
                502,
                format!("xAI 视频响应不是 JSON：{e}"),
            )
        })?;
        let id = v
            .get("request_id")
            .or_else(|| v.get("id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                UpstreamError::new(UpstreamKind::Upstream, 502, "xAI 没有返回 request_id")
            })?;
        self.media_verdict(credential, true);
        Ok(VideoJob {
            request_id: id.to_string(),
        })
    }

    async fn poll_video(
        &self,
        credential: &Credential,
        request_id: &str,
    ) -> Result<VideoStatus, UpstreamError> {
        let mut req = self
            .client
            .get(self.media_url(&format!("videos/{request_id}")))
            .timeout(VIDEO_POLL_TIMEOUT);
        for (k, v) in media_headers(
            &credential.access_token,
            !Self::is_api_key(credential),
            None,
        ) {
            req = req.header(k, v);
        }
        let res = req.send().await.map_err(|e| {
            UpstreamError::new(UpstreamKind::Upstream, 502, format!("连不上 xAI：{e}"))
        })?;
        let status = res.status().as_u16();
        let text = res.text().await.unwrap_or_default();
        // 202 = 还在做。有些实现给 202 + 空体。
        if status == 202 && text.trim().is_empty() {
            return Ok(VideoStatus {
                status: "pending".into(),
                ..Default::default()
            });
        }
        if !(200..300).contains(&status) {
            return Err(map_media_status(status, &text));
        }
        let v: Value = serde_json::from_str(&text).map_err(|e| {
            UpstreamError::new(
                UpstreamKind::Upstream,
                502,
                format!("xAI 视频状态不是 JSON：{e}"),
            )
        })?;
        Ok(parse_video_status(&v))
    }
}

impl Default for GrokUpstream {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Upstream for GrokUpstream {
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
        request: &'a ImageRequest,
    ) -> BoxFuture<'a, Result<GeneratedImage, UpstreamError>> {
        Box::pin(self.draw(credential, request))
    }

    fn video_start<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a VideoRequest,
    ) -> BoxFuture<'a, Result<VideoJob, UpstreamError>> {
        Box::pin(self.start_video(credential, request))
    }

    fn video_status<'a>(
        &'a self,
        credential: &'a Credential,
        request_id: &'a str,
    ) -> BoxFuture<'a, Result<VideoStatus, UpstreamError>> {
        Box::pin(self.poll_video(credential, request_id))
    }
}

/// 响应头里值得留下的：限流窗口、恢复时刻。挂到客户端响应上，也喂给额度快照。
pub fn quota_headers(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(k, v)| {
            let key = k.as_str().to_ascii_lowercase();
            (key.starts_with("x-ratelimit-") || key == "retry-after")
                .then(|| v.to_str().ok().map(|s| (key.clone(), s.to_string())))
                .flatten()
        })
        .collect()
}

pub fn prepare_passthrough(
    raw: &Map<String, Value>,
    model: &str,
    session: &str,
) -> Map<String, Value> {
    let mut body = raw.clone();
    for k in STRIP {
        body.remove(*k);
    }
    if !model.is_empty() {
        body.insert("model".into(), json!(model));
    }
    body.insert("stream".into(), json!(true));
    if !session.trim().is_empty() {
        body.entry("prompt_cache_key")
            .or_insert_with(|| json!(session));
    }
    body
}

/// 档位后缀（`grok-4.5-high`）→ `reasoning.effort`。Grok 认 `low` / `high`；`medium` 也收。
fn split_effort(model: &str) -> (String, Option<&'static str>) {
    for e in ["low", "medium", "high"] {
        if let Some(head) = model.strip_suffix(&format!("-{e}")) {
            if !head.is_empty() {
                return (head.to_string(), Some(e));
            }
        }
    }
    (model.to_string(), None)
}

/// 中间表示 → Responses 体。system 并进 `instructions`；工具、图片、工具调用与结果都带上。
pub fn build_bridge(request: &ChatRequest, model: &str, session: &str) -> Map<String, Value> {
    let (model, effort) = split_effort(model);
    let mut instructions: Vec<String> = Vec::new();
    let mut input: Vec<Value> = Vec::new();
    for m in &request.messages {
        match m.role {
            Role::System => {
                if !m.text.trim().is_empty() {
                    instructions.push(m.text.clone());
                }
            }
            Role::User => {
                let mut content: Vec<Value> = Vec::new();
                if !m.text.trim().is_empty() || m.images.is_empty() {
                    content.push(json!({ "type": "input_text", "text": m.text }));
                }
                for img in &m.images {
                    content.push(json!({
                        "type": "input_image",
                        "image_url": format!("data:{};base64,{}", img.mime_type, img.data),
                        "detail": "auto",
                    }));
                }
                input.push(json!({ "type": "message", "role": "user", "content": content }));
            }
            Role::Assistant => {
                if !m.text.trim().is_empty() {
                    input.push(json!({
                        "type": "message", "role": "assistant",
                        "content": [{ "type": "output_text", "text": m.text }],
                    }));
                }
                for c in &m.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": if c.id.is_empty() { "call_0" } else { c.id.as_str() },
                        "name": if c.name.is_empty() { "tool" } else { c.name.as_str() },
                        "arguments": if c.arguments.is_empty() { "{}" } else { c.arguments.as_str() },
                    }));
                }
            }
            Role::Tool => {
                for r in &m.tool_results {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": if r.tool_call_id.is_empty() { "call_0" } else { r.tool_call_id.as_str() },
                        "output": if r.is_error { format!("[error] {}", r.text) } else { r.text.clone() },
                    }));
                }
                if m.tool_results.is_empty() && !m.text.is_empty() {
                    input.push(json!({ "type": "message", "role": "user",
                                       "content": [{ "type": "input_text", "text": format!("[tool] {}", m.text) }] }));
                }
            }
        }
    }
    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert("stream".into(), json!(true));
    body.insert("store".into(), json!(false));
    if !instructions.is_empty() {
        body.insert("instructions".into(), json!(instructions.join("\n\n")));
    }
    body.insert("input".into(), Value::Array(input));
    if !session.trim().is_empty() {
        body.insert("prompt_cache_key".into(), json!(session));
    }
    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                    "strict": false,
                })
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        let choice = match &request.tool_choice {
            ToolChoice::Auto => json!("auto"),
            ToolChoice::None => json!("none"),
            ToolChoice::Required => json!("required"),
            ToolChoice::Tool(name) => json!({ "type": "function", "name": name }),
        };
        body.insert("tool_choice".into(), choice);
    }
    if let Some(max) = request.sampling.max_output_tokens {
        body.insert("max_output_tokens".into(), json!(max));
    }
    if let Some(t) = request.sampling.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if let Some(p) = request.sampling.top_p {
        body.insert("top_p".into(), json!(p));
    }
    if let Some(e) = effort {
        body.insert(
            "reasoning".into(),
            json!({ "effort": e, "summary": "auto" }),
        );
    }
    body
}

/// OpenAI images 请求 → xAI Imagine 请求。`size` 换算成 `aspect_ratio`；`quality: hd` 换成
/// 质量档模型；参考图与遮罩按 xAI 的 `image` / `images` / `mask` 对象形状。
pub fn build_image_body(request: &ImageRequest, model: &str) -> Value {
    let mut body = json!({
        "model": model,
        "prompt": request.prompt,
        "n": 1,
        "response_format": "b64_json",
    });
    let obj = body.as_object_mut().expect("json 对象");
    if let Some(size) = &request.size {
        if let Some(ar) = crate::media::size_to_aspect(size) {
            obj.insert("aspect_ratio".into(), json!(ar));
        }
        let lower = size.to_ascii_lowercase();
        if lower.contains("2048") || lower.contains("2k") {
            obj.insert("resolution".into(), json!("2k"));
        } else {
            obj.insert("resolution".into(), json!("1k"));
        }
    } else {
        obj.insert("resolution".into(), json!("1k"));
    }
    if request
        .quality
        .as_deref()
        .is_some_and(|q| q.eq_ignore_ascii_case("hd") || q.eq_ignore_ascii_case("high"))
        && model == "grok-imagine-image"
    {
        obj.insert("model".into(), json!("grok-imagine-image-quality"));
    }
    if let Some(fmt) = &request.output_format {
        let f = fmt.to_ascii_lowercase();
        if matches!(f.as_str(), "png" | "jpeg" | "jpg" | "webp") {
            obj.insert(
                "output_format".into(),
                json!(if f == "jpg" { "jpeg" } else { f.as_str() }),
            );
        }
    }
    match request.references.len() {
        0 => {}
        1 => {
            obj.insert("image".into(), json!({ "url": request.references[0] }));
        }
        _ => {
            let imgs: Vec<Value> = request
                .references
                .iter()
                .take(3)
                .map(|u| json!({ "url": u }))
                .collect();
            obj.insert("images".into(), Value::Array(imgs));
        }
    }
    if let Some(mask) = &request.mask {
        obj.insert("mask".into(), json!({ "url": mask }));
    }
    body
}

pub fn build_video_body(request: &VideoRequest) -> Value {
    let mut body = json!({ "model": upstream_video_model(&request.model) });
    let obj = body.as_object_mut().expect("json 对象");
    if let Some(p) = &request.prompt {
        obj.insert("prompt".into(), json!(p));
    }
    if let Some(img) = &request.image {
        obj.insert("image".into(), json!({ "url": img }));
    }
    if !request.reference_images.is_empty() {
        let refs: Vec<Value> = request
            .reference_images
            .iter()
            .map(|u| json!({ "url": u }))
            .collect();
        obj.insert("reference_images".into(), Value::Array(refs));
    }
    if let Some(v) = &request.video {
        obj.insert("video".into(), json!({ "url": v }));
    }
    if let Some(d) = request.duration {
        obj.insert("duration".into(), json!(d));
    }
    if let Some(a) = &request.aspect_ratio {
        obj.insert("aspect_ratio".into(), json!(a));
    }
    if let Some(r) = &request.resolution {
        obj.insert("resolution".into(), json!(r));
    }
    body
}

pub fn parse_video_status(v: &Value) -> VideoStatus {
    let status = v
        .get("status")
        .and_then(Value::as_str)
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_else(|| "pending".into());
    let video = v.get("video");
    VideoStatus {
        video_url: video
            .and_then(|x| x.get("url"))
            .or_else(|| v.get("url"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        duration_secs: video
            .and_then(|x| x.get("duration"))
            .and_then(Value::as_f64)
            .map(|d| d.round() as u32),
        resolution: video
            .and_then(|x| x.get("resolution"))
            .and_then(Value::as_str)
            .map(str::to_string),
        error: v
            .pointer("/error/message")
            .or_else(|| v.get("error"))
            .and_then(Value::as_str)
            .map(str::to_string),
        status,
        raw: Some(v.clone()),
    }
}

async fn fetch_as_b64(
    client: &reqwest::Client,
    url: &str,
) -> Result<(String, String), UpstreamError> {
    let res = client
        .get(url)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|e| {
            UpstreamError::new(UpstreamKind::Upstream, 502, format!("下载图片失败：{e}"))
        })?;
    if !res.status().is_success() {
        return Err(UpstreamError::new(
            UpstreamKind::Upstream,
            502,
            format!("下载图片 HTTP {}", res.status().as_u16()),
        ));
    }
    let mime = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/png")
        .to_string();
    let bytes = res.bytes().await.map_err(|e| {
        UpstreamError::new(UpstreamKind::Upstream, 502, format!("读取图片失败：{e}"))
    })?;
    Ok((
        base64::engine::general_purpose::STANDARD.encode(&bytes),
        mime,
    ))
}

fn map_status(status: u16, text: &str) -> UpstreamError {
    let head: String = text.chars().take(240).collect();
    let lower = text.to_ascii_lowercase();
    let kind = match status {
        401 => UpstreamKind::Auth,
        402 => UpstreamKind::Quota,
        403 => UpstreamKind::Forbidden,
        429 => UpstreamKind::RateLimit,
        400 => UpstreamKind::BadRequest,
        404 if lower.contains("model") => UpstreamKind::ModelUnsupported,
        // 426：我们冒充的 Grok CLI 版本旧了。换号无用；要升 `nexus_grok::protocol::CLIENT_VERSION`。
        426 => {
            return UpstreamError::new(
                UpstreamKind::Upstream,
                426,
                format!("Grok CLI 版本头过旧，上游拒收（换账号无用，需要更新 Nexus）：{head}"),
            );
        }
        _ => UpstreamKind::Upstream,
    };
    let mut err = UpstreamError::new(kind, status, format!("Grok {status}：{head}"));
    if let Some(secs) = retry_after_in(text) {
        err = err.with_reset_at_ms(crate::media::now_ms() + secs * 1000);
    }
    err
}

/// 媒体端点的错误：402 / 403 是「这个号没有 Imagine 权益」，让选路换号并把号记下来。
fn map_media_status(status: u16, text: &str) -> UpstreamError {
    let head: String = text.chars().take(240).collect();
    match status {
        401 => UpstreamError::new(UpstreamKind::Auth, 401, format!("xAI 401：{head}")),
        402 | 403 => UpstreamError::new(
            UpstreamKind::Forbidden,
            403,
            format!("这个号出不了 Imagine 媒体（xAI {status}）：{head}"),
        ),
        429 => UpstreamError::new(UpstreamKind::RateLimit, 429, format!("xAI 429：{head}")),
        400 | 413 | 422 => UpstreamError::new(
            UpstreamKind::BadRequest,
            400,
            format!("xAI {status}：{head}"),
        ),
        404 => UpstreamError::new(UpstreamKind::Upstream, 404, format!("xAI 404：{head}")),
        _ => UpstreamError::new(UpstreamKind::Upstream, 502, format!("xAI {status}：{head}")),
    }
}

fn retry_after_in(text: &str) -> Option<i64> {
    let v: Value = serde_json::from_str(text).ok()?;
    v.pointer("/error/retry_after")
        .or_else(|| v.get("retry_after"))
        .and_then(Value::as_i64)
}

// ---------- SSE ----------

struct Collected {
    text: String,
    thinking: String,
    tool_calls: Vec<ToolCall>,
    usage: Option<Usage>,
    completed: bool,
    incomplete_reason: Option<String>,
    raw_response: Option<Value>,
    ttft: Option<u64>,
}

async fn read_sse(
    mut res: reqwest::Response,
    passthrough: bool,
    started: Instant,
    idle: Duration,
    on_delta: DeltaSink<'_>,
) -> Result<Completion, UpstreamError> {
    let mut decoder = SseDecoder::default();
    let mut c = Collected {
        text: String::new(),
        thinking: String::new(),
        tool_calls: Vec::new(),
        usage: None,
        completed: false,
        incomplete_reason: None,
        raw_response: None,
        ttft: None,
    };
    loop {
        let chunk = match tokio::time::timeout(idle, res.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                if c.completed {
                    break;
                }
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    502,
                    format!("Grok 流中断：{e}"),
                ));
            }
            Err(_) => {
                return Err(UpstreamError::new(
                    UpstreamKind::Timeout,
                    504,
                    format!("Grok {} 秒没有动静", idle.as_secs()),
                ));
            }
        };
        for ev in decoder.push(&chunk) {
            handle_event(&ev, passthrough, &mut c, started, on_delta)?;
        }
        if c.completed {
            break;
        }
    }
    for ev in decoder.finish() {
        handle_event(&ev, passthrough, &mut c, started, on_delta)?;
    }
    let usage_measured = c.usage.is_some();
    let usage = c.usage.unwrap_or(Usage {
        input_tokens: 0,
        output_tokens: estimate_tokens(&c.text),
        ..Default::default()
    });
    let finish_reason = if !c.tool_calls.is_empty() {
        FinishReason::ToolCalls
    } else if c.incomplete_reason.as_deref() == Some("max_output_tokens") {
        FinishReason::Length
    } else if c.incomplete_reason.as_deref() == Some("content_filter") {
        FinishReason::ContentFilter
    } else {
        FinishReason::Stop
    };
    Ok(Completion {
        text: c.text,
        thinking: c.thinking,
        tool_calls: c.tool_calls,
        finish_reason,
        usage,
        usage_measured,
        routed_model: c
            .raw_response
            .as_ref()
            .and_then(|r| r.get("model"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| Some("grok".into())),
        ttft_ms: c.ttft,
        turn_ms: started.elapsed().as_millis() as u64,
        raw_response: c.raw_response,
    })
}

fn usage_of(v: &Value) -> Option<Usage> {
    let u = v.get("usage")?;
    let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
    Some(Usage {
        input_tokens: n("input_tokens"),
        output_tokens: n("output_tokens"),
        cache_read_tokens: u
            .pointer("/input_tokens_details/cached_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
        cache_write_tokens: 0,
        reasoning_tokens: u
            .pointer("/output_tokens_details/reasoning_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32,
    })
}

fn handle_event(
    ev: &SseEvent,
    passthrough: bool,
    c: &mut Collected,
    started: Instant,
    on_delta: DeltaSink<'_>,
) -> Result<(), UpstreamError> {
    if ev.data.trim() == "[DONE]" {
        c.completed = true;
        return Ok(());
    }
    if passthrough {
        on_delta(Delta::Raw {
            event: ev.event.clone(),
            data: ev.data.clone(),
        });
    }
    let data: Value = match serde_json::from_str(&ev.data) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    let kind = data
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or(ev.event.as_str());
    match kind {
        "response.output_text.delta" => {
            if let Some(d) = data.get("delta").and_then(|v| v.as_str()) {
                if c.ttft.is_none() {
                    c.ttft = Some(started.elapsed().as_millis() as u64);
                }
                c.text.push_str(d);
                if !passthrough {
                    on_delta(Delta::Text(d.to_string()));
                }
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(d) = data.get("delta").and_then(|v| v.as_str()) {
                c.thinking.push_str(d);
                if !passthrough {
                    on_delta(Delta::Thinking(d.to_string()));
                }
            }
        }
        "response.output_item.done" => {
            if let Some(item) = data.get("item") {
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    c.tool_calls.push(ToolCall {
                        id: item
                            .get("call_id")
                            .or_else(|| item.get("id"))
                            .and_then(Value::as_str)
                            .unwrap_or("call_0")
                            .to_string(),
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("tool")
                            .to_string(),
                        arguments: item
                            .get("arguments")
                            .and_then(Value::as_str)
                            .unwrap_or("{}")
                            .to_string(),
                    });
                }
            }
        }
        "response.completed" | "response.incomplete" => {
            c.completed = true;
            if let Some(resp) = data.get("response") {
                c.usage = usage_of(resp);
                c.raw_response = Some(resp.clone());
                c.incomplete_reason = resp
                    .pointer("/incomplete_details/reason")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                // 非流式客户端只看 Completion，工具调用没在 done 事件里出现过也要从终态补齐。
                if c.tool_calls.is_empty() {
                    if let Some(out) = resp.get("output").and_then(Value::as_array) {
                        for item in out {
                            if item.get("type").and_then(Value::as_str) == Some("function_call") {
                                c.tool_calls.push(ToolCall {
                                    id: item
                                        .get("call_id")
                                        .and_then(Value::as_str)
                                        .unwrap_or("call_0")
                                        .to_string(),
                                    name: item
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("tool")
                                        .to_string(),
                                    arguments: item
                                        .get("arguments")
                                        .and_then(Value::as_str)
                                        .unwrap_or("{}")
                                        .to_string(),
                                });
                            }
                        }
                    }
                }
            }
        }
        "response.failed" => {
            c.completed = true;
            let msg = data
                .pointer("/response/error/message")
                .or_else(|| data.get("error").and_then(|e| e.get("message")))
                .and_then(|v| v.as_str())
                .unwrap_or("Grok 响应失败");
            return Err(UpstreamError::new(UpstreamKind::Upstream, 502, msg));
        }
        "error" => {
            let msg = data
                .get("message")
                .or_else(|| data.pointer("/error/message"))
                .and_then(|v| v.as_str())
                .unwrap_or("Grok error");
            let code = data
                .get("code")
                .or_else(|| data.pointer("/error/code"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let kind = if code.contains("rate_limit") {
                UpstreamKind::RateLimit
            } else if code.contains("insufficient")
                || code.contains("quota")
                || code.contains("credit")
            {
                UpstreamKind::Quota
            } else {
                UpstreamKind::Upstream
            };
            let status = match kind {
                UpstreamKind::RateLimit => 429,
                UpstreamKind::Quota => 402,
                _ => 502,
            };
            return Err(UpstreamError::new(kind, status, msg));
        }
        _ => {}
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalized::{ImageInput, Message, Sampling, ToolDef, ToolResult};

    #[test]
    fn passthrough_strips_rejected_fields() {
        let mut raw = Map::new();
        raw.insert("model".into(), json!("grok-4"));
        raw.insert("previous_response_id".into(), json!("r1"));
        raw.insert("service_tier".into(), json!("default"));
        raw.insert("input".into(), json!("hi"));
        let out = prepare_passthrough(&raw, "grok-4.5", "sess");
        assert!(out.get("previous_response_id").is_none());
        assert!(out.get("service_tier").is_none());
        assert_eq!(out.get("model").unwrap(), "grok-4.5");
        assert_eq!(out.get("stream").unwrap(), true);
        assert_eq!(out.get("prompt_cache_key").unwrap(), "sess");
    }

    #[test]
    fn bridge_carries_tools_images_and_tool_round_trips() {
        let mut user = Message::text(Role::User, "看图");
        user.images.push(ImageInput {
            data: "AAAA".into(),
            mime_type: "image/png".into(),
        });
        let mut asst = Message::text(Role::Assistant, "");
        asst.tool_calls.push(ToolCall {
            id: "call_1".into(),
            name: "lookup".into(),
            arguments: "{\"q\":1}".into(),
        });
        let mut tool = Message::text(Role::Tool, "");
        tool.tool_results.push(ToolResult {
            tool_call_id: "call_1".into(),
            tool_name: "lookup".into(),
            text: "42".into(),
            is_error: false,
        });
        let req = ChatRequest {
            model: "grok-4.5-high".into(),
            messages: vec![Message::text(Role::System, "be brief"), user, asst, tool],
            tools: vec![ToolDef {
                name: "lookup".into(),
                description: "d".into(),
                parameters: json!({ "type": "object" }),
                ..Default::default()
            }],
            tool_choice: ToolChoice::Required,
            sampling: Sampling {
                max_output_tokens: Some(100),
                ..Default::default()
            },
            ..Default::default()
        };
        let body = build_bridge(&req, "grok-4.5-high", "s");
        assert_eq!(body["model"], "grok-4.5");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["instructions"], "be brief");
        assert_eq!(body["tools"][0]["name"], "lookup");
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["max_output_tokens"], 100);
        let input = body["input"].as_array().unwrap();
        assert_eq!(input[0]["content"][1]["type"], "input_image");
        assert_eq!(input[1]["type"], "function_call");
        assert_eq!(input[2]["type"], "function_call_output");
        assert_eq!(input[2]["output"], "42");
    }

    #[test]
    fn image_body_maps_openai_fields_onto_imagine() {
        let req = ImageRequest {
            model: "grok-imagine".into(),
            prompt: "a cat".into(),
            size: Some("1792x1024".into()),
            quality: Some("hd".into()),
            output_format: Some("jpg".into()),
            ..Default::default()
        };
        let body = build_image_body(&req, &upstream_image_model(&req.model));
        assert_eq!(body["model"], "grok-imagine-image-quality", "hd 换质量档");
        assert_eq!(body["aspect_ratio"], "16:9");
        assert_eq!(body["response_format"], "b64_json");
        assert_eq!(body["output_format"], "jpeg");
        assert!(body.get("image").is_none());

        let edit = ImageRequest {
            references: vec![
                "data:image/png;base64,A".into(),
                "data:image/png;base64,B".into(),
            ],
            mask: Some("data:image/png;base64,M".into()),
            ..Default::default()
        };
        let body = build_image_body(&edit, nexus_grok::DEFAULT_IMAGE_EDIT_MODEL);
        assert_eq!(body["images"].as_array().unwrap().len(), 2);
        assert_eq!(body["mask"]["url"], "data:image/png;base64,M");
    }

    #[test]
    fn video_body_and_status_follow_the_imagine_shapes() {
        let req = VideoRequest {
            op: Some(VideoOp::Generate),
            model: "".into(),
            prompt: Some("waves".into()),
            image: Some("data:image/png;base64,X".into()),
            duration: Some(8),
            aspect_ratio: Some("16:9".into()),
            resolution: Some("720p".into()),
            ..Default::default()
        };
        let body = build_video_body(&req);
        assert_eq!(body["model"], nexus_grok::DEFAULT_VIDEO_MODEL);
        assert_eq!(body["image"]["url"], "data:image/png;base64,X");
        assert_eq!(body["duration"], 8);

        let st = parse_video_status(&json!({
            "status": "done", "video": { "url": "https://v/x.mp4", "duration": 8, "resolution": "720p" }
        }));
        assert_eq!(st.status, "done");
        assert_eq!(st.video_url.as_deref(), Some("https://v/x.mp4"));
        assert_eq!(st.duration_secs, Some(8));
        let pending = parse_video_status(&json!({ "status": "pending" }));
        assert!(pending.video_url.is_none());
    }

    #[test]
    fn status_mapping_tells_version_from_account_problems() {
        assert_eq!(map_status(426, "outdated").kind, UpstreamKind::Upstream);
        assert!(!map_status(426, "outdated").kind.blames_account());
        assert_eq!(map_status(402, "credits").kind, UpstreamKind::Quota);
        assert_eq!(
            map_media_status(403, "no entitlement").kind,
            UpstreamKind::Forbidden
        );
        assert_eq!(
            map_media_status(413, "too large").kind,
            UpstreamKind::BadRequest
        );
    }

    #[test]
    fn sse_collects_text_tools_usage_and_final_response() {
        let mut c = Collected {
            text: String::new(),
            thinking: String::new(),
            tool_calls: vec![],
            usage: None,
            completed: false,
            incomplete_reason: None,
            raw_response: None,
            ttft: None,
        };
        let started = Instant::now();
        let mut seen: Vec<Delta> = vec![];
        let mut sink = |d: Delta| seen.push(d);
        let ev = |t: &str, d: Value| SseEvent {
            event: t.into(),
            data: d.to_string(),
        };
        handle_event(
            &ev(
                "",
                json!({ "type": "response.output_text.delta", "delta": "hi" }),
            ),
            false,
            &mut c,
            started,
            &mut sink,
        )
        .unwrap();
        handle_event(
            &ev(
                "",
                json!({ "type": "response.reasoning_summary_text.delta", "delta": "think" }),
            ),
            false,
            &mut c,
            started,
            &mut sink,
        )
        .unwrap();
        handle_event(&ev("", json!({ "type": "response.output_item.done", "item": { "type": "function_call", "call_id": "c1", "name": "f", "arguments": "{}" } })), false, &mut c, started, &mut sink).unwrap();
        handle_event(&ev("", json!({ "type": "response.completed", "response": { "id": "r", "model": "grok-4.5", "usage": { "input_tokens": 3, "output_tokens": 2 } } })), false, &mut c, started, &mut sink).unwrap();
        assert_eq!(c.text, "hi");
        assert_eq!(c.thinking, "think");
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.usage.unwrap().input_tokens, 3);
        assert!(c.completed);
        assert_eq!(seen.len(), 2);
    }
}
