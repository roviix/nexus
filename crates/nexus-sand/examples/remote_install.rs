//! 对真远程主机跑一次 [`nexus_sand::RemoteSand::install`] / `uninstall`——和桌面端「安装到远程」
//! 按钮走的是同一段代码，只是少了界面。**会改远程文件、会重启远程的 cursor-server。**
//!
//! ```bash
//! NEXUS_REMOTE_WRITE=1 cargo run -p nexus-sand --example remote_install -- devbox-01 http://127.0.0.1:8688
//! NEXUS_REMOTE_WRITE=1 cargo run -p nexus-sand --example remote_install -- devbox-01 --uninstall
//! NEXUS_REMOTE_WRITE=1 cargo run -p nexus-sand --example remote_install -- devbox-01 --restart
//! ```
//!
//! 存在的理由：界面那条链路每次失败都要重新构建、重新点，太慢；这里能在几秒内把「拉 → 打 → 推 →
//! 重启」对真机跑通，再回去点界面时剩下的只有网关和隧道。

use nexus_sand::model::InstallOptions;
use nexus_sand::RemoteSand;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(host) = args.first().cloned() else {
        eprintln!("用法：remote_install <ssh 主机> [<推理端点> | --uninstall]");
        std::process::exit(2);
    };
    if std::env::var("NEXUS_REMOTE_WRITE").ok().as_deref() != Some("1") {
        eprintln!("这会改远程 ~/.cursor-server 里的文件并重启远程 cursor-server。确认的话设 NEXUS_REMOTE_WRITE=1。");
        std::process::exit(2);
    }
    let uninstall = args.iter().any(|a| a == "--uninstall");
    let restart_only = args.iter().any(|a| a == "--restart");
    let endpoint = args.get(1).filter(|a| !a.starts_with("--")).cloned();

    let local_commit = nexus_cursor::Cursor::detect()
        .ok()
        .and_then(|c| nexus_sand::SandLayout::resolve(&c.paths).ok())
        .and_then(|l| {
            let raw = std::fs::read_to_string(&l.product_json).ok()?;
            let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
            v.get("commit")?.as_str().map(str::to_string)
        });
    let data_dir = std::env::temp_dir().join("nexus-sand-remote-install");
    let sand = RemoteSand::with_system_ssh(&data_dir, local_commit);

    if restart_only {
        match sand.restart_server(&host) {
            Ok(true) => {
                println!("远程 cursor-server 已杀掉，Cursor 重连时会用盘上的 bundle 起来。")
            }
            Ok(false) => println!("没有杀到 server（可能本来就没在跑，或 pkill 没匹配到）。"),
            Err(e) => {
                eprintln!("失败：[{:?}] {}", e.code, e.message);
                std::process::exit(1);
            }
        }
        return;
    }

    let progress = |p: nexus_sand::SandProgress| eprintln!("  [{:?}] {}", p.step, p.detail);
    let result = if uninstall {
        sand.uninstall(&host, &progress)
    } else {
        let options = InstallOptions {
            inference_endpoint: endpoint.clone(),
            relaunch: false,
            ..InstallOptions::default()
        };
        eprintln!(
            "install → {host}  端点：{}",
            endpoint.as_deref().unwrap_or("（不改道）")
        );
        sand.install(&host, options, &progress)
    };

    match result {
        Ok(o) => {
            println!(
                "\n{:?} 完成：wrote={} files={} backup={:?} server_restarted={}",
                o.operation, o.wrote, o.files_written, o.backup_id, o.server_restarted
            );
            println!(
                "状态：complete={} endpoint={:?} patched={:?}",
                o.status.complete, o.status.inference_endpoint, o.status.patched_files
            );
        }
        Err(e) => {
            eprintln!("\n失败：[{:?}] {}", e.code, e.message);
            if let Some(h) = e.hint {
                eprintln!("提示：{h}");
            }
            std::process::exit(1);
        }
    }
}
