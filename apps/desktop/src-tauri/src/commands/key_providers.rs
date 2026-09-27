//! 用 API Key 接入的供应商——网关的 `provider/` 通道背后那几家。
//!
//! 列表没有明文。显示钥匙、拉模型列表、探活，都在这边拿 secrets 里的那把，用完即弃。拉模型时
//! 表单里还没保存的钥匙可以随这次请求过来，但不落库。增删改之后顺手让网关忘掉这家的冷却
//! 记录：钥匙刚换过，别让它还因为旧钥匙的 401 歇着。

use crate::state::AppState;
use nexus_core::{AppError, Result};
use nexus_gateway::channel::PROVIDER;
use nexus_store::activity;
use nexus_store::key_providers::{
    self, ApiFormat, AuthField, Endpoint, KeyProvider, KeyProviderInput,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::{Duration, Instant};
use tauri::State;

#[tauri::command(async)]
pub fn key_providers_list(state: State<'_, AppState>) -> Result<Vec<KeyProvider>> {
    key_providers::list(&state.db)
}

#[tauri::command(async)]
pub fn key_providers_save(
    state: State<'_, AppState>,
    input: KeyProviderInput,
) -> Result<KeyProvider> {
    let before = input
        .id
        .as_deref()
        .and_then(|id| key_providers::get(&state.db, id).ok());
    let saved = key_providers::save(&state.db, state.secrets.as_ref(), input)?;
    if let Some(old) = &before {
        state.gateway.channel_forget(PROVIDER, &old.name);
    }
    state.gateway.channel_forget(PROVIDER, &saved.name);
    activity::info(
        &state.db,
        "provider",
        None,
        format!(
            "已保存供应商「{}」（{} 个模型）",
            saved.name,
            saved.models.len()
        ),
    );
    Ok(saved)
}

#[tauri::command(async)]
pub fn key_providers_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<KeyProvider> {
    let saved = key_providers::set_enabled(&state.db, &id, enabled)?;
    state.gateway.channel_forget(PROVIDER, &saved.name);
    activity::info(
        &state.db,
        "provider",
        None,
        format!(
            "{}供应商「{}」",
            if enabled { "启用了" } else { "停用了" },
            saved.name
        ),
    );
    Ok(saved)
}

#[tauri::command(async)]
pub fn key_providers_delete(state: State<'_, AppState>, id: String) -> Result<()> {
    let name = key_providers::get(&state.db, &id).map(|p| p.name).ok();
    key_providers::delete(&state.db, state.secrets.as_ref(), &id)?;
    if let Some(name) = &name {
        state.gateway.channel_forget(PROVIDER, name);
    }
    activity::info(
        &state.db,
        "provider",
        None,
        match name {
            Some(name) => format!("已删除供应商「{name}」"),
            None => "已删除供应商".into(),
        },
    );
    Ok(())
}

/// 用户显式点了「显示」。记一笔，不把钥匙写进活动日志。
#[tauri::command(async)]
pub fn key_providers_reveal(state: State<'_, AppState>, id: String) -> Result<String> {
    let provider = key_providers::get(&state.db, &id)?;
    let secret = key_providers::load_key(state.secrets.as_ref(), &provider.id)?;
    activity::info(
        &state.db,
        "provider",
        None,
        format!("查看了供应商「{}」的 API Key", provider.name),
    );
    Ok(secret.expose().to_string())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelsQuery {
    pub base_url: String,
    pub api_format: ApiFormat,
    pub auth_field: AuthField,
    pub api_key: Option<String>,
    pub provider_id: Option<String>,
}

/// 从这家拉模型目录（`/v1/models`；地址带了版本段就是 `{base}/models`）。
#[tauri::command]
pub async fn key_providers_models(
    state: State<'_, AppState>,
    query: ModelsQuery,
) -> Result<Vec<String>> {
    let base = key_providers::normalize_base_url(&query.base_url, query.api_format)?;
    let key = resolve_key(
        &state,
        query.api_key.as_deref(),
        query.provider_id.as_deref(),
    )?;
    let url = key_providers::endpoint(&base, query.api_format, Endpoint::Models);
    let resp = send(http()?.get(&url), &key, query.api_format, query.auth_field).await?;
    let text = read_ok(resp).await?;
    let ids = parse_model_ids(&text);
    if ids.is_empty() {
        return Err(AppError::upstream("这个地址没有返回模型列表。")
            .with_hint("有的中转不开放 /models，模型 id 照它文档手动填就行。"));
    }
    Ok(ids)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingQuery {
    pub provider_id: String,
    /// 空着就测这家清单里的第一个模型。
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPing {
    pub text: String,
    pub model: String,
    pub duration_ms: u64,
}

/// 直接打这家供应商（不经过网关）：钥匙对不对、地址通不通、模型认不认。
#[tauri::command]
pub async fn key_providers_ping(
    state: State<'_, AppState>,
    query: PingQuery,
) -> Result<ProviderPing> {
    let provider = key_providers::get(&state.db, &query.provider_id)?;
    let model = query
        .model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| provider.models.first().cloned())
        .ok_or_else(|| AppError::invalid("这家还没有模型。"))?;
    let secret = key_providers::load_key(state.secrets.as_ref(), &provider.id)?;
    let url = key_providers::endpoint(&provider.base_url, provider.api_format, Endpoint::Chat);
    let body = ping_body(provider.api_format, &model);
    let started = Instant::now();
    let resp = send(
        http()?.post(&url).json(&body),
        secret.expose(),
        provider.api_format,
        provider.auth_field,
    )
    .await?;
    let text = read_ok(resp).await?;
    let duration_ms = started.elapsed().as_millis() as u64;
    let reply =
        extract_text(provider.api_format, &text).unwrap_or_else(|| "（上游没有返回文本）".into());
    state.gateway.channel_forget(PROVIDER, &provider.name);
    Ok(ProviderPing {
        text: reply.chars().take(2000).collect(),
        model,
        duration_ms,
    })
}

fn resolve_key(
    state: &AppState,
    api_key: Option<&str>,
    provider_id: Option<&str>,
) -> Result<String> {
    if let Some(key) = api_key.map(str::trim).filter(|s| !s.is_empty()) {
        return Ok(key.to_string());
    }
    let Some(id) = provider_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err(AppError::invalid("先填 API Key。"));
    };
    let secret = key_providers::load_key(state.secrets.as_ref(), id)?;
    Ok(secret.expose().to_string())
}

fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(8))
        .timeout(Duration::from_secs(40))
        .user_agent("Nexus")
        .build()
        .map_err(|e| AppError::internal(format!("HTTP 客户端初始化失败：{e}")))
}

