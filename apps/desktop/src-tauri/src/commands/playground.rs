//! 游乐场命令：会话的增删改、发一轮对话、出一批图、停、看进行中的。
//!
//! 这一层只做两件事：解出本地网关的地址与口令（`gateway::local_endpoint`），
//! 以及把流式回字变成 `playground://chat` 事件。持久化与历史拼装都在 `nexus-playground`。
//! 图片字节不走 IPC —— 前端凭 id 经 `nexus-image://` 协议取（见 `lib.rs`）。

use crate::commands::events;
use crate::commands::gateway::{local_endpoint, TryFrame};
use crate::state::AppState;
use nexus_core::{AppError, Result};
use nexus_playground::{
    ActiveRun, Asset, Attachment, ImageRequest, Kind, Message, Source, Thread, ThreadDetail,
    ThreadSummary, VideoRequest,
};
use tauri::{AppHandle, Emitter, State};

#[tauri::command(async)]
pub fn playground_threads(
    state: State<'_, AppState>,
    kind: Option<Kind>,
) -> Result<Vec<ThreadSummary>> {
    state.playground.threads(kind)
}

#[tauri::command(async)]
pub fn playground_thread(state: State<'_, AppState>, id: String) -> Result<ThreadDetail> {
    state.playground.thread(&id)
}

#[tauri::command(async)]
pub fn playground_thread_create(
    state: State<'_, AppState>,
    kind: Kind,
    model: String,
) -> Result<Thread> {
    state
        .playground
        .create_thread(kind, Source::Local, &model, None)
}

#[tauri::command(async)]
pub fn playground_thread_rename(
    state: State<'_, AppState>,
    id: String,
    title: String,
) -> Result<Thread> {
    state.playground.rename_thread(&id, &title)
}

/// 换这个会话下一次发送用的模型。
#[tauri::command(async)]
pub fn playground_thread_set_target(
    state: State<'_, AppState>,
    id: String,
    model: String,
) -> Result<Thread> {
    state
        .playground
        .set_target(&id, Source::Local, &model, None)
}

#[tauri::command(async)]
pub fn playground_thread_delete(state: State<'_, AppState>, id: String) -> Result<()> {
    state.playground.delete_thread(&id)
}

#[tauri::command(async)]
pub fn playground_message_delete(state: State<'_, AppState>, id: String) -> Result<()> {
    state.playground.delete_message(&id)
}

/// 这个会话有没有请求正在跑（切走再切回来时接上半截回复）。
#[tauri::command(async)]
pub fn playground_active(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<Option<ActiveRun>> {
    Ok(state.playground.active(&thread_id))
}

/// 发一轮对话。`prompt` 缺省 = 重新生成末尾那条回复。
///
/// `attachments` 是随这一句发上去的图（base64 进来，落盘后挂在 user 消息上）；缺省为空，
/// 只发字的调用方不必带它。
///
/// 命令本身等流走完才返回，返回的是**已落库**的那条回复；中途的字都在 `playground://chat`
/// 事件里，按 `request_id` 认。
#[tauri::command]
pub async fn playground_chat_send(
    app: AppHandle,
    state: State<'_, AppState>,
    request_id: String,
    thread_id: String,
    prompt: Option<String>,
    attachments: Option<Vec<Attachment>>,
) -> Result<Message> {
    if request_id.trim().is_empty() {
        return Err(AppError::invalid("缺少 request_id。"));
    }
    state.playground.thread(&thread_id)?;
    let ep = local_endpoint(&state)?;
    let id = request_id.clone();
    state
        .playground
        .chat(
            &request_id,
            &thread_id,
            prompt.as_deref(),
            attachments.as_deref().unwrap_or(&[]),
            &ep,
            |event| {
                let _ = app.emit(
                    events::PLAYGROUND_CHAT,
                    TryFrame {
                        id: id.clone(),
                        event,
                    },
                );
            },
        )
        .await
}

/// 停掉一次进行中的请求（对话或出图）。没有这么一次也不算错——用户可能连点了两下。
#[tauri::command(async)]
pub fn playground_stop(state: State<'_, AppState>, request_id: String) -> Result<bool> {
    Ok(state.playground.stop(&request_id))
}

/// 出一批图。同样等做完才返回；图片会话没有流式，进度只有「在跑 / 跑完」。
///
/// 走 `{base}/v1/images/generations`：本地网关那条背后是 Cursor 的 `RunGenerateImage`
/// （固定 1536×1024、账号要有 Developer / Sand 计划的生图权限），权限不够时网关会把原因
/// 写进错误体，这里照样落成一条带 `error` 的回复。
#[tauri::command]
pub async fn playground_image_generate(
    state: State<'_, AppState>,
    request_id: String,
    thread_id: String,
    request: ImageRequest,
) -> Result<Message> {
    if request_id.trim().is_empty() {
        return Err(AppError::invalid("缺少 request_id。"));
    }
    state.playground.thread(&thread_id)?;
    let ep = local_endpoint(&state)?;
    state
        .playground
        .generate_image(&request_id, &thread_id, request, &ep)
        .await
}

/// 出一段视频。上游是异步任务，这里等它做完（最多几分钟）再返回；中途点「停」就不再等。
/// 走 `{base}/v1/videos/*`：只有 Grok 通道有；没有时网关会回 404，落成带 `error` 的回复。
#[tauri::command]
pub async fn playground_video_generate(
    state: State<'_, AppState>,
    request_id: String,
    thread_id: String,
    request: VideoRequest,
) -> Result<Message> {
    if request_id.trim().is_empty() {
        return Err(AppError::invalid("缺少 request_id。"));
    }
    state.playground.thread(&thread_id)?;
    let ep = local_endpoint(&state)?;
    state
        .playground
        .generate_video(&request_id, &thread_id, request, &ep)
        .await
}

/// 资产页：全部生成过的图，新的在前。
#[tauri::command(async)]
pub fn playground_assets(state: State<'_, AppState>) -> Result<Vec<Asset>> {
    state.playground.assets()
}

/// 删一张图（连盘上的文件）。消息本身留着。
#[tauri::command(async)]
pub fn playground_image_delete(state: State<'_, AppState>, id: String) -> Result<()> {
    state.playground.delete_image(&id)
}

/// 在系统文件管理器里显示这张图。
///
/// **必须留在主线程，不要加 `(async)`。** 理由同 `backup_reveal`：`reveal_item_in_dir`
/// 在 macOS 上直接调 AppKit 的 `NSWorkspace`，插件没有自己往主队列派发。
#[tauri::command]
pub fn playground_image_reveal(
    app: AppHandle,
    state: State<'_, AppState>,
    id: String,
) -> Result<()> {
    let (path, _) = state
        .playground
        .image_path(&id)?
        .ok_or_else(|| AppError::invalid("这张图已经不在了。"))?;
    if !path.is_file() {
        return Err(AppError::invalid("这张图的文件已经不在了。")
            .with_hint("它可能被手动删掉了，或这份记录来自别的机器的备份。"));
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|e| AppError::internal(format!("打不开文件管理器：{e}")))
}
