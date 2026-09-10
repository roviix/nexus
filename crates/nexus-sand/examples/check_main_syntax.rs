//! 对本机 Cursor 的 agent-host `main.js` 在内存里打补丁，然后 `node --check`。
//!
//!   cargo run -p nexus-sand --example check_main_syntax [box_relay|direct|off]
//!
//! 盘上已经装着补丁也没关系：Literal / legacy 语义会原地迁到所选形态，检查的是最终文本。
use nexus_sand::engine;
use nexus_sand::model::{GrokBotAuthMode, InstallOptions};
use nexus_sand::rules;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let mode = match std::env::args().nth(1).as_deref() {
        Some("direct") => GrokBotAuthMode::Direct,
        Some("off") => GrokBotAuthMode::Off,
        _ => GrokBotAuthMode::BoxRelay,
    };
    let main = PathBuf::from(
        "/Applications/Cursor.app/Contents/Resources/app/extensions/cursor-agent-host/dist/main.js",
    );
    let base = std::fs::read_to_string(&main).expect("read main.js");
    eprintln!(
        "on disk: grokbot={:?}",
        rules::installed_grokbot_auth(&base)
    );

    let opts = InstallOptions {
        grokbot_auth: mode,
        ..InstallOptions::default()
    };
    let catalog = rules::catalog(&opts).expect("catalog");
    let (patched, rep) = engine::apply(&base, &catalog);
    eprintln!(
        "apply({mode:?}): grokbot hits={} migrated={} → on disk would be {:?}",
        rep.hits.grokbot_stream_auth,
        rep.migrated.grokbot_stream_auth,
        rules::installed_grokbot_auth(&patched)
    );

    let tmp = std::env::temp_dir().join(format!("nexus-sand-main-{}.js", mode.as_str()));
    std::fs::write(&tmp, &patched).expect("write temp");
    let out = Command::new("node")
        .args(["--check", tmp.to_str().unwrap()])
        .output()
        .expect("node");
    if out.status.success() {
        eprintln!("node --check: OK");
    } else {
        eprintln!(
            "node --check: FAIL\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        std::process::exit(1);
    }
}
