//! 两边都要知道的常量：域名、模型目录、公开名字怎么落到上游 key。
//!
//! 请求体和 SSE 在 `chat`，签名在 `cosy`。这里不发网络请求。

use crate::model::QoderBackend;

pub const ROUTE_PREFIXES: &[&str] = &["qoder/"];

pub const OPENAPI_COSY_VERSION: &str = "1.0.1";
pub const GATEWAY_COSY_VERSION: &str = "1.1.38";
pub const CLIENT_TYPE: &str = "5";
/// 上游文档里的单次输出上限。目录接口不报每模型的 cap，就用这一档。
pub const MAX_OUTPUT_TOKENS: u32 = 131_072;

const EFFORTS: &[&str] = &["low", "medium", "high", "xhigh", "max"];

struct ModelSpec {
    id: &'static str,
    key: &'static str,
    backend: QoderBackend,
    reasoning: bool,
    supports_effort: bool,
}

/// 公开 id 是给人看的（`Qwen3.8-Max`）。`key` 才是网关认的（`qmodel_preview`）。
const MODELS: &[ModelSpec] = &[
    spec("Auto", "auto", QoderBackend::Global, true, false),
    spec("Ultimate", "ultimate", QoderBackend::Global, true, true),
    spec(
        "Performance",
        "performance",
        QoderBackend::Global,
        true,
        true,
    ),
    spec("Efficient", "efficient", QoderBackend::Global, false, false),
    spec("Lite", "lite", QoderBackend::Global, false, false),
    spec("Qwen3.7-Plus", "qmodel", QoderBackend::Global, false, false),
    spec("Cantus", "cmodel", QoderBackend::Global, true, true),
    spec(
        "Qwen3.8-Max",
        "qmodel_preview",
        QoderBackend::Global,
        true,
        true,
    ),
    spec(
        "Qwen3.7-Max",
        "qmodel_latest",
        QoderBackend::Global,
        false,
        false,
    ),
    spec(
        "DeepSeek-V4-Pro",
        "dmodel",
        QoderBackend::Global,
        true,
        true,
    ),
    spec(
        "DeepSeek-V4-Flash",
        "dfmodel",
        QoderBackend::Global,
        true,
        true,
    ),
    spec("GLM-5.2", "gm51model", QoderBackend::Global, true, true),
    spec(
        "Kimi-K2.7-Code",
        "kmodel",
        QoderBackend::Global,
        false,
        false,
    ),
    spec(
        "Kimi-K3",
        "kmodel_latest",
        QoderBackend::Global,
        false,
        false,
    ),
    spec("MiniMax-M3", "mmodel", QoderBackend::Global, false, false),
    spec("Auto", "auto", QoderBackend::Cn, true, false),
    spec(
        "Qwen3.7-Max",
        "qmodel_latest",
        QoderBackend::Cn,
        true,
        false,
    ),
    spec("Qwen3.7-Plus", "qmodel", QoderBackend::Cn, true, false),
    spec("Qwen3.6-Flash", "q36fmodel", QoderBackend::Cn, true, false),
    spec("DeepSeek-V4-Pro", "dmodel", QoderBackend::Cn, true, false),
    spec(
        "DeepSeek-V4-Flash",
        "dfmodel",
        QoderBackend::Cn,
        false,
        false,
    ),
    spec("GLM-5.2", "gm51model", QoderBackend::Cn, true, false),
    spec("Kimi-K2.7-Code", "kmodel", QoderBackend::Cn, true, false),
    spec("MiniMax-M2.7", "mmodel", QoderBackend::Cn, false, false),
];

const fn spec(
    id: &'static str,
    key: &'static str,
    backend: QoderBackend,
    reasoning: bool,
    supports_effort: bool,
) -> ModelSpec {
    ModelSpec {
        id,
        key,
        backend,
        reasoning,
        supports_effort,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModel {
    pub id: String,
    pub key: String,
    pub reasoning: bool,
    pub supports_effort: bool,
    pub effort: Option<String>,
}

pub fn openapi_base(backend: QoderBackend) -> &'static str {
    match backend {
        QoderBackend::Global => "https://openapi.qoder.sh",
        QoderBackend::Cn => "https://openapi.qoder.com.cn",
    }
}

pub fn gateway_base(backend: QoderBackend) -> &'static str {
    match backend {
        QoderBackend::Global => "https://api3.qoder.sh/",
        QoderBackend::Cn => "https://gateway.qoder.com.cn/",
    }
}

pub fn exchange_url(backend: QoderBackend) -> String {
    format!("{}/api/v1/jobToken/exchange", openapi_base(backend))
}

