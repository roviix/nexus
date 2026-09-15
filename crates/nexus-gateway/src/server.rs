//! 本机 `127.0.0.1` 上的 HTTP 服务。
//!
//! 只做三件事：认方言、把请求交给 [`Lane`] + [`Upstream`]、把结果按方言写回去（流式走 SSE）。
//! 业务逻辑一行都不在这里——解析在 `inbound::parse`，事件序列在 `inbound::serialize`，
//! 号在 `Lane`，推理在 `Upstream`，所以这一层能用假后端从头到尾测。
//!
//! 路由同时挂在 `/v1`、`/openai/v1`、`/anthropic/v1` 下，并容忍客户端把 `/v1` 写进 Base URL 后
//! 拼出来的 `/v1/v1/…`——那不是客户的错，直接 404 只会让人以为服务挂了。

use crate::channel::{Capability, Channel, ChannelRegistry, Resolved};
use crate::error::UpstreamError;
use crate::images::{self, GeneratedImage, ImageRequest};
use crate::inbound::{parse_request, protocol_error, Dialect, Serializer, SseFrame};
use crate::inference::STATIC_MODELS;
use crate::lane::{Credential, Lane, Outcome};
use crate::ledger::{Ledger, RequestRecord};
use crate::media::{self, MediaJobs};
use crate::normalized::{estimate_tokens, Completion, Delta, Usage};
use crate::upstream::Upstream;
use axum::body::{Body, Bytes};
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::UnboundedReceiverStream;

pub struct Gateway {
    /// 全部通道。选路见 [`ChannelRegistry::resolve`]：前缀强制，裸名走用户默认通道。
    pub channels: ChannelRegistry,
    /// 可选的本地口令。Cursor local mode 的配置表单要求填一个 key；设了就校验
    /// （`Authorization: Bearer` 或 `x-api-key` 任一），没设就不管——只监听回环，
    /// 风险面是本机其他进程。
    pub api_key: Option<String>,
    /// 请求账本。`None` = 不记（测试、examples）。
    pub ledger: Option<Arc<Ledger>>,
    /// 异步媒体任务（生视频）的登记簿：`request_id → 通道 / 账号`，状态轮询要回到创建它的号。
    /// `None` = 不落库（测试、examples），视频任务只活在这一次进程里也查不到。
    pub media_jobs: Option<Arc<MediaJobs>>,
}

impl Gateway {
    /// 只有一条 Cursor 通道的网关。给测试与 examples。
    pub fn single(lane: Arc<dyn Lane>, upstream: Arc<dyn Upstream>) -> Self {
        Self {
            channels: ChannelRegistry::new(crate::channel::cursor_channel(lane, upstream)),
            api_key: None,
            ledger: None,
            media_jobs: None,
        }
    }

    fn route(&self, model: &str, cap: Capability) -> Resolved<'_> {
        self.channels.resolve(model, cap)
    }
}

type Shared = Arc<Gateway>;

/// 客户端请求头里与上游协议有关的那几个，转给后端前先挑出来（小写键）。
///
/// 白名单而不是全转：`authorization` 是我们自己的口令、`host` / `content-length` 是这一跳的，
/// `x-forwarded-*` 之类会暴露拓扑。Codex CLI 每个请求都带一组 `x-codex-*` 与 `session_id`，
/// 上游据此判定设备与会话——透传时要带着走（按账号收敛后）。
pub fn pick_client_headers(headers: &HeaderMap) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    for (name, value) in headers {
        let key = name.as_str().to_ascii_lowercase();
        let allowed = (key.starts_with("x-codex-")
            || key.starts_with("x-openai-")
            || key.starts_with("x-client-"))
            || matches!(
                key.as_str(),
                "originator"
                    | "session_id"
                    | "conversation_id"
                    | "thread-id"
                    | "accept-language"
                    | "openai-beta"
            );
        let denied = key.starts_with("x-forwarded-") || key == "x-real-ip" || key == "x-request-id";
        if !allowed || denied {
            continue;
        }
        if let Ok(v) = value.to_str() {
            if !v.trim().is_empty() {
                out.insert(key, v.trim().to_string());
            }
        }
    }
    out
}

/// 等上游响应头最多这么久：ChatGPT 通道要把会话状态印 / 额度头挂到客户端响应上，只能在
/// 首字节之前。上游一般一两秒内给头；超过就不等了，正文照常流。
const UPSTREAM_HEADERS_PATIENCE: std::time::Duration = std::time::Duration::from_secs(8);

fn dialect_name(d: Dialect) -> &'static str {
    match d {
        Dialect::OpenAiChat => "openai",
        Dialect::AnthropicMessages => "anthropic",
        Dialect::OpenAiResponses => "responses",
    }
}

/// 一次请求收尾：先告诉 lane（它据此接力 / 冷却），再记账。两件事的顺序无关紧要，
/// 但都得做——账本漏一条只是数字不准，lane 漏一次是号会被用穿。
fn settle(
    gw: &Gateway,
    channel: &Channel,
    credential: &Credential,
    model: &str,
    dialect: Dialect,
    result: &Result<Completion, UpstreamError>,
    started: std::time::Instant,
) {
    match result {
        Ok(c) => channel
            .lane
            .report(credential, model, Outcome::Ok(&c.usage)),
        Err(e) => channel.lane.report(credential, model, Outcome::Err(e)),
    }
    let Some(ledger) = &gw.ledger else { return };
    let elapsed = started.elapsed().as_millis() as u64;
    match result {
        Ok(c) => ledger.record(RequestRecord {
            channel: channel.id,
            account: &credential.label,
            model,
            routed: c.routed_model.as_deref(),
            dialect: dialect_name(dialect),
            ok: true,
            status: 200,
            kind: None,
            usage: c.usage,
            usage_measured: c.usage_measured,
            ttft_ms: c.ttft_ms,
            duration_ms: if c.turn_ms > 0 { c.turn_ms } else { elapsed },
        }),
        Err(e) => ledger.record(RequestRecord {
            channel: channel.id,
            account: &credential.label,
            model,
            routed: None,
            dialect: dialect_name(dialect),
            ok: false,
            status: e.status,
            kind: Some(e.kind.as_str()),
            usage: Default::default(),
            usage_measured: false,
            ttft_ms: None,
            duration_ms: elapsed,
        }),
    }
}

/// 入站请求体上限。Axum 默认 2 MB，`Bytes` 抽取时直接 413，handler / 上游都进不去，
/// 客户端看到的是一句英文 `Failed to buffer the request body: length limit exceeded`。
/// 64 MB 和出图参考图、Connect 信封同一量级；云端 TS 网关是 32 MB，本机回环多给一点，
/// 两张 20 MB 的 data URL 也过得去。
pub const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;

pub fn router(gw: Shared) -> Router {
    let v1 = Router::new()
        .route("/chat/completions", post(chat_completions))
        .route("/messages", post(messages))
        .route("/messages/count_tokens", post(count_tokens))
        .route("/responses", post(responses))
        .route("/images/generations", post(image_generations))
        .route("/images/edits", post(image_edits))
        .route("/videos/generations", post(video_generations))
        .route("/videos/edits", post(video_edits))
        .route("/videos/extensions", post(video_extensions))
        .route("/videos/{request_id}", get(video_status))
        .route("/videos/{request_id}/content", get(video_content))
        .route("/models", get(models))
        // 客户端把 /v1 写进 Base URL 后再拼一次的形态。
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/v1/responses", post(responses))
        .route("/v1/images/generations", post(image_generations))
        .route("/v1/images/edits", post(image_edits))
        .route("/v1/videos/generations", post(video_generations))
        .route("/v1/videos/edits", post(video_edits))
        .route("/v1/videos/extensions", post(video_extensions))
        .route("/v1/videos/{request_id}", get(video_status))
        .route("/v1/videos/{request_id}/content", get(video_content))
        .route("/v1/models", get(models))
        .with_state(gw);
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/v1", v1.clone())
        .nest("/openai/v1", v1.clone())
        .nest("/anthropic/v1", v1)
        // 后挂的 layer 在最外：先写入上限，再在回程把 Axum 那句英文 413 换成方言 JSON。
        .layer(middleware::from_fn(rewrite_payload_too_large))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
}

fn dialect_for_path(path: &str) -> Dialect {
    if path.contains("/messages") {
        Dialect::AnthropicMessages
    } else if path.contains("/responses") {
        Dialect::OpenAiResponses
    } else {
        Dialect::OpenAiChat
    }
}

async fn rewrite_payload_too_large(req: Request, next: Next) -> Response {
    let dialect = dialect_for_path(req.uri().path());
    let res = next.run(req).await;
    if res.status() != StatusCode::PAYLOAD_TOO_LARGE {
        return res;
    }
    err_json(
        dialect,
        413,
        &format!("请求体过大（上限 {} MB）", MAX_BODY_BYTES / 1024 / 1024),
    )
}

/// 绑到地址。传 `127.0.0.1:0` 让系统挑端口，实际端口从返回值的 `local_addr()` 拿。
pub async fn bind(addr: SocketAddr) -> std::io::Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(addr).await
}

pub async fn serve(
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown)
        .await
}

