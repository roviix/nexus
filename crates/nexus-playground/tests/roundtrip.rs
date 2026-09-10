//! 对着一个假的 OpenAI 口把整条链路走一遍：发一轮 → 流式攒字 → 落库 → 再发一轮时历史带上；
//! 出一批图 → b64 / url 两种回法 → 落盘 + 落库；中途停止 → 半截回复带「已停止」落库。
//!
//! 假服务器是 `std::net` 手写的：只认这几种请求，按顺序吐预先写好的响应。不引 axum / wiremock，
//! 这个 crate 的测试依赖不该比它本体重。

use base64::Engine;
use nexus_gateway::playground::TryEvent;
use nexus_playground::{Attachment, Endpoint, ImageRequest, Kind, PlaygroundService, Role, Source};
use nexus_store::Db;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// 按顺序服务 N 次请求，把每次请求的原文记下来。第 i 次请求拿第 i 份响应。
struct FakeUpstream {
    base_url: String,
    requests: Arc<Mutex<Vec<String>>>,
}

fn read_request(s: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = s.read(&mut tmp).unwrap_or(0);
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= pos + 4 + len {
                break;
            }
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn serve(responses: Vec<Vec<u8>>) -> FakeUpstream {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    std::thread::spawn(move || {
        for response in responses {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let req = read_request(&mut s);
            seen.lock().unwrap().push(req);
            let _ = s.write_all(&response);
            let _ = s.flush();
        }
    });
    FakeUpstream { base_url, requests }
}

fn http(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
    let mut out = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

fn sse(frames: &[&str]) -> Vec<u8> {
    let body: String = frames.iter().map(|f| format!("data: {f}\n\n")).collect();
    http("200 OK", "text/event-stream", body.as_bytes())
}

fn png(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend_from_slice(&13u32.to_be_bytes());
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.extend_from_slice(&[8, 6, 0, 0, 0]);
    v
}

fn service() -> (PlaygroundService, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db = Arc::new(Db::open_in_memory().unwrap());
    (PlaygroundService::new(db, dir.path()), dir)
}

fn endpoint(up: &FakeUpstream) -> Endpoint {
    Endpoint {
        base_url: up.base_url.clone(),
        api_key: "sk-test".into(),
    }
}

#[tokio::test]
async fn two_turns_of_chat_stream_persist_and_carry_history() {
    let up = serve(vec![
        sse(&[
            r#"{"model":"claude-sonnet-5","choices":[{"index":0,"delta":{"role":"assistant","content":"你"}}]}"#,
            r#"{"model":"claude-sonnet-5","choices":[{"index":0,"delta":{"reasoning_content":"想想"}}]}"#,
            r#"{"model":"claude-sonnet-5","choices":[{"index":0,"delta":{"content":"好"}}]}"#,
            r#"{"model":"claude-sonnet-5","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            // include_usage 的尾帧：choices 为空、只有 usage。
            r#"{"model":"claude-sonnet-5","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2}}"#,
            "[DONE]",
        ]),
        sse(&[
            r#"{"model":"claude-sonnet-5","choices":[{"index":0,"delta":{"content":"第二轮"}}]}"#,
            r#"{"model":"claude-sonnet-5","choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":9,"completion_tokens":3}}"#,
            "[DONE]",
        ]),
    ]);
    let (s, _dir) = service();
    let ep = endpoint(&up);
    let t = s
        .create_thread(Kind::Chat, Source::Cloud, "auto", Some("tok"))
        .unwrap();

    let mut seen = Vec::new();
    let reply = s
        .chat("r1", &t.id, Some("你好吗"), &[], &ep, |ev| seen.push(ev))
        .await
        .unwrap();
    assert_eq!(reply.role, Role::Assistant);
    assert_eq!(reply.content, "你好");
    assert_eq!(reply.thinking.as_deref(), Some("想想"));
    assert_eq!(reply.routed.as_deref(), Some("claude-sonnet-5"));
    assert_eq!(reply.model.as_deref(), Some("auto"));
    let usage = reply.usage.expect("尾帧里的 usage 要被接住");
    assert_eq!((usage.prompt_tokens, usage.completion_tokens), (5, 2));
    assert!(reply.error.is_none());
    assert!(reply.ttft_ms.is_some());
    assert!(reply.duration_ms.is_some());
    assert_eq!(reply.seq, 2);
    assert!(
        seen.iter().any(|e| matches!(e, TryEvent::Usage { .. })),
        "监听者也该看到用量帧"
    );

    let d = s.thread(&t.id).unwrap();
    assert_eq!(d.thread.title, "你好吗");
    assert_eq!(d.messages.len(), 2);
    assert_eq!(d.messages[1].content, "你好");

    // 第二轮：历史要整段带上，包括上一轮的回复。
    let reply2 = s
        .chat("r2", &t.id, Some("再说一次"), &[], &ep, |_| {})
        .await
        .unwrap();
    assert_eq!(reply2.content, "第二轮");
    assert_eq!(reply2.usage.unwrap().completion_tokens, 3);

    let reqs = up.requests.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    assert!(reqs[0].starts_with("POST /v1/chat/completions "));
    assert!(reqs[0]
        .to_ascii_lowercase()
        .contains("authorization: bearer sk-test"));
    let body1: serde_json::Value =
        serde_json::from_str(reqs[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body1["model"], "auto");
    assert_eq!(body1["stream"], true);
    assert_eq!(body1["stream_options"]["include_usage"], true);
    assert_eq!(body1["messages"].as_array().unwrap().len(), 1);
    let body2: serde_json::Value =
        serde_json::from_str(reqs[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    let msgs = body2["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0]["role"], "user");
    assert_eq!(msgs[0]["content"], "你好吗");
    assert_eq!(msgs[1]["role"], "assistant");
    assert_eq!(msgs[1]["content"], "你好");
    assert_eq!(msgs[2]["content"], "再说一次");
}

#[tokio::test]
async fn an_attached_image_is_sent_as_content_parts_and_stays_in_later_turns() {
    let up = serve(vec![
        sse(&[
            r#"{"model":"m","choices":[{"index":0,"delta":{"content":"一只猫"}}]}"#,
            r#"{"model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]),
        sse(&[
            r#"{"model":"m","choices":[{"index":0,"delta":{"content":"橘色的"}}]}"#,
            r#"{"model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]),
    ]);
    let (s, _dir) = service();
    let ep = endpoint(&up);
    let t = s
        .create_thread(Kind::Chat, Source::Cloud, "m", Some("tok"))
        .unwrap();
    let bytes = png(8, 6);
    let files = [Attachment {
        name: "猫.png".into(),
        mime: "image/png".into(),
        data_base64: base64::engine::general_purpose::STANDARD.encode(&bytes),
    }];
    s.chat("r1", &t.id, Some("这是什么"), &files, &ep, |_| {})
        .await
        .unwrap();
    // 第二轮不带新附件：上一轮那张图仍要跟着历史发上去。
    s.chat("r2", &t.id, Some("什么颜色"), &[], &ep, |_| {})
        .await
        .unwrap();

    let expected = format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    );
    let reqs = up.requests.lock().unwrap();
    for (i, n) in [(0usize, 1usize), (1, 3)] {
        let body: serde_json::Value =
            serde_json::from_str(reqs[i].split("\r\n\r\n").nth(1).unwrap()).unwrap();
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), n);
        assert_eq!(msgs[0]["role"], "user");
        assert_eq!(
            msgs[0]["content"],
            serde_json::json!([
                { "type": "text", "text": "这是什么" },
                { "type": "image_url", "image_url": { "url": expected } },
            ])
        );
    }
    // 不带图的消息还是老形状：一个字符串。
    let body: serde_json::Value =
        serde_json::from_str(reqs[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["messages"][1]["content"], "一只猫");
    assert_eq!(body["messages"][2]["content"], "什么颜色");
}

#[tokio::test]
async fn an_in_band_error_frame_lands_on_the_reply_and_is_left_out_of_history() {
    let up = serve(vec![
        sse(&[
            r#"{"error":{"message":"ERROR_RATE_LIMITED: 慢一点","type":"upstream_error","code":429},"choices":[]}"#,
            "[DONE]",
        ]),
        sse(&[
            r#"{"model":"m","choices":[{"index":0,"delta":{"content":"ok"}}]}"#,
            r#"{"model":"m","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}"#,
            "[DONE]",
        ]),
    ]);
    let (s, _dir) = service();
    let ep = endpoint(&up);
    let t = s
        .create_thread(Kind::Chat, Source::Local, "m", None)
        .unwrap();
    let reply = s
        .chat("r1", &t.id, Some("问"), &[], &ep, |_| {})
        .await
        .unwrap();
    assert_eq!(reply.error.as_deref(), Some("ERROR_RATE_LIMITED: 慢一点"));
    assert!(reply.content.is_empty());

    // 重新生成：删掉那条失败的回复，用同一句话再问；历史里不该有空回复。
    let reply = s.chat("r2", &t.id, None, &[], &ep, |_| {}).await.unwrap();
    assert_eq!(reply.content, "ok");
    let d = s.thread(&t.id).unwrap();
    assert_eq!(d.messages.len(), 2, "失败那条被替换，不是叠着");
    let reqs = up.requests.lock().unwrap();
    let body: serde_json::Value =
        serde_json::from_str(reqs[1].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn stopping_mid_stream_keeps_the_partial_text_and_marks_it_stopped() {
    // 手写一个「吐一帧就挂住」的服务器：chunked，不收尾。
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let Ok((mut s, _)) = listener.accept() else {
            return;
        };
        let _ = read_request(&mut s);
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
        let frame = "data: {\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"半截\"}}]}\n\n";
        let _ = s.write_all(head.as_bytes());
        let _ = s.write_all(format!("{:x}\r\n{}\r\n", frame.len(), frame).as_bytes());
        let _ = s.flush();
        std::thread::sleep(Duration::from_secs(20));
    });
    let (s, _dir) = service();
    let ep = Endpoint {
        base_url,
        api_key: "k".into(),
    };
    let t = s
        .create_thread(Kind::Chat, Source::Local, "m", None)
        .unwrap();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let mut tx = Some(tx);
    let seen = Mutex::new(Vec::new());
    let chat = s.chat("req", &t.id, Some("说"), &[], &ep, |ev| {
        if matches!(ev, TryEvent::Delta { .. }) {
            if let Some(tx) = tx.take() {
                let _ = tx.send(());
            }
        }
        seen.lock().unwrap().push(ev);
    });
    let stopper = async {
        rx.await.ok();
        assert!(s.stop("req"));
    };
    let (reply, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(chat, stopper)
    })
    .await
    .expect("停止后命令要立刻返回，不能等上游");
    let reply = reply.unwrap();
    assert_eq!(reply.content, "半截");
    assert_eq!(reply.error.as_deref(), Some("已停止。"));
    assert!(s.active(&t.id).is_none());
    assert!(matches!(
        seen.lock().unwrap().last(),
        Some(TryEvent::Done { finish: Some(f), .. }) if f == "cancelled"
    ));
}

#[tokio::test]
async fn b64_images_land_on_disk_and_leave_with_the_thread() {
    let b64 = base64::engine::general_purpose::STANDARD.encode(png(8, 6));
    let up = serve(vec![http(
        "200 OK",
        "application/json",
        format!(
            r#"{{"created":1,"data":[{{"b64_json":"{b64}","revised_prompt":"一只更好的猫"}}]}}"#
        )
        .as_bytes(),
    )]);

    let (s, dir) = service();
    let ep = endpoint(&up);
    let t = s
        .create_thread(Kind::Image, Source::Cloud, "gpt-image-1", Some("tok"))
        .unwrap();
    let reply = s
        .generate_image(
            "r1",
            &t.id,
            ImageRequest {
                prompt: "画只猫".into(),
                size: Some("1024x1024".into()),
                n: 1,
            },
            &ep,
        )
        .await
        .unwrap();
    assert!(reply.error.is_none(), "{:?}", reply.error);
    assert_eq!(reply.images.len(), 1);
    let img = &reply.images[0];
    assert_eq!(img.mime, "image/png");
    assert_eq!((img.width, img.height), (Some(8), Some(6)));
    assert_eq!(img.size.as_deref(), Some("1024x1024"));
    assert_eq!(reply.content, "一只更好的猫");
    let (path, mime) = s.image_path(&img.id).unwrap().unwrap();
    assert!(path.starts_with(dir.path()));
    assert!(path.is_file());
    assert_eq!(mime, "image/png");
    assert_eq!(std::fs::read(&path).unwrap(), png(8, 6));

    let list = s.threads(Some(Kind::Image)).unwrap();
    assert_eq!(list[0].cover_image_id.as_deref(), Some(img.id.as_str()));
    assert_eq!(list[0].thread.title, "画只猫");

    let reqs = up.requests.lock().unwrap();
    assert!(reqs[0].starts_with("POST /v1/images/generations "));
    let body: serde_json::Value =
        serde_json::from_str(reqs[0].split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(body["model"], "gpt-image-1");
    assert_eq!(body["prompt"], "画只猫");
    assert_eq!(body["size"], "1024x1024");
    assert_eq!(body["n"], 1);
    drop(reqs);

    // 删会话：文件跟着走。
    s.delete_thread(&t.id).unwrap();
    assert!(!path.exists());
}

#[tokio::test]
async fn image_urls_are_downloaded_immediately() {
    // 先起图片服务器拿到地址，再起返回 url 的接口服务器。
    let pic = serve(vec![http("200 OK", "image/png", &png(16, 9))]);
    let api = serve(vec![http(
        "200 OK",
        "application/json",
        format!(r#"{{"data":[{{"url":"{}/out.png"}}]}}"#, pic.base_url).as_bytes(),
    )]);
    let (s, _dir) = service();
    let t = s
        .create_thread(Kind::Image, Source::Cloud, "seedream-5", None)
        .unwrap();
    let reply = s
        .generate_image(
            "r",
            &t.id,
            ImageRequest {
                prompt: "海".into(),
                size: None,
                n: 1,
            },
            &endpoint(&api),
        )
        .await
        .unwrap();
    assert!(reply.error.is_none(), "{:?}", reply.error);
    assert_eq!(reply.images.len(), 1);
    assert_eq!(
        (reply.images[0].width, reply.images[0].height),
        (Some(16), Some(9))
    );
    assert!(reply.images[0].size.is_none(), "没发 size 就不记 size");
    let body: serde_json::Value = serde_json::from_str(
        api.requests.lock().unwrap()[0]
            .split("\r\n\r\n")
            .nth(1)
            .unwrap(),
    )
    .unwrap();
    assert!(body.get("size").is_none(), "固定规格的模型不发 size");
    assert!(pic.requests.lock().unwrap()[0].starts_with("GET /out.png "));
}

#[tokio::test]
async fn upstream_rejections_become_a_failed_reply_in_plain_words() {
    let up = serve(vec![http(
        "402 Payment Required",
        "application/json",
        br#"{"error":{"message":"insufficient credits"}}"#,
    )]);
    let (s, _dir) = service();
    let t = s
        .create_thread(Kind::Image, Source::Cloud, "m", None)
        .unwrap();
    let reply = s
        .generate_image(
            "r",
            &t.id,
            ImageRequest {
                prompt: "x".into(),
                size: None,
                n: 1,
            },
            &endpoint(&up),
        )
        .await
        .unwrap();
    assert!(reply.images.is_empty());
    assert!(reply.error.as_deref().unwrap().contains("积分不足"));
    assert_eq!(s.thread(&t.id).unwrap().messages.len(), 2);
}
