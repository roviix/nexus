//! Claude Code：`~/.claude/settings.json`。
//!
//! 只动 `env` 里我们那几个键，其余（permissions、hooks、theme…）一字不改。Claude Code 的
//! 模型选择是一组槽位而不是一个变量：UI 里切 Opus / Sonnet / Haiku 各走各的，后台标题生成、
//! 文件摘要还另走 SMALL_FAST；任何一个没钉死它就会发官方模型名，中转一律 BAD_MODEL_NAME。
//! 新版读 `*_MODEL_NAME`、老版读 `*_MODEL`，两套都写才不用管版本。
//!
//! **这张键表与前端 `relay/snippets.ts::claudeSettings` 必须一致**：一键接入写进去的，
//! 要和「复制」抄出来的是同一份东西。

use crate::Target;
use nexus_core::{AppError, Result};
use serde_json::{Map, Value};

/// 我们会写的键。撤销时若没有备份可还原，就是把这些键删掉。
pub const ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
];

fn parse(existing: Option<&str>) -> Result<Map<String, Value>> {
    let Some(text) = existing.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(Map::new());
    };
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(
            AppError::invalid("~/.claude/settings.json 顶层不是一个对象，没有改动它。")
                .with_hint("把它改成 `{ ... }` 的形状，或先备份后删掉再接入。"),
        ),
        Err(e) => Err(AppError::invalid(format!(
            "~/.claude/settings.json 不是合法的 JSON（{e}），没有改动它。"
        ))
        .with_hint("手动修好这份文件，或先备份后删掉它再接入。")),
    }
}

/// 把我们的 env 键合进现有内容，返回要写回的文本。
pub fn merge(existing: Option<&str>, t: &Target) -> Result<String> {
    let mut root = parse(existing)?;
    let env = root
        .entry("env")
        .or_insert_with(|| Value::Object(Map::new()));
    if !env.is_object() {
        *env = Value::Object(Map::new());
    }
    let env = env.as_object_mut().expect("刚确认过是对象");
    env.insert("ANTHROPIC_BASE_URL".into(), Value::String(t.root_url()));
    env.insert(
        "ANTHROPIC_AUTH_TOKEN".into(),
        Value::String(t.api_key.clone()),
    );
    for k in ENV_KEYS.iter().skip(2) {
        env.insert((*k).into(), Value::String(t.model.clone()));
    }
    Ok(pretty(&Value::Object(root)))
}

/// 撤销时的兜底：没有备份可还原（文件是我们建的、或备份丢了），就把我们的键删掉。
/// 删完 `env` 空了就连 `env` 一起删；整份空了返回 `None` = 直接删文件。
pub fn strip(existing: &str) -> Result<Option<String>> {
    let mut root = parse(Some(existing))?;
    if let Some(Value::Object(env)) = root.get_mut("env") {
        for k in ENV_KEYS {
            env.remove(*k);
        }
        if env.is_empty() {
            root.remove("env");
        }
    }
    if root.is_empty() {
        return Ok(None);
    }
    Ok(Some(pretty(&Value::Object(root))))
}

/// 现在指向哪：`(base_url, model)`。文件不存在 / 不合法 / 没配都返回 `None`。
pub fn inspect(existing: Option<&str>) -> (Option<String>, Option<String>) {
    let Ok(root) = parse(existing) else {
        return (None, None);
    };
    let env = root.get("env").and_then(Value::as_object);
    let get = |k: &str| {
        env.and_then(|e| e.get(k))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    (get("ANTHROPIC_BASE_URL"), get("ANTHROPIC_MODEL"))
}

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            base_url: "http://127.0.0.1:8787".into(),
            api_key: "nx-key".into(),
            model: "claude-sonnet-5".into(),
        }
    }

    #[test]
    fn merge_keeps_unrelated_settings_and_env() {
        let existing = r#"{
  "permissions": { "allow": ["Bash(ls:*)"] },
  "env": { "MY_FLAG": "1", "ANTHROPIC_MODEL": "old" },
  "theme": "dark"
}"#;
        let out = merge(Some(existing), &target()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["permissions"]["allow"][0], "Bash(ls:*)");
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["env"]["MY_FLAG"], "1");
        assert_eq!(v["env"]["ANTHROPIC_MODEL"], "claude-sonnet-5");
        assert_eq!(v["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:8787");
        assert_eq!(v["env"]["ANTHROPIC_AUTH_TOKEN"], "nx-key");
        assert_eq!(
            v["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME"],
            "claude-sonnet-5"
        );
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn merge_from_nothing_builds_a_fresh_file() {
        let out = merge(None, &target()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["env"].as_object().unwrap().len(), ENV_KEYS.len());
        let (base, model) = inspect(Some(&out));
        assert_eq!(base.as_deref(), Some("http://127.0.0.1:8787"));
        assert_eq!(model.as_deref(), Some("claude-sonnet-5"));
    }

    #[test]
    fn a_broken_file_is_refused_not_overwritten() {
        let err = merge(Some("{ not json"), &target()).unwrap_err();
        assert!(err.message.contains("不是合法的 JSON"));
        let err = merge(Some("[1,2]"), &target()).unwrap_err();
        assert!(err.message.contains("不是一个对象"));
        assert_eq!(inspect(Some("{ not json")), (None, None));
    }

    #[test]
    fn strip_removes_only_our_keys() {
        let merged = merge(Some(r#"{"env":{"MY_FLAG":"1"},"theme":"dark"}"#), &target()).unwrap();
        let back = strip(&merged).unwrap().unwrap();
        let v: Value = serde_json::from_str(&back).unwrap();
        assert_eq!(v["env"]["MY_FLAG"], "1");
        assert!(v["env"].get("ANTHROPIC_BASE_URL").is_none());
        assert_eq!(v["theme"], "dark");

        // 只有我们的键 → 整份文件该消失。
        let ours = merge(None, &target()).unwrap();
        assert!(strip(&ours).unwrap().is_none());
    }
}
