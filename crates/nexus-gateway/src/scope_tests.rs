//! 客户端作用域与供应商通道的端到端用例：真起一个网关、一个假供应商，全程走 HTTP。

use crate::channel::{cursor_channel, ChannelRegistry};
use crate::error::UpstreamError;
use crate::identity::DeviceIdentity;
use crate::lane::{BoxFuture, Credential, StaticLane};
use crate::normalized::{ChatRequest, Completion, FinishReason, Usage};
use crate::provider::{provider_channel, ProviderLane};
use crate::reqlog::RequestLog;
use crate::routes::{ClientRoute, ClientRoutes};
use crate::server::{bind, router, serve, Gateway};
use crate::upstream::{DeltaSink, Upstream};
use axum::body::{Body, Bytes};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::Response;
use nexus_store::key_providers::{self, ApiFormat, AuthField, KeyProviderInput};
use nexus_store::{Db, SecretStore, SqliteSecrets};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};

/// Cursor 通道的假后端：把收到的模型名当回答吐回去，好验证路由换成了什么。
struct Echo;

impl Upstream for Echo {
    fn stream<'a>(
        &'a self,
        _credential: &'a Credential,
        request: &'a ChatRequest,
        _on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
        Box::pin(async move {
            Ok(Completion {
                text: request.model.clone(),
                thinking: String::new(),
                tool_calls: vec![],
                finish_reason: FinishReason::Stop,
                usage: Usage::default(),
                usage_measured: false,
                routed_model: None,
                ttft_ms: None,
                turn_ms: 1,
                raw_response: None,
            })
        })
    }
}

#[derive(Debug, Clone)]
struct Seen {
    path: String,
    headers: HashMap<String, String>,
    body: Value,
}

const ANTHROPIC_SSE: &str = concat!(
    "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"up\",\"usage\":{\"input_tokens\":5,\"output_tokens\":0}}}\n\n",
    "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"pong\"}}\n\n",
    "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":1}}\n\n",
    "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
);

const CHAT_SSE: &str = concat!(
    "data: {\"choices\":[{\"delta\":{\"content\":\"pong\"}}]}\n\n",
    "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":1}}\n\n",
    "data: [DONE]\n\n",
);

/// 一个假供应商：记下收到的请求；`fail` 给了就一律回这个状态码。
async fn fake_provider(fail: Option<u16>) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let log = seen.clone();
    let app = axum::Router::new().fallback(move |uri: Uri, headers: HeaderMap, body: Bytes| {
        let log = log.clone();
        async move {
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let headers: HashMap<String, String> = headers
                .iter()
                .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                .collect();
            let path = uri.path().to_string();
            log.lock().unwrap().push(Seen {
                path: path.clone(),
                headers,
                body: body.clone(),
            });
            if let Some(code) = fail {
                return Response::builder()
                    .status(StatusCode::from_u16(code).unwrap())
                    .body(Body::from(r#"{"error":{"message":"upstream is down"}}"#))
                    .unwrap();
            }
            let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
            let (ctype, text) = if path.ends_with("/messages") {
                if stream {
                    ("text/event-stream", ANTHROPIC_SSE.to_string())
                } else {
                    (
                        "application/json",
                        json!({
                            "id": "msg_1", "type": "message", "role": "assistant", "model": "up",
                            "content": [{ "type": "text", "text": "pong" }],
                            "stop_reason": "end_turn",
                            "usage": { "input_tokens": 5, "output_tokens": 1 }
                        })
                        .to_string(),
                    )
                }
            } else if stream || path.contains("/sse-only/") {
                ("text/event-stream", CHAT_SSE.to_string())
            } else {
                (
                    "application/json",
                    json!({
                        "id": "chatcmpl-1", "object": "chat.completion", "model": "up",
                        "choices": [{ "index": 0, "message": { "role": "assistant", "content": "pong" }, "finish_reason": "stop" }],
                        "usage": { "prompt_tokens": 3, "completion_tokens": 1 }
                    })
                    .to_string(),
                )
            };
            Response::builder()
                .status(200)
                .header("content-type", ctype)
                .body(Body::from(text))
                .unwrap()
        }
    });
    let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, app, std::future::pending()));
    (format!("http://{addr}"), seen)
}

struct Env {
    base: String,
    log: Arc<RequestLog>,
}