fn authorized(gw: &Gateway, headers: &HeaderMap) -> bool {
    let Some(want) = gw.api_key.as_deref() else {
        return true;
    };
    let header = |k: &str| headers.get(k).and_then(|v| v.to_str().ok()).map(str::trim);
    let bearer = header("authorization")
        .and_then(|s| {
            s.strip_prefix("Bearer ")
                .or_else(|| s.strip_prefix("bearer "))
        })
        .map(str::trim);
    bearer == Some(want) || header("x-api-key") == Some(want)
}

fn err_json(dialect: Dialect, status: u16, message: &str) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
    (code, Json(protocol_error(dialect, status, message))).into_response()
}

fn sse_response(rx: mpsc::UnboundedReceiver<Result<Bytes, std::convert::Infallible>>) -> Response {
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "text/event-stream")
        .header("cache-control", "no-cache")
        .header("x-accel-buffering", "no")
        .body(Body::from_stream(UnboundedReceiverStream::new(rx)))
        .expect("静态响应头")
}

async fn chat_completions(State(gw): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    handle(gw, Dialect::OpenAiChat, headers, body).await
}

async fn messages(State(gw): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    handle(gw, Dialect::AnthropicMessages, headers, body).await
}

/// Codex 主用的方言。
async fn responses(State(gw): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    handle(gw, Dialect::OpenAiResponses, headers, body).await
}

async fn handle(gw: Shared, dialect: Dialect, headers: HeaderMap, body: Bytes) -> Response {
    if !authorized(&gw, &headers) {
        return err_json(dialect, 401, "invalid api key");
    }
    let body: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err_json(dialect, 400, &format!("invalid JSON body: {e}")),
    };
    let conversation = headers
        .get("x-conversation-id")
        .and_then(|v| v.to_str().ok());
    let mut parsed = match parse_request(dialect, &body, conversation) {
        Ok(p) => p,
        Err(e) => return err_json(dialect, e.status(), &e.to_string()),
    };
    let resolved = gw.route(&parsed.request.model, Capability::Chat);
    let model_label = if parsed.request.model.trim().is_empty() {
        crate::channel::qualify(resolved.channel.id, &resolved.base_model)
    } else {
        parsed.request.model.clone()
    };
    parsed.request.model = resolved.base_model.clone();
    parsed.request.client_headers = pick_client_headers(&headers);
    let passthrough_route = resolved.channel.passthrough;
    // Responses 透传通道（ChatGPT / Grok）会用到原始体。别的通道不带：中间表示够用。
    if dialect == Dialect::OpenAiResponses && passthrough_route {
        parsed.request.raw_responses = Some(Arc::new(body));
    }
    let mut ser = Serializer::new(dialect, &model_label, &parsed.request.tools);
    let request = parsed.request;

    if !parsed.stream {
        let mut relayed: Vec<(String, String)> = Vec::new();
        let mut sink = |d: Delta| {
            if let Delta::Headers(h) = d {
                relayed = h;
            }
        };
        let result = relay(&gw, &model_label, dialect, &request, &mut sink).await;
        return match result {
            Ok(c) => {
                let body = match &c.raw_response {
                    // 透传：上游最终的 response 对象原样回，不照着中间表示再拼一遍。
                    Some(raw) if dialect == Dialect::OpenAiResponses => raw.clone(),
                    _ => ser.final_json(&c),
                };
                let mut res = (StatusCode::OK, Json(body)).into_response();
                attach_headers(&mut res, &relayed);
                res
            }
            Err(e) => err_json(dialect, e.status, &e.message),
        };
    }

    let (tx, rx) = mpsc::unbounded_channel::<Result<Bytes, std::convert::Infallible>>();
    // ChatGPT 通道：上游响应头（会话状态印、额度）要挂到客户端响应上，而响应头只能在首字节
    // 之前定——所以等一下上游的头再开始回。Cursor 通道不等：它没有这种头。
    let (hdr_tx, hdr_rx) = tokio::sync::oneshot::channel::<Vec<(String, String)>>();
    tokio::spawn(async move {
        // 首字节之前先发一条注释保活：上游 TTFT 动辄两三秒、带思考的模型更久，
        // 有客户端等不了那么久的空白。SSE 解析器会忽略注释行。
        let _ = tx.send(Ok(Bytes::from(SseFrame::comment("keepalive"))));
        let push = |tx: &mpsc::UnboundedSender<_>, frames: Vec<SseFrame>| {
            for f in frames {
                let _ = tx.send(Ok(Bytes::from(f.to_wire())));
            }
        };
        let mut hdr_tx = Some(hdr_tx);
        // 透传帧到过一次，收尾就不再用序列化器：上游已经把 response.completed 原样发出去了。
        let mut raw_mode = false;
        let mut on_delta = |d: Delta| {
            let frames = match d {
                Delta::Text(t) => ser.text(&t),
                Delta::Thinking(t) => ser.thinking(&t),
                Delta::Raw { event, data } => {
                    raw_mode = true;
                    vec![SseFrame::event(event, data)]
                }
                Delta::Headers(h) => {
                    if let Some(tx) = hdr_tx.take() {
                        let _ = tx.send(h);
                    }
                    Vec::new()
                }
            };
            push(&tx, frames);
        };
        let result = relay(&gw, &model_label, dialect, &request, &mut on_delta).await;
        // 上游没给头（换号失败、Cursor 通道）：放行等头的那一侧，别让客户端白等。
        drop(hdr_tx.take());
        let frames = match &result {
            Ok(c) if raw_mode => {
                let _ = c;
                Vec::new()
            }
            Ok(c) => ser.finish(c),
            Err(e) if raw_mode => vec![SseFrame::json(
                Some("error"),
                &json!({ "type": "error", "code": e.kind.as_str(), "message": e.message, "status": e.status }),
            )],
            Err(e) => ser.error(&e.message, e.status),
        };
        push(&tx, frames);
        // tx 在这里 drop，流随之结束。
    });
    let relayed = if passthrough_route {
        match tokio::time::timeout(UPSTREAM_HEADERS_PATIENCE, hdr_rx).await {
            Ok(Ok(h)) => h,
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let mut res = sse_response(rx);
    attach_headers(&mut res, &relayed);
    res
}

fn attach_headers(res: &mut Response, extra: &[(String, String)]) {
    for (k, v) in extra {
        if let (Ok(name), Ok(value)) = (
            axum::http::HeaderName::from_bytes(k.as_bytes()),
            axum::http::HeaderValue::from_str(v),
        ) {
            res.headers_mut().insert(name, value);
        }
    }
}

/// 一次请求最多在几个号上试。号池的意义是把几个号串成一个大额度，所以一个号出不了不该
/// 让客户端看见——只要还没往外吐过一个字，就换号重来。上限防的是一池死号把一次请求拖成
/// 几十秒：lane 会把试过的号记成耗尽 / 冷却，下一次请求自然跳过它们，收敛得很快。
const MAX_ACCOUNT_ATTEMPTS: usize = 4;

/// 取号 → 打上游 → 收尾（回报 lane、记账），怪号的错误在**首字节之前**换号重来。
///
/// 只有 `blames_account` 的错误换号：额度 / 鉴权 / 权限 / 限流 / 这个号出不了这个模型。
/// 供应商抖动换号无用、请求本身的问题换号必然重现、超时和取消不重试——这些直接回。
/// 已经吐过增量就不能换：客户端已经收到半段回答，换号重来会拼出两段。
/// 换号途中 lane 也没号可给了、或又把刚失败的号递回来（只有一个号 / lane 不认回报），
/// 回上游最后那条错误——它比「没号了」更说明发生了什么，状态码也是客户端认得的 402 / 429 / 401。
async fn relay(
    gw: &Gateway,
    model: &str,
    dialect: Dialect,
    request: &crate::normalized::ChatRequest,
    on_delta: &mut (dyn FnMut(Delta) + Send),
) -> Result<Completion, UpstreamError> {
    let mut last: Option<UpstreamError> = None;
    let mut tried: Vec<String> = Vec::new();
    let route = gw.route(model, Capability::Chat).channel;
    for attempt in 1..=MAX_ACCOUNT_ATTEMPTS {
        let credential = match route.lane.acquire(model).await {
            Ok(c) => c,
            Err(e) => return Err(last.unwrap_or(e)),
        };
        if let (Some(e), true) = (&last, tried.contains(&credential.label)) {
            return Err(e.clone());
        }
        tried.push(credential.label.clone());
        let started = std::time::Instant::now();
        let mut emitted = false;
        let mut sink = |d: Delta| {
            // 响应头不是给客户端的字节：还没吐过内容就能换号。
            if !matches!(d, Delta::Headers(_)) {
                emitted = true;
            }
            on_delta(d);
        };
        let result = route.upstream.stream(&credential, request, &mut sink).await;
        settle(gw, route, &credential, model, dialect, &result, started);
        match result {
            Err(e) if e.kind.blames_account() && !emitted && attempt < MAX_ACCOUNT_ATTEMPTS => {
                tracing::info!(
                    account = %credential.label, model, kind = e.kind.as_str(), status = e.status, attempt,
                    "这个号出不了，换号重来"
                );
                last = Some(e);
            }
            other => return other,
        }
    }
    Err(last.expect("循环至少跑过一轮，退出时一定带着最后一条错误"))
}

/// 出图收尾：告诉 lane、记账。没有 token 用量（上游不报），账本上按「一次请求」记，
/// `usage_measured = false` 说明那一列的零是「没有」而不是「量到零」。
fn settle_media<T>(
    gw: &Gateway,
    channel: &Channel,
    credential: &Credential,
    model: &str,
    dialect: &'static str,
    result: &Result<T, UpstreamError>,
    started: std::time::Instant,
) {
    let none = Usage::default();
    match result {
        Ok(_) => channel.lane.report(credential, model, Outcome::Ok(&none)),
        Err(e) => channel.lane.report(credential, model, Outcome::Err(e)),
    }
    let Some(ledger) = &gw.ledger else { return };
    let elapsed = started.elapsed().as_millis() as u64;
    let (ok, status, kind) = match result {
        Ok(_) => (true, 200, None),
        Err(e) => (false, e.status, Some(e.kind.as_str())),
    };
    ledger.record(RequestRecord {
        channel: channel.id,
        account: &credential.label,
        model,
        routed: None,
        dialect,
        ok,
        status,
        kind,
        usage: none,
        usage_measured: false,
        ttft_ms: None,
        duration_ms: elapsed,
    });
}

/// OpenAI 的 `POST /v1/images/generations`。背后两条路：Cursor 的 `RunGenerateImage`
/// （`nano-banana-2`），或 ChatGPT 订阅号的 `image_generation` 工具（`gpt-image-*`）。
///
/// 协议一次一张，`n` 张就串行 `n` 次，**每张各自取号、各自记账**：一张出完再取下一张的号，
/// 中途某个号没权限 / 被限流，lane 会自然接力到下一个。已经出来的图不因为后面一张失败而
/// 作废——一张要等十几秒，扔掉等于让用户白等；`data` 的长度就是实际拿到的张数。
/// 一张都没有才回错误。客户端要的 `size` Cursor 那条链路给不了（出图固定 1536×1024），
/// 如实忽略并在响应头里说明，别回显一个没兑现的值；ChatGPT 那条认它，不加这个头。
async fn image_generations(State(gw): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let dialect = Dialect::OpenAiChat;
    if !authorized(&gw, &headers) {
        return err_json(dialect, 401, "invalid api key");
    }
    let body: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err_json(dialect, 400, &format!("invalid JSON body: {e}")),
    };
    let req = match images::parse_generation(&body) {
        Ok(r) => r,
        Err(m) => return err_json(dialect, 400, &m),
    };
    run_images(&gw, req, Vec::new(), None).await
}

