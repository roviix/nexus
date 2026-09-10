//! 「我的账号」命令：托管、授权、刷用量、取码。

use crate::commands::events;
use crate::state::AppState;
use nexus_accounts::{
    Account, AccountPatch, AccountUsage, ActiveSession, KickOutcome, NewAccount, OauthSession,
    OauthState,
};
use nexus_core::{AccountId, AppError, Clock, ErrorCode, Result};
use nexus_cursor::AuthBundle;
use nexus_store::activity;
use nexus_store::keys::AccountSecret;
use nexus_switcher::SwitchProfile;
use serde::Serialize;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};

#[tauri::command(async)]
pub fn accounts_list(state: State<'_, AppState>) -> Result<Vec<Account>> {
    state.accounts.repo.list()
}

/// 手工添加一个号。托管门槛在 `NewAccount::qualify` 里，进不来的会说明原因。
#[allow(clippy::too_many_arguments)]
#[tauri::command(async)]
pub fn accounts_add(
    state: State<'_, AppState>,
    email: String,
    refresh_token: Option<String>,
    // access_token：session / access token（`user_xxx::<jwt>` 或裸 JWT），没 refresh 时靠它撑到过期。
    access_token: Option<String>,
    cursor_password: Option<String>,
    email_password: Option<String>,
    recovery_email: Option<String>,
    note: Option<String>,
) -> Result<Account> {
    let account = state.accounts.repo.upsert(NewAccount {
        email,
        refresh_token,
        access_token,
        cursor_password,
        email_password,
        recovery_email,
        note,
        source: None,
    })?;
    activity::info(&state.db, "accounts", Some(&account.email), "已添加账号");
    Ok(account)
}

/// 解析一份粘进来的清单，**只预览不写库**。
///
/// 分两步是有意的：清单来自各种地方、格式极乱，用户得先看清「会收哪 12 个、
/// 剩下 3 个为什么不收」再按下确认。一步到位的导入出了偏差只能事后收拾。
#[tauri::command(async)]
pub fn accounts_parse_dump(text: String) -> Result<nexus_accounts::ImportPreview> {
    Ok(nexus_accounts::preview_import(&text).1)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub imported: u32,
    pub skipped: u32,
    /// 逐条失败原因。整批里坏一条不该让其余的都不进来。
    pub failures: Vec<String>,
}

