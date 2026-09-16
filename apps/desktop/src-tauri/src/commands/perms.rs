//! 权限预检：把「中途才发现没权限」提前到用户自己选的时刻。
//!
//! 我们要动的东西有四类，各需要一种权限；其中两种在 macOS 上是系统弹窗（TCC），只在**第一次
//! 真去做**时弹——也就是 Sand 装到一半、冷切退到一半的时候。这一页把那一下提前：
//!
//! | id                 | 要做什么                         | 平台     | 怎么申请                                   |
//! |--------------------|----------------------------------|----------|--------------------------------------------|
//! | `cursor_app`       | 改 Cursor 安装（Sand 补丁）      | 全部     | macOS：以写模式打开 bundle 里的 product.json，触发「App 管理」弹窗；其它平台：同样的探针，直接得出能不能写 |
//! | `cursor_automation`| 优雅退出 / 重启 Cursor（冷切、Sand）| macOS   | 向正在跑的 Cursor 发一句无害的 AppleScript，触发「自动化」弹窗；Cursor 没在跑时申请不了 |
//! | `cursor_data`      | 写 Cursor 登录态（切号）         | 全部     | 探针：state.vscdb 能不能以写模式打开；没有系统弹窗 |
//! | `client_configs`   | 写 `~/.claude` / `~/.codex`（一键接入） | 全部 | 探针：目录能不能建文件；没有系统弹窗 |
//!
//! 探针都**不改任何内容**：以写模式打开再关掉、建一个空文件再删掉。TCC 的结果系统不给查询接口，
//! 所以两种系统弹窗类的结论**记在设置表里**（`perms.<id>`），下次直接报；用户去系统设置里改了，
//! 点「重新申请」会得到新结论。

use crate::state::AppState;
use nexus_core::Result;
use nexus_cursor::CursorControl;
use nexus_store::settings;
use serde::{Deserialize, Serialize};
use std::path::Path;
use tauri::{AppHandle, State};

