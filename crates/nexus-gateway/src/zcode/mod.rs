//! ZCode（智谱 GLM）直连：`POST /v1/messages`。
//!
//! 出站讲 **Anthropic Messages**，两档套餐都一样 —— 官方客户端把 OpenAI 那条路废了
//! （`.../zcode-plan/chat/completions` 从 2026-08-28 起回 404），所以这里只有一种出站方言。
//! 这也是本地网关第一条 Anthropic 出站：Cursor / Kiro 各讲自己的私有协议，Codex 讲 Responses。
//!
//! 对外模型名是裸 `glm-*`（本地网关里不和谁撞车），`zcode/` 前缀可以显式指定。

use crate::error::{UpstreamError, UpstreamKind};
use crate::inference::{DEFAULT_IDLE_TIMEOUT, DEFAULT_MAX_TURN};
use crate::lane::{BoxFuture, Credential};
use crate::normalized::{
    ChatRequest, Completion, Delta, FinishReason, ImageInput, Message, Role, ToolCall, ToolChoice,
    Usage,
};
use crate::sse::{SseDecoder, SseEvent};
use crate::upstream::{DeltaSink, Upstream};
use nexus_zcode::model::{ZcodePlan, ZcodeProvider};
use nexus_zcode::protocol::{
    auth_headers, chat_url, default_max_tokens, glm53_effort, is_glm53_family,
    llm_identity_headers, trace_headers, upstream_model, GLM53_MIN_BUDGET,
};
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 一个号该发到哪。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZcodeRoute {
    pub provider: ZcodeProvider,
    pub plan: ZcodePlan,
}

impl Default for ZcodeRoute {
    /// 问不出路由时的落点。**刻意落在 coding-plan**：它不需要验证码，
    /// 猜错了只会是一次 401，而猜成 start-plan 会把请求送进要过门禁的那条路。
    fn default() -> Self {
        Self {
            provider: ZcodeProvider::Zai,
            plan: ZcodePlan::CodingPlan,
        }
    }
}

/// 网关要从账号层问的唯一一件事：这个号走哪条上游。
///
/// `Credential` 只带 label 和 token，装不下服务商与套餐档；而这两者决定 URL 和认证头。
/// 用一个窄接口回头问账号层，比把路由塞进共享的 `Credential` 干净。
pub trait ZcodeRouting: Send + Sync {
    fn route(&self, label: &str) -> Option<ZcodeRoute>;
}

/// 路由永远给同一个答案。测试和单账号场景用。
pub struct FixedRoute(pub ZcodeRoute);

impl ZcodeRouting for FixedRoute {
    fn route(&self, _label: &str) -> Option<ZcodeRoute> {
        Some(self.0)
    }
}

pub struct ZcodeUpstream {
    client: reqwest::Client,
    routing: Arc<dyn ZcodeRouting>,
    /// 测试用：盖掉真实端点。
    endpoint_override: Option<String>,
    idle_timeout: Duration,
    max_turn: Duration,
}

impl ZcodeUpstream {
    pub fn new(routing: Arc<dyn ZcodeRouting>) -> Self {
        Self {
            client: crate::inference::http_client(),
            routing,
            endpoint_override: None,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            max_turn: DEFAULT_MAX_TURN,
        }
    }

    #[cfg(test)]
    pub fn with_endpoint(routing: Arc<dyn ZcodeRouting>, endpoint: String) -> Self {
        Self {
            endpoint_override: Some(endpoint),
            ..Self::new(routing)
        }
    }

    async fn run(
        &self,
        credential: &Credential,
        request: &ChatRequest,
        on_delta: DeltaSink<'_>,
    ) -> Result<Completion, UpstreamError> {
        let route = self.routing.route(&credential.label).unwrap_or_default();
        let model = upstream_model(&request.model);

        if route.plan == ZcodePlan::StartPlan {
            // 体验套餐的网关每个请求都要一枚阿里云验证码（V3）票据，拿不到票就是 3012。
            // 与其发一个注定被拒的请求、让用户对着一句上游错误发呆，不如在这里说清楚。
            return Err(UpstreamError::new(
                UpstreamKind::BadRequest,
                400,
                "ZCode 体验套餐（start-plan）暂不支持转发：它的网关要求每个请求都带一枚\
                 阿里云验证码票据，本地网关还没有实现求解器。请改用编码套餐（coding-plan）的号。",
            ));
        }

        let body = build_body(request, &model, route.plan);
        let payload = serde_json::to_vec(&body).map_err(|e| {
            UpstreamError::new(
                UpstreamKind::BadRequest,
                400,
                format!("请求体无法序列化：{e}"),
            )
        })?;

        let url = self
            .endpoint_override
            .clone()
            .unwrap_or_else(|| chat_url(route.provider, route.plan));
        let request_id = uuid::Uuid::new_v4().to_string();
        let trace_id = uuid::Uuid::new_v4().to_string();
        let session = request
            .conversation_id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let mut req = self
            .client
            .post(&url)
            .timeout(self.max_turn)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .body(payload);
        for (k, v) in llm_identity_headers() {
            req = req.header(k, v);
        }
        for (k, v) in trace_headers(&request_id, &trace_id, &session) {
            req = req.header(k, v);
        }
        for (k, v) in auth_headers(route.plan, &credential.access_token) {
            req = req.header(k, v);
        }

        let res = match req.send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                return Err(UpstreamError::new(
                    UpstreamKind::Timeout,
                    504,
                    format!("连 ZCode 超时：{e}"),
                ));
            }
            Err(e) => {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    502,
                    format!("连不上 ZCode：{e}"),
                ));
            }
        };

        let status = res.status().as_u16();
        if !(200..300).contains(&status) {
            let text = res.text().await.unwrap_or_default();
            return Err(map_status(status, &text));
        }
        on_delta(Delta::Headers(Vec::new()));
        read_sse(res, Instant::now(), self.idle_timeout, &model, on_delta).await
    }
}

