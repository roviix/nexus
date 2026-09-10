//! 对着一个正在跑的网关走一遍「试一下」（`playground::run`），逐帧打印。
//!
//! ```bash
//! GATEWAY_URL=http://127.0.0.1:8787 GATEWAY_KEY=probe-key \
//!   cargo run -p nexus-gateway --example try_playground -- auto "用一句话介绍你自己"
//! ```

use nexus_gateway::playground::{self, TryEvent};
use std::io::Write;

#[tokio::main]
async fn main() {
    let url = std::env::var("GATEWAY_URL").unwrap_or_else(|_| "http://127.0.0.1:8787".into());
    let key = std::env::var("GATEWAY_KEY").unwrap_or_default();
    let mut args = std::env::args().skip(1);
    let model = args.next().unwrap_or_else(|| "auto".into());
    let prompt = args.next().unwrap_or_else(|| "用一句话介绍你自己。".into());

    let started = std::time::Instant::now();
    let res = playground::run(&url, &key, &model, &prompt, |ev| match ev {
        TryEvent::Routed { model } => eprintln!("[routed] {model}"),
        TryEvent::Thinking { text } => eprint!("\x1b[2m{text}\x1b[0m"),
        TryEvent::Delta { text } => {
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        TryEvent::Done { finish, usage } => eprintln!(
            "\n[done] finish={finish:?} usage={usage:?} in {:.1}s",
            started.elapsed().as_secs_f32()
        ),
        TryEvent::Usage { usage } => eprintln!("\n[usage] {usage:?}"),
        TryEvent::Error { message } => eprintln!("\n[error] {message}"),
    })
    .await;
    if let Err(e) = res {
        eprintln!("[failed] {e}");
        std::process::exit(1);
    }
}