/// 执行导入。同一份文本再解析一次 —— 前端只拿到预览，明文凭证从没出过 Rust。
#[tauri::command(async)]
pub fn accounts_import_dump(state: State<'_, AppState>, text: String) -> Result<ImportOutcome> {
    let (accepted, report) = nexus_accounts::preview_import(&text);
    let mut outcome = ImportOutcome {
        imported: 0,
        skipped: report.rejected_count,
        failures: Vec::new(),
    };

    for parsed in accepted {
        let email = parsed.email.clone();
        match state.accounts.repo.upsert(parsed.into()) {
            Ok(_) => outcome.imported += 1,
            Err(err) => outcome.failures.push(format!("{email}：{}", err.message)),
        }
    }

    activity::info(
        &state.db,
        "accounts",
        None,
        format!(
            "批量导入：收下 {}，跳过 {}",
            outcome.imported,
            outcome.skipped + outcome.failures.len() as u32
        ),
    );
    Ok(outcome)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOutcome {
    /// 写到了哪。前端只拿到路径，文件内容从没经过 IPC。
    pub path: String,
    pub count: u32,
}

/// 把全部账号连凭证导出成一份清单文件，落在 `~/.roviix/exports`。
///
/// 与 `accounts_import_dump` 是同一种文件的两个方向：导出去的文件粘回「批量添加」就能
/// 原样收回。**明文凭证**：文件权限收到 0600，并记一条 warn 级活动日志 —— 和「显示明文」
/// 一样，这是秘密离开 Rust 的显式动作，得留痕。
#[tauri::command(async)]
pub fn accounts_export_dump(state: State<'_, AppState>) -> Result<ExportOutcome> {
    let export = state.accounts.repo.export_dump()?;
    let path = state.exports_dir.join(format!(
        "nexus-accounts-{}.json",
        nexus_core::file_stamp(nexus_core::SystemClock.now())
    ));
    super::backup::write_private(&path, export.json.as_bytes())?;
    activity::warn(
        &state.db,
        "accounts",
        None,
        format!(
            "已导出 {} 个账号的明文凭证：{}",
            export.count,
            path.display()
        ),
    );
    Ok(ExportOutcome {
        path: path.display().to_string(),
        count: export.count as u32,
    })
}

#[tauri::command(async)]
pub fn accounts_patch(
    state: State<'_, AppState>,
    id: String,
    patch: AccountPatch,
) -> Result<Account> {
    state.accounts.repo.patch(&AccountId::from_raw(id), patch)
}

#[tauri::command(async)]
pub fn accounts_remove(state: State<'_, AppState>, id: String) -> Result<()> {
    let id = AccountId::from_raw(id);
    let email = state.accounts.repo.get(&id)?.email;
    state.accounts.repo.remove(&id)?;
    activity::info(&state.db, "accounts", Some(&email), "已删除账号");
    Ok(())
}

/// `day_start_ms` 是前端的本地零点（epoch ms）：有它才有「今天 / 近 7 天」两个时间窗。
/// 本地时区只有 WebView 那边知道得可靠，所以由前端递进来。
#[tauri::command]
pub async fn accounts_refresh_usage(
    state: State<'_, AppState>,
    id: String,
    day_start_ms: Option<i64>,
) -> Result<AccountUsage> {
    state
        .accounts
        .refresh_usage(&AccountId::from_raw(id), day_start_ms)
        .await
}

/// 批量刷。逐个推事件，界面能一行一行亮起来而不是整片转圈。
#[tauri::command]
pub async fn accounts_refresh_all(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Option<Vec<String>>,
    day_start_ms: Option<i64>,
) -> Result<u32> {
    let ids: Vec<AccountId> = match ids {
        Some(list) => list.into_iter().map(AccountId::from_raw).collect(),
        // 没指定就刷所有查得动的。
        None => state
            .accounts
            .repo
            .list()?
            .into_iter()
            .filter(Account::can_query_usage)
            .map(|a| a.id)
            .collect(),
    };

    #[derive(Serialize, Clone)]
    #[serde(rename_all = "camelCase")]
    struct Refreshed {
        id: String,
        ok: bool,
        message: Option<String>,
    }

    let results = state
        .accounts
        .refresh_all(&ids, day_start_ms, &|id, outcome| {
            let _ = app.emit(
                events::ACCOUNT_REFRESHED,
                Refreshed {
                    id: id.to_string(),
                    ok: outcome.is_ok(),
                    message: outcome.err().map(|e| e.message.clone()),
                },
            );
        })
        .await?;
    Ok(results.iter().filter(|(_, r)| r.is_ok()).count() as u32)
}

// ── OAuth ──────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OauthStarted {
    pub uuid: String,
    /// 已经在隐私窗口打开了；这里回一份，方便用户手动复制。
    pub login_url: String,
}

/// 开一次 OAuth：生成 PKCE、用隐私窗口打开登录页、后台开始轮询。
///
/// **弹窗关掉不影响收 token** —— 轮询是后台任务，跟窗口没关系（§2.3）。
#[tauri::command]
pub async fn accounts_start_oauth(
    app: AppHandle,
    state: State<'_, AppState>,
    email: String,
) -> Result<OauthStarted> {
    let session = Arc::new(OauthSession::start());
    let handle = session.handle();

    open_in_browser(&app, &handle.login_url)?;
    state
        .oauth
        .lock()
        .expect("oauth lock")
        .insert(handle.uuid.clone(), session.clone());

    let accounts = state.accounts.clone();
    let db = state.db.clone();
    let emitter = app.clone();
    let uuid = handle.uuid.clone();

    tauri::async_runtime::spawn(async move {
        let http = accounts.http().clone();
        let result = session
            .poll(&http, nexus_accounts::DEFAULT_TIMEOUT, &|st: OauthState| {
                let _ = emitter.emit(events::OAUTH_STATE, st);
            })
            .await;

        // 无论成败都从进行中列表里摘掉，否则这张表只增不减。
        emitter
            .state::<crate::state::AppState>()
            .oauth
            .lock()
            .expect("oauth lock")
            .remove(&uuid);

        match result {
            Ok(tokens) => match accounts.complete_oauth(&email, &tokens).await {
                Ok(account) => {
                    activity::info(
                        &db,
                        "accounts",
                        Some(&account.email),
                        "已通过授权获取 token",
                    );
                    let _ = emitter.emit(
                        events::OAUTH_STATE,
                        OauthState::Succeeded {
                            uuid,
                            email: Some(account.email),
                        },
                    );
                }
                Err(err) => {
                    activity::error(
                        &db,
                        "accounts",
                        Some(&email),
                        format!("授权后落库失败：{}", err.message),
                    );
                    let _ = emitter.emit(
                        events::OAUTH_STATE,
                        OauthState::Failed {
                            uuid,
                            message: err.message,
                        },
                    );
                }
            },
            // 超时 / 取消的事件已经在 poll 里发过了，这里只补一条日志。
            Err(err) if err.code != ErrorCode::Cancelled => {
                activity::warn(
                    &db,
                    "accounts",
                    Some(&email),
                    format!("授权未完成：{}", err.message),
                );
            }
            Err(_) => {}
        }
    });

    Ok(OauthStarted {
        uuid: handle.uuid,
        login_url: handle.login_url,
    })
}

