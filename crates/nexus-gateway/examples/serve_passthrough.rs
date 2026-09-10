//! 起一个真的透传服务，给 `cursor-agent -e ... --agent-endpoint ...` 联调用——这是
//! `passthrough` 模块唯一没法用单元测试覆盖的一层（真 TLS 握手、真上游、真 cursor-agent 二进制）。
//!
//! 用 Cursor 里**正登着的号**：token 从它自己的登录态库读，设备身份钉成真机
//! `telemetry.machineId`（和 IDE 是同一台电脑，不会多出一台）。**不刷 refresh_token**——
//! 道理和 `examples/probe.rs` 一样。
//!
//! ```bash
//! NEXUS_GATEWAY_PROBE=1 cargo run -p nexus-gateway --example serve_passthrough   # 127.0.0.1:8788
//!
//! # 另一个终端：
//! cursor-agent -e http://127.0.0.1:8788 --agent-endpoint http://127.0.0.1:8788 \
//!   --model auto --print "PONG"
//! ```

use nexus_gateway::lane::{CursorLoginSource, RelayLane, Roster, Source};
use nexus_gateway::passthrough::{serve, PassthroughContext};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("nexus_gateway=debug")),
        )
        .init();
    if std::env::var("NEXUS_GATEWAY_PROBE").ok().as_deref() != Some("1") {
        eprintln!(
            "这会把 cursor-agent 的全部流量导到本地、再用 Cursor 里正登着的号转发出去。\
             确认的话设 NEXUS_GATEWAY_PROBE=1 再跑。"
        );
        std::process::exit(2);
    }

    let port: u16 = std::env::var("GATEWAY_PASSTHROUGH_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8788);
    let client_type = std::env::var("GATEWAY_CLIENT_TYPE").unwrap_or_else(|_| "cli".into());

    let cursor = Arc::new(nexus_cursor::Cursor::detect().expect("找不到本机 Cursor"));
    let sources: Vec<Arc<dyn Source>> = vec![Arc::new(CursorLoginSource::new(cursor))];
    // 探针不走桌面端的名单：此刻登着的号直接进队。
    let everyone: Vec<String> = sources
        .iter()
        .flat_map(|s| s.candidates())
        .map(|c| c.label)
        .collect();
    let lane = Arc::new(RelayLane::new(
        sources,
        Arc::new(Roster::in_memory(&everyone)),
    ));
    let snap = lane.snapshot();
    eprintln!("候选号 {} 个：", snap.candidates.len());
    for c in &snap.candidates {
        eprintln!(
            "  {} [{}]{}",
            c.label,
            c.source,
            if c.pinned { " 真机码" } else { "" }
        );
    }
    if snap.candidates.is_empty() {
        eprintln!(
            "没有候选号：Cursor 没登录，或 access token 已过期（在 Cursor 里用一下让它自己刷新）。"
        );
        std::process::exit(3);
    }

    let ctx = Arc::new(PassthroughContext::new(lane, client_type.clone()));
    let listener = nexus_gateway::server::bind(format!("127.0.0.1:{port}").parse().unwrap())
        .await
        .expect("端口被占了");
    let addr = listener.local_addr().unwrap();
    eprintln!("nexus-gateway 透传监听 http://{addr} · client-type {client_type}");
    eprintln!("  cursor-agent -e http://{addr} --agent-endpoint http://{addr} --model auto --print \"PONG\"");
    eprintln!("Ctrl-C 退出");

    serve(listener, ctx, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .expect("服务退出异常");
}
