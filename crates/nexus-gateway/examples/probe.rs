//! 对真 api2 打一句话，验证协议引擎的 HTTP/2 + Connect 胶水——这是唯一没法用单元测试
//! 覆盖的一层。
//!
//! 用 Cursor 里**正登着的号**：token 从它自己的登录态库读，设备身份钉成真机 `telemetry.machineId`
//! （和 IDE 是同一台电脑，不会多出一台）。**不刷 refresh_token**——Cursor 会轮换它，从外面刷一次
//! 可能把 IDE 手里那把作废、把用户登出；access token 过期就只报告，不动手。
//!
//! ```bash
//! NEXUS_GATEWAY_PROBE=1 cargo run -p nexus-gateway --example probe
//! NEXUS_GATEWAY_PROBE=1 PROBE_MODEL=claude-sonnet-5 PROBE_PROMPT='1+1=?' cargo run -p nexus-gateway --example probe
//! ```
//!
//! `PROBE_BASE_URL` 把这一发打到别处而不是真 api2——`http://` 的地址 reqwest 会走
//! HTTP/1.1，正好复刻 Cursor agent-host 的 `backendTransport`（它建 Connect 传输时写死
//! `httpVersion:"1.1"`）。拿它对着 `examples/serve_passthrough` 打，就能在不动 Cursor
//! 一个字节的前提下验证「Connect over h1 经本地终止型代理转发到 api2 还能不能出字」，
//! 这正是 remote SSH 那条链路上我们要替换掉 SSH 隧道的那一段。
//!
//! 两个排障开关：
//! - `PROBE_ACCESS_TOKEN`：不读 Cursor 的登录态，直接用这个 access token（设备身份按账号
//!   派生，和网关给池子号用的一样）。用来复现「网关里某个号出了什么错」。
//! - `PROBE_TOOL=1`：附一个最小的工具定义。「不带工具能出字、带工具就被拒」这类问题只有
//!   这样才分得出是编码问题还是上游问题。

use nexus_cursor::Cursor;
use nexus_gateway::{
    inference, ChatRequest, Delta, DeviceIdentity, Message, Role, Sampling, StreamConfig,
    ToolChoice, ToolDef,
};
use std::io::Write;

fn jwt_exp_secs(jwt: &str) -> Option<i64> {
    use base64::Engine;
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()?
        .get("exp")?
        .as_i64()
}

#[tokio::main]
async fn main() {
    if std::env::var("NEXUS_GATEWAY_PROBE").ok().as_deref() != Some("1") {
        eprintln!(
            "这会用 Cursor 里正登着的号对 api2 发一次推理。确认的话设 NEXUS_GATEWAY_PROBE=1 再跑。"
        );
        std::process::exit(2);
    }

    let (access, email, identity) = match std::env::var("PROBE_ACCESS_TOKEN") {
        Ok(token) if !token.trim().is_empty() => {
            let token = token.trim().to_string();
            let identity = DeviceIdentity::derived(&token);
            (token, "(PROBE_ACCESS_TOKEN)".to_string(), identity)
        }
        _ => {
            let cursor = Cursor::detect().expect("找不到本机 Cursor");
            let auth = cursor.state.read_auth().expect("读不到 Cursor 登录态");
            let access = auth
                .get("cursorAuth/accessToken")
                .filter(|t| !t.is_empty())
                .expect("Cursor 当前没登录（没有 accessToken）")
                .to_string();
            let email = auth.email().unwrap_or_else(|| "(unknown)".into());
            let machine = cursor.machine.read();
            let identity = if machine.machine_id.len() == 64 {
                DeviceIdentity::pinned(&access, machine.machine_id.clone())
            } else {
                eprintln!("读不到真机 machineId，退回按账号派生");
                DeviceIdentity::derived(&access)
            };
            (access, email, identity)
        }
    };

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match jwt_exp_secs(&access) {
        Some(exp) if exp <= now => {
            eprintln!(
                "accessToken 已过期（{} 秒前）。在 Cursor 里随便用一下让它自己刷新，再跑。",
                now - exp
            );
            std::process::exit(3);
        }
        Some(exp) => eprintln!("accessToken 还有 {} 分钟有效", (exp - now) / 60),
        None => eprintln!("accessToken 不像 JWT，照样试"),
    }

    let model = std::env::var("PROBE_MODEL").unwrap_or_else(|_| "auto".into());
    let prompt = std::env::var("PROBE_PROMPT").unwrap_or_else(|_| "用一句话打个招呼。".into());
    let mut cfg = StreamConfig::default();
    if let Ok(base) = std::env::var("PROBE_BASE_URL") {
        cfg.base_url = base;
    }
    if let Ok(ct) = std::env::var("PROBE_CLIENT_TYPE") {
        cfg.client_type = ct;
    }

    let tools = if std::env::var("PROBE_TOOL").ok().as_deref() == Some("1") {
        vec![ToolDef {
            name: "read_file".into(),
            description: "Read a file from disk.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string", "description": "absolute path" } },
                "required": ["path"]
            }),
            ..ToolDef::default()
        }]
    } else {
        Vec::new()
    };

    eprintln!(
        "账号 {email} ({}) · 电脑 {}… · client-type {} · 模型 {model} · 工具 {} · 上游 {}",
        identity.account,
        &identity.machine_id[..12],
        cfg.client_type,
        tools.len(),
        cfg.base_url
    );
    eprintln!("---");

    let request = ChatRequest {
        model,
        messages: vec![Message::text(Role::User, prompt)],
        tools,
        tool_choice: ToolChoice::Auto,
        sampling: Sampling {
            max_output_tokens: Some(200),
            ..Default::default()
        },
        conversation_id: None,
        ..Default::default()
    };

    let client = inference::http_client();
    let mut on_delta = |d: Delta| match d {
        Delta::Text(t) => {
            print!("{t}");
            let _ = std::io::stdout().flush();
        }
        Delta::Thinking(t) => eprint!("\x1b[2m{t}\x1b[0m"),
        // Cursor 后端不发这两种（只有 ChatGPT 透传会）。
        Delta::Raw { .. } | Delta::Headers(_) => {}
    };

    match inference::stream(&client, &cfg, &access, &identity, &request, &mut on_delta).await {
        Ok(c) => {
            println!();
            eprintln!("---");
            eprintln!(
                "finish={} routed={} usage(in={} out={} cache_r={} cache_w={} measured={}) ttft={:?}ms turn={}ms",
                c.finish_reason.as_str(),
                c.routed_model.as_deref().unwrap_or("?"),
                c.usage.input_tokens,
                c.usage.output_tokens,
                c.usage.cache_read_tokens,
                c.usage.cache_write_tokens,
                c.usage_measured,
                c.ttft_ms,
                c.turn_ms
            );
        }
        Err(e) => {
            println!();
            eprintln!("--- 失败 ---");
            eprintln!(
                "kind={} status={} cursor_code={:?}\n{}",
                e.kind.as_str(),
                e.status,
                e.cursor_code,
                e.message
            );
            std::process::exit(1);
        }
    }
}