#[tauri::command(async)]
pub fn accounts_cancel_oauth(state: State<'_, AppState>, uuid: String) -> Result<()> {
    if let Some(session) = state.oauth.lock().expect("oauth lock").remove(&uuid) {
        session.cancel();
    }
    Ok(())
}

/// 这个号在 Cursor 侧还有哪些活跃会话（IDE / 网页 / 移动端…）。
#[tauri::command]
pub async fn accounts_list_sessions(
    state: State<'_, AppState>,
    id: String,
) -> Result<Vec<ActiveSession>> {
    state.accounts.list_sessions(&AccountId::from_raw(id)).await
}

/// 踢掉其它活跃会话，尽量只留 Nexus 当前这把；保不住就全踢，可能要重新授权。
#[tauri::command]
pub async fn accounts_kick_sessions(state: State<'_, AppState>, id: String) -> Result<KickOutcome> {
    let id = AccountId::from_raw(id);
    let email = state.accounts.repo.get(&id)?.email;
    let outcome = state.accounts.kick_other_sessions(&id).await?;
    let note = if outcome.kept_current {
        format!("已踢掉 {} 个会话，保留当前", outcome.revoked)
    } else if outcome.refresh_alive {
        format!(
            "已踢掉 {} 个会话（未能识别当前会话，已全踢；refresh 仍可用）",
            outcome.revoked
        )
    } else {
        format!(
            "已踢掉 {} 个会话；refresh 失效，需要重新授权",
            outcome.revoked
        )
    };
    activity::info(&state.db, "accounts", Some(&email), note);
    Ok(outcome)
}

// ── 凭证 ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SecretKind {
    Refresh,
    /// 裸 access JWT。粘进来时接受 `user_xxx::<jwt>` 形态，落库前剥前缀（见 `put_access`）。
    Access,
    CursorPassword,
    EmailPassword,
    RecoveryEmail,
}

impl From<SecretKind> for AccountSecret {
    fn from(k: SecretKind) -> Self {
        match k {
            SecretKind::Refresh => AccountSecret::Refresh,
            SecretKind::Access => AccountSecret::Access,
            SecretKind::CursorPassword => AccountSecret::CursorPassword,
            SecretKind::EmailPassword => AccountSecret::EmailPassword,
            SecretKind::RecoveryEmail => AccountSecret::RecoveryEmail,
        }
    }
}

/// 显示一条凭证的明文。
///
/// **这是秘密唯一的 IPC 出口**（§4.3），所以它是显式动作、一次一条、并且记活动日志。
#[tauri::command(async)]
pub fn accounts_reveal_secret(
    state: State<'_, AppState>,
    id: String,
    kind: SecretKind,
) -> Result<String> {
    let id = AccountId::from_raw(id);
    let account = state.accounts.repo.get(&id)?;
    let secret = state.accounts.repo.require_secret(&id, kind.into())?;
    activity::warn(
        &state.db,
        "accounts",
        Some(&account.email),
        format!("已查看明文凭证（{:?}）", kind),
    );
    Ok(secret.expose().to_string())
}

