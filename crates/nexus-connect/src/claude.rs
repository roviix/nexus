//! Claude Code：`~/.claude/settings.json`。
//!
//! 只动 `env` 里我们那几个键，其余（permissions、hooks、theme…）一字不改。Claude Code 的
//! 模型选择是一组槽位而不是一个变量：UI 里切 Opus / Sonnet / Haiku / Fable 各走各的，后台的
//! 标题生成、文件摘要走 Haiku 那一档；任何一档没钉死，它就会发官方模型名。
//!
//! 接到本地网关时，四档写的是**稳定的官方档名**（[`ClaudeModels`]），不是真实模型：网关按档名
//! 里的 sonnet / opus / haiku / fable 认出是哪一档，再换成应用里配的模型。于是之后换模型、换通道
//! 都不用再动这份文件、不用重启 Claude Code。`*_MODEL_NAME` 是菜单里显示的名字，写真实模型，
//! 让人看得出这一档此刻走的是什么。`ANTHROPIC_MODEL` 删掉，默认档交给 Claude Code 自己选
//! （它也是在这四档里选）。
//!
//! **这张键表与前端 `relay/snippets.ts::claudeSettings` 必须一致**：一键接入写进去的，
//! 要和「复制」抄出来的是同一份东西。

use crate::{Seen, Target};
use nexus_core::{AppError, Result};
use serde_json::{Map, Value};
use std::path::Path;

/// 我们会动的键。撤销时若没有备份可还原，就是把这些键删掉。
///
/// 认证两个都列在这里：一次写入只放其中一个（`AUTH_TOKEN` 或 `API_KEY`），
/// 另一个要删掉，否则 Claude Code 会拿着上次留下的那把钥匙。
pub const ENV_KEYS: &[&str] = &[
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_MODEL",
    "ANTHROPIC_SMALL_FAST_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL",
    "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME",
    "ANTHROPIC_DEFAULT_SONNET_MODEL",
    "ANTHROPIC_DEFAULT_SONNET_MODEL_NAME",
    "ANTHROPIC_DEFAULT_FABLE_MODEL",
    "ANTHROPIC_DEFAULT_FABLE_MODEL_NAME",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
    "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
];

/// 一档：写进配置的模型名，和菜单里显示的名字。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClaudeSlot {
    pub model: String,
    pub display: String,
}

/// 四档各写什么。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClaudeModels {
    pub sonnet: ClaudeSlot,
    pub opus: ClaudeSlot,
    pub haiku: ClaudeSlot,
    pub fable: ClaudeSlot,
}

/// 写给 Claude Code 的四个稳定档名。Claude Code 认得它们（上下文窗口、思考、计价都按档走），
/// 网关靠名字里的档位词认档。
pub const ALIAS_SONNET: &str = "claude-sonnet-4-6";
pub const ALIAS_OPUS: &str = "claude-opus-4-8";
pub const ALIAS_HAIKU: &str = "claude-haiku-4-5";
pub const ALIAS_FABLE: &str = "claude-fable-5";

