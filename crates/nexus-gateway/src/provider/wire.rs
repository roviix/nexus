//! 供应商的出站：按它的方言发请求，把回来的东西收成增量。
//!
//! 两条路：
//!
//! - **原样转发**：客户端和供应商讲同一种方言（Claude Code → Anthropic 格式的供应商、
//!   OpenCode → OpenAI Chat 的供应商）。请求体原封不动只换模型名，回来的 SSE 一帧不改地写回去。
//!   中间表示装不下的东西——`cache_control`、带签名的 thinking 块、`anthropic-beta` 头——
//!   只有这样才不丢；丢了就是缓存不命中、多轮思考 400。
//! - **翻译**：方言不一样（Claude Code → 只讲 Chat 的国产中转）。入站层已经收成
//!   [`ChatRequest`]，这里按供应商的方言写回去，流式增量收成 [`Delta`]。

use crate::error::{UpstreamError, UpstreamKind};
use crate::inbound::Dialect;
use crate::normalized::{
    ChatRequest, Completion, Delta, FinishReason, Message, RawInbound, Role, Sampling, ToolCall,
    ToolChoice, ToolDef, Usage,
};
use crate::sse::SseDecoder;
use nexus_claude::fingerprint::{self, ToolMap};
use nexus_claude::transport;
use nexus_store::key_providers::{self, ApiFormat, AuthField, Endpoint};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Claude 订阅号（OAuth / setup-token）出站时要按官方 CLI 补指纹。
#[derive(Debug, Clone)]
pub struct ClaudeOauthTune {
    pub account_ref: String,
    pub native: bool,
    pub requested_betas: Vec<String>,
    pub wants_1m: bool,
}

/// 这一次打哪家、用哪个模型。`api_key` 是秘密，不进 Debug。
pub struct Target {
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    pub format: ApiFormat,
    pub auth: AuthField,
    /// 发给上游的模型名（已去掉 `provider/` 前缀和 `[1m]`）。
    pub upstream_model: String,
    /// 协议头。OAuth 号要带 `anthropic-beta: oauth-2025-04-20`，和客户端自己的 beta 合并。
    pub extra_headers: Vec<(String, String)>,
    /// 只有 Claude 订阅通道填。供应商通道保持 `None`，请求体不动。
    pub claude_oauth: Option<ClaudeOauthTune>,
}

impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("format", &self.format)
            .field("upstream_model", &self.upstream_model)
            .field("claude_oauth", &self.claude_oauth)
            .finish()
    }
}

/// 供应商方言对应的入站方言。两边一致才原样转发。
pub fn dialect_of(format: ApiFormat) -> Dialect {
    match format {
        ApiFormat::Anthropic => Dialect::AnthropicMessages,
        ApiFormat::OpenaiChat => Dialect::OpenAiChat,
        ApiFormat::OpenaiResponses => Dialect::OpenAiResponses,
    }
}

/// 这次会不会原样转发（给账本和界面说清楚走的是哪条路）。
pub fn is_passthrough(target: &Target, request: &ChatRequest) -> bool {
    request
        .raw_inbound
        .as_ref()
        .is_some_and(|raw| raw.dialect == dialect_of(target.format))
}

pub async fn execute(
    target: &Target,
    request: &ChatRequest,
    on_delta: &mut (dyn FnMut(Delta) + Send),
) -> Result<Completion, UpstreamError> {
    match &request.raw_inbound {
        Some(raw) if raw.dialect == dialect_of(target.format) => {
            passthrough(target, raw, on_delta).await
        }
        _ => translate(target, request, on_delta).await,
    }
}

async fn passthrough(
    target: &Target,
    raw: &RawInbound,
    on_delta: &mut (dyn FnMut(Delta) + Send),
) -> Result<Completion, UpstreamError> {
    let started = Instant::now();
    let mut body = (*raw.body).clone();
    if let Some(obj) = body.as_object_mut() {
        obj.insert("model".into(), json!(target.upstream_model));
        // 流式的 Chat 默认不报用量；补上这一项账本才有数。客户端自己写了就尊重它。
        if raw.dialect == Dialect::OpenAiChat && raw.stream && !obj.contains_key("stream_options") {
            obj.insert("stream_options".into(), json!({ "include_usage": true }));
        }
    }
    let tuned = apply_claude_oauth(&mut body, target);
    let tools = tuned.tools;
    let url = key_providers::endpoint(&target.base_url, target.format, Endpoint::Chat);
    let resp = send(target, &url, &body, &raw.headers, &tuned.extra_headers).await?;
    let mut acc = Acc::new(started);
    let mut noop = |_d: Delta| {};

    if !raw.stream {
        let text = read_body(target, resp).await?;
        // 没要流、有的中转照样回 SSE：那就按流收，最后由序列化层拼成客户端要的整段。
        if !text.trim_start().starts_with('{') {
            let mut dec = SseDecoder::default();
            let mut events = dec.push(text.as_bytes());
            events.extend(dec.finish());
            for ev in events {
                apply_event(target.format, &ev.event, &ev.data, &mut acc, &mut noop);
            }
            if let Some((kind, status, message)) = acc.error.take() {
                return Err(UpstreamError::new(kind, status, message));
            }
            if !acc.emitted() {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    502,
                    format!(
                        "供应商「{}」回的东西认不出来：{}",
                        target.name,
                        snippet(&text)
                    ),
                ));
            }
            return Ok(finish_with_tools(acc, target, &tools));
        }
        let v: Value = serde_json::from_str(&text).map_err(|_| {
            UpstreamError::new(
                UpstreamKind::Upstream,
                502,
                format!("供应商「{}」回的不是 JSON：{}", target.name, snippet(&text)),
            )
        })?;
        apply_piece(target.format, "", &v, &mut acc, &mut noop);
        if let Some((kind, status, message)) = acc.error.take() {
            return Err(UpstreamError::new(kind, status, message));
        }
        let mut done = finish_with_tools(acc, target, &tools);
        let mut v = v;
        fingerprint::restore_tools_in_value(&mut v, &tools);
        done.raw_response = Some(v);
        return Ok(done);
    }

    let mut dec = SseDecoder::default();
    let mut resp = resp;
    let mut forwarded = false;
    // 转发开始之后供应商才报的错已经原封写给客户端了，收尾只把它标成错误。
    let mut failed_midway = false;
    loop {
        let chunk = resp.chunk(target).await?;
        let events = match &chunk {
            Some(bytes) => dec.push(bytes),
            None => dec.finish(),
        };
        for ev in events {
            if ev.data.trim() != "[DONE]" {
                if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                    apply_piece(target.format, &ev.event, &v, &mut acc, &mut noop);
                }
            }
            if let Some((kind, status, message)) = acc.error.take() {
                // 一个字都还没转给客户端：交给上层，它还能换一家重来。
                if !forwarded {
                    return Err(UpstreamError::new(kind, status, message));
                }
                failed_midway = true;
            }
            forwarded = true;
            on_delta(Delta::Raw {
                event: ev.event,
                data: fingerprint::restore_tools_in_sse(&ev.data, &tools),
            });
        }
        if chunk.is_none() {
            break;
        }
    }
    let mut done = finish_with_tools(acc, target, &tools);
    if failed_midway {
        done.finish_reason = FinishReason::Error;
    }
    Ok(done)
}