const PREFLIGHT_DONE: &str = "perms.preflight_done";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermStatus {
    /// 探过了，能做。
    Ok,
    /// 探过了，被拒 —— 要去系统设置里开。
    Denied,
    /// 系统弹窗类，还没申请过；或此刻申请不了（Cursor 没在跑）。
    Unknown,
    /// 这个平台 / 这台机器上不适用（没装 Cursor、不是 macOS）。
    NotApplicable,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermItem {
    pub id: &'static str,
    pub title: &'static str,
    /// 给谁用的：切号 / Sand / 一键接入。
    pub used_by: &'static str,
    pub status: PermStatus,
    /// 一句补充：被拒时怎么办、为什么申请不了、探的是哪个路径。
    pub detail: Option<String>,
    /// 现在点「申请」有没有意义（会弹系统窗 / 会重新探）。
    pub can_request: bool,
    /// 系统设置里对应的那一页；只有 macOS 的两种 TCC 有。
    pub settings_url: Option<&'static str>,
    /// 这一项是「必须」还是「建议」。缺自动化权限只是退出 Cursor 要多等一会儿，不算硬伤。
    pub required: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PermReport {
    pub platform: &'static str,
    /// 首次启动的「准备工作」走完了没有（不管结果）。
    pub preflight_done: bool,
    pub items: Vec<PermItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Remembered {
    status: PermStatus,
    at: String,
}

fn remember(state: &AppState, id: &str, status: PermStatus) {
    let _ = settings::set(
        &state.db,
        &format!("perms.{id}"),
        &Remembered {
            status,
            at: nexus_core::now_iso(),
        },
    );
}

fn recall(state: &AppState, id: &str) -> Option<Remembered> {
    settings::get_or::<Option<Remembered>>(&state.db, &format!("perms.{id}"), None)
}

const PLATFORM: &str = if cfg!(target_os = "macos") {
    "macos"
} else if cfg!(target_os = "windows") {
    "windows"
} else {
    "linux"
};

const URL_APP_BUNDLES: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AppBundles";
const URL_AUTOMATION: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation";

/// 以写模式打开一个已有文件再关掉。不改一个字节；被拒就是被拒。
fn open_for_write(path: &Path) -> std::io::Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map(|_| ())
}

/// 目录里能不能建文件：建一个空探针再删掉。目录不存在算「能」——我们会建它。
fn dir_writable(dir: &Path) -> std::io::Result<()> {
    if !dir.exists() {
        // 目录不在，看它的上一级（多半是用户目录）能不能建东西。
        return match dir.parent() {
            Some(p) if p.exists() => dir_writable(p),
            _ => Ok(()),
        };
    }
    let probe = dir.join(format!(".nexus-probe-{}", std::process::id()));
    std::fs::File::create(&probe)?;
    let _ = std::fs::remove_file(&probe);
    Ok(())
}

fn status_of(r: std::io::Result<()>) -> (PermStatus, Option<String>) {
    match r {
        Ok(()) => (PermStatus::Ok, None),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => (PermStatus::Denied, None),
        Err(e) => (PermStatus::Unknown, Some(e.to_string())),
    }
}

/// 等系统弹窗被回答的耐心。弹窗是异步的：第一次 `open()` 先回 EPERM，窗随后才出现，
/// 用户点「允许」之后再试才会通。
const APP_MANAGEMENT_PATIENCE: std::time::Duration = std::time::Duration::from_secs(25);

/// macOS「App 管理」的探针。
///
/// 2026-09-07 真机：第一次探到的 EPERM 被当场记成 `denied`，用户随后在弹窗里点了允许也没人再探，
/// 重启后界面一直显示「被拒」——「一直申请、一直没权限」的一半原因就在这里（另一半是没签名的
/// 包算不出 designated requirement，系统存不住授权，见 scripts/macos-signing-identity.sh）。
/// 现在拿到 EPERM 后每秒再试一次，最多等 25 秒：用户一点允许就通；到点还不通，才算被拒。
fn probe_app_management(product: &Path) -> (PermStatus, Option<String>) {
    let (mut status, mut detail) = status_of(open_for_write(product));
    if status != PermStatus::Denied {
        return (status, detail);
    }
    let deadline = std::time::Instant::now() + APP_MANAGEMENT_PATIENCE;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_secs(1));
        let (s, d) = status_of(open_for_write(product));
        if s == PermStatus::Ok {
            status = s;
            detail = d;
            break;
        }
    }
    if status == PermStatus::Denied {
        detail = Some(
            "系统没有放行：在系统设置 → 隐私与安全性 → App 管理 里允许 Nexus。那里找不到 Nexus、\
             或允许了仍然这样，多半是这份包没签名（授权存不住）——换 scripts/build-dmg.sh 打的签名包。"
                .into(),
        );
    }
    (status, detail)
}

// ── 四项 ────────────────────────────────────────────────────────────────────

fn cursor_app(state: &AppState, probe: bool) -> PermItem {
    let paths = &state.switcher.cursor().paths;
    let Some(product) = paths.product_json() else {
        return PermItem {
            id: "cursor_app",
            title: "修改 Cursor 安装",
            used_by: "Sand / CRSR 补丁",
            status: PermStatus::NotApplicable,
            detail: Some("没找到 Cursor 的安装目录。".into()),
            can_request: false,
            settings_url: None,
            required: false,
        };
    };
    let (status, detail) = if cfg!(target_os = "macos") {
        // macOS：探针会弹「App 管理」系统窗，只在用户点了申请时探；平时报上次的结论。
        if probe {
            let (s, d) = probe_app_management(&product);
            if matches!(s, PermStatus::Ok | PermStatus::Denied) {
                remember(state, "cursor_app", s);
            }
            (s, d)
        } else {
            // 换签名身份后，库里那条「已允许」会撒谎（这次重装后卸载失败就是这个）。
            // 先做一次不弹窗的即时探针：能写就当真允许，EPERM 就当没放行。
            let (live, _) = status_of(open_for_write(&product));
            if live == PermStatus::Ok {
                remember(state, "cursor_app", PermStatus::Ok);
                (PermStatus::Ok, Some(product.display().to_string()))
            } else if live == PermStatus::Denied {
                (
                    PermStatus::Denied,
                    Some(
                        "系统没有放行：在系统设置 → 隐私与安全性 → App 管理 里允许 Nexus。".into(),
                    ),
                )
            } else {
                match recall(state, "cursor_app") {
                    Some(r) => (r.status, None),
                    None => (PermStatus::Unknown, Some("还没申请过。".into())),
                }
            }
        }
    } else {
        status_of(open_for_write(&product))
    };
    let detail = detail.or_else(|| match status {
        PermStatus::Denied if cfg!(target_os = "macos") => {
            Some("在系统设置 → 隐私与安全性 → App 管理 里允许 Nexus。".into())
        }
        PermStatus::Denied if cfg!(target_os = "windows") => Some(
            "这份 Cursor 装在需要管理员权限的目录（如 Program Files）。用管理员身份重开 Nexus，或把 Cursor 改装到用户目录。".into(),
        ),
        PermStatus::Denied => Some("当前用户改不动这个 Cursor 安装，检查它的属主与权限。".into()),
        _ => Some(product.display().to_string()),
    });
    PermItem {
        id: "cursor_app",
        title: "修改 Cursor 安装",
        used_by: "Sand / CRSR 补丁",
        status,
        detail,
        can_request: true,
        settings_url: if cfg!(target_os = "macos") {
            Some(URL_APP_BUNDLES)
        } else {
            None
        },
        required: false,
    }
}

