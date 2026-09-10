//! Python 参考实现 ↔ Rust 的常量护栏（docs/SAND.md §8）。
//!
//! `gateway/scripts/sand-stream-installer.py` 是 `rules.rs` / `layout.rs` / `model.rs` 的移植规格
//! 来源。§8 规定两边的 marker 字符串、期望计数、目标文件表、适配版本**必须一致**，Cursor 升级时
//! 先改 Python 验证通过、再同步 Rust。这里把这条约定做成 CI 门：直接读 Python 源码、逐行正则抓
//! 顶层常量、与 Rust 侧对账。不执行 Python，不引新依赖。
//!
//! 所有断言都把两边的实际值打出来；`assert_eq!` 里 **left = Python，right = Rust**。
//!
//! **那两份 Python 脚本在上游的私有仓库里，不随开源发布**，所以这组对拍在公开 CI 上**整组
//! 跳过**（跳过时会打一行说明，不会静悄悄地绿）。手上有上游仓库的话，把 `SAND_PYTHON_REF`
//! 指到它的 `gateway/scripts` 目录，这道门就会真的落下来：
//!
//! ```text
//! SAND_PYTHON_REF=/path/to/upstream/gateway/scripts cargo test -p nexus-sand --test python_parity
//! ```
//!
//! 跳过是有代价的：Cursor 升级时「先改 Python 验证、再同步 Rust」这条约定在公开仓库里就只剩
//! 人工遵守。真值仍然是 `rules.rs` 自己 —— `rust_marker_table_and_rule_markers_cover_each_other`
//! 不依赖 Python，任何时候都跑。

use nexus_sand::layout;
use nexus_sand::rules::{self, RuleId};
use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// 读 Python 源码
// ---------------------------------------------------------------------------

/// 参考实现所在目录，找不到就 `None`。
fn python_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("SAND_PYTHON_REF") {
        let dir = PathBuf::from(dir);
        return dir.is_dir().then_some(dir);
    }
    // 历史位置：本仓库曾是上游 monorepo 的 `desktop/` 子目录，脚本在它的同级 `gateway/` 下。
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../gateway/scripts");
    dir.is_dir().then_some(dir)
}

/// 读一份参考实现；没有就打一行说明并返回 `None`，由调用方跳过。
fn python_file(name: &str) -> Option<String> {
    let Some(dir) = python_dir() else {
        eprintln!(
            "跳过：没有 Sand 的 Python 参考实现。它在上游私有仓库里，\
             把 SAND_PYTHON_REF 指到那边的 gateway/scripts 即可启用这组对拍。"
        );
        return None;
    };
    let path = dir.join(name);
    match std::fs::read_to_string(&path) {
        Ok(src) => Some(src),
        Err(e) => {
            eprintln!("跳过：读不到 {}：{e}", path.display());
            None
        }
    }
}

/// 整份 Python 源码。
fn python_source() -> Option<String> {
    python_file("sand-stream-installer.py")
}

// ---------------------------------------------------------------------------
// 逐行正则解析（只认顶层、无缩进的赋值）
// ---------------------------------------------------------------------------

/// 顶层 `NAME = "…"`（也认 `r"…"` 原始字符串）。
fn py_str(src: &str, name: &str) -> String {
    let re = Regex::new(&format!(
        r#"(?m)^{}\s*=\s*r?"([^"]*)"\s*(?:#.*)?$"#,
        regex::escape(name)
    ))
    .unwrap();
    let caps = re
        .captures(src)
        .unwrap_or_else(|| panic!("Python 里找不到顶层字符串常量 {name}"));
    caps[1].to_string()
}

/// 顶层 `NAME = 123`。
fn py_int(src: &str, name: &str) -> u32 {
    let re = Regex::new(&format!(
        r"(?m)^{}\s*=\s*(\d+)\s*(?:#.*)?$",
        regex::escape(name)
    ))
    .unwrap();
    let caps = re
        .captures(src)
        .unwrap_or_else(|| panic!("Python 里找不到顶层整数常量 {name}"));
    caps[1].parse().unwrap()
}

