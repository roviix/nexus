//! 真机联调：钥匙串 → descriptor → pod exec 读 sbi → 续期 → 写凭证文件。
//!
//!   cargo run -p nexus-grokbot --example live_mint [data_dir]
use nexus_grokbot::GrokBotService;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("nexus-grokbot-live"));
    std::fs::create_dir_all(&dir).unwrap();
    let svc = GrokBotService::new(&dir);
    eprintln!("status(before): {:#?}", svc.status());
    match svc.refresh_relay() {
        Ok(d) => eprintln!("relay: {} fp={:?}", d.base_url, d.account_fingerprint),
        Err(e) => eprintln!("relay FAIL: {e}"),
    }
    let cred = nexus_grokbot::run_sync(async move { svc.mint_direct().await });
    match cred {
        Ok(c) => eprintln!(
            "mint OK: email={:?} exp={:?} sbi={}… file={}",
            c.account_email,
            c.expires_at_ms,
            &c.renewal_credential.as_deref().unwrap_or("")[..8],
            dir.join(nexus_grokbot::STREAM_CREDENTIAL_FILENAME)
                .display()
        ),
        Err(e) => {
            eprintln!("mint FAIL: {e}");
            std::process::exit(1);
        }
    }
}
