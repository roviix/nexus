//! 本地网关命令。
//!
//! 网关默认关着；开 / 关 / 换当前号都是用户点击触发。状态一律整份返回（`GatewayStatus`），
//! 前端不用自己拼。**口令只经 `gateway_reveal_key` 一条路出去**，状态里只有「设了没设」。

use crate::commands::events;
use crate::state::AppState;
use nexus_core::{AppError, Result};
use nexus_gateway::models::CatalogEntry;
use nexus_gateway::playground::{self, TryEvent};
use nexus_gateway::{
    GatewaySettings, GatewayStatus, MediaJob, RewriteRule, SettingsPatch, UsageSummary,
};
use nexus_playground::Endpoint;
use nexus_store::activity;
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

#[tauri::command(async)]
pub fn gateway_status(state: State<'_, AppState>) -> Result<GatewayStatus> {
    state.gateway.status()
}

/// 把运行中的网关解成「地址 + 口令」。**只在一次请求里活着**，不缓存、不落库。
///
/// 游乐场每次发送都走它：网关没开就在这里拦下来，几处对着网关发请求的地方说的是同一句话。
pub(crate) fn local_endpoint(state: &AppState) -> Result<Endpoint> {
    let status = state.gateway.status()?;
    let Some(running) = status.running else {
        return Err(AppError::invalid("本地网关没开。").with_hint("先在「本地网关」里开启。"));
    };
    Ok(Endpoint {
        base_url: running.base_url,
        api_key: state.gateway.api_key()?,
    })
}

/// 本地用量：最近 `days` 天方言口处理过的请求，按天 / 按模型 / 按账号聚合。
/// `tz_offset_min` 是前端的本地时区偏移（`-new Date().getTimezoneOffset()`），
/// 「今天」按用户墙上的钟算。
#[tauri::command(async)]
pub fn gateway_usage(
    state: State<'_, AppState>,
    days: u32,
    tz_offset_min: i32,
) -> Result<UsageSummary> {
    state.gateway.usage(days, tz_offset_min)
}

/// IDE Agent 面板经本机网关的用量（Sand 补丁把推理改道到透传口之后才有数据）。
/// 和 `gateway_usage` 分开：那是标准 API 客户端的请求，这是 Cursor 自己每一轮的模型调用，口径不同。
#[tauri::command(async)]
pub fn gateway_ide_usage(
    state: State<'_, AppState>,
    days: u32,
    tz_offset_min: i32,
) -> Result<UsageSummary> {
    state.gateway.ide_usage(days, tz_offset_min)
}

/// 透传口的 Grok Bot 额度开关：开着时 IDE Agent 面板经网关的 `InferenceService/Stream` 用
/// grokBotToken 出去（不动接力队里的号）。热生效；开的时候没凭证会当场去 Grok Bot 生成。
#[tauri::command]
pub async fn gateway_set_grokbot_stream(
    state: State<'_, AppState>,
    on: bool,
) -> Result<GatewayStatus> {
    activity::info(
        &state.db,
        "gateway",
        None,
        if on {
            "透传口：Agent 面板 Stream 改用 Grok Bot 额度"
        } else {
            "透传口：Agent 面板 Stream 回到接力队"
        },
    );
    state.gateway.set_grokbot_stream(on).await
}

/// 改 IDE 拦截的上下文改写规则（开关 / 哨兵位置 / 哨兵文本）。落库并立刻生效，不用重启网关。
/// 改写发出去的是用户自己的对话，所以只能由用户显式点开，且开着时界面常显。
#[tauri::command(async)]
pub fn gateway_set_intercept(
    state: State<'_, AppState>,
    rule: RewriteRule,
) -> Result<GatewayStatus> {
    activity::info(
        &state.db,
        "gateway",
        None,
        if rule.enabled {
            format!("IDE 拦截：开启上下文改写（{:?}）", rule.position)
        } else {
            "IDE 拦截：关闭上下文改写".to_string()
        },
    );
    state.gateway.set_intercept_rule(rule)
}

/// 模型广场的「本地」一列：网关能替客户端映射到的 Cursor 模型，带系列 / 档位 / 别名；
/// 有可用的 ChatGPT 号时再并上 Codex 的对话模型与 `gpt-image-*`（和网关的路由条件一致）。
/// 静态目录，不联网；某个号到底能不能跑其中某个模型，由上游在请求时裁决。
#[tauri::command(async)]
pub fn gateway_models(state: State<'_, AppState>) -> Vec<CatalogEntry> {
    state.gateway.catalog()
}

#[tauri::command]
pub async fn gateway_start(state: State<'_, AppState>) -> Result<GatewayStatus> {
    let status = state.gateway.start().await?;
    if let Some(r) = &status.running {
        activity::info(
            &state.db,
            "gateway",
            None,
            format!("网关已开：{}", r.base_url),
        );
    }
    Ok(status)
}

