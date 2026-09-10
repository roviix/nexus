//! Codex CLI：`~/.codex/config.toml` + `~/.codex/auth.json`。
//!
//! 密钥内联进 config.toml（`experimental_bearer_token`）：这是 Codex 三条取钥匙的路里唯一不依赖
//! 终端环境、也不会误发 ChatGPT 登录态的一条（`requires_openai_auth = false` 就是为了后者）。
//! 0.46 及更早只认 `auth.json`，所以那份也写，两份都在就覆盖全部版本。
//!
//! 用 `toml_edit` 而不是重新序列化：用户自己的 config.toml 里有注释、有他调好的别的
//! provider，重排一遍等于替他改了一份他没让改的文件。
//!
//! **写进去的键与前端 `relay/snippets.ts::codexToml` 一致。**

use crate::Target;
use nexus_core::{AppError, Result};
use serde_json::{Map, Value};
use toml_edit::{value, DocumentMut, Item, Table};

pub const PROVIDER: &str = "nexus";

fn parse(existing: Option<&str>) -> Result<DocumentMut> {
    let Some(text) = existing else {
        return Ok(DocumentMut::new());
    };
    text.parse::<DocumentMut>().map_err(|e| {
        AppError::invalid(format!(
            "~/.codex/config.toml 不是合法的 TOML（{e}），没有改动它。"
        ))
        .with_hint("手动修好这份文件，或先备份后删掉它再接入。")
    })
}

/// 合进 `model_provider` / `model` / `[model_providers.nexus]`，其余原样保留。
pub fn merge_config(existing: Option<&str>, t: &Target) -> Result<String> {
    let mut doc = parse(existing)?;
    doc["model_provider"] = value(PROVIDER);
    doc["model"] = value(t.model.as_str());

    let providers = doc
        .entry("model_providers")
        .or_insert(Item::Table(Table::new()));
    if !providers.is_table() {
        *providers = Item::Table(Table::new());
    }
    let providers = providers.as_table_mut().expect("刚确认过是表");
    // 隐式：不单独打一行空的 `[model_providers]` 头，只出 `[model_providers.nexus]`。
    providers.set_implicit(true);
    let ours = providers
        .entry(PROVIDER)
        .or_insert(Item::Table(Table::new()));
    if !ours.is_table() {
        *ours = Item::Table(Table::new());
    }
    let ours = ours.as_table_mut().expect("刚确认过是表");
    ours["name"] = value(PROVIDER);
    ours["base_url"] = value(t.v1_url());
    ours["wire_api"] = value("responses");
    ours["requires_openai_auth"] = value(false);
    ours["experimental_bearer_token"] = value(t.api_key.as_str());
    Ok(doc.to_string())
}

/// 撤销的兜底：删掉我们的 provider；`model_provider` 若正指着它也删。`None` = 文件可以删了。
pub fn strip_config(existing: &str) -> Result<Option<String>> {
    let mut doc = parse(Some(existing))?;
    let pointing_at_us = doc
        .get("model_provider")
        .and_then(Item::as_str)
        .map(|s| s == PROVIDER)
        .unwrap_or(false);
    if pointing_at_us {
        doc.remove("model_provider");
        doc.remove("model");
    }
    let mut drop_providers = false;
    if let Some(t) = doc.get_mut("model_providers").and_then(Item::as_table_mut) {
        t.remove(PROVIDER);
        drop_providers = t.is_empty();
    }
    if drop_providers {
        doc.remove("model_providers");
    }
    let text = doc.to_string();
    if text.trim().is_empty() {
        return Ok(None);
    }
    Ok(Some(text))
}

/// 现在指向哪：只在 `model_provider = "nexus"` 时给出 `(base_url, model)`；用别的 provider
/// 算「没接」。
pub fn inspect_config(existing: Option<&str>) -> (Option<String>, Option<String>) {
    let Ok(doc) = parse(existing) else {
        return (None, None);
    };
    let provider = doc.get("model_provider").and_then(Item::as_str);
    if provider != Some(PROVIDER) {
        return (None, None);
    }
    let base = doc
        .get("model_providers")
        .and_then(|p| p.get(PROVIDER))
        .and_then(|p| p.get("base_url"))
        .and_then(Item::as_str)
        .map(str::to_string);
    let model = doc.get("model").and_then(Item::as_str).map(str::to_string);
    (base, model)
}

