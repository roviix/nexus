//! 补丁引擎：拿一份文件内容和一组规则，算出改后的内容。**纯函数，不碰文件系统。**
//!
//! 它不认识任何具体锚点（那在 `rules`），所以 Cursor 升级时这里一行不用改。
//! 三个入口对应安装器的三个函数：`apply` ≈ `apply_patch_to_content`，
//! `remove` ≈ `remove_patch_from_content`，`inspect` ≈ `inspect_status` 的单文件部分。
//!
//! 不变量（测试钉住）：对任何内容 `remove(apply(x)) == x`，且 `apply(apply(x)) == apply(x)`。

use regex::Regex;

use crate::model::MarkerCounts;
use crate::rules::{
    PatchRule, RuleId, RuleKind, CLIENT_MARKER_GUARD_PATTERN, ELIGIBILITY_MARKER_GUARD_PATTERN,
    LEGACY_KC_CLIENT_MARKER, LEGACY_KC_ELIGIBILITY_MARKER, LEGACY_SESSION_STREAM_MARKER,
    LEGACY_TASK_TOOL_MARKERS,
};

/// `apply` 的结果：新命中了什么、把什么旧版本迁移成了新版本。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub hits: MarkerCounts,
    pub migrated: MarkerCounts,
}

/// 对一份内容打全部补丁。已经打过的位置不会重复打（幂等）。
pub fn apply(content: &str, rules: &[PatchRule]) -> (String, ApplyReport) {
    apply_refs(content, rules.iter())
}

/// [`apply`] 的按引用版本：给「规则表里挑一部分」的调用方用（`PatchRule` 不 Clone，也不该 Clone——
/// 正则那一类编译一次就够了）。
pub fn apply_refs<'a>(
    content: &str,
    rules: impl IntoIterator<Item = &'a PatchRule>,
) -> (String, ApplyReport) {
    let mut out = content.to_string();
    let mut report = ApplyReport::default();
    for rule in rules {
        match &rule.kind {
            RuleKind::Literal {
                original,
                patched,
                legacy,
            } => {
                let n = count(&out, original);
                if n > 0 {
                    out = out.replace(original.as_str(), patched);
                    *rule.id.slot(&mut report.hits) += n;
                } else if !out.contains(patched.as_str()) {
                    for old in legacy {
                        let m = count(&out, old);
                        if m > 0 {
                            out = out.replace(old.as_str(), patched);
                            *rule.id.slot(&mut report.migrated) += m;
                            break;
                        }
                    }
                }
            }
            RuleKind::Regex {
                apply,
                apply_repl,
                skip_if_followed_by,
                skip_file_if_marked,
                per_file_limit,
                ..
            } => {
                if *skip_file_if_marked && rule.id.markers().iter().any(|m| out.contains(m)) {
                    continue;
                }
                let (next, n) = replace_guarded(
                    &out,
                    apply,
                    *apply_repl,
                    skip_if_followed_by.as_ref(),
                    *per_file_limit,
                );
                out = next;
                *rule.id.slot(&mut report.hits) += n;
            }
            RuleKind::AnchoredInsert {
                anchor,
                injection,
                legacy_injections,
            } => {
                // 顺序：当前注入体在 → 幂等；任一旧形态在 → 原地换成当前形态（计 migrated），
                // 这样「改了选项点重新安装」不是静默 no-op；本类 marker 在但两者都不是 →
                // 不认识的变体，不动也不叠加；否则在锚点后插入。
                // 旧形态（legacy_injections）可以带着 marker 表以外的 marker——已下线 Session
                // 引擎的空 marker 就是这样——所以先查 legacy 再查 marker，否则会把注入体叠在它后面。
                if out.contains(injection.as_str()) {
                    continue;
                }
                let migrated = legacy_injections
                    .iter()
                    .find(|old| out.contains(old.as_str()));
                if let Some(old) = migrated {
                    let m = count(&out, old);
                    out = out.replace(old.as_str(), injection);
                    *rule.id.slot(&mut report.migrated) += m;
                } else if rule.id.markers().iter().any(|m| out.contains(m)) {
                    continue;
                } else if out.contains(anchor.as_str()) {
                    out = out.replacen(anchor.as_str(), &format!("{anchor}{injection}"), 1);
                    *rule.id.slot(&mut report.hits) += 1;
                }
            }
        }
    }
    (out, report)
}

