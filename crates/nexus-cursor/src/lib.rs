//! `nexus-cursor` —— 与本机 Cursor 打交道的**唯一**入口。
//!
//! 做的事只有一件：读写 Cursor **自己的**登录态和机器码，再控制它的进程。不逆向协议、
//! 不改二进制（ARCHITECTURE D4）。这也是唯一「装完一直能用」的机制：官方登录往同一个地方写，
//! Cursor 升级不会让它失效。
//!
//! 边界：这个 crate 只知道「Cursor 的文件长什么样」，不知道「切号本」「我的账号」是什么。
//! 编排在 `nexus-switcher`。

pub mod app;
pub mod locate;
pub mod machine;
pub mod state_db;

pub use app::{CursorControl, QuitOutcome, SystemCursor};
pub use locate::CursorPaths;
pub use machine::MachineIds;
pub use state_db::{AuthBundle, AuthSummary, SchemaCheck, StateDb, AUTH_KEYS, REQUIRED_KEYS};

use nexus_core::Result;

/// 一台机器上 Cursor 的全套操作口。把四个模块拼好，上层只拿这一个。
#[derive(Debug, Clone)]
pub struct Cursor {
    pub paths: CursorPaths,
    pub state: StateDb,
    pub machine: MachineIds,
}

impl Cursor {
    pub fn from_paths(paths: CursorPaths) -> Self {
        Self {
            state: StateDb::new(paths.state_db.clone()),
            machine: MachineIds::new(paths.storage_json.clone(), paths.machine_id_file.clone()),
            paths,
        }
    }

    /// 按平台默认位置探测。
    pub fn detect() -> Result<Self> {
        Ok(Self::from_paths(CursorPaths::detect()?))
    }

    /// 用户在设置里手工指定目录。
    pub fn at(user_dir: impl Into<std::path::PathBuf>) -> Self {
        Self::from_paths(CursorPaths::from_user_dir(user_dir))
    }

    /// 用设置里的安装目录覆盖自动探测。空值 = 不覆盖。
    pub fn with_app_override(self, app_dir: Option<&str>) -> Self {
        Self::from_paths(self.paths.with_app_override(app_dir))
    }

    /// 启动自检（§5.2）。**应用起来第一件事就跑它**：不通过就把切号降级成只读。
    pub fn check(&self) -> SchemaCheck {
        self.state.check(self.paths.version())
    }

    /// 进程控制口。
    pub fn control(&self) -> SystemCursor {
        SystemCursor::new(self.paths.app.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assembles_consistent_paths() {
        let c = Cursor::at("/tmp/fake-cursor");
        assert_eq!(c.state.path(), c.paths.state_db.as_path());
        assert!(!c.state.exists());
        let check = c.check();
        assert!(!check.writable());
    }
}
