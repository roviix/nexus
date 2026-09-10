//! 对本机真实安装的 Cursor 做**只读**冒烟：定位、预检锚点、inspect、以及规则表的逐字节往返。
//! 不写任何文件。
//!
//! 默认 `#[ignore]`：CI 机器上没有 Cursor。本地跑：
//!     cargo test -p nexus-sand --test live_cursor -- --ignored --nocapture

use nexus_sand::engine;
use nexus_sand::model::{InstallOptions, SUPPORTED_CURSOR_VERSION};
use nexus_sand::rules::{self, RuleId};
use nexus_sand::{MarkerCounts, SandLayout};
use std::path::{Path, PathBuf};

/// 默认打本机安装。`SAND_LIVE_APP` 可以指向一份**摆成 `.app` 形状的**别的 bundle——升级适配时
/// 本机往往还停在旧版（补丁装着、故意不让它自动更新），新版只有一份从官方 dmg 抽出来的副本，
/// 金标准得能对着那一份跑。
fn live_app() -> Option<PathBuf> {
    let p = std::env::var("SAND_LIVE_APP")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/Applications/Cursor.app"));
    p.exists().then_some(p)
}

#[test]
#[ignore = "需要本机装有 Cursor"]
fn layout_resolves_all_expected_targets_on_this_machine() {
    let Some(app) = live_app() else { return };
    let l = SandLayout::from_app(&app).expect("layout");
    eprintln!(
        "Cursor {} · {} 个目标 · ext_host={}",
        l.version,
        l.targets.len(),
        l.ext_host.is_some()
    );
    for t in &l.targets {
        eprintln!("  - {}", l.relative(t));
    }
    if l.version == SUPPORTED_CURSOR_VERSION {
        // 适配版本上，全部目标都应存在（3.19.13 起是 10 个：9909.js 那块已并进 main.js）。
        assert_eq!(
            l.targets.len(),
            nexus_sand::layout::TARGET_SPECS.len(),
            "适配版本上目标文件应齐全"
        );
        assert!(l.ext_host.is_some());
    }
}

#[test]
#[ignore = "需要本机装有 Cursor"]
fn preflight_anchors_hit_exactly_once_on_supported_version() {
    let Some(app) = live_app() else { return };
    let l = SandLayout::from_app(&app).expect("layout");
    if l.version != SUPPORTED_CURSOR_VERSION {
        eprintln!("本机 {}，非适配版本，跳过锚点断言", l.version);
        return;
    }
    let contents: Vec<String> = l
        .targets
        .iter()
        .map(|t| std::fs::read_to_string(t).expect("read"))
        .collect();
    for a in rules::preflight_anchors() {
        let n = a.count(contents.iter().map(String::as_str));
        eprintln!("preflight {:24} hits={n} expect={}", a.name, a.expect);
        assert_eq!(n, a.expect, "预检锚点「{}」", a.name);
    }
}

#[test]
#[ignore = "需要本机装有 Cursor"]
fn inspect_reports_marker_counts_without_panicking() {
    let Some(app) = live_app() else { return };
    let l = SandLayout::from_app(&app).expect("layout");
    let rules = rules::catalog(&InstallOptions::default()).unwrap_or_default();
    let mut total = nexus_sand::MarkerCounts::default();
    let mut foreign = 0;
    let mut legacy = 0;
    for t in &l.targets {
        let c = std::fs::read_to_string(t).expect("read");
        let ins = engine::inspect(&c, &rules);
        total = total.plus(&ins.markers);
        foreign += ins.foreign;
        legacy += ins.legacy;
    }
    eprintln!(
        "markers total={} foreign={foreign} legacy={legacy}",
        total.total()
    );
    eprintln!("{total:#?}");
}