impl Upstream for ZcodeUpstream {
    fn stream<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ChatRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
        Box::pin(self.run(credential, request, on_delta))
    }
}

pub fn accepts(base_model: &str) -> bool {
    nexus_zcode::is_zcode_model(base_model)
}

// ---------------------------------------------------------------------------
// 出站：中间表示 → Anthropic Messages
// ---------------------------------------------------------------------------

fn text_block(text: &str) -> Value {
    json!({ "type": "text", "text": text })
}

fn image_block(img: &ImageInput) -> Value {
    json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": if img.mime_type.is_empty() { "image/png" } else { img.mime_type.as_str() },
            "data": img.data,
        }
    })
}

/// 一条 assistant 消息的内容块。thinking 不回放：我们手里只有明文，没有上游签发的
/// `signature`，而 Anthropic 形态的 thinking 块缺签名会被拒。
fn assistant_blocks(m: &Message) -> Vec<Value> {
    let mut blocks = Vec::new();
    if !m.text.trim().is_empty() {
        blocks.push(text_block(&m.text));
    }
    for c in &m.tool_calls {
        let input: Value = serde_json::from_str(&c.arguments).unwrap_or_else(|_| json!({}));
        blocks.push(json!({
            "type": "tool_use",
            "id": if c.id.is_empty() { "toolu_0".to_string() } else { c.id.clone() },
            "name": if c.name.is_empty() { "tool".to_string() } else { c.name.clone() },
            "input": if input.is_object() { input } else { json!({ "input": input }) },
        }));
    }
    blocks
}

