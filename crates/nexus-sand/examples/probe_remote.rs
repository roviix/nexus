//! 手动跑一次远程探针：从远程实地打 `127.0.0.1:<remote_port>`，报出断在哪一跳。
//!
//!   cargo run -p nexus-sand --example probe_remote -- devbox-01 41777            # 网关模式：拿到任何 HTTP 应答就算通
//!   cargo run -p nexus-sand --example probe_remote -- devbox-01 41777 api2.cursor.sh   # 代理模式：CONNECT → TLS → GET
//!
//! 配合 `relay` 示例（先把中继起起来），就是桌面端「验一遍」按钮背后的那次调用。

use nexus_sand::{ProbeTarget, RemoteSand};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (host, port, target) = match args.as_slice() {
        [h, p] => (
            h.clone(),
            p.parse::<u16>().expect("端口"),
            ProbeTarget::Local,
        ),
        [h, p, t] => (
            h.clone(),
            p.parse::<u16>().expect("端口"),
            ProbeTarget::Proxy { host: t.clone() },
        ),
        _ => {
            eprintln!("用法：probe_remote <ssh 主机> <远程端口> [CONNECT 目标主机]");
            std::process::exit(2);
        }
    };
    let dir = std::env::temp_dir().join("nexus-probe-remote");
    let sand = RemoteSand::with_system_ssh(&dir, None);
    match sand.probe(&host, port, &target) {
        Ok(r) => println!("{}", serde_json::to_string_pretty(&r).unwrap()),
        Err(e) => {
            eprintln!("探针没跑起来：{}", e.message);
            if let Some(h) = e.hint {
                eprintln!("  {h}");
            }
            std::process::exit(1);
        }
    }
}
