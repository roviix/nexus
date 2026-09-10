//! OpenCode：`~/.config/opencode/opencode.json`。
//!
//! 只改内置 `openai` provider 的 `options.baseURL` + `apiKey`，以及顶层 `model`（写成
//! `openai/{id}`）。不发明自定义 provider——否则丢掉 OpenCode 自带的模型元数据。

use crate::Target;
use nexus_core::{AppError, Result};
use serde_json::{json, Map, Value};

fn parse(existing: Option<&str>) -> Result<Value> {
    let Some(text) = existing else {
        return Ok(json!({}));
    };
    serde_json::from_str(text).map_err(|e| {
        AppError::invalid(format!(
            "~/.config/opencode/opencode.json 不是合法 JSON（{e}），没有改动它。"
        ))
        .with_hint("手动修好这份文件，或先备份后删掉它再接入。")
    })
}

fn as_object_mut<'a>(v: &'a mut Value, hint: &str) -> Result<&'a mut Map<String, Value>> {
    if !v.is_object() {
        *v = json!({});
    }
    v.as_object_mut()
        .ok_or_else(|| AppError::invalid(format!("{hint} 不是对象，没有改动它。")))
}

pub fn merge(existing: Option<&str>, t: &Target) -> Result<String> {
    let mut root = parse(existing)?;
    let obj = as_object_mut(&mut root, "opencode.json")?;
    let provider = obj.entry("provider").or_insert_with(|| json!({}));
    let provider = as_object_mut(provider, "provider")?;
    let openai = provider.entry("openai").or_insert_with(|| json!({}));
    let openai = as_object_mut(openai, "provider.openai")?;
    let options = openai.entry("options").or_insert_with(|| json!({}));
    let options = as_object_mut(options, "provider.openai.options")?;
    options.insert("baseURL".into(), json!(t.v1_url()));
    options.insert("apiKey".into(), json!(t.api_key));
    obj.insert("model".into(), json!(format!("openai/{}", t.model.trim())));
    Ok(serde_json::to_string_pretty(&root)? + "\n")
}

pub fn inspect(existing: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(text) = existing else {
        return (None, None);
    };
    let Ok(v) = serde_json::from_str::<Value>(text) else {
        return (None, None);
    };
    let base = v
        .pointer("/provider/openai/options/baseURL")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let model = v
        .get("model")
        .and_then(|x| x.as_str())
        .map(|s| s.trim().trim_start_matches("openai/").to_string())
        .filter(|s| !s.is_empty());
    (base, model)
}

pub fn strip(existing: &str) -> Result<Option<String>> {
    let mut root = parse(Some(existing))?;
    let Some(obj) = root.as_object_mut() else {
        return Ok(None);
    };
    if let Some(provider) = obj.get_mut("provider").and_then(|p| p.as_object_mut()) {
        if let Some(openai) = provider.get_mut("openai").and_then(|p| p.as_object_mut()) {
            if let Some(options) = openai.get_mut("options").and_then(|p| p.as_object_mut()) {
                options.remove("baseURL");
                options.remove("apiKey");
                if options.is_empty() {
                    openai.remove("options");
                }
            }
            if openai.is_empty() {
                provider.remove("openai");
            }
        }
        if provider.is_empty() {
            obj.remove("provider");
        }
    }
    obj.remove("model");
    if obj.is_empty() {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string_pretty(&root)? + "\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Target {
        Target {
            base_url: "http://127.0.0.1:8787".into(),
            api_key: "nx-x".into(),
            model: "grok-4.5".into(),
        }
    }

    #[test]
    fn merge_keeps_other_provider_fields() {
        let existing = r#"{"provider":{"openai":{"options":{"timeout":30},"name":"OpenAI"}}}"#;
        let out = merge(Some(existing), &t()).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            v["provider"]["openai"]["options"]["baseURL"],
            "http://127.0.0.1:8787/v1"
        );
        assert_eq!(v["provider"]["openai"]["options"]["apiKey"], "nx-x");
        assert_eq!(v["provider"]["openai"]["options"]["timeout"], 30);
        assert_eq!(v["model"], "openai/grok-4.5");
    }
}
