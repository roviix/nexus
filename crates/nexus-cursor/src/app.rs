//! 退出 / 等待 / 启动 Cursor 桌面端；以及热登录（deep link）。
//!
//! **冷切**必须在 Cursor 退出后写库：它跑着的时候会用内存里的状态把我们写的覆盖回去，
//! 机器码也是启动时读一次就缓存。所以冷切顺序永远是 **退出 → 等进程真的消失 → 写 → 启动**。
//!
//! **热切**不走写库：把 `cursor://cursorAuth?route=login&…` 交给正在跑的 Cursor，
//! 由它自己的 `storeAccessRefreshToken` 更新内存与磁盘（见 `inject_login`）。
//!
//! 「等进程真的消失」不是多余的：AppleScript 的 quit 一发出就返回，此时 Cursor 还在
//! 保存会话、关 WAL。这个空档里写库，写进去的会被它的收尾覆盖掉。
//!
//! 接口做成 trait，是为了让 `nexus-switcher` 的编排逻辑能在不真的关用户编辑器的前提下
//! 被测到——那段逻辑正是最不能出错、又最难手工回归的部分。

use nexus_core::{AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// 退出动作的结果。界面按它显示「Cursor 已退出」还是「已强制结束」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuitOutcome {
    /// 动手之前它在不在跑。
    pub was_running: bool,
    /// 是否用了强制手段（优雅退出超时）。
    pub forced: bool,
}

pub trait CursorControl: Send + Sync {
    fn is_running(&self) -> Result<bool>;
    /// 优雅退出并**等到进程真的消失**；超时则强制结束。
    fn quit(&self, timeout: Duration) -> Result<QuitOutcome>;
    fn launch(&self) -> Result<()>;
    /// 热登录：把一对 token 交给**正在跑的** Cursor（deep link），由它自己
    /// `storeAccessRefreshToken`——写盘 + 刷内存缓存。这是热切的唯一入口。
    ///
    /// 调用方必须保证 Cursor 在跑；没在跑时这条路无效，应改走冷切写库。
    fn inject_login(&self, access_token: &str, refresh_token: &str) -> Result<()>;
}

/// 真机实现。
#[derive(Debug, Clone, Default)]
pub struct SystemCursor {
    /// 应用本体路径。`None` 时启动走系统默认解析（macOS 的 `open -a Cursor`）。
    app: Option<PathBuf>,
}

impl SystemCursor {
    pub fn new(app: Option<PathBuf>) -> Self {
        Self { app }
    }
}

/// 主进程退了之后，helper 进程还要一会儿才放开文件。这段等待是实测出来的，
/// 省掉它会偶发「写进去又被覆盖」。
const SETTLE: Duration = Duration::from_millis(1200);
const POLL: Duration = Duration::from_millis(400);

impl CursorControl for SystemCursor {
    fn is_running(&self) -> Result<bool> {
        if cfg!(target_os = "macos") || cfg!(target_os = "linux") {
            // 用 pgrep 而不是 osascript：快一个数量级，且不会触发 macOS 的
            // 「Nexus 想要控制 Cursor」自动化授权弹窗——那个弹窗只在真要 quit 时才该出现。
            let out = command("pgrep").args(["-x", "Cursor"]).output()?;
            Ok(out.status.success() && !out.stdout.is_empty())
        } else if cfg!(target_os = "windows") {
            // 不用 `/NH` 的纯文本：中文系统的表头和换行位置都会变。`/FO CSV` 是
            // 稳定的机器格式，判断依据也从「输出里有没有这个词」收紧成「第一列是不是它」。
            let out = command("tasklist")
                .args(["/FI", "IMAGENAME eq Cursor.exe", "/FO", "CSV", "/NH"])
                .output()?;
            Ok(csv_lists_process(
                &String::from_utf8_lossy(&out.stdout),
                "cursor.exe",
            ))
        } else {
            Err(AppError::unsupported_platform("检测 Cursor 是否在运行"))
        }
    }

