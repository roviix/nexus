//! 找到要打补丁的那几个文件。
//!
//! 和 `nexus_cursor::CursorPaths` 的分工：那边知道「用户数据在哪」（state.vscdb、机器码），
//! 这边知道「应用代码在哪」（`Contents/Resources/app/` 下的 bundle）。补丁只碰后者。
//!
//! 移植自 `gateway/scripts/sand-stream-installer.py` 的 `layout_from_path` / `TARGET_SPECS`。

use crate::rules::LayoutProfile;
use nexus_core::{AppError, ErrorCode, Result};
use nexus_cursor::CursorPaths;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// 目标文件（相对 app 根）与它所属扩展名。扩展名非空的，其 `main.js` sha256 被内嵌在
/// `extensionHostProcess.js` 里，改动后必须同步（否则 Cursor 报「安装已损坏」）。
pub const TARGET_SPECS: &[(&str, Option<&str>)] = &[
    ("out/main.js", None),
    (
        "out/vs/workbench/api/worker/extensionHostWorkerMain.js",
        None,
    ),
    ("out/vs/workbench/api/node/extensionHostProcess.js", None),
    ("out/vs/workbench/workbench.glass.main.js", None),
    ("out/vs/workbench/workbench.desktop.main.js", None),
    (
        "extensions/cursor-always-local/dist/main.js",
        Some("cursor-always-local"),
    ),
    (
        "extensions/cursor-local-agent-runtime/dist/main.js",
        Some("cursor-local-agent-runtime"),
    ),
    (
        "extensions/cursor-agent-host/dist/main.js",
        Some("cursor-agent-host"),
    ),
    (
        "extensions/cursor-agent-exec/dist/main.js",
        Some("cursor-agent-exec"),
    ),
    // chunk 编号随 Cursor 版本变（3.18.25 是 61.js / 675.js，3.19.7 是 9909.js / 4883.js）；
    // 3.19.13 把 9909.js 那一整块内联进了 agent-host 的 main.js（1.8MB → 11.7MB），只剩这一个 chunk。
    ("extensions/cursor-agent-host/dist/4884.js", None),
];

/// 内嵌扩展 hash 所在的文件。
pub const EXT_HOST_REL: &str = "out/vs/workbench/api/node/extensionHostProcess.js";

/// 一台机器上 Cursor 的补丁目标全貌。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandLayout {
    /// `.app` 目录（macOS）或安装根目录（Windows）。备份键、进程识别都用它。
    pub install_root: PathBuf,
    /// `…/Resources/app`。所有目标都在它下面。
    pub app_root: PathBuf,
    pub product_json: PathBuf,
    /// 存在的目标文件（绝对路径，已 resolve）。不存在的目标直接跳过，不算错。
    pub targets: Vec<PathBuf>,
    /// `extensionHostProcess.js`；不存在时为 `None`（则不做扩展 hash 同步）。
    pub ext_host: Option<PathBuf>,
    pub version: String,
    /// 决定期望命中数用哪一套。**由调用方显式给**，不从目录形状猜：猜错的代价是把一个残缺的
    /// desktop 安装当成 server，硬校验随之放宽，正是最不该放宽的时候。
    pub profile: LayoutProfile,
}

impl SandLayout {
    /// 从 `CursorPaths.app` 解析。找不到 app 或 app 里没有可识别的目标 → `CursorNotFound`。
    pub fn resolve(paths: &CursorPaths) -> Result<Self> {
        let app = paths.app.as_ref().ok_or_else(|| {
            AppError::new(ErrorCode::CursorNotFound, "没找到 Cursor 应用本体。")
                .with_hint(missing_app_hint())
        })?;
        Self::from_app(app)
    }

    /// 给定应用本体路径（macOS 的 `Cursor.app`，或 Windows 的安装目录）。
    pub fn from_app(app: &Path) -> Result<Self> {
        Self::from_root(app, LayoutProfile::Desktop)
    }

    /// 同 [`Self::from_app`]，但由调用方指定安装形态。
    ///
    /// remote 那条路用的是 `LayoutProfile::Server` + 一个**暂存镜像**目录：把远程那几个目标文件
    /// 按 `<staging>/resources/app/<rel>` 摆好再指过来，下面探测 app 根时的第二个候选形状
    /// （`resources/app`）正好接得住，于是 engine / integrity / commit / backup 全都一行不改地复用。
    pub fn from_root(app: &Path, profile: LayoutProfile) -> Result<Self> {
        let app_root = [
            app.join("Contents/Resources/app"),
            app.join("resources/app"),
        ]
        .into_iter()
        .find(|p| p.join("product.json").is_file())
        .ok_or_else(|| {
            AppError::new(
                ErrorCode::CursorNotFound,
                format!("{} 里没有 Resources/app/product.json。", app.display()),
            )
            .with_hint("这可能不是 Cursor 安装目录，或者该版本改用了 app.asar 打包。")
        })?;
        let product_json = app_root.join("product.json");
        let version = read_version(&product_json)?;

        let mut targets = Vec::new();
        for (rel, _) in TARGET_SPECS {
            let t = app_root.join(rel);
            if !t.is_file() {
                continue;
            }
            let real = t.canonicalize()?;
            if !real.starts_with(app_root.canonicalize()?) {
                return Err(AppError::new(
                    ErrorCode::SandIntegrity,
                    format!("目标文件符号链接逃逸出 Cursor 目录：{}", t.display()),
                ));
            }
            targets.push(real);
        }
        if targets.is_empty() {
            return Err(AppError::new(
                ErrorCode::SandUnsupportedVersion,
                "这个 Cursor 里没有任何可识别的补丁目标文件。",
            )
            .with_hint("可能是版本差异太大或打包方式变了；等待适配。"));
        }

        let ext_host = {
            let p = app_root.join(EXT_HOST_REL);
            p.is_file().then(|| p.canonicalize()).transpose()?
        };

        Ok(Self {
            install_root: app.to_path_buf(),
            app_root,
            product_json,
            targets,
            ext_host,
            version,
            profile,
        })
    }