/// OpenAI 的 `POST /v1/images/edits`：参考图 + 提示词 → 一张新图。只有 ChatGPT 的 gpt-image 会做；
/// Cursor 那条路会明确拒掉。SDK 发的是 multipart（图片是文件），也接受 JSON（`image` 是 data URL）。
async fn image_edits(State(gw): State<Shared>, req: axum::extract::Request) -> Response {
    let dialect = Dialect::OpenAiChat;
    if !authorized(&gw, req.headers()) {
        return err_json(dialect, 401, "invalid api key");
    }
    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let parsed = if content_type.starts_with("multipart/form-data") {
        use axum::extract::FromRequest;
        match axum::extract::Multipart::from_request(req, &()).await {
            Ok(form) => images::parse_edit_multipart(form).await,
            Err(e) => Err(format!("multipart 解析失败：{e}")),
        }
    } else {
        match axum::body::to_bytes(req.into_body(), MAX_BODY_BYTES).await {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(v) => images::parse_edit_json(&v),
                Err(e) => Err(format!("invalid JSON body: {e}")),
            },
            Err(e) => Err(format!("读取请求体失败：{e}")),
        }
    };
    let edit = match parsed {
        Ok(e) => e,
        Err(m) => return err_json(dialect, 400, &m),
    };
    run_images(&gw, edit.base, edit.references, edit.mask).await
}

/// 出图的公共尾巴：选路 → 一张一张取号、出图、记账 → 拼 OpenAI 响应。
///
/// `references` 非空就是编辑。协议一次一张，`n` 张就串行 `n` 次，每张各自取号、各自记账：
/// 中途某个号没权限 / 被限流，lane 会自然接力到下一个。已经出来的图不因为后面一张失败而作废；
/// 一张都没有才回错误。
async fn run_images(
    gw: &Gateway,
    req: images::GenerationRequest,
    references: Vec<String>,
    mask: Option<String>,
) -> Response {
    let dialect = Dialect::OpenAiChat;
    let resolved = gw.route(&req.model, Capability::Image);
    let model_label = if req.model.trim().is_empty() {
        crate::channel::qualify(resolved.channel.id, &resolved.base_model)
    } else {
        req.model.clone()
    };
    let one = ImageRequest {
        model: resolved.base_model.clone(),
        prompt: req.prompt.clone(),
        size: req.size.clone(),
        quality: req.quality.clone(),
        background: req.background.clone(),
        output_format: req.output_format.clone(),
        references,
        mask,
    };
    let route = gw.route(&model_label, Capability::Image).channel;

    let mut out: Vec<GeneratedImage> = Vec::with_capacity(req.n as usize);
    let mut last_err: Option<UpstreamError> = None;
    for _ in 0..req.n {
        let started = std::time::Instant::now();
        let credential = match route.lane.acquire(&model_label).await {
            Ok(c) => c,
            Err(e) => {
                last_err = Some(e);
                break;
            }
        };
        let result = route.upstream.image(&credential, &one).await;
        settle_media(
            gw,
            route,
            &credential,
            &model_label,
            "images",
            &result,
            started,
        );
        match result {
            Ok(img) => out.push(img),
            Err(e) => {
                last_err = Some(e);
                break;
            }
        }
    }

    if out.is_empty() {
        let e = last_err.expect("没有图就一定有错");
        return err_json(dialect, e.status, &e.message);
    }
    let mut res = Json(images::generations_body(&out)).into_response();
    if let (Some(size), false) = (&req.size, route.passthrough) {
        if let Ok(v) = size.parse::<axum::http::HeaderValue>() {
            res.headers_mut().insert("x-nexus-size-ignored", v);
        }
    }
    if last_err.is_some() {
        if let Ok(v) = format!("{}/{}", out.len(), req.n).parse::<axum::http::HeaderValue>() {
            res.headers_mut().insert("x-nexus-partial", v);
        }
    }
    res
}

/// Anthropic 的 `count_tokens`：本地估算，不打上游。
async fn count_tokens(State(gw): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    let dialect = Dialect::AnthropicMessages;
    if !authorized(&gw, &headers) {
        return err_json(dialect, 401, "invalid api key");
    }
    let body: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err_json(dialect, 400, &format!("invalid JSON body: {e}")),
    };
    let parsed = match parse_request(dialect, &body, None) {
        Ok(p) => p,
        Err(e) => return err_json(dialect, e.status(), &e.to_string()),
    };
    let mut text: String = parsed
        .request
        .messages
        .iter()
        .map(|m| m.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    for t in &parsed.request.tools {
        text.push('\n');
        text.push_str(&t.name);
        text.push_str(&t.description);
        text.push_str(&t.parameters.to_string());
    }
    Json(json!({ "input_tokens": estimate_tokens(&text) })).into_response()
}

async fn models(State(gw): State<Shared>, headers: HeaderMap) -> Response {
    if !authorized(&gw, &headers) {
        return err_json(Dialect::OpenAiChat, 401, "invalid api key");
    }
    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut data: Vec<Value> = Vec::new();
    let mut push = |channel: &str, vendor: &str, id: &str| {
        let qid = crate::channel::qualify(channel, id);
        data.push(json!({
            "id": qid,
            "object": "model",
            "type": "model",
            "created": created,
            "owned_by": vendor,
            "display_name": qid,
        }));
    };
    for id in STATIC_MODELS {
        push(crate::channel::CURSOR, "cursor", id);
    }
    for (id, _) in crate::models::IMAGE_MODELS {
        push(crate::channel::CURSOR, "cursor", id);
    }
    // 订阅通道有号才报。同名并列：`cursor/gpt-5.6-sol` 和 `chatgpt/gpt-5.6-sol` 都在。
    for ch in gw.channels.extras() {
        if ch.gate.ready() {
            for id in ch.gate.models(Capability::Chat) {
                push(ch.id, ch.vendor, &id);
            }
        }
        if ch.gate.media_ready() {
            for id in ch.gate.models(Capability::Image) {
                push(ch.id, ch.vendor, &id);
            }
            for id in ch.gate.models(Capability::Video) {
                push(ch.id, ch.vendor, &id);
            }
        }
    }
    Json(json!({ "object": "list", "data": data })).into_response()
}

// ---------- 生视频（异步任务）----------

/// `POST /v1/videos/generations`：文生视频 / 图生视频 / 参考图生视频。上游是异步的：这里只拿到
/// `request_id`，把「哪条通道、哪个号」记进登记簿，之后的状态轮询回到同一个号——任务是账号
/// 维度的，换号去查会得到 404。
async fn video_generations(State(gw): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    start_video(gw, headers, body, media::VideoOp::Generate).await
}

async fn video_edits(State(gw): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    start_video(gw, headers, body, media::VideoOp::Edit).await
}

async fn video_extensions(State(gw): State<Shared>, headers: HeaderMap, body: Bytes) -> Response {
    start_video(gw, headers, body, media::VideoOp::Extend).await
}

