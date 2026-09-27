//! 一键接入：把客户端的配置指到本地网关上**它自己的那个口**（`/client/{名字}`），模型按这个
//! 客户端的路由走（见 `nexus_gateway::routes`）。
//!
//! 这一层做三件事：把路由记进网关、确保网关开着（接进来的客户端离不开它，所以顺手设成随应用
//! 启动）、把地址和口令交给 `nexus-connect` 去改文件。**钥匙从头到尾不经过前端**：前端只交
//! 工具名和路由，拿回的是写了哪些文件。
//!
//! 查状态时顺带比对配置里的端口和口令是不是网关现在的——换过口令、端口被占顺延之后，客户端会
//! 悄悄开始 401 / 连不上，这一步把它当场说出来。

use crate::state::AppState;
use nexus_connect::{
    Applied, AuthEnv, ClaudeModels, EnvConflict, Inspection, Layout, Reverted, Target, Tool,
    WireApi,
};
use nexus_core::{AppError, Result};
use nexus_gateway::routes::{ClaudeRole, ClientRoute};
use nexus_gateway::SettingsPatch;
use nexus_store::activity;
use serde::Serialize;
use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tauri::State;

fn layout(state: &AppState) -> Layout {
    Layout::new(state.home_dir.clone(), state.client_backups_dir.clone())
}

const TOOLS: [Tool; 4] = [Tool::ClaudeCode, Tool::Codex, Tool::OpenCode, Tool::Grok];

fn tool_of(id: &str) -> Result<Tool> {
    Tool::parse(id).ok_or_else(|| {
        AppError::invalid(format!("「{id}」没有可以直接写的配置文件。"))
            .with_hint("只有 Claude Code、Codex、OpenCode、Grok CLI 有配置文件；其余工具照配置抄进设置面板即可。")
    })
}

/// 配置现在指向哪里。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PointsTo {
    /// 本机回环地址：本地网关。
    Local,
    /// 配了别的地址（官方直连、别家中转）。
    Other,
    /// 没配 / 文件不存在 / 用的不是我们认得的 provider。
    None,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientState {
    pub tool: &'static str,
    #[serde(flatten)]
    pub inspection: Inspection,
    pub points_to: PointsTo,
    /// 指到的是这个客户端自己的口（按客户端路由选模型）。早先的接入指的是全局 `/v1`，
    /// 走默认通道。
    pub scoped: bool,
    /// 配置里的口令和网关现在的一致。没接到本机时为 `None`。
    pub key_ok: Option<bool>,
    /// 配置里的端口和网关现在的一致。没接到本机时为 `None`。
    pub port_ok: Option<bool>,
    /// 会让这个客户端不照配置走的环境变量。
    pub env: Vec<EnvConflict>,
}

fn classify(base_url: Option<&str>) -> PointsTo {
    let Some(url) = base_url.map(str::trim).filter(|s| !s.is_empty()) else {
        return PointsTo::None;
    };
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://127.0.0.1")
        || lower.starts_with("http://localhost")
        || lower.starts_with("http://[::1]")
    {
        return PointsTo::Local;
    }
    PointsTo::Other
}

/// `http://127.0.0.1:8787/client/claude` → 8787。
fn port_of(url: &str) -> Option<u16> {
    let rest = url.split_once("://")?.1;
    let hostport = rest.split('/').next()?;
    hostport.rsplit_once(':')?.1.parse().ok()
}

fn is_scoped(url: &str, tool: Tool) -> bool {
    url.trim_end_matches('/')
        .trim_end_matches("/v1")
        .ends_with(&format!("/client/{}", tool.id()))
}

fn inspect_one(state: &AppState, tool: Tool, key: Option<&str>, port: u16) -> Result<ClientState> {
    let inspection = nexus_connect::inspect(&layout(state), tool)?;
    let points_to = classify(inspection.base_url.as_deref());
    let local = points_to == PointsTo::Local;
    let url = inspection.base_url.clone().unwrap_or_default();
    Ok(ClientState {
        tool: tool.id(),
        points_to,
        scoped: local && is_scoped(&url, tool),
        key_ok: local.then(|| key.is_some() && inspection.api_key.as_deref() == key),
        port_ok: local.then(|| port_of(&url) == Some(port)),
        env: nexus_connect::env_conflicts(tool, &state.home_dir),
        inspection,
    })
}