    fn inject_login(&self, access_token: &str, refresh_token: &str) -> Result<()> {
        open_cursor_auth_login(access_token, refresh_token)
    }

    fn quit(&self, timeout: Duration) -> Result<QuitOutcome> {
        if !self.is_running()? {
            return Ok(QuitOutcome {
                was_running: false,
                forced: false,
            });
        }
        request_quit()?;

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if !self.is_running()? {
                std::thread::sleep(SETTLE);
                return Ok(QuitOutcome {
                    was_running: true,
                    forced: false,
                });
            }
            std::thread::sleep(POLL);
        }

        tracing::warn!("Cursor 没在限时内退出，强制结束");
        force_quit()?;
        std::thread::sleep(SETTLE);
        if self.is_running()? {
            return Err(
                AppError::new(ErrorCode::CursorControl, "Cursor 无法结束，切号已中止。")
                    .with_hint("手动退出 Cursor 后重试。你的登录态没有被改动。"),
            );
        }
        Ok(QuitOutcome {
            was_running: true,
            forced: true,
        })
    }

    fn launch(&self) -> Result<()> {
        let status = if cfg!(target_os = "macos") {
            match &self.app {
                Some(app) => command("open").arg("-a").arg(app).status(),
                None => command("open").args(["-a", "Cursor"]).status(),
            }
        } else if cfg!(target_os = "windows") {
            // 直接 spawn 那个 exe，不经 `cmd /C start`：后者对路径里的空格和引号
            // 极其敏感（`C:\Program Files\...` 会被 `start` 当成窗口标题），
            // 而且会闪一个控制台窗口。
            let exe = self
                .app
                .as_ref()
                .map(|dir| dir.join("Cursor.exe"))
                .unwrap_or_else(|| PathBuf::from("Cursor.exe"));
            command(&exe).spawn().map(|_| Default::default())
        } else {
            let bin = self
                .app
                .as_ref()
                .map(|dir| dir.join("cursor"))
                .unwrap_or_else(|| PathBuf::from("cursor"));
            command(&bin).spawn().map(|_| Default::default())
        }?;
        if status.success() {
            Ok(())
        } else {
            Err(
                AppError::new(ErrorCode::CursorControl, "启动 Cursor 失败。")
                    .with_hint("登录态已经切好了，手动打开 Cursor 即可，不需要重新切换。"),
            )
        }
    }
}

