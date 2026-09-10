//! 从命令行给本机 Cursor 装 / 卸 Sand 补丁——**只为取证**，日常请用桌面应用的 Sand 页。
//!
//! 存在的理由只有一个：带 `SAND_INFERENCE_ENDPOINT` 的安装没法靠打包好的应用做。GUI 应用
//! 不继承 shell 的环境变量，而重打一个包只为改一个排障开关不划算。
//!
//! 它和应用走的是同一个 [`SandService`]、同一个数据目录、同一套备份，所以装完之后应用的
//! Sand 页能看见这次安装、也能用它的「还原」把 Cursor 恢复原样。
//!
//! ```bash
//! # 装：把推理改道到本机 passthrough，好录官方客户端真实发出的请求
//! NEXUS_SAND_INSTALL=1 cargo run -p nexus-sand --example install_local -- \
//!   install http://127.0.0.1:8799
//!
//! # 卸：恢复原版
//! NEXUS_SAND_INSTALL=1 cargo run -p nexus-sand --example install_local -- uninstall
//! ```
//!
//! **会退出正在运行的 Cursor**（补丁要改它的文件）。开着的 Nexus 桌面应用建议先关掉：
//! 两个进程同时写 `nexus.db` 虽然 WAL 扛得住，但没必要给自己找麻烦。

use nexus_cursor::Cursor;
use nexus_sand::{with_inference_endpoint, InstallOptions, ModeGate, SandProgress, SandService};
use nexus_store::Db;
use std::sync::Arc;

fn data_dir() -> std::path::PathBuf {
    if let Ok(dir) = std::env::var("NEXUS_DATA_DIR") {
        return dir.into();
    }
    let home = std::env::var("HOME").expect("读不到 HOME");
    if cfg!(target_os = "macos") {
        std::path::Path::new(&home).join("Library/Application Support/com.roviix.nexus")
    } else {
        std::path::Path::new(&home).join(".local/share/com.roviix.nexus")
    }
}

fn main() {
    if std::env::var("NEXUS_SAND_INSTALL").ok().as_deref() != Some("1") {
        eprintln!(
            "这会退出正在运行的 Cursor 并改它 bundle 里的文件。确认的话设 NEXUS_SAND_INSTALL=1 再跑。"
        );
        std::process::exit(2);
    }

    let action = std::env::args().nth(1).unwrap_or_else(|| "install".into());
    let endpoint = std::env::args().nth(2);

    let dir = data_dir();
    let db = Arc::new(Db::open(dir.join("nexus.db")).expect("打不开 nexus.db"));
    let cursor = Cursor::detect().expect("找不到本机 Cursor");
    let service = SandService::new(db, &dir, cursor.paths.clone(), Arc::new(cursor.control()));

    let progress = |p: SandProgress| eprintln!("  [{:?}] {}", p.step, p.detail);

    let outcome = match action.as_str() {
        "install" => {
            // 与桌面 Sand 页默认一致：Ask / Debug / Multitask 也放行，避免只装 Agent 后别的模式硬失败。
            let options = InstallOptions {
                mode_gate: ModeGate::All,
                ..InstallOptions::default()
            };
            let options =
                with_inference_endpoint(options, endpoint.as_deref()).expect("推理端点不合法");
            eprintln!(
                "安装：Direct 引擎 · 自摘要 {} · 改道 {}",
                if options.self_summary { "开" } else { "关" },
                options.inference_endpoint.as_deref().unwrap_or("无")
            );
            service.install(options, &progress)
        }
        "uninstall" => {
            eprintln!("卸载，恢复原版");
            service.uninstall(true, &progress)
        }
        other => {
            eprintln!("认不出的动作 {other}；只有 install / uninstall。");
            std::process::exit(2);
        }
    };

    match outcome {
        Ok(o) => {
            eprintln!(
                "完成：wrote={} files={} backup={:?} relaunched={}",
                o.wrote, o.files_written, o.backup_id, o.cursor_relaunched
            );
        }
        Err(e) => {
            eprintln!("失败：{} ({:?})", e.message, e.code);
            if let Some(hint) = e.hint {
                eprintln!("提示：{hint}");
            }
            std::process::exit(1);
        }
    }
}