async fn translate(
    target: &Target,
    request: &ChatRequest,
    on_delta: &mut (dyn FnMut(Delta) + Send),
) -> Result<Completion, UpstreamError> {
    let started = Instant::now();
    let url = key_providers::endpoint(&target.base_url, target.format, Endpoint::Chat);
    let mut body = encode(target, request);
    let tuned = apply_claude_oauth(&mut body, target);
    let tools = tuned.tools;
    let resp = send(target, &url, &body, &[], &tuned.extra_headers).await?;
    let mut acc = Acc::new(started);
    let mut dec = SseDecoder::default();
    let mut resp = resp;
    // 要的是流，有的中转却一口气回整段 JSON。看第一个非空白字节认形状。
    let mut shape: Option<bool> = None;
    let mut whole = Vec::new();
    loop {
        let chunk = resp.chunk(target).await?;
        let Some(bytes) = chunk else { break };
        if shape.is_none() {
            if let Some(b) = bytes.iter().find(|b| !b.is_ascii_whitespace()) {
                shape = Some(*b != b'{');
            }
        }
        match shape {
            Some(true) => {
                for ev in dec.push(&bytes) {
                    apply_event(target.format, &ev.event, &ev.data, &mut acc, on_delta);
                }
            }
            _ => whole.extend_from_slice(&bytes),
        }
    }
    if shape == Some(true) {
        for ev in dec.finish() {
            apply_event(target.format, &ev.event, &ev.data, &mut acc, on_delta);
        }
    } else if let Ok(v) = serde_json::from_slice::<Value>(&whole) {
        apply_piece(target.format, "", &v, &mut acc, on_delta);
    }
    if let Some((kind, status, message)) = acc.error.take() {
        if !acc.emitted() {
            return Err(UpstreamError::new(kind, status, message));
        }
        acc.finish = FinishReason::Error;
    }
    Ok(finish_with_tools(acc, target, &tools))
}

struct OauthOut {
    tools: ToolMap,
    extra_headers: Vec<(String, String)>,
}

fn apply_claude_oauth(body: &mut Value, target: &Target) -> OauthOut {
    let Some(tune) = &target.claude_oauth else {
        return OauthOut {
            tools: ToolMap::default(),
            extra_headers: target.extra_headers.clone(),
        };
    };
    let prep = fingerprint::prepare(
        body,
        &tune.account_ref,
        tune.native,
        &tune.requested_betas,
        tune.wants_1m,
    );
    let mut extra_headers = target.extra_headers.clone();
    if !prep.betas.is_empty() {
        extra_headers.retain(|(k, _)| !k.eq_ignore_ascii_case("anthropic-beta"));
        extra_headers.push(("anthropic-beta".into(), prep.betas));
    }
    if let Some(session) = body
        .pointer("/metadata/user_id")
        .and_then(Value::as_str)
        .and_then(fingerprint::session_from_user_id)
    {
        // 和写进 metadata.user_id 的是同一个 session，真实 CLI 这两处一致。
        upsert_header(
            &mut extra_headers,
            "x-claude-code-session-id",
            session,
            true,
        );
    }
    if !header_present(&extra_headers, "x-client-request-id") {
        extra_headers.push((
            "x-client-request-id".into(),
            uuid::Uuid::new_v4().to_string(),
        ));
    }
    OauthOut {
        tools: prep.tools,
        extra_headers,
    }
}

fn header_present(headers: &[(String, String)], name: &str) -> bool {
    headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
}

fn upsert_header(headers: &mut Vec<(String, String)>, name: &str, value: &str, replace: bool) {
    if let Some(slot) = headers
        .iter_mut()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
    {
        if replace {
            slot.1 = value.to_string();
        }
    } else {
        headers.push((name.to_string(), value.to_string()));
    }
}

/// `count_tokens` 只认提示本身。`max_tokens`、`metadata`、`temperature` 带上去会 400
/// （`Extra inputs are not permitted`），而且它们也不占输入 token。
const COUNT_FIELDS: &[&str] = &[
    "model",
    "messages",
    "system",
    "tools",
    "tool_choice",
    "thinking",
    "output_config",
];

struct CountPrep {
    url: String,
    body: Value,
    forwarded: Vec<(String, String)>,
    extra_headers: Vec<(String, String)>,
}

fn prepare_count(target: &Target, request: &ChatRequest) -> CountPrep {
    let passthrough = request
        .raw_inbound
        .as_ref()
        .is_some_and(|raw| raw.dialect == dialect_of(target.format));
    let mut body = if passthrough {
        let raw = request.raw_inbound.as_ref().expect("passthrough");
        let mut body = (*raw.body).clone();
        if let Some(obj) = body.as_object_mut() {
            obj.insert("model".into(), json!(target.upstream_model));
        }
        body
    } else {
        encode(target, request)
    };
    let tuned = apply_claude_oauth(&mut body, target);
    retain_count_fields(&mut body);
    let mut extra_headers = tuned.extra_headers;
    let mut forwarded = if passthrough {
        request
            .raw_inbound
            .as_ref()
            .map(|raw| raw.headers.clone())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    // Claude Code 的 count_tokens 不带 X-Stainless-Timeout。真实 CLI 自己带了就留着。
    if target
        .claude_oauth
        .as_ref()
        .is_some_and(|tune| !tune.native)
    {
        extra_headers.retain(|(k, _)| !k.eq_ignore_ascii_case("x-stainless-timeout"));
        forwarded.retain(|(k, _)| !k.eq_ignore_ascii_case("x-stainless-timeout"));
    }
    let base = target.base_url.trim_end_matches('/');
    CountPrep {
        url: format!("{base}/v1/messages/count_tokens"),
        body,
        forwarded,
        extra_headers,
    }
}

fn retain_count_fields(body: &mut Value) {
    let Some(obj) = body.as_object_mut() else {
        return;
    };
    obj.retain(|k, _| COUNT_FIELDS.iter().any(|name| k == *name));
    if let Some(thinking) = obj.get_mut("thinking").and_then(Value::as_object_mut) {
        if thinking.get("display").and_then(Value::as_str) == Some("updates") {
            thinking.remove("display");
        }
    }
}

pub async fn count_input_tokens(
    target: &Target,
    request: &ChatRequest,
) -> Result<u64, UpstreamError> {
    let prep = prepare_count(target, request);
    let resp = send(
        target,
        &prep.url,
        &prep.body,
        &prep.forwarded,
        &prep.extra_headers,
    )
    .await?;
    let text = read_body(target, resp).await?;
    let value: Value = serde_json::from_str(&text).map_err(|_| {
        UpstreamError::new(
            UpstreamKind::Upstream,
            502,
            format!(
                "供应商「{}」的 count_tokens 不是 JSON：{}",
                target.name,
                snippet(&text)
            ),
        )
    })?;
    value
        .get("input_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            UpstreamError::new(
                UpstreamKind::Upstream,
                502,
                format!(
                    "供应商「{}」的 count_tokens 没有 input_tokens：{}",
                    target.name,
                    snippet(&text)
                ),
            )
        })
}

