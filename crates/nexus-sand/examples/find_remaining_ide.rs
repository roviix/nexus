//! 找出打完补丁后仍残留的 client-type `ide` 匹配（dry-run 里 remaining_ide 的来源）。
//!
//! ```bash
//! cargo run -p nexus-sand --example find_remaining_ide -- /Applications/Cursor.app
//! ```

use nexus_sand::engine;
use nexus_sand::model::InstallOptions;
use nexus_sand::rules::{self, PatchRule, RuleId, RuleKind};
use nexus_sand::SandLayout;
use regex::Regex;
use std::collections::HashMap;
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "/Applications/Cursor.app".into()),
    );
    let layout = SandLayout::from_app(&root).expect("解析 bundle 失败");
    let catalog = rules::catalog(&InstallOptions::default()).expect("规则表");
    let mut contents: HashMap<PathBuf, String> = HashMap::new();
    for t in &layout.targets {
        let bytes = std::fs::read(t).expect("读文件");
        if let Ok(text) = String::from_utf8(bytes) {
            contents.insert(t.clone(), text);
        }
    }

    let client_rules: Vec<&PatchRule> = catalog
        .iter()
        .filter(|r| r.id == RuleId::ClientType)
        .collect();

    for t in &layout.targets {
        let Some(src) = contents.get(t) else { continue };
        let (patched, _) = engine::apply(src, &catalog);
        let ins = engine::inspect(&patched, &catalog);
        if ins.remaining_ide == 0 {
            continue;
        }
        println!(
            "\n{}  remaining_ide={}",
            layout.relative(t),
            ins.remaining_ide
        );
        for (i, rule) in client_rules.iter().enumerate() {
            let RuleKind::Regex {
                apply,
                skip_if_followed_by,
                ..
            } = &rule.kind
            else {
                continue;
            };
            for m in apply.find_iter(&patched) {
                if engine_followed_by(&patched, m.end(), skip_if_followed_by.as_ref()) {
                    continue;
                }
                let start = m.start().saturating_sub(40);
                let end = (m.end() + 80).min(patched.len());
                println!(
                    "  rule[{i}] @ {}..{}: …{}…",
                    m.start(),
                    m.end(),
                    &patched[start..end]
                );
            }
        }
    }
}

fn engine_followed_by(content: &str, at: usize, guard: Option<&Regex>) -> bool {
    match guard {
        Some(g) => g.find_at(content, at).is_some_and(|m| m.start() == at),
        None => false,
    }
}