/// 把全部补丁（含所有旧版本 / 其它档位的变体）还原。返回移除了多少。
pub fn remove(content: &str, rules: &[PatchRule]) -> (String, MarkerCounts) {
    let mut out = content.to_string();
    let mut removed = MarkerCounts::default();
    for rule in rules {
        match &rule.kind {
            RuleKind::Literal {
                original,
                patched,
                legacy,
            } => {
                for p in std::iter::once(patched).chain(legacy.iter()) {
                    let n = count(&out, p);
                    if n > 0 {
                        out = out.replace(p.as_str(), original);
                        *rule.id.slot(&mut removed) += n;
                    }
                }
            }
            RuleKind::Regex {
                remove: re,
                remove_repl,
                ..
            } => {
                let (next, n) = replace_guarded(&out, re, *remove_repl, None, None);
                out = next;
                *rule.id.slot(&mut removed) += n;
            }
            RuleKind::AnchoredInsert {
                injection,
                legacy_injections,
                ..
            } => {
                for inj in std::iter::once(injection).chain(legacy_injections.iter()) {
                    let n = count(&out, inj);
                    if n > 0 {
                        out = out.replace(inj.as_str(), "");
                        *rule.id.slot(&mut removed) += n;
                    }
                }
            }
        }
    }
    (out, removed)
}

/// 单个文件的只读检查。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileInspection {
    pub markers: MarkerCounts,
    /// client-type 锚点里**还没打上 marker** 的位置数。装完必须为 0。
    pub remaining_ide: u32,
    /// 其它工具的 marker 数。
    pub foreign: u32,
    /// 旧版本 marker 数（会被 install 迁移）。
    pub legacy: u32,
}

impl FileInspection {
    pub fn touched(&self) -> bool {
        self.markers.total() + self.legacy > 0
    }
}

/// 数 marker、数外部 marker、数旧版 marker、数剩余未打的 client-type 位置。
/// `rules` 只用来找 client-type 的正则（数 `remaining_ide`）；传空切片则该项为 0。
pub fn inspect(content: &str, rules: &[PatchRule]) -> FileInspection {
    let mut ins = FileInspection::default();
    for id in RuleId::ALL {
        let n: u32 = id.markers().iter().map(|m| count(content, m)).sum();
        *id.slot(&mut ins.markers) += n;
    }

    let legacy_kc_client = count(content, LEGACY_KC_CLIENT_MARKER);
    let legacy_kc_elig = count(content, LEGACY_KC_ELIGIBILITY_MARKER);
    ins.legacy = legacy_kc_client
        + legacy_kc_elig
        + count(content, LEGACY_SESSION_STREAM_MARKER)
        + LEGACY_TASK_TOOL_MARKERS
            .iter()
            .map(|m| count(content, m))
            .sum::<u32>();

    let client_guard = Regex::new(CLIENT_MARKER_GUARD_PATTERN).expect("static regex");
    let elig_guard = Regex::new(ELIGIBILITY_MARKER_GUARD_PATTERN).expect("static regex");
    let all_client = client_guard.find_iter(content).count() as u32;
    let all_elig = elig_guard.find_iter(content).count() as u32;
    ins.foreign = all_client.saturating_sub(ins.markers.client_type + legacy_kc_client)
        + all_elig.saturating_sub(ins.markers.eligibility + legacy_kc_elig);

    for rule in rules.iter().filter(|r| r.id == RuleId::ClientType) {
        if let RuleKind::Regex {
            apply,
            skip_if_followed_by,
            ..
        } = &rule.kind
        {
            ins.remaining_ide += apply
                .find_iter(content)
                .filter(|m| !followed_by(content, m.end(), skip_if_followed_by.as_ref()))
                .count() as u32;
        }
    }
    ins
}

fn count(haystack: &str, needle: &str) -> u32 {
    if needle.is_empty() {
        return 0;
    }
    haystack.matches(needle).count() as u32
}

fn followed_by(content: &str, at: usize, guard: Option<&Regex>) -> bool {
    match guard {
        Some(g) => g.find_at(content, at).is_some_and(|m| m.start() == at),
        None => false,
    }
}

