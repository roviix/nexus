//! 「试一下」：对着本机网关的 OpenAI 口发一句话，把流式回字逐段交给回调。
//!
//! 走真 HTTP 而不是直接调 `Upstream`：用户点「试一下」想知道的是**这个地址、这把口令**
//! 能不能用——口令校验、方言解析、模型映射、号的接力，一个都不该跳过。所以这里就是
//! 一个最小的 OpenAI Chat 客户端，只认我们自己 `inbound::serialize` 吐出的那几种帧。

use nexus_core::AppError;
use serde::Serialize;
use std::time::Duration;

/// 一次试用里会发生的事，按时间顺序交给回调。
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TryEvent {
    /// 上游实际路由到的模型（`auto` 时才有意义）。
    Routed { model: String },
    /// 正文增量。
    Delta { text: String },
    /// 思考增量（`reasoning_content`）。
    Thinking { text: String },
    /// 正常收尾。`usage` 是网关在最后一帧里报的；`finish` 是 OpenAI 的 finish_reason。
    Done {
        finish: Option<String>,
        usage: Option<TryUsage>,
    },
    /// 收尾之后单独补来的用量帧（OpenAI `stream_options.include_usage` 的形态：
    /// `finish_reason` 那帧不带 usage，再来一帧 `choices: []` 只带 usage）。
    Usage { usage: TryUsage },
    /// 流内错误：网关已把上游错误原因塞进帧里并终结了流。
    Error { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TryUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// 首字前最多等这么久：网关自己对上游有心跳与超时，这里只兜「网关本身没响应」。
const FIRST_BYTE_TIMEOUT: Duration = Duration::from_secs(60);

/// OpenAI Chat 形状的一条消息。游乐场把整段历史按这个形状发上去。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChatMessage {
    /// system | user | assistant
    pub role: String,
    pub content: Content,
}

/// 一条消息的内容。
///
/// `untagged` 是为了让**纯文本仍旧序列化成一个字符串**：这是所有上游都认的老形状，
/// 只有真带了图那条才摊成 OpenAI 的 parts 数组。反过来（一律发数组）会把只认字符串的
/// 兼容上游打回 400。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Content {
    Text(String),
    Parts(Vec<Part>),
}

/// 多模态内容里的一块。图片用 `data:<mime>;base64,…` 内联——本机文件路径上游取不到。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImageUrl {
    pub url: String,
}

impl ChatMessage {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Content::Text(content.into()),
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: Content::Text(content.into()),
        }
    }

    /// 带图的那句话：正文与图片各占一块。
    pub fn user_parts(parts: Vec<Part>) -> Self {
        Self {
            role: "user".into(),
            content: Content::Parts(parts),
        }
    }
}

/// 发一句话，逐帧回调。返回 `Ok(())` 表示流正常走完（含流内 `Error` 事件那种）；
/// `Err` 是连网关都没打通：拒绝口令、地址不通、请求形状被 400。
///
/// 客户端现建：一次试用一条连接，没有复用的价值；调用方（Tauri 层）也不必为此引 reqwest。
pub async fn run(
    base_url: &str,
    api_key: &str,
    model: &str,
    prompt: &str,
    on_event: impl FnMut(TryEvent),
) -> Result<(), AppError> {
    run_messages(
        base_url,
        api_key,
        model,
        &[ChatMessage::user(prompt)],
        on_event,
    )
    .await
}

/// 多轮版本：把整段历史发上去。游乐场用它，「试一下」是它只有一条 user 消息的特例。
///
/// `stream_options.include_usage` 显式带上：云端中转对 openai_http 上游本来就会在末帧
/// 回 usage，别的兼容上游要看到这个字段才给；本地网关按 `Value` 解请求，多一个字段无害。
pub async fn run_messages(
    base_url: &str,
    api_key: &str,
    model: &str,
    messages: &[ChatMessage],
    mut on_event: impl FnMut(TryEvent),
) -> Result<(), AppError> {
    let client = http_client(base_url)?;
    let body = serde_json::json!({
        "model": model,
        "stream": true,
        "stream_options": { "include_usage": true },
        "messages": messages,
    });
    let res = tokio::time::timeout(
        FIRST_BYTE_TIMEOUT,
        client
            .post(format!(
                "{}/v1/chat/completions",
                base_url.trim_end_matches('/')
            ))
            .bearer_auth(api_key)
            .json(&body)
            .send(),
    )
    .await
    .map_err(|_| AppError::network("网关没有在 60 秒内响应。"))?
    .map_err(|e| AppError::network(format!("连不上网关：{e}")))?;

    let status = res.status();
    if !status.is_success() {
        let text = res.text().await.unwrap_or_default();
        return Err(AppError::upstream(http_error_message(
            status.as_u16(),
            &text,
        )));
    }

    let mut res = res;
    let mut buf: Vec<u8> = Vec::new();
    let mut routed: Option<String> = None;
    // `chunk()` 不需要 reqwest 的 stream 特性；inference 那边读上游也是这么读的。
    while let Some(chunk) = res
        .chunk()
        .await
        .map_err(|e| AppError::network(format!("读网关响应中断：{e}")))?
    {
        buf.extend_from_slice(&chunk);
        // SSE 帧以空行分隔；一帧里可能有多行 data:。
        while let Some(pos) = find_frame_end(&buf) {
            let frame: Vec<u8> = buf.drain(..pos + 2).collect();
            let frame = String::from_utf8_lossy(&frame);
            for line in frame.lines() {
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data == "[DONE]" {
                    return Ok(());
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                    continue;
                };
                for ev in events_from_chunk(&v) {
                    // Routed 只报一次：每一帧都带 model，第一帧之后都是重复。
                    if let TryEvent::Routed { model } = &ev {
                        if routed.as_deref() == Some(model.as_str()) {
                            continue;
                        }
                        routed = Some(model.clone());
                    }
                    on_event(ev);
                }
            }
        }
    }
    Ok(())
}

