//! 备份：动任何文件之前，把它们**改动前**的完整字节存下来。
//!
//! 放应用数据目录 `sand/backups/<install-hash>/<stamp>-<op>/`，每份一个 `manifest.json`
//! 加 `files/<相对路径>`。不进 `SecretStore`（没有秘密）也不进 SQLite（单个 bundle 几 MB）。
//!
//! `install-hash` 是 app 根路径的 sha256 前 16 位：同一台机器装了两个 Cursor 也不会串。

use nexus_core::{now_iso, AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::integrity::sha256_hex;
use crate::model::{Operation, SandBackup};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub version: u32,
    pub operation: Operation,
    /// `prepared` → `committed` | `rolled_back`。
    pub state: String,
    pub app_root: String,
    pub cursor_version: String,
    pub created_at: String,
    pub finished_at: Option<String>,
    pub error: Option<String>,
    pub files: Vec<ManifestFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManifestFile {
    /// 相对 app 根。
    pub path: String,
    pub original_sha256: String,
    pub next_sha256: String,
    #[cfg(unix)]
    pub mode: u32,
}

/// 一份待写入的文件：改动前 / 改动后的字节。
#[derive(Debug, Clone)]
pub struct PlannedFile {
    pub path: PathBuf,
    pub original: Vec<u8>,
    pub next: Vec<u8>,
}

pub struct Backups {
    root: PathBuf,
}

impl Backups {
    pub fn new(data_dir: &Path, app_root: &Path) -> Self {
        Self::for_app(data_dir, "sand", app_root)
    }

    /// 按「哪个安装器 + 哪份 Cursor」分目录。CRSR 用 `scope = "crsr"`，避免和 Sand 的备份搅在一起。
    pub fn for_app(data_dir: &Path, scope: &str, app_root: &Path) -> Self {
        Self::with_scope(data_dir, scope, &install_key(app_root))
    }

    /// 用一个自定义的键分组。remote 用它：那边的 app 根是一个**每次都不同**的本地暂存目录，
    /// 拿路径当键会让同一台远程的备份散成一堆；改用 `ssh://<host>/<commit>` 这种稳定标识。
    pub fn with_key(data_dir: &Path, key: &str) -> Self {
        Self::with_scope(data_dir, "sand", key)
    }

    pub fn with_scope(data_dir: &Path, scope: &str, key: &str) -> Self {
        let key = &sha256_hex(key.as_bytes())[..16];
        Self {
            root: data_dir.join(scope).join("backups").join(key),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 写一份 `prepared` 状态的备份，返回其 id（= 目录名）。
    pub fn create(
        &self,
        app_root: &Path,
        cursor_version: &str,
        operation: Operation,
        plan: &[PlannedFile],
    ) -> Result<String> {
        let stamp = now_iso().replace([':', '.'], "");
        let id = format!("{stamp}-{}", operation.as_str());
        let dir = self.root.join(&id);
        let files_dir = dir.join("files");
        let mut entries = Vec::with_capacity(plan.len());
        for f in plan {
            let rel = relative(app_root, &f.path)?;
            let dst = files_dir.join(&rel);
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dst, &f.original)?;
            entries.push(ManifestFile {
                path: rel.to_string_lossy().replace('\\', "/"),
                original_sha256: sha256_hex(&f.original),
                next_sha256: sha256_hex(&f.next),
                #[cfg(unix)]
                mode: {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::metadata(&f.path)?.permissions().mode()
                },
            });
        }
        let manifest = Manifest {
            version: 1,
            operation,
            state: "prepared".into(),
            app_root: app_root.to_string_lossy().into_owned(),
            cursor_version: cursor_version.to_string(),
            created_at: now_iso(),
            finished_at: None,
            error: None,
            files: entries,
        };
        self.write_manifest(&id, &manifest)?;
        Ok(id)
    }

    pub fn finish(&self, id: &str, state: &str, error: Option<String>) -> Result<()> {
        let mut m = self.manifest(id)?;
        m.state = state.into();
        m.finished_at = Some(now_iso());
        m.error = error.map(|e| e.chars().take(1000).collect());
        self.write_manifest(id, &m)
    }

    pub fn manifest(&self, id: &str) -> Result<Manifest> {
        let path = self.root.join(id).join("manifest.json");
        if !path.is_file() {
            return Err(AppError::new(
                ErrorCode::BackupNotFound,
                format!("没有这份备份：{id}"),
            ));
        }
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }

    /// 读回某个文件改动前的字节。
    pub fn original_bytes(&self, id: &str, rel: &str) -> Result<Vec<u8>> {
        Ok(std::fs::read(self.root.join(id).join("files").join(rel))?)
    }

    /// 按时间升序。
    pub fn list(&self) -> Result<Vec<SandBackup>> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.root) else {
            return Ok(out);
        };
        for entry in rd.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            let Ok(m) = self.manifest(&id) else { continue };
            out.push(SandBackup {
                id,
                created_at: m.created_at,
                operation: m.operation,
                cursor_version: m.cursor_version,
                files: m.files.len() as u32,
                state: m.state,
                error: m.error,
            });
        }
        out.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(out)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        let dir = self.root.join(id);
        if !dir.is_dir() {
            return Err(AppError::new(
                ErrorCode::BackupNotFound,
                format!("没有这份备份：{id}"),
            ));
        }
        std::fs::remove_dir_all(dir)?;
        Ok(())
    }

    /// 只留最近 `keep` 份。
    pub fn prune(&self, keep: usize) -> Result<usize> {
        let list = self.list()?;
        let excess = list.len().saturating_sub(keep);
        for b in list.iter().take(excess) {
            let _ = self.remove(&b.id);
        }
        Ok(excess)
    }

    fn write_manifest(&self, id: &str, m: &Manifest) -> Result<()> {
        let dir = self.root.join(id);
        std::fs::create_dir_all(&dir)?;
        let data = serde_json::to_vec_pretty(m)?;
        crate::commit::atomic_write(&dir.join("manifest.json"), &data)
    }
}

