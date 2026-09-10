//! 手动验隧道：起一条到某台远程的中继，把远程 `127.0.0.1:<remote_port>` 接回本机
//! `127.0.0.1:<local_port>`，然后一直挂着打印状态，Ctrl-C 退出。
//!
//!   cargo run -p nexus-sand --example relay -- devbox-01 41777 8000
//!
//! 另开一个终端在远程上验：`ssh devbox-01 curl -s http://127.0.0.1:41777/`——
//! 应该拿到本机 8000 上那个服务的应答。这就是桌面端「远程主机」卡片背后的那条链路，只是
//! 把两头都摆到了命令行上，排障时不用开界面。

use nexus_sand::{Tunnel, TunnelPhase, TunnelSpec};
use std::sync::Arc;
use std::time::Duration;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (host, remote_port, local_port) = match args.as_slice() {
        [h, r, l] => (
            h.clone(),
            r.parse::<u16>().expect("remote_port 要是端口号"),
            l.parse::<u16>().expect("local_port 要是端口号"),
        ),
        _ => {
            eprintln!("用法：relay <ssh 主机> <远程端口> <本机端口>");
            std::process::exit(2);
        }
    };
    let tunnel = Arc::new(Tunnel::new());
    let mut rx = tunnel.subscribe();
    tunnel
        .start(TunnelSpec {
            host,
            remote_port,
            local_port,
        })
        .await
        .expect("start");
    eprintln!("· 已提交，等中继就绪（走跳板的话十几秒）");

    let mut last: Option<String> = None;
    loop {
        let st = rx.borrow().clone();
        let line = format!(
            "{:?} · 重连 {} 次 · {} 条连接{}",
            st.phase,
            st.reconnects,
            st.streams,
            st.last_error
                .as_deref()
                .map(|e| format!(" · {e}"))
                .unwrap_or_default()
        );
        if last.as_deref() != Some(&line) {
            eprintln!("{line}");
            if st.phase == TunnelPhase::Connected {
                eprintln!("  远程上验：curl -s http://127.0.0.1:{remote_port}/");
            }
            last = Some(line);
        }
        tokio::select! {
            _ = rx.changed() => {}
            _ = tokio::time::sleep(Duration::from_secs(1)) => {}
            _ = tokio::signal::ctrl_c() => {
                eprintln!("· 收尾");
                tunnel.stop().await;
                return;
            }
        }
    }
}
