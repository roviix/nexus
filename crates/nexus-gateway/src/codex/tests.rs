//! ChatGPT 通道的端到端：假 Codex 后端 + 真实 server，不联网。
//!
//! 考的是「到达上游的形状」与「回到客户端的形状」两端，中间的 lane / 路由 / 序列化都是真的。

use super::{CodexConfig, CodexUpstream};
use crate::channel::{self, Capability, Channel, ChannelGate, ChannelRegistry};
use crate::error::UpstreamError;
use crate::identity::DeviceIdentity;
use crate::lane::{BoxFuture, Credential, Lane, Outcome, StaticLane};
use crate::normalized::{ChatRequest, Completion, Delta, FinishReason, Usage};
use crate::server::{bind, router, serve, Gateway};
use crate::upstream::{DeltaSink, Upstream};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// 假上游的一次脚本：状态码、响应头、事件（200 时拼成 SSE）。
#[derive(Clone)]
struct Plan {
    status: u16,
    headers: Vec<(&'static str, String)>,
    events: Vec<Value>,
    json: Option<Value>,
}

impl Plan {
    fn ok(events: Vec<Value>) -> Self {
        Self {
            status: 200,
            headers: vec![],
            events,
            json: None,
        }
    }

    fn error(status: u16, json: Value) -> Self {
        Self {
            status,
            headers: vec![],
            events: vec![],
            json: Some(json),
        }
    }

    fn with_header(mut self, k: &'static str, v: &str) -> Self {
        self.headers.push((k, v.to_string()));
        self
    }
}

#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<(HashMap<String, String>, Value)>>,
    queue: Mutex<Vec<Plan>>,
}

async fn responses(State(f): State<Arc<Fake>>, headers: HeaderMap, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let hs: HashMap<String, String> = headers
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    f.calls.lock().unwrap().push((hs, body));
    let plan = {
        let mut q = f.queue.lock().unwrap();
        if q.is_empty() {
            Plan::ok(happy(true))
        } else {
            q.remove(0)
        }
    };
    let mut res = if plan.status >= 400 {
        (
            axum::http::StatusCode::from_u16(plan.status).unwrap(),
            [("content-type", "application/json")],
            plan.json
                .unwrap_or(json!({ "error": { "message": "nope" } }))
                .to_string(),
        )
            .into_response()
    } else {
        let mut sse = String::new();
        for ev in &plan.events {
            sse.push_str(&format!(
                "event: {}\ndata: {}\n\n",
                ev["type"].as_str().unwrap_or(""),
                ev
            ));
        }
        (
            axum::http::StatusCode::OK,
            [("content-type", "text/event-stream")],
            sse,
        )
            .into_response()
    };
    for (k, v) in plan.headers {
        res.headers_mut()
            .insert(axum::http::HeaderName::from_static(k), v.parse().unwrap());
    }
    res
}

