//! 对着真远程主机跑 [`nexus_sand::RemoteSand`] 的只读部分：发现 server、数 marker、看端点。
//!
//! 只读，不写任何东西。安装 / 卸载不放进例子里——那是会改远程文件的操作，只从界面走。
//!
//! ```bash
//! cargo run -p nexus-sand --example remote_probe -- devbox-01
//! ```

use nexus_sand::rules::{LayoutProfile, RuleId};
use nexus_sand::RemoteSand;

fn main() {
    let host = match std::env::args().nth(1) {
        Some(h) => h,
        None => {
            eprintln!("用法：cargo run -p nexus-sand --example remote_probe -- <ssh 主机>");
            std::process::exit(2);
        }
    };

    // 本机 Cursor 的 commit 用来在远程一堆 commit 里挑对的那份。
    let local_commit = nexus_cursor::Cursor::detect()
        .ok()
        .and_then(|c| nexus_sand::SandLayout::resolve(&c.paths).ok())
        .and_then(|l| read_commit(&l.product_json));
    println!(
        "本机 Cursor commit: {}",
        local_commit.as_deref().unwrap_or("(未知)")
    );

    let dir = std::env::temp_dir().join("nexus-sand-remote-probe");
    let sand = RemoteSand::with_system_ssh(&dir, local_commit);

    let status = match sand.status(&host) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("失败：[{:?}] {}", e.code, e.message);
            if let Some(h) = e.hint {
                eprintln!("提示：{h}");
            }
            std::process::exit(1);
        }
    };

    println!(
        "\n远程 {} 上的 server（{} 份）：",
        status.host,
        status.servers.len()
    );
    for s in &status.servers {
        let mark = if Some(&s.commit) == status.selected.as_ref().map(|x| &x.commit) {
            "→"
        } else {
            " "
        };
        println!(
            "  {mark} {}  {}",
            &s.commit[..12.min(s.commit.len())],
            s.version
        );
    }

    let Some(sel) = &status.selected else {
        println!("\n没有可用的 server。");
        return;
    };
    println!(
        "\n选中 {}  root={}",
        &sel.commit[..12.min(sel.commit.len())],
        sel.root
    );
    println!("  版本受支持   : {}", status.version_supported);
    println!("  与本机同 commit: {}", status.commit_matches_local);
    println!(
        "  推理端点     : {}",
        status.inference_endpoint.as_deref().unwrap_or("未改道")
    );
    println!("  完整         : {}", status.complete);

    println!("\n== marker（对照 Server profile 的期望）==");
    for id in RuleId::ALL {
        let got = id.get(&status.markers);
        match id.expected_for(LayoutProfile::Server) {
            Some(want) if want == got => println!("  ok  {:<26} {got}", id.name()),
            Some(want) => println!("  ≠   {:<26} {got}（需 {want}）", id.name()),
            None => println!("  -   {:<26} {got}（不校验）", id.name()),
        }
    }

    println!("\n== 已改动的文件 ==");
    for f in &status.patched_files {
        println!("  {f}");
    }
}

fn read_commit(product_json: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(product_json).ok()?;
    let v: serde_json::Value = serde_json::from_str(&raw).ok()?;
    v.get("commit")?.as_str().map(str::to_string)
}