/// 中间表示 → Anthropic Messages 请求体。
///
/// 三条 Anthropic 的硬规矩，违反哪条都是 400：
///
/// 1. `system` 在顶层，不能当成一条 `role: "system"` 的消息；
/// 2. `tool_result` 块只能出现在 **user** 消息里（我们的 `Role::Tool` 要折进 user）；
/// 3. 同角色的消息不能连续出现，要合并成一条多块消息。
pub fn build_body(request: &ChatRequest, model: &str, plan: ZcodePlan) -> Value {
    let mut system = String::new();
    // (role, blocks)。合并相邻同角色。
    let mut turns: Vec<(&'static str, Vec<Value>)> = Vec::new();

    let push =
        |role: &'static str, blocks: Vec<Value>, turns: &mut Vec<(&'static str, Vec<Value>)>| {
            if blocks.is_empty() {
                return;
            }
            match turns.last_mut() {
                Some((last, acc)) if *last == role => acc.extend(blocks),
                _ => turns.push((role, blocks)),
            }
        };

    for m in &request.messages {
        match m.role {
            Role::System => {
                if !m.text.trim().is_empty() {
                    if !system.is_empty() {
                        system.push_str("\n\n");
                    }
                    system.push_str(m.text.trim());
                }
            }
            Role::User => {
                let mut blocks = Vec::new();
                for img in &m.images {
                    blocks.push(image_block(img));
                }
                if !m.text.trim().is_empty() {
                    blocks.push(text_block(&m.text));
                }
                push("user", blocks, &mut turns);
            }
            Role::Assistant => {
                push("assistant", assistant_blocks(m), &mut turns);
            }
            Role::Tool => {
                // 工具结果是 user 那一轮的内容块，Anthropic 没有独立的 tool 角色。
                let mut blocks = Vec::new();
                for r in &m.tool_results {
                    blocks.push(json!({
                        "type": "tool_result",
                        "tool_use_id": if r.tool_call_id.is_empty() { "toolu_0".to_string() } else { r.tool_call_id.clone() },
                        "content": [text_block(&r.text)],
                        "is_error": r.is_error,
                    }));
                }
                if m.tool_results.is_empty() && !m.text.trim().is_empty() {
                    blocks.push(text_block(&m.text));
                }
                push("user", blocks, &mut turns);
            }
        }
    }

    // Anthropic 要求 messages 非空，且第一条是 user。
    if turns.is_empty() {
        turns.push(("user", vec![text_block("")]));
    }
    if turns[0].0 != "user" {
        turns.insert(0, ("user", vec![text_block("")]));
    }

    let messages: Vec<Value> = turns
        .into_iter()
        .map(|(role, content)| json!({ "role": role, "content": content }))
        .collect();

    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert("messages".into(), Value::Array(messages));
    body.insert("stream".into(), json!(true));

    // `max_tokens` 在 Anthropic 里是必填。缺省时按目录给这个模型的真实上限，
    // 而不是一个通用小值 —— 填 4096 会把 GLM 的长输出直接截断。
    let max_tokens = request
        .sampling
        .max_output_tokens
        .filter(|n| *n > 0)
        .unwrap_or_else(|| default_max_tokens(model));
    body.insert("max_tokens".into(), json!(max_tokens));

    if !system.is_empty() {
        body.insert("system".into(), json!([text_block(&system)]));
    }
    if let Some(t) = request.sampling.temperature {
        body.insert("temperature".into(), json!(t));
    }
    if let Some(p) = request.sampling.top_p {
        body.insert("top_p".into(), json!(p));
    }
    if !request.sampling.stop_sequences.is_empty() {
        body.insert(
            "stop_sequences".into(),
            json!(request.sampling.stop_sequences),
        );
    }

    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": if t.description.is_empty() { t.name.as_str() } else { t.description.as_str() },
                    "input_schema": if t.parameters.is_object() {
                        t.parameters.clone()
                    } else {
                        json!({ "type": "object" })
                    },
                })
            })
            .collect();
        body.insert("tools".into(), Value::Array(tools));
        let choice = match &request.tool_choice {
            ToolChoice::Auto => json!({ "type": "auto" }),
            ToolChoice::None => json!({ "type": "none" }),
            ToolChoice::Required => json!({ "type": "any" }),
            ToolChoice::Tool(name) => json!({ "type": "tool", "name": name }),
        };
        body.insert("tool_choice".into(), choice);
    }

    // GLM-5.3 这一族的思考深度只认 `output_config.effort`，而且必须配一个匹配的
    // `thinking.budget_tokens`；低于 1024 上游会塌缩成几乎不思考。
    // 5 / 5.1 / 5.2 都不认 effort，别顺手放进来。
    if is_glm53_family(model) {
        let (effort, budget) = glm53_effort(None);
        let budget = budget
            .max(GLM53_MIN_BUDGET)
            .min(max_tokens.max(GLM53_MIN_BUDGET));
        body.insert("output_config".into(), json!({ "effort": effort }));
        body.insert(
            "thinking".into(),
            json!({ "type": "enabled", "budget_tokens": budget }),
        );
    }

    let _ = plan;
    Value::Object(body)
}

fn map_status(status: u16, text: &str) -> UpstreamError {
    let head: String = text.chars().take(300).collect();
    // 上游把业务码放在 JSON 里，HTTP 状态常常还是 200/400。3012 是「没通过内容检查」。
    let kind = match status {
        401 | 403 => UpstreamKind::Auth,
        429 => UpstreamKind::RateLimit,
        400 | 404 | 422 => UpstreamKind::BadRequest,
        _ => UpstreamKind::Upstream,
    };
    UpstreamError::new(kind, status, format!("ZCode {status}：{head}"))
}

// ---------------------------------------------------------------------------
// 回程：Anthropic SSE → Delta
// ---------------------------------------------------------------------------

/// 一个正在攒的 `tool_use` 块。`input` 按 `input_json_delta` 的片段流下来，
/// 攒齐才交出去 —— 半截 JSON 对客户端没有用。
#[derive(Default)]
struct ToolUseBuf {
    id: String,
    name: String,
    json: String,
}

/// 内容块按 `index` 编址，而 index 不保证连续，所以用 (index, 种类) 的表而不是数组。
enum Block {
    Text,
    Thinking,
    ToolUse(ToolUseBuf),
    Other,
}

#[derive(Default)]
struct Collected {
    text: String,
    thinking: String,
    tool_calls: Vec<ToolCall>,
    blocks: Vec<(u64, Block)>,
    usage: Usage,
    usage_measured: bool,
    finish: Option<FinishReason>,
    routed_model: Option<String>,
    ttft: Option<u64>,
    error: Option<String>,
}

impl Collected {
    fn block_mut(&mut self, index: u64) -> Option<&mut Block> {
        self.blocks
            .iter_mut()
            .find(|(i, _)| *i == index)
            .map(|(_, b)| b)
    }
}