/// 一段「思考 + 正文 + 一个工具调用」的 Responses 事件流。
fn happy(tool: bool) -> Vec<Value> {
    let mut ev = vec![
        json!({ "type": "response.created", "response": { "id": "resp_1", "model": "gpt-5.4", "status": "in_progress" } }),
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "id": "rs_1", "type": "reasoning", "summary": [] } }),
        json!({ "type": "response.reasoning_summary_text.delta", "item_id": "rs_1", "output_index": 0, "delta": "想一想" }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": { "id": "rs_1", "type": "reasoning", "summary": [{ "type": "summary_text", "text": "想一想" }], "encrypted_content": "gAAAAAblob" } }),
        json!({ "type": "response.output_item.added", "output_index": 1, "item": { "id": "msg_1", "type": "message", "role": "assistant", "content": [] } }),
        json!({ "type": "response.output_text.delta", "item_id": "msg_1", "output_index": 1, "delta": "你好，" }),
        json!({ "type": "response.output_text.delta", "item_id": "msg_1", "output_index": 1, "delta": "世界" }),
        json!({ "type": "response.output_item.done", "output_index": 1, "item": { "id": "msg_1", "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "你好，世界" }] } }),
    ];
    if tool {
        ev.push(json!({ "type": "response.output_item.done", "output_index": 2, "item": { "id": "fc_1", "type": "function_call", "call_id": "call_abc", "name": "python__nexus", "arguments": "{\"code\":\"1\"}" } }));
    }
    ev.push(json!({ "type": "response.completed", "response": {
        "id": "resp_1", "model": "gpt-5.4", "status": "completed",
        "output": [
            { "id": "rs_1", "type": "reasoning", "summary": [{ "type": "summary_text", "text": "想一想" }], "encrypted_content": "gAAAAAblob" },
            { "id": "msg_1", "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "你好，世界" }] },
        ],
        "usage": { "input_tokens": 120, "output_tokens": 30, "input_tokens_details": { "cached_tokens": 100 }, "output_tokens_details": { "reasoning_tokens": 12 } }
    } }));
    ev
}

fn jwt_with_account(account: &str) -> String {
    use base64::Engine;
    let b64 = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes());
    format!(
        "{}.{}.sig",
        b64(r#"{"alg":"RS256"}"#),
        b64(&json!({ "exp": 4_000_000_000u64, "https://api.openai.com/auth": { "chatgpt_account_id": account } }).to_string())
    )
}

/// Cursor 那一侧的假后端：记下它收到了什么，好证明路由没把 Codex 的请求送错地方。
struct CursorFake {
    seen: Mutex<Vec<String>>,
}

impl Upstream for CursorFake {
    fn image<'a>(
        &'a self,
        _credential: &'a Credential,
        request: &'a crate::images::ImageRequest,
    ) -> BoxFuture<'a, Result<crate::images::GeneratedImage, UpstreamError>> {
        Box::pin(async move {
            self.seen
                .lock()
                .unwrap()
                .push(format!("image:{}", request.model));
            Ok(crate::images::GeneratedImage {
                b64: "Q1VSU09S".into(),
                mime: "image/png".into(),
                size: Some((1536, 1024)),
                revised_prompt: None,
            })
        })
    }

    fn stream<'a>(
        &'a self,
        _credential: &'a Credential,
        request: &'a ChatRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
        Box::pin(async move {
            self.seen.lock().unwrap().push(request.model.clone());
            on_delta(Delta::Text("from cursor".into()));
            Ok(Completion {
                text: "from cursor".into(),
                thinking: String::new(),
                tool_calls: vec![],
                finish_reason: FinishReason::Stop,
                usage: Usage::default(),
                usage_measured: false,
                routed_model: Some("auto".into()),
                ttft_ms: None,
                turn_ms: 1,
                raw_response: None,
            })
        })
    }
}

/// 记回报的 lane：证明 429 的重置时刻、401 的分类都到了 lane 手里。
struct RecordingLane {
    inner: StaticLane,
    reports: Mutex<Vec<(String, Option<i64>)>>,
}

impl Lane for RecordingLane {
    fn acquire<'a>(&'a self, model: &'a str) -> BoxFuture<'a, Result<Credential, UpstreamError>> {
        self.inner.acquire(model)
    }

    fn report(&self, credential: &Credential, model: &str, outcome: Outcome<'_>) {
        let (kind, reset) = match &outcome {
            Outcome::Ok(_) => ("ok".to_string(), None),
            Outcome::Err(e) => (e.kind.as_str().to_string(), e.reset_at_ms),
        };
        self.reports.lock().unwrap().push((kind, reset));
        self.inner.report(credential, model, outcome);
    }
}

/// 测试用门禁：有没有号、额外认哪些模型都写死。
struct StaticGate {
    ready: bool,
    extra_models: Vec<String>,
}

impl ChannelGate for StaticGate {
    fn ready(&self) -> bool {
        self.ready
    }

    fn owns(&self, cap: Capability, base_model: &str) -> bool {
        match cap {
            Capability::Chat => {
                let base = super::protocol::split_effort_suffix(base_model).0;
                super::protocol::is_codex_model(base) || self.extra_models.iter().any(|m| m == base)
            }
            Capability::Image => super::protocol::is_codex_image_model(base_model),
            Capability::Video => false,
        }
    }

    fn models(&self, cap: Capability) -> Vec<String> {
        match cap {
            Capability::Chat => super::protocol::CODEX_MODELS
                .iter()
                .map(|s| s.to_string())
                .chain(self.extra_models.iter().cloned())
                .collect(),
            Capability::Image => super::protocol::CODEX_IMAGE_MODELS
                .iter()
                .map(|s| s.to_string())
                .collect(),
            Capability::Video => vec![],
        }
    }
}

struct Harness {
    base: String,
    fake: Arc<Fake>,
    cursor: Arc<CursorFake>,
    lane: Arc<RecordingLane>,
}

