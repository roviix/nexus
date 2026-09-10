//! Nexus 桌面端的 Tauri 壳。
//!
//! 这一层只做三件事：装配状态、注册命令、把长任务的进度变成事件。业务逻辑一行都不在
//! 这里 —— 全在 `crates/nexus-*` 里，那样它们才能被单独测到。

mod commands;
mod state;

use nexus_store::activity;
use state::AppState;
use tauri::Manager;

/// 活动日志保留条数。启动时裁一次，日志不会无限长。
const ACTIVITY_KEEP: u32 = 2000;

/// 把 URL 路径里的 `%XX` 还原。图片 id 是 uuid，本不需要；留着是因为前端用
/// `encodeURIComponent` 拼地址，形态上就该按编码过的来读。
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &bytes[i + 1..i + 3];
            if hex.iter().all(u8::is_ascii_hexdigit) {
                if let Ok(v) = u8::from_str_radix(std::str::from_utf8(hex).unwrap_or("zz"), 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// panic 的最后一道防线：**把现场写下来，然后确保进程真的死掉**。
///
/// 两个理由，都来自 2026-09-04 一次「应用卡死、200% CPU」的现场：
///
/// 1. **不写下来就查不出来。** release 是 `strip = true` + `panic = "abort"`，Finder 启动的应用 stderr
///    进 `/dev/null`，`tauri_plugin_log` 只桥接 `log` 宏——panic 消息哪儿都不落。那次卡死采样只看到
///    两条线程停在同一个匿名地址，什么都对不出来。这里把消息 + 线程名 + 回溯追加到日志目录的
///    `panic.log`（和 `nexus.log` 同一目录），下次一眼能看到是哪一行。
/// 2. **`abort()` 在这个进程里不一定能杀死自己。** 那次的形态——两条不相干的线程停在同一条指令上
///    烧 CPU、进程不退——正是 `abort()` 发出的 SIGABRT 被进程内的 WebKit 异常处理接管、线程回到
///    abort 之后的 `brk` 陷阱上反复重执行。`std::process::exit` 走 `_exit` 系统调用，不经信号，吞不掉。
///    只在 `panic = "abort"` 的构建里这么做：debug 是 unwind，tokio 任务里的 panic 本来就会被兜住，
///    不该因为这个 hook 变成整个应用退出。
fn install_panic_hook(log_dir: std::path::PathBuf) {
    use std::io::Write as _;
    let _ = std::fs::create_dir_all(&log_dir);
    let path = log_dir.join("panic.log");
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 先让默认 hook 打到 stderr（`cargo tauri dev` 时终端里能看见）。
        previous(info);
        let thread = std::thread::current();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let backtrace = std::backtrace::Backtrace::force_capture();
        let record = format!(
            "==== panic @{stamp} thread={:?} ====\n{info}\n{backtrace}\n\n",
            thread.name().unwrap_or("<unnamed>")
        );
        log::error!("{}", record.lines().nth(1).unwrap_or("panic"));
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = f.write_all(record.as_bytes());
            let _ = f.flush();
        }
        if cfg!(panic = "abort") {
            std::process::exit(101);
        }
    }));
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let mut builder = tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(
            tauri_plugin_log::Builder::new()
                .level(log::LevelFilter::Info)
                // 遥测默认关闭；日志只落本地，用户可手动导出（§9）。
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir {
                        file_name: Some("nexus".into()),
                    }),
                ])
                .build(),
        );

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        builder = builder
            .plugin(tauri_plugin_updater::Builder::new().build())
            .plugin(tauri_plugin_process::init());
    }

    builder
        .setup(|app| {
            // 越早越好，但要等到知道日志目录在哪。setup 之前的 panic 只剩系统崩溃报告可查。
            if let Ok(log_dir) = app.path().app_log_dir() {
                install_panic_hook(log_dir);
            }
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;
            // 备份与导出住在用户目录下的 `~/.roviix`，不在应用数据目录里：卸载应用、
            // 换机器搬家时它得还在（ARCHITECTURE §1.1）。
            let roviix_dir = app.path().home_dir()?.join(".roviix");

            let state = AppState::build(&data_dir, &roviix_dir).map_err(|err| {
                // 走到这里说明连本地库都开不起来，应用没法工作。把原因说清楚再退出，
                // 比给一个空白窗口强。
                tracing::error!(%err, "初始化失败");
                std::io::Error::other(err.to_string())
            })?;

            let _ = activity::prune(&state.db, ACTIVITY_KEEP);
            activity::info(&state.db, "app", None, "应用已启动");

            // 网关默认关着；只有用户在设置里开了「随应用启动」才自动起。
            let gateway = state.gateway.clone();
            let autostart = gateway.settings().autostart;
            // 隧道不是「可选的自动启动」，是**必须跟着应用起**：远程 bundle 里的端点改道是写进
            // 文件的，隧道一断远程就只剩 ECONNREFUSED，而且那一侧看不出原因。见
            // `RemoteSandHub::restore_tunnels`。
            let sand_remote = state.sand_remote.clone();
            let db = state.db.clone();
            app.manage(state);
            if autostart {
                tauri::async_runtime::spawn(async move {
                    if let Err(err) = gateway.start().await {
                        tracing::error!(%err, "网关自动启动失败");
                    }
                });
            }
            tauri::async_runtime::spawn(async move {
                for host in sand_remote.restore_tunnels().await {
                    activity::info(&db, "sand", None, format!("已恢复到 {host} 的隧道"));
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // app
            commands::app::app_status,
            commands::app::app_activity,
            commands::app::app_update_settings,
            // 切号
            commands::switcher::switcher_overview,
            commands::switcher::switcher_list,
            commands::switcher::switcher_capture_current,
            commands::switcher::switcher_set_note,
            commands::switcher::switcher_remove,
            commands::switcher::switcher_switch_to,
            commands::switcher::switcher_backups,
            commands::switcher::switcher_backup_now,
            commands::switcher::switcher_remove_backup,
            commands::switcher::switcher_restore_backup,
            commands::switcher::switcher_restore_machine,
            // 我的账号
            commands::accounts::accounts_list,
            commands::accounts::accounts_add,
            commands::accounts::accounts_parse_dump,
            commands::accounts::accounts_import_dump,
            commands::accounts::accounts_patch,
            commands::accounts::accounts_remove,
            commands::accounts::accounts_refresh_usage,
            commands::accounts::accounts_refresh_all,
            commands::accounts::accounts_start_oauth,
            commands::accounts::accounts_cancel_oauth,
            commands::accounts::accounts_list_sessions,
            commands::accounts::accounts_kick_sessions,
            commands::accounts::accounts_reveal_secret,
            commands::accounts::accounts_reveal_session,
            commands::accounts::accounts_set_secret,
            commands::accounts::accounts_add_to_switch_book,
            commands::accounts::accounts_export_dump,
            // 本地备份（~/.roviix）
            commands::backup::backup_list,
            commands::backup::backup_create,
            commands::backup::backup_restore,
            commands::backup::backup_remove,
            commands::backup::backup_reveal,
            // Sand 补丁
            commands::sand::sand_release,
            commands::sand::sand_status,
            commands::sand::sand_install,
            commands::sand::sand_uninstall,
            commands::sand::sand_backups,
            commands::sand::sand_remove_backup,
            commands::sand::sand_restore_backup,
            // Grok Bot 桥（Sand / 网关按需借额度；不是账号管理）
            commands::grokbot::grokbot_status,
            commands::grokbot::grokbot_launch,
            commands::grokbot::grokbot_identify,
            commands::grokbot::grokbot_import_active_account,
            commands::grokbot::grokbot_relay_refresh,
            commands::grokbot::grokbot_relay_clear,
            commands::grokbot::grokbot_mint_direct,
            commands::grokbot::grokbot_mint_for_account,
            commands::grokbot::grokbot_renew_direct,
            commands::grokbot::grokbot_clear_direct,
            commands::grokbot::grokbot_forget,
            // 远程 Sand
            commands::sand_remote::sand_remote_overview,
            commands::sand_remote::sand_remote_hosts,
            commands::sand_remote::sand_remote_add_host,
            commands::sand_remote::sand_remote_remove_host,
            commands::sand_remote::sand_remote_update_host,
            commands::sand_remote::sand_remote_install,
            commands::sand_remote::sand_remote_probe,
            commands::sand_remote::sand_remote_uninstall,
            commands::sand_remote::sand_remote_tunnel_start,
            commands::sand_remote::sand_remote_tunnel_stop,
            commands::sand_remote::sand_remote_tunnel_status,
            // 本地网关
            commands::gateway::gateway_status,
            commands::gateway::gateway_usage,
            commands::gateway::gateway_ide_usage,
            commands::gateway::gateway_set_intercept,
            commands::gateway::gateway_set_grokbot_stream,
            commands::gateway::gateway_models,
            commands::gateway::gateway_try,
            commands::gateway::gateway_start,
            commands::gateway::gateway_stop,
            commands::gateway::gateway_update_settings,
            commands::gateway::gateway_enroll,
            commands::gateway::gateway_unenroll,
            commands::gateway::gateway_set_current,
            commands::gateway::gateway_reset_lane,
            commands::gateway::gateway_channel_set_current,
            commands::gateway::gateway_channel_reset_lane,
            commands::gateway::gateway_media_jobs,
            commands::gateway::gateway_reveal_key,
            commands::gateway::gateway_rotate_key,
            // 订阅通道的账号（ChatGPT / Grok / Kiro）：号在各自的表，接力队在网关里按通道分
            commands::chatgpt::chatgpt_list,
            commands::chatgpt::chatgpt_models,
            commands::chatgpt::chatgpt_refresh_models,
            commands::chatgpt::chatgpt_login_start,
            commands::chatgpt::chatgpt_login_complete,
            commands::chatgpt::chatgpt_login_cancel,
            commands::chatgpt::chatgpt_import_codex_cli,
            commands::chatgpt::chatgpt_import_text,
            commands::chatgpt::chatgpt_remove,
            commands::chatgpt::chatgpt_set_enabled,
            commands::chatgpt::chatgpt_set_note,
            commands::chatgpt::chatgpt_refresh_usage,
            commands::grok::grok_list,
            commands::grok::grok_login_start,
            commands::grok::grok_login_cancel,
            commands::grok::grok_import_cli,
            commands::grok::grok_import_text,
            commands::grok::grok_add_api_key,
            commands::grok::grok_remove,
            commands::grok::grok_set_enabled,
            commands::grok::grok_set_note,
            commands::grok::grok_refresh_quota,
            commands::grok::grok_set_media_override,
            commands::grok::grok_models,
            commands::grok::grok_refresh_models,
            commands::kiro::kiro_list,
            commands::kiro::kiro_login_start,
            commands::kiro::kiro_login_cancel,
            commands::kiro::kiro_import_cli,
            commands::kiro::kiro_import_text,
            commands::kiro::kiro_remove,
            commands::kiro::kiro_set_enabled,
            commands::kiro::kiro_set_note,
            // 一键接入（改客户端配置文件）
            commands::connect::connect_inspect,
            commands::connect::connect_apply,
            commands::connect::connect_revert,
            // 权限预检
            commands::perms::perms_check,
            commands::perms::perms_request,
            commands::perms::perms_mark_preflight,
            commands::perms::perms_open_settings,
            // 游乐场
            commands::playground::playground_threads,
            commands::playground::playground_thread,
            commands::playground::playground_thread_create,
            commands::playground::playground_thread_rename,
            commands::playground::playground_thread_set_target,
            commands::playground::playground_thread_delete,
            commands::playground::playground_message_delete,
            commands::playground::playground_active,
            commands::playground::playground_chat_send,
            commands::playground::playground_stop,
            commands::playground::playground_image_generate,
            commands::playground::playground_video_generate,
            commands::playground::playground_image_reveal,
            commands::playground::playground_assets,
            commands::playground::playground_image_delete,
        ])
        // 游乐场生成的图片按 id 经这个协议给 WebView：`nexus-image://localhost/<id>`
        // （Windows 上是 `http://nexus-image.localhost/<id>`，前端用 `convertFileSrc(id, "nexus-image")`
        // 拼，两种形态路径都是 `/<id>`）。只认库里登记过的 id，不接受任何路径——前端从头到尾
        // 不知道文件在哪，也就没有「让 WebView 读任意文件」这条口子。
        //
        // 用 asynchronous 版本而不是 `register_uri_scheme_protocol`：WebKit 在**主线程**上回调
        // 协议处理器（`webView:startURLSchemeTask:`），同步版本就地跑闭包，等于让每张图的
        // 查库 + 整文件读盘都发生在 UI 线程上；碰上别的线程正握着库锁，界面会跟着一起等。
        // 这里只在主线程上收请求，活儿交给阻塞线程池，读完再 respond（ARCHITECTURE §3.3）。
        .register_asynchronous_uri_scheme_protocol("nexus-image", |ctx, request, responder| {
            let id = percent_decode(request.uri().path().trim_start_matches('/'));
            let app = ctx.app_handle().clone();
            tauri::async_runtime::spawn_blocking(move || {
                let response = match app
                    .state::<AppState>()
                    .playground
                    .image_path(&id)
                    .ok()
                    .flatten()
                    .and_then(|(path, mime)| std::fs::read(path).ok().map(|bytes| (bytes, mime)))
                {
                    Some((bytes, mime)) => tauri::http::Response::builder()
                        .status(200)
                        .header("content-type", mime)
                        // id 是 uuid、文件只写一次：可以放心地让 WebView 永久缓存。
                        .header("cache-control", "public, max-age=31536000, immutable")
                        .body(bytes)
                        .unwrap_or_else(|_| tauri::http::Response::new(Vec::new())),
                    None => tauri::http::Response::builder()
                        .status(404)
                        .header("content-type", "text/plain; charset=utf-8")
                        .body(b"no such image".to_vec())
                        .unwrap_or_else(|_| tauri::http::Response::new(Vec::new())),
                };
                responder.respond(response);
            });
        })
        .build(tauri::generate_context!())
        .expect("启动 Nexus 失败")
        .run(|app, event| {
            // 隧道是我们唯一会留在系统里的子进程；应用退出时显式收掉，别让一条 ssh 挂在那儿
            // 占着远程端口。`kill_on_drop` 是兜底，不是主路径。
            if let tauri::RunEvent::ExitRequested { .. } = event {
                if let Some(state) = app.try_state::<AppState>() {
                    let hub = state.sand_remote.clone();
                    tauri::async_runtime::block_on(hub.shutdown());
                }
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试二进制是 unwind，hook 里不会 exit，正好能验「现场写下来了」这一半。
    /// `exit(101)` 那一半只在 `panic = "abort"` 的构建里生效，靠 `cfg!` 分支保证。
    #[test]
    fn panic_hook_records_message_thread_and_backtrace() {
        let dir = tempfile::tempdir().unwrap();
        install_panic_hook(dir.path().to_path_buf());
        let joined = std::thread::Builder::new()
            .name("boom-thread".into())
            .spawn(|| panic!("deliberate: {}", 42))
            .unwrap()
            .join();
        assert!(joined.is_err(), "线程应该 panic");
        // 装回默认 hook，别影响同一进程里别的测试的输出。
        let _ = std::panic::take_hook();

        let log = std::fs::read_to_string(dir.path().join("panic.log")).unwrap();
        assert!(log.contains("==== panic @"), "{log}");
        assert!(log.contains("thread=\"boom-thread\""), "{log}");
        assert!(log.contains("deliberate: 42"), "{log}");
        assert!(log.contains("lib.rs"), "应带上 panic 位置：{log}");
    }
}