/// `osascript` 对 Cursor 说一句无害的话。只在它跑着的时候：AppleScript 对没在跑的应用
/// 会把它启动起来，那不是「申请权限」该有的副作用。
fn cursor_automation(state: &AppState, probe: bool) -> PermItem {
    let base = PermItem {
        id: "cursor_automation",
        title: "退出与重启 Cursor",
        used_by: "冷切换、Sand 补丁",
        status: PermStatus::NotApplicable,
        detail: None,
        can_request: false,
        settings_url: Some(URL_AUTOMATION),
        required: false,
    };
    if !cfg!(target_os = "macos") {
        return PermItem {
            detail: Some("这个系统上退出程序不需要额外授权。".into()),
            settings_url: None,
            ..base
        };
    }
    let running = state
        .switcher
        .cursor()
        .control()
        .is_running()
        .unwrap_or(false);
    if probe && running {
        let out = std::process::Command::new("osascript")
            .args(["-e", r#"tell application "Cursor" to get name"#])
            .output();
        let (status, detail) = match out {
            Ok(o) if o.status.success() => (PermStatus::Ok, None),
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stderr);
                if err.contains("-1743") || err.contains("Not authorized") {
                    (
                        PermStatus::Denied,
                        Some(
                            "在系统设置 → 隐私与安全性 → 自动化 里，允许 Nexus 控制 Cursor。"
                                .into(),
                        ),
                    )
                } else {
                    (PermStatus::Unknown, Some(err.trim().to_string()))
                }
            }
            Err(e) => (PermStatus::Unknown, Some(e.to_string())),
        };
        if matches!(status, PermStatus::Ok | PermStatus::Denied) {
            remember(state, "cursor_automation", status);
        }
        return PermItem {
            status,
            detail,
            can_request: true,
            ..base
        };
    }
    match recall(state, "cursor_automation") {
        Some(r) => PermItem {
            status: r.status,
            detail: match r.status {
                PermStatus::Denied => {
                    Some("在系统设置 → 隐私与安全性 → 自动化 里，允许 Nexus 控制 Cursor。".into())
                }
                _ => None,
            },
            can_request: running,
            ..base
        },
        None => PermItem {
            status: PermStatus::Unknown,
            detail: Some(if running {
                "还没申请过。".into()
            } else {
                "Cursor 没在运行，打开它之后才能申请。没有这项权限也能用：退出 Cursor 会等超时后强制结束。".into()
            }),
            can_request: running,
            ..base
        },
    }
}

fn cursor_data(state: &AppState) -> PermItem {
    let paths = &state.switcher.cursor().paths;
    let (status, detail) = if paths.state_db_exists() {
        status_of(open_for_write(&paths.state_db))
    } else {
        (
            PermStatus::NotApplicable,
            Some("Cursor 还没有登录态库（没登录过）。".into()),
        )
    };
    PermItem {
        id: "cursor_data",
        title: "写入 Cursor 登录态",
        used_by: "切号",
        status,
        detail: detail.or_else(|| Some(paths.state_db.display().to_string())),
        can_request: true,
        settings_url: None,
        required: true,
    }
}