/// 正则替换，但跳过「匹配结束处紧跟 guard」的位置，并支持替换次数上限。
/// 自己拼输出而不用 `replace_all`：那个拿不到匹配后面的文本，判不了 guard。
fn replace_guarded(
    content: &str,
    re: &Regex,
    repl: fn(&regex::Captures<'_>) -> String,
    guard: Option<&Regex>,
    limit: Option<usize>,
) -> (String, u32) {
    let mut out = String::with_capacity(content.len() + 64);
    let mut last = 0;
    let mut n = 0usize;
    for caps in re.captures_iter(content) {
        let m = caps.get(0).expect("group 0");
        if limit.is_some_and(|l| n >= l) {
            break;
        }
        if followed_by(content, m.end(), guard) {
            continue;
        }
        // 闭包用「原样返回」表示捕获的标识符对不上（Python 的 `\1` 在那种位置根本不匹配）：
        // 不算命中，文本留给下一段一起原样拷出。
        let replacement = repl(&caps);
        if replacement == m.as_str() {
            continue;
        }
        out.push_str(&content[last..m.start()]);
        out.push_str(&replacement);
        last = m.end();
        n += 1;
    }
    out.push_str(&content[last..]);
    (out, n as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::SAND_MANAGED_LOCAL_ROUTE_MARKER;

    fn literal(id: RuleId, original: &str, patched: &str, legacy: &[&str]) -> PatchRule {
        PatchRule {
            id,
            kind: RuleKind::Literal {
                original: original.into(),
                patched: patched.into(),
                legacy: legacy.iter().map(|s| s.to_string()).collect(),
            },
        }
    }

    fn route_rule() -> PatchRule {
        literal(
            RuleId::ManagedLocalRoute,
            "gate?A:B",
            &format!("{SAND_MANAGED_LOCAL_ROUTE_MARKER}A"),
            &[],
        )
    }

    #[test]
    fn literal_apply_then_remove_round_trips() {
        let rules = [route_rule()];
        let src = "x gate?A:B y gate?A:B z";
        let (p, rep) = apply(src, &rules);
        assert_eq!(rep.hits.managed_local_route, 2);
        assert!(!p.contains("gate?A:B"));
        let (back, removed) = remove(&p, &rules);
        assert_eq!(back, src);
        assert_eq!(removed.managed_local_route, 2);
    }

    #[test]
    fn apply_is_idempotent() {
        let rules = [route_rule()];
        let (once, _) = apply("gate?A:B", &rules);
        let (twice, rep) = apply(&once, &rules);
        assert_eq!(once, twice);
        assert_eq!(rep.hits.total(), 0);
    }

    #[test]
    fn literal_migrates_a_legacy_variant_in_place() {
        let rules = [literal(
            RuleId::ManagedTaskTool,
            "props:void 0",
            "props:{/*V4*/}",
            &["props:{/*V2*/}", "props:{/*V3*/}"],
        )];
        let (p, rep) = apply("a props:{/*V3*/} b", &rules);
        assert_eq!(p, "a props:{/*V4*/} b");
        assert_eq!(rep.migrated.managed_task_tool, 1);
        assert_eq!(rep.hits.managed_task_tool, 0);
        // 卸载认全部变体
        let (back, removed) = remove("props:{/*V2*/}|props:{/*V4*/}", &rules);
        assert_eq!(back, "props:void 0|props:void 0");
        assert_eq!(removed.managed_task_tool, 2);
    }

    fn client_rule() -> PatchRule {
        fn to_sand(c: &regex::Captures<'_>) -> String {
            format!("{}\"sand\"/*SAND_CLIENT_MODE_V1*/", &c[1])
        }
        fn to_ide(c: &regex::Captures<'_>) -> String {
            format!("{}\"ide\"", &c[1])
        }
        PatchRule {
            id: RuleId::ClientType,
            kind: RuleKind::Regex {
                // 和 Python 一样同时匹配 ide|sand，靠 guard 跳过已打过的位置。
                apply: Regex::new(r#"(type:)"(?:ide|sand)""#).unwrap(),
                apply_repl: to_sand,
                skip_if_followed_by: Some(Regex::new(CLIENT_MARKER_GUARD_PATTERN).unwrap()),
                skip_file_if_marked: false,
                remove: Regex::new(r#"(type:)"sand"/\*SAND_CLIENT_MODE_V1\*/"#).unwrap(),
                remove_repl: to_ide,
                per_file_limit: None,
            },
        }
    }

    #[test]
    fn regex_guard_replaces_python_negative_lookahead() {
        let rules = [client_rule()];
        // 第二处已经打过（"sand" 后面紧跟 marker），不该被重复打。
        let src = r#"type:"ide" | type:"sand"/*SAND_CLIENT_MODE_V1*/"#;
        let (p, rep) = apply(src, &rules);
        assert_eq!(rep.hits.client_type, 1);
        assert_eq!(p.matches("/*SAND_CLIENT_MODE_V1*/").count(), 2);
        let (back, _) = remove(&p, &rules);
        assert_eq!(back, r#"type:"ide" | type:"ide""#);
    }

    /// 仿 agent_host_enablement：注入体**前置**、原文保留，所以 apply 正则在打过之后仍然匹配。
    fn enablement_rule(skip_file_if_marked: bool) -> PatchRule {
        fn bump(c: &regex::Captures<'_>) -> String {
            format!("{}=!0;/*SAND_AGENT_HOST_ENABLEMENT_V1*/{}", &c[1], &c[0])
        }
        fn unbump(c: &regex::Captures<'_>) -> String {
            c[1].to_string()
        }
        PatchRule {
            id: RuleId::AgentHostEnablement,
            kind: RuleKind::Regex {
                apply: Regex::new(r"this\._agentHostEnabled=([a-z]+),").unwrap(),
                apply_repl: bump,
                skip_if_followed_by: None,
                skip_file_if_marked,
                remove: Regex::new(
                    r"[a-z]+=!0;/\*SAND_AGENT_HOST_ENABLEMENT_V1\*/(this\._agentHostEnabled=[a-z]+,)",
                )
                .unwrap(),
                remove_repl: unbump,
                per_file_limit: Some(1),
            },
        }
    }

    #[test]
    fn regex_per_file_limit_caps_replacements() {
        let rules = [enablement_rule(false)];
        let src = "this._agentHostEnabled=a, this._agentHostEnabled=b,";
        let (p, rep) = apply(src, &rules);
        assert_eq!(rep.hits.agent_host_enablement, 1);
        assert_eq!(p.matches("SAND_AGENT_HOST_ENABLEMENT_V1").count(), 1);
        let (back, _) = remove(&p, &rules);
        assert_eq!(back, src);
    }

    #[test]
    fn regex_skip_file_if_marked_makes_prefix_injections_idempotent() {
        let src = "x this._agentHostEnabled=n, y";
        // 没有整文件开关：原文在注入后原样保留，第二次 apply 会再注入一次。
        let (once, _) = apply(src, &[enablement_rule(false)]);
        let (twice, rep) = apply(&once, &[enablement_rule(false)]);
        assert_eq!(rep.hits.agent_host_enablement, 1);
        assert_ne!(twice, once);

        // 有开关：apply(apply(x)) == apply(x)，第二次零命中。
        let rules = [enablement_rule(true)];
        let (once, rep1) = apply(src, &rules);
        assert_eq!(rep1.hits.agent_host_enablement, 1);
        assert_eq!(
            once,
            "x n=!0;/*SAND_AGENT_HOST_ENABLEMENT_V1*/this._agentHostEnabled=n, y"
        );
        let (twice, rep2) = apply(&once, &rules);
        assert_eq!(twice, once);
        assert_eq!(rep2.hits.total(), 0);
        // 该规则任一 marker 已在文件里 → 整条跳过，哪怕别处还有能匹配的原文。
        let partial = "/*SAND_AGENT_HOST_ENABLEMENT_V1*/ this._agentHostEnabled=n,";
        let (same, rep3) = apply(partial, &rules);
        assert_eq!(same, partial);
        assert_eq!(rep3.hits.total(), 0);
        // 卸载不受开关影响。
        let (back, removed) = remove(&once, &rules);
        assert_eq!(back, src);
        assert_eq!(removed.agent_host_enablement, 1);
    }

    #[test]
    fn anchored_insert_once_and_removes_every_variant() {
        let rules = [PatchRule {
            id: RuleId::InferenceStream,
            kind: RuleKind::AnchoredInsert {
                anchor: "function hre(){".into(),
                injection: "{/*SAND_DIRECT_INFERENCE_STREAM_V1*/new}".into(),
                legacy_injections: vec!["{/*SAND_DIRECT_INFERENCE_STREAM_V1*/old}".into()],
            },
        }];
        let src = "function hre(){ body }";
        let (p, rep) = apply(src, &rules);
        assert_eq!(rep.hits.inference_stream, 1);
        assert!(p.starts_with("function hre(){{/*SAND_DIRECT_INFERENCE_STREAM_V1*/new}"));
        let (again, rep2) = apply(&p, &rules);
        assert_eq!(again, p);
        assert_eq!(rep2.hits.inference_stream, 0);
        assert_eq!(rep2.migrated.inference_stream, 0);
        // 旧版注入体也能卸
        let legacy = "function hre(){{/*SAND_DIRECT_INFERENCE_STREAM_V1*/old} body }";
        let (back, removed) = remove(legacy, &rules);
        assert_eq!(back, src);
        assert_eq!(removed.inference_stream, 1);
    }

    /// 盘上是不在本类 marker 表里的旧形态（已下线 Session 引擎的空 marker）：不能再往锚点后
    /// 叠一份，而要走迁移分支原地换掉——所以 legacy 检查必须排在 marker 检查前面。
    /// 反过来，本类 marker 在但既不是当前体也不是任何已知旧形态：不动、不叠加。
    #[test]
    fn anchored_insert_migrates_legacy_shapes_outside_the_marker_table_and_never_stacks() {
        let rules = [PatchRule {
            id: RuleId::InferenceStream,
            kind: RuleKind::AnchoredInsert {
                anchor: "function hre(){".into(),
                injection: "{/*SAND_DIRECT_INFERENCE_STREAM_V1*/direct}".into(),
                legacy_injections: vec![LEGACY_SESSION_STREAM_MARKER.into()],
            },
        }];
        let on_disk = format!("function hre(){{{LEGACY_SESSION_STREAM_MARKER} body }}");
        let before = inspect(&on_disk, &rules);
        assert_eq!((before.markers.inference_stream, before.legacy), (0, 1));
        let (swapped, rep) = apply(&on_disk, &rules);
        assert_eq!(
            swapped,
            "function hre(){{/*SAND_DIRECT_INFERENCE_STREAM_V1*/direct} body }"
        );
        assert_eq!(
            (rep.hits.inference_stream, rep.migrated.inference_stream),
            (0, 1)
        );
        let after = inspect(&swapped, &rules);
        assert_eq!((after.markers.inference_stream, after.legacy), (1, 0));
        assert!(!swapped.contains(LEGACY_SESSION_STREAM_MARKER));

        let unknown = "function hre(){{/*SAND_DIRECT_INFERENCE_STREAM_V1*/from-the-future} body }";
        let (same, rep) = apply(unknown, &rules);
        assert_eq!(same, unknown);
        assert_eq!(rep.hits.total() + rep.migrated.total(), 0);
    }

    /// 盘上装的是另一个变体（自摘要开关取反）时，apply 要原地换成当前变体：结果与对干净基线
    /// 全新 apply 逐字节一致、marker 数不变、只计 migrated 不计 hits。这就是「改了选项点重新安装」
    /// 能生效而不必先卸载的依据。
    #[test]
    fn anchored_insert_migrates_other_variant_in_place() {
        let rules = [PatchRule {
            id: RuleId::InferenceStream,
            kind: RuleKind::AnchoredInsert {
                anchor: "function hre(){".into(),
                injection: "{/*SAND_DIRECT_INFERENCE_STREAM_V1*/on}".into(),
                legacy_injections: vec![
                    "{/*SAND_DIRECT_INFERENCE_STREAM_V1*/off}".into(),
                    "{/*SAND_DIRECT_INFERENCE_STREAM_V1*/early}".into(),
                ],
            },
        }];
        let src = "function hre(){ body }";
        let (fresh, _) = apply(src, &rules);

        for old in ["off", "early"] {
            let on_disk =
                format!("function hre(){{{{/*SAND_DIRECT_INFERENCE_STREAM_V1*/{old}}} body }}");
            assert_eq!(inspect(&on_disk, &rules).markers.inference_stream, 1);
            let (migrated, rep) = apply(&on_disk, &rules);
            assert_eq!(migrated, fresh, "迁移 {old} 应与全新安装逐字节一致");
            assert_eq!(rep.hits.inference_stream, 0);
            assert_eq!(rep.migrated.inference_stream, 1);
            assert_eq!(inspect(&migrated, &rules).markers.inference_stream, 1);
            // 迁移后再 apply 幂等；remove 回到基线。
            let (again, rep2) = apply(&migrated, &rules);
            assert_eq!(again, migrated);
            assert_eq!(rep2.migrated.inference_stream, 0);
            let (back, _) = remove(&migrated, &rules);
            assert_eq!(back, src);
        }
    }

    #[test]
    fn inspect_counts_markers_foreign_legacy_and_remaining() {
        let rules = [client_rule()];
        let content = concat!(
            r#"type:"ide"/*SAND_CLIENT_MODE_V1*/ "#,  // 我们的
            r#"type:"ide"/*OTHER_SAND_CLIENT_V1*/ "#, // 外部工具
            r#"type:"ide" "#,                         // 还没打
            "/*SAND_MANAGED_TASK_TOOL_V2*/ ",         // 旧版
            "/*SAND_CONTEXT_WINDOW_V1*/",
        );
        let ins = inspect(content, &rules);
        assert_eq!(ins.markers.client_type, 1);
        assert_eq!(ins.markers.context_window, 1);
        assert_eq!(ins.foreign, 1);
        assert_eq!(ins.legacy, 1);
        assert_eq!(ins.remaining_ide, 1);
        assert!(ins.touched());
        assert!(!inspect("nothing here", &rules).touched());
    }
}