#[tauri::command]
pub async fn gateway_stop(state: State<'_, AppState>) -> Result<GatewayStatus> {
    let status = state.gateway.stop().await?;
    activity::info(&state.db, "gateway", None, "网关已关");
    Ok(status)
}

#[tauri::command(async)]
pub fn gateway_update_settings(
    state: State<'_, AppState>,
    patch: SettingsPatch,
) -> Result<GatewaySettings> {
    state.gateway.update_settings(patch)
}

/// 把号放进网关的接力队。名单外的号网关一概不碰——这是用户显式点出来的动作，一次可以点几个。
#[tauri::command(async)]
pub fn gateway_enroll(state: State<'_, AppState>, labels: Vec<String>) -> Result<GatewayStatus> {
    for l in &labels {
        activity::info(&state.db, "gateway", Some(l.as_str()), "加入网关号池");
    }
    state.gateway.enroll(&labels)
}

/// 把号移出接力队。它若正是当前号，接力立刻转到下一个。
#[tauri::command(async)]
pub fn gateway_unenroll(state: State<'_, AppState>, label: String) -> Result<GatewayStatus> {
    activity::info(&state.db, "gateway", Some(label.as_str()), "移出网关号池");
    state.gateway.unenroll(&label)
}

/// 指定当前用哪个号。
#[tauri::command(async)]
pub fn gateway_set_current(state: State<'_, AppState>, label: String) -> Result<GatewayStatus> {
    activity::info(
        &state.db,
        "gateway",
        Some(label.as_str()),
        "手动指定网关当前号",
    );
    state.gateway.set_current(&label)
}

/// 清掉耗尽 / 冷却记录（额度刚重置、或者想让它重试）。
#[tauri::command(async)]
pub fn gateway_reset_lane(state: State<'_, AppState>) -> Result<GatewayStatus> {
    state.gateway.reset_lane()
}

/// 订阅通道（chatgpt / grok / kiro）：指定当前用哪个号。
#[tauri::command(async)]
pub fn gateway_channel_set_current(
    state: State<'_, AppState>,
    channel: String,
    label: String,
) -> Result<GatewayStatus> {
    activity::info(
        &state.db,
        &channel,
        Some(label.as_str()),
        "手动指定通道当前号",
    );
    state.gateway.channel_set_current(&channel, &label)
}

/// 订阅通道：清掉耗尽 / 冷却记录。
#[tauri::command(async)]
pub fn gateway_channel_reset_lane(
    state: State<'_, AppState>,
    channel: String,
) -> Result<GatewayStatus> {
    state.gateway.channel_reset_lane(&channel)
}

/// 最近的异步媒体任务（生视频）。
#[tauri::command(async)]
pub fn gateway_media_jobs(state: State<'_, AppState>, limit: Option<usize>) -> Vec<MediaJob> {
    state.gateway.media_jobs(limit.unwrap_or(20))
}

/// 秘密的唯一 IPC 出口。它是本机回环口令，不是上游凭证，但同样只在用户显式点「显示」时给。
#[tauri::command(async)]
pub fn gateway_reveal_key(state: State<'_, AppState>) -> Result<String> {
    activity::info(&state.db, "gateway", None, "查看网关口令");
    state.gateway.api_key()
}

/// 换一把口令。已经配了旧口令的客户端会立刻 401。
#[tauri::command(async)]
pub fn gateway_rotate_key(state: State<'_, AppState>) -> Result<String> {
    activity::info(&state.db, "gateway", None, "更换网关口令");
    state.gateway.rotate_api_key()
}

/// 「试一下」推给前端的一帧：带上请求 id，同一页里连点两次也分得清。
#[derive(Debug, Clone, Serialize)]
pub struct TryFrame {
    pub id: String,
    #[serde(flatten)]
    pub event: TryEvent,
}

/// 模型广场「试一下」：对着**正在运行的**网关发一句话，逐帧用事件推回去。
///
/// 走网关自己的 HTTP 口而不是直接调上游——用户想验证的就是「这个地址、这把口令」。
/// 命令本身等流走完才返回；中途的字都在 `gateway://try` 事件里。
#[tauri::command]
pub async fn gateway_try(
    app: AppHandle,
    state: State<'_, AppState>,
    request_id: String,
    model: String,
    prompt: String,
) -> Result<()> {
    let status = state.gateway.status()?;
    let Some(running) = status.running else {
        return Err(AppError::invalid("网关没开。").with_hint("先在「使用 → 本地网关」里开启。"));
    };
    let key = state.gateway.api_key()?;
    let prompt = prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(AppError::invalid("说点什么再试。"));
    }
    let id = request_id.clone();
    playground::run(&running.base_url, &key, &model, &prompt, |event| {
        let _ = app.emit(
            events::GATEWAY_TRY,
            TryFrame {
                id: id.clone(),
                event,
            },
        );
    })
    .await
}
