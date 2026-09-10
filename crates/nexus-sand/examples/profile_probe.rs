//! 量一个 bundle 上每类规则实际能命中几次——用来定 [`nexus_sand::rules::LayoutProfile`] 的期望值。
//!
//! 期望命中数是硬校验的依据，不能拍脑袋写：desktop 的 23 / 2 / 1 是按本机全套 11 个文件算的，
//! 而 remote server 只有其中 7 个（少了 `out/main.js`、两个 `workbench.*.main.js`、
//! `extensionHostWorkerMain.js`），数目必然不同。做法是把一份**原版**远程 bundle 按
//! `<root>/resources/app/<rel>` 的形状摆好，跑这个例子，把打印出来的数字连同这里的输出一起
//! 写进 `rules.rs` 的 profile 表。
//!
//! ```bash
//! cargo run -p nexus-sand --example profile_probe -- /tmp/server-staging
//! cargo run -p nexus-sand --example profile_probe -- /tmp/server-staging http://127.0.0.1:8790
//! ```
//!
//! 第二个参数给推理端点，用来验证 remote 那两条改道规则在真 bundle 上确实各命中一次。

use nexus_sand::engine;
use nexus_sand::model::InstallOptions;
use nexus_sand::rules::{self, RuleId};
use nexus_sand::SandLayout;
use std::collections::HashMap;
use std::path::PathBuf;

fn main() {
    let root = match std::env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("用法：cargo run -p nexus-sand --example profile_probe -- <bundle 根目录>");
            std::process::exit(2);
        }
    };

    let layout = SandLayout::from_app(&root).expect("解析 bundle 失败");
    println!("root    : {}", layout.app_root.display());
    println!("version : {}", layout.version);
    println!("targets : {}", layout.targets.len());
    for t in &layout.targets {
        println!("  - {}", layout.relative(t));
    }

    let mut contents: HashMap<PathBuf, String> = HashMap::new();
    for t in &layout.targets {
        let bytes = std::fs::read(t).expect("读目标文件失败");
        match String::from_utf8(bytes) {
            Ok(text) => {
                contents.insert(t.clone(), text);
            }
            Err(_) => println!("  ! 非 UTF-8，跳过：{}", layout.relative(t)),
        }
    }

    println!("\n== 预检锚点 ==");
    for a in rules::preflight_anchors() {
        let n = a.count(contents.values().map(String::as_str));
        let flag = if n == a.expect { "ok" } else { "≠" };
        println!("  {flag} {:<28} {n}（desktop 期望 {}）", a.name, a.expect);
    }

    let options = InstallOptions {
        inference_endpoint: std::env::args().nth(2),
        ..InstallOptions::default()
    };
    if let Some(e) = &options.inference_endpoint {
        println!("\n推理端点改道到 {e}");
    }
    let catalog = rules::catalog(&options).expect("构建规则表失败");
    let mut after = nexus_sand::MarkerCounts::default();
    let (mut remaining_ide, mut foreign, mut legacy) = (0u32, 0u32, 0u32);
    let mut changed = 0u32;
    let mut per_file: Vec<(String, u32)> = Vec::new();
    for t in &layout.targets {
        let Some(c) = contents.get(t) else { continue };
        let (next, _) = engine::apply(c, &catalog);
        let ins = engine::inspect(&next, &catalog);
        let total = ins.markers.total();
        after = after.plus(&ins.markers);
        remaining_ide += ins.remaining_ide;
        foreign += ins.foreign;
        legacy += ins.legacy;
        if next != *c {
            changed += 1;
            per_file.push((layout.relative(t), total));
        }
    }

    println!("\n== 每类规则打完后的命中数 ==");
    for id in RuleId::ALL {
        let got = id.get(&after);
        let want = id.expected();
        let note = match want {
            None => "不校验".to_string(),
            Some(w) if w == got => "= desktop".to_string(),
            Some(w) => format!("≠ desktop（desktop {w}）"),
        };
        println!("  {:<26} {got:>3}   {note}", id.name());
    }

    println!("\n== 打完后的残留（都必须为 0，否则 is_complete 不通过）==");
    println!("  remaining_ide {remaining_ide}   foreign {foreign}   legacy {legacy}");

    println!("\n== 会改动的文件（{changed} 个）==");
    for (rel, n) in &per_file {
        println!("  {n:>3} 处  {rel}");
    }
}