fn finish_with_tools(mut acc: Acc, target: &Target, tools: &ToolMap) -> Completion {
    let mut done = acc.finish(target);
    for t in &mut done.tool_calls {
        t.name = tools.restore(&t.name);
    }
    if let Some(v) = done.raw_response.as_mut() {
        fingerprint::restore_tools_in_value(v, tools);
    }
    done
}

fn apply_event(
    format: ApiFormat,
    event: &str,
    data: &str,
    acc: &mut Acc,
    on_delta: &mut dyn FnMut(Delta),
) {
    let data = data.trim();
    if data.is_empty() || data == "[DONE]" {
        return;
    }
    if let Ok(v) = serde_json::from_str::<Value>(data) {
        apply_piece(format, event, &v, acc, on_delta);
    }
}

async fn send(
    target: &Target,
    url: &str,
    body: &Value,
    forwarded: &[(String, String)],
    extra_headers: &[(String, String)],
) -> Result<UpResp, UpstreamError> {
    let mut headers = merge_headers(forwarded, extra_headers);
    if target.format == ApiFormat::Anthropic
        && !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("anthropic-version"))
    {
        headers.push(("anthropic-version".into(), "2023-06-01".into()));
    }
    // 打到 Anthropic 自己才换 Node/OpenSSL 握手。中转域名留在共享客户端上。
    if transport::is_anthropic_api(&target.base_url) {
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("accept"))
        {
            headers.push(("accept".into(), "application/json".into()));
        }
        if !headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("accept-encoding"))
        {
            headers.push(("accept-encoding".into(), "gzip, deflate, br, zstd".into()));
        }
        return send_claude(target, url, body, &headers).await;
    }
    send_shared(target, url, body, &headers).await
}

async fn send_shared(
    target: &Target,
    url: &str,
    body: &Value,
    headers: &[(String, String)],
) -> Result<UpResp, UpstreamError> {
    let mut req = client().post(url).json(body);
    for (k, v) in headers {
        req = req.header(k.as_str(), v.as_str());
    }
    req = apply_auth_reqwest(req, target);
    let resp = req
        .send()
        .await
        .map_err(|e| net_err(target, e.is_timeout(), &e))?;
    finish_send(target, UpResp::Shared(resp)).await
}

async fn send_claude(
    target: &Target,
    url: &str,
    body: &Value,
    headers: &[(String, String)],
) -> Result<UpResp, UpstreamError> {
    let mut headers = headers.to_vec();
    match (target.format, target.auth) {
        (ApiFormat::Anthropic, AuthField::ApiKey) => {
            headers.push(("x-api-key".into(), target.api_key.clone()));
        }
        (ApiFormat::Anthropic, AuthField::AuthToken)
        | (ApiFormat::OpenaiChat | ApiFormat::OpenaiResponses, _) => {
            headers.push(("authorization".into(), format!("Bearer {}", target.api_key)));
        }
    }
    if !headers
        .iter()
        .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
    {
        headers.push(("content-type".into(), "application/json".into()));
    }
    let bytes = serde_json::to_vec(body).map_err(|e| {
        UpstreamError::new(UpstreamKind::Upstream, 502, format!("请求体编码失败：{e}"))
    })?;
    let resp = transport::request(
        transport::Profile::Inference,
        "POST",
        url,
        &headers,
        Some(&bytes),
    )
    .await
    .map_err(|e| net_err(target, e.timeout, &e))?;
    finish_send(target, UpResp::Claude(resp)).await
}

fn apply_auth_reqwest(req: reqwest::RequestBuilder, target: &Target) -> reqwest::RequestBuilder {
    match (target.format, target.auth) {
        (ApiFormat::Anthropic, AuthField::ApiKey) => req.header("x-api-key", &target.api_key),
        (ApiFormat::Anthropic, AuthField::AuthToken) => req.bearer_auth(&target.api_key),
        (ApiFormat::OpenaiChat | ApiFormat::OpenaiResponses, _) => req.bearer_auth(&target.api_key),
    }
}

async fn finish_send(target: &Target, resp: UpResp) -> Result<UpResp, UpstreamError> {
    let status = resp.status();
    if !(200..300).contains(&status) {
        let text = resp.error_text().await;
        return Err(http_err(&target.name, status, &text));
    }
    Ok(resp)
}

/// 共享客户端和 Claude 专用客户端的响应，读的方式一样。
enum UpResp {
    Shared(reqwest::Response),
    Claude(transport::Response),
}

impl UpResp {
    fn status(&self) -> u16 {
        match self {
            Self::Shared(resp) => resp.status().as_u16(),
            Self::Claude(resp) => resp.status(),
        }
    }

    async fn chunk(&mut self, target: &Target) -> Result<Option<Vec<u8>>, UpstreamError> {
        match self {
            Self::Shared(resp) => resp
                .chunk()
                .await
                .map(|c| c.map(|b| b.to_vec()))
                .map_err(|e| net_err(target, e.is_timeout(), &e)),
            Self::Claude(resp) => resp
                .chunk()
                .await
                .map_err(|e| net_err(target, e.timeout, &e)),
        }
    }

    async fn error_text(self) -> String {
        match self {
            Self::Shared(resp) => resp.text().await.unwrap_or_default(),
            Self::Claude(resp) => resp.text().await.unwrap_or_default(),
        }
    }

    async fn bytes(self, target: &Target) -> Result<Vec<u8>, UpstreamError> {
        match self {
            Self::Shared(resp) => resp
                .bytes()
                .await
                .map(|b| b.to_vec())
                .map_err(|e| net_err(target, e.is_timeout(), &e)),
            Self::Claude(resp) => resp
                .bytes()
                .await
                .map_err(|e| net_err(target, e.timeout, &e)),
        }
    }
}

fn merge_headers(
    forwarded: &[(String, String)],
    extra: &[(String, String)],
) -> Vec<(String, String)> {
    let mut out = forwarded.to_vec();
    for (key, value) in extra {
        if let Some(slot) = out.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
            if key.eq_ignore_ascii_case("anthropic-beta") && !beta_has(&slot.1, value) {
                let current = slot.1.trim().trim_end_matches(',');
                slot.1 = if current.is_empty() {
                    value.clone()
                } else {
                    format!("{current},{value}")
                };
            }
        } else {
            out.push((key.clone(), value.clone()));
        }
    }
    out
}

fn beta_has(list: &str, token: &str) -> bool {
    list.split(',')
        .any(|part| part.trim().eq_ignore_ascii_case(token.trim()))
}