/// 某个工具的配置此刻是什么状态。不碰文件内容以外的任何东西，不弹窗、不写。
#[tauri::command(async)]
pub fn connect_inspect(state: State<'_, AppState>, tool: String) -> Result<ClientState> {
    let tool = tool_of(&tool)?;
    let key = state.gateway.api_key_if_set()?;
    inspect_one(&state, tool, key.as_deref(), state.gateway.effective_port())
}

/// 四个客户端一起查：接入页的总览要一眼看全。
#[tauri::command(async)]
pub fn connect_inspect_all(state: State<'_, AppState>) -> Result<Vec<ClientState>> {
    let key = state.gateway.api_key_if_set()?;
    let port = state.gateway.effective_port();
    TOOLS
        .iter()
        .map(|t| inspect_one(&state, *t, key.as_deref(), port))
        .collect()
}

/// 写进客户端配置的模型名：路由主模型去掉通道前缀。客户端作用域里裸名就走路由那条通道，
/// 而 Codex 这类客户端要认得模型名才开得了对应的能力（`chatgpt/gpt-5.4` 它不认）。
fn written_model(model: &str) -> String {
    let m = model.trim();
    match m.split_once('/') {
        Some((head, rest))
            if nexus_gateway::channel::parse_id(head).is_some()
                || head == "codex"
                || head == "xai" =>
        {
            rest.to_string()
        }
        _ => m.to_string(),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectResult {
    #[serde(flatten)]
    pub applied: Applied,
    /// 网关原来关着，这一步把它开了。
    pub gateway_started: bool,
    /// 这一步把「随应用启动」打开了。
    pub autostart_enabled: bool,
    /// 顺手补了 Claude Code 的首次引导标记。
    pub onboarded: bool,
    pub base_url: String,
}

/// 接入 / 更新接入：记下路由 → 确保网关开着 → 备份、合并、写配置。
///
/// 已经接好的客户端改路由也走这里：路由本身即时生效，重写配置只为让客户端菜单里的显示名、
/// 下次启动的默认模型跟上。
#[tauri::command]
pub async fn connect_apply(
    state: State<'_, AppState>,
    tool: String,
    route: ClientRoute,
) -> Result<ConnectResult> {
    let tool = tool_of(&tool)?;
    let route = route.cleaned();
    if route.model.is_empty() {
        return Err(AppError::invalid("先选一个模型。"));
    }
    let written = (tool != Tool::ClaudeCode).then(|| written_model(&route.model));
    state
        .gateway
        .set_route(tool.id(), Some(route.clone()), written.as_deref())?;

    let mut gateway_started = false;
    if !state.gateway.is_running() {
        state.gateway.start().await?;
        gateway_started = true;
    }
    let mut autostart_enabled = false;
    if !state.gateway.settings().autostart {
        state.gateway.update_settings(SettingsPatch {
            autostart: Some(true),
            ..SettingsPatch::default()
        })?;
        autostart_enabled = true;
    }
    let status = state.gateway.status()?;
    let root = status
        .running
        .map(|r| r.base_url)
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", status.settings.port));
    let base_url = format!("{}/client/{}", root.trim_end_matches('/'), tool.id());
    let key = state.gateway.api_key()?;

    let target = match tool {
        Tool::ClaudeCode => Target {
            base_url: base_url.clone(),
            api_key: key,
            model: String::new(),
            claude: Some(ClaudeModels::gateway(
                route.role_model(ClaudeRole::Sonnet),
                route.role_model(ClaudeRole::Opus),
                route.role_model(ClaudeRole::Haiku),
                route.role_model(ClaudeRole::Fable),
                route.context1m,
            )),
            auth_env: AuthEnv::AuthToken,
            wire_api: WireApi::Responses,
        },
        _ => Target::simple(base_url.clone(), key, written.clone().unwrap_or_default()),
    };
    let applied = nexus_connect::apply(&layout(&state), tool, &target)?;
    let onboarded = tool == Tool::ClaudeCode
        && nexus_connect::ensure_claude_onboarded(&state.home_dir).unwrap_or(false);

    activity::info(
        &state.db,
        "connect",
        None,
        format!(
            "已把 {} 接到本地网关（{}）{}",
            tool.label(),
            route.model,
            if gateway_started {
                "，网关已开启"
            } else {
                ""
            }
        ),
    );
    Ok(ConnectResult {
        applied,
        gateway_started,
        autostart_enabled,
        onboarded,
        base_url,
    })
}

/// 撤销接入：按清单把备份拷回去；文件是我们建的就删；没清单就只剔掉我们的键。
/// 路由留着——再接一次时还是这套选择。
#[tauri::command(async)]
pub fn connect_revert(state: State<'_, AppState>, tool: String) -> Result<Reverted> {
    let tool = tool_of(&tool)?;
    let reverted = nexus_connect::revert(&layout(&state), tool)?;
    activity::info(
        &state.db,
        "connect",
        None,
        format!(
            "已撤销 {} 的接入（还原 {} · 删除 {} · 剔键 {}）",
            tool.label(),
            reverted.restored.len(),
            reverted.removed.len(),
            reverted.stripped.len()
        ),
    );
    Ok(reverted)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectTest {
    pub ok: bool,
    pub text: String,
    /// 客户端报的名字（Claude Code 是档名）。
    pub requested: String,
    /// 路由换算后的模型、落在哪条通道哪个号：从网关的请求明细里取。
    pub target: Option<String>,
    pub channel: Option<String>,
    pub account: Option<String>,
    pub duration_ms: u64,
    pub error: Option<String>,
}

/// 照这个客户端的样子发一句：同一个口、同一种方言、配置里写的那个模型名。验证的就是
/// 「它现在打过来会落到哪、通不通」，不是随便挑个模型试试。
#[tauri::command]
pub async fn connect_test(
    state: State<'_, AppState>,
    tool: String,
    prompt: Option<String>,
) -> Result<ConnectTest> {
    let tool = tool_of(&tool)?;
    let status = state.gateway.status()?;
    let Some(running) = status.running else {
        return Err(AppError::invalid("本地网关没开。")
            .with_hint("接入一次会自动开启；也可以去「本地网关」页打开。"));
    };
    let route = status.routes.get(tool.id()).cloned();
    let key = state.gateway.api_key()?;
    let prompt = prompt
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "用一句话介绍你自己，并说出你是哪个模型。".into());
    let root = format!(
        "{}/client/{}/v1",
        running.base_url.trim_end_matches('/'),
        tool.id()
    );
    let requested = match tool {
        Tool::ClaudeCode => sonnet_alias(route.as_ref()),
        _ => route
            .as_ref()
            .map(|r| written_model(&r.model))
            .unwrap_or_default(),
    };
    let (url, body) = match tool {
        Tool::ClaudeCode => (
            format!("{root}/messages"),
            json!({ "model": requested, "max_tokens": 256, "messages": [{ "role": "user", "content": prompt }] }),
        ),
        Tool::OpenCode => (
            format!("{root}/chat/completions"),
            json!({ "model": requested, "max_tokens": 256, "messages": [{ "role": "user", "content": prompt }] }),
        ),
        Tool::Codex | Tool::Grok => (
            format!("{root}/responses"),
            json!({ "model": requested, "max_output_tokens": 256, "input": prompt, "stream": false }),
        ),
    };
    let started = Instant::now();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| AppError::internal(format!("HTTP 客户端初始化失败：{e}")))?;
    let resp = client
        .post(&url)
        .bearer_auth(&key)
        .header("x-api-key", &key)
        .header("anthropic-version", "2023-06-01")
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::network(format!("连不上本地网关：{e}")))?;
    let status_code = resp.status();
    let value: Value = resp.json().await.unwrap_or(Value::Null);
    let duration_ms = started.elapsed().as_millis() as u64;
    let latest = state.gateway.requests(1).into_iter().next();
    let (target, channel, account) = latest
        .map(|e| (e.target, e.channel, e.account))
        .unwrap_or((None, None, None));
    if !status_code.is_success() {
        let message = value
            .pointer("/error/message")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("网关返回了错误")
            .to_string();
        return Ok(ConnectTest {
            ok: false,
            text: String::new(),
            requested,
            target,
            channel,
            account,
            duration_ms,
            error: Some(format!("{}：{message}", status_code.as_u16())),
        });
    }
    Ok(ConnectTest {
        ok: true,
        text: reply_text(&value).chars().take(2000).collect(),
        requested,
        target,
        channel,
        account,
        duration_ms,
        error: None,
    })
}