fn find_frame_end(buf: &[u8]) -> Option<usize> {
    buf.windows(2).position(|w| w == b"\n\n")
}

fn is_loopback(base_url: &str) -> bool {
    let u = base_url.to_ascii_lowercase();
    u.contains("127.0.0.1") || u.contains("localhost") || u.contains("[::1]")
}

/// 游乐场打的是本机网关时，必须绕过系统代理、且只用 HTTP/1.1。
/// 这跟上游 api2 走 HTTP/2 无关：`127.0.0.1:8687` 是本机 axum，再被 Clash
/// HTTP 代理拐走会回一页空/HTML 502，账本里一行都没有。
fn http_client(base_url: &str) -> Result<reqwest::Client, AppError> {
    let mut b = reqwest::Client::builder().connect_timeout(Duration::from_secs(5));
    if is_loopback(base_url) {
        b = b.http1_only().no_proxy();
    }
    b.build()
        .map_err(|e| AppError::internal(format!("http 客户端初始化失败：{e}")))
}

/// 网关拒绝时的正文是 `{"error":{"message":…}}`；能解就用它的话，不能就报状态码。
fn http_error_message(status: u16, body: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(body).ok();
    let detail = parsed.as_ref().and_then(|v| {
        v["error"]["message"]
            .as_str()
            .or_else(|| v["message"].as_str())
            .or_else(|| v["error"].as_str())
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
    });
    if let Some(m) = detail {
        return format!("网关拒绝了请求（{status}）：{m}");
    }
    match status {
        401 => "网关拒绝了口令（401）。换过口令的话，重开这一页再试。".into(),
        404 => "网关上没有 /v1/chat/completions（404）——端口指错了？".into(),
        _ => {
            let snippet = body.trim();
            if !snippet.is_empty() && snippet.len() < 240 && !snippet.starts_with('<') {
                format!("网关返回 {status}：{snippet}")
            } else if snippet.is_empty() {
                format!(
                    "网关返回 {status}，且没有正文。请求若没进账本，多半还没打到方言口（本机代理拦了 127.0.0.1，或进程内 HTTP 异常）。"
                )
            } else {
                format!("网关返回 {status}。")
            }
        }
    }
}

/// 一帧 OpenAI Chat 流式 JSON → 零到多个事件。纯函数，方便对着固定帧做测试。
pub fn events_from_chunk(v: &serde_json::Value) -> Vec<TryEvent> {
    let mut out = Vec::new();
    // 流内错误帧：`{"error":{...}}`（可能同时带 choices，为空）。
    if let Some(msg) = v["error"]["message"].as_str() {
        out.push(TryEvent::Error {
            message: msg.to_string(),
        });
        return out;
    }
    if let Some(model) = v["model"].as_str().filter(|m| !m.is_empty()) {
        out.push(TryEvent::Routed {
            model: model.to_string(),
        });
    }
    let choice = &v["choices"][0];
    // 只带 usage、没有 choice 的尾帧。
    if choice.is_null() {
        if let Some(usage) = parse_usage(&v["usage"]) {
            out.push(TryEvent::Usage { usage });
        }
        return out;
    }
    if let Some(t) = choice["delta"]["reasoning_content"]
        .as_str()
        .filter(|t| !t.is_empty())
    {
        out.push(TryEvent::Thinking {
            text: t.to_string(),
        });
    }
    if let Some(t) = choice["delta"]["content"]
        .as_str()
        .filter(|t| !t.is_empty())
    {
        out.push(TryEvent::Delta {
            text: t.to_string(),
        });
    }
    if let Some(finish) = choice["finish_reason"].as_str() {
        out.push(TryEvent::Done {
            finish: Some(finish.to_string()),
            usage: parse_usage(&v["usage"]),
        });
    }
    out
}