async fn spawn(ready: bool) -> Harness {
    spawn_with_models(ready, vec![]).await
}

async fn spawn_with_models(ready: bool, extra_models: Vec<String>) -> Harness {
    let fake = Arc::new(Fake::default());
    let app = Router::new()
        .route("/backend-api/codex/responses", post(responses))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let up_addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let credential = Credential {
        label: "alice@example.com".into(),
        access_token: jwt_with_account("acct_A"),
        identity: DeviceIdentity::derived("x"),
    };
    let lane = Arc::new(RecordingLane {
        inner: StaticLane::new(credential),
        reports: Mutex::new(vec![]),
    });
    let cursor = Arc::new(CursorFake {
        seen: Mutex::new(vec![]),
    });
    let cfg = CodexConfig {
        backend_url: format!("http://{up_addr}/backend-api"),
        ..CodexConfig::default()
    };
    let cursor_lane: Arc<dyn Lane> = Arc::new(StaticLane::new(Credential {
        label: "cursor@example.com".into(),
        access_token: "tok".into(),
        identity: DeviceIdentity::derived("tok"),
    }));
    let channels =
        ChannelRegistry::new(channel::cursor_channel(cursor_lane, cursor.clone())).with(Channel {
            id: channel::CHATGPT,
            label: "ChatGPT",
            vendor: "openai",
            prefixes: super::protocol::ROUTE_PREFIXES,
            lane: lane.clone(),
            upstream: Arc::new(CodexUpstream::new(cfg, None)),
            gate: Arc::new(StaticGate {
                ready,
                extra_models,
            }),
            passthrough: true,
        });
    // 这些用例测的是 ChatGPT 通道本身：有号时把它设成默认，裸名继续打进来。
    if ready {
        channels.set_default(channel::CHATGPT).unwrap();
    }
    let gw = Arc::new(Gateway {
        channels,
        api_key: None,
        ledger: None,
        media_jobs: None,
    });
    let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, router(gw), std::future::pending()));
    Harness {
        base: format!("http://{addr}"),
        fake,
        cursor,
        lane,
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn sse_events(text: &str) -> Vec<(String, Value)> {
    text.split("\n\n")
        .filter_map(|block| {
            let ev = block.lines().find_map(|l| l.strip_prefix("event: "))?;
            let data = block.lines().find_map(|l| l.strip_prefix("data: "))?;
            Some((ev.to_string(), serde_json::from_str(data).ok()?))
        })
        .collect()
}

#[tokio::test]
async fn responses_passthrough_reaches_upstream_as_codex_and_comes_back_verbatim() {
    let h = spawn(true).await;
    h.fake.queue.lock().unwrap().push(
        Plan::ok(happy(true))
            .with_header("x-codex-turn-state", "state-A")
            .with_header("x-codex-primary-used-percent", "41"),
    );
    let res = http()
        .post(format!("{}/v1/responses", h.base))
        .header("x-codex-installation-id", "11111111-1111-4111-8111-111111111111")
        .header("x-codex-beta-features", "remote_compaction_v2,foo")
        .json(&json!({
            "model": "gpt-5.4-high",
            "stream": true,
            "store": true,
            "temperature": 0.3,
            "instructions": "You are Codex.",
            "prompt_cache_key": "7d1e9c1a-1111-4222-8333-444455556666",
            "client_metadata": { "x-codex-installation-id": "11111111-1111-4111-8111-111111111111", "session_id": "7d1e9c1a-1111-4222-8333-444455556666" },
            "input": [
                { "id": "msg_0", "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] },
                { "id": "rs_0", "type": "reasoning", "summary": [], "encrypted_content": "gAAAAAprev" },
                { "type": "compaction_trigger" }
            ],
            "tools": [{ "type": "function", "name": "python", "parameters": { "type": "object" } }],
            "include": ["reasoning.encrypted_content"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    // 上游铸的印打上标记转给客户端；额度头原样。
    let turn = res
        .headers()
        .get("x-codex-turn-state")
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        turn.starts_with("nx1.") && turn.ends_with(".state-A"),
        "{turn}"
    );
    assert_eq!(
        res.headers().get("x-codex-primary-used-percent").unwrap(),
        "41"
    );
    let text = res.text().await.unwrap();

    // 到达上游的形状。
    let (headers, body) = h.fake.calls.lock().unwrap()[0].clone();
    assert_eq!(headers["chatgpt-account-id"], "acct_A");
    assert_eq!(headers["originator"], "codex-tui");
    assert!(headers["user-agent"].starts_with("codex-tui/"));
    assert_eq!(headers["openai-beta"], "responses=experimental");
    assert_eq!(
        headers["x-codex-beta-features"], "remote_compaction_v2,foo",
        "客户端声明了就原样"
    );
    assert!(headers["authorization"].starts_with("Bearer ey"));
    let scoped_inst = &headers["x-codex-installation-id"];
    assert_ne!(
        scoped_inst, "11111111-1111-4111-8111-111111111111",
        "设备 id 换了脸"
    );
    assert_eq!(
        body["client_metadata"]["x-codex-installation-id"], *scoped_inst,
        "头和体一致"
    );
    assert_eq!(body["model"], "gpt-5.4");
    assert_eq!(body["reasoning"]["effort"], "high");
    assert_eq!(body["stream"], true);
    assert_eq!(body["store"], false);
    assert!(body.get("temperature").is_none());
    assert_eq!(body["instructions"], "You are Codex.");
    assert_eq!(
        headers["session_id"],
        body["prompt_cache_key"].as_str().unwrap(),
        "session_id 与 prompt_cache_key 一致"
    );
    assert_ne!(
        body["prompt_cache_key"], "7d1e9c1a-1111-4222-8333-444455556666",
        "缓存键按账号换了脸"
    );
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 3);
    assert!(input.iter().all(|i| i.get("id").is_none()));
    assert_eq!(
        input[1]["encrypted_content"], "gAAAAAprev",
        "回放凭据原样带给上游"
    );
    assert_eq!(
        input[2]["type"], "compaction_trigger",
        "中间表示放不下的项透传"
    );
    assert_eq!(body["tools"][0]["name"], "python__nexus", "保留字改名");
    assert_eq!(body["include"][0], "reasoning.encrypted_content");

    // 回到客户端的形状：上游事件原样，工具名还原，凭据在。
    let events = sse_events(&text);
    let names: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
    assert_eq!(names.first().copied(), Some("response.created"));
    assert_eq!(names.last().copied(), Some("response.completed"));
    assert!(names.contains(&"response.reasoning_summary_text.delta"));
    let done: Vec<&Value> = events
        .iter()
        .filter(|(e, _)| e == "response.output_item.done")
        .map(|(_, d)| &d["item"])
        .collect();
    assert_eq!(done[0]["encrypted_content"], "gAAAAAblob");
    assert_eq!(done[0]["id"], "rs_1", "上游的 id 原样");
    assert_eq!(done[2]["name"], "python", "回程还原成客户端声明的名字");
    let completed = &events.last().unwrap().1;
    assert_eq!(completed["response"]["usage"]["input_tokens"], 120);
    assert!(
        h.cursor.seen.lock().unwrap().is_empty(),
        "没碰 Cursor 那条通道"
    );
    assert_eq!(h.lane.reports.lock().unwrap()[0].0, "ok");

    // 第二轮：客户端回带打了标记的印 → 上游收到裸印。
    h.fake.queue.lock().unwrap().push(Plan::ok(happy(false)));
    let res = http()
        .post(format!("{}/v1/responses", h.base))
        .header("x-codex-turn-state", &turn)
        .json(&json!({ "model": "gpt-5.4", "stream": true, "input": "again" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let _ = res.text().await;
    let (headers, _) = h.fake.calls.lock().unwrap()[1].clone();
    assert_eq!(headers["x-codex-turn-state"], "state-A");
}

#[tokio::test]
async fn non_stream_passthrough_returns_the_upstream_response_object() {
    let h = spawn(true).await;
    let res = http()
        .post(format!("{}/v1/responses", h.base))
        .json(&json!({ "model": "chatgpt/gpt-5.4", "input": "hi", "stream": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["id"], "resp_1", "上游最终的 response 对象原样回");
    assert_eq!(body["status"], "completed");
    assert_eq!(body["output"][0]["encrypted_content"], "gAAAAAblob");
    assert_eq!(body["output"][1]["content"][0]["text"], "你好，世界");
    let (_, sent) = h.fake.calls.lock().unwrap()[0].clone();
    assert_eq!(sent["stream"], true, "上游只讲流式，非流式由我们聚合");
    assert_eq!(sent["input"][0]["content"], "hi");
}

#[tokio::test]
async fn chat_and_anthropic_clients_are_bridged_onto_the_responses_shape() {
    let h = spawn(true).await;
    h.fake.queue.lock().unwrap().push(Plan::ok(happy(false)));
    let res = http()
        .post(format!("{}/v1/chat/completions", h.base))
        .json(&json!({
            "model": "gpt-5.4-xhigh",
            "messages": [{ "role": "system", "content": "be terse" }, { "role": "user", "content": "hello" }],
            "temperature": 0.1
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["object"], "chat.completion");
    assert_eq!(body["choices"][0]["message"]["content"], "你好，世界");
    assert_eq!(body["usage"]["prompt_tokens"], 120);
    let (_, sent) = h.fake.calls.lock().unwrap()[0].clone();
    assert_eq!(sent["instructions"], "be terse");
    assert_eq!(sent["input"][0]["role"], "user");
    assert_eq!(sent["input"][0]["content"][0]["text"], "hello");
    assert_eq!(sent["reasoning"]["effort"], "xhigh");
    assert_eq!(sent["reasoning"]["summary"], "auto");
    assert!(sent.get("temperature").is_none(), "采样参数上游不收");
    assert_eq!(sent["store"], false);

    // Anthropic 流式：思考与正文都到，事件序列完整。
    h.fake.queue.lock().unwrap().push(Plan::ok(happy(false)));
    let text = http()
        .post(format!("{}/v1/messages", h.base))
        .json(&json!({ "model": "gpt-5.4", "stream": true, "max_tokens": 5, "messages": [{ "role": "user", "content": "x" }] }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        text.contains("message_start") && text.contains("message_stop"),
        "{text}"
    );
    assert!(text.contains("thinking_delta"), "思考也桥接出去了：{text}");
    assert!(text.contains("世界"), "{text}");
}

#[tokio::test]
async fn codex_models_route_to_chatgpt_only_when_the_channel_is_ready() {
    // 有号：默认通道是 ChatGPT，裸名走它；要 Cursor 得写 cursor/。
    let h = spawn(true).await;
    for (model, expect_cursor) in [
        ("gpt-5.4", false),
        ("chatgpt/gpt-5.4", false),
        ("cursor/claude-sonnet-5", true),
        ("cursor/gpt-5.6-sol", true),
        ("gpt-5.6-sol", false),
    ] {
        let res = http()
            .post(format!("{}/v1/chat/completions", h.base))
            .json(&json!({ "model": model, "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "{model}");
        let body: Value = res.json().await.unwrap();
        let text = body["choices"][0]["message"]["content"].as_str().unwrap();
        if expect_cursor {
            assert_eq!(text, "from cursor", "{model} 该走 Cursor");
        } else {
            assert_eq!(text, "你好，世界", "{model} 该走 ChatGPT");
        }
    }
    let models: Value = http()
        .get(format!("{}/v1/models", h.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"chatgpt/gpt-5.4"), "Codex 模型进目录");
    assert!(ids.contains(&"cursor/claude-sonnet-5"));
    assert!(ids.contains(&"cursor/gpt-5.6-sol"));
    assert!(ids.contains(&"chatgpt/gpt-5.6-sol"));

    // 上游目录里拉到的新模型（静态表里没有）也归 ChatGPT，且出现在 /v1/models 里。
    let h3 = spawn_with_models(true, vec!["gpt-7-new".to_string()]).await;
    h3.fake.queue.lock().unwrap().push(Plan::ok(happy(false)));
    let res = http()
        .post(format!("{}/v1/chat/completions", h3.base))
        .json(
            &json!({ "model": "chatgpt/gpt-7-new-high", "messages": [{ "role": "user", "content": "x" }] }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "你好，世界");
    let (_, sent) = h3.fake.calls.lock().unwrap()[0].clone();
    assert_eq!(sent["model"], "gpt-7-new", "档位后缀剥掉了");
    assert_eq!(sent["reasoning"]["effort"], "high");
    let models: Value = http()
        .get(format!("{}/v1/models", h3.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(models["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == "chatgpt/gpt-7-new"));

    // 没号：同名 GPT 模型照旧走 Cursor；显式前缀才撞 ChatGPT 通道。
    let h2 = spawn(false).await;
    let res = http()
        .post(format!("{}/v1/chat/completions", h2.base))
        .json(&json!({ "model": "gpt-5.6-sol", "messages": [{ "role": "user", "content": "x" }] }))
        .send()
        .await
        .unwrap();
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], "from cursor");
    assert!(h2.fake.calls.lock().unwrap().is_empty());
    let models: Value = http()
        .get(format!("{}/v1/models", h2.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!models["data"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["id"] == "chatgpt/gpt-5.4"));
}

#[tokio::test]
async fn upstream_errors_are_classified_and_the_reset_time_reaches_the_lane() {
    let h = spawn(true).await;
    h.fake.queue.lock().unwrap().push(Plan::error(
        429,
        json!({ "error": { "type": "usage_limit_reached", "message": "You've hit your usage limit", "resets_in_seconds": 1800 } }),
    ));
    let res = http()
        .post(format!("{}/v1/responses", h.base))
        .json(&json!({ "model": "gpt-5.4", "input": "hi" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429);
    let body: Value = res.json().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("usage limit"));
    {
        let reports = h.lane.reports.lock().unwrap();
        assert_eq!(reports[0].0, "rate_limit");
        assert!(reports[0].1.is_some(), "重置时刻交给了 lane");
    }

    // 401 → auth；403 HTML → 不怪号（upstream）。
    h.fake
        .queue
        .lock()
        .unwrap()
        .push(Plan::error(401, json!({ "detail": "Unauthorized" })));
    let res = http()
        .post(format!("{}/v1/responses", h.base))
        .json(&json!({ "model": "gpt-5.4", "input": "hi" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    assert_eq!(h.lane.reports.lock().unwrap()[1].0, "auth");

    // 400 指着字段不认 → 修了再发，客户端无感；上游收到两次。
    let before = h.fake.calls.lock().unwrap().len();
    h.fake.queue.lock().unwrap().push(Plan::error(
        400,
        json!({ "error": { "code": "unknown_parameter", "param": "include", "message": "Unknown parameter" } }),
    ));
    h.fake.queue.lock().unwrap().push(Plan::ok(happy(false)));
    let res = http()
        .post(format!("{}/v1/responses", h.base))
        .json(&json!({ "model": "gpt-5.4", "input": "hi", "include": ["reasoning.encrypted_content"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let calls = h.fake.calls.lock().unwrap();
    assert_eq!(calls.len(), before + 2);
    assert!(calls[before].1.get("include").is_some());
    assert!(
        calls[before + 1].1.get("include").is_none(),
        "第二次没带上游不认的字段"
    );
}

#[tokio::test]
async fn gpt_image_requests_run_the_image_generation_tool_and_honour_the_size() {
    let h = spawn(true).await;
    // 一张 1x1 的 PNG（base64），让「量尺寸」那条路也走到。
    let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    h.fake.queue.lock().unwrap().push(Plan::ok(vec![
        json!({ "type": "response.created", "response": { "id": "resp_img", "model": "gpt-5.4-mini" } }),
        json!({ "type": "response.output_item.added", "output_index": 0, "item": { "id": "ig_1", "type": "image_generation_call", "status": "in_progress" } }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": {
            "id": "ig_1", "type": "image_generation_call", "status": "completed",
            "result": png_b64, "output_format": "png", "size": "1024x1536", "quality": "high",
            "revised_prompt": "A red panda painted in watercolor"
        } }),
        json!({ "type": "response.completed", "response": { "id": "resp_img", "status": "completed", "output": [], "usage": { "input_tokens": 50, "output_tokens": 0 } } }),
    ]));
    let res = http()
        .post(format!("{}/v1/images/generations", h.base))
        .json(&json!({ "model": "gpt-image-2", "prompt": "a red panda, watercolor", "size": "1024x1536", "quality": "high", "n": 1 }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert!(
        res.headers().get("x-nexus-size-ignored").is_none(),
        "gpt-image 认 size，不该说「已忽略」"
    );
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["data"][0]["b64_json"], png_b64);
    assert_eq!(
        body["data"][0]["revised_prompt"],
        "A red panda painted in watercolor"
    );

    let (headers, sent) = h.fake.calls.lock().unwrap()[0].clone();
    assert_eq!(headers["chatgpt-account-id"], "acct_A");
    assert_eq!(sent["model"], "gpt-5.4-mini", "由便宜的文本模型代调工具");
    assert_eq!(sent["tools"][0]["type"], "image_generation");
    assert_eq!(sent["tools"][0]["model"], "gpt-image-2");
    assert_eq!(sent["tools"][0]["size"], "1024x1536");
    assert_eq!(sent["tools"][0]["quality"], "high");
    assert_eq!(sent["tool_choice"]["type"], "image_generation");
    assert_eq!(
        sent["input"][0]["content"][0]["text"],
        "a red panda, watercolor"
    );
    assert!(
        h.cursor.seen.lock().unwrap().is_empty(),
        "没碰 Cursor 的出图"
    );

    // 模型只回了一段话没画：按内容策略的是 400，其余是「这个号出不了」。
    h.fake.queue.lock().unwrap().push(Plan::ok(vec![
        json!({ "type": "response.created", "response": { "id": "r2", "model": "gpt-5.4-mini" } }),
        json!({ "type": "response.output_item.done", "output_index": 0, "item": { "id": "m", "type": "message", "role": "assistant",
            "content": [{ "type": "output_text", "text": "I can't create that image because it violates our content policy." }] } }),
        json!({ "type": "response.completed", "response": { "id": "r2", "status": "completed", "output": [] } }),
    ]));
    let res = http()
        .post(format!("{}/v1/images/generations", h.base))
        .json(&json!({ "model": "gpt-image-2", "prompt": "something disallowed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    let body: Value = res.json().await.unwrap();
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("内容策略"));

    // Cursor 的出图模型不受影响，照旧走 Cursor 并报「size 已忽略」。
    let res = http()
        .post(format!("{}/v1/images/generations", h.base))
        .json(&json!({ "model": "cursor/nano-banana-2", "prompt": "x", "size": "1024x1024" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(
        res.headers().get("x-nexus-size-ignored").unwrap(),
        "1024x1024"
    );
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["data"][0]["b64_json"], "Q1VSU09S");
    assert_eq!(
        *h.cursor.seen.lock().unwrap(),
        vec!["image:nano-banana-2".to_string()]
    );

    // 没有 ChatGPT 号时 gpt-image-2 也退回 Cursor（它会按自己的能力处理）。
    let h2 = spawn(false).await;
    let res = http()
        .post(format!("{}/v1/images/generations", h2.base))
        .json(&json!({ "model": "gpt-image-2", "prompt": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    assert!(h2.fake.calls.lock().unwrap().is_empty());
    assert_eq!(
        *h2.cursor.seen.lock().unwrap(),
        vec!["image:gpt-image-2".to_string()]
    );
}

#[tokio::test]
async fn image_edits_accept_multipart_and_json_and_become_an_edit_tool_call() {
    let h = spawn(true).await;
    let edited = |b64: &str| {
        Plan::ok(vec![
            json!({ "type": "response.created", "response": { "id": "r", "model": "gpt-5.4-mini" } }),
            json!({ "type": "response.output_item.done", "output_index": 0, "item": {
                "id": "ig_e", "type": "image_generation_call", "status": "completed", "result": b64, "output_format": "png", "size": "1024x1024"
            } }),
            json!({ "type": "response.completed", "response": { "id": "r", "status": "completed", "output": [] } }),
        ])
    };

    // JSON 形态：image 是 data URL（可多张），mask 是 data URL。
    h.fake.queue.lock().unwrap().push(edited("RURJVEVE"));
    let res = http()
        .post(format!("{}/v1/images/edits", h.base))
        .json(&json!({
            "model": "gpt-image-2", "prompt": "make it night",
            "image": ["data:image/png;base64,AAAA", "data:image/jpeg;base64,BBBB"],
            "mask": "data:image/png;base64,MMMM",
            "size": "1024x1024"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["data"][0]["b64_json"], "RURJVEVE");
    let (_, sent) = h.fake.calls.lock().unwrap()[0].clone();
    assert_eq!(sent["tools"][0]["action"], "edit");
    assert_eq!(sent["tools"][0]["model"], "gpt-image-2");
    assert_eq!(
        sent["tools"][0]["input_image_mask"]["image_url"],
        "data:image/png;base64,MMMM"
    );
    let content = sent["input"][0]["content"].as_array().unwrap();
    assert_eq!(content.len(), 3);
    assert_eq!(content[0]["text"], "make it night");
    assert_eq!(content[1]["image_url"], "data:image/png;base64,AAAA");
    assert_eq!(content[2]["image_url"], "data:image/jpeg;base64,BBBB");

    // multipart 形态：OpenAI SDK 发的就是它。手拼一份。
    h.fake.queue.lock().unwrap().push(edited("TVVMVEk="));
    let boundary = "----nexus-test-boundary";
    let mut form = String::new();
    let part =
        |form: &mut String, name: &str, filename: Option<&str>, ctype: Option<&str>, body: &str| {
            form.push_str(&format!("--{boundary}\r\n"));
            match filename {
                Some(f) => form.push_str(&format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\n"
                )),
                None => form.push_str(&format!(
                    "Content-Disposition: form-data; name=\"{name}\"\r\n"
                )),
            }
            if let Some(c) = ctype {
                form.push_str(&format!("Content-Type: {c}\r\n"));
            }
            form.push_str("\r\n");
            form.push_str(body);
            form.push_str("\r\n");
        };
    part(&mut form, "model", None, None, "gpt-image-2");
    part(&mut form, "prompt", None, None, "add a hat");
    part(&mut form, "n", None, None, "1");
    part(&mut form, "quality", None, None, "low");
    part(
        &mut form,
        "image",
        Some("cat.png"),
        Some("image/png"),
        "PNGBYTES",
    );
    part(
        &mut form,
        "image",
        Some("dog.jpg"),
        Some("image/jpeg"),
        "JPGBYTES",
    );
    part(
        &mut form,
        "mask",
        Some("mask.png"),
        Some("image/png"),
        "MASKBYTES",
    );
    form.push_str(&format!("--{boundary}--\r\n"));
    let res = http()
        .post(format!("{}/v1/images/edits", h.base))
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(form)
        .send()
        .await
        .unwrap();
    assert_eq!(
        res.status(),
        200,
        "{}",
        res.text().await.unwrap_or_default()
    );
    let body: Value = res.json().await.unwrap();
    assert_eq!(body["data"][0]["b64_json"], "TVVMVEk=");
    let (_, sent) = h.fake.calls.lock().unwrap()[1].clone();
    assert_eq!(sent["tools"][0]["action"], "edit");
    assert_eq!(sent["tools"][0]["quality"], "low");
    let content = sent["input"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["text"], "add a hat");
    assert_eq!(
        content[1]["image_url"], "data:image/png;base64,UE5HQllURVM=",
        "文件按 part 的类型编成 data URL"
    );
    assert_eq!(
        content[2]["image_url"],
        "data:image/jpeg;base64,SlBHQllURVM="
    );
    assert_eq!(
        sent["tools"][0]["input_image_mask"]["image_url"],
        "data:image/png;base64,TUFTS0JZVEVT"
    );

    // 没有 image 是 400。
    let res = http()
        .post(format!("{}/v1/images/edits", h.base))
        .json(&json!({ "model": "gpt-image-2", "prompt": "x" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);
    assert_eq!(h.fake.calls.lock().unwrap().len(), 2, "没到上游");
}

#[tokio::test]
async fn in_stream_errors_become_gateway_errors_in_the_clients_dialect() {
    let h = spawn(true).await;
    h.fake.queue.lock().unwrap().push(Plan::ok(vec![
        json!({ "type": "response.created", "response": { "id": "r", "model": "gpt-5.4" } }),
        json!({ "type": "error", "code": "server_is_overloaded", "message": "try again" }),
    ]));
    let res = http()
        .post(format!("{}/v1/chat/completions", h.base))
        .json(&json!({ "model": "gpt-5.4", "messages": [{ "role": "user", "content": "x" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 429, "降载按限流处理");
    assert_eq!(h.lane.reports.lock().unwrap()[0].0, "rate_limit");

    // 透传里流中出错：已经吐过帧，只能在流里说，用 Responses 的 error 事件。
    h.fake.queue.lock().unwrap().push(Plan::ok(vec![
        json!({ "type": "response.created", "response": { "id": "r", "model": "gpt-5.4" } }),
        json!({ "type": "response.output_text.delta", "delta": "half" }),
        json!({ "type": "response.failed", "response": { "error": { "code": "invalid_prompt", "message": "too long" } } }),
    ]));
    let text = http()
        .post(format!("{}/v1/responses", h.base))
        .json(&json!({ "model": "gpt-5.4", "input": "x", "stream": true }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let events = sse_events(&text);
    assert!(events
        .iter()
        .any(|(e, _)| e == "response.output_text.delta"));
    let (last_ev, last) = events.last().unwrap();
    assert_eq!(last_ev, "error");
    assert_eq!(last["code"], "bad_request");
    assert!(last["message"].as_str().unwrap().contains("too long"));
}