pub fn user_info_url(backend: QoderBackend) -> String {
    format!("{}/api/v1/userinfo", openapi_base(backend))
}

pub fn chat_url(backend: QoderBackend) -> String {
    format!(
        "{}algo/api/v2/service/pro/sse/agent_chat_generation?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1",
        gateway_base(backend)
    )
}

/// 签名用的路径：去掉域名和 query，再剥掉网关统一的 `/algo` 前缀。
pub fn sig_path(request_url: &str) -> String {
    let path = request_url
        .split('?')
        .next()
        .unwrap_or(request_url)
        .rsplit("//")
        .next()
        .unwrap_or(request_url);
    let path = match path.find('/') {
        Some(i) => &path[i..],
        None => path,
    };
    path.strip_prefix("/algo").unwrap_or(path).to_string()
}

/// 对外目录。两边都有的名字只出现一次；只有一边有的也列出来，
/// 当前号没有这个模型时换号再试。
pub fn catalog() -> Vec<String> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for model in MODELS {
        let key = fold(model.id);
        if seen.iter().any(|s| s == &key) {
            continue;
        }
        seen.push(key);
        out.push(model.id.to_string());
    }
    out
}

pub fn known_model(model: &str) -> bool {
    let (base, _) = split_effort(strip_prefix(model));
    MODELS
        .iter()
        .any(|m| fold(m.id) == fold(base) || fold(m.key) == fold(base))
}

pub fn resolve_model(backend: QoderBackend, model: &str) -> Option<ResolvedModel> {
    let (base, effort) = split_effort(strip_prefix(model));
    let folded = fold(base);
    let hit = MODELS
        .iter()
        .find(|m| m.backend == backend && (fold(m.id) == folded || fold(m.key) == folded))?;
    let effort = effort.filter(|_| hit.supports_effort);
    Some(ResolvedModel {
        id: hit.id.to_string(),
        key: hit.key.to_string(),
        reasoning: hit.reasoning,
        supports_effort: hit.supports_effort,
        effort: effort.map(str::to_string),
    })
}

fn strip_prefix(model: &str) -> &str {
    let model = model.trim();
    for prefix in ROUTE_PREFIXES {
        if model.len() >= prefix.len()
            && model.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
        {
            return model[prefix.len()..].trim();
        }
    }
    model
}

fn split_effort(model: &str) -> (&str, Option<&str>) {
    let lower = model.to_ascii_lowercase();
    let marker = "-effort-";
    let Some(at) = lower.rfind(marker) else {
        return (model, None);
    };
    let effort = &model[at + marker.len()..];
    if EFFORTS.iter().any(|e| e.eq_ignore_ascii_case(effort)) {
        (model[..at].trim(), Some(effort))
    } else {
        (model, None)
    }
}

fn fold(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_url_keeps_the_algo_prefix_and_sig_path_strips_it() {
        let url = chat_url(QoderBackend::Global);
        assert!(url.starts_with("https://api3.qoder.sh/algo/api/v2/service/pro/sse/"));
        assert_eq!(
            sig_path(&url),
            "/api/v2/service/pro/sse/agent_chat_generation"
        );
        assert_eq!(
            sig_path(&chat_url(QoderBackend::Cn)),
            "/api/v2/service/pro/sse/agent_chat_generation"
        );
    }

    #[test]
    fn public_names_and_upstream_keys_both_resolve() {
        let hit = resolve_model(QoderBackend::Global, "qoder/Qwen3.8-Max").unwrap();
        assert_eq!(hit.key, "qmodel_preview");
        let by_key = resolve_model(QoderBackend::Global, "qmodel_preview").unwrap();
        assert_eq!(by_key.id, "Qwen3.8-Max");
        let dashed = resolve_model(QoderBackend::Global, "qwen3.8-max-effort-high").unwrap();
        assert_eq!(dashed.key, "qmodel_preview");
        assert_eq!(dashed.effort.as_deref(), Some("high"));
    }

    #[test]
    fn a_cn_only_model_does_not_resolve_on_global() {
        assert!(resolve_model(QoderBackend::Cn, "Qwen3.6-Flash").is_some());
        assert!(resolve_model(QoderBackend::Global, "Qwen3.6-Flash").is_none());
        assert!(known_model("Qwen3.6-Flash"));
    }

    #[test]
    fn effort_on_a_model_that_has_no_effort_is_dropped() {
        let hit = resolve_model(QoderBackend::Cn, "Qwen3.7-Max-effort-max").unwrap();
        assert!(hit.effort.is_none());
        assert!(hit.reasoning);
    }
}
