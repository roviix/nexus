//! ChatGPT 订阅号命令：授权登录、导入、启停、额度。
//!
//! 这些号只有一个用途——给本地网关出 Codex 流量——所以命令都挂在网关页的「ChatGPT 账号」
//! 卡片上。状态整份返回（账号列表 / `GatewayStatus`），前端不用自己拼。**凭证从不经过 IPC。**

use crate::commands::events;
use crate::state::AppState;
use nexus_chatgpt::{
    ChatGptAccount, ChatGptBilling, ChatGptService, CodexUsage, ImportOutcome, LocalTraffic,
    LoginHandle, LoginState, ManifestModel,
};
use nexus_core::{AppError, ChatGptAccountId, ErrorCode, Result};
use nexus_store::activity;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

#[tauri::command(async)]
pub fn chatgpt_list(state: State<'_, AppState>) -> Result<Vec<ChatGptAccount>> {
    let mut list = state.chatgpt.list()?;
    attach_local_traffic(&state, &mut list);
    Ok(list)
}

/// 账本按邮箱记。对不上（还没走过网关、或导入时没邮箱）就空着，别编 0。
fn attach_local_traffic(state: &AppState, list: &mut [ChatGptAccount]) {
    let Ok(rows) = state.gateway.channel_account_totals("chatgpt", 90) else {
        return;
    };
    if rows.is_empty() {
        return;
    }
    let by_name: std::collections::HashMap<String, _> =
        rows.into_iter().map(|r| (r.name.clone(), r)).collect();
    for account in list {
        let key = account.label().to_ascii_lowercase();
        if let Some(row) = by_name.get(&key) {
            account.traffic = Some(LocalTraffic {
                requests: row.calls,
                tokens: row.tokens,
                errors: row.errors,
                days: 90,
            });
        }
    }
}

/// 上次从上游拉到的模型目录（按当时那个号的套餐筛过）。空 = 还没拉过，网关用静态清单。
#[tauri::command(async)]
pub fn chatgpt_models(state: State<'_, AppState>) -> Vec<ManifestModel> {
    state.chatgpt.models()
}

/// 现在拉一次目录。新模型上线、换了套餐时点它；加号和开网关时也会自动拉一次。
#[tauri::command]
pub async fn chatgpt_refresh_models(state: State<'_, AppState>) -> Result<Vec<ManifestModel>> {
    let models = state.chatgpt.refresh_models_any().await?;
    activity::info(
        &state.db,
        "chatgpt",
        None,
        format!("拉到 Codex 模型目录：{} 个", models.len()),
    );
    Ok(models)
}

/// 号刚进来：顺手用它拉一次目录，失败只记日志——目录是锦上添花，不是进池的条件。
fn refresh_models_soon(chatgpt: Arc<ChatGptService>, id: ChatGptAccountId) {
    tauri::async_runtime::spawn(async move {
        if let Err(err) = chatgpt.refresh_models(&id).await {
            tracing::info!(%err, "刚加的号拉模型目录失败，沿用现有清单");
        }
    });
}

/// 发起授权：拼好授权链接、在本机 1455 上把回调监听绑起来、用系统浏览器打开。
///
/// 用**默认**浏览器而不是隐私窗口（Cursor 授权那边是隐私窗口）：用户平时登着 ChatGPT 的
/// 就是默认配置文件，隐私窗口只会逼他再登一遍。监听绑成功时后台等回调、完成后经
/// `chatgpt://login` 事件通知；绑不成功（多半是 `codex login` 正在跑）前端退回「贴回调地址」。
#[tauri::command]
pub async fn chatgpt_login_start(
    app: AppHandle,
    state: State<'_, AppState>,
    note: Option<String>,
) -> Result<LoginHandle> {
    let handle = state.chatgpt.start_login(note).await?;
    open_default_browser(&app, &handle.authorize_url)?;
    activity::info(&state.db, "chatgpt", None, "发起 ChatGPT 授权");

    if handle.callback_listening {
        let chatgpt = state.chatgpt.clone();
        let db = state.db.clone();
        let emitter = app.clone();
        let session_id = handle.session_id.clone();
        tauri::async_runtime::spawn(async move {
            let result = chatgpt
                .wait_login(&session_id, &|st: LoginState| {
                    let _ = emitter.emit(events::CHATGPT_LOGIN, st);
                })
                .await;
            match result {
                Ok(up) => {
                    activity::info(
                        &db,
                        "chatgpt",
                        Some(&up.account.label()),
                        if up.created {
                            "已通过授权添加 ChatGPT 账号"
                        } else {
                            "已重新授权 ChatGPT 账号"
                        },
                    );
                    refresh_models_soon(chatgpt.clone(), up.account.id.clone());
                }
                Err(err) if err.code != ErrorCode::Cancelled => activity::warn(
                    &db,
                    "chatgpt",
                    None,
                    format!("ChatGPT 授权未完成：{}", err.message),
                ),
                Err(_) => {}
            }
        });
    }
    Ok(handle)
}