async fn start_video(gw: Shared, headers: HeaderMap, body: Bytes, op: media::VideoOp) -> Response {
    let dialect = Dialect::OpenAiChat;
    if !authorized(&gw, &headers) {
        return err_json(dialect, 401, "invalid api key");
    }
    let body: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return err_json(dialect, 400, &format!("invalid JSON body: {e}")),
    };
    let req = match media::parse_video_request(op, &body) {
        Ok(r) => r,
        Err(m) => return err_json(dialect, 400, &m),
    };
    let resolved = gw.route(&req.model, Capability::Video);
    let model_label = if req.model.trim().is_empty() {
        let base = if resolved.base_model.is_empty() {
            media::DEFAULT_VIDEO_MODEL.to_string()
        } else {
            resolved.base_model.clone()
        };
        crate::channel::qualify(resolved.channel.id, &base)
    } else {
        req.model.clone()
    };
    let route = resolved.channel;
    let mut last: Option<UpstreamError> = None;
    let mut tried: Vec<String> = Vec::new();
    for _ in 0..MAX_ACCOUNT_ATTEMPTS {
        let started = std::time::Instant::now();
        let credential = match route.lane.acquire(&model_label).await {
            Ok(c) => c,
            Err(e) => return err_json(dialect, e.status, &last.unwrap_or(e).message),
        };
        if tried.contains(&credential.label) {
            break;
        }
        tried.push(credential.label.clone());
        let result = route.upstream.video_start(&credential, &req).await;
        settle_media(
            &gw,
            route,
            &credential,
            &model_label,
            "videos",
            &result,
            started,
        );
        match result {
            Ok(job) => {
                if let Some(jobs) = &gw.media_jobs {
                    jobs.record(&media::MediaJob {
                        request_id: job.request_id.clone(),
                        channel: route.id.to_string(),
                        account: credential.label.clone(),
                        model: model_label.clone(),
                        op: op.as_str().to_string(),
                        status: "pending".into(),
                        video_url: None,
                        duration_secs: req.duration,
                        resolution: req.resolution.clone(),
                        created_ms: media::now_ms(),
                        updated_ms: media::now_ms(),
                    });
                }
                return Json(json!({
                    "request_id": job.request_id,
                    "status": "pending",
                    "model": model_label,
                    "channel": route.id,
                }))
                .into_response();
            }
            Err(e) if e.kind.blames_account() => last = Some(e),
            Err(e) => return err_json(dialect, e.status, &e.message),
        }
    }
    let e = last.expect("循环至少跑过一轮");
    err_json(dialect, e.status, &e.message)
}

