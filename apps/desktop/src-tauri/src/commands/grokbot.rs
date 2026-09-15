//! Grok Bot 桥命令。
//!
//! **不是账号管理**：这里不建账号、不存账号。Cursor 账号仍在 `accounts_*`；Grok Bot 只是 Sand 补丁 /
//! 本机网关按需借额度的来源——装没装、登没登录、拉起它、刷两份配置文件。凭证（grokBotToken /
//! Box token）只在 Rust 侧与磁盘文件之间流动，状态里只有「有没有、几点过期」。
//!
//! 读钥匙串的动作（刷 Box Relay / 生成直连凭证）在 GUI 里第一次会弹系统授权窗，所以都由用户点击触发。

use crate::commands::switcher::run_blocking;
use crate::state::AppState;
use nexus_accounts::{Account, NewAccount};
use nexus_core::{AccountId, Result};
use nexus_grokbot::{
    BoxRelayDescriptor, CuaProbe, GrokBotIdentity, GrokBotStatus, StreamCredential, CUA_PROBE_MODEL,
};
use nexus_store::activity;
use serde::Serialize;
use std::time::Duration;
use tauri::State;

/// 界面用的直连凭证摘要（不含 token）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectMinted {
    pub account_email: Option<String>,
    pub expires_at_ms: Option<u64>,
}

impl From<StreamCredential> for DirectMinted {
    fn from(c: StreamCredential) -> Self {
        Self {
            expires_at_ms: c.expires_at_ms(),
            account_email: c.account_email,
        }
    }
}

/// 界面用的 Box Relay 摘要（不含 token）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayRefreshed {
    pub base_url: String,
    pub relay_path: String,
    pub account_fingerprint: Option<String>,
}

impl From<BoxRelayDescriptor> for RelayRefreshed {
    fn from(d: BoxRelayDescriptor) -> Self {
        Self {
            base_url: d.base_url,
            relay_path: d.relay_path,
            account_fingerprint: d.account_fingerprint,
        }
    }
}

/// 只读、不碰钥匙串：装没装 / 登没登录 / 两份本地配置的状态。
#[tauri::command(async)]
pub fn grokbot_status(state: State<'_, AppState>) -> Result<GrokBotStatus> {
    Ok(state.grokbot.status())
}

/// 打开（或前置）Grok Bot 客户端。
#[tauri::command(async)]
pub fn grokbot_launch(state: State<'_, AppState>) -> Result<()> {
    activity::info(&state.db, "grokbot", None, "打开 Grok Bot");
    state.grokbot.launch()
}

/// Grok Bot 登着谁（要解钥匙串，首次弹授权）。账号抽屉用它判断「这个号是不是 Grok Bot 正在用的」。
#[tauri::command]
pub async fn grokbot_identify(state: State<'_, AppState>) -> Result<GrokBotIdentity> {
    let gb = state.grokbot.clone();
    run_blocking(move || gb.identify()).await
}

/// 把 Grok Bot 当前登着的账号收进「我的账号」（email + refresh token，与 OAuth 授权拿到的同一种凭证）。
/// 已在库里就只补 refresh token。明文从没出过 Rust。
#[tauri::command]
pub async fn grokbot_import_active_account(state: State<'_, AppState>) -> Result<Account> {
    let gb = state.grokbot.clone();
    let exported = run_blocking(move || gb.export_active_account()).await?;
    let account = state.accounts.repo.upsert(NewAccount {
        email: exported.email,
        refresh_token: Some(exported.refresh_token),
        note: Some("来自 Grok Bot".into()),
        ..NewAccount::default()
    })?;
    activity::info(
        &state.db,
        "accounts",
        Some(&account.email),
        "从 Grok Bot 收进账号库",
    );
    Ok(account)
}

/// 从 Grok Bot 读 gateway descriptor，写 `grok-box-relay.json`（Box Relay 模式的前提）。
#[tauri::command]
pub async fn grokbot_relay_refresh(state: State<'_, AppState>) -> Result<RelayRefreshed> {
    let gb = state.grokbot.clone();
    let db = state.db.clone();
    run_blocking(move || {
        let d = gb.refresh_relay()?;
        activity::info(
            &db,
            "grokbot",
            None,
            format!(
                "刷新 Box Relay：{}",
                d.account_fingerprint.as_deref().unwrap_or("-")
            ),
        );
        Ok(RelayRefreshed::from(d))
    })
    .await
}

#[tauri::command(async)]
pub fn grokbot_relay_clear(state: State<'_, AppState>) -> Result<()> {
    state.grokbot.clear_relay()
}

/// 生成 / 重新生成直连凭证：descriptor → pod 读 `sbi_*` → 续期 → 落盘。直连模式与网关 Grok 开关的前提。
#[tauri::command]
pub async fn grokbot_mint_direct(state: State<'_, AppState>) -> Result<DirectMinted> {
    let gb = state.grokbot.clone();
    let cred = gb.mint_direct().await?;
    activity::info(
        &state.db,
        "grokbot",
        None,
        format!(
            "生成直连凭证：{}",
            cred.account_email.as_deref().unwrap_or("-")
        ),
    );
    Ok(DirectMinted::from(cred))
}

