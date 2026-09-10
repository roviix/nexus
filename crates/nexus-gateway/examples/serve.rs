//! 在本机起一个真网关：对外是 OpenAI / Anthropic 兼容口，号走 `RelayLane` 额度接力——
//! 缺省只有 Cursor 里正登着的号（钉真机 machineId）；设 `GATEWAY_STORED=1` 再把 desktop
//! 「我的账号」里的托管号接在后面（读真实的 nexus.db，凭证也在那个库里）。
//!
//! ```bash
//! NEXUS_GATEWAY_PROBE=1 cargo run -p nexus-gateway --example serve          # 127.0.0.1:8787
//! NEXUS_GATEWAY_PROBE=1 GATEWAY_PORT=9000 GATEWAY_KEY=abc GATEWAY_STORED=1 cargo run -p nexus-gateway --example serve
//!
//! curl -N http://127.0.0.1:8787/v1/chat/completions -H 'content-type: application/json' \
//!   -d '{"model":"auto","stream":true,"messages":[{"role":"user","content":"hi"}]}'
//! ```
//!
//! **不刷 Cursor 登录号的 refresh_token**（Cursor 会轮换它，从外面刷可能把 IDE 登出）；
//! 托管号的刷新由 nexus-accounts 负责并把轮换结果存回本地库。

use nexus_gateway::lane::Source;
use nexus_gateway::server::{bind, router, serve, Gateway};
use nexus_gateway::{
    CursorLoginSource, CursorUpstream, RelayLane, Roster, StoredAccountsSource, StreamConfig,
};
use std::sync::Arc;

#[tokio::main]
async fn main() {
    if std::env::var("NEXUS_GATEWAY_PROBE").ok().as_deref() != Some("1") {
        eprintln!("这会用你的 Cursor 账号对外提供推理。确认的话设 NEXUS_GATEWAY_PROBE=1 再跑。");
        std::process::exit(2);
    }

    let port: u16 = std::env::var("GATEWAY_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8787);
    let api_key = std::env::var("GATEWAY_KEY").ok().filter(|k| !k.is_empty());
    let mut cfg = StreamConfig::default();
    if let Ok(ct) = std::env::var("GATEWAY_CLIENT_TYPE") {
        cfg.client_type = ct;
    }
    cfg.force_model = std::env::var("GATEWAY_FORCE_MODEL")
        .ok()
        .filter(|m| !m.is_empty());

    let cursor = Arc::new(nexus_cursor::Cursor::detect().expect("找不到本机 Cursor"));
    let mut sources: Vec<Arc<dyn Source>> = vec![Arc::new(CursorLoginSource::new(cursor))];

    if std::env::var("GATEWAY_STORED").ok().as_deref() == Some("1") {
        use nexus_store::{Db, SecretStore, SqliteSecrets};
        let data_dir = dirs_data_dir();
        let db = Arc::new(Db::open(data_dir.join("nexus.db")).expect("打不开 nexus.db"));
        let secrets: Arc<dyn SecretStore> = Arc::new(SqliteSecrets::new(db.clone()));
        let accounts = Arc::new(nexus_accounts::AccountsService::new(db, secrets));
        sources.push(Arc::new(StoredAccountsSource::new(
            accounts,
            cfg.client_type.clone(),
        )));
    }

    // 探针不走桌面端的名单：此刻找到的号全部进队。
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
            "  {} [{}]{}{}",
            c.label,
            c.source,
            if c.pinned { " 真机码" } else { "" },
            c.percent_used
                .map(|p| format!(" 用量 {p:.0}%"))
                .unwrap_or_default()
        );
    }

    // 这个探针只探 Cursor 那条通道。
    let gw = Arc::new(Gateway {
        api_key: api_key.clone(),
        ..Gateway::single(lane.clone(), Arc::new(CursorUpstream::new(cfg.clone())))
    });

    let listener = bind(format!("127.0.0.1:{port}").parse().unwrap())
        .await
        .expect("端口被占了");
    let addr = listener.local_addr().unwrap();
    eprintln!(
        "nexus-gateway 监听 http://{addr} · client-type {} · key {}",
        cfg.client_type,
        api_key.as_deref().unwrap_or("(不校验)")
    );
    eprintln!("  OpenAI:     http://{addr}/v1/chat/completions");
    eprintln!("  Responses:  http://{addr}/v1/responses");
    eprintln!("  Anthropic:  http://{addr}/v1/messages");
    eprintln!("Ctrl-C 退出");

    serve(listener, router(gw), async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .expect("服务退出异常");
}

/// desktop 的数据目录（与 Tauri `app_data_dir` 一致）。只在 macOS 上写死了路径——这只是 example。
fn dirs_data_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").expect("HOME");
    std::path::PathBuf::from(home).join("Library/Application Support/com.roviix.nexus")
}