/// 备份桶的键：同一个安装永远算出同一个键。
///
/// Windows 上要先归一化再哈希。同一个目录可以写成 `C:\Users\me\...`、`c:/users/me/...`，
/// 大小写和分隔符都不固定（我们自己就有 `canonicalize` 和用户手填两个来源）。不归一化的话，
/// 同一份 Cursor 会因为路径写法不同散进好几个备份桶，用户在界面上看不到刚才那份备份。
/// **macOS / Linux 保持原样**：那里路径区分大小写，归一化反而会把两个不同目录并成一个，
/// 而且会让已有用户的备份目录整体失联。
fn install_key(app_root: &Path) -> String {
    let raw = app_root.to_string_lossy();
    if cfg!(target_os = "windows") {
        raw.replace('\\', "/").trim_end_matches('/').to_lowercase()
    } else {
        raw.into_owned()
    }
}

fn relative(app_root: &Path, path: &Path) -> Result<PathBuf> {
    let root = app_root.canonicalize().unwrap_or(app_root.to_path_buf());
    let p = path.canonicalize().unwrap_or(path.to_path_buf());
    p.strip_prefix(&root).map(Path::to_path_buf).map_err(|_| {
        AppError::new(
            ErrorCode::SandIntegrity,
            format!("计划文件逃逸出 Cursor 目录：{}", path.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_list_finish_and_restore_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let app_root = dir.path().join("app");
        std::fs::create_dir_all(app_root.join("out")).unwrap();
        let target = app_root.join("out/main.js");
        std::fs::write(&target, b"orig").unwrap();

        let b = Backups::new(dir.path(), &app_root);
        let plan = vec![PlannedFile {
            path: target.clone(),
            original: b"orig".to_vec(),
            next: b"patched".to_vec(),
        }];
        let id = b
            .create(&app_root, "3.18.9", Operation::Install, &plan)
            .unwrap();
        assert_eq!(b.original_bytes(&id, "out/main.js").unwrap(), b"orig");

        let list = b.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].state, "prepared");
        assert_eq!(list[0].files, 1);

        b.finish(&id, "committed", None).unwrap();
        assert_eq!(b.manifest(&id).unwrap().state, "committed");

        b.remove(&id).unwrap();
        assert!(b.list().unwrap().is_empty());
        assert_eq!(b.manifest(&id).unwrap_err().code, ErrorCode::BackupNotFound);
    }

    #[test]
    fn two_installs_do_not_share_a_backup_root() {
        let dir = tempfile::tempdir().unwrap();
        let a = Backups::new(dir.path(), Path::new("/Applications/Cursor.app/x"));
        let b = Backups::new(dir.path(), Path::new("/Users/me/Applications/Cursor.app/x"));
        assert_ne!(a.root(), b.root());
    }

    /// Windows 上同一个安装的几种写法必须落进同一个桶，否则备份会「消失」。
    #[cfg(windows)]
    #[test]
    fn windows_path_spellings_map_to_one_backup_root() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = Backups::new(dir.path(), Path::new(r"C:\Users\me\AppData\Local\cursor"));
        for spelling in [
            r"c:\users\me\appdata\local\cursor",
            "C:/Users/me/AppData/Local/cursor",
            r"C:\Users\me\AppData\Local\cursor\",
        ] {
            assert_eq!(
                Backups::new(dir.path(), Path::new(spelling)).root(),
                canonical.root(),
                "{spelling} 该和标准写法同桶"
            );
        }
        // 归一化不能把两个真正不同的安装并到一起。
        assert_ne!(
            Backups::new(dir.path(), Path::new(r"C:\Program Files\cursor")).root(),
            canonical.root()
        );
    }

    /// 反过来，macOS / Linux 上路径区分大小写，不能归一化。
    #[cfg(not(windows))]
    #[test]
    fn case_differences_stay_distinct_on_unix() {
        let dir = tempfile::tempdir().unwrap();
        assert_ne!(
            Backups::new(dir.path(), Path::new("/Applications/Cursor.app")).root(),
            Backups::new(dir.path(), Path::new("/applications/cursor.app")).root()
        );
    }

    #[test]
    fn plan_file_outside_app_root_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let app_root = dir.path().join("app");
        std::fs::create_dir_all(&app_root).unwrap();
        let outside = dir.path().join("elsewhere.js");
        std::fs::write(&outside, b"x").unwrap();
        let b = Backups::new(dir.path(), &app_root);
        let err = b
            .create(
                &app_root,
                "3.18.9",
                Operation::Install,
                &[PlannedFile {
                    path: outside,
                    original: b"x".to_vec(),
                    next: b"y".to_vec(),
                }],
            )
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::SandIntegrity);
    }
}