fn map_stop_reason(raw: &str) -> FinishReason {
    match raw {
        "tool_use" => FinishReason::ToolCalls,
        "max_tokens" => FinishReason::Length,
        "refusal" => FinishReason::ContentFilter,
        // end_turn / stop_sequence / 其他
        _ => FinishReason::Stop,
    }
}

fn u32_at(v: &Value, key: &str) -> Option<u32> {
    v.get(key).and_then(Value::as_u64).map(|n| n as u32)
}

fn handle_event(ev: &SseEvent, c: &mut Collected, started: Instant, on_delta: DeltaSink<'_>) {
    if ev.data.trim().is_empty() || ev.data.trim() == "[DONE]" {
        return;
    }
    let Ok(v) = serde_json::from_str::<Value>(&ev.data) else {
        return;
    };
    // `event:` 行缺失时按载荷里的 `type` 认（上游偶尔只发 data）。
    let kind = if ev.event.is_empty() {
        v.get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    } else {
        ev.event.clone()
    };

    match kind.as_str() {
        "message_start" => {
            let Some(msg) = v.get("message") else { return };
            if let Some(m) = msg.get("model").and_then(Value::as_str) {
                c.routed_model = Some(m.to_string());
            }
            if let Some(u) = msg.get("usage") {
                c.usage.input_tokens = u32_at(u, "input_tokens").unwrap_or(0);
                c.usage.cache_read_tokens = u32_at(u, "cache_read_input_tokens").unwrap_or(0);
                c.usage.cache_write_tokens = u32_at(u, "cache_creation_input_tokens").unwrap_or(0);
                c.usage_measured = true;
            }
        }
        "content_block_start" => {
            let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
            let cb = v.get("content_block").unwrap_or(&Value::Null);
            let block = match cb.get("type").and_then(Value::as_str).unwrap_or("") {
                "text" => Block::Text,
                "thinking" | "redacted_thinking" => Block::Thinking,
                "tool_use" => Block::ToolUse(ToolUseBuf {
                    id: cb
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("toolu_0")
                        .to_string(),
                    name: cb
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string(),
                    json: String::new(),
                }),
                _ => Block::Other,
            };
            // 同一个 index 被复用时覆盖旧的。
            match c.blocks.iter_mut().find(|(i, _)| *i == index) {
                Some(slot) => slot.1 = block,
                None => c.blocks.push((index, block)),
            }
        }
        "content_block_delta" => {
            let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
            let Some(d) = v.get("delta") else { return };
            match d.get("type").and_then(Value::as_str).unwrap_or("") {
                "text_delta" => {
                    if let Some(piece) = d
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        if c.ttft.is_none() {
                            c.ttft = Some(started.elapsed().as_millis() as u64);
                        }
                        c.text.push_str(piece);
                        on_delta(Delta::Text(piece.to_string()));
                    }
                }
                "thinking_delta" => {
                    if let Some(piece) = d
                        .get("thinking")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        if c.ttft.is_none() {
                            c.ttft = Some(started.elapsed().as_millis() as u64);
                        }
                        c.thinking.push_str(piece);
                        on_delta(Delta::Thinking(piece.to_string()));
                    }
                }
                "input_json_delta" => {
                    if let Some(piece) = d.get("partial_json").and_then(Value::as_str) {
                        if let Some(Block::ToolUse(buf)) = c.block_mut(index) {
                            buf.json.push_str(piece);
                        }
                    }
                }
                // signature_delta：thinking 块的签名，我们不回放 thinking，用不上。
                _ => {}
            }
        }
        "content_block_stop" => {
            // 一个 tool_use 块收尾了就把它交出去。文本 / thinking 块已经边流边发过，
            // 这里只留着让 index 继续占位（同一 index 被复用时会被覆盖）。
            let index = v.get("index").and_then(Value::as_u64).unwrap_or(0);
            let is_tool = matches!(c.block_mut(index), Some(Block::ToolUse(_)));
            if is_tool {
                let pos = c
                    .blocks
                    .iter()
                    .position(|(i, _)| *i == index)
                    .expect("刚查到这个 index");
                if let Block::ToolUse(buf) = c.blocks.remove(pos).1 {
                    c.tool_calls.push(tool_call_of(buf));
                }
            }
        }
        "message_delta" => {
            if let Some(d) = v.get("delta") {
                if let Some(r) = d.get("stop_reason").and_then(Value::as_str) {
                    c.finish = Some(map_stop_reason(r));
                }
            }
            if let Some(u) = v.get("usage") {
                if let Some(n) = u32_at(u, "output_tokens") {
                    c.usage.output_tokens = n;
                    c.usage_measured = true;
                }
            }
        }
        "error" => {
            let msg = v
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("上游报了一个错误")
                .to_string();
            c.error = Some(msg);
        }
        // message_stop / ping：不关心。
        _ => {}
    }
}