fn client_configs(state: &AppState) -> PermItem {
    let dirs = [
        state.home_dir.join(".claude"),
        state.home_dir.join(".codex"),
    ];
    let mut worst = (PermStatus::Ok, None);
    for d in &dirs {
        let (s, detail) = status_of(dir_writable(d));
        if s != PermStatus::Ok {
            worst = (
                s,
                Some(format!(
                    "{}：{}",
                    d.display(),
                    detail.unwrap_or_else(|| "没有写权限".into())
                )),
            );
            break;
        }
    }
    PermItem {
        id: "client_configs",
        title: "写入客户端配置",
        used_by: "一键接入（Claude Code / Codex）",
        status: worst.0,
        detail: worst
            .1
            .or_else(|| Some(format!("{} · {}", dirs[0].display(), dirs[1].display()))),
        can_request: true,
        settings_url: None,
        required: false,
    }
}

fn report(state: &AppState, probe: bool) -> PermReport {
    PermReport {
        platform: PLATFORM,
        preflight_done: settings::get_or(&state.db, PREFLIGHT_DONE, false),
        items: vec![
            cursor_data(state),
            client_configs(state),
            cursor_app(state, probe),
            cursor_automation(state, probe),
        ],
    }
}

/// 现状。**不弹任何系统窗**：系统弹窗类只报上次记下的结论。
#[tauri::command(async)]
pub fn perms_check(state: State<'_, AppState>) -> Result<PermReport> {
    Ok(report(&state, false))
}

/// 申请：把会弹窗的探针真跑一遍（用户点了才跑）。`id` 为空 = 全部。
#[tauri::command(async)]
pub fn perms_request(state: State<'_, AppState>, id: Option<String>) -> Result<PermReport> {
    let mut r = report(&state, true);
    if let Some(id) = id.as_deref().filter(|s| !s.is_empty()) {
        // 只申请一项时，其余照「不弹窗」的口径报，免得点 A 弹出 B 的窗。
        let quiet = report(&state, false);
        r.items = quiet
            .items
            .into_iter()
            .map(|q| {
                if q.id == id {
                    r.items.iter().find(|p| p.id == id).cloned().unwrap_or(q)
                } else {
                    q
                }
            })
            .collect();
    }
    Ok(r)
}

/// 首次启动的「准备工作」走完了（不管结果）：以后不再主动弹。
#[tauri::command(async)]
pub fn perms_mark_preflight(state: State<'_, AppState>) -> Result<()> {
    settings::set(&state.db, PREFLIGHT_DONE, &true)
}

/// 打开系统设置里对应的那一页（macOS）。由 Rust 侧开，前端没有 opener 能力。
#[tauri::command(async)]
pub fn perms_open_settings(app: AppHandle, id: String) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;
    let url = match id.as_str() {
        "cursor_app" => URL_APP_BUNDLES,
        "cursor_automation" => URL_AUTOMATION,
        _ => {
            return Err(nexus_core::AppError::invalid(
                "这一项没有对应的系统设置页。",
            ))
        }
    };
    app.opener()
        .open_url(url, None::<&str>)
        .map_err(|e| nexus_core::AppError::internal(format!("打不开系统设置：{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probes_report_permission_problems_without_touching_content() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("product.json");
        std::fs::write(&f, "{}").unwrap();
        assert_eq!(status_of(open_for_write(&f)).0, PermStatus::Ok);
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{}", "探针不能改内容");

        let missing = dir.path().join("nope.json");
        let (s, detail) = status_of(open_for_write(&missing));
        assert_eq!(s, PermStatus::Unknown, "文件不在不是权限问题");
        assert!(detail.is_some());

        // 目录探针：存在的目录建再删，不留东西；不存在的看上一级。
        assert_eq!(status_of(dir_writable(dir.path())).0, PermStatus::Ok);
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "只剩 product.json"
        );
        assert_eq!(
            status_of(dir_writable(&dir.path().join(".claude"))).0,
            PermStatus::Ok
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_directory_is_reported_as_denied() {
        use std::os::unix::fs::PermissionsExt;
        // root 无视权限位，CI 若以 root 跑就跳过这条。
        if running_as_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let ro = dir.path().join("ro");
        std::fs::create_dir(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(status_of(dir_writable(&ro)).0, PermStatus::Denied);
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// 粗略判断：能以写模式打开 /etc/hosts 的就是 root。
    #[cfg(unix)]
    fn running_as_root() -> bool {
        std::fs::OpenOptions::new()
            .write(true)
            .open("/etc/hosts")
            .is_ok()
    }
}