/// 规则表的金标准：本机 Cursor 已由 Python 安装器（fixed.4，默认选项）装过补丁。对每个目标文件：
/// `remove(盘上内容)` 得到干净基线 → `apply(基线)` 必须**逐字节**等于盘上内容（Rust 复现 Python
/// 的安装结果）→ `remove(apply(基线))` 逐字节回到基线。同时 `inspect` 的 16 项计数等于 `expected()`。
///
/// 只在内存里算，不写盘。若盘上是用非默认选项装的（自动摘要关 —— v1.2.6.7 之前的旧默认 / 放行
/// 档位非 agent），金标准会在 4884.js 上不等——那不是规则表的错，看打印出来的差异位置就能判断
/// （自摘要开关只差 `supportsSelfSummary:!0` / `!1` 两个字符；3.19.7 起锚点在这个 chunk 上）。
#[test]
#[ignore = "需要本机装有 Cursor"]
fn apply_remove_round_trip_matches_python_install() {
    let Some(app) = live_app() else { return };
    let l = SandLayout::from_app(&app).expect("layout");
    if l.version != SUPPORTED_CURSOR_VERSION {
        eprintln!("本机 {}，非适配版本，跳过往返验收", l.version);
        return;
    }
    // 自摘要开关按盘上实际装的来建规则表：金标准比的是「规则表能否复现这份安装」，
    // 不该因为默认值翻转（v1.2.6.7 起自摘要默认开）就在 4884.js 上不等。
    let contents: Vec<String> = l
        .targets
        .iter()
        .filter_map(|t| std::fs::read_to_string(t).ok())
        .collect();
    let on_disk_self_summary = contents
        .iter()
        .find_map(|c| rules::installed_self_summary(c));
    if on_disk_self_summary.is_none() {
        // 盘上是原版（例如 SAND_LIVE_APP 指着一份刚从官方 dmg 抽出来的新版副本）。金标准的问法是
        // 「Rust 能不能复现盘上这份 Python 安装」，没安装就无从比起——那种情形改看
        // `apply_matches_python_reference_in_memory`（它自己在内存里让 Python 装一遍）。
        eprintln!("盘上没有补丁（原版 bundle），跳过金标准；改跑 apply_matches_python_reference_in_memory");
        return;
    }
    let options = InstallOptions {
        self_summary: on_disk_self_summary.unwrap_or(InstallOptions::default().self_summary),
        ..InstallOptions::default()
    };
    eprintln!(
        "盘上自摘要开关：{on_disk_self_summary:?}，按 self_summary={} 建规则表",
        options.self_summary
    );
    let rules = rules::catalog(&options).expect("catalog");

    let mut removed_total = MarkerCounts::default();
    let mut hits_total = MarkerCounts::default();
    let mut markers_total = MarkerCounts::default();
    let mut remaining_ide = 0;
    let mut golden_mismatches: Vec<String> = Vec::new();
    let mut round_trip_mismatches: Vec<String> = Vec::new();
    let mut legacy_on_disk = 0u32;

    for t in &l.targets {
        let rel = l.relative(t);
        let disk = std::fs::read_to_string(t).expect("read");
        // 盘上装的是不是旧版补丁（例如 Python 上一版的 task tool V4）。是的话「金标准」
        // 不能要求 apply(基线) 逐字节等于盘上内容——盘上就是旧版；改为要求**迁移等价**：
        // 对盘上旧版原地 apply（走 legacy 迁移）得到的字节，必须与对干净基线全新 apply 的字节
        // 完全一致。这比金标准弱一点，但它在真实字节上验证了迁移路径。
        let disk_legacy = engine::inspect(&disk, &rules).legacy;
        legacy_on_disk += disk_legacy;

        // ① 盘 → 基线：一个 marker 都不能剩。
        let (baseline, removed) = engine::remove(&disk, &rules);
        let ins0 = engine::inspect(&baseline, &rules);
        assert_eq!(
            (ins0.markers.total(), ins0.legacy, ins0.foreign),
            (0, 0, 0),
            "{rel}: remove(disk) 之后仍有标记 {:?}",
            ins0
        );
        removed_total = removed_total.plus(&removed);

        // ② 基线 → 打补丁：计数与 marker 一致。
        let (patched, report) = engine::apply(&baseline, &rules);
        assert_eq!(report.migrated.total(), 0, "{rel}: 干净基线上不该出现迁移");
        let ins1 = engine::inspect(&patched, &rules);
        hits_total = hits_total.plus(&report.hits);
        markers_total = markers_total.plus(&ins1.markers);
        remaining_ide += ins1.remaining_ide;

        // ③ 金标准：与 Python 装在盘上的结果逐字节相等；盘上是旧版时退为迁移等价。
        let (golden, golden_label, golden_lhs_name, golden_lhs) = if disk_legacy == 0 {
            (patched == disk, "golden", "盘上(Python)", disk.clone())
        } else {
            let (migrated, _) = engine::apply(&disk, &rules);
            (migrated == patched, "migrate", "apply(盘上旧版)", migrated)
        };
        // ④ 再卸回去：逐字节等于基线。
        let (back, _) = engine::remove(&patched, &rules);
        let round_trip = back == baseline;

        eprintln!(
            "{rel:<58} removed={:>2} hits={:>2} markers={:>2} {golden_label}={} round_trip={}",
            removed.total(),
            report.hits.total(),
            ins1.markers.total(),
            if golden { "OK  " } else { "DIFF" },
            if round_trip { "OK" } else { "DIFF" },
        );
        if !golden {
            golden_mismatches.push(describe_diff(
                &rel,
                golden_lhs_name,
                &golden_lhs,
                "Rust apply(基线)",
                &patched,
            ));
        }
        if !round_trip {
            round_trip_mismatches.push(describe_diff(
                &rel,
                "基线",
                &baseline,
                "remove(apply)",
                &back,
            ));
        }
    }

    eprintln!("remaining_ide={remaining_ide}");
    if legacy_on_disk > 0 {
        eprintln!(
            "注意：盘上有 {legacy_on_disk} 处旧版 marker，本轮用「迁移等价」代替金标准；\
             重新 install 后金标准会恢复为与盘上逐字节比对。"
        );
    }
    eprintln!("removed  {removed_total:?}");
    eprintln!("hits     {hits_total:?}");
    eprintln!("markers  {markers_total:?}");
    for id in RuleId::ALL {
        let Some(want) = id.expected() else { continue };
        assert_eq!(id.get(&markers_total), want, "{} markers", id.name());
        assert_eq!(id.get(&hits_total), want, "{} hits", id.name());
        assert_eq!(id.get(&removed_total), want, "{} removed", id.name());
    }
    assert_eq!(markers_total.eligibility, 0, "3.19.x 上 eligibility 应为 0");
    assert_eq!(remaining_ide, 0);

    for m in &golden_mismatches {
        eprintln!("\n=== 金标准不等 ===\n{m}");
    }
    for m in &round_trip_mismatches {
        eprintln!("\n=== 往返不等 ===\n{m}");
    }
    assert!(
        round_trip_mismatches.is_empty(),
        "{} 个文件 remove(apply(基线)) != 基线",
        round_trip_mismatches.len()
    );
    assert!(
        golden_mismatches.is_empty(),
        "{} 个文件 Rust apply 与盘上 Python 安装结果不等",
        golden_mismatches.len()
    );
}