/// 显示这个号的**会话 token**（`user_xxx::<access jwt>`，即 `WorkosCursorSessionToken` cookie 的值）。
///
/// 它是 access 的派生物：手上那把还有效就直接给，过期了拿 refresh 换一把新的（顺带把轮换后的
/// refresh 落库）；只有 session token 的号过期了就报错让人粘新的。和 `accounts_reveal_secret` 一样是显式动作、
/// 记活动日志——它能直接登进 cursor.com，泄露的后果和 refresh_token 一样重。
#[tauri::command(async)]
pub async fn accounts_reveal_session(state: State<'_, AppState>, id: String) -> Result<String> {
    let id = AccountId::from_raw(id);
    let account = state.accounts.repo.get(&id)?;
    let session = state.accounts.session(&id).await?;
    activity::warn(
        &state.db,
        "accounts",
        Some(&account.email),
        "已查看会话 token（user_id::access）",
    );
    Ok(session.session_token.expose().to_string())
}

/// 改一条凭证：线下改过密码、手上换了一份 refresh_token，都从这里落库。
///
/// `value` 为空即**清除**这一条。这里之所以敢把「空」当清除，是因为它来自用户在界面上
/// 的一次明确编辑；仓库层的 `put_secret` 反过来把空当「没提供」——那是给批量导入用的，
/// 一份清单少给一个字段不该把本地的抹了（见 `put_secret` 的注释）。两种语义都需要，
/// 区别在于调用方知不知道自己在说「清掉它」。
///
/// 改完不自动去验证：验密码要走一次真登录，那是「授权」按钮的事。
#[tauri::command(async)]
pub fn accounts_set_secret(
    state: State<'_, AppState>,
    id: String,
    kind: SecretKind,
    value: Option<String>,
) -> Result<Account> {
    let id = AccountId::from_raw(id);
    let account = state.accounts.repo.get(&id)?;
    let trimmed = value.as_deref().map(str::trim).unwrap_or("");

    if trimmed.is_empty() {
        state.accounts.repo.clear_secret(&id, kind.into())?;
        activity::warn(
            &state.db,
            "accounts",
            Some(&account.email),
            format!("已清除凭证（{:?}）", kind),
        );
    } else {
        match kind {
            // access 要过一遍形状检查并剥掉 `user_xxx::` 前缀，不能原样塞进去。
            SecretKind::Access => state.accounts.repo.put_access(&id, trimmed)?,
            _ => state
                .accounts
                .repo
                .put_secret(&id, kind.into(), Some(trimmed))?,
        }
        activity::info(
            &state.db,
            "accounts",
            Some(&account.email),
            format!("已更新凭证（{:?}）", kind),
        );
    }
    state.accounts.repo.get(&id)
}

// ── 跨模块的那一次显式拷贝 ───────────────────────────────────────────────────

/// 「加入切号本」。
///
/// **这是 `nexus-accounts` 与 `nexus-switcher` 之间唯一的数据通路**（ARCHITECTURE R1）。
/// 两个 crate 互不依赖，拷贝发生在这里、由用户点击触发、一次一个号。
///
/// 切号需要 `accessToken`，而我们长期存的是 `refreshToken`，所以要先换一次 session。
#[tauri::command]
pub async fn accounts_add_to_switch_book(
    state: State<'_, AppState>,
    id: String,
) -> Result<SwitchProfile> {
    let id = AccountId::from_raw(id);
    let account = state.accounts.repo.get(&id)?;
    if !account.can_switch() {
        return Err(AppError::new(
            ErrorCode::ProfileIncomplete,
            format!("{} 还不能切入 Cursor。", account.email),
        )
        .with_hint(if account.session_only() {
            "只有 session token 的号切不进 Cursor：写进去的登录态到期没法自己续。用密码授权一次拿到 refresh_token 就行。"
        } else {
            "这个号需要先完成一次授权拿到 refresh_token。"
        }));
    }

    // 这一把是要写进 Cursor 登录态的，所以强制换新，不用复用的那把 ——
    // 复用的可能只剩一分钟寿命，Cursor 一启动就得先去续期。
    let session = state.accounts.fresh_session(&id).await?;
    let refresh = state
        .accounts
        .repo
        .require_secret(&id, AccountSecret::Refresh)?;

    let mut bundle = AuthBundle::new();
    bundle.insert("cursorAuth/cachedEmail", &account.email);
    bundle.insert("cursorAuth/accessToken", session.access_token.expose());
    bundle.insert("cursorAuth/refreshToken", refresh.expose());
    bundle.insert("cursorAuth/cachedUserId", &session.user_id);
    // 订阅档只影响 Cursor 界面上的显示，有就带上。
    if let Some(plan) = account.membership.as_deref() {
        bundle.insert("cursorAuth/stripeMembershipType", plan);
    }
    if let Some(t) = account.signup_type.as_deref() {
        bundle.insert("cursorAuth/cachedSignUpType", t);
    }

    state.switcher.adopt(&bundle, account.note.as_deref())
}

