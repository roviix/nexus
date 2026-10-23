//! 本地烟测：Cursor Stream 墓碑必须在取号/出网前失败；非流式回 HTTP 503，流式在
//! HTTP 200 SSE 内回 `code=503` 并完整终结；目录不再宣传对话模型。

use nexus_gateway::channel::{self, ChannelRegistry};
use nexus_gateway::lane::{Credential, StaticLane};
use nexus_gateway::server::{self, Gateway};
use nexus_gateway::{CursorUpstream, DeviceIdentity, StreamConfig};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let lane = Arc::new(StaticLane::new(Credential {
        label: "must-not-be-acquired@example.invalid".into(),
        access_token: "must-not-leave-process".into(),
        identity: DeviceIdentity::derived("must-not-leave-process"),
    }));
    let cursor = channel::unavailable_cursor_channel(
        lane,
        Arc::new(CursorUpstream::new(StreamConfig::default())),
    );
    let gateway = Arc::new(Gateway {
        channels: ChannelRegistry::new(cursor),
        api_key: None,
        ledger: None,
        media_jobs: None,
        routes: Arc::default(),
        log: None,
        mesh: None,
    });
    let listener = server::bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(server::serve(listener, server::router(gateway), async {
        let _ = stop_rx.await;
    }));
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let base = format!("http://{address}");

    let response = client
        .post(format!("{base}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "cursor/claude-sonnet-5",
            "messages": [{ "role": "user", "content": "ping" }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    let error: serde_json::Value = response.json().await.unwrap();
    let message = error["error"]["message"].as_str().unwrap_or("");
    assert!(message.contains("InferenceService/Stream"), "{error}");

    let models: serde_json::Value = client
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
        .filter_map(|model| model["id"].as_str())
        .collect();
    assert!(!ids.contains(&"cursor/auto"), "{ids:?}");
    assert!(
        ids.contains(&"cursor/nano-banana-2"),
        "生图不是 Stream，不能替它判死：{ids:?}"
    );

    let stream = client
        .post(format!("{base}/v1/messages"))
        .json(&serde_json::json!({
            "model": "cursor/claude-sonnet-5",
            "stream": true,
            "max_tokens": 8,
            "messages": [{ "role": "user", "content": "ping" }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(stream.status(), 200, "流式错误在 SSE 内终结");
    let stream = stream.text().await.unwrap();
    assert!(stream.contains("InferenceService/Stream"), "{stream}");
    assert!(stream.contains(r#""code":503"#), "{stream}");
    assert!(stream.contains("message_stop"), "{stream}");

    println!(
        "PASS · 非流式 HTTP 503 · 流式 HTTP 200 + SSE code=503 完整终结 · Cursor 对话目录已隐藏 · 生图目录保留"
    );
    let _ = stop_tx.send(());
    task.await.unwrap().unwrap();
}
