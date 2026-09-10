//! 跑 `ssh` 的那一层。
//!
//! **用系统 `ssh` 二进制，不用 Rust 的 SSH 库**，这是有意的：用户的 `~/.ssh/config` 里可能有
//! Host 别名、`ProxyCommand`（本仓库作者那台就是走公司代理的 `nc -X 5 -x …`）、跳板机、
//! 各种密钥与 agent 转发。换成 russh / ssh2 就得把 ssh_config 解析和 ProxyCommand 重写一遍，
//! 而且 `-R` 反向隧道也得自己实现。代价是：GUI 进程没有 TTY，`ssh` 一旦要交互（问 host key、
//! 要私钥密码）就会卡住，所以这里一律 `BatchMode=yes`——宁可失败也不吊死，并在错误提示里
//! 明确告诉用户「先在终端里 `ssh <host>` 能进去」。
//!
//! 连接复用（`ControlMaster`）不是优化而是必需：一次 status 要跑好几条命令，每条都重新握手
//! 的话，走跳板的主机能慢到十几秒。

use nexus_core::{AppError, ErrorCode, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// 连接超时。真正的耗时在传 bundle，那个不设上限（由调用方的进度反馈兜着）。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// 一条 ssh 命令的结果。
#[derive(Debug, Clone)]
pub struct SshOutput {
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

impl SshOutput {
    pub fn ok(&self) -> bool {
        self.status == 0
    }
}

/// 对一台远程主机执行命令。抽成 trait 只为测试能塞假的进来——真实现只有 [`SystemSsh`] 一个。
pub trait SshRunner: Send + Sync {
    /// 把 `script` 交给远程的 `sh -s` 执行；`stdin_after` 会接在脚本之后继续喂给它的标准输入
    /// （用来把 tar 流送过去）。
    fn run(&self, host: &str, script: &str, stdin_after: Option<&[u8]>) -> Result<SshOutput>;

    /// 远程 `tar czf -` 打包若干相对路径，落到本地 `dest` 目录。
    fn pull_tar(&self, host: &str, remote_dir: &str, rels: &[String], dest: &Path) -> Result<()>;
}

/// `ControlPath` 是一个 unix socket，路径有硬上限（macOS `sun_path` 104 字节，Linux 108）。
/// 超了 ssh 直接拒绝跑，报 `ControlPath too long`。而 `%C` 本身就占 40 个字符，再加上 macOS 那种
/// `/var/folders/n1/0c4…/T/` 的临时目录就必然超——所以**不能**拿应用数据目录去拼，得自己另找
/// 一个短的落脚点。
const CONTROL_PATH_LIMIT: usize = 100;

pub struct SystemSsh {
    /// 已经算好的 `ControlPath` 模板；`None` = 不复用连接（路径怎么都放不下，或平台不支持）。
    control_path: Option<String>,
}

impl SystemSsh {
    /// `scope` 只用来把不同的应用数据目录区分开，不会被直接当路径用。
    pub fn new(scope: impl Into<PathBuf>) -> Self {
        Self {
            control_path: control_path_for(&scope.into()),
        }
    }

    fn base_args(&self) -> Vec<String> {
        let mut args = vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            format!("ConnectTimeout={}", CONNECT_TIMEOUT.as_secs()),
        ];
        // 连接复用不是优化而是必需：一次 status 要跑好几条命令，走跳板的主机每条都重新握手
        // 能慢到十几秒。放不下就退回不复用——慢，但能用。
        if let Some(path) = &self.control_path {
            args.extend([
                "-o".into(),
                "ControlMaster=auto".into(),
                "-o".into(),
                format!("ControlPath={path}"),
                "-o".into(),
                "ControlPersist=120".into(),
            ]);
        }
        args
    }
}

/// 在 `/tmp` 下按 scope 开一个短目录。返回的模板里已经含 `%C`。
fn control_path_for(scope: &Path) -> Option<String> {
    if cfg!(windows) {
        // Windows 的 OpenSSH 不支持 ControlMaster。
        return None;
    }
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(scope.to_string_lossy().as_bytes());
    let short = &format!("{digest:x}")[..8];
    let dir = PathBuf::from("/tmp").join(format!("nxs-{short}"));
    std::fs::create_dir_all(&dir).ok()?;
    let path = format!("{}/%C", dir.display());
    // `%C` 展开后是 40 个十六进制字符，这里按展开后的长度算。
    (path.len() - 2 + 40 <= CONTROL_PATH_LIMIT).then_some(path)
}

fn spawn_failed(err: std::io::Error) -> AppError {
    AppError::new(ErrorCode::Io, format!("起不来 ssh 进程：{err}"))
        .with_hint("确认系统里有 ssh 命令（macOS 自带）。")
}

/// 把 ssh 自己的失败翻译成人话。BatchMode 下的典型失败就这几种。
pub fn ssh_error(host: &str, out: &SshOutput) -> AppError {
    let tail = out.stderr.trim();
    let hint = if tail.contains("Host key verification failed") {
        format!("先在终端里 `ssh {host}` 连一次、确认它的指纹，再回来。")
    } else if tail.contains("Permission denied") || tail.contains("publickey") {
        format!("`ssh {host}` 在终端里能免密登录才行（BatchMode 不会向你要密码）。")
    } else if tail.contains("Could not resolve") || tail.contains("Connection timed out") {
        format!("连不上 {host}：确认网络 / VPN / ~/.ssh/config 里的 ProxyCommand。")
    } else {
        format!("先确认 `ssh {host}` 在终端里能直接进去。")
    };
    AppError::new(
        ErrorCode::Network,
        format!("ssh {host} 失败（退出码 {}）：{tail}", out.status),
    )
    .with_hint(hint)
}

/// 把脚本装进远程命令行而不是标准输入：`sh -c 'eval "$(printf %s <base64> | base64 -d)"'`。
///
/// 用在**标准输入要留给二进制载荷**的场合。第一版是 `sh -s` 读脚本、脚本后面紧跟 tar 流——在 bash 上
/// 碰巧能跑（bash 对不可 seek 的 stdin 逐字节读），到了远程 `/bin/sh -> dash` 就炸了：dash 按 8K 块读
/// 命令，把 tar 流的开头一起吞进去，远程 `tar xzf -` 收到的是残流，报 `gzip: stdin: not in gzip format`。
/// base64 只含 `[A-Za-z0-9+/=]`，不需要任何引用；外层单引号能原样穿过用户的登录 shell（zsh / fish 都行），
/// `$(…)` 的展开留给 `sh` 自己做，脚本从 argv 进、stdin 一个字节都不碰。
fn remote_command_from_argv(script: &str) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(script.as_bytes());
    format!("sh -c 'eval \"$(printf %s {b64} | base64 -d)\"'")
}