async fn read_body(target: &Target, resp: UpResp) -> Result<String, UpstreamError> {
    let bytes = resp.bytes(target).await?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn client() -> &'static reqwest::Client {
    use std::sync::OnceLock;
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

fn encode(target: &Target, request: &ChatRequest) -> Value {
    match target.format {
        ApiFormat::OpenaiChat => encode_chat(target, request),
        ApiFormat::Anthropic => encode_anthropic(target, request),
        ApiFormat::OpenaiResponses => encode_responses(target, request),
    }
}

fn encode_chat(target: &Target, request: &ChatRequest) -> Value {
    let mut body = json!({
        "model": target.upstream_model,
        "messages": openai_messages(request),
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    sampling_chat(&mut body, &request.sampling);
    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools.iter().map(openai_tool).collect::<Vec<_>>());
        body["tool_choice"] = openai_tool_choice(&request.tool_choice);
    }
    body
}

fn encode_anthropic(target: &Target, request: &ChatRequest) -> Value {
    let (system, messages) = anthropic_messages(request);
    let max = request.sampling.max_output_tokens.unwrap_or(8192);
    let mut body = json!({
        "model": target.upstream_model,
        "max_tokens": max,
        "messages": messages,
        "stream": true,
    });
    if !system.is_empty() {
        body["system"] = json!(system);
    }
    if let Some(t) = request.sampling.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = request.sampling.top_p {
        body["top_p"] = json!(p);
    }
    if !request.sampling.stop_sequences.is_empty() {
        body["stop_sequences"] = json!(request.sampling.stop_sequences);
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools.iter().map(anthropic_tool).collect::<Vec<_>>());
        body["tool_choice"] = anthropic_tool_choice(&request.tool_choice);
    }
    body
}

fn encode_responses(target: &Target, request: &ChatRequest) -> Value {
    let (instructions, input) = responses_input(request);
    let mut body = json!({
        "model": target.upstream_model,
        "input": input,
        "stream": true,
        "store": false,
    });
    if !instructions.is_empty() {
        body["instructions"] = json!(instructions);
    }
    if let Some(max) = request.sampling.max_output_tokens {
        body["max_output_tokens"] = json!(max);
    }
    if let Some(t) = request.sampling.temperature {
        body["temperature"] = json!(t);
    }
    if !request.tools.is_empty() {
        body["tools"] = json!(request.tools.iter().map(responses_tool).collect::<Vec<_>>());
        body["tool_choice"] = responses_tool_choice(&request.tool_choice);
    }
    body
}

fn sampling_chat(body: &mut Value, s: &Sampling) {
    if let Some(max) = s.max_output_tokens {
        body["max_tokens"] = json!(max);
    }
    if let Some(t) = s.temperature {
        body["temperature"] = json!(t);
    }
    if let Some(p) = s.top_p {
        body["top_p"] = json!(p);
    }
    if !s.stop_sequences.is_empty() {
        body["stop"] = json!(s.stop_sequences);
    }
}

fn data_url(mime: &str, data: &str) -> String {
    let mime = if mime.is_empty() { "image/png" } else { mime };
    format!("data:{mime};base64,{data}")
}

fn openai_messages(request: &ChatRequest) -> Vec<Value> {
    let mut out = Vec::new();
    for m in &request.messages {
        match m.role {
            Role::System => out.push(json!({ "role": "system", "content": m.text })),
            Role::User => out.push(openai_user(m)),
            Role::Assistant => out.push(openai_assistant(m)),
            Role::Tool => {
                for r in &m.tool_results {
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": r.tool_call_id,
                        "content": r.text,
                    }));
                }
            }
        }
    }
    out
}

fn openai_user(m: &Message) -> Value {
    if m.images.is_empty() {
        return json!({ "role": "user", "content": m.text });
    }
    let mut parts = vec![json!({ "type": "text", "text": m.text })];
    for img in &m.images {
        parts.push(json!({
            "type": "image_url",
            "image_url": { "url": data_url(&img.mime_type, &img.data) },
        }));
    }
    json!({ "role": "user", "content": parts })
}

fn openai_assistant(m: &Message) -> Value {
    let mut msg = json!({ "role": "assistant", "content": m.text });
    if !m.tool_calls.is_empty() {
        msg["tool_calls"] = json!(m
            .tool_calls
            .iter()
            .map(|c| json!({
                "id": c.id,
                "type": "function",
                "function": { "name": c.name, "arguments": c.arguments },
            }))
            .collect::<Vec<_>>());
    }
    msg
}

fn openai_tool(tool: &ToolDef) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool_schema(tool),
        },
    })
}

/// Codex 的语法工具（`exec` / `apply_patch`）没有 JSON 参数。发给只懂 function 的上游时
/// 给一个 `{ input: string }` 的壳，回程再由序列化层拆回 custom_tool_call。
fn tool_schema(tool: &ToolDef) -> Value {
    if tool.grammar || tool.parameters.is_null() {
        return json!({
            "type": "object",
            "properties": { "input": { "type": "string" } },
            "required": ["input"],
        });
    }
    tool.parameters.clone()
}

fn openai_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool(name) => json!({ "type": "function", "function": { "name": name } }),
    }
}

fn anthropic_messages(request: &ChatRequest) -> (String, Vec<Value>) {
    let mut system = String::new();
    let mut messages = Vec::new();
    for m in &request.messages {
        match m.role {
            Role::System => {
                if !system.is_empty() {
                    system.push('\n');
                }
                system.push_str(&m.text);
            }
            Role::User => messages.push(anthropic_user(m)),
            Role::Assistant => messages.push(anthropic_assistant(m)),
            Role::Tool => {
                let blocks: Vec<Value> = m
                    .tool_results
                    .iter()
                    .map(|r| {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": r.tool_call_id,
                            "content": r.text,
                            "is_error": r.is_error,
                        })
                    })
                    .collect();
                if !blocks.is_empty() {
                    messages.push(json!({ "role": "user", "content": blocks }));
                }
            }
        }
    }
    (system, messages)
}

fn anthropic_user(m: &Message) -> Value {
    if m.images.is_empty() {
        return json!({ "role": "user", "content": m.text });
    }
    let mut blocks = vec![json!({ "type": "text", "text": m.text })];
    for img in &m.images {
        let mime = if img.mime_type.is_empty() {
            "image/png"
        } else {
            &img.mime_type
        };
        blocks.push(json!({
            "type": "image",
            "source": { "type": "base64", "media_type": mime, "data": img.data },
        }));
    }
    json!({ "role": "user", "content": blocks })
}

fn anthropic_assistant(m: &Message) -> Value {
    if m.tool_calls.is_empty() {
        return json!({ "role": "assistant", "content": m.text });
    }
    let mut blocks = Vec::new();
    if !m.text.is_empty() {
        blocks.push(json!({ "type": "text", "text": m.text }));
    }
    for c in &m.tool_calls {
        let input = serde_json::from_str::<Value>(&c.arguments).unwrap_or(json!({}));
        blocks.push(json!({
            "type": "tool_use",
            "id": c.id,
            "name": c.name,
            "input": input,
        }));
    }
    json!({ "role": "assistant", "content": blocks })
}

fn anthropic_tool(tool: &ToolDef) -> Value {
    json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool_schema(tool),
    })
}

fn anthropic_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!({ "type": "auto" }),
        ToolChoice::None => json!({ "type": "none" }),
        ToolChoice::Required => json!({ "type": "any" }),
        ToolChoice::Tool(name) => json!({ "type": "tool", "name": name }),
    }
}