fn parse_usage(v: &serde_json::Value) -> Option<TryUsage> {
    v.as_object().map(|u| TryUsage {
        prompt_tokens: u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
        completion_tokens: u
            .get("completion_tokens")
            .and_then(|x| x.as_u64())
            .unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_content_chunk_yields_routed_model_and_delta() {
        let v = json!({ "id": "c1", "model": "composer-2.5-fast",
                        "choices": [{ "index": 0, "delta": { "content": "你好" } }] });
        assert_eq!(
            events_from_chunk(&v),
            vec![
                TryEvent::Routed {
                    model: "composer-2.5-fast".into()
                },
                TryEvent::Delta {
                    text: "你好".into()
                },
            ]
        );
    }

    #[test]
    fn reasoning_goes_to_thinking_and_the_last_chunk_carries_usage() {
        let v = json!({ "model": "claude-sonnet-5",
                        "choices": [{ "index": 0, "delta": { "reasoning_content": "想一想" } }] });
        assert!(
            matches!(&events_from_chunk(&v)[1], TryEvent::Thinking { text } if text == "想一想")
        );

        let last = json!({ "model": "claude-sonnet-5",
                           "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
                           "usage": { "prompt_tokens": 12, "completion_tokens": 34, "total_tokens": 46 } });
        let evs = events_from_chunk(&last);
        assert_eq!(
            evs.last(),
            Some(&TryEvent::Done {
                finish: Some("stop".into()),
                usage: Some(TryUsage {
                    prompt_tokens: 12,
                    completion_tokens: 34
                }),
            })
        );
    }

    #[test]
    fn a_usage_only_tail_frame_becomes_a_usage_event() {
        // include_usage 的形态：finish 那帧不带 usage，再补一帧 choices 为空、只有 usage。
        let tail = json!({ "model": "gpt-5.6-sol", "choices": [],
                           "usage": { "prompt_tokens": 7, "completion_tokens": 21 } });
        assert_eq!(
            events_from_chunk(&tail),
            vec![
                TryEvent::Routed {
                    model: "gpt-5.6-sol".into()
                },
                TryEvent::Usage {
                    usage: TryUsage {
                        prompt_tokens: 7,
                        completion_tokens: 21
                    }
                },
            ]
        );
        // 空 choices 且没有 usage：什么都不该冒出来（心跳类帧）。
        assert!(events_from_chunk(&json!({ "choices": [] })).is_empty());
    }

    #[test]
    fn chat_messages_serialize_in_openai_shape() {
        let v =
            serde_json::to_value([ChatMessage::user("hi"), ChatMessage::assistant("yo")]).unwrap();
        assert_eq!(v[0], json!({ "role": "user", "content": "hi" }));
        assert_eq!(v[1], json!({ "role": "assistant", "content": "yo" }));
    }

    #[test]
    fn a_message_with_images_serializes_as_a_content_array() {
        let m = ChatMessage::user_parts(vec![
            Part::Text {
                text: "这张图里是什么".into(),
            },
            Part::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/png;base64,AAAA".into(),
                },
            },
        ]);
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["role"], "user");
        assert_eq!(
            v["content"],
            json!([
                { "type": "text", "text": "这张图里是什么" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } },
            ])
        );
    }

    #[test]
    fn an_in_band_error_frame_becomes_a_single_error_event() {
        let v = json!({ "error": { "message": "ERROR_RATE_LIMITED: 慢一点", "type": "upstream_error", "code": 429 },
                        "choices": [] });
        assert_eq!(
            events_from_chunk(&v),
            vec![TryEvent::Error {
                message: "ERROR_RATE_LIMITED: 慢一点".into()
            }]
        );
    }

    #[test]
    fn empty_deltas_produce_nothing_but_the_model() {
        let v = json!({ "model": "auto", "choices": [{ "index": 0, "delta": { "role": "assistant", "content": "" } }] });
        assert_eq!(
            events_from_chunk(&v),
            vec![TryEvent::Routed {
                model: "auto".into()
            }]
        );
    }

    #[test]
    fn http_errors_prefer_the_gateways_own_message() {
        assert_eq!(
            http_error_message(401, r#"{"error":{"message":"invalid api key"}}"#),
            "网关拒绝了请求（401）：invalid api key"
        );
        assert!(http_error_message(401, "not json").contains("口令"));
        assert!(http_error_message(503, "").contains("503"));
        assert!(http_error_message(502, "").contains("没有正文"));
    }

    #[test]
    fn loopback_urls_are_the_ones_that_must_bypass_the_proxy() {
        assert!(is_loopback("http://127.0.0.1:8687"));
        assert!(is_loopback("http://localhost:8687"));
        assert!(!is_loopback("https://api.roviix.com"));
    }

    #[test]
    fn frame_boundary_is_the_blank_line() {
        assert_eq!(find_frame_end(b"data: a\n\ndata: b"), Some(7));
        assert_eq!(find_frame_end(b"data: a\n"), None);
    }
}