async fn send(
    req: reqwest::RequestBuilder,
    key: &str,
    format: ApiFormat,
    auth: AuthField,
) -> Result<reqwest::Response> {
    let req = match format {
        ApiFormat::Anthropic if auth == AuthField::ApiKey => req
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        ApiFormat::Anthropic => req
            .header("authorization", format!("Bearer {key}"))
            .header("anthropic-version", "2023-06-01"),
        ApiFormat::OpenaiChat | ApiFormat::OpenaiResponses => {
            req.header("authorization", format!("Bearer {key}"))
        }
    };
    req.send().await.map_err(|e| {
        AppError::network(format!("连不上这个地址：{e}")).with_hint("检查请求地址和网络。")
    })
}

async fn read_ok(resp: reqwest::Response) -> Result<String> {
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| AppError::network(format!("读响应失败：{e}")))?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(AppError::upstream("上游的响应太大了。"));
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if !status.is_success() {
        return Err(upstream_error(status.as_u16(), &text));
    }
    Ok(text)
}

fn upstream_error(status: u16, body: &str) -> AppError {
    let detail = if let Ok(v) = serde_json::from_str::<Value>(body) {
        v.get("error")
            .and_then(|e| {
                e.get("message")
                    .or_else(|| e.get("msg"))
                    .and_then(Value::as_str)
                    .or_else(|| e.as_str())
            })
            .or_else(|| v.get("message").and_then(Value::as_str))
            .map(str::to_string)
    } else {
        None
    };
    let detail = detail.unwrap_or_else(|| {
        body.chars()
            .filter(|c| !c.is_control())
            .take(180)
            .collect::<String>()
    });
    let message = if detail.is_empty() {
        format!("上游返回 {status}")
    } else {
        format!("上游 {status}：{detail}")
    };
    let err = AppError::upstream(message);
    match status {
        401 | 403 => {
            err.with_hint("核对 API Key；Anthropic 格式再看认证字段是 Bearer 还是 x-api-key。")
        }
        404 => err.with_hint("核对请求地址和 API 格式；OpenAI 格式的地址一般要带 /v1。"),
        _ => err,
    }
}

fn parse_model_ids(body: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(body) else {
        return Vec::new();
    };
    let Some(arr) = v
        .get("data")
        .or_else(|| v.get("models"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in arr {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| item.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(id) = id {
            if !out.iter().any(|e: &String| e == id) {
                out.push(id.to_string());
            }
        }
        if out.len() >= key_providers::MAX_MODELS {
            break;
        }
    }
    out
}

fn ping_body(format: ApiFormat, model: &str) -> Value {
    let prompt = "Reply with exactly: pong";
    match format {
        ApiFormat::Anthropic => serde_json::json!({
            "model": model,
            "max_tokens": 32,
            "messages": [{ "role": "user", "content": prompt }],
        }),
        ApiFormat::OpenaiChat => serde_json::json!({
            "model": model,
            "max_tokens": 32,
            "messages": [{ "role": "user", "content": prompt }],
        }),
        ApiFormat::OpenaiResponses => serde_json::json!({
            "model": model,
            "input": prompt,
            "max_output_tokens": 32,
        }),
    }
}

fn extract_text(format: ApiFormat, body: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    match format {
        ApiFormat::Anthropic => v.get("content").and_then(Value::as_array).and_then(|arr| {
            arr.iter()
                .find_map(|part| part.get("text").and_then(Value::as_str).map(str::to_string))
        }),
        ApiFormat::OpenaiChat => v.pointer("/choices/0/message/content").and_then(value_text),
        ApiFormat::OpenaiResponses => v
            .get("output_text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                v.get("output").and_then(Value::as_array).and_then(|items| {
                    items.iter().find_map(|i| {
                        i.pointer("/content/0/text")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                    })
                })
            }),
    }
}

fn value_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => parts
            .iter()
            .find_map(|p| p.get("text").and_then(Value::as_str).map(str::to_string)),
        _ => None,
    }
}