fn tool_call_of(buf: ToolUseBuf) -> ToolCall {
    ToolCall {
        id: if buf.id.is_empty() {
            "toolu_0".into()
        } else {
            buf.id
        },
        name: if buf.name.is_empty() {
            "tool".into()
        } else {
            buf.name
        },
        // 半截或空的 args 交出 `{}`：客户端对着 `{"pa` 只能解析失败。
        arguments: if buf.json.trim().is_empty() {
            "{}".into()
        } else {
            buf.json
        },
    }
}

/// 收尾：没等到 `content_block_stop` 的 tool_use 也交出去 —— 上游偶尔直接断流。
fn finish_tool_blocks(c: &mut Collected) {
    for (_, b) in std::mem::take(&mut c.blocks) {
        if let Block::ToolUse(buf) = b {
            c.tool_calls.push(tool_call_of(buf));
        }
    }
}

async fn read_sse(
    mut res: reqwest::Response,
    started: Instant,
    idle: Duration,
    model: &str,
    on_delta: DeltaSink<'_>,
) -> Result<Completion, UpstreamError> {
    let mut decoder = SseDecoder::default();
    let mut c = Collected::default();
    loop {
        let chunk = match tokio::time::timeout(idle, res.chunk()).await {
            Ok(Ok(Some(chunk))) => chunk,
            Ok(Ok(None)) => break,
            Ok(Err(e)) => {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    502,
                    format!("ZCode 流中断：{e}"),
                ));
            }
            Err(_) => {
                return Err(UpstreamError::new(
                    UpstreamKind::Timeout,
                    504,
                    format!("ZCode {} 秒没有动静", idle.as_secs()),
                ));
            }
        };
        for ev in decoder.push(&chunk) {
            handle_event(&ev, &mut c, started, on_delta);
        }
    }
    for ev in decoder.finish() {
        handle_event(&ev, &mut c, started, on_delta);
    }

    if let Some(msg) = c.error {
        return Err(UpstreamError::new(
            UpstreamKind::Upstream,
            502,
            format!("ZCode：{msg}"),
        ));
    }

    finish_tool_blocks(&mut c);

    // 上游没报 output_tokens 时估一个，并标明是估的。
    if c.usage.output_tokens == 0 {
        c.usage.output_tokens = crate::normalized::estimate_tokens(&c.text);
    }
    if !c.thinking.is_empty() {
        c.usage.reasoning_tokens = crate::normalized::estimate_tokens(&c.thinking);
    }
    let finish = c.finish.unwrap_or(if c.tool_calls.is_empty() {
        FinishReason::Stop
    } else {
        FinishReason::ToolCalls
    });
    // 有工具调用就是 tool_calls，哪怕上游说 end_turn —— 客户端按这个字段决定要不要执行工具。
    let finish = if !c.tool_calls.is_empty() {
        FinishReason::ToolCalls
    } else {
        finish
    };

    Ok(Completion {
        text: c.text,
        thinking: c.thinking,
        tool_calls: c.tool_calls,
        finish_reason: finish,
        usage: c.usage,
        usage_measured: c.usage_measured,
        routed_model: c.routed_model.or_else(|| Some(model.to_string())),
        ttft_ms: c.ttft,
        turn_ms: started.elapsed().as_millis() as u64,
        raw_response: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalized::{Sampling, ToolDef, ToolResult};

    fn req(messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: "glm-4.7".into(),
            messages,
            ..Default::default()
        }
    }

    #[test]
    fn system_goes_to_the_top_level_not_into_messages() {
        let body = build_body(
            &req(vec![
                Message::text(Role::System, "be brief"),
                Message::text(Role::User, "hi"),
            ]),
            "glm-4.7",
            ZcodePlan::CodingPlan,
        );
        assert_eq!(body["system"][0]["text"], "be brief");
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1, "system 不占一条消息");
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[0]["content"][0]["text"], "hi");
    }

    #[test]
    fn several_system_messages_are_joined_rather_than_last_one_wins() {
        let body = build_body(
            &req(vec![
                Message::text(Role::System, "a"),
                Message::text(Role::System, "b"),
                Message::text(Role::User, "hi"),
            ]),
            "glm-4.7",
            ZcodePlan::CodingPlan,
        );
        assert_eq!(body["system"][0]["text"], "a\n\nb");
    }

    #[test]
    fn consecutive_same_role_turns_are_merged() {
        // Anthropic 拒收连续同角色的消息。
        let body = build_body(
            &req(vec![
                Message::text(Role::User, "one"),
                Message::text(Role::User, "two"),
            ]),
            "glm-4.7",
            ZcodePlan::CodingPlan,
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn tool_results_become_a_user_turn_and_pair_with_the_call() {
        let mut asst = Message::text(Role::Assistant, "let me look");
        asst.tool_calls.push(ToolCall {
            id: "toolu_9".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"a.rs"}"#.into(),
        });
        let mut tool = Message::text(Role::Tool, "");
        tool.tool_results.push(ToolResult {
            tool_call_id: "toolu_9".into(),
            tool_name: "read_file".into(),
            text: "fn main(){}".into(),
            is_error: false,
        });
        let body = build_body(
            &req(vec![Message::text(Role::User, "read a.rs"), asst, tool]),
            "glm-4.7",
            ZcodePlan::CodingPlan,
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
        assert_eq!(msgs[1]["content"][1]["input"]["path"], "a.rs");
        // 关键：工具结果必须落在 user 里，落在 tool 角色上是 400。
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "toolu_9");
    }

    #[test]
    fn a_conversation_starting_with_an_assistant_turn_gets_a_user_stub() {
        // Anthropic 要求首条是 user。
        let body = build_body(
            &req(vec![Message::text(Role::Assistant, "hello")]),
            "glm-4.7",
            ZcodePlan::CodingPlan,
        );
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(msgs[1]["role"], "assistant");
    }

    #[test]
    fn max_tokens_is_always_present_and_comes_from_the_catalog() {
        // Anthropic 里 max_tokens 必填；缺省时不能填一个通用小值。
        let body = build_body(
            &req(vec![Message::text(Role::User, "hi")]),
            "glm-4.7",
            ZcodePlan::CodingPlan,
        );
        assert_eq!(body["max_tokens"], 131_072);

        let mut r = req(vec![Message::text(Role::User, "hi")]);
        r.sampling = Sampling {
            max_output_tokens: Some(256),
            ..Default::default()
        };
        let body = build_body(&r, "glm-4.7", ZcodePlan::CodingPlan);
        assert_eq!(body["max_tokens"], 256, "客户端给了就用它的");
    }

    #[test]
    fn only_the_53_family_carries_effort_and_a_thinking_budget() {
        let body = build_body(
            &req(vec![Message::text(Role::User, "hi")]),
            "glm-5.3",
            ZcodePlan::CodingPlan,
        );
        assert_eq!(body["output_config"]["effort"], "max");
        assert_eq!(body["thinking"]["type"], "enabled");
        assert!(body["thinking"]["budget_tokens"].as_u64().unwrap() >= 1024);

        let body = build_body(
            &req(vec![Message::text(Role::User, "hi")]),
            "glm-5.2",
            ZcodePlan::CodingPlan,
        );
        assert!(body.get("output_config").is_none(), "5.2 不认 effort");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn a_small_max_tokens_clamps_the_thinking_budget_below_it() {
        // budget 超过 max_tokens 上游会 400。
        let mut r = req(vec![Message::text(Role::User, "hi")]);
        r.sampling = Sampling {
            max_output_tokens: Some(4_000),
            ..Default::default()
        };
        let body = build_body(&r, "glm-5.3", ZcodePlan::CodingPlan);
        assert_eq!(body["thinking"]["budget_tokens"], 4_000);
    }

    #[test]
    fn tool_choice_maps_required_to_any() {
        let mut r = req(vec![Message::text(Role::User, "hi")]);
        r.tools = vec![ToolDef {
            name: "f".into(),
            parameters: json!({ "type": "object" }),
            ..Default::default()
        }];
        r.tool_choice = ToolChoice::Required;
        let body = build_body(&r, "glm-4.7", ZcodePlan::CodingPlan);
        assert_eq!(body["tool_choice"]["type"], "any", "Anthropic 管它叫 any");
        assert_eq!(body["tools"][0]["input_schema"]["type"], "object");

        r.tool_choice = ToolChoice::Tool("f".into());
        let body = build_body(&r, "glm-4.7", ZcodePlan::CodingPlan);
        assert_eq!(body["tool_choice"], json!({ "type": "tool", "name": "f" }));
    }

    // ---- 回程 ----

    fn ev(event: &str, data: Value) -> SseEvent {
        SseEvent {
            event: event.into(),
            data: data.to_string(),
        }
    }

    fn drain(events: Vec<SseEvent>) -> (Collected, Vec<Delta>) {
        let started = Instant::now();
        let mut c = Collected::default();
        let mut seen = Vec::new();
        {
            let mut sink = |d: Delta| seen.push(d);
            for e in &events {
                handle_event(e, &mut c, started, &mut sink);
            }
        }
        finish_tool_blocks(&mut c);
        (c, seen)
    }

    #[test]
    fn text_and_thinking_stream_separately_and_usage_is_measured() {
        let (c, seen) = drain(vec![
            ev(
                "message_start",
                json!({ "message": { "model": "glm-4.7", "usage": { "input_tokens": 12, "cache_read_input_tokens": 5 } } }),
            ),
            ev(
                "content_block_start",
                json!({ "index": 0, "content_block": { "type": "thinking" } }),
            ),
            ev(
                "content_block_delta",
                json!({ "index": 0, "delta": { "type": "thinking_delta", "thinking": "hmm" } }),
            ),
            ev("content_block_stop", json!({ "index": 0 })),
            ev(
                "content_block_start",
                json!({ "index": 1, "content_block": { "type": "text" } }),
            ),
            ev(
                "content_block_delta",
                json!({ "index": 1, "delta": { "type": "text_delta", "text": "hi " } }),
            ),
            ev(
                "content_block_delta",
                json!({ "index": 1, "delta": { "type": "text_delta", "text": "there" } }),
            ),
            ev(
                "message_delta",
                json!({ "delta": { "stop_reason": "end_turn" }, "usage": { "output_tokens": 7 } }),
            ),
            ev("message_stop", json!({})),
        ]);
        assert_eq!(c.text, "hi there");
        assert_eq!(c.thinking, "hmm");
        assert_eq!(c.usage.input_tokens, 12);
        assert_eq!(c.usage.cache_read_tokens, 5);
        assert_eq!(c.usage.output_tokens, 7);
        assert!(c.usage_measured);
        assert_eq!(c.finish, Some(FinishReason::Stop));
        assert_eq!(c.routed_model.as_deref(), Some("glm-4.7"));
        assert_eq!(
            seen,
            vec![
                Delta::Thinking("hmm".into()),
                Delta::Text("hi ".into()),
                Delta::Text("there".into()),
            ]
        );
    }

    #[test]
    fn a_tool_call_is_assembled_from_json_fragments_and_not_streamed() {
        let (c, seen) = drain(vec![
            ev(
                "content_block_start",
                json!({ "index": 0, "content_block": { "type": "tool_use", "id": "toolu_1", "name": "read_file" } }),
            ),
            ev(
                "content_block_delta",
                json!({ "index": 0, "delta": { "type": "input_json_delta", "partial_json": "{\"pa" } }),
            ),
            ev(
                "content_block_delta",
                json!({ "index": 0, "delta": { "type": "input_json_delta", "partial_json": "th\":\"a.rs\"}" } }),
            ),
            ev("content_block_stop", json!({ "index": 0 })),
            ev(
                "message_delta",
                json!({ "delta": { "stop_reason": "tool_use" } }),
            ),
        ]);
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].id, "toolu_1");
        assert_eq!(c.tool_calls[0].name, "read_file");
        assert_eq!(c.tool_calls[0].arguments, r#"{"path":"a.rs"}"#);
        assert!(seen.is_empty(), "工具调用不流式下发，半截 JSON 没有用");
    }

    #[test]
    fn a_tool_call_cut_off_before_its_stop_is_still_delivered() {
        let (c, _) = drain(vec![
            ev(
                "content_block_start",
                json!({ "index": 0, "content_block": { "type": "tool_use", "id": "toolu_1", "name": "f" } }),
            ),
            ev(
                "content_block_delta",
                json!({ "index": 0, "delta": { "type": "input_json_delta", "partial_json": "{}" } }),
            ),
            // 上游直接断流，没有 content_block_stop。
        ]);
        assert_eq!(c.tool_calls.len(), 1);
        assert_eq!(c.tool_calls[0].arguments, "{}");
    }

    #[test]
    fn an_empty_tool_input_becomes_an_empty_object() {
        let (c, _) = drain(vec![
            ev(
                "content_block_start",
                json!({ "index": 0, "content_block": { "type": "tool_use", "id": "t", "name": "f" } }),
            ),
            ev("content_block_stop", json!({ "index": 0 })),
        ]);
        assert_eq!(c.tool_calls[0].arguments, "{}");
    }

    #[test]
    fn an_error_frame_is_captured_rather_than_silently_ending_the_turn() {
        let (c, _) = drain(vec![ev(
            "error",
            json!({ "type": "error", "error": { "type": "overloaded_error", "message": "too busy" } }),
        )]);
        assert_eq!(c.error.as_deref(), Some("too busy"));
    }

    #[test]
    fn events_without_an_event_line_are_read_from_the_payload_type() {
        // 上游偶尔只发 data，不发 event。
        let (c, _) = drain(vec![ev(
            "",
            json!({ "type": "content_block_delta", "index": 0, "delta": { "type": "text_delta", "text": "x" } }),
        )]);
        // index 0 没有 content_block_start 也要认这个 text_delta。
        assert_eq!(c.text, "x");
    }

    #[test]
    fn stop_reasons_map_to_the_internal_names() {
        assert_eq!(map_stop_reason("tool_use"), FinishReason::ToolCalls);
        assert_eq!(map_stop_reason("max_tokens"), FinishReason::Length);
        assert_eq!(map_stop_reason("end_turn"), FinishReason::Stop);
        assert_eq!(map_stop_reason("stop_sequence"), FinishReason::Stop);
        assert_eq!(map_stop_reason("refusal"), FinishReason::ContentFilter);
    }

    #[test]
    fn auth_failures_are_marked_so_the_lane_can_switch_accounts() {
        assert_eq!(map_status(401, "bad key").kind, UpstreamKind::Auth);
        assert_eq!(map_status(429, "slow down").kind, UpstreamKind::RateLimit);
        assert_eq!(map_status(400, "nope").kind, UpstreamKind::BadRequest);
        assert_eq!(map_status(503, "oops").kind, UpstreamKind::Upstream);
    }

    #[test]
    fn the_default_route_is_the_plan_that_needs_no_captcha() {
        let r = ZcodeRoute::default();
        assert_eq!(r.plan, ZcodePlan::CodingPlan);
        assert_eq!(r.provider, ZcodeProvider::Zai);
    }

    /// 拿本机官方客户端的真 key 打一次 `api.z.ai`，把整条转发链走通。
    ///
    /// 默认不跑：要联网、要本机装过官方 ZCode 客户端并登录过，而且会花掉真额度。
    ///
    /// `cargo test -p nexus-gateway -- --ignored --nocapture live_coding_plan`
    #[tokio::test]
    #[ignore = "要联网 + 本机 ZCode 登录态，会花真额度"]
    async fn live_coding_plan_round_trip() {
        let found = nexus_zcode::import::read_local(&nexus_zcode::import::credentials_path())
            .expect("读本机 ZCode 凭证");
        let coding: Vec<_> = found
            .iter()
            .filter(|a| a.plan == ZcodePlan::CodingPlan && a.api_key.is_some())
            .collect();
        assert!(!coding.is_empty(), "至少要有一个编码套餐的号");

        // 逐个试：个人版和团队版的额度是分开的，一个没钱不代表另一个也没有。
        let mut ok = 0;
        for a in &coding {
            let up = ZcodeUpstream::new(Arc::new(FixedRoute(ZcodeRoute {
                provider: a.provider,
                plan: ZcodePlan::CodingPlan,
            })));
            let cred = Credential {
                label: "live".into(),
                access_token: a.api_key.clone().expect("刚筛过"),
                identity: crate::identity::DeviceIdentity::derived("live"),
            };
            let mut request = req(vec![Message::text(
                Role::User,
                "Reply with exactly the word: pong",
            )]);
            request.sampling.max_output_tokens = Some(64);

            let mut pieces = Vec::new();
            let done = {
                let mut sink = |d: Delta| {
                    if let Delta::Text(t) = d {
                        pieces.push(t);
                    }
                };
                up.stream(&cred, &request, &mut sink).await
            };
            let family = a.family.as_deref().unwrap_or("-");
            match done {
                Ok(done) => {
                    ok += 1;
                    println!(
                        "[{family}] OK routed={:?} finish={:?} in={} out={} measured={} ttft={:?}ms chunks={} text={:?}",
                        done.routed_model,
                        done.finish_reason,
                        done.usage.input_tokens,
                        done.usage.output_tokens,
                        done.usage_measured,
                        done.ttft_ms,
                        pieces.len(),
                        done.text,
                    );
                    assert!(!done.text.trim().is_empty(), "要有正文");
                    assert!(!pieces.is_empty(), "要是流式下发的，不是一次性给完");
                    assert!(done.usage_measured, "上游应当报了真实用量");
                    assert!(done.usage.input_tokens > 0);
                }
                Err(err) => {
                    // 认证和请求体的问题要能和「这个号没钱」分开看：前者是我们写错了，
                    // 后者只是这个号不能用。
                    println!("[{family}] {:?} {} {}", err.kind, err.status, err.message);
                    assert_ne!(err.kind, UpstreamKind::Auth, "认证被拒说明头或 key 发错了");
                    assert_ne!(
                        err.kind,
                        UpstreamKind::BadRequest,
                        "请求体被拒说明出站格式写错了"
                    );
                }
            }
        }
        println!("{ok}/{} 个编码套餐号能出流量", coding.len());
    }

    #[tokio::test]
    async fn a_start_plan_account_fails_with_an_explanation_not_an_upstream_error() {
        let up = ZcodeUpstream::new(Arc::new(FixedRoute(ZcodeRoute {
            provider: ZcodeProvider::Zai,
            plan: ZcodePlan::StartPlan,
        })));
        let cred = Credential {
            label: "x".into(),
            access_token: "jwt".into(),
            identity: crate::identity::DeviceIdentity::derived("x"),
        };
        let mut sink = |_: Delta| {};
        let err = up
            .stream(
                &cred,
                &req(vec![Message::text(Role::User, "hi")]),
                &mut sink,
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind, UpstreamKind::BadRequest);
        assert!(err.message.contains("验证码"), "要说清为什么不支持");
    }
}