    /// 某个目标属于哪个扩展（用于 hash 同步）。不属于任何扩展返回 `None`。
    pub fn extension_name_of(&self, target: &Path) -> Option<&'static str> {
        TARGET_SPECS.iter().find_map(|(rel, ext)| {
            let ext = (*ext)?;
            let candidate = self.app_root.join(rel).canonicalize().ok()?;
            (candidate == target).then_some(ext)
        })
    }

    /// 目标相对 app 根的路径（给界面显示、给备份做 key）。
    pub fn relative(&self, target: &Path) -> String {
        target
            .strip_prefix(
                self.app_root
                    .canonicalize()
                    .unwrap_or(self.app_root.clone()),
            )
            .or_else(|_| target.strip_prefix(&self.app_root))
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| target.to_string_lossy().into_owned())
    }

    pub fn version_supported(&self) -> bool {
        self.version == crate::model::SUPPORTED_CURSOR_VERSION
    }
}

/// 找不到安装目录时，告诉用户在**他这个系统上**该去哪儿看。
/// 给 Windows 用户指「Cursor.app / /Applications」只会把人带偏。
fn missing_app_hint() -> &'static str {
    if cfg!(target_os = "windows") {
        "在设置里指定 Cursor 的安装目录（默认是 %LOCALAPPDATA%\\Programs\\cursor）。"
    } else if cfg!(target_os = "macos") {
        "在设置里指定 Cursor.app 的位置，或确认 Cursor 已安装在 /Applications。"
    } else {
        "在设置里指定 Cursor 的安装目录（常见于 /usr/share/cursor 或 /opt/cursor）。"
    }
}

fn read_version(product_json: &Path) -> Result<String> {
    let raw = std::fs::read(product_json)?;
    let text = String::from_utf8_lossy(raw.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&raw));
    let json: serde_json::Value = serde_json::from_str(&text)?;
    let name = json
        .get("applicationName")
        .or_else(|| json.get("nameShort"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !name.eq_ignore_ascii_case("cursor") {
        return Err(AppError::new(
            ErrorCode::CursorNotFound,
            format!("{} 不是 Cursor 的 product.json。", product_json.display()),
        ));
    }
    Ok(json
        .get("version")
        .or_else(|| json.get("commit"))
        .and_then(|v| v.as_str())
        .unwrap_or("未知")
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 适配版本用常量，别在测试里写死数字 —— Cursor 一升级测试就跟着炸。
    const SUPPORTED: &str = crate::model::SUPPORTED_CURSOR_VERSION;
    /// 任何一个不等于 SUPPORTED 的版本号。
    const OTHER: &str = "0.0.1";

    fn fake_bundle(dir: &Path, version: &str, files: &[&str]) -> PathBuf {
        let app = dir.join("Cursor.app");
        let root = app.join("Contents/Resources/app");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("product.json"),
            format!(r#"{{"applicationName":"Cursor","version":"{version}"}}"#),
        )
        .unwrap();
        for f in files {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"// js").unwrap();
        }
        app
    }

    #[test]
    fn resolves_only_the_targets_that_exist() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(
            dir.path(),
            SUPPORTED,
            &["out/main.js", "extensions/cursor-agent-host/dist/4884.js"],
        );
        let l = SandLayout::from_app(&app).unwrap();
        assert_eq!(l.version, SUPPORTED);
        assert!(l.version_supported());
        assert_eq!(l.targets.len(), 2);
        assert!(l.ext_host.is_none());
        assert_eq!(
            l.relative(&l.targets[1]),
            "extensions/cursor-agent-host/dist/4884.js"
        );
    }

    #[test]
    fn knows_which_extension_a_target_belongs_to() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(
            dir.path(),
            "3.18.9",
            &[
                "extensions/cursor-agent-host/dist/main.js",
                "extensions/cursor-agent-host/dist/4884.js",
            ],
        );
        let l = SandLayout::from_app(&app).unwrap();
        let main = l
            .targets
            .iter()
            .find(|t| t.ends_with("dist/main.js"))
            .unwrap();
        let chunk = l.targets.iter().find(|t| t.ends_with("4884.js")).unwrap();
        assert_eq!(l.extension_name_of(main), Some("cursor-agent-host"));
        assert_eq!(l.extension_name_of(chunk), None);
    }

    #[test]
    fn unsupported_version_is_reported_but_layout_still_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(dir.path(), OTHER, &["out/main.js"]);
        let l = SandLayout::from_app(&app).unwrap();
        assert!(!l.version_supported());
    }

    #[test]
    fn no_targets_means_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(dir.path(), "3.18.9", &[]);
        let err = SandLayout::from_app(&app).unwrap_err();
        assert_eq!(err.code, ErrorCode::SandUnsupportedVersion);
    }

    #[test]
    fn not_a_cursor_product_json_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Other.app");
        let root = app.join("Contents/Resources/app");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("product.json"),
            r#"{"applicationName":"Code","version":"1.0"}"#,
        )
        .unwrap();
        let err = SandLayout::from_app(&app).unwrap_err();
        assert_eq!(err.code, ErrorCode::CursorNotFound);
    }
}