fn responses_input(request: &ChatRequest) -> (String, Vec<Value>) {
    let mut instructions = String::new();
    let mut input = Vec::new();
    for m in &request.messages {
        match m.role {
            Role::System => {
                if !instructions.is_empty() {
                    instructions.push('\n');
                }
                instructions.push_str(&m.text);
            }
            Role::User => {
                let mut content = vec![json!({ "type": "input_text", "text": m.text })];
                for img in &m.images {
                    content.push(json!({
                        "type": "input_image",
                        "image_url": data_url(&img.mime_type, &img.data),
                    }));
                }
                input.push(json!({ "type": "message", "role": "user", "content": content }));
            }
            Role::Assistant => {
                if !m.text.is_empty() {
                    input.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{ "type": "output_text", "text": m.text }],
                    }));
                }
                for c in &m.tool_calls {
                    input.push(json!({
                        "type": "function_call",
                        "call_id": c.id,
                        "name": c.name,
                        "arguments": c.arguments,
                    }));
                }
            }
            Role::Tool => {
                for r in &m.tool_results {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": r.tool_call_id,
                        "output": r.text,
                    }));
                }
            }
        }
    }
    (instructions, input)
}

fn responses_tool(tool: &ToolDef) -> Value {
    json!({
        "type": "function",
        "name": tool.name,
        "description": tool.description,
        "parameters": tool_schema(tool),
    })
}

fn responses_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Tool(name) => json!({ "type": "function", "name": name }),
    }
}

struct ToolAcc {
    id: String,
    name: String,
    arguments: String,
}

struct Acc {
    text: String,
    thinking: String,
    tools: Vec<ToolAcc>,
    usage: Usage,
    usage_measured: bool,
    finish: FinishReason,
    started: Instant,
    ttft: Option<Instant>,
    /// Anthropic / Responses 当前正在写参数的工具下标。
    tool_index: Option<usize>,
    /// 流里报的错（HTTP 200 之后才出的那种）。
    error: Option<(UpstreamKind, u16, String)>,
}

impl Acc {
    fn new(started: Instant) -> Self {
        Self {
            text: String::new(),
            thinking: String::new(),
            tools: Vec::new(),
            usage: Usage::default(),
            usage_measured: false,
            finish: FinishReason::Stop,
            started,
            ttft: None,
            tool_index: None,
            error: None,
        }
    }

    fn emitted(&self) -> bool {
        !self.text.is_empty() || !self.thinking.is_empty() || !self.tools.is_empty()
    }

    fn mark(&mut self) {
        if self.ttft.is_none() {
            self.ttft = Some(Instant::now());
        }
    }

    fn push_text(&mut self, s: &str, on_delta: &mut dyn FnMut(Delta)) {
        if s.is_empty() {
            return;
        }
        self.mark();
        self.text.push_str(s);
        on_delta(Delta::Text(s.to_string()));
    }

    fn push_thinking(&mut self, s: &str, on_delta: &mut dyn FnMut(Delta)) {
        if s.is_empty() {
            return;
        }
        self.mark();
        self.thinking.push_str(s);
        on_delta(Delta::Thinking(s.to_string()));
    }

    fn tool_slot(&mut self, index: usize) -> &mut ToolAcc {
        self.mark();
        while self.tools.len() <= index {
            self.tools.push(ToolAcc {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
        }
        &mut self.tools[index]
    }

    fn fail(&mut self, v: &Value) {
        let message = error_message(v).unwrap_or_else(|| snippet(&v.to_string()));
        let kind = classify(0, &message);
        self.error = Some((kind, status_of(kind), message));
    }

    fn finish(&mut self, target: &Target) -> Completion {
        let tool_calls = std::mem::take(&mut self.tools)
            .into_iter()
            .filter(|t| !t.name.is_empty() || !t.arguments.is_empty())
            .map(|t| ToolCall {
                id: t.id,
                name: t.name,
                arguments: if t.arguments.is_empty() {
                    "{}".into()
                } else {
                    t.arguments
                },
            })
            .collect::<Vec<_>>();
        let finish = if !tool_calls.is_empty() {
            FinishReason::ToolCalls
        } else {
            self.finish
        };
        Completion {
            text: std::mem::take(&mut self.text),
            thinking: std::mem::take(&mut self.thinking),
            tool_calls,
            finish_reason: finish,
            usage: self.usage,
            usage_measured: self.usage_measured,
            routed_model: Some(target.upstream_model.clone()),
            ttft_ms: self
                .ttft
                .map(|t| t.duration_since(self.started).as_millis() as u64),
            turn_ms: self.started.elapsed().as_millis() as u64,
            raw_response: None,
        }
    }
}

fn apply_piece(
    format: ApiFormat,
    event: &str,
    v: &Value,
    acc: &mut Acc,
    on_delta: &mut dyn FnMut(Delta),
) {
    match format {
        ApiFormat::OpenaiChat => apply_chat(v, acc, on_delta),
        ApiFormat::Anthropic => apply_anthropic(event, v, acc, on_delta),
        ApiFormat::OpenaiResponses => apply_responses(event, v, acc, on_delta),
    }
}

fn apply_chat(v: &Value, acc: &mut Acc, on_delta: &mut dyn FnMut(Delta)) {
    if v.get("error").is_some_and(|e| !e.is_null()) {
        acc.fail(v);
        return;
    }
    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
        take_openai_usage(u, acc);
    }
    let Some(choice) = v.pointer("/choices/0") else {
        return;
    };
    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
        acc.finish = map_finish(reason);
    }
    let delta = choice.get("delta").or_else(|| choice.get("message"));
    let Some(delta) = delta else { return };
    // DeepSeek 叫 reasoning_content，OpenRouter 叫 reasoning。
    for key in ["reasoning_content", "reasoning"] {
        if let Some(s) = delta.get(key).and_then(Value::as_str) {
            acc.push_thinking(s, on_delta);
        }
    }
    if let Some(s) = delta.get("content").and_then(Value::as_str) {
        acc.push_text(s, on_delta);
    }
    if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
        for (pos, call) in calls.iter().enumerate() {
            let index = call
                .get("index")
                .and_then(Value::as_u64)
                .map(|i| i as usize)
                .unwrap_or(pos);
            let slot = acc.tool_slot(index);
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                if !id.is_empty() {
                    slot.id = id.to_string();
                }
            }
            if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                if !name.is_empty() {
                    slot.name = name.to_string();
                }
            }
            if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                slot.arguments.push_str(args);
            }
        }
    }
}