impl SshRunner for SystemSsh {
    fn run(&self, host: &str, script: &str, stdin_after: Option<&[u8]>) -> Result<SshOutput> {
        let mut cmd = Command::new("ssh");
        cmd.args(self.base_args()).arg(host);
        match stdin_after {
            // 有载荷：脚本走 argv，stdin 只放载荷。见 `remote_command_from_argv`。
            Some(_) => {
                cmd.arg(remote_command_from_argv(script));
            }
            // 无载荷：`sh -s` 从 stdin 读脚本，省掉一层引用。
            None => {
                cmd.arg("sh").arg("-s");
            }
        }
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(spawn_failed)?;
        // 写 stdin 失败（ssh 早退、管道断了）**不能**在这里 `?` 返回：`Child` 被 drop 不会 wait，
        // 留下僵尸进程；而且 ssh 的 stderr 里才是真正的原因（认证失败 / 连不上），得等它退出拿到手。
        let write_err = {
            let mut stdin = child.stdin.take().expect("已经 piped");
            let bytes = match stdin_after {
                Some(payload) => payload,
                None => script.as_bytes(),
            };
            stdin.write_all(bytes).err()
        };
        let out = child.wait_with_output()?;
        let result = SshOutput {
            status: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        };
        if let (Some(err), true) = (write_err, result.ok()) {
            // ssh 说成功、我们却没写完——这不可能是「成功」，如实报。
            return Err(AppError::new(
                ErrorCode::Io,
                format!("向 ssh 写入失败：{err}"),
            ));
        }
        Ok(result)
    }

    fn pull_tar(&self, host: &str, remote_dir: &str, rels: &[String], dest: &Path) -> Result<()> {
        std::fs::create_dir_all(dest)?;
        let list = rels
            .iter()
            .map(|r| shell_quote(r))
            .collect::<Vec<_>>()
            .join(" ");
        // gzip 很值：bundle 是 JS，压缩比大概 4:1，35MB 的传输量能降到 8MB 上下。
        let script = format!("exec tar czf - -C {} {list}\n", shell_quote(remote_dir));

        // 任何一条提前返回的路都不能把 ssh 留成僵尸：`Reap` 在 drop 时 kill + wait。
        let mut ssh = Reap(Some(
            Command::new("ssh")
                .args(self.base_args())
                .arg(host)
                .arg("sh")
                .arg("-s")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(spawn_failed)?,
        ));
        let child = ssh.0.as_mut().expect("刚 spawn");
        // 脚本只有一行，写失败只可能是 ssh 已经退了；原因在它的 stderr 里，下面统一收。
        let _ = child
            .stdin
            .take()
            .expect("已经 piped")
            .write_all(script.as_bytes());
        let ssh_out = child.stdout.take().expect("已经 piped");

        let tar = Command::new("tar")
            .arg("xzf")
            .arg("-")
            .arg("-C")
            .arg(dest)
            .stdin(Stdio::from(ssh_out))
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| AppError::new(ErrorCode::Io, format!("起不来 tar：{e}")))?;