/// 用系统浏览器打开。优先隐私 / 无痕窗口，避免默认配置文件里已登录的 Cursor 账号串号。
///
/// **不在应用内 WebView 打开任何远程页面**（§4.4）。找不到 Chrome/Edge 时回落到默认浏览器。
pub(crate) fn open_in_browser(app: &AppHandle, url: &str) -> Result<()> {
    if open_in_private_browser(url).is_ok() {
        return Ok(());
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(url, None::<&str>).map_err(|err| {
        AppError::internal(format!("打不开系统浏览器：{err}"))
            .with_hint("手动复制链接到浏览器里打开也可以。")
    })
}

/// 尽量开一个干净的隐私窗口。Chrome / Edge / Chromium；Safari 的私人浏览没法可靠地从外部拉起。
fn open_in_private_browser(url: &str) -> std::io::Result<()> {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    // Linux 那一支靠 `which_bin` 找二进制，用不到它。
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn exists(p: &Path) -> bool {
        p.is_file() || p.is_dir()
    }

    fn spawn(bin: &Path, args: &[&str]) -> std::io::Result<()> {
        Command::new(bin).args(args).spawn().map(|_| ())
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let chrome_apps = [
            PathBuf::from("/Applications/Google Chrome.app"),
            home.join("Applications/Google Chrome.app"),
        ];
        for app in &chrome_apps {
            let bin = app.join("Contents/MacOS/Google Chrome");
            if exists(&bin) {
                // 直接起二进制，Chrome 已在跑时 `--incognito` 才会生效。
                return spawn(&bin, &["--incognito", "--new-window", url]);
            }
            if exists(app) {
                return Command::new("open")
                    .args([
                        "-na",
                        app.to_str().unwrap_or(""),
                        "--args",
                        "--incognito",
                        "--new-window",
                        url,
                    ])
                    .spawn()
                    .map(|_| ());
            }
        }
        let edge_apps = [
            PathBuf::from("/Applications/Microsoft Edge.app"),
            home.join("Applications/Microsoft Edge.app"),
        ];
        for app in &edge_apps {
            let bin = app.join("Contents/MacOS/Microsoft Edge");
            if exists(&bin) {
                return spawn(&bin, &["--inprivate", "--new-window", url]);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no private-capable browser",
        ))
    }

    #[cfg(target_os = "windows")]
    {
        let local = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_default();
        let program = std::env::var_os("PROGRAMFILES")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
        let program_x86 = std::env::var_os("PROGRAMFILES(X86)")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Program Files (x86)"));

        let chrome = [
            local.join(r"Google\Chrome\Application\chrome.exe"),
            program.join(r"Google\Chrome\Application\chrome.exe"),
            program_x86.join(r"Google\Chrome\Application\chrome.exe"),
        ];
        for bin in &chrome {
            if exists(bin) {
                return spawn(bin, &["--incognito", "--new-window", url]);
            }
        }
        let edge = [
            program_x86.join(r"Microsoft\Edge\Application\msedge.exe"),
            program.join(r"Microsoft\Edge\Application\msedge.exe"),
        ];
        for bin in &edge {
            if exists(bin) {
                return spawn(bin, &["--inprivate", "--new-window", url]);
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no private-capable browser",
        ))
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let bins = [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
            "microsoft-edge-stable",
        ];
        for name in bins {
            if let Ok(path) = which_bin(name) {
                let private = if name.contains("edge") {
                    "--inprivate"
                } else {
                    "--incognito"
                };
                return spawn(&path, &[private, "--new-window", url]);
            }
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no private-capable browser",
        ));

        fn which_bin(name: &str) -> std::io::Result<PathBuf> {
            let out = Command::new("which").arg(name).output()?;
            if !out.status.success() {
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, name));
            }
            let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if p.is_empty() {
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, name));
            }
            Ok(PathBuf::from(p))
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows", unix)))]
    {
        let _ = url;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "private browser not supported",
        ))
    }
}
