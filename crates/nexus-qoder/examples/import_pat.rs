//! 把一把 Qoder PAT 导入本机 Nexus 库。凭证只从环境变量读，不进参数、不进仓库。
//!
//! ```text
//! QODER_PAT=pt-… QODER_BACKEND=global cargo run -p nexus-qoder --example import_pat
//! ```
//!
//! `QODER_BACKEND` 缺省是 `global`。国内版传 `cn`。

use nexus_qoder::{QoderBackend, QoderService};
use nexus_store::{Db, SqliteSecrets};
use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let pat = std::env::var("QODER_PAT").unwrap_or_else(|_| {
        eprintln!("需要环境变量 QODER_PAT。");
        std::process::exit(2);
    });
    let backend =
        QoderBackend::parse(&std::env::var("QODER_BACKEND").unwrap_or_else(|_| "global".into()));
    let db_path = std::env::var_os("NEXUS_DB")
        .map(PathBuf::from)
        .unwrap_or_else(default_db);
    let db = Arc::new(Db::open(&db_path).unwrap_or_else(|err| {
        eprintln!("打不开 {}：{err}", db_path.display());
        std::process::exit(1);
    }));
    let secrets = Arc::new(SqliteSecrets::new(db.clone()));
    let service = QoderService::new(db, secrets);
    match service.import_text(&pat, backend, Some("本机导入")).await {
        Ok(report) => {
            for account in &report.accounts {
                println!(
                    "{}  {}  {}",
                    if report.created > 0 && report.updated == 0 {
                        "已加入"
                    } else {
                        "已更新"
                    },
                    account.label(),
                    account.plan_type
                );
            }
            for why in &report.skipped {
                eprintln!("没导进来：{why}");
            }
        }
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    }
}

fn default_db() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join("Library/Application Support/com.roviix.nexus/nexus.db")
}
