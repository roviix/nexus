//! `RemoteSand` 的端到端：**只把 ssh 换成假的**，其余（发现 → 拉 → 打 → 校验 → 推 → 备份 →
//! 还原）全是真代码、真文件。假的 ssh 把一个本地目录当成「远程主机」，脚本按行为等价实现
//! （不是跑 sh，是在 Rust 里做同样的事），tar 流原样进出。
//!
//! 默认 `#[ignore]`：需要一份原版 remote bundle 的镜像。本地跑：
//!
//! ```bash
//! # 先把远程 bundle 镜像到 /tmp/server-staging/resources/app（见 examples/profile_probe.rs）
//! SAND_SERVER_MIRROR=/tmp/server-staging/resources/app \
//!   cargo test -p nexus-sand --test remote_roundtrip -- --ignored
//! ```
//!
//! 用真 bundle 而不是合成的小文件，是因为这里要验的正是「统一规则表在 server 形态上落地」：
//! client-type 命中 2、端点改道命中 2、integrity 对 server 上唯一有内嵌 hash 的扩展
//! （cursor-always-local）不误报——这些只有真文件才能证明。

use nexus_core::Result;
use nexus_sand::model::InstallOptions;
use nexus_sand::remote::{SshOutput, SshRunner};
use nexus_sand::rules::{LayoutProfile, RuleId, SAND_INFERENCE_ENDPOINT_MARKER};
use nexus_sand::{RemoteSand, SUPPORTED_CURSOR_VERSION};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

const COMMIT: &str = "90de2327392570a5f5f625c656c6749d228e6430";

fn mirror() -> Option<PathBuf> {
    std::env::var_os("SAND_SERVER_MIRROR")
        .map(PathBuf::from)
        .filter(|p| p.join("product.json").is_file())
}

/// 一个假的「远程机器」：`home` 下有 `.cursor-server/bin/linux-x64/<commit>/`。
struct FakeHost {
    home: PathBuf,
    /// 记录被杀过几次（restart）。
    restarts: Mutex<u32>,
}

impl FakeHost {
    fn server_root(&self) -> PathBuf {
        self.home.join(".cursor-server/bin/linux-x64").join(COMMIT)
    }
    fn backup_root(&self) -> PathBuf {
        self.home.join(".nexus-sand-backup").join(COMMIT)
    }
}

/// 把 `RemoteSand` 发来的脚本翻译成对 `FakeHost` 目录的操作。
/// 只认我们自己生成的那几种脚本——认不出的直接 panic，免得测试静默漂过。
struct FakeSsh {
    host: Arc<FakeHost>,
}

fn ok(stdout: String) -> Result<SshOutput> {
    Ok(SshOutput {
        status: 0,
        stdout,
        stderr: String::new(),
    })
}

impl SshRunner for FakeSsh {
    fn run(&self, _host: &str, script: &str, stdin_after: Option<&[u8]>) -> Result<SshOutput> {
        let root = self.host.server_root();
        if script.contains(".cursor-server/bin/*/*/") {
            // discover
            let v = product_version(&root.join("product.json"));
            return ok(format!("{COMMIT}\t{v}\t{}\n", root.display()));
        }
        if script.contains("pkill -f") {
            *self.host.restarts.lock().unwrap() += 1;
            return ok("killed\n".into());
        }
        if script.contains("cat > \"$pat\" <<'SANDPAT'") {
            return ok(scan(&root, script));
        }
        if script.contains("&& echo product.json") {
            return ok(exists(&root, script));
        }
        if script.contains("tar xzf - -C \"$tmp\"") {
            push(&self.host, script, stdin_after.expect("push 要带 tar 流"));
            return ok("done\n".into());
        }
        if script.contains("find . -type f") {
            return ok(restore(&self.host));
        }
        if script.contains("SAND_PORT=") {
            // 探针：假远程上没有中继在听，照实说「没人接」——这条路径的真值在
            // `remote::mod` 的单测里（那边拿本机 node 真跑一遍 JS）。
            let port = script
                .lines()
                .find_map(|l| l.split("SAND_PORT=").nth(1))
                .and_then(|rest| rest.split_whitespace().next())
                .unwrap_or("0");
            return ok(format!(
                "RESULT {{\"ok\":false,\"stage\":\"tunnel\",\"detail\":\"connect ECONNREFUSED 127.0.0.1:{port}\"}}\n"
            ));
        }
        panic!("FakeSsh 不认识这段脚本：\n{script}");
    }

