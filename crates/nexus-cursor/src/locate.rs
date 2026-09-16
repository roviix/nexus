//! 找到这台机器上的 Cursor：用户目录、状态库、机器码文件、应用本体、版本。
//!
//! 全部路径集中在这一个结构里，别处不许再拼路径。理由很实际：测试要能把整套指到
//! 临时目录，用户探测失败时要能手工指定一个目录，两件事都只需要换一次 `CursorPaths`。

use nexus_core::{AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Cursor 在本机的一整套路径。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CursorPaths {
    /// `~/Library/Application Support/Cursor`（macOS）。下面几项都由它派生。
    pub user_dir: PathBuf,
    /// 登录态库。
    pub state_db: PathBuf,
    /// 机器码所在的 JSON。
    pub storage_json: PathBuf,
    /// `<user_dir>/machineid`，单独一个文件，内容是 UUID。
    pub machine_id_file: PathBuf,
    /// 应用本体（macOS 是 .app 目录）。找不到时为 `None`——不影响读写登录态，
    /// 只影响「启动 Cursor」。
    pub app: Option<PathBuf>,
}

impl CursorPaths {
    /// 按平台默认位置探测。`CURSOR_USER_DIR` 环境变量可覆盖（测试与排障用）。
    pub fn detect() -> Result<Self> {
        let user_dir = match std::env::var_os("CURSOR_USER_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => default_user_dir()?,
        };
        Ok(Self::from_user_dir(user_dir))
    }

    /// 用户手工指定目录时走这条。
    pub fn from_user_dir(user_dir: impl Into<PathBuf>) -> Self {
        let user_dir = user_dir.into();
        let state_db = match std::env::var_os("CURSOR_STATE_DB") {
            Some(p) => PathBuf::from(p),
            None => user_dir.join("User/globalStorage/state.vscdb"),
        };
        Self {
            storage_json: user_dir.join("User/globalStorage/storage.json"),
            machine_id_file: user_dir.join("machineid"),
            state_db,
            user_dir,
            app: find_app(),
        }
    }

    /// 用用户在设置里指定的安装目录覆盖自动探测的结果。
    ///
    /// 数据目录和安装目录是两件事：Windows 上前者恒在 `%APPDATA%\Cursor`，后者却可能
    /// 被装到任意盘符。探不到安装目录只影响「启动 Cursor」和 Sand，不影响读写登录态，
    /// 所以这里是可选覆盖而不是必填项。空字符串 = 回到自动探测。
    ///
    /// **填错了不算填了。** 以前这里照单全收，于是随手写个盘符也能让界面上那枚
    /// 「未检测到」熄灭 —— 用户以为指对了，启动 Cursor 和 Sand 到用的时候才失败，
    /// 而且失败的地方离设置页很远。现在只认里面真有 Cursor 的目录，
    /// 填错就退回自动探测的结果（通常是 `None`），界面照旧说没找到。
    pub fn with_app_override(mut self, app: Option<&str>) -> Self {
        let explicit = app.map(str::trim).filter(|s| !s.is_empty());
        if let Some(dir) = explicit {
            let dir = PathBuf::from(dir);
            if is_cursor_install(&dir) {
                self.app = Some(dir);
            }
        }
        self
    }

    /// 状态库在不在。不在 = Cursor 没装或从没登录过。
    pub fn state_db_exists(&self) -> bool {
        self.state_db.is_file()
    }

    pub fn require_state_db(&self) -> Result<&Path> {
        if self.state_db_exists() {
            Ok(&self.state_db)
        } else {
            Err(AppError::new(
                ErrorCode::CursorNotFound,
                format!("没找到 Cursor 状态库：{}", self.state_db.display()),
            )
            .with_hint("确认 Cursor 已安装并至少登录过一次，或在设置里手动指定 Cursor 数据目录。"))
        }
    }

    /// 从 `product.json` 读版本号。读不到返回 `None`——版本只用于「未在此版本验证过」
    /// 这类提示，缺了不该挡住任何操作。
    pub fn version(&self) -> Option<String> {
        let product = self.product_json()?;
        let raw = std::fs::read_to_string(product).ok()?;
        let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
        json.get("version")?.as_str().map(str::to_string)
    }

    /// bundle 里的 `product.json`。读版本用它；权限预检也拿它当「能不能改 Cursor 本体」的探针——
    /// 每个版本都有、又不是被 Sand 改写的那几个文件。
    pub fn product_json(&self) -> Option<PathBuf> {
        product_json_in(self.app.as_ref()?)
    }
}

/// `product.json` 在这个安装目录里的位置。macOS 是 bundle 布局，Windows / Linux 是安装目录布局。
fn product_json_in(app: &Path) -> Option<PathBuf> {
    [
        app.join("Contents/Resources/app/product.json"), // macOS bundle
        app.join("resources/app/product.json"),          // Windows / Linux 安装目录
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// 这个目录里是不是一个 Cursor 安装。
///
/// 两个标志物任一命中就算：`product.json`（版本号和 Sand 补丁读它）或平台的可执行本体
/// （Windows 的 `Cursor.exe`、macOS bundle 里的 `Contents/MacOS`）。两个都找是因为
/// 它们各自对应一件事 —— 打补丁要前者，启动要后者，便携版少一个也还有用。
///
/// **不能只看目录存不存在。** 卸载后 `%LOCALAPPDATA%\Programs\cursor` 常留一个空壳，
/// 那时「探到了」比「没探到」更坏：界面说一切正常，启动 Cursor 却静静地失败。
pub fn is_cursor_install(dir: &Path) -> bool {
    product_json_in(dir).is_some()
        || dir.join("Cursor.exe").is_file()
        || dir.join("Contents/MacOS").is_dir()
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| {
            AppError::new(ErrorCode::CursorNotFound, "读不到当前用户的主目录。")
                .with_hint("在设置里手动指定 Cursor 数据目录。")
        })
}

fn default_user_dir() -> Result<PathBuf> {
    if cfg!(target_os = "macos") {
        Ok(home_dir()?.join("Library/Application Support/Cursor"))
    } else if cfg!(target_os = "windows") {
        let appdata = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or(home_dir()?.join("AppData/Roaming"));
        Ok(appdata.join("Cursor"))
    } else {
        Ok(home_dir()?.join(".config/Cursor"))
    }
}

/// 找应用本体。顺序是「用户安装 → 系统安装」。
///
/// Windows 上这个顺序和 macOS 相反，是照着两边的实际习惯来的：Cursor 的 Windows 安装器
/// 默认装到 `%LOCALAPPDATA%\Programs\cursor`（per-user，不需要管理员），机器级安装是
/// 少数派；而 macOS 上 `/Applications` 才是常态。先命中常见的那个，少走一次 stat。
fn find_app() -> Option<PathBuf> {
    // 排障用的后门，照单全收（只要路径在）：设置页那一格要挡住填错的，这里不挡 ——
    // 会去设这个环境变量的人正是想强行指一个不合常规布局的目录。
    if let Some(explicit) = std::env::var_os("CURSOR_APP_PATH") {
        let p = PathBuf::from(explicit);
        return p.exists().then_some(p);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        candidates.push(PathBuf::from("/Applications/Cursor.app"));
        if let Some(h) = &home {
            candidates.push(h.join("Applications/Cursor.app"));
        }
    } else if cfg!(target_os = "windows") {
        // 目录名大小写在这里无所谓（NTFS 不区分），但盘符和 Program Files 的位置
        // 必须问环境变量：中文系统、非 C 盘安装、32/64 位两个 Program Files 都存在，
        // 写死 `C:\Program Files` 会漏掉一大片。
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            candidates.push(PathBuf::from(local).join("Programs/cursor"));
        }
        for key in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(dir) = std::env::var_os(key) {
                candidates.push(PathBuf::from(dir).join("cursor"));
            }
        }
        // Scoop / 便携版的常见落点。探不到也不报错，用户还能在设置里手填。
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            let profile = PathBuf::from(profile);
            candidates.push(profile.join("scoop/apps/cursor/current"));
            candidates.push(profile.join("AppData/Local/cursor"));
        }
    } else {
        candidates.push(PathBuf::from("/usr/share/cursor"));
        candidates.push(PathBuf::from("/opt/cursor"));
    }
    candidates.into_iter().find(|p| is_cursor_install(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_every_path_from_the_user_dir() {
        let p = CursorPaths::from_user_dir("/tmp/fake-cursor");
        assert!(p.state_db.ends_with("User/globalStorage/state.vscdb"));
        assert!(p.storage_json.ends_with("User/globalStorage/storage.json"));
        assert!(p.machine_id_file.ends_with("machineid"));
        assert!(p.state_db.starts_with("/tmp/fake-cursor"));
    }

    #[test]
    fn missing_state_db_reports_cursor_not_found_with_a_next_step() {
        let p = CursorPaths::from_user_dir("/tmp/definitely-not-here-9f3a");
        assert!(!p.state_db_exists());
        let err = p.require_state_db().unwrap_err();
        assert_eq!(err.code, ErrorCode::CursorNotFound);
        assert!(err.hint.unwrap().contains("手动指定"));
    }

    #[test]
    fn existing_state_db_is_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let p = CursorPaths::from_user_dir(dir.path());
        std::fs::create_dir_all(p.state_db.parent().unwrap()).unwrap();
        std::fs::write(&p.state_db, b"").unwrap();
        assert!(p.state_db_exists());
        assert!(p.require_state_db().is_ok());
    }

    #[test]
    fn version_is_none_when_product_json_is_absent() {
        let mut p = CursorPaths::from_user_dir("/tmp/fake-cursor");
        p.app = Some(PathBuf::from("/tmp/definitely-not-here-9f3a"));
        assert!(p.version().is_none());
    }

    /// 造一个 Windows 布局的安装目录：`Cursor.exe` + `resources/app/product.json`。
    fn windows_install(root: &Path, version: &str) -> PathBuf {
        let app = root.join("cursor");
        let res = app.join("resources/app");
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(
            res.join("product.json"),
            format!(r#"{{"version":"{version}"}}"#),
        )
        .unwrap();
        std::fs::write(app.join("Cursor.exe"), b"MZ").unwrap();
        app
    }

    #[test]
    fn an_explicit_app_override_wins_over_detection() {
        let dir = tempfile::tempdir().unwrap();
        let app = windows_install(dir.path(), "3.19.13");
        let p = CursorPaths::from_user_dir("/tmp/fake-cursor")
            .with_app_override(Some(app.to_str().unwrap()));
        assert_eq!(p.app.as_deref(), Some(app.as_path()));
    }

    /// Windows 实测：用户把装在别处的 Cursor 填进设置里，填错一级也照单全收，
    /// 界面上那枚「未检测到」就熄了 —— 直到启动 Cursor 或打 Sand 补丁时才失败。
    #[test]
    fn an_app_override_that_holds_no_cursor_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("not-cursor");
        std::fs::create_dir_all(&empty).unwrap();
        let detected = CursorPaths::from_user_dir("/tmp/fake-cursor").app;
        for bad in [empty.to_str().unwrap(), "/tmp/definitely-not-here-9f3a"] {
            let p = CursorPaths::from_user_dir("/tmp/fake-cursor").with_app_override(Some(bad));
            assert_eq!(p.app, detected, "目录里没有 Cursor，不该当成指对了：{bad}");
        }
    }

    #[test]
    fn an_install_is_recognised_by_product_json_or_the_executable() {
        let dir = tempfile::tempdir().unwrap();
        assert!(is_cursor_install(&windows_install(dir.path(), "3.19.13")));

        // 只有可执行本体的便携版也算：启动得了，只是打不了补丁。
        let portable = dir.path().join("portable");
        std::fs::create_dir_all(&portable).unwrap();
        std::fs::write(portable.join("Cursor.exe"), b"MZ").unwrap();
        assert!(is_cursor_install(&portable));

        // 卸载后留下的空壳不算 —— 这正是「只看目录在不在」会误判的那一种。
        let shell = dir.path().join("leftover");
        std::fs::create_dir_all(shell.join("resources")).unwrap();
        assert!(!is_cursor_install(&shell));
    }

    #[test]
    fn a_blank_app_override_falls_back_to_detection() {
        let detected = CursorPaths::from_user_dir("/tmp/fake-cursor").app;
        for blank in [None, Some(""), Some("   ")] {
            let p = CursorPaths::from_user_dir("/tmp/fake-cursor").with_app_override(blank);
            assert_eq!(p.app, detected, "空值不该把探测结果抹掉：{blank:?}");
        }
    }

    #[test]
    fn version_reads_from_a_windows_style_install() {
        // Windows / Linux 的安装目录里是 `resources/app`，没有 `Contents/`。
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("cursor");
        let res = app.join("resources/app");
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(res.join("product.json"), r#"{"version":"3.18.25"}"#).unwrap();
        let mut p = CursorPaths::from_user_dir(dir.path());
        p.app = Some(app);
        assert_eq!(p.version().as_deref(), Some("3.18.25"));
    }

    #[test]
    fn version_reads_from_a_macos_style_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Cursor.app");
        let res = app.join("Contents/Resources/app");
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(res.join("product.json"), r#"{"version":"3.18.9"}"#).unwrap();
        let mut p = CursorPaths::from_user_dir(dir.path());
        p.app = Some(app);
        assert_eq!(p.version().as_deref(), Some("3.18.9"));
    }
}