/// Claude Code 平时默认发的是 Sonnet 那一档的档名。
fn sonnet_alias(route: Option<&ClientRoute>) -> String {
    let one_m = route.is_some_and(|r| r.context1m);
    let m = ClaudeModels::gateway("", "", "", "", one_m);
    m.sonnet.model
}

fn reply_text(v: &Value) -> String {
    if let Some(parts) = v.get("content").and_then(Value::as_array) {
        return parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("");
    }
    if let Some(s) = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
    {
        return s.to_string();
    }
    if let Some(s) = v.get("output_text").and_then(Value::as_str) {
        return s.to_string();
    }
    v.get("output")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|i| i.get("type").and_then(Value::as_str) == Some("message"))
                .flat_map(|i| {
                    i.get("content")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|p| p.get("text").and_then(Value::as_str).map(str::to_string))
                .collect::<Vec<_>>()
                .join("")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_tells_local_and_other_apart() {
        assert_eq!(classify(Some("http://127.0.0.1:8787")), PointsTo::Local);
        assert_eq!(classify(Some("http://localhost:9000/v1")), PointsTo::Local);
        assert_eq!(classify(Some("https://api.anthropic.com")), PointsTo::Other);
        assert_eq!(classify(None), PointsTo::None);
        assert_eq!(classify(Some("  ")), PointsTo::None);
    }

    #[test]
    fn unknown_tools_are_refused_with_a_hint() {
        let err = tool_of("cline").unwrap_err();
        assert!(err.hint.is_some());
        assert_eq!(tool_of("claude").unwrap(), Tool::ClaudeCode);
        assert_eq!(tool_of("codex").unwrap(), Tool::Codex);
    }

    #[test]
    fn scope_and_port_are_read_off_the_url() {
        assert!(is_scoped(
            "http://127.0.0.1:8787/client/claude",
            Tool::ClaudeCode
        ));
        assert!(is_scoped(
            "http://127.0.0.1:8787/client/codex/v1",
            Tool::Codex
        ));
        assert!(!is_scoped("http://127.0.0.1:8787/v1", Tool::Codex));
        assert!(!is_scoped(
            "http://127.0.0.1:8787/client/claude",
            Tool::Codex
        ));
        assert_eq!(port_of("http://127.0.0.1:8790/client/codex/v1"), Some(8790));
        assert_eq!(port_of("http://localhost/v1"), None);
    }

    #[test]
    fn written_model_drops_only_channel_prefixes() {
        assert_eq!(written_model("chatgpt/gpt-5.4"), "gpt-5.4");
        assert_eq!(written_model("provider/deepseek-v4-pro"), "deepseek-v4-pro");
        assert_eq!(written_model("codex/gpt-5.5"), "gpt-5.5");
        assert_eq!(
            written_model("openrouter/anthropic/claude"),
            "openrouter/anthropic/claude"
        );
        assert_eq!(written_model("gpt-5.4"), "gpt-5.4");
    }

    #[test]
    fn reply_text_reads_all_three_dialects() {
        assert_eq!(
            reply_text(&json!({ "content": [{ "type": "text", "text": "a" }] })),
            "a"
        );
        assert_eq!(
            reply_text(&json!({ "choices": [{ "message": { "content": "b" } }] })),
            "b"
        );
        assert_eq!(
            reply_text(
                &json!({ "output": [{ "type": "message", "content": [{ "type": "output_text", "text": "c" }] }] })
            ),
            "c"
        );
    }
}