    fn pull_tar(&self, _host: &str, remote_dir: &str, rels: &[String], dest: &Path) -> Result<()> {
        // 假远程也用真 tar：这样进出的字节和真机一致。
        let mut cmd = Command::new("tar");
        cmd.arg("czf").arg("-").arg("-C").arg(remote_dir);
        for r in rels {
            cmd.arg(r);
        }
        let tar = cmd.output().expect("tar");
        assert!(
            tar.status.success(),
            "{}",
            String::from_utf8_lossy(&tar.stderr)
        );
        let mut x = Command::new("tar")
            .arg("xzf")
            .arg("-")
            .arg("-C")
            .arg(dest)
            .stdin(Stdio::piped())
            .spawn()
            .expect("tar x");
        std::io::Write::write_all(x.stdin.as_mut().unwrap(), &tar.stdout).unwrap();
        drop(x.stdin.take());
        assert!(x.wait().unwrap().success());
        Ok(())
    }
}

fn product_version(p: &Path) -> String {
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap();
    v["version"].as_str().unwrap().to_string()
}

/// 与 `exists_script` 行为等价：逐个探测脚本里列出的相对路径。
fn exists(root: &Path, script: &str) -> String {
    let mut out = String::new();
    for line in script.lines() {
        let Some(rest) = line.strip_prefix("[ -f \"$root/") else {
            continue;
        };
        let rel = rest.split("\" ]").next().unwrap();
        if root.join(rel).is_file() {
            out.push_str(rel);
            out.push('\n');
        }
    }
    out
}

/// 与 `scan_script` 行为等价：数每个目标文件里每个 marker 的出现次数。
fn scan(root: &Path, script: &str) -> String {
    let pats: Vec<&str> = script
        .split("<<'SANDPAT'\n")
        .nth(1)
        .unwrap()
        .split("\nSANDPAT")
        .next()
        .unwrap()
        .lines()
        .collect();
    let mut out = String::new();
    // 脚本里每个目标文件出现两次：前半段数 marker，`---ENDPOINT---` 之后那一段抠端点。
    // 这里只走前半段，否则每个数都翻倍（2026-09-08 这条测试就是这样静默失效了几个月）。
    let marker_half = script.split("---ENDPOINT---").next().unwrap();
    for line in marker_half.lines() {
        let Some(rest) = line.strip_prefix("f=\"$root/") else {
            continue;
        };
        let rel = rest.split('"').next().unwrap();
        let p = root.join(rel);
        if !p.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&p).unwrap();
        for m in &pats {
            let n = text.matches(m).count();
            if n > 0 {
                out.push_str(&format!("{rel}\t{n}\t{m}\n"));
            }
        }
    }
    out.push_str("---ENDPOINT---\n");
    // 与 `scan_script` 一样扫全部目标：transport 装配处所在的 chunk 每版都可能换文件。
    for (rel, _) in nexus_sand::layout::TARGET_SPECS {
        let Ok(t) = std::fs::read_to_string(root.join(rel)) else {
            continue;
        };
        let head = "sandInferenceTransport:this.transportFactory.createTransport({baseUrl:\"";
        if let Some(i) = t.find(head) {
            let rest = &t[i..];
            let end = rest[head.len()..].find('"').unwrap() + head.len() + 1;
            out.push_str(&rest[..end]);
            out.push('\n');
        }
    }
    out
}

/// 与 `push_script` 行为等价：先备份原文件（只第一次），再解包覆盖。
fn push(host: &FakeHost, script: &str, tar_gz: &[u8]) {
    let root = host.server_root();
    let bak = host.backup_root();
    let rels: Vec<&str> = script
        .lines()
        .find(|l| l.starts_with("for rel in "))
        .unwrap()
        .trim_start_matches("for rel in ")
        .trim_end_matches("; do")
        .split(' ')
        .map(|q| q.trim_matches('\''))
        .collect();
    for rel in &rels {
        let dst = bak.join(rel);
        if !dst.exists() {
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            std::fs::copy(root.join(rel), &dst).unwrap();
        }
    }
    let tmp = tempfile::tempdir_in(&root).unwrap();
    let mut x = Command::new("tar")
        .arg("xzf")
        .arg("-")
        .arg("-C")
        .arg(tmp.path())
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    std::io::Write::write_all(x.stdin.as_mut().unwrap(), tar_gz).unwrap();
    drop(x.stdin.take());
    assert!(x.wait().unwrap().success());
    for rel in &rels {
        std::fs::rename(tmp.path().join(rel), root.join(rel)).unwrap();
    }
}

/// 与 `restore_script` 行为等价。
fn restore(host: &FakeHost) -> String {
    let root = host.server_root();
    let bak = host.backup_root();
    if !bak.is_dir() {
        return String::new();
    }
    let mut out = String::new();
    for entry in walkdir(&bak) {
        let rel = entry.strip_prefix(&bak).unwrap();
        std::fs::copy(&entry, root.join(rel)).unwrap();
        out.push_str(&format!("{}\n", rel.display()));
    }
    std::fs::remove_dir_all(&bak).unwrap();
    out
}

fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walkdir(&p));
        } else {
            out.push(p);
        }
    }
    out.sort();
    out
}

fn copy_tree(src: &Path, dst: &Path) {
    for p in walkdir(src) {
        let rel = p.strip_prefix(src).unwrap();
        let d = dst.join(rel);
        std::fs::create_dir_all(d.parent().unwrap()).unwrap();
        std::fs::copy(&p, &d).unwrap();
    }
}

fn setup() -> Option<(tempfile::TempDir, Arc<FakeHost>, RemoteSand)> {
    let mirror = mirror()?;
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let host = Arc::new(FakeHost {
        home: home.clone(),
        restarts: Mutex::new(0),
    });
    copy_tree(&mirror, &host.server_root());
    let sand = RemoteSand::new(
        tmp.path().join("data"),
        Arc::new(FakeSsh { host: host.clone() }),
        Some(COMMIT.into()),
    );
    Some((tmp, host, sand))
}

#[test]
#[ignore]
fn status_on_a_pristine_server_is_stock_and_selects_the_matching_commit() {
    let Some((_tmp, _host, sand)) = setup() else {
        eprintln!("没有 SAND_SERVER_MIRROR，跳过");
        return;
    };
    let st = sand.status("box").unwrap();
    assert_eq!(st.servers.len(), 1);
    assert_eq!(st.selected.as_ref().unwrap().commit, COMMIT);
    assert!(st.commit_matches_local);
    assert_eq!(
        st.selected.as_ref().unwrap().version,
        SUPPORTED_CURSOR_VERSION
    );
    assert!(st.version_supported);
    assert_eq!(st.markers.total(), 0);
    assert!(st.patched_files.is_empty());
    assert!(st.inference_endpoint.is_none());
    assert!(!st.complete);
}