/// 手贴路径：用户把浏览器地址栏的回调地址贴回来。
#[tauri::command]
pub async fn chatgpt_login_complete(
    state: State<'_, AppState>,
    session_id: String,
    callback: String,
) -> Result<ChatGptAccount> {
    let up = state.chatgpt.complete_login(&session_id, &callback).await?;
    activity::info(
        &state.db,
        "chatgpt",
        Some(&up.account.label()),
        if up.created {
            "已通过授权添加 ChatGPT 账号"
        } else {
            "已重新授权 ChatGPT 账号"
        },
    );
    refresh_models_soon(state.chatgpt.clone(), up.account.id.clone());
    Ok(up.account)
}

#[tauri::command(async)]
pub fn chatgpt_login_cancel(state: State<'_, AppState>, session_id: String) -> Result<()> {
    state.chatgpt.cancel_login(&session_id);
    Ok(())
}

/// 从本机 Codex CLI 的登录态导入（`$CODEX_HOME/auth.json`，默认 `~/.codex/auth.json`）。
#[tauri::command]
pub async fn chatgpt_import_codex_cli(state: State<'_, AppState>) -> Result<ChatGptAccount> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| state.home_dir.join(".codex"));
    let up = state.chatgpt.import_codex_cli(&home).await?;
    activity::info(
        &state.db,
        "chatgpt",
        Some(&up.account.label()),
        "从本机 Codex CLI 导入了 ChatGPT 账号",
    );
    refresh_models_soon(state.chatgpt.clone(), up.account.id.clone());
    Ok(up.account)
}

/// 导入一段文本：`auth.json`、sub2api 的 Codex session JSON（数组 / 多行）、
/// `access----refresh`，或单独一个 refresh token。可以一次贴多个。只有 refresh
/// token 的条目会先刷一次，所以可能联网。明文只进 Rust，不落日志。
#[tauri::command]
pub async fn chatgpt_import_text(
    state: State<'_, AppState>,
    text: String,
    note: Option<String>,
) -> Result<ImportOutcome> {
    let out = state.chatgpt.import_dump(&text, note.as_deref()).await?;
    activity::info(
        &state.db,
        "chatgpt",
        None,
        format!(
            "导入 ChatGPT 账号：新建 {}，更新 {}{}",
            out.created,
            out.updated,
            if out.failed > 0 {
                format!("，{} 个没进去", out.failed)
            } else {
                String::new()
            }
        ),
    );
    if out.accepted() > 0 {
        let chatgpt = state.chatgpt.clone();
        tauri::async_runtime::spawn(async move {
            if let Err(err) = chatgpt.refresh_models_any().await {
                tracing::info!(%err, "导入后拉模型目录失败，沿用现有清单");
            }
        });
    }
    Ok(out)
}

#[tauri::command(async)]
pub fn chatgpt_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    let id = ChatGptAccountId::from_raw(id);
    let label = state.chatgpt.get(&id)?.label();
    state.chatgpt.remove(&id)?;
    state.gateway.channel_forget("chatgpt", &label);
    activity::info(&state.db, "chatgpt", Some(&label), "已删除 ChatGPT 账号");
    Ok(())
}

/// 开 / 关这个号进网关接力队。关掉的号立刻不再是当前号。
#[tauri::command(async)]
pub fn chatgpt_set_enabled(
    state: State<'_, AppState>,
    id: String,
    enabled: bool,
) -> Result<ChatGptAccount> {
    let id = ChatGptAccountId::from_raw(id);
    let account = state.chatgpt.set_enabled(&id, enabled)?;
    if !enabled {
        state.gateway.channel_forget("chatgpt", &account.label());
    }
    activity::info(
        &state.db,
        "chatgpt",
        Some(&account.label()),
        if enabled {
            "ChatGPT 账号已加入网关"
        } else {
            "ChatGPT 账号已暂停"
        },
    );
    Ok(account)
}

#[tauri::command(async)]
pub fn chatgpt_set_note(
    state: State<'_, AppState>,
    id: String,
    note: Option<String>,
) -> Result<ChatGptAccount> {
    let note = note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
    state
        .chatgpt
        .set_note(&ChatGptAccountId::from_raw(id), note.as_deref())
}

/// 主动问一次额度（`/wham/usage`）。平时不用点：每次经网关的请求都会把额度头写回来。
/// 用量到手后会顺带读一次订阅账单；账单失败不影响额度快照。
#[tauri::command]
pub async fn chatgpt_refresh_usage(state: State<'_, AppState>, id: String) -> Result<CodexUsage> {
    state
        .chatgpt
        .refresh_usage(&ChatGptAccountId::from_raw(id))
        .await
}

/// 主动问一次订阅（`accounts/check`，必要时再问 `subscriptions`）。
/// 没有标价和发票——Codex OAuth 打不开 ChatGPT 的 Stripe 门户。
#[tauri::command]
pub async fn chatgpt_refresh_billing(
    state: State<'_, AppState>,
    id: String,
) -> Result<ChatGptBilling> {
    state
        .chatgpt
        .refresh_billing(&ChatGptAccountId::from_raw(id))
        .await
}

fn open_default_browser(app: &AppHandle, url: &str) -> Result<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(url, None::<&str>).map_err(|err| {
        AppError::internal(format!("打不开系统浏览器：{err}"))
            .with_hint("手动复制授权链接到浏览器里打开也可以。")
    })
}