/// **不经 Grok Bot 客户端**：用「我的账号」里这个号直接换到 grokBotToken，写成直连凭证。
/// 之后 Sand 直连补丁 / 网关的 Grok 开关下一发就在花这个号的额度——这就是「无感切号」。
/// 副作用：会给这个号建（或唤醒）一个 Box pod，和在 Grok Bot 里登录它是同一件事。
#[tauri::command]
pub async fn grokbot_mint_for_account(
    state: State<'_, AppState>,
    id: AccountId,
) -> Result<DirectMinted> {
    let account = state.accounts.repo.get(&id)?;
    let session = state.accounts.session(&id).await?;
    let access = session.access_token.expose().to_string();
    // machineId 用这个号在网关里的派生机器码：和网关一致，不额外多出一台「电脑」。
    let machine_id = nexus_gateway::DeviceIdentity::derived(&access).machine_id;
    let cred = state
        .grokbot
        .mint_direct_for_account(&account.email, &access, &machine_id)
        .await?;
    activity::info(
        &state.db,
        "grokbot",
        Some(&account.email),
        "Agent 面板改用这个号的 Grok 额度（直连凭证已生成）",
    );
    Ok(DirectMinted::from(cred))
}

/// 手动续一次直连 token（正常不需要——补丁 / 网关快过期会自己续；给「现在就验证一下」用）。
#[tauri::command]
pub async fn grokbot_renew_direct(state: State<'_, AppState>) -> Result<DirectMinted> {
    let gb = state.grokbot.clone();
    Ok(DirectMinted::from(gb.fresh_stream_credential().await?))
}

#[tauri::command(async)]
pub fn grokbot_clear_direct(state: State<'_, AppState>) -> Result<()> {
    state.grokbot.clear_direct()
}

/// 忘掉缓存的钥匙串口令 / 解出来的秘密（Grok Bot 换了账号或重装后用）。
#[tauri::command(async)]
pub fn grokbot_forget(state: State<'_, AppState>) -> Result<()> {
    state.grokbot.forget_secrets();
    Ok(())
}

/// 上次探过这个号的 `sand-cua` 落点（磁盘缓存，不含 token）。
#[tauri::command(async)]
pub fn grokbot_cua_probe_get(
    state: State<'_, AppState>,
    id: AccountId,
) -> Result<Option<CuaProbe>> {
    let account = state.accounts.repo.get(&id)?;
    Ok(state.grokbot.cua_probe_for(&account.email))
}

/// 打一发 `sand-cua`，看这个号有没有 grok 4.7 灰度。
///
/// 等到已知分片（4.7 / luna）或出字后的干净 ResponseInfo 再挂断。
/// 不覆盖本机正在用的直连凭证：当前凭证就是这个号才续用，否则内存里换一把。
/// 副作用：会唤醒（或新建）这个号的 Box pod，并消耗很少额度。
#[tauri::command]
pub async fn grokbot_cua_probe(state: State<'_, AppState>, id: AccountId) -> Result<CuaProbe> {
    let account = state.accounts.repo.get(&id)?;
    let session = state.accounts.session(&id).await?;
    let access = session.access_token.expose().to_string();
    let machine_id = nexus_gateway::DeviceIdentity::derived(&access).machine_id;

    let cred = match state
        .grokbot
        .stream_credential_for_probe(&account.email)
        .await?
    {
        Some(c) => c,
        None => {
            state
                .grokbot
                .mint_direct_for_account_ephemeral(&account.email, &access, &machine_id)
                .await?
        }
    };

    let identity =
        nexus_gateway::DeviceIdentity::pinned(&cred.grok_bot_token, cred.machine_id.clone());
    // thinking 模型（4.7）首 token 可能要十几秒；总时长给够等 ResponseInfo。
    let cfg = nexus_gateway::StreamConfig {
        client_type: "sand".into(),
        idle_timeout: Duration::from_secs(35),
        max_turn: Duration::from_secs(55),
        ..nexus_gateway::StreamConfig::default()
    };
    let client = nexus_gateway::http_client();
    let probed = nexus_gateway::probe_routed_model(
        &client,
        &cfg,
        &cred.grok_bot_token,
        &identity,
        CUA_PROBE_MODEL,
    )
    .await;

    let now = nexus_grokbot::credential::now_ms();
    let probe = match probed {
        Ok(r) => match r.resolved_model {
            Some(model) => CuaProbe {
                email: account.email.clone(),
                requested_model: CUA_PROBE_MODEL.into(),
                resolved_model: Some(model.clone()),
                has_grok47: nexus_gateway::models::is_grok47_request(&model),
                probed_at_ms: now,
                error: None,
            },
            None => CuaProbe {
                email: account.email.clone(),
                requested_model: CUA_PROBE_MODEL.into(),
                resolved_model: None,
                has_grok47: false,
                probed_at_ms: now,
                error: Some(r.note.unwrap_or_else(|| "上游没回落到哪个模型。".into())),
            },
        },
        Err(e) => CuaProbe {
            email: account.email.clone(),
            requested_model: CUA_PROBE_MODEL.into(),
            resolved_model: None,
            has_grok47: false,
            probed_at_ms: now,
            error: Some(e.message),
        },
    };
    state.grokbot.save_cua_probe(&probe)?;
    activity::info(
        &state.db,
        "grokbot",
        Some(&account.email),
        match (probe.has_grok47, probe.resolved_model.as_deref()) {
            (true, Some(m)) => format!("4.7 灰度：有（{m}）"),
            (false, Some(m)) => format!("4.7 灰度：无（sand-cua → {m}）"),
            _ => format!(
                "4.7 灰度：未测到（{}）",
                probe.error.as_deref().unwrap_or("无模型名")
            ),
        },
    );
    Ok(probe)
}