/// 不依赖盘上装的是哪一版：让 Python 参考实现在内存里对同一份基线做 `apply(remove(disk))`，
/// 与 Rust 的结果逐字节比。规则表改了、还没重新 install 时，用这个代替上面的盘上金标准。
/// 需要本机有 `python3`（只 import 安装器模块，不执行任何写盘命令）。
#[test]
#[ignore = "需要本机装有 Cursor 与 python3"]
fn apply_matches_python_reference_in_memory() {
    let Some(app) = live_app() else { return };
    let l = SandLayout::from_app(&app).expect("layout");
    if l.version != SUPPORTED_CURSOR_VERSION {
        eprintln!("本机 {}，非适配版本，跳过", l.version);
        return;
    }
    // 自摘要开 / 关各比一遍：两种都会把整段 Direct 注入体写进 4884.js，
    // Python / Rust 的逐字节一致只有这里能验。
    eprintln!("--- 默认选项（自摘要开）---");
    python_reference_matches(&l, &[], &InstallOptions::default());
    eprintln!("--- 自摘要关 ---");
    python_reference_matches(
        &l,
        &[("SAND_ENABLE_SELF_SUMMARY", "0")],
        &InstallOptions {
            self_summary: false,
            ..InstallOptions::default()
        },
    );
}