impl ClaudeModels {
    /// 接本地网关用的四档：稳定档名 + 显示名。`context1m` 给 Sonnet / Opus / Fable 补 `[1M]`，
    /// Claude Code 靠它按 1M 上下文算预算（Haiku 没有 1M 版）。
    pub fn gateway(sonnet: &str, opus: &str, haiku: &str, fable: &str, context1m: bool) -> Self {
        let slot = |alias: &str, display: &str, one_m: bool| ClaudeSlot {
            model: if one_m {
                format!("{alias}[1M]")
            } else {
                alias.to_string()
            },
            display: display.trim().to_string(),
        };
        Self {
            sonnet: slot(ALIAS_SONNET, sonnet, context1m),
            opus: slot(ALIAS_OPUS, opus, context1m),
            haiku: slot(ALIAS_HAIKU, haiku, false),
            fable: slot(ALIAS_FABLE, fable, context1m),
        }
    }
}

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
    // 两个认证键只留这次要的那个。
    env.remove("ANTHROPIC_AUTH_TOKEN");
    env.remove("ANTHROPIC_API_KEY");
    env.insert(t.auth_env.as_env().into(), Value::String(t.api_key.clone()));

    let models = t.claude.clone().unwrap_or_else(|| {
        let slot = ClaudeSlot {
            model: t.model.clone(),
            display: t.model.clone(),
        };
        ClaudeModels {
            sonnet: slot.clone(),
            opus: slot.clone(),
            haiku: slot.clone(),
            fable: slot,
        }
    });
    if t.claude.is_some() {
        env.remove("ANTHROPIC_MODEL");
    } else {
        env.insert("ANTHROPIC_MODEL".into(), Value::String(t.model.clone()));
    }
    env.insert(
        "ANTHROPIC_SMALL_FAST_MODEL".into(),
        Value::String(models.haiku.model.clone()),
    );
    for (role, slot) in [
        ("SONNET", &models.sonnet),
        ("OPUS", &models.opus),
        ("HAIKU", &models.haiku),
        ("FABLE", &models.fable),
    ] {
        env.insert(
            format!("ANTHROPIC_DEFAULT_{role}_MODEL"),
            Value::String(slot.model.clone()),
        );
        let name_key = format!("ANTHROPIC_DEFAULT_{role}_MODEL_NAME");
        if slot.display.is_empty() {
            env.remove(&name_key);
        } else {
            env.insert(name_key, Value::String(slot.display.clone()));
        }
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

/// 现在指向哪。文件不存在 / 不合法 / 没配都是空的。
///
/// 模型给菜单里 Sonnet 档显示的那个名字（接网关时就是它此刻真实走的模型）；没有显示名的老配置
/// 退回 `ANTHROPIC_MODEL`。
pub fn inspect(existing: Option<&str>) -> Seen {
    let Ok(root) = parse(existing) else {
        return Seen::default();
    };
    let env = root.get("env").and_then(Value::as_object);
    let get = |k: &str| {
        env.and_then(|e| e.get(k))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Seen {
        base_url: get("ANTHROPIC_BASE_URL"),
        model: get("ANTHROPIC_DEFAULT_SONNET_MODEL_NAME")
            .or_else(|| get("ANTHROPIC_MODEL"))
            .or_else(|| get("ANTHROPIC_DEFAULT_SONNET_MODEL")),
        api_key: get("ANTHROPIC_AUTH_TOKEN").or_else(|| get("ANTHROPIC_API_KEY")),
    }
}

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

/// `~/.claude.json` 里的首次引导标记。
///
/// 没跑过官方登录的 Claude Code 第一次启动会先进引导（选主题、选登录方式），哪怕 settings.json
/// 里已经给了地址和钥匙——那一步正是「接上了却打不开」的样子。这里只补一个
/// `hasCompletedOnboarding: true`：文件里别的东西（项目历史、MCP）一字不碰；已经是 true 就
/// 连文件都不写（Claude Code 开着的时候也在写它）。文件坏了就算了，不因此让接入失败。
///
/// 返回这次是不是真的写了。
pub fn ensure_onboarded(home: &Path) -> Result<bool> {
    let path = home.join(".claude.json");
    let existing = crate::fsx::read_opt(&path)?;
    let mut root = match existing.as_deref().map(str::trim) {
        None | Some("") => Map::new(),
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(m)) => m,
            _ => return Ok(false),
        },
    };
    if root.get("hasCompletedOnboarding").and_then(Value::as_bool) == Some(true) {
        return Ok(false);
    }
    root.insert("hasCompletedOnboarding".into(), Value::Bool(true));
    crate::fsx::write_atomic(&path, &pretty(&Value::Object(root)))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target::simple("http://127.0.0.1:8787", "nx-key", "claude-sonnet-5")
    }

    fn gateway_target() -> Target {
        let mut t = Target::simple("http://127.0.0.1:8787/client/claude", "nx-key", "");
        t.claude = Some(ClaudeModels::gateway(
            "cursor/claude-sonnet-5",
            "cursor/claude-opus-5",
            "chatgpt/gpt-5.4-mini",
            "cursor/claude-opus-5",
            false,
        ));
        t
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
        assert_eq!(v["env"]["ANTHROPIC_DEFAULT_FABLE_MODEL"], "claude-sonnet-5");
        assert!(v["env"].get("ANTHROPIC_API_KEY").is_none());
        assert!(out.ends_with('\n'));
    }

    #[test]
    fn gateway_mode_writes_stable_aliases_with_real_display_names() {
        let existing = r#"{"env":{"ANTHROPIC_MODEL":"old","ANTHROPIC_API_KEY":"sk-ant-old"}}"#;
        let out = merge(Some(existing), &gateway_target()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        let env = &v["env"];
        assert_eq!(
            env["ANTHROPIC_BASE_URL"],
            "http://127.0.0.1:8787/client/claude"
        );
        assert!(
            env.get("ANTHROPIC_MODEL").is_none(),
            "默认档交给 Claude Code 自己选"
        );
        assert!(env.get("ANTHROPIC_API_KEY").is_none());
        assert_eq!(env["ANTHROPIC_DEFAULT_SONNET_MODEL"], ALIAS_SONNET);
        assert_eq!(env["ANTHROPIC_DEFAULT_OPUS_MODEL"], ALIAS_OPUS);
        assert_eq!(env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], ALIAS_HAIKU);
        assert_eq!(env["ANTHROPIC_DEFAULT_FABLE_MODEL"], ALIAS_FABLE);
        assert_eq!(env["ANTHROPIC_SMALL_FAST_MODEL"], ALIAS_HAIKU);
        assert_eq!(
            env["ANTHROPIC_DEFAULT_SONNET_MODEL_NAME"],
            "cursor/claude-sonnet-5"
        );
        assert_eq!(
            env["ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME"],
            "chatgpt/gpt-5.4-mini"
        );

        let seen = inspect(Some(&out));
        assert_eq!(seen.model.as_deref(), Some("cursor/claude-sonnet-5"));
        assert_eq!(seen.api_key.as_deref(), Some("nx-key"));
    }

    #[test]
    fn one_m_goes_on_every_role_but_haiku() {
        let m = ClaudeModels::gateway("a", "b", "c", "d", true);
        assert_eq!(m.sonnet.model, "claude-sonnet-4-6[1M]");
        assert_eq!(m.opus.model, "claude-opus-4-8[1M]");
        assert_eq!(m.fable.model, "claude-fable-5[1M]");
        assert_eq!(m.haiku.model, "claude-haiku-4-5");
    }

    #[test]
    fn merge_from_nothing_builds_a_fresh_file() {
        let out = merge(None, &target()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        // 两个认证键只写一个，所以比 ENV_KEYS 少一。
        assert_eq!(v["env"].as_object().unwrap().len(), ENV_KEYS.len() - 1);
        let seen = inspect(Some(&out));
        assert_eq!(seen.base_url.as_deref(), Some("http://127.0.0.1:8787"));
        assert_eq!(seen.model.as_deref(), Some("claude-sonnet-5"));
    }

    #[test]
    fn a_broken_file_is_refused_not_overwritten() {
        let err = merge(Some("{ not json"), &target()).unwrap_err();
        assert!(err.message.contains("不是合法的 JSON"));
        let err = merge(Some("[1,2]"), &target()).unwrap_err();
        assert!(err.message.contains("不是一个对象"));
        assert_eq!(inspect(Some("{ not json")), Seen::default());
    }

    #[test]
    fn strip_removes_only_our_keys() {
        let merged = merge(
            Some(r#"{"env":{"MY_FLAG":"1"},"theme":"dark"}"#),
            &gateway_target(),
        )
        .unwrap();
        let back = strip(&merged).unwrap().unwrap();
        let v: Value = serde_json::from_str(&back).unwrap();
        assert_eq!(v["env"]["MY_FLAG"], "1");
        assert!(v["env"].get("ANTHROPIC_BASE_URL").is_none());
        assert!(v["env"]
            .get("ANTHROPIC_DEFAULT_SONNET_MODEL_NAME")
            .is_none());
        assert_eq!(v["theme"], "dark");

        // 只有我们的键 → 整份文件该消失。
        let ours = merge(None, &gateway_target()).unwrap();
        assert!(strip(&ours).unwrap().is_none());
    }

    #[test]
    fn onboarding_flag_is_added_once_and_the_rest_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        std::fs::write(
            &path,
            r#"{"projects":{"/x":{"history":[1]}},"numStartups":3}"#,
        )
        .unwrap();
        assert!(ensure_onboarded(dir.path()).unwrap());
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["hasCompletedOnboarding"], true);
        assert_eq!(v["projects"]["/x"]["history"][0], 1);
        assert_eq!(v["numStartups"], 3);
        assert!(
            !ensure_onboarded(dir.path()).unwrap(),
            "已经是 true 就不再写"
        );

        let fresh = tempfile::tempdir().unwrap();
        assert!(ensure_onboarded(fresh.path()).unwrap());
        let broken = tempfile::tempdir().unwrap();
        std::fs::write(broken.path().join(".claude.json"), "{ nope").unwrap();
        assert!(!ensure_onboarded(broken.path()).unwrap(), "坏文件不碰");
    }
}