/// 起一个不弹控制台窗口的子进程。
///
/// Windows 上从 GUI 程序 spawn 控制台程序（tasklist / taskkill）会闪一个黑框；
/// 切号一次要轮询好几遍 `is_running`，那就是连闪几次。`CREATE_NO_WINDOW` 压掉它。
fn command(program: impl AsRef<std::ffi::OsStr>) -> Command {
    #[allow(unused_mut)]
    let mut cmd = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// `tasklist /FO CSV /NH` 的输出里，第一列（映像名）是不是这个进程。
///
/// 过滤器没匹配到任何进程时 tasklist 会打一句「信息: 没有运行的任务…」而**不是**空输出，
/// 所以不能只判断非空；按 CSV 第一列比对才不会把那句提示当成命中。
fn csv_lists_process(stdout: &str, image_lower: &str) -> bool {
    stdout.lines().any(|line| {
        line.trim_start()
            .strip_prefix('"')
            .and_then(|rest| rest.split('"').next())
            .is_some_and(|name| name.eq_ignore_ascii_case(image_lower))
    })
}

/// 优雅退出：让 Cursor 自己走保存和清理流程。
fn request_quit() -> Result<()> {
    if cfg!(target_os = "macos") {
        // 失败不算错：可能它刚好自己退了，也可能用户拒了自动化授权——
        // 两种情况都由后面的「等 + 强制」兜住。
        let _ = command("osascript")
            .args(["-e", r#"tell application "Cursor" to quit"#])
            .output();
        Ok(())
    } else if cfg!(target_os = "windows") {
        // `/T` 连子进程一起收：Cursor 是 Electron，渲染进程和 extension host 都是
        // 独立进程，只关主进程的话它们还攥着 state.vscdb 的文件锁，随后写库就会撞上
        // SQLITE_BUSY，或者写完又被它们的收尾覆盖掉。
        let _ = command("taskkill")
            .args(["/IM", "Cursor.exe", "/T"])
            .output();
        Ok(())
    } else if cfg!(target_os = "linux") {
        let _ = command("pkill").args(["-x", "Cursor"]).output();
        Ok(())
    } else {
        Err(AppError::unsupported_platform("退出 Cursor"))
    }
}

fn force_quit() -> Result<()> {
    if cfg!(target_os = "windows") {
        let _ = command("taskkill")
            .args(["/F", "/T", "/IM", "Cursor.exe"])
            .output();
    } else {
        let _ = command("pkill").args(["-9", "-x", "Cursor"]).output();
    }
    Ok(())
}

/// 拼 Cursor 官方登录 deep link，并交给 OS 打开。
///
/// 形状来自本机 `workbench.desktop.main.js`（3.18.25）实测：
/// `urlService` 在 `scheme ∈ {control, cursor, urlProtocol}` 且
/// `authority === "cursorAuth"` 时把 query 交给 `handleAuth`；
/// `route=login` 且带齐 access/refresh 时走 `storeAccessRefreshToken`。
/// **没有额外签名校验**——token 本身就是凭证。
fn open_cursor_auth_login(access_token: &str, refresh_token: &str) -> Result<()> {
    if access_token.is_empty() || refresh_token.is_empty() {
        return Err(AppError::new(
            ErrorCode::ProfileIncomplete,
            "热登录需要 accessToken 和 refreshToken。",
        ));
    }
    let url = format!(
        "cursor://cursorAuth?route=login&accessToken={}&refreshToken={}",
        percent_encode(access_token),
        percent_encode(refresh_token)
    );
    open_url_scheme(&url)
}

fn open_url_scheme(url: &str) -> Result<()> {
    // Windows 不走 `cmd /C start`：登录深链的 query 里有 `&`，Rust 只给含空格的参数加引号，
    // 于是 cmd 把它当命令分隔符——`start` 只收到 `…?route=login`，后面的
    // `accessToken=…` / `refreshToken=…` 被当成两条不存在的命令执行，exit code 1。
    // Cursor 收到的是一条没有 token 的深链，什么也不做；我们这边报「打开深链失败」。
    // ShellExecuteW 把 URL 当数据而不是命令行文本，不存在这层解析。
    #[cfg(windows)]
    {
        return shell_execute_url(url);
    }

    #[allow(unreachable_code)]
    let result = if cfg!(target_os = "macos") {
        command("open").arg(url).status()
    } else if cfg!(target_os = "linux") {
        command("xdg-open").arg(url).status()
    } else {
        return Err(AppError::unsupported_platform("打开 Cursor 登录深链"));
    };
    match result {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(AppError::new(
            ErrorCode::CursorControl,
            format!("打开 Cursor 深链失败（exit {status}）。"),
        )
        .with_hint("确认 Cursor 已安装且 `cursor://` 协议已注册。")),
        Err(err) => Err(AppError::new(
            ErrorCode::CursorControl,
            format!("打不开 Cursor 深链：{err}"),
        )
        .with_hint("确认 Cursor 已安装且 `cursor://` 协议已注册。")),
    }
}

/// 用 `ShellExecuteW(open)` 打开一个 URL scheme，等价于用户双击了这条链接。
///
/// 直接 `#[link]` shell32 而不是引入 windows-sys：只用这一个函数，不值得多一棵依赖树。
#[cfg(windows)]
fn shell_execute_url(url: &str) -> Result<()> {
    use std::ptr::null;

    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: isize,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show_cmd: i32,
        ) -> isize;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    const SW_SHOWNORMAL: i32 = 1;
    // 返回值 > 32 表示成功；≤ 32 是错误码（Win32 约定）。
    const SE_ERR_NOASSOC: isize = 31;
    const SE_ERR_ACCESSDENIED: isize = 5;

    let verb = wide("open");
    let file = wide(url);
    let code = unsafe {
        ShellExecuteW(
            0,
            verb.as_ptr(),
            file.as_ptr(),
            null(),
            null(),
            SW_SHOWNORMAL,
        )
    };
    if code > 32 {
        return Ok(());
    }
    let hint = match code {
        SE_ERR_NOASSOC => {
            "这台机器没有注册 `cursor://` 协议。重装或重新打开一次 Cursor 通常会补上注册。"
        }
        SE_ERR_ACCESSDENIED => "系统拒绝了打开协议链接。检查是否有安全软件拦截了 `cursor://`。",
        _ => "确认 Cursor 已安装且 `cursor://` 协议已注册。",
    };
    Err(AppError::new(
        ErrorCode::CursorControl,
        format!("打开 Cursor 深链失败（ShellExecute 返回 {code}）。"),
    )
    .with_hint(hint))
}

/// RFC 3986 unreserved 原样保留，其余百分号编码。JWT / refresh token 里的
/// `+` `/` `=` 都必须编码，否则 deep link 的 query 会被截断。
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod encode_tests {
    use super::percent_encode;

    #[test]
    fn percent_encode_leaves_jwt_safe_chars_and_encodes_the_rest() {
        assert_eq!(percent_encode("abc-._~"), "abc-._~");
        assert_eq!(percent_encode("a+b/c="), "a%2Bb%2Fc%3D");
        assert_eq!(percent_encode("x y"), "x%20y");
    }
}

/// 测试替身。开 `testing` 特性才编进来，发布构建里不存在。
#[cfg(feature = "testing")]
pub mod testing {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// 热登录时写入磁盘的回调。测试 harness 挂上「把 token 写进 state.vscdb」，
    /// 模拟 Cursor 收到 deep link 后自己 `storeAccessRefreshToken`。
    pub type InjectHandler = Arc<dyn Fn(&str, &str) + Send + Sync>;

    /// 记录被调用过什么的假 Cursor。用来验证切号编排的**顺序**——
    /// 那是整个应用里最不能错、又最难手工回归的一段。
    #[derive(Default)]
    pub struct FakeCursor {
        running: AtomicBool,
        pub quits: AtomicUsize,
        pub launches: AtomicUsize,
        pub injects: AtomicUsize,
        /// 设成 true 时 `quit` 报错，用来测「卡在退出这一步」的路径。
        pub quit_fails: AtomicBool,
        /// 设成 true 时 `launch` 报错，用来测「已切好但没起来」的路径。
        pub launch_fails: AtomicBool,
        /// 设成 true 时 `inject_login` 报错。
        pub inject_fails: AtomicBool,
        on_inject: Mutex<Option<InjectHandler>>,
    }

    impl std::fmt::Debug for FakeCursor {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("FakeCursor")
                .field("running", &self.running.load(Ordering::SeqCst))
                .field("quits", &self.quit_count())
                .field("launches", &self.launch_count())
                .field("injects", &self.inject_count())
                .finish()
        }
    }

    impl FakeCursor {
        pub fn running() -> Self {
            let f = Self::default();
            f.running.store(true, Ordering::SeqCst);
            f
        }

        pub fn stopped() -> Self {
            Self::default()
        }

        pub fn quit_count(&self) -> usize {
            self.quits.load(Ordering::SeqCst)
        }

        pub fn launch_count(&self) -> usize {
            self.launches.load(Ordering::SeqCst)
        }

        pub fn inject_count(&self) -> usize {
            self.injects.load(Ordering::SeqCst)
        }

        pub fn fail_quit(&self) {
            self.quit_fails.store(true, Ordering::SeqCst);
        }

        pub fn fail_launch(&self) {
            self.launch_fails.store(true, Ordering::SeqCst);
        }

        pub fn fail_inject(&self) {
            self.inject_fails.store(true, Ordering::SeqCst);
        }

        /// 挂上「收到热登录时把 token 落到盘上」的回调。
        pub fn on_inject(&self, handler: InjectHandler) {
            *self.on_inject.lock().expect("inject lock") = Some(handler);
        }
    }

    impl CursorControl for FakeCursor {
        fn is_running(&self) -> Result<bool> {
            Ok(self.running.load(Ordering::SeqCst))
        }

        fn quit(&self, _timeout: Duration) -> Result<QuitOutcome> {
            if self.quit_fails.load(Ordering::SeqCst) {
                return Err(AppError::new(ErrorCode::CursorControl, "假装退不掉"));
            }
            self.quits.fetch_add(1, Ordering::SeqCst);
            let was_running = self.running.swap(false, Ordering::SeqCst);
            Ok(QuitOutcome {
                was_running,
                forced: false,
            })
        }

        fn launch(&self) -> Result<()> {
            if self.launch_fails.load(Ordering::SeqCst) {
                return Err(AppError::new(ErrorCode::CursorControl, "假装起不来"));
            }
            self.launches.fetch_add(1, Ordering::SeqCst);
            self.running.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn inject_login(&self, access_token: &str, refresh_token: &str) -> Result<()> {
            if self.inject_fails.load(Ordering::SeqCst) {
                return Err(AppError::new(ErrorCode::CursorControl, "假装深链打不开"));
            }
            self.injects.fetch_add(1, Ordering::SeqCst);
            if let Some(handler) = self.on_inject.lock().expect("inject lock").as_ref() {
                handler(access_token, refresh_token);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detecting_a_running_cursor_does_not_error_on_this_platform() {
        // 真机上跑：不断言结果（取决于用户此刻有没有开 Cursor），只断言不报错。
        // 这一条挡住的是命令名写错、参数写错这类低级错误。
        if cfg!(any(
            target_os = "macos",
            target_os = "windows",
            target_os = "linux"
        )) {
            assert!(SystemCursor::default().is_running().is_ok());
        }
    }

    #[test]
    fn tasklist_csv_is_read_by_column_not_by_substring() {
        // 真实命中。
        assert!(csv_lists_process(
            "\"Cursor.exe\",\"12345\",\"Console\",\"1\",\"420,000 K\"\r\n",
            "cursor.exe"
        ));
        // 没匹配到时 tasklist 打的是这句话，不是空输出 —— 它里面也含 "Cursor.exe"，
        // 用 `contains` 判断就会把「没在跑」读成「在跑」。
        assert!(!csv_lists_process(
            "信息: 没有运行的任务匹配指定标准: IMAGENAME eq Cursor.exe\r\n",
            "cursor.exe"
        ));
        assert!(!csv_lists_process("", "cursor.exe"));
        // 别的进程的命令行里提到它也不算。
        assert!(!csv_lists_process(
            "\"other.exe\",\"1\",\"Console\",\"1\",\"Cursor.exe\"\r\n",
            "cursor.exe"
        ));
    }

    #[test]
    fn quit_on_a_stopped_cursor_is_a_no_op() {
        // 用假的验证契约：没在跑时不该去动任何进程。
        #[derive(Default)]
        struct NotRunning;
        impl CursorControl for NotRunning {
            fn is_running(&self) -> Result<bool> {
                Ok(false)
            }
            fn quit(&self, _t: Duration) -> Result<QuitOutcome> {
                Ok(QuitOutcome {
                    was_running: false,
                    forced: false,
                })
            }
            fn launch(&self) -> Result<()> {
                Ok(())
            }
            fn inject_login(&self, _: &str, _: &str) -> Result<()> {
                Ok(())
            }
        }
        let outcome = NotRunning.quit(Duration::from_secs(1)).unwrap();
        assert!(!outcome.was_running && !outcome.forced);
    }
}
