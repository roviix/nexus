//! 对真远程主机跑一次 [`nexus_sand::Tunnel`]，把每一次状态变化打出来。
//!
//! ```bash
//! cargo run -p nexus-sand --example tunnel_probe -- devbox-01 18688
//! ```
//!
//! 存在的理由：「隧道明明通了、界面却一直显示连接中」这种问题，只看界面分不清是
//! 状态判定错了还是界面没刷新。这里绕开界面，直接盯 `watch` 里的相位。

use nexus_sand::{Tunnel, TunnelSpec};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(host) = args.first().cloned() else {
        eprintln!("用法：tunnel_probe <ssh 主机> [端口，默认 18688]");
        std::process::exit(2);
    };
    let port: u16 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(18688);
    let seconds: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(45);

    let tunnel = Arc::new(Tunnel::new());
    let mut rx = tunnel.subscribe();
    let t0 = Instant::now();
    let started = tunnel
        .start(TunnelSpec::same_port(host.clone(), port))
        .await
        .expect("起隧道");
    println!(
        "[{:>6.2}s] start → {:?}",
        t0.elapsed().as_secs_f32(),
        started.phase
    );

    let watch = async {
        while rx.changed().await.is_ok() {
            let s = rx.borrow().clone();
            println!(
                "[{:>6.2}s] {:?} reconnects={} last_error={:?}",
                t0.elapsed().as_secs_f32(),
                s.phase,
                s.reconnects,
                s.last_error
            );
        }
    };
    let _ = tokio::time::timeout(Duration::from_secs(seconds), watch).await;

    println!("最终：{:?}", tunnel.status());
    tunnel.stop().await;
}
