//! 实验：**不经 Grok Bot 客户端**，用「我的账号」里某个号直接换到 grokBotToken（无感切号的可行性）。
//!
//! 链路：账号库 refresh → session/access token → `SandBoxService/EnsureSandBox(wake)`（拿这个号自己的 pod）
//! → pod exec daemon 读 `SAND_INFERENCE_RENEWAL_CREDENTIAL` → `/sand-box/inference-credential` → grokBotToken。
//! 成功则把凭证写到 `--out` 目录（不覆盖应用数据目录里的那份），再用
//! `gateway/scripts/probe-grokbot-header-tolerance.mjs <out>/grokbot-stream-credential.json` 打一发 Stream 验证。
//!
//! ```bash
//! NEXUS_GATEWAY_PROBE=1 cargo run -p nexus-gateway --example grokbot_for_account -- <email> [--out DIR]
//! ```
//!
//! 副作用：EnsureSandBox 会给这个号建（或唤醒）一个 Box pod——Grok Bot 客户端登录时做的就是这件事。

use nexus_accounts::AccountsService;
use nexus_grokbot::credential::{self, StreamCredential};
use nexus_grokbot::pod::{self, ExecTarget};
use nexus_store::{Db, SecretStore, SqliteSecrets};
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    if std::env::var("NEXUS_GATEWAY_PROBE").ok().as_deref() != Some("1") {
        eprintln!(
            "这会读账号库里一个号的凭证并为它建 Box pod。确认的话设 NEXUS_GATEWAY_PROBE=1 再跑。"
        );
        std::process::exit(2);
    }
    let mut args = std::env::args().skip(1);
    let email = args
        .next()
        .expect("usage: grokbot_for_account <email> [--out DIR]");
    let mut out = std::env::temp_dir().join("nexus-grokbot-for-account");
    while let Some(a) = args.next() {
        if a == "--out" {
            out = PathBuf::from(args.next().expect("--out DIR"));
        }
    }
    let data_dir = PathBuf::from(std::env::var("HOME").unwrap())
        .join("Library/Application Support/com.roviix.nexus");
    let db = Arc::new(Db::open(data_dir.join("nexus.db")).expect("open nexus.db"));
    let secrets: Arc<dyn SecretStore> = Arc::new(SqliteSecrets::new(db.clone()));
    let accounts = AccountsService::new(db, secrets);
    let account = accounts
        .repo
        .by_email(&email)
        .expect("query")
        .unwrap_or_else(|| panic!("账号库里没有 {email}"));
    eprintln!(
        "① 账号 {} status={:?} has_refresh={}",
        account.email, account.status, account.has_refresh
    );

    let session = accounts
        .session(&account.id)
        .await
        .expect("换 session 失败");
    let access = session.access_token.expose().to_string();
    let sub = nexus_grokbot::secrets::jwt_subject(&access).unwrap_or_default();
    eprintln!(
        "② access token OK sub={sub} exp={:?}",
        nexus_grokbot::secrets::jwt_exp_ms(&access)
    );

    // machineId：这个号在 Nexus 里的派生机器码（和网关一致），不用 Grok Bot 的。
    let machine_id = nexus_gateway::DeviceIdentity::derived(&access).machine_id;
    let ensured = match pod::ensure_sandbox(&access, &machine_id).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("③ EnsureSandBox 失败：{e}");
            std::process::exit(1);
        }
    };
    eprintln!(
        "③ EnsureSandBox OK pod_id={} exec_url={} has_network_token={}",
        ensured.pod_id,
        ensured.exec_daemon_url,
        !ensured.network_token.is_empty()
    );
    let target = ExecTarget::from_ensure(&ensured).expect("exec target");
    let sbi = match pod::read_renewal_credential(&target).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("④ 读 sbi 失败：{e}");
            std::process::exit(1);
        }
    };
    eprintln!("④ sbi={}… ({} chars)", &sbi[..8], sbi.len());
    let renewed = credential::renew(&sbi).await.expect("续期失败");
    eprintln!(
        "⑤ grokBotToken OK exp={} sub={:?}",
        renewed.expires_at_ms,
        nexus_grokbot::secrets::jwt_subject(&renewed.grok_bot_token)
    );
    std::fs::create_dir_all(&out).unwrap();
    let cred = StreamCredential {
        grok_bot_token: renewed.grok_bot_token,
        machine_id,
        renewal_credential: Some(sbi),
        expires_at_ms: Some(renewed.expires_at_ms),
        client_version: credential::DEFAULT_CLIENT_VERSION.into(),
        namespace: credential::DEFAULT_NAMESPACE.into(),
        account_email: Some(account.email.clone()),
        account_slot: None,
        source: Some(credential::CredentialSource::Library),
        minted_at_ms: Some(credential::now_ms()),
        renewed_at_ms: None,
    };
    let path = cred.save(&out).unwrap();
    eprintln!(
        "⑥ 写到 {}\n   验证：cd gateway && node scripts/probe-grokbot-header-tolerance.mjs {}",
        path.display(),
        path.display()
    );
}