async fn gateway(
    providers: Vec<(&str, &str, ApiFormat, Vec<&str>)>,
    routes: ClientRoutes,
    api_key: Option<&str>,
) -> Env {
    let db = Arc::new(Db::open_in_memory().unwrap());
    let secrets: Arc<dyn SecretStore> = Arc::new(SqliteSecrets::new(db.clone()));
    for (name, base, format, models) in providers {
        key_providers::save(
            &db,
            secrets.as_ref(),
            KeyProviderInput {
                id: None,
                name: name.into(),
                website: None,
                base_url: base.into(),
                api_format: format,
                auth_field: AuthField::AuthToken,
                models: models.into_iter().map(str::to_string).collect(),
                enabled: None,
                api_key: Some(format!("sk-{name}-secret")),
            },
        )
        .unwrap();
    }
    let lane = Arc::new(ProviderLane::new(db.clone(), secrets));
    let channels = ChannelRegistry::new(cursor_channel(
        Arc::new(StaticLane::new(Credential {
            label: "cursor@x".into(),
            access_token: "tok".into(),
            identity: DeviceIdentity::derived("tok"),
        })),
        Arc::new(Echo),
    ))
    .with(provider_channel(lane, db));
    let log = Arc::new(RequestLog::new());
    let gw = Arc::new(Gateway {
        channels,
        api_key: api_key.map(str::to_string),
        ledger: None,
        media_jobs: None,
        routes: Arc::new(RwLock::new(routes)),
        log: Some(log.clone()),
    });
    let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(serve(listener, router(gw), std::future::pending()));
    Env {
        base: format!("http://{addr}"),
        log,
    }
}

fn http() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn claude_routes(model: &str, opus: Option<&str>) -> ClientRoutes {
    let mut routes = ClientRoutes::new();
    routes.insert(
        "claude".into(),
        ClientRoute {
            model: model.into(),
            opus: opus.map(str::to_string),
            ..ClientRoute::default()
        },
    );
    routes
}

