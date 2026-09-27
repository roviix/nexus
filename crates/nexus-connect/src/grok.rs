//! Grok CLI：`~/.grok/config.toml`。
//!
//! 写法对齐 sub2api / CLIProxyAPI 给 Grok CLI 写的那份：一个 named model，走 Responses。

use crate::{Seen, Target};
use nexus_core::{AppError, Result};
use toml_edit::{value, DocumentMut, Item, Table};

fn parse(existing: Option<&str>) -> Result<DocumentMut> {
    let Some(text) = existing else {
        return Ok(DocumentMut::new());
    };
    text.parse::<DocumentMut>().map_err(|e| {
        AppError::invalid(format!(
            "~/.grok/config.toml 不是合法的 TOML（{e}），没有改动它。"
        ))
        .with_hint("手动修好这份文件，或先备份后删掉它再接入。")
    })
}

pub fn merge(existing: Option<&str>, t: &Target) -> Result<String> {
    let mut doc = parse(existing)?;
    let models = doc.entry("models").or_insert(Item::Table(Table::new()));
    if !models.is_table() {
        *models = Item::Table(Table::new());
    }
    models.as_table_mut().expect("表")["default"] = value("grok");

    let model = doc.entry("model").or_insert(Item::Table(Table::new()));
    if !model.is_table() {
        *model = Item::Table(Table::new());
    }
    let model = model.as_table_mut().expect("表");
    model.set_implicit(true);
    let ours = model.entry("grok").or_insert(Item::Table(Table::new()));
    if !ours.is_table() {
        *ours = Item::Table(Table::new());
    }
    let ours = ours.as_table_mut().expect("表");
    ours["model"] = value(t.model.as_str());
    ours["base_url"] = value(t.v1_url());
    ours["api_key"] = value(t.api_key.as_str());
    ours["api_backend"] = value("responses");
    Ok(doc.to_string())
}

pub fn inspect(existing: Option<&str>) -> Seen {
    let Some(text) = existing else {
        return Seen::default();
    };
    let Ok(doc) = text.parse::<DocumentMut>() else {
        return Seen::default();
    };
    let grok = doc.get("model").and_then(|m| m.get("grok"));
    let field = |k: &str| {
        grok.and_then(|t| t.get(k))
            .and_then(Item::as_str)
            .map(str::to_string)
    };
    Seen {
        base_url: field("base_url"),
        model: field("model"),
        api_key: field("api_key"),
    }
}

pub fn strip(existing: &str) -> Result<Option<String>> {
    let mut doc = parse(Some(existing))?;
    if let Some(models) = doc.get_mut("models").and_then(Item::as_table_mut) {
        if models
            .get("default")
            .and_then(Item::as_str)
            .is_some_and(|s| s == "grok")
        {
            models.remove("default");
        }
        if models.is_empty() {
            doc.remove("models");
        }
    }
    let mut drop_model = false;
    if let Some(model) = doc.get_mut("model").and_then(Item::as_table_mut) {
        model.remove("grok");
        drop_model = model.is_empty();
    }
    if drop_model {
        doc.remove("model");
    }
    let text = doc.to_string();
    Ok(if text.trim().is_empty() {
        None
    } else {
        Some(text)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Target {
        Target::simple("http://127.0.0.1:8787/v1", "nx-x", "grok-4.5")
    }

    #[test]
    fn merge_writes_named_model() {
        let out = merge(None, &t()).unwrap();
        assert!(out.contains("default = \"grok\""));
        assert!(out.contains("api_backend = \"responses\""));
        assert!(out.contains("base_url = \"http://127.0.0.1:8787/v1\""));
        let seen = inspect(Some(&out));
        assert_eq!(seen.base_url.as_deref(), Some("http://127.0.0.1:8787/v1"));
        assert_eq!(seen.model.as_deref(), Some("grok-4.5"));
        assert_eq!(seen.api_key.as_deref(), Some("nx-x"));
    }
}