/// `GET /v1/videos/{request_id}`：查状态。回到创建任务的那个号；登记簿里没有它（重启前的
/// 进程没落库、或根本不是这里发起的）就按默认媒体通道用当前号试一次。
async fn video_status(
    State(gw): State<Shared>,
    axum::extract::Path(request_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    let dialect = Dialect::OpenAiChat;
    if !authorized(&gw, &headers) {
        return err_json(dialect, 401, "invalid api key");
    }
    let Some((route, credential)) = video_owner(&gw, &request_id).await else {
        return err_json(dialect, 404, "video request not found");
    };
    let started = std::time::Instant::now();
    let result = route.upstream.video_status(&credential, &request_id).await;
    settle_media(
        &gw,
        route,
        &credential,
        "video-status",
        "videos",
        &result,
        started,
    );
    match result {
        Ok(status) => {
            if let Some(jobs) = &gw.media_jobs {
                jobs.update_status(&request_id, &status);
            }
            Json(media::status_body(&request_id, &status)).into_response()
        }
        Err(e) => err_json(dialect, e.status, &e.message),
    }
}

/// `GET /v1/videos/{request_id}/content`：把成片字节代下载回来。上游给的是带签名的临时 URL，
/// 客户端拿不到它也无所谓——从这里拉一样是那份字节。
async fn video_content(
    State(gw): State<Shared>,
    axum::extract::Path(request_id): axum::extract::Path<String>,
    headers: HeaderMap,
) -> Response {
    let dialect = Dialect::OpenAiChat;
    if !authorized(&gw, &headers) {
        return err_json(dialect, 401, "invalid api key");
    }
    let Some((route, credential)) = video_owner(&gw, &request_id).await else {
        return err_json(dialect, 404, "video request not found");
    };
    let status = match route.upstream.video_status(&credential, &request_id).await {
        Ok(s) => s,
        Err(e) => return err_json(dialect, e.status, &e.message),
    };
    if let Some(jobs) = &gw.media_jobs {
        jobs.update_status(&request_id, &status);
    }
    let Some(url) = status.video_url.as_deref().filter(|u| !u.is_empty()) else {
        return err_json(dialect, 409, &format!("video is {}", status.status));
    };
    match media::download(url).await {
        Ok((mime, bytes)) => Response::builder()
            .status(StatusCode::OK)
            .header("content-type", mime)
            .header(
                "content-disposition",
                format!("inline; filename=\"{request_id}.mp4\""),
            )
            .body(Body::from(bytes))
            .expect("静态响应头"),
        Err(e) => err_json(dialect, e.status, &e.message),
    }
}

/// 找到任务归属的通道与号。
async fn video_owner<'a>(gw: &'a Gateway, request_id: &str) -> Option<(&'a Channel, Credential)> {
    if let Some(job) = gw.media_jobs.as_ref().and_then(|j| j.get(request_id)) {
        let ch = gw.channels.get(&job.channel)?;
        let credential = ch.lane.acquire_label(&job.account).await.ok()?;
        return Some((ch, credential));
    }
    let ch = gw
        .channels
        .get(crate::channel::GROK)
        .unwrap_or_else(|| gw.channels.default_channel());
    let credential = ch.lane.acquire(media::DEFAULT_VIDEO_MODEL).await.ok()?;
    Some((ch, credential))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{UpstreamError, UpstreamKind};
    use crate::identity::DeviceIdentity;
    use crate::lane::{BoxFuture, Credential, StaticLane};
    use crate::normalized::{ChatRequest, Completion, FinishReason, ToolCall, Usage};
    use crate::upstream::DeltaSink;
    use std::sync::Mutex;

    /// 照本宣科的假后端：先吐脚本里的增量，再给定结局。顺带记下收到的请求。
    /// 出图按 `images` 里的脚本一张一个结局（用完了就一直是最后一个）。
    struct FakeUpstream {
        deltas: Vec<Delta>,
        result: Result<Completion, UpstreamError>,
        seen: Mutex<Vec<ChatRequest>>,
        images: Mutex<Vec<Result<GeneratedImage, UpstreamError>>>,
        seen_images: Mutex<Vec<ImageRequest>>,
    }

    impl FakeUpstream {
        fn chat(deltas: Vec<Delta>, result: Result<Completion, UpstreamError>) -> Self {
            Self {
                deltas,
                result,
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            }
        }

        fn drawing(script: Vec<Result<GeneratedImage, UpstreamError>>) -> Self {
            Self {
                images: Mutex::new(script),
                ..Self::chat(vec![], Ok(completion("", vec![])))
            }
        }
    }

    impl Upstream for FakeUpstream {
        fn stream<'a>(
            &'a self,
            _credential: &'a Credential,
            request: &'a ChatRequest,
            on_delta: DeltaSink<'a>,
        ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
            Box::pin(async move {
                self.seen.lock().unwrap().push(request.clone());
                for d in &self.deltas {
                    on_delta(d.clone());
                }
                self.result.clone()
            })
        }

        fn image<'a>(
            &'a self,
            _credential: &'a Credential,
            request: &'a ImageRequest,
        ) -> BoxFuture<'a, Result<GeneratedImage, UpstreamError>> {
            Box::pin(async move {
                self.seen_images.lock().unwrap().push(request.clone());
                let mut script = self.images.lock().unwrap();
                if script.len() > 1 {
                    script.remove(0)
                } else {
                    script.first().cloned().unwrap_or_else(|| {
                        Err(UpstreamError::new(UpstreamKind::Upstream, 502, "no script"))
                    })
                }
            })
        }
    }

    fn picture(b64: &str) -> GeneratedImage {
        GeneratedImage {
            b64: b64.into(),
            mime: "image/png".into(),
            size: Some((1536, 1024)),
            revised_prompt: None,
        }
    }

    /// 只会出图 / 出视频的假媒体后端，挂成一条「grok」通道，考的是选路 + 视频任务的登记与回查。
    struct FakeMedia {
        polls: Mutex<u32>,
        seen_video: Mutex<Vec<crate::media::VideoRequest>>,
    }

    impl Upstream for FakeMedia {
        fn stream<'a>(
            &'a self,
            _c: &'a Credential,
            _r: &'a ChatRequest,
            _d: DeltaSink<'a>,
        ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
            Box::pin(async {
                Err(UpstreamError::new(
                    UpstreamKind::BadRequest,
                    400,
                    "媒体后端不聊天",
                ))
            })
        }

        fn image<'a>(
            &'a self,
            _c: &'a Credential,
            request: &'a ImageRequest,
        ) -> BoxFuture<'a, Result<GeneratedImage, UpstreamError>> {
            Box::pin(async move {
                Ok(GeneratedImage {
                    b64: format!("img-for-{}", request.model),
                    mime: "image/png".into(),
                    size: None,
                    revised_prompt: None,
                })
            })
        }

        fn video_start<'a>(
            &'a self,
            _c: &'a Credential,
            request: &'a crate::media::VideoRequest,
        ) -> BoxFuture<'a, Result<crate::media::VideoJob, UpstreamError>> {
            Box::pin(async move {
                self.seen_video.lock().unwrap().push(request.clone());
                Ok(crate::media::VideoJob {
                    request_id: "vid_1".into(),
                })
            })
        }

        fn video_status<'a>(
            &'a self,
            _c: &'a Credential,
            request_id: &'a str,
        ) -> BoxFuture<'a, Result<crate::media::VideoStatus, UpstreamError>> {
            Box::pin(async move {
                if request_id != "vid_1" {
                    return Err(UpstreamError::new(UpstreamKind::Upstream, 404, "不认识"));
                }
                let mut n = self.polls.lock().unwrap();
                *n += 1;
                Ok(if *n < 2 {
                    crate::media::VideoStatus {
                        status: "pending".into(),
                        ..Default::default()
                    }
                } else {
                    crate::media::VideoStatus {
                        status: "done".into(),
                        video_url: Some("https://cdn.example/v.mp4".into()),
                        duration_secs: Some(6),
                        resolution: Some("480p".into()),
                        error: None,
                        raw: Some(
                            json!({ "status": "done", "video": { "url": "https://cdn.example/v.mp4" } }),
                        ),
                    }
                })
            })
        }
    }

    struct MediaGate {
        media: bool,
    }

    impl crate::channel::ChannelGate for MediaGate {
        fn ready(&self) -> bool {
            true
        }
        fn owns(&self, cap: Capability, m: &str) -> bool {
            match cap {
                Capability::Chat => m == "grok-4.5",
                Capability::Image => m == "grok-imagine-image",
                Capability::Video => m.starts_with("grok-imagine-video"),
            }
        }
        fn models(&self, cap: Capability) -> Vec<String> {
            match cap {
                Capability::Chat => vec!["grok-4.5".into()],
                Capability::Image => vec!["grok-imagine-image".into()],
                Capability::Video => vec!["grok-imagine-video-1.5".into()],
            }
        }
        fn media_ready(&self) -> bool {
            self.media
        }
    }

    async fn spawn_with_media(media_ready: bool) -> (String, Arc<FakeMedia>, Arc<MediaJobs>) {
        let cursor = Arc::new(FakeUpstream::drawing(vec![Ok(picture("cursor-img"))]));
        let lane: Arc<dyn Lane> = Arc::new(StaticLane::new(Credential {
            label: "cursor@x".into(),
            access_token: "tok".into(),
            identity: DeviceIdentity::derived("tok"),
        }));
        let grok_lane: Arc<dyn Lane> = Arc::new(StaticLane::new(Credential {
            label: "grok@x".into(),
            access_token: "gtok".into(),
            identity: DeviceIdentity::derived("gtok"),
        }));
        let media = Arc::new(FakeMedia {
            polls: Mutex::new(0),
            seen_video: Mutex::new(vec![]),
        });
        let jobs = Arc::new(MediaJobs::new(Arc::new(
            nexus_store::Db::open_in_memory().unwrap(),
        )));
        let gw = Arc::new(Gateway {
            channels: ChannelRegistry::new(crate::channel::cursor_channel(lane, cursor)).with(
                Channel {
                    id: crate::channel::GROK,
                    label: "Grok",
                    vendor: "xai",
                    prefixes: &["grok/", "xai/"],
                    lane: grok_lane,
                    upstream: media.clone(),
                    gate: Arc::new(MediaGate { media: media_ready }),
                    passthrough: true,
                },
            ),
            api_key: None,
            ledger: None,
            media_jobs: Some(jobs.clone()),
        });
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, router(gw), std::future::pending()));
        (format!("http://{addr}"), media, jobs)
    }

    #[tokio::test]
    async fn media_models_route_by_capability_and_media_gate() {
        // 裸名走用户默认通道（出厂 Cursor），不按模型名猜该不该去 Grok。
        let (base, _, _) = spawn_with_media(false).await;
        let body: Value = http()
            .post(format!("{base}/v1/images/generations"))
            .json(&json!({ "model": "grok-imagine-image", "prompt": "cat" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["data"][0]["b64_json"], "cursor-img");

        // 要走 Grok 就写通道前缀。
        let (base, _, _) = spawn_with_media(true).await;
        let body: Value = http()
            .post(format!("{base}/v1/images/generations"))
            .json(&json!({ "model": "grok/grok-imagine-image", "prompt": "cat" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["data"][0]["b64_json"], "img-for-grok-imagine-image");

        // /v1/models 把媒体模型也列出来，归 xai。
        let models: Value = http()
            .get(format!("{base}/v1/models"))
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
        assert!(ids.contains(&"grok/grok-imagine-video-1.5"));
        assert!(ids.contains(&"grok/grok-4.5"));
        assert!(ids.contains(&"cursor/claude-opus-5"));
    }

    #[tokio::test]
    async fn video_jobs_are_registered_and_polled_on_the_creating_account() {
        let (base, media, jobs) = spawn_with_media(true).await;
        let res = http()
            .post(format!("{base}/v1/videos/generations"))
            .json(&json!({ "model": "grok/grok-imagine-video-1.5", "prompt": "waves", "seconds": 6, "size": "1792x1024" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["request_id"], "vid_1");
        assert_eq!(body["status"], "pending");
        assert_eq!(body["channel"], "grok");
        {
            let seen = media.seen_video.lock().unwrap();
            assert_eq!(seen[0].duration, Some(6));
            assert_eq!(seen[0].aspect_ratio.as_deref(), Some("16:9"));
        }
        let job = jobs.get("vid_1").expect("登记了");
        assert_eq!(job.account, "grok@x");
        assert_eq!(job.channel, "grok");

        let first: Value = http()
            .get(format!("{base}/v1/videos/vid_1"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(first["status"], "pending");
        let second: Value = http()
            .get(format!("{base}/v1/videos/vid_1"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(second["status"], "done");
        assert_eq!(second["video"]["url"], "https://cdn.example/v.mp4");
        assert_eq!(second["request_id"], "vid_1");
        assert_eq!(jobs.get("vid_1").unwrap().status, "done");

        let missing = http()
            .get(format!("{base}/v1/videos/nope"))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), 404, "登记簿里没有、上游也不认");

        let bad = http()
            .post(format!("{base}/v1/videos/edits"))
            .json(&json!({ "prompt": "x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(bad.status(), 400, "编辑要有 video");
    }

    fn completion(text: &str, tools: Vec<ToolCall>) -> Completion {
        Completion {
            text: text.into(),
            thinking: String::new(),
            finish_reason: if tools.is_empty() {
                FinishReason::Stop
            } else {
                FinishReason::ToolCalls
            },
            tool_calls: tools,
            usage: Usage {
                input_tokens: 7,
                output_tokens: 3,
                ..Default::default()
            },
            usage_measured: true,
            routed_model: Some("routed".into()),
            ttft_ms: Some(1),
            turn_ms: 2,
            raw_response: None,
        }
    }

    async fn spawn(upstream: FakeUpstream, api_key: Option<&str>) -> (String, Arc<FakeUpstream>) {
        spawn_with_ledger(upstream, api_key, None).await
    }

    async fn spawn_with_ledger(
        upstream: FakeUpstream,
        api_key: Option<&str>,
        ledger: Option<Arc<Ledger>>,
    ) -> (String, Arc<FakeUpstream>) {
        let upstream = Arc::new(upstream);
        let gw = Arc::new(Gateway {
            api_key: api_key.map(str::to_string),
            ledger,
            ..Gateway::single(
                Arc::new(StaticLane::new(Credential {
                    label: "test@x".into(),
                    access_token: "tok".into(),
                    identity: DeviceIdentity::derived("tok"),
                })),
                upstream.clone(),
            )
        });
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, router(gw), std::future::pending()));
        (format!("http://{addr}"), upstream)
    }

    #[tokio::test]
    async fn every_dialect_request_lands_in_the_ledger() {
        let ledger = Arc::new(Ledger::new(Arc::new(
            nexus_store::Db::open_in_memory().unwrap(),
        )));
        let (base, _) = spawn_with_ledger(
            FakeUpstream {
                deltas: vec![Delta::Text("hi".into())],
                result: Ok(completion("hi", vec![])),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
            Some(ledger.clone()),
        )
        .await;
        // 非流式 + 流式各一次，账本里该有两行、模型名与方言都对。
        http()
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({ "model": "claude-sonnet-5", "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap();
        let streamed = http()
            .post(format!("{base}/v1/messages"))
            .json(&json!({ "model": "m", "stream": true, "max_tokens": 5,
                           "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap();
        // 流式的账在流结束后才记：把 body 读完再看。
        let _ = streamed.text().await.unwrap();

        let s = ledger.summary(1, 0).unwrap();
        assert_eq!(s.today.calls, 2);
        assert_eq!(s.today.errors, 0);
        assert_eq!(s.today.input_tokens, 14, "两次各 7 个输入 token");
        assert_eq!(s.by_account[0].name, "test@x");
        assert_eq!(s.recent[0].model, "m");
        assert_eq!(s.recent[0].routed.as_deref(), Some("routed"));
        assert_eq!(s.recent[1].model, "claude-sonnet-5");

        // 失败也记，而且记的是失败。
        let (base, _) = spawn_with_ledger(
            FakeUpstream {
                deltas: vec![],
                result: Err(UpstreamError::new(UpstreamKind::Quota, 402, "dry")),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
            Some(ledger.clone()),
        )
        .await;
        http()
            .post(format!("{base}/v1/responses"))
            .json(&json!({ "model": "m", "input": "x" }))
            .send()
            .await
            .unwrap();
        let s = ledger.summary(1, 0).unwrap();
        assert_eq!(s.today.calls, 3);
        assert_eq!(s.today.errors, 1);
        assert_eq!(s.recent[0].kind.as_deref(), Some("quota"));
        assert_eq!(s.recent[0].status, 402);
    }

    fn http() -> reqwest::Client {
        reqwest::Client::new()
    }

    #[tokio::test]
    async fn responses_accepts_a_body_larger_than_axum_default_2mb() {
        let (base, up) = spawn(
            FakeUpstream {
                deltas: vec![],
                result: Ok(completion("ok", vec![])),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
        )
        .await;
        // Axum 默认 2 MB；超过就会在抽取阶段 413，假后端根本收不到。
        let pad = "x".repeat(2 * 1024 * 1024 + 8 * 1024);
        let res = http()
            .post(format!("{base}/v1/responses"))
            .json(&json!({ "model": "m", "input": pad }))
            .send()
            .await
            .unwrap();
        let status = res.status();
        assert_ne!(status, 413, "超过 2 MB 不该再被 DefaultBodyLimit 挡下");
        assert_eq!(status, 200, "{}", res.text().await.unwrap_or_default());
        assert_eq!(up.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn images_generations_draws_n_pictures_one_call_each_and_reports_the_ignored_size() {
        let ledger = Arc::new(Ledger::new(Arc::new(
            nexus_store::Db::open_in_memory().unwrap(),
        )));
        let (base, up) = spawn_with_ledger(
            FakeUpstream::drawing(vec![Ok(picture("AAAA")), Ok(picture("BBBB"))]),
            Some("k"),
            Some(ledger.clone()),
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/images/generations"))
            .bearer_auth("k")
            .json(&json!({ "model": "nano-banana-2", "prompt": "a red panda", "n": 2, "size": "1024x1024" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(
            res.headers().get("x-nexus-size-ignored").unwrap(),
            "1024x1024",
            "客户要的规格给不了，要说出来而不是回显"
        );
        assert!(res.headers().get("x-nexus-partial").is_none());
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["data"][0]["b64_json"], "AAAA");
        assert_eq!(body["data"][1]["b64_json"], "BBBB");
        assert!(body["created"].as_u64().is_some());

        let seen = up.seen_images.lock().unwrap();
        assert_eq!(seen.len(), 2, "协议一次一张，两张就是两次调用");
        assert_eq!(seen[0].prompt, "a red panda");
        assert_eq!(seen[0].model, "nano-banana-2");
        drop(seen);

        let s = ledger.summary(1, 0).unwrap();
        assert_eq!(s.today.calls, 2, "每张各记一笔");
        assert_eq!(s.recent[0].model, "nano-banana-2");
    }

    #[tokio::test]
    async fn images_generations_keeps_what_it_got_when_a_later_picture_fails() {
        let (base, _) = spawn(
            FakeUpstream::drawing(vec![
                Ok(picture("AAAA")),
                Err(UpstreamError::new(
                    UpstreamKind::RateLimit,
                    429,
                    "slow down",
                )),
            ]),
            None,
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/images/generations"))
            .json(&json!({ "prompt": "x", "n": 3 }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "已经出来的图不作废");
        assert_eq!(res.headers().get("x-nexus-partial").unwrap(), "1/3");
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn images_generations_maps_refusals_auth_and_bad_bodies_to_openai_errors() {
        // 一张都没出：上游的错误原样成为响应，状态码跟着走。
        let (base, _) = spawn(
            FakeUpstream::drawing(vec![Err(UpstreamError::new(
                UpstreamKind::ModelUnsupported,
                403,
                "这个 Cursor 账号没有生图权限",
            ))]),
            Some("k"),
        )
        .await;
        let res = http()
            .post(format!("{base}/openai/v1/images/generations"))
            .bearer_auth("k")
            .json(&json!({ "prompt": "x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 403);
        let body: Value = res.json().await.unwrap();
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("生图权限"));

        // 口令不对 401；没有 prompt 400；url 回法 400 —— 都是 OpenAI 形状的错误体。
        let res = http()
            .post(format!("{base}/v1/images/generations"))
            .json(&json!({ "prompt": "x" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401);
        let res = http()
            .post(format!("{base}/v1/images/generations"))
            .bearer_auth("k")
            .json(&json!({ "model": "m" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 400);
        let res = http()
            .post(format!("{base}/v1/images/generations"))
            .bearer_auth("k")
            .json(&json!({ "prompt": "x", "response_format": "url" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 400);
        let body: Value = res.json().await.unwrap();
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("b64_json"));
    }

    #[tokio::test]
    async fn openai_non_stream_round_trip() {
        let (base, up) = spawn(
            FakeUpstream {
                deltas: vec![],
                result: Ok(completion("hi there", vec![])),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({ "model": "claude-sonnet-5", "messages": [{ "role": "user", "content": "hello" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["choices"][0]["message"]["content"], "hi there");
        assert_eq!(body["usage"]["prompt_tokens"], 7);
        let seen = up.seen.lock().unwrap();
        assert_eq!(seen[0].model, "claude-sonnet-5");
        assert_eq!(seen[0].messages[0].text, "hello");
    }

    #[tokio::test]
    async fn anthropic_stream_emits_the_full_event_sequence_over_sse() {
        let (base, _) = spawn(
            FakeUpstream {
                deltas: vec![
                    Delta::Thinking("mm".into()),
                    Delta::Text("Hel".into()),
                    Delta::Text("lo".into()),
                ],
                result: Ok(completion("Hello", vec![])),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/messages"))
            .json(&json!({ "model": "m", "stream": true, "max_tokens": 10,
                           "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(res.headers()["content-type"], "text/event-stream");
        let text = res.text().await.unwrap();
        assert!(text.starts_with(": keepalive\n\n"), "首字节前先保活");
        let order: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("event: "))
            .collect();
        assert_eq!(order.first(), Some(&"message_start"));
        assert_eq!(order.last(), Some(&"message_stop"));
        assert!(order.contains(&"content_block_delta"));
        assert!(text.contains(r#""thinking_delta""#));
        assert!(text.contains(r#""text":"Hel""#));
        assert!(text.contains(r#""stop_reason":"end_turn""#));
    }

    #[tokio::test]
    async fn openai_stream_ends_with_done_and_carries_tool_calls() {
        let call = ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: r#"{"p":"a"}"#.into(),
        };
        let (base, _) = spawn(
            FakeUpstream {
                deltas: vec![],
                result: Ok(completion("", vec![call])),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
        )
        .await;
        let res = http()
            .post(format!("{base}/openai/v1/chat/completions"))
            .json(&json!({ "model": "m", "stream": true,
                           "messages": [{ "role": "user", "content": "x" }],
                           "tools": [{ "type": "function", "function": { "name": "read", "parameters": {} } }] }))
            .send()
            .await
            .unwrap();
        let text = res.text().await.unwrap();
        assert!(text.trim_end().ends_with("data: [DONE]"));
        let chunks: Vec<Value> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter(|d| *d != "[DONE]")
            .map(|d| serde_json::from_str(d).unwrap())
            .collect();
        let with_calls = chunks
            .iter()
            .find(|c| !c["choices"][0]["delta"]["tool_calls"].is_null())
            .expect("有一帧带 tool_calls");
        let tc = &with_calls["choices"][0]["delta"]["tool_calls"][0];
        assert_eq!(tc["index"], 0);
        assert_eq!(tc["id"], "c1");
        assert_eq!(tc["function"]["name"], "read");
        assert_eq!(tc["function"]["arguments"], r#"{"p":"a"}"#);
        assert_eq!(
            chunks.last().unwrap()["choices"][0]["finish_reason"],
            "tool_calls"
        );
    }

    #[tokio::test]
    async fn responses_non_stream_round_trip_handles_heterogeneous_input() {
        let (base, up) = spawn(
            FakeUpstream {
                deltas: vec![],
                result: Ok(completion("done reading", vec![])),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/responses"))
            .json(&json!({
                "model": "gpt-5-codex",
                "instructions": "be terse",
                "input": [
                    { "role": "user", "content": "read a.txt" },
                    { "type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{\"path\":\"a.txt\"}" },
                    { "type": "function_call_output", "call_id": "call_1", "output": "file body" },
                ],
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["object"], "response");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["output"][0]["type"], "message");
        assert_eq!(body["output"][0]["content"][0]["text"], "done reading");
        assert_eq!(
            body["usage"]["output_tokens_details"]["reasoning_tokens"],
            0
        );
        let seen = up.seen.lock().unwrap();
        assert_eq!(seen[0].messages[0].role, crate::normalized::Role::System);
        assert_eq!(seen[0].messages[1].text, "read a.txt");
        assert_eq!(seen[0].messages[2].tool_calls[0].name, "read");
        assert_eq!(seen[0].messages[3].tool_results[0].text, "file body");
    }

    #[tokio::test]
    async fn responses_stream_emits_output_item_lifecycle_and_completes() {
        let call = ToolCall {
            id: "call_1".into(),
            name: "read".into(),
            arguments: r#"{"p":"a"}"#.into(),
        };
        let (base, _) = spawn(
            FakeUpstream {
                deltas: vec![Delta::Thinking("mm".into()), Delta::Text("Hi".into())],
                result: Ok(completion("Hi", vec![call])),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
        )
        .await;
        let res = http()
            .post(format!("{base}/openai/v1/responses"))
            .json(&json!({ "model": "m", "stream": true, "input": "x",
                           "tools": [{ "type": "function", "name": "read", "parameters": {} }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(res.headers()["content-type"], "text/event-stream");
        let text = res.text().await.unwrap();
        assert!(text.starts_with(": keepalive\n\n"), "首字节前先保活");
        let order: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("event: "))
            .collect();
        assert_eq!(order.first(), Some(&"response.created"));
        assert_eq!(order.last(), Some(&"response.completed"));
        assert!(order.contains(&"response.output_text.delta"));
        assert!(order.contains(&"response.function_call_arguments.done"));
        // response.completed 的 output 必须原样复用流式已经发出的 item，条数要对得上：
        // 一段思考 + 一段正文 + 一个工具调用。
        let data_lines: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .collect();
        let completed: Value = data_lines
            .iter()
            .map(|d| serde_json::from_str::<Value>(d).unwrap())
            .find(|v| v["type"] == "response.completed")
            .unwrap();
        assert_eq!(completed["response"]["output"].as_array().unwrap().len(), 3);
        assert_eq!(completed["response"]["output"][2]["type"], "function_call");
        assert_eq!(completed["response"]["output"][2]["call_id"], "call_1");
    }

    /// Codex Desktop 的 code-mode 全程：`exec` 按 `type:"custom"` 声明在 additional_tools 的
    /// namespace 壳里，上游按 `{ input }` 壳回参数，客户端必须拿到 `custom_tool_call`。
    /// 回成 `function_call` 的话 Codex 对不上载荷类型，整次调用不记结果，模型只看到
    /// 「aborted」——2026-09-04 本地网关上 exec 无限重试就是这个。
    #[tokio::test]
    async fn responses_codex_exec_grammar_tool_comes_back_as_a_custom_tool_call() {
        let js = "await tools.exec_command({ cmd: \"ls\" });";
        let exec = ToolCall {
            id: "call_exec".into(),
            name: "exec".into(),
            arguments: json!({ "input": js }).to_string(),
        };
        let spawn_agent = ToolCall {
            id: "call_spawn".into(),
            name: "spawn_agent".into(),
            arguments: r#"{"task":"t"}"#.into(),
        };
        let (base, upstream) = spawn(
            FakeUpstream::chat(vec![], Ok(completion("", vec![exec, spawn_agent]))),
            None,
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/responses"))
            .json(&json!({
                "model": "m", "stream": true, "tools": [],
                "input": [
                    { "role": "user", "content": [{ "type": "input_text", "text": "list files" }] },
                    { "type": "additional_tools", "tools": [
                        { "type": "namespace", "name": "functions", "tools": [
                            { "type": "custom", "name": "exec", "description": "Run JS.",
                              "format": { "type": "grammar", "syntax": "lark", "definition": "start: /.*/" } },
                        ]},
                        { "type": "namespace", "name": "collaboration", "tools": [
                            { "type": "function", "name": "spawn_agent", "parameters": { "type": "object" } },
                        ]},
                    ]},
                ],
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);

        // 上游只认 JSON Schema：exec 到它那里是 { input: string } 的函数，名字不变。
        {
            let seen = upstream.seen.lock().unwrap();
            let exec_def = seen[0].tools.iter().find(|t| t.name == "exec").unwrap();
            assert!(exec_def.grammar);
            assert_eq!(exec_def.parameters["properties"]["input"]["type"], "string");
        }

        let text = res.text().await.unwrap();
        let order: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("event: "))
            .collect();
        assert!(order.contains(&"response.custom_tool_call_input.delta"));
        assert!(order.contains(&"response.custom_tool_call_input.done"));
        assert!(order.contains(&"response.function_call_arguments.done"));
        let items: Vec<Value> = text
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .map(|d| serde_json::from_str::<Value>(d).unwrap())
            .filter(|v| v["type"] == "response.output_item.done")
            .map(|v| v["item"].clone())
            .collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["type"], "custom_tool_call");
        assert_eq!(items[0]["name"], "exec");
        assert_eq!(items[0]["call_id"], "call_exec");
        assert_eq!(
            items[0]["input"], js,
            "拆掉 {{input}} 壳，客户端拿到的是裸 JS"
        );
        assert!(items[0].get("namespace").is_none(), "默认命名空间不带字段");
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(items[1]["name"], "spawn_agent");
        assert_eq!(
            items[1]["namespace"], "collaboration",
            "非默认命名空间原样带回，Codex 靠它找 handler"
        );
        let completed: Value = text
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .map(|d| serde_json::from_str::<Value>(d).unwrap())
            .find(|v| v["type"] == "response.completed")
            .unwrap();
        assert_eq!(completed["response"]["output"], json!(items));
    }

    #[tokio::test]
    async fn responses_rejects_previous_response_id_without_history() {
        let (base, _) = spawn(
            FakeUpstream {
                deltas: vec![],
                result: Ok(completion("ok", vec![])),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/responses"))
            .json(&json!({ "model": "m", "previous_response_id": "resp_1", "input": "hi again" }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 400);
        let body: Value = res.json().await.unwrap();
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("store: false"));
    }

    #[tokio::test]
    async fn upstream_error_before_stream_is_a_proper_http_status() {
        let (base, _) = spawn(
            FakeUpstream {
                deltas: vec![],
                result: Err(UpstreamError::new(UpstreamKind::Auth, 401, "token dead")),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/messages"))
            .json(&json!({ "model": "m", "max_tokens": 5, "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 401);
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["error"]["type"], "authentication_error");
        assert_eq!(body["error"]["message"], "token dead");
    }

    #[tokio::test]
    async fn upstream_error_mid_stream_is_reported_in_band_and_terminated() {
        let (base, _) = spawn(
            FakeUpstream {
                deltas: vec![Delta::Text("part".into())],
                result: Err(UpstreamError::new(
                    UpstreamKind::RateLimit,
                    429,
                    "slow down",
                )),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({ "model": "m", "stream": true, "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200, "头已经出去了，只能在流里说");
        let text = res.text().await.unwrap();
        assert!(text.contains("[网关错误] slow down"));
        assert!(text.contains(r#""type":"error""#));
        assert!(text.trim_end().ends_with("data: [DONE]"));
    }

    #[tokio::test]
    async fn api_key_is_enforced_when_configured() {
        let (base, _) = spawn(
            FakeUpstream {
                deltas: vec![],
                result: Ok(completion("ok", vec![])),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            Some("local-secret"),
        )
        .await;
        let body = json!({ "model": "m", "messages": [{ "role": "user", "content": "x" }] });
        let unauth = http()
            .post(format!("{base}/v1/chat/completions"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(unauth.status(), 401);
        let bearer = http()
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth("local-secret")
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(bearer.status(), 200);
        let xkey = http()
            .post(format!("{base}/v1/messages"))
            .header("x-api-key", "local-secret")
            .json(&json!({ "model": "m", "max_tokens": 5, "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(xkey.status(), 200);
    }

    #[tokio::test]
    async fn bad_requests_get_400_in_the_dialects_shape() {
        let (base, _) = spawn(
            FakeUpstream {
                deltas: vec![],
                result: Ok(completion("ok", vec![])),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
        )
        .await;
        let not_json = http()
            .post(format!("{base}/v1/chat/completions"))
            .header("content-type", "application/json")
            .body("{nope")
            .send()
            .await
            .unwrap();
        assert_eq!(not_json.status(), 400);
        let empty = http()
            .post(format!("{base}/v1/messages"))
            .json(&json!({ "model": "m", "messages": [] }))
            .send()
            .await
            .unwrap();
        assert_eq!(empty.status(), 400);
        let body: Value = empty.json().await.unwrap();
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["message"], "messages required");
    }

    #[tokio::test]
    async fn aliases_models_and_count_tokens_work() {
        let (base, _) = spawn(
            FakeUpstream {
                deltas: vec![],
                result: Ok(completion("ok", vec![])),
                seen: Mutex::new(vec![]),
                images: Mutex::new(vec![]),
                seen_images: Mutex::new(vec![]),
            },
            None,
        )
        .await;
        for path in [
            "/v1/models",
            "/openai/v1/models",
            "/anthropic/v1/models",
            "/v1/v1/models",
        ] {
            let res = http().get(format!("{base}{path}")).send().await.unwrap();
            assert_eq!(res.status(), 200, "{path}");
            let body: Value = res.json().await.unwrap();
            assert_eq!(body["object"], "list");
            assert!(body["data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["id"] == "cursor/auto"));
            assert!(
                body["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|m| m["id"].as_str().is_some_and(|id| id.contains('/'))),
                "目录只报 通道/模型，不列裸名"
            );
        }
        let res = http()
            .post(format!("{base}/v1/messages/count_tokens"))
            .json(&json!({ "model": "m", "messages": [{ "role": "user", "content": "hello world again" }] }))
            .send()
            .await
            .unwrap();
        let body: Value = res.json().await.unwrap();
        assert!(body["input_tokens"].as_u64().unwrap() >= 1);
        assert_eq!(
            http()
                .get(format!("{base}/healthz"))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap(),
            "ok"
        );
    }

    // ---------- 请求内换号 ----------

    /// 按顺序发号的假 lane，记下每次回报。发完了就说没号。
    struct ScriptedLane {
        queue: Mutex<Vec<&'static str>>,
        reports: Mutex<Vec<(String, &'static str)>>,
    }

    impl ScriptedLane {
        fn new(labels: &[&'static str]) -> Self {
            Self {
                queue: Mutex::new(labels.to_vec()),
                reports: Mutex::new(vec![]),
            }
        }
    }

    impl Lane for ScriptedLane {
        fn acquire<'a>(
            &'a self,
            _model: &'a str,
        ) -> BoxFuture<'a, Result<Credential, UpstreamError>> {
            Box::pin(async move {
                let mut q = self.queue.lock().unwrap();
                if q.is_empty() {
                    return Err(UpstreamError::new(
                        UpstreamKind::Upstream,
                        503,
                        "没有可用的账号",
                    ));
                }
                let label = q.remove(0);
                Ok(Credential {
                    label: label.into(),
                    access_token: "tok".into(),
                    identity: DeviceIdentity::derived(label),
                })
            })
        }

        fn report(&self, credential: &Credential, _model: &str, outcome: Outcome<'_>) {
            let tag = match outcome {
                Outcome::Ok(_) => "ok",
                Outcome::Err(e) => e.kind.as_str(),
            };
            self.reports
                .lock()
                .unwrap()
                .push((credential.label.clone(), tag));
        }
    }

    /// 一个号的脚本：先吐的增量 + 结局。
    type Script = (Vec<Delta>, Result<Completion, UpstreamError>);

    /// 看号回话的假后端：每个号一段脚本，并记下被哪些号调过。
    struct PerAccountUpstream {
        scripts: std::collections::HashMap<&'static str, Script>,
        seen: Mutex<Vec<String>>,
    }

    impl Upstream for PerAccountUpstream {
        fn stream<'a>(
            &'a self,
            credential: &'a Credential,
            _request: &'a ChatRequest,
            on_delta: DeltaSink<'a>,
        ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
            Box::pin(async move {
                self.seen.lock().unwrap().push(credential.label.clone());
                let (deltas, result) = self
                    .scripts
                    .get(credential.label.as_str())
                    .expect("脚本里没有这个号");
                for d in deltas {
                    on_delta(d.clone());
                }
                result.clone()
            })
        }
    }

    fn dead(kind: UpstreamKind, status: u16, msg: &str) -> Script {
        (vec![], Err(UpstreamError::new(kind, status, msg)))
    }

    async fn spawn_relay(
        labels: &[&'static str],
        scripts: Vec<(&'static str, Script)>,
    ) -> (String, Arc<ScriptedLane>, Arc<PerAccountUpstream>) {
        let lane = Arc::new(ScriptedLane::new(labels));
        let upstream = Arc::new(PerAccountUpstream {
            scripts: scripts.into_iter().collect(),
            seen: Mutex::new(vec![]),
        });
        let gw = Arc::new(Gateway::single(lane.clone(), upstream.clone()));
        let listener = bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener, router(gw), std::future::pending()));
        (format!("http://{addr}"), lane, upstream)
    }

    #[tokio::test]
    async fn a_dead_account_is_relayed_past_before_the_first_byte_in_both_modes() {
        // 真机 2026-09-04：号池 33 个号，当前号欠费，客户端连吃四个 429 才轮到一个能用的。
        // 号池的意义就是把这种事吞掉：怪号的错误在首字节之前换号重来，客户端只看到成功。
        let (base, lane, up) = spawn_relay(
            &["unpaid@x", "closed@x", "live@x"],
            vec![
                (
                    "unpaid@x",
                    dead(
                        UpstreamKind::Quota,
                        402,
                        "ERROR_RATE_LIMITED: You have an unpaid invoice",
                    ),
                ),
                (
                    "closed@x",
                    dead(UpstreamKind::Auth, 401, "ERROR_ACCOUNT_CLOSED"),
                ),
                (
                    "live@x",
                    (
                        vec![Delta::Text("PONG".into())],
                        Ok(completion("PONG", vec![])),
                    ),
                ),
            ],
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({ "model": "claude-sonnet-5", "messages": [{ "role": "user", "content": "ping" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["choices"][0]["message"]["content"], "PONG");
        assert_eq!(*up.seen.lock().unwrap(), ["unpaid@x", "closed@x", "live@x"]);
        // 每个号的结局都回报给了 lane（它据此标耗尽），也就是每一次尝试都收了尾。
        assert_eq!(
            *lane.reports.lock().unwrap(),
            [
                ("unpaid@x".to_string(), "quota"),
                ("closed@x".to_string(), "auth"),
                ("live@x".to_string(), "ok"),
            ]
        );

        // 流式同理：换号发生在保活注释之后、首个内容帧之前，客户端看到的是一段干净的流。
        let (base, _, up) = spawn_relay(
            &["unpaid@x", "live@x"],
            vec![
                (
                    "unpaid@x",
                    dead(
                        UpstreamKind::RateLimit,
                        429,
                        "ERROR_RATE_LIMITED_CHANGEABLE",
                    ),
                ),
                (
                    "live@x",
                    (
                        vec![Delta::Text("PONG".into())],
                        Ok(completion("PONG", vec![])),
                    ),
                ),
            ],
        )
        .await;
        let text = http()
            .post(format!("{base}/v1/messages"))
            .json(&json!({ "model": "m", "stream": true, "max_tokens": 5,
                           "messages": [{ "role": "user", "content": "ping" }] }))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(*up.seen.lock().unwrap(), ["unpaid@x", "live@x"]);
        assert!(text.contains("PONG"), "{text}");
        assert!(
            !text.contains("event: error"),
            "换号成功后不该有错误帧：{text}"
        );
        assert!(text.contains("message_stop"), "{text}");
    }

    #[tokio::test]
    async fn relay_stops_where_switching_accounts_cannot_help() {
        // 供应商在抖：每个号都会撞上同一条，换号只是白烧号。原样回 429，只打了一个号。
        let (base, _, up) = spawn_relay(
            &["a@x", "b@x"],
            vec![
                (
                    "a@x",
                    dead(
                        UpstreamKind::Provider,
                        429,
                        "ERROR_PROVIDER_ERROR: provider down",
                    ),
                ),
                ("b@x", (vec![], Ok(completion("never", vec![])))),
            ],
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({ "model": "m", "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 429);
        assert_eq!(*up.seen.lock().unwrap(), ["a@x"]);

        // 已经吐过增量再失败：客户端手里有半段回答，换号重来会拼出两段。不换，如实报错。
        let (base, _, up) = spawn_relay(
            &["a@x", "b@x"],
            vec![
                (
                    "a@x",
                    (
                        vec![Delta::Text("half".into())],
                        Err(UpstreamError::new(
                            UpstreamKind::Quota,
                            402,
                            "dry mid-stream",
                        )),
                    ),
                ),
                ("b@x", (vec![], Ok(completion("never", vec![])))),
            ],
        )
        .await;
        let text = http()
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({ "model": "m", "stream": true, "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert_eq!(*up.seen.lock().unwrap(), ["a@x"]);
        assert!(text.contains("half"), "{text}");
        assert!(text.contains("dry mid-stream"), "{text}");

        // 试满上限还是不行：回最后一个号的错误，而不是继续把号池烧完。
        let (base, _, up) = spawn_relay(
            &["a@x", "b@x", "c@x", "d@x", "e@x"],
            vec![
                ("a@x", dead(UpstreamKind::Quota, 402, "a dry")),
                ("b@x", dead(UpstreamKind::Quota, 402, "b dry")),
                ("c@x", dead(UpstreamKind::Auth, 401, "c closed")),
                ("d@x", dead(UpstreamKind::RateLimit, 429, "d limited")),
                ("e@x", (vec![], Ok(completion("never", vec![])))),
            ],
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({ "model": "m", "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 429);
        let body: Value = res.json().await.unwrap();
        assert_eq!(body["error"]["message"], "d limited");
        assert_eq!(up.seen.lock().unwrap().len(), MAX_ACCOUNT_ATTEMPTS);

        // 换号途中 lane 也没号了：回上游最后那条错误——它才说明发生了什么，状态码客户端也认得。
        let (base, _, _) = spawn_relay(
            &["a@x"],
            vec![(
                "a@x",
                dead(
                    UpstreamKind::Quota,
                    402,
                    "ERROR_RATE_LIMITED: You have an unpaid invoice",
                ),
            )],
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({ "model": "m", "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 402);
        let body: Value = res.json().await.unwrap();
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unpaid invoice"));

        // lane 不认回报、把刚失败的号原样递回来（StaticLane 就是这样）：别在同一个号上撞四次。
        let (base, _, up) = spawn_relay(
            &["a@x", "a@x", "a@x", "a@x"],
            vec![("a@x", dead(UpstreamKind::Quota, 402, "a dry"))],
        )
        .await;
        let res = http()
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({ "model": "m", "messages": [{ "role": "user", "content": "x" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(res.status(), 402);
        assert_eq!(*up.seen.lock().unwrap(), ["a@x"], "同一个号只试一次");
    }
}