fn apply_anthropic(event: &str, v: &Value, acc: &mut Acc, on_delta: &mut dyn FnMut(Delta)) {
    let kind = if event.is_empty() {
        v.get("type").and_then(Value::as_str).unwrap_or("")
    } else {
        event
    };
    match kind {
        "error" => acc.fail(v),
        "content_block_start" => {
            let block = v.get("content_block");
            if block.and_then(|b| b.get("type")).and_then(Value::as_str) == Some("tool_use") {
                let index = v
                    .get("index")
                    .and_then(Value::as_u64)
                    .unwrap_or(acc.tools.len() as u64) as usize;
                let slot = acc.tool_slot(index);
                if let Some(id) = block.and_then(|b| b.get("id")).and_then(Value::as_str) {
                    slot.id = id.to_string();
                }
                if let Some(name) = block.and_then(|b| b.get("name")).and_then(Value::as_str) {
                    slot.name = name.to_string();
                }
                acc.tool_index = Some(index);
            }
        }
        "content_block_delta" => {
            let delta = v.get("delta");
            let ty = delta
                .and_then(|d| d.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("");
            match ty {
                "text_delta" => {
                    if let Some(s) = delta.and_then(|d| d.get("text")).and_then(Value::as_str) {
                        acc.push_text(s, on_delta);
                    }
                }
                "thinking_delta" => {
                    if let Some(s) = delta
                        .and_then(|d| d.get("thinking"))
                        .and_then(Value::as_str)
                    {
                        acc.push_thinking(s, on_delta);
                    }
                }
                "input_json_delta" => {
                    if let Some(s) = delta
                        .and_then(|d| d.get("partial_json"))
                        .and_then(Value::as_str)
                    {
                        let index = v
                            .get("index")
                            .and_then(Value::as_u64)
                            .map(|i| i as usize)
                            .or(acc.tool_index)
                            .unwrap_or(0);
                        acc.tool_slot(index).arguments.push_str(s);
                    }
                }
                _ => {}
            }
        }
        "message_delta" => {
            if let Some(reason) = v.pointer("/delta/stop_reason").and_then(Value::as_str) {
                acc.finish = match reason {
                    "max_tokens" => FinishReason::Length,
                    "tool_use" => FinishReason::ToolCalls,
                    _ => FinishReason::Stop,
                };
            }
            if let Some(u) = v.get("usage") {
                take_anthropic_usage(u, acc);
            }
        }
        "message_start" => {
            if let Some(u) = v.pointer("/message/usage") {
                take_anthropic_usage(u, acc);
            }
        }
        _ => {
            // 非流式的整段 message。
            if let Some(blocks) = v.pointer("/content").and_then(Value::as_array) {
                for block in blocks {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(s) = block.get("text").and_then(Value::as_str) {
                                acc.push_text(s, on_delta);
                            }
                        }
                        Some("thinking") => {
                            if let Some(s) = block.get("thinking").and_then(Value::as_str) {
                                acc.push_thinking(s, on_delta);
                            }
                        }
                        Some("tool_use") => {
                            let index = acc.tools.len();
                            let slot = acc.tool_slot(index);
                            slot.id = block
                                .get("id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            slot.name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            if let Some(input) = block.get("input") {
                                slot.arguments = input.to_string();
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(reason) = v.get("stop_reason").and_then(Value::as_str) {
                    acc.finish = match reason {
                        "max_tokens" => FinishReason::Length,
                        "tool_use" => FinishReason::ToolCalls,
                        _ => FinishReason::Stop,
                    };
                }
            }
            if let Some(u) = v.get("usage") {
                take_anthropic_usage(u, acc);
            }
        }
    }
}

fn apply_responses(event: &str, v: &Value, acc: &mut Acc, on_delta: &mut dyn FnMut(Delta)) {
    let kind = if event.is_empty() {
        v.get("type").and_then(Value::as_str).unwrap_or("")
    } else {
        event
    };
    match kind {
        "error" | "response.failed" => {
            let inner = v.pointer("/response/error").unwrap_or(v);
            acc.fail(inner);
        }
        "response.output_text.delta" => {
            if let Some(s) = v.get("delta").and_then(Value::as_str) {
                acc.push_text(s, on_delta);
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(s) = v.get("delta").and_then(Value::as_str) {
                acc.push_thinking(s, on_delta);
            }
        }
        "response.output_item.added" => {
            let item = v.get("item");
            if item.and_then(|i| i.get("type")).and_then(Value::as_str) == Some("function_call") {
                let index = acc.tools.len();
                let slot = acc.tool_slot(index);
                slot.id = item
                    .and_then(|i| i.get("call_id").or_else(|| i.get("id")))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                slot.name = item
                    .and_then(|i| i.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                acc.tool_index = Some(index);
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(s) = v.get("delta").and_then(Value::as_str) {
                let index = acc.tool_index.unwrap_or(0);
                acc.tool_slot(index).arguments.push_str(s);
            }
        }
        "response.completed" | "response.incomplete" => {
            let response = v.get("response").unwrap_or(v);
            finish_responses(kind, response, acc, on_delta);
        }
        _ => {
            // 非流式：整个 response 对象。
            if v.get("object").and_then(Value::as_str) == Some("response") {
                let status = v.get("status").and_then(Value::as_str).unwrap_or("");
                let kind = if status == "incomplete" {
                    "response.incomplete"
                } else {
                    "response.completed"
                };
                finish_responses(kind, v, acc, on_delta);
            }
        }
    }
}

fn finish_responses(kind: &str, response: &Value, acc: &mut Acc, on_delta: &mut dyn FnMut(Delta)) {
    if let Some(u) = response.get("usage") {
        take_responses_usage(u, acc);
    }
    if kind == "response.incomplete" {
        acc.finish = FinishReason::Length;
    }
    // 流里已经收过增量就不再从终态里重拼一遍。
    if acc.emitted() {
        return;
    }
    let Some(items) = response.get("output").and_then(Value::as_array) else {
        return;
    };
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("message") => {
                if let Some(parts) = item.get("content").and_then(Value::as_array) {
                    for part in parts {
                        if let Some(s) = part.get("text").and_then(Value::as_str) {
                            acc.push_text(s, on_delta);
                        }
                    }
                }
            }
            Some("function_call") => {
                let index = acc.tools.len();
                let slot = acc.tool_slot(index);
                slot.id = item
                    .get("call_id")
                    .or_else(|| item.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                slot.name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                slot.arguments = item
                    .get("arguments")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
            }
            _ => {}
        }
    }
}

fn take_openai_usage(u: &Value, acc: &mut Acc) {
    acc.usage_measured = true;
    acc.usage.input_tokens = num(u.get("prompt_tokens"));
    acc.usage.output_tokens = num(u.get("completion_tokens"));
    acc.usage.cache_read_tokens = num(u.pointer("/prompt_tokens_details/cached_tokens"));
    acc.usage.reasoning_tokens = num(u.pointer("/completion_tokens_details/reasoning_tokens"));
}

fn take_anthropic_usage(u: &Value, acc: &mut Acc) {
    acc.usage_measured = true;
    if let Some(n) = u.get("input_tokens").and_then(Value::as_u64) {
        acc.usage.input_tokens = n as u32;
    }
    if let Some(n) = u.get("output_tokens").and_then(Value::as_u64) {
        acc.usage.output_tokens = n as u32;
    }
    if let Some(n) = u.get("cache_read_input_tokens").and_then(Value::as_u64) {
        acc.usage.cache_read_tokens = n as u32;
    }
    if let Some(n) = u.get("cache_creation_input_tokens").and_then(Value::as_u64) {
        acc.usage.cache_write_tokens = n as u32;
    }
}

fn take_responses_usage(u: &Value, acc: &mut Acc) {
    acc.usage_measured = true;
    acc.usage.input_tokens = num(u.get("input_tokens"));
    acc.usage.output_tokens = num(u.get("output_tokens"));
    acc.usage.cache_read_tokens = num(u.pointer("/input_tokens_details/cached_tokens"));
    acc.usage.reasoning_tokens = num(u.pointer("/output_tokens_details/reasoning_tokens"));
}

fn num(v: Option<&Value>) -> u32 {
    v.and_then(Value::as_u64).unwrap_or(0) as u32
}

fn map_finish(reason: &str) -> FinishReason {
    match reason {
        "length" => FinishReason::Length,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        _ => FinishReason::Stop,
    }
}

fn snippet(body: &str) -> String {
    body.chars().filter(|c| !c.is_control()).take(240).collect()
}

/// 各家报错的写法：`{error:{message}}`、`{error:"..."}`、`{message}`、`{msg}`、Anthropic 的
/// `{type:"error",error:{type,message}}`。
fn error_message(v: &Value) -> Option<String> {
    let pick = |v: &Value| -> Option<String> {
        v.get("message")
            .or_else(|| v.get("msg"))
            .or_else(|| v.get("detail"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    match v.get("error") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(e @ Value::Object(_)) => pick(e),
        _ => pick(v),
    }
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty())
}

/// 同一个状态码各家意思不一样：国产中转常拿 403 说「余额不足」、拿 400 说「模型不存在」。
/// 分错类的代价是具体的——余额不足按「没权限」处理就不会换一家，模型不存在按「请求有错」
/// 处理也不会换一家。
fn classify(status: u16, message: &str) -> UpstreamKind {
    let lower = message.to_lowercase();
    let quota = [
        "insufficient",
        "quota",
        "balance",
        "credit",
        "billing",
        "extra usage",
        "exceeded your current",
        "余额",
        "额度",
        "欠费",
        "充值",
        "积分",
    ]
    .iter()
    .any(|w| lower.contains(w));
    let model = ["model", "模型"].iter().any(|w| lower.contains(w))
        && [
            "not found",
            "not exist",
            "does not exist",
            "unsupported",
            "not supported",
            "invalid model",
            "no such",
            "不存在",
            "不支持",
            "无权",
            "未开通",
        ]
        .iter()
        .any(|w| lower.contains(w));
    match status {
        401 => UpstreamKind::Auth,
        402 => UpstreamKind::Quota,
        403 if quota => UpstreamKind::Quota,
        403 => UpstreamKind::Forbidden,
        404 => UpstreamKind::ModelUnsupported,
        408 | 504 => UpstreamKind::Timeout,
        413 => UpstreamKind::BadRequest,
        429 if quota => UpstreamKind::Quota,
        429 => UpstreamKind::RateLimit,
        400 | 422 if model => UpstreamKind::ModelUnsupported,
        400 | 422 => UpstreamKind::BadRequest,
        500..=599 => UpstreamKind::Upstream,
        // 流里报的错没有状态码：按文字认。
        0 if quota => UpstreamKind::Quota,
        0 if lower.contains("rate") || lower.contains("overloaded") => UpstreamKind::RateLimit,
        0 if model => UpstreamKind::ModelUnsupported,
        _ => UpstreamKind::Upstream,
    }
}

fn status_of(kind: UpstreamKind) -> u16 {
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

fn net_err(target: &Target, timeout: bool, e: &dyn std::fmt::Display) -> UpstreamError {
    if timeout {
        return UpstreamError::new(
            UpstreamKind::Timeout,
            504,
            format!("供应商「{}」超时没响应", target.name),
        );
    }
    UpstreamError::new(
        UpstreamKind::Upstream,
        502,
        format!("连不上供应商「{}」：{e}", target.name),
    )
}

fn http_err(name: &str, status: u16, body: &str) -> UpstreamError {
    let detail = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| error_message(&v))
        .unwrap_or_else(|| snippet(body));
    let kind = classify(status, &detail);
    let message = if detail.is_empty() {
        format!("供应商「{name}」回了 {status}")
    } else {
        format!("供应商「{name}」回了 {status}：{detail}")
    };
    UpstreamError::new(kind, status, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalized::Message;

    fn req() -> ChatRequest {
        ChatRequest {
            model: "deepseek-v4-pro".into(),
            messages: vec![
                Message::text(Role::System, "be brief"),
                Message::text(Role::User, "ping"),
            ],
            tools: vec![ToolDef {
                name: "echo".into(),
                description: "echo".into(),
                parameters: json!({ "type": "object", "properties": {} }),
                ..ToolDef::default()
            }],
            tool_choice: ToolChoice::Auto,
            sampling: Sampling {
                max_output_tokens: Some(32),
                ..Sampling::default()
            },
            ..ChatRequest::default()
        }
    }

    fn target(format: ApiFormat, base: &str) -> Target {
        Target {
            name: "Wasu".into(),
            base_url: base.into(),
            api_key: "sk".into(),
            format,
            auth: AuthField::AuthToken,
            upstream_model: "deepseek-v4-pro".into(),
            extra_headers: Vec::new(),
            claude_oauth: None,
        }
    }

    #[test]
    fn chat_body_keeps_system_and_uses_the_upstream_name() {
        let t = target(ApiFormat::OpenaiChat, "https://token.wasu.cn");
        let body = encode(&t, &req());
        assert_eq!(body["model"], "deepseek-v4-pro");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "ping");
        assert_eq!(body["tools"][0]["function"]["name"], "echo");
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn anthropic_body_lifts_system_out_of_messages() {
        let t = target(ApiFormat::Anthropic, "https://api.deepseek.com/anthropic");
        let body = encode(&t, &req());
        assert_eq!(body["system"], "be brief");
        assert_eq!(body["messages"][0]["role"], "user");
        assert_eq!(body["max_tokens"], 32);
    }

    #[test]
    fn grammar_tools_get_a_string_input_shell() {
        let mut r = req();
        r.tools = vec![ToolDef {
            name: "apply_patch".into(),
            grammar: true,
            ..ToolDef::default()
        }];
        let t = target(ApiFormat::OpenaiChat, "https://x.example");
        let body = encode(&t, &r);
        assert_eq!(
            body["tools"][0]["function"]["parameters"]["properties"]["input"]["type"],
            "string"
        );
    }

    #[test]
    fn chat_sse_accumulates_text_reasoning_tools_and_usage() {
        let mut acc = Acc::new(Instant::now());
        let mut texts = Vec::new();
        let mut on = |d: Delta| {
            if let Delta::Text(t) = d {
                texts.push(t);
            }
        };
        let mut dec = SseDecoder::default();
        let events = dec.push(
            concat!(
                "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"think\",\"content\":\"po\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"echo\",\"arguments\":\"{\\\"a\\\"\"}}]}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\":1}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
                "data: [DONE]\n\n",
            )
            .as_bytes(),
        );
        for ev in events {
            apply_event(
                ApiFormat::OpenaiChat,
                &ev.event,
                &ev.data,
                &mut acc,
                &mut on,
            );
        }
        assert_eq!(texts, vec!["po".to_string()]);
        let t = target(ApiFormat::OpenaiChat, "https://x.example");
        let done = acc.finish(&t);
        assert_eq!(done.thinking, "think");
        assert_eq!(done.tool_calls[0].name, "echo");
        assert_eq!(done.tool_calls[0].arguments, "{\"a\":1}");
        assert_eq!(done.finish_reason, FinishReason::ToolCalls);
        assert_eq!(done.usage.input_tokens, 3);
        assert!(done.usage_measured);
    }

    #[test]
    fn an_error_inside_a_200_stream_is_noticed() {
        let mut acc = Acc::new(Instant::now());
        apply_event(
            ApiFormat::Anthropic,
            "error",
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            &mut acc,
            &mut |_| {},
        );
        let (kind, _, message) = acc.error.clone().unwrap();
        assert_eq!(kind, UpstreamKind::RateLimit);
        assert_eq!(message, "Overloaded");
    }

    #[test]
    fn domestic_relays_saying_no_balance_with_403_count_as_quota() {
        let e = http_err(
            "Wasu",
            403,
            r#"{"error":{"message":"积分余额不足，请充值或联系管理员处理","type":"new_api_error"}}"#,
        );
        assert_eq!(e.kind, UpstreamKind::Quota);
        assert!(e.message.contains("Wasu"));
        assert!(e.message.contains("积分余额不足"));
        assert_eq!(http_err("x", 403, "nope").kind, UpstreamKind::Forbidden);
        assert_eq!(
            http_err(
                "x",
                400,
                r#"{"error":{"message":"model gpt-9 does not exist"}}"#
            )
            .kind,
            UpstreamKind::ModelUnsupported
        );
        assert_eq!(
            http_err("x", 400, r#"{"error":{"message":"max_tokens too large"}}"#).kind,
            UpstreamKind::BadRequest
        );
        assert_eq!(http_err("x", 503, "busy").kind, UpstreamKind::Upstream);
    }

    #[test]
    fn claude_oauth_tune_rewrites_tools_and_injects_identity() {
        let mut body = json!({
            "model": "claude-sonnet-4-6",
            "system": "be brief",
            "messages": [{ "role": "user", "content": "hello world" }],
            "tools": [{ "name": "bash" }]
        });
        let t = Target {
            name: "Claude".into(),
            base_url: "https://api.anthropic.com".into(),
            api_key: "sk".into(),
            format: ApiFormat::Anthropic,
            auth: AuthField::AuthToken,
            upstream_model: "claude-sonnet-4-6".into(),
            extra_headers: Vec::new(),
            claude_oauth: Some(ClaudeOauthTune {
                account_ref: "acct".into(),
                native: false,
                requested_betas: Vec::new(),
                wants_1m: false,
            }),
        };
        let map = apply_claude_oauth(&mut body, &t);
        assert_eq!(body["tools"][0]["name"], "Bash");
        assert_eq!(map.tools.restore("Bash"), "bash");
        assert!(map
            .extra_headers
            .iter()
            .any(|(k, v)| k == "anthropic-beta" && v.contains("oauth-2025-04-20")));
        assert!(body["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cc_entrypoint=cli"));
        let uid = body["metadata"]["user_id"].as_str().unwrap();
        assert!(uid.starts_with("user_"));
        let session = map
            .extra_headers
            .iter()
            .find(|(k, _)| k == "x-claude-code-session-id")
            .map(|(_, v)| v.as_str())
            .unwrap();
        assert!(uid.ends_with(session));
        let request_id = map
            .extra_headers
            .iter()
            .find(|(k, _)| k == "x-client-request-id")
            .map(|(_, v)| v.as_str())
            .unwrap();
        assert_eq!(request_id.len(), 36);
    }

    #[test]
    fn count_tokens_body_keeps_the_prompt_and_drops_sampling() {
        use std::sync::Arc;

        use crate::inbound::Dialect;
        use crate::normalized::RawInbound;

        let request = ChatRequest {
            model: "claude-sonnet-4-6".into(),
            raw_inbound: Some(RawInbound {
                dialect: Dialect::AnthropicMessages,
                body: Arc::new(json!({
                    "model": "claude-sonnet-4-6",
                    "max_tokens": 128000,
                    "temperature": 1,
                    "stream": false,
                    "metadata": { "user_id": "client" },
                    "system": "be brief",
                    "messages": [{ "role": "user", "content": "hello" }],
                    "thinking": { "type": "adaptive", "display": "updates" }
                })),
                stream: false,
                headers: vec![("user-agent".into(), "opencode/1.0".into())],
            }),
            ..ChatRequest::default()
        };
        let mut t = target(ApiFormat::Anthropic, "https://api.anthropic.com");
        t.upstream_model = "claude-sonnet-4-6".into();
        t.extra_headers = vec![
            ("x-stainless-timeout".into(), "600".into()),
            ("anthropic-version".into(), "2023-06-01".into()),
        ];
        t.claude_oauth = Some(ClaudeOauthTune {
            account_ref: "acct".into(),
            native: false,
            requested_betas: Vec::new(),
            wants_1m: false,
        });
        let prep = prepare_count(&t, &request);
        assert_eq!(
            prep.url,
            "https://api.anthropic.com/v1/messages/count_tokens"
        );
        assert!(prep.body.get("max_tokens").is_none());
        assert!(prep.body.get("temperature").is_none());
        assert!(prep.body.get("stream").is_none());
        assert!(prep.body.get("metadata").is_none());
        assert!(prep.body["system"].is_array());
        assert!(prep.body["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cc_entrypoint=cli"));
        assert_eq!(prep.body["messages"][2]["content"], "hello");
        assert!(prep.body["thinking"].get("display").is_none());
        assert_eq!(prep.body["thinking"]["type"], "adaptive");
        assert!(prep
            .extra_headers
            .iter()
            .any(|(k, v)| k == "anthropic-beta" && v.contains("oauth-2025-04-20")));
        assert!(prep
            .extra_headers
            .iter()
            .all(|(k, _)| !k.eq_ignore_ascii_case("x-stainless-timeout")));
        assert!(prep
            .forwarded
            .iter()
            .all(|(k, _)| !k.eq_ignore_ascii_case("x-stainless-timeout")));
        assert!(prep
            .extra_headers
            .iter()
            .any(|(k, _)| k == "x-claude-code-session-id"));
    }

    #[test]
    fn a_non_stream_responses_object_is_read_whole() {
        let mut acc = Acc::new(Instant::now());
        apply_piece(
            ApiFormat::OpenaiResponses,
            "",
            &json!({
                "object": "response",
                "status": "completed",
                "output": [{ "type": "message", "content": [{ "type": "output_text", "text": "hi" }] }],
                "usage": { "input_tokens": 5, "output_tokens": 1 }
            }),
            &mut acc,
            &mut |_| {},
        );
        let t = target(ApiFormat::OpenaiResponses, "https://x.example/v1");
        let done = acc.finish(&t);
        assert_eq!(done.text, "hi");
        assert_eq!(done.usage.input_tokens, 5);
    }
}