fn python_reference_matches(l: &SandLayout, env: &[(&str, &str)], options: &InstallOptions) {
    let installer = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../gateway/scripts/sand-stream-installer.py");
    let out_dir = std::env::temp_dir().join(format!(
        "nexus-sand-py-golden-{}-self-summary-{}",
        std::process::id(),
        options.self_summary
    ));
    std::fs::create_dir_all(&out_dir).expect("mkdir");

    // Python：对每个目标文件 remove → apply，按扁平文件名写到 out_dir；默认选项（与 Rust 一致）。
    let script = r#"
import importlib.util, sys, pathlib
inst_path, out_dir, *targets = sys.argv[1:]
spec = importlib.util.spec_from_file_location("inst", inst_path)
m = importlib.util.module_from_spec(spec); sys.modules["inst"] = m; spec.loader.exec_module(m)
for t in targets:
    p = pathlib.Path(t)
    disk = m._decode_js(p.read_bytes(), p)
    base, _ = m.remove_patch_from_content(disk)
    patched, _ = m.apply_patch_to_content(base)
    (pathlib.Path(out_dir) / t.replace("/", "__")).write_text(patched, encoding="utf-8")
"#;
    let mut cmd = std::process::Command::new("python3");
    cmd.arg("-c").arg(script).arg(&installer).arg(&out_dir);
    for t in &l.targets {
        cmd.arg(t);
    }
    // 先把可能污染的环境变量全清掉（默认 = 摘要开、agent 档），再按需覆盖。
    cmd.env_remove("SAND_ENABLE_SELF_SUMMARY")
        .env_remove("SAND_ENABLE_PLAN_MODE")
        .env_remove("SAND_ENABLE_ALL_MODES");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let status = cmd.status().expect("运行 python3");
    assert!(status.success(), "Python 参考实现执行失败");

    let rules = rules::catalog(options).expect("catalog");
    let mut mismatches = Vec::new();
    for t in &l.targets {
        let rel = l.relative(t);
        let disk = std::fs::read_to_string(t).expect("read");
        let (baseline, _) = engine::remove(&disk, &rules);
        let (rust_patched, _) = engine::apply(&baseline, &rules);
        let py_path = out_dir.join(t.to_string_lossy().replace('/', "__"));
        let py_patched = std::fs::read_to_string(&py_path).expect("读 Python 输出");
        let same = rust_patched == py_patched;
        eprintln!(
            "{rel:<58} python==rust: {}",
            if same { "OK" } else { "DIFF" }
        );
        if !same {
            mismatches.push(describe_diff(
                &rel,
                "Python",
                &py_patched,
                "Rust",
                &rust_patched,
            ));
        }
    }
    let _ = std::fs::remove_dir_all(&out_dir);
    for m in &mismatches {
        eprintln!("\n=== Python 与 Rust 不等 ===\n{m}");
    }
    assert!(
        mismatches.is_empty(),
        "{} 个文件 Rust apply 与 Python 不等",
        mismatches.len()
    );
}

/// 第一个不同字节的位置、前后 120 字符、以及附近是哪条规则的 marker。
fn describe_diff(rel: &str, a_name: &str, a: &str, b_name: &str, b: &str) -> String {
    let at = a
        .bytes()
        .zip(b.bytes())
        .position(|(x, y)| x != y)
        .unwrap_or(a.len().min(b.len()));
    let near = nearest_rule(a, at)
        .or_else(|| nearest_rule(b, at))
        .map(|(id, d)| format!("{}（marker 距差异 {d} 字节）", id.name()))
        .unwrap_or_else(|| "附近 2000 字节内没有任何 marker".into());
    format!(
        "{rel}\n  长度 {a_name}={} {b_name}={}；第一个差异在字节 {at}；附近规则：{near}\n  {a_name}: …{}…\n  {b_name}: …{}…",
        a.len(),
        b.len(),
        window(a, at),
        window(b, at),
    )
}

fn nearest_rule(s: &str, at: usize) -> Option<(RuleId, usize)> {
    let lo = at.saturating_sub(2000);
    let hi = (at + 2000).min(s.len());
    let (lo, hi) = (floor_char(s, lo), floor_char(s, hi));
    let hay = &s[lo..hi];
    let mut best: Option<(RuleId, usize)> = None;
    for id in RuleId::ALL {
        for m in id.markers() {
            for (pos, _) in hay.match_indices(m) {
                let abs = lo + pos;
                let d = abs.abs_diff(at);
                if best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((*id, d));
                }
            }
        }
    }
    best
}

fn window(s: &str, at: usize) -> String {
    let lo = floor_char(s, at.saturating_sub(120));
    let hi = floor_char(s, (at + 120).min(s.len()));
    s[lo..hi].replace('\n', "⏎")
}

fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}