/// 顶层 `NAME = "a" + "b"`：把拼接的字面量接回去。KC legacy marker 在 Python 里故意拆成两段，
/// 免得脚本自己被当成外部 marker。
fn py_concat_str(src: &str, name: &str) -> String {
    let re = Regex::new(&format!(r"(?m)^{}\s*=\s*(.+)$", regex::escape(name))).unwrap();
    let caps = re
        .captures(src)
        .unwrap_or_else(|| panic!("Python 里找不到顶层常量 {name}"));
    let rhs = caps[1].to_string();
    Regex::new(r#""([^"]*)""#)
        .unwrap()
        .captures_iter(&rhs)
        .map(|c| c[1].to_string())
        .collect()
}

/// 全部顶层 `SAND_<X>_MARKER = "…"`（`LEGACY_` 前缀的不在此列），按出现顺序。
fn py_sand_markers(src: &str) -> Vec<(String, String)> {
    Regex::new(r#"(?m)^(SAND_[A-Z0-9_]+_MARKER)\s*=\s*"([^"]*)"\s*(?:#.*)?$"#)
        .unwrap()
        .captures_iter(src)
        .map(|c| (c[1].to_string(), c[2].to_string()))
        .collect()
}

/// `LEGACY_SAND_MANAGED_TASK_TOOL_MARKER`、`…_V2`、`…_V3` 等全部旧版 task tool marker。
fn py_legacy_task_tool_markers(src: &str) -> BTreeSet<String> {
    Regex::new(r#"(?m)^LEGACY_SAND_MANAGED_TASK_TOOL_MARKER[A-Z0-9_]*\s*=\s*"([^"]*)""#)
        .unwrap()
        .captures_iter(src)
        .map(|c| c[1].to_string())
        .collect()
}

/// `TARGET_SPECS … = (` 那一行起、到顶层 `)` 收尾的整段。元组跨多行，有的条目还拆成两行，
/// 所以先整段抓出来再从里面挑字面量。
fn py_target_specs_block(src: &str) -> String {
    let mut lines = src.lines().skip_while(|l| !l.starts_with("TARGET_SPECS"));
    let head = lines.next().expect("Python 里找不到顶层 TARGET_SPECS");
    let mut block = vec![head];
    for line in lines {
        block.push(line);
        if line.trim_end() == ")" {
            return block.join("\n");
        }
    }
    panic!("Python 的 TARGET_SPECS 没有顶层 `)` 收尾");
}

/// 目标相对路径（整段里所有含 `/` 的字符串字面量），按出现顺序。
fn py_target_paths(block: &str) -> Vec<String> {
    Regex::new(r#""([^"]*)""#)
        .unwrap()
        .captures_iter(block)
        .map(|c| c[1].to_string())
        .filter(|s| s.contains('/'))
        .collect()
}

/// `(path, extension)` 二元组，按出现顺序；Python 的 `None` 对应 Rust 的 `None`。
fn py_target_specs(block: &str) -> Vec<(String, Option<String>)> {
    Regex::new(r#"\(\s*"([^"]+)"\s*,\s*(?:None|"([^"]*)")\s*,?\s*\)"#)
        .unwrap()
        .captures_iter(block)
        .map(|c| (c[1].to_string(), c.get(2).map(|m| m.as_str().to_string())))
        .collect()
}

// ---------------------------------------------------------------------------
// Rust 侧的 marker 表
// ---------------------------------------------------------------------------

/// Rust 没有反射，`rules::SAND_*` 只能手抄一份 `(Python 常量名, 值)`：19 个 `RuleId` 各一个，
/// 加 client-type 接管已有安装用的 `SAND_CLIENT_EXISTING_MARKER`。已下线 Session 引擎的空 marker
/// 两边都带 `LEGACY_` 前缀，在 [`legacy_kc_markers_and_guard_patterns_match_python`] 里单独对。
/// `rust_marker_table_and_rule_markers_cover_each_other` 保证这张表不会与 `RuleId::markers()` 脱节。
const RUST_SAND_MARKERS: &[(&str, &str)] = &[
    ("SAND_CLIENT_MARKER", rules::SAND_CLIENT_MARKER),
    (
        "SAND_CLIENT_EXISTING_MARKER",
        rules::SAND_CLIENT_EXISTING_MARKER,
    ),
    ("SAND_ELIGIBILITY_MARKER", rules::SAND_ELIGIBILITY_MARKER),
    (
        "SAND_MANAGED_LOCAL_ROUTE_MARKER",
        rules::SAND_MANAGED_LOCAL_ROUTE_MARKER,
    ),
    (
        "SAND_DIRECT_STREAM_MARKER",
        rules::SAND_DIRECT_STREAM_MARKER,
    ),
    (
        "SAND_AGENT_HOST_ENABLEMENT_MARKER",
        rules::SAND_AGENT_HOST_ENABLEMENT_MARKER,
    ),
    (
        "SAND_LOCAL_RUNTIME_LOAD_MARKER",
        rules::SAND_LOCAL_RUNTIME_LOAD_MARKER,
    ),
    (
        "SAND_AGENT_HOST_IDENTITY_MARKER",
        rules::SAND_AGENT_HOST_IDENTITY_MARKER,
    ),
    (
        "SAND_AGENT_HOST_MOVE_EXEC_MARKER",
        rules::SAND_AGENT_HOST_MOVE_EXEC_MARKER,
    ),
    (
        "SAND_MANAGED_SUBAGENT_ROUTE_MARKER",
        rules::SAND_MANAGED_SUBAGENT_ROUTE_MARKER,
    ),
    (
        "SAND_MANAGED_SUBAGENT_SESSION_MARKER",
        rules::SAND_MANAGED_SUBAGENT_SESSION_MARKER,
    ),
    (
        "SAND_MANAGED_TASK_TOOL_MARKER",
        rules::SAND_MANAGED_TASK_TOOL_MARKER,
    ),
    (
        "SAND_MANAGED_ACTION_ROUTE_MARKER",
        rules::SAND_MANAGED_ACTION_ROUTE_MARKER,
    ),
    (
        "SAND_SUBAGENT_RESUME_MODE_MARKER",
        rules::SAND_SUBAGENT_RESUME_MODE_MARKER,
    ),
    (
        "SAND_SUBAGENT_COMPLETION_WAKE_MARKER",
        rules::SAND_SUBAGENT_COMPLETION_WAKE_MARKER,
    ),
    (
        "SAND_SUBAGENT_INTERACTION_BUBBLE_MARKER",
        rules::SAND_SUBAGENT_INTERACTION_BUBBLE_MARKER,
    ),
    (
        "SAND_SUBAGENT_MODEL_VARIANTS_MARKER",
        rules::SAND_SUBAGENT_MODEL_VARIANTS_MARKER,
    ),
    (
        "SAND_CONTEXT_WINDOW_MARKER",
        rules::SAND_CONTEXT_WINDOW_MARKER,
    ),
    (
        "SAND_GROK_BOX_RELAY_AUTH_MARKER",
        rules::SAND_GROK_BOX_RELAY_AUTH_MARKER,
    ),
    (
        "SAND_GROKBOT_DIRECT_AUTH_MARKER",
        rules::SAND_GROKBOT_DIRECT_AUTH_MARKER,
    ),
];

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[test]
fn supported_cursor_version_matches_python() {
    let Some(src) = python_source() else { return };
    assert_eq!(
        py_str(&src, "SUPPORTED_CURSOR_VERSION"),
        nexus_sand::SUPPORTED_CURSOR_VERSION,
        "适配的 Cursor 版本不一致（left = Python，right = Rust）"
    );
}

#[test]
fn sand_marker_constants_match_python_by_name_and_value() {
    let Some(src) = python_source() else { return };
    let py = py_sand_markers(&src);

    // 双向对账，一次把全部漂移列出来，而不是撞到第一个就停。
    let mut drift = Vec::new();
    for (name, py_val) in &py {
        let (name, py_val) = (name.as_str(), py_val.as_str());
        match RUST_SAND_MARKERS.iter().find(|&&(n, _)| n == name) {
            None => drift.push(format!("Python 有 {name} = {py_val:?}，Rust 没有同名常量")),
            Some(&(_, rs_val)) if rs_val != py_val => {
                drift.push(format!("{name}：Python {py_val:?} ≠ Rust {rs_val:?}"));
            }
            Some(_) => {}
        }
    }
    for &(name, rs_val) in RUST_SAND_MARKERS {
        if !py.iter().any(|(n, _)| n == name) {
            drift.push(format!("Rust 有 {name} = {rs_val:?}，Python 没有同名常量"));
        }
    }
    assert!(
        drift.is_empty(),
        "SAND_*_MARKER 常量漂移（{} 处）：\n  {}",
        drift.len(),
        drift.join("\n  ")
    );

    // 旧版 task tool marker：Python 三个独立常量 ↔ Rust 一个切片，按集合比。
    let rust_legacy: BTreeSet<String> = rules::LEGACY_TASK_TOOL_MARKERS
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert_eq!(
        py_legacy_task_tool_markers(&src),
        rust_legacy,
        "旧版 task tool marker 集合不一致（left = Python，right = Rust）"
    );
}

#[test]
fn legacy_kc_markers_and_guard_patterns_match_python() {
    let Some(src) = python_source() else { return };
    assert_eq!(
        py_concat_str(&src, "LEGACY_SAND_CLIENT_MARKER"),
        rules::LEGACY_KC_CLIENT_MARKER,
        "KC 旧版 client marker 不一致（left = Python，right = Rust）"
    );
    assert_eq!(
        py_concat_str(&src, "LEGACY_SAND_ELIGIBILITY_MARKER"),
        rules::LEGACY_KC_ELIGIBILITY_MARKER,
        "KC 旧版 eligibility marker 不一致（left = Python，right = Rust）"
    );
    assert_eq!(
        py_str(&src, "LEGACY_SAND_SESSION_STREAM_MARKER"),
        rules::LEGACY_SESSION_STREAM_MARKER,
        "已下线 Session 引擎的空 marker 不一致（left = Python，right = Rust）"
    );
    assert_eq!(
        py_str(&src, "CLIENT_MARKER_GUARD_PATTERN"),
        rules::CLIENT_MARKER_GUARD_PATTERN,
        "外部 client marker 探测正则不一致（left = Python，right = Rust）"
    );
    assert_eq!(
        py_str(&src, "ELIGIBILITY_MARKER_GUARD_PATTERN"),
        rules::ELIGIBILITY_MARKER_GUARD_PATTERN,
        "外部 eligibility marker 探测正则不一致（left = Python，right = Rust）"
    );
}

#[test]
fn expected_marker_counts_match_python() {
    let Some(src) = python_source() else { return };

    let client = py_int(&src, "EXPECTED_CLIENT_MARKERS");
    assert_eq!(
        client,
        rules::EXPECTED_CLIENT_MARKERS,
        "EXPECTED_CLIENT_MARKERS 不一致（left = Python，right = Rust）"
    );
    assert_eq!(
        Some(client),
        RuleId::ClientType.expected(),
        "EXPECTED_CLIENT_MARKERS 与 RuleId::ClientType.expected() 不一致（left = Python，right = Rust）"
    );
    assert_eq!(
        Some(py_int(&src, "EXPECTED_AGENT_HOST_ENABLEMENT_MARKERS")),
        RuleId::AgentHostEnablement.expected(),
        "EXPECTED_AGENT_HOST_ENABLEMENT_MARKERS 与 RuleId::AgentHostEnablement.expected() 不一致（left = Python，right = Rust）"
    );
    assert_eq!(
        Some(py_int(&src, "EXPECTED_BACKGROUND_COMPLETION_WAKE_MARKERS")),
        RuleId::SubagentCompletionWake.expected(),
        "EXPECTED_BACKGROUND_COMPLETION_WAKE_MARKERS 与 RuleId::SubagentCompletionWake.expected() 不一致（left = Python，right = Rust）"
    );
    assert_eq!(
        Some(py_int(&src, "EXPECTED_SUBAGENT_MODEL_VARIANTS_MARKERS")),
        RuleId::SubagentModelVariants.expected(),
        "EXPECTED_SUBAGENT_MODEL_VARIANTS_MARKERS 与 RuleId::SubagentModelVariants.expected() 不一致（left = Python，right = Rust）"
    );
}

#[test]
fn target_specs_match_python_in_order() {
    let Some(src) = python_source() else { return };
    let block = py_target_specs_block(&src);

    // 路径列表，顺序也要一致（备份键、界面顺序都按它来）。
    let py_paths = py_target_paths(&block);
    let rust_paths: Vec<String> = layout::TARGET_SPECS
        .iter()
        .map(|&(p, _)| p.to_string())
        .collect();
    assert_eq!(
        py_paths, rust_paths,
        "TARGET_SPECS 目标路径不一致（含顺序；left = Python，right = Rust）"
    );

    // 扩展名列：决定哪些文件改完要同步 extensionHostProcess.js 里的内嵌 hash。
    let py_specs = py_target_specs(&block);
    let rust_specs: Vec<(String, Option<String>)> = layout::TARGET_SPECS
        .iter()
        .map(|&(p, e)| (p.to_string(), e.map(str::to_string)))
        .collect();
    assert_eq!(
        py_specs, rust_specs,
        "TARGET_SPECS 的 (路径, 扩展名) 表不一致（left = Python，right = Rust）"
    );

    assert_eq!(
        py_str(&src, "EXT_HOST_REL"),
        layout::EXT_HOST_REL,
        "EXT_HOST_REL 不一致（left = Python，right = Rust）"
    );
}

/// 推理端点改道只存在于 remote server 那条路，desktop 安装器（本文件对账的那个 Python）里没有
/// 它的对应物，它的 Python 参考实现是同目录的 `sand-remote-server.py`。所以这两个 marker 不进
/// `RUST_SAND_MARKERS`，改由 [`remote_only_markers_match_the_remote_python_script`] 单独对账。
const REMOTE_ONLY_MARKERS: &[(&str, &str)] = &[
    (
        "SAND_INFERENCE_ENDPOINT_MARKER",
        rules::SAND_INFERENCE_ENDPOINT_MARKER,
    ),
    (
        "SAND_REMOTE_INFERENCE_ROUTE_MARKER",
        rules::SAND_REMOTE_INFERENCE_ROUTE_MARKER,
    ),
];

/// 手抄表的自检：表里的值 + remote 专用的两个 == 全部 `RuleId::markers()` 的并集。
/// 这样 Rust 侧新增 / 改名 marker 而忘了更新上表时，这里会先炸，而不是让 Python 对账悄悄漏项。
#[test]
fn rust_marker_table_and_rule_markers_cover_each_other() {
    let table: BTreeSet<&str> = RUST_SAND_MARKERS
        .iter()
        .chain(REMOTE_ONLY_MARKERS)
        .map(|&(_, v)| v)
        .collect();
    let used: BTreeSet<&str> = RuleId::ALL
        .iter()
        .flat_map(|id| id.markers().iter().copied())
        .collect();
    assert_eq!(
        table, used,
        "手抄的 marker 表与 RuleId::markers() 不一致（left = 表，right = 规则）"
    );
    // 每个 RuleId 一个 marker，另加三处「一个 RuleId 带两个 marker」的：client-type 的
    // 接管已有安装、端点改道的建 transport / 挂路由、Grok 鉴权的 Box Relay / 直连两种形态。
    assert_eq!(
        RUST_SAND_MARKERS.len() + REMOTE_ONLY_MARKERS.len(),
        RuleId::ALL.len() + 3,
        "marker 表条数不对：新增 RuleId 或改了 markers() 时要同步这里"
    );
    assert_eq!(
        table.len(),
        RUST_SAND_MARKERS.len() + REMOTE_ONLY_MARKERS.len(),
        "表里有重复的 marker 值"
    );
}

/// remote 专用 marker 与 `gateway/scripts/sand-remote-server.py` 对账。
///
/// 值必须逐字一致：那个脚本装过的远程机器，desktop 这边要认得出是自己人（而不是判成
/// 「外部工具 marker」而拒绝接管），也要能精确卸载干净。
#[test]
fn remote_only_markers_match_the_remote_python_script() {
    let Some(src) = python_file("sand-remote-server.py") else {
        return;
    };
    for (name, value) in REMOTE_ONLY_MARKERS {
        assert_eq!(
            py_str(&src, name),
            *value,
            "{name} 与 sand-remote-server.py 不一致（left = Python，right = Rust）"
        );
    }
}