#[tokio::test]
async fn claude_aliases_follow_the_client_route() {
    let env = gateway(
        vec![],
        claude_routes("cursor/claude-sonnet-5", Some("cursor/claude-opus-5")),
        None,
    )
    .await;
    let res: Value = http()
        .post(format!("{}/client/claude/v1/messages", env.base))
        .json(&json!({ "model": "claude-opus-4-8", "max_tokens": 16, "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // 上游收到的是路由换算后的模型；响应里回显的是客户端自己报的名字。
    assert_eq!(res["content"][0]["text"], "claude-opus-5");
    assert_eq!(res["model"], "claude-opus-4-8");
    let entry = &env.log.recent(1)[0];
    assert_eq!(entry.client.as_deref(), Some("claude"));
    assert_eq!(entry.target.as_deref(), Some("cursor/claude-opus-5"));
    assert_eq!(entry.channel.as_deref(), Some("cursor"));
    assert!(entry.ok);

    // 同一个名字不从客户端口进来：不看路由。
    let res: Value = http()
        .post(format!("{}/v1/messages", env.base))
        .json(&json!({ "model": "claude-opus-4-8", "max_tokens": 16, "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["content"][0]["text"], "claude-opus-4-8");
}

#[tokio::test]
async fn same_dialect_providers_get_the_raw_body_and_headers() {
    let (fake, seen) = fake_provider(None).await;
    let env = gateway(
        vec![(
            "DeepSeek",
            &format!("{fake}/anthropic"),
            ApiFormat::Anthropic,
            vec!["deepseek-v4-pro"],
        )],
        claude_routes("provider/deepseek-v4-pro", None),
        None,
    )
    .await;
    let body = json!({
        "model": "claude-sonnet-4-6",
        "max_tokens": 64,
        "stream": true,
        "thinking": { "type": "enabled", "budget_tokens": 1024 },
        "system": [{ "type": "text", "text": "be brief", "cache_control": { "type": "ephemeral" } }],
        "messages": [{ "role": "user", "content": [{ "type": "text", "text": "ping", "cache_control": { "type": "ephemeral" } }] }]
    });
    let text = http()
        .post(format!("{}/client/claude/v1/messages", env.base))
        .header("anthropic-beta", "interleaved-thinking-2025-05-14")
        .header("user-agent", "claude-cli/9.9.9")
        .json(&body)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    // 回来的帧原封不动。
    assert!(text.contains("event: message_start"), "{text}");
    assert!(text.contains("\"text\":\"pong\""), "{text}");
    assert!(text.contains("event: message_stop"), "{text}");

    let got = seen.lock().unwrap()[0].clone();
    assert_eq!(got.path, "/anthropic/v1/messages");
    assert_eq!(got.body["model"], "deepseek-v4-pro");
    // 中间表示装不下的都还在。
    assert_eq!(got.body["thinking"]["budget_tokens"], 1024);
    assert_eq!(got.body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(
        got.body["messages"][0]["content"][0]["cache_control"]["type"],
        "ephemeral"
    );
    assert_eq!(
        got.headers["anthropic-beta"],
        "interleaved-thinking-2025-05-14"
    );
    assert_eq!(got.headers["user-agent"], "claude-cli/9.9.9");
    assert_eq!(got.headers["authorization"], "Bearer sk-DeepSeek-secret");
    assert_eq!(got.headers["anthropic-version"], "2023-06-01");

    let entry = &env.log.recent(1)[0];
    assert_eq!(entry.channel.as_deref(), Some("provider"));
    assert_eq!(entry.account.as_deref(), Some("DeepSeek"));
    assert_eq!(entry.input_tokens, 5);
    assert_eq!(entry.output_tokens, 1);
}

#[tokio::test]
async fn a_chat_only_provider_is_translated_for_claude_code() {
    let (fake, seen) = fake_provider(None).await;
    let env = gateway(
        vec![(
            "Wasu",
            &format!("{fake}/v1"),
            ApiFormat::OpenaiChat,
            vec!["deepseek-v4-flash"],
        )],
        claude_routes("provider/deepseek-v4-flash", None),
        None,
    )
    .await;
    let text = http()
        .post(format!("{}/client/claude/v1/messages", env.base))
        .json(&json!({
            "model": "claude-sonnet-4-6",
            "max_tokens": 64,
            "stream": true,
            "system": "be brief",
            "messages": [{ "role": "user", "content": "ping" }]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    // 客户端收到的是 Anthropic 形状的事件，内容是上游 Chat 流里那段。
    assert!(text.contains("event: message_start"), "{text}");
    assert!(text.contains("pong"), "{text}");
    let got = seen.lock().unwrap()[0].clone();
    assert_eq!(got.path, "/v1/chat/completions");
    assert_eq!(got.body["model"], "deepseek-v4-flash");
    assert_eq!(got.body["messages"][0]["role"], "system");
    assert_eq!(got.body["messages"][1]["content"], "ping");
    // 翻译的时候不冒充客户端。
    assert!(!got.headers.contains_key("anthropic-beta"));
}

#[tokio::test]
async fn a_failing_provider_hands_over_to_the_next_one() {
    let (down, down_seen) = fake_provider(Some(503)).await;
    let (up, up_seen) = fake_provider(None).await;
    let env = gateway(
        vec![
            (
                "a-down",
                &format!("{down}/v1"),
                ApiFormat::OpenaiChat,
                vec!["m"],
            ),
            (
                "b-up",
                &format!("{up}/v1"),
                ApiFormat::OpenaiChat,
                vec!["m"],
            ),
        ],
        ClientRoutes::new(),
        None,
    )
    .await;
    let res: Value = http()
        .post(format!("{}/v1/chat/completions", env.base))
        .json(
            &json!({ "model": "provider/m", "messages": [{ "role": "user", "content": "ping" }] }),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["choices"][0]["message"]["content"], "pong");
    assert_eq!(down_seen.lock().unwrap().len(), 1);
    assert_eq!(up_seen.lock().unwrap().len(), 1);
    let log = env.log.recent(5);
    assert_eq!(log.len(), 2);
    assert!(log[0].ok);
    assert_eq!(log[0].attempt, 2);
    assert!(!log[1].ok);
    assert_eq!(log[1].status, 503);
    assert!(log[1].error.as_deref().unwrap().contains("a-down"));

    // 一个裸名恰好是某家声明的模型：走那家，不走默认通道。
    let res: Value = http()
        .post(format!("{}/v1/chat/completions", env.base))
        .json(&json!({ "model": "m", "messages": [{ "role": "user", "content": "ping" }] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["choices"][0]["message"]["content"], "pong");
}

#[tokio::test]
async fn a_relay_that_streams_when_not_asked_still_answers_whole() {
    let (fake, _seen) = fake_provider(None).await;
    let env = gateway(
        vec![(
            "Relay",
            &format!("{fake}/sse-only/v1"),
            ApiFormat::OpenaiChat,
            vec!["m"],
        )],
        ClientRoutes::new(),
        None,
    )
    .await;
    let res: Value = http()
        .post(format!("{}/v1/chat/completions", env.base))
        .json(
            &json!({ "model": "provider/m", "messages": [{ "role": "user", "content": "ping" }] }),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(res["choices"][0]["message"]["content"], "pong");
    assert_eq!(res["usage"]["prompt_tokens"], 3);
}

#[tokio::test]
async fn rejected_requests_leave_a_line_in_the_log() {
    let env = gateway(vec![], ClientRoutes::new(), Some("nx-right")).await;
    let res = http()
        .post(format!("{}/client/codex/v1/responses", env.base))
        .bearer_auth("nx-wrong")
        .json(&json!({ "model": "gpt-5.4", "input": "hi" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let entry = &env.log.recent(1)[0];
    assert_eq!(entry.client.as_deref(), Some("codex"));
    assert_eq!(entry.kind.as_deref(), Some("auth"));
    assert!(entry.error.as_deref().unwrap().contains("口令"));
}

#[tokio::test]
async fn scoped_model_lists_put_the_routes_channel_first_as_bare_names() {
    let (fake, _seen) = fake_provider(None).await;
    let mut routes = ClientRoutes::new();
    routes.insert(
        "codex".into(),
        ClientRoute {
            model: "provider/deepseek-v4-pro".into(),
            ..ClientRoute::default()
        },
    );
    let env = gateway(
        vec![(
            "DeepSeek",
            &format!("{fake}/v1"),
            ApiFormat::OpenaiChat,
            vec!["deepseek-v4-pro", "deepseek-v4-flash"],
        )],
        routes,
        None,
    )
    .await;
    let res: Value = http()
        .get(format!("{}/client/codex/v1/models", env.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = res["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids[0], "deepseek-v4-pro");
    assert_eq!(ids[1], "deepseek-v4-flash");
    assert!(ids.contains(&"provider/deepseek-v4-pro"));
}