#[test]
#[ignore]
fn install_then_uninstall_round_trips_the_real_server_bundle() {
    let Some((_tmp, host, sand)) = setup() else {
        eprintln!("没有 SAND_SERVER_MIRROR，跳过");
        return;
    };
    let root = host.server_root();
    let before: Vec<(PathBuf, Vec<u8>)> = walkdir(&root)
        .into_iter()
        .map(|p| (p.clone(), std::fs::read(&p).unwrap()))
        .collect();

    let options = InstallOptions {
        inference_endpoint: Some("http://127.0.0.1:8790".into()),
        ..InstallOptions::default()
    };
    let steps = Mutex::new(Vec::new());
    let out = sand
        .install("box", options, &|p| {
            steps.lock().unwrap().push(format!("{:?}", p.step))
        })
        .unwrap();
    let steps = steps.into_inner().unwrap();

    assert!(out.wrote);
    assert_eq!(out.commit, COMMIT);
    assert!(out.backup_id.is_some(), "本地这一侧也要留备份");
    assert!(out.server_restarted);
    assert_eq!(*host.restarts.lock().unwrap(), 1);
    // 会变几个文件随版本走（3.18.9 是 4 个含 always-local main.js，3.19.13 是 3 个），这里不钉死
    // 数字，只钉「改了的就是 status 报出来的那几个」和「server 的 product.json 没被碰」。
    let written = out.files_written as usize;
    assert!(written >= 3, "steps={steps:?}");
    assert!(
        !std::fs::read_to_string(root.join("product.json"))
            .unwrap()
            .contains("nexus"),
        "server 的 product.json 没有 checksums，不该被碰"
    );

    // 装完的状态：server profile 完整、端点在。
    let st = &out.status;
    assert!(st.complete, "markers={:?}", st.markers);
    assert_eq!(
        st.inference_endpoint.as_deref(),
        Some("http://127.0.0.1:8790")
    );
    assert_eq!(
        RuleId::ClientType.get(&st.markers),
        RuleId::ClientType
            .expected_for(LayoutProfile::Server)
            .unwrap()
    );
    assert_eq!(st.markers.inference_endpoint, 2);
    assert_eq!(st.patched_files.len(), written);

    // 远程侧的字节真的变了，而且 agent-host 的 main.js 里有端点（3.19.13 起 transport 装配处在这里）。
    const HOST_MAIN: &str = "extensions/cursor-agent-host/dist/main.js";
    let host_main = std::fs::read_to_string(root.join(HOST_MAIN)).unwrap();
    assert!(host_main.contains(SAND_INFERENCE_ENDPOINT_MARKER));
    assert!(host_main.contains("baseUrl:\"http://127.0.0.1:8790\""));
    // 远程备份里是原版字节。
    let backed_up = std::fs::read(host.backup_root().join(HOST_MAIN)).unwrap();
    let original = &before
        .iter()
        .find(|(p, _)| p.ends_with(HOST_MAIN))
        .unwrap()
        .1;
    assert_eq!(&backed_up, original);

    // 重装：无事发生，也不会把远程备份覆盖成补丁后的内容。
    let again = sand
        .install(
            "box",
            InstallOptions {
                inference_endpoint: Some("http://127.0.0.1:8790".into()),
                ..InstallOptions::default()
            },
            &|_| {},
        )
        .unwrap();
    assert!(!again.wrote);
    assert_eq!(
        std::fs::read(host.backup_root().join(HOST_MAIN)).unwrap(),
        **original
    );

    // 换端口重装：盘上的旧端点要**原地迁移**成新的，而不是「无事发生」。2026-09-08 真机：
    // 远程 bundle 里留着 8688，界面说重装成功，Agent 每一发 ECONNREFUSED 127.0.0.1:8688——
    // 远程那条路当时只按选项组规则表，不知道盘上装着什么。
    let moved = sand
        .install(
            "box",
            InstallOptions {
                inference_endpoint: Some("http://127.0.0.1:41777".into()),
                ..InstallOptions::default()
            },
            &|_| {},
        )
        .unwrap();
    assert!(moved.wrote, "换端口必须真写");
    assert_eq!(
        moved.status.inference_endpoint.as_deref(),
        Some("http://127.0.0.1:41777")
    );
    assert_eq!(moved.status.markers.inference_endpoint, 2);
    let host_main = std::fs::read_to_string(root.join(HOST_MAIN)).unwrap();
    assert!(host_main.contains("baseUrl:\"http://127.0.0.1:41777\""));
    assert!(!host_main.contains("127.0.0.1:8790"), "旧端点要一处不剩");
    // 远程备份仍是原版字节（不是第一次补丁后的内容）。
    assert_eq!(
        std::fs::read(host.backup_root().join(HOST_MAIN)).unwrap(),
        **original
    );

    // 关掉改道重装：端点两处都剥掉，其余补丁不动。
    let stripped = sand
        .install("box", InstallOptions::default(), &|_| {})
        .unwrap();
    assert!(stripped.wrote);
    assert!(stripped.status.complete);
    assert!(stripped.status.inference_endpoint.is_none());
    assert_eq!(stripped.status.markers.inference_endpoint, 0);

    // 卸载 = 用远程备份逐字节还原。
    let un = sand.uninstall("box", &|_| {}).unwrap();
    assert!(un.wrote);
    assert_eq!(un.files_written as usize, written);
    assert!(!host.backup_root().exists(), "还原后远程备份目录应清掉");
    for (p, bytes) in &before {
        assert_eq!(
            &std::fs::read(p).unwrap(),
            bytes,
            "{} 没回到原样",
            p.display()
        );
    }
    assert_eq!(un.status.markers.total(), 0);
    assert!(un.status.inference_endpoint.is_none());
}

#[test]
#[ignore]
fn install_without_endpoint_is_allowed_and_leaves_inference_untouched() {
    let Some((_tmp, host, sand)) = setup() else {
        eprintln!("没有 SAND_SERVER_MIRROR，跳过");
        return;
    };
    let out = sand
        .install("box", InstallOptions::default(), &|_| {})
        .unwrap();
    assert!(out.wrote);
    assert!(
        out.status.complete,
        "markers={:?} patched={:?}",
        out.status.markers, out.status.patched_files
    );
    assert!(out.status.inference_endpoint.is_none());
    assert_eq!(out.status.markers.inference_endpoint, 0);
    let host_main = std::fs::read_to_string(
        host.server_root()
            .join("extensions/cursor-agent-host/dist/main.js"),
    )
    .unwrap();
    assert!(!host_main.contains(SAND_INFERENCE_ENDPOINT_MARKER));
}

#[test]
#[ignore]
fn foreign_markers_on_the_remote_refuse_install() {
    let Some((_tmp, host, sand)) = setup() else {
        eprintln!("没有 SAND_SERVER_MIRROR，跳过");
        return;
    };
    // 别人的工具留下的 marker
    let p = host
        .server_root()
        .join("extensions/cursor-agent-host/dist/4884.js");
    let mut t = std::fs::read_to_string(&p).unwrap();
    t.push_str("/*XYZ_SAND_CLIENT_V1*/");
    std::fs::write(&p, t).unwrap();
    let err = sand
        .install("box", InstallOptions::default(), &|_| {})
        .unwrap_err();
    assert_eq!(err.code, nexus_core::ErrorCode::SandForeignMarkers);
    assert_eq!(
        *host.restarts.lock().unwrap(),
        0,
        "预检失败不能去重启远程 server"
    );
}