/// `auth.json`：只动 `OPENAI_API_KEY`。
pub fn merge_auth(existing: Option<&str>, t: &Target) -> Result<String> {
    let mut root = match existing.map(str::trim).filter(|s| !s.is_empty()) {
        None => Map::new(),
        Some(text) => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(m)) => m,
            _ => {
                return Err(AppError::invalid(
                    "~/.codex/auth.json 不是一个合法的 JSON 对象，没有改动它。",
                )
                .with_hint("手动修好这份文件，或先备份后删掉它再接入。"))
            }
        },
    };
    root.insert("OPENAI_API_KEY".into(), Value::String(t.api_key.clone()));
    let mut s = serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    Ok(s)
}

pub fn strip_auth(existing: &str) -> Result<Option<String>> {
    let mut root = match serde_json::from_str::<Value>(existing) {
        Ok(Value::Object(m)) => m,
        _ => return Ok(Some(existing.to_string())), // 看不懂的就别碰
    };
    root.remove("OPENAI_API_KEY");
    if root.is_empty() {
        return Ok(None);
    }
    let mut s = serde_json::to_string_pretty(&Value::Object(root)).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    Ok(Some(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            base_url: "http://127.0.0.1:8787".into(),
            api_key: "nx-key".into(),
            model: "gpt-5.6-sol".into(),
        }
    }

    #[test]
    fn merge_preserves_comments_and_other_providers() {
        let existing = r#"# my codex config
model_provider = "openai"
model = "gpt-4.1"
approval_policy = "never"

[model_providers.work]
name = "work"
base_url = "https://work.example/v1"
"#;
        let out = merge_config(Some(existing), &target()).unwrap();
        assert!(out.starts_with("# my codex config"), "注释要留着：{out}");
        assert!(out.contains(r#"approval_policy = "never""#));
        assert!(out.contains("[model_providers.work]"));
        assert!(out.contains(r#"model_provider = "nexus""#));
        assert!(out.contains(r#"model = "gpt-5.6-sol""#));
        assert!(out.contains("[model_providers.nexus]"));
        assert!(out.contains(r#"base_url = "http://127.0.0.1:8787/v1""#));
        assert!(out.contains("requires_openai_auth = false"));
        assert!(out.contains(r#"experimental_bearer_token = "nx-key""#));
        assert!(
            !out.contains("\n[model_providers]\n"),
            "不该打空表头：{out}"
        );

        let (base, model) = inspect_config(Some(&out));
        assert_eq!(base.as_deref(), Some("http://127.0.0.1:8787/v1"));
        assert_eq!(model.as_deref(), Some("gpt-5.6-sol"));
    }

    #[test]
    fn merge_from_nothing_and_strip_back_to_nothing() {
        let out = merge_config(None, &target()).unwrap();
        assert!(out.contains("[model_providers.nexus]"));
        assert!(
            strip_config(&out).unwrap().is_none(),
            "只有我们的东西 → 文件该消失"
        );
    }

    #[test]
    fn strip_leaves_the_users_other_provider_alone() {
        let existing = r#"model_provider = "openai"
model = "gpt-4.1"

[model_providers.work]
name = "work"
"#;
        let merged = merge_config(Some(existing), &target()).unwrap();
        let back = strip_config(&merged).unwrap().unwrap();
        assert!(back.contains("[model_providers.work]"));
        assert!(!back.contains("nexus"));
        // 用户原来的 model_provider 已被我们覆盖过，撤销时只能删掉我们的值，不能凭空造回 "openai"。
        assert!(!back.contains("model_provider ="));
        assert!(!back.contains("model ="));
    }

    #[test]
    fn inspect_ignores_other_providers_and_broken_files() {
        assert_eq!(
            inspect_config(Some(r#"model_provider = "openai""#)),
            (None, None)
        );
        assert_eq!(inspect_config(Some("= broken")), (None, None));
        assert_eq!(inspect_config(None), (None, None));
    }

    #[test]
    fn auth_json_only_touches_the_key() {
        let out = merge_auth(Some(r#"{"tokens": {"a": 1}}"#), &target()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["OPENAI_API_KEY"], "nx-key");
        assert_eq!(v["tokens"]["a"], 1);
        let back = strip_auth(&out).unwrap().unwrap();
        let v: Value = serde_json::from_str(&back).unwrap();
        assert!(v.get("OPENAI_API_KEY").is_none());
        assert!(strip_auth(&merge_auth(None, &target()).unwrap())
            .unwrap()
            .is_none());
        assert!(merge_auth(Some("[]"), &target()).is_err());
    }
}