        let tar_out = tar.wait_with_output()?;
        let ssh_res = ssh.0.take().expect("还没 wait").wait_with_output()?;
        let ssh_status = SshOutput {
            status: ssh_res.status.code().unwrap_or(-1),
            stdout: String::new(),
            stderr: String::from_utf8_lossy(&ssh_res.stderr).into_owned(),
        };
        if !ssh_status.ok() {
            return Err(ssh_error(host, &ssh_status));
        }
        if !tar_out.status.success() {
            return Err(AppError::new(
                ErrorCode::Io,
                format!(
                    "解包远程 bundle 失败：{}",
                    String::from_utf8_lossy(&tar_out.stderr).trim()
                ),
            ));
        }
        Ok(())
    }
}

/// 持有一个还没 wait 的子进程；提前返回时 kill + wait，不留僵尸。
/// 正常路径上用 `take()` 拿走去 `wait_with_output`，drop 时就什么都不做。
struct Reap(Option<std::process::Child>);

impl Drop for Reap {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// 单引号包起来，内部的单引号按 `'\''` 转义。远程路径里出现空格 / 引号时不至于把脚本撕开。
pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：第一版拿应用数据目录拼 ControlPath，在 macOS 上被 `/var/folders/…/T/` 撑爆了
    /// （`ControlPath too long`），ssh 直接拒绝跑。
    #[cfg(not(windows))]
    #[test]
    fn control_path_fits_the_unix_socket_limit_even_for_a_long_scope() {
        let long_scope = Path::new(
            "/var/folders/n1/0c4l00r51534x07s734492l80000gn/T/some-very-long-application-support-dir/nexus/sand/ssh",
        );
        let path = control_path_for(long_scope).expect("应该总能在 /tmp 下放下");
        assert!(path.ends_with("/%C"));
        assert!(path.starts_with("/tmp/nxs-"));
        // %C 展开后 40 个字符
        assert!(path.len() - 2 + 40 <= CONTROL_PATH_LIMIT, "{path}");
        // 同一个 scope 得到同一个路径，不同 scope 不同。
        assert_eq!(path, control_path_for(long_scope).unwrap());
        assert_ne!(path, control_path_for(Path::new("/other")).unwrap());
    }

    /// 回归：脚本 + 二进制载荷不能共用 stdin。这里不走 ssh，直接把 `remote_command_from_argv`
    /// 产出的那条命令交给本机 shell 跑（有 dash 就用 dash——远程 `/bin/sh` 就是它，正是它把
    /// 载荷吞掉的），stdin 喂一段 gzip 流，脚本里的 `tar xzf -` 必须能完整收到。
    #[cfg(unix)]
    #[test]
    fn payload_survives_when_script_travels_by_argv() {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let shell = ["/bin/dash", "/usr/bin/dash", "/bin/sh"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .expect("总有 /bin/sh");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), b"payload intact").unwrap();
        let tar = Command::new("tar")
            .arg("czf")
            .arg("-")
            .arg("-C")
            .arg(dir.path())
            .arg("hello.txt")
            .output()
            .unwrap()
            .stdout;
        let out_dir = tempfile::tempdir().unwrap();
        // 脚本故意写得长一点、带引号和 here-doc，逼近真实 push_script 的形状。
        let script = format!(
            "set -e\ndest={}\ncat <<'EOF' >/dev/null\nfiller line with 'quotes' and \"more\"\nEOF\ntar xzf - -C \"$dest\"\necho done\n",
            shell_quote(&out_dir.path().to_string_lossy())
        );
        // 模拟「登录 shell 收到 ssh 送来的一条命令字符串」：用 -c 执行整条。
        let mut child = Command::new(shell)
            .arg("-c")
            .arg(remote_command_from_argv(&script))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(&tar).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "done");
        assert_eq!(
            std::fs::read(out_dir.path().join("hello.txt")).unwrap(),
            b"payload intact"
        );
    }

    #[test]
    fn shell_quote_survives_spaces_and_quotes() {
        assert_eq!(shell_quote("/a/b"), "'/a/b'");
        assert_eq!(shell_quote("/a b/c"), "'/a b/c'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
    }

    #[test]
    fn ssh_error_explains_the_common_batchmode_failures() {
        let host = "box";
        let cases = [
            ("Host key verification failed.", "指纹"),
            ("Permission denied (publickey).", "免密"),
            ("ssh: Could not resolve hostname box", "连不上"),
        ];
        for (stderr, want) in cases {
            let err = ssh_error(
                host,
                &SshOutput {
                    status: 255,
                    stdout: String::new(),
                    stderr: stderr.into(),
                },
            );
            assert_eq!(err.code, ErrorCode::Network);
            assert!(
                err.hint.as_deref().unwrap_or_default().contains(want),
                "stderr={stderr:?} 的提示里应该出现「{want}」，实际是 {:?}",
                err.hint
            );
        }
    }
}
