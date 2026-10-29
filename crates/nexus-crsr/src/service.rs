//! 编排：status / install / uninstall / restore / mint。
//!
//! 顺序是硬约束（和 Sand 一样）：**预检 → 备份 → 退出 Cursor → 写入 → 校验 → 启动**。
//! 预检不过就什么都不碰；写入失败 `commit_plan` 会回滚。任何时刻只允许一个操作在跑
//! （`ErrorCode::Busy`）。备份落在 `crsr/backups`，不和 Sand 混。

use nexus_accounts::AccountsService;
use nexus_core::{AccountId, AppError, ErrorCode, Result};
use nexus_cursor::{CursorControl, CursorPaths};
use nexus_sand::backup::{Backups, PlannedFile};
use nexus_sand::commit::{commit_plan, ensure_writable};
use nexus_sand::integrity;
use nexus_sand::layout::SandLayout;
use nexus_sand::{Operation, SandBackup, SandProgress, SandStep, SUPPORTED_CURSOR_VERSION};
use nexus_store::keys::AccountSecret;
use nexus_store::{activity, Db};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::credential::{self, CrsrCredential, CrsrCredentialInfo};
use crate::inject::{self, AUTH_MARKER, EXPECTED_HITS};

const BACKUP_KEEP: usize = 10;
const QUIT_TIMEOUT: Duration = Duration::from_secs(20);
const SCOPE: &str = "crsr";

pub struct CrsrService {
    db: Arc<Db>,
    data_dir: PathBuf,
    paths: Mutex<CursorPaths>,
    control: Mutex<Arc<dyn CursorControl>>,
    accounts: Arc<AccountsService>,
    http: reqwest::Client,
    busy: Mutex<()>,
}

/// 只读检查结果。前端「CRSR」页顶部那张卡全靠它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrsrStatus {
    pub cursor_version: Option<String>,
    pub supported_version: String,
    pub version_supported: bool,
    /// 盘上有 `/*CRSR_AUTH_V1*/`。
    pub installed: bool,
    /// 两处 `applyAuthorization` 都打上了。
    pub complete: bool,
    pub hits: u32,
    pub expected_hits: u32,
    /// 原版或已打补丁的锚点数。装之前应为 2。
    pub anchors: u32,
    pub patched_files: Vec<String>,
    /// 盘上已经装着 Sand 时给出原因；没有冲突为 `None`。
    pub sand_conflict: Option<String>,
    pub backups: u32,
    /// 本地凭证摘要（不含秘密）。没写过文件为 `None`。
    pub credential: Option<CrsrCredentialInfo>,
}

/// 安装 / 卸载 / 还原的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CrsrOutcome {
    pub operation: Operation,
    pub wrote: bool,
    pub files_written: u32,
    pub backup_id: Option<String>,
    pub cursor_relaunched: bool,
    pub status: CrsrStatus,
}

impl CrsrService {
    pub fn new(
        db: Arc<Db>,
        data_dir: impl Into<PathBuf>,
        paths: CursorPaths,
        control: Arc<dyn CursorControl>,
        accounts: Arc<AccountsService>,
    ) -> Self {
        Self {
            db,
            data_dir: data_dir.into(),
            paths: Mutex::new(paths),
            control: Mutex::new(control),
            accounts,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            busy: Mutex::new(()),
        }
    }

    fn paths(&self) -> CursorPaths {
        self.paths.lock().expect("paths").clone()
    }

    fn control(&self) -> Arc<dyn CursorControl> {
        self.control.lock().expect("control").clone()
    }

    /// 设置页改了 Cursor 目录之后立刻换上。CRSR 操作正在跑时拒绝。
    pub fn retarget(&self, paths: CursorPaths, control: Arc<dyn CursorControl>) -> Result<()> {
        let _guard = self.acquire()?;
        *self.paths.lock().expect("paths") = paths;
        *self.control.lock().expect("control") = control;
        Ok(())
    }

    // ------------------------------------------------------------------ 只读

    pub fn status(&self) -> Result<CrsrStatus> {
        let layout = SandLayout::resolve(&self.paths())?;
        let contents = read_targets(&layout)?;
        let agg = inspect(&layout, &contents);
        let backups = Backups::for_app(&self.data_dir, SCOPE, &layout.app_root)
            .list()?
            .len() as u32;
        let credential = CrsrCredential::load(&self.data_dir)?.map(|c| c.info());
        Ok(CrsrStatus {
            cursor_version: Some(layout.version.clone()),
            supported_version: SUPPORTED_CURSOR_VERSION.into(),
            version_supported: layout.version_supported(),
            installed: agg.installed,
            complete: agg.complete,
            hits: agg.hits,
            expected_hits: EXPECTED_HITS,
            anchors: agg.anchors,
            patched_files: agg.patched_files,
            sand_conflict: agg.sand_conflict.map(str::to_string),
            backups,
            credential,
        })
    }

    pub fn backups(&self) -> Result<Vec<SandBackup>> {
        let layout = SandLayout::resolve(&self.paths())?;
        Backups::for_app(&self.data_dir, SCOPE, &layout.app_root).list()
    }

    pub fn remove_backup(&self, id: &str) -> Result<()> {
        let layout = SandLayout::resolve(&self.paths())?;
        Backups::for_app(&self.data_dir, SCOPE, &layout.app_root).remove(id)
    }

    /// 用账号库里这个号生成凭证文件。不必重装补丁。
    /// 优先使用该号已有的 `crsr_` API Key；若无则自动在线铸造；
    /// 若铸造不可用，直接复用有效的 session 凭证；自动保留已有 gateway_url。
    pub async fn mint_for_account(&self, id: &AccountId) -> Result<CrsrCredentialInfo> {
        let account = self.accounts.repo.get(id)?;
        let maybe_key = self.accounts.repo.secret(id, AccountSecret::ApiKey)?;
        let cred = if let Some(key) = maybe_key {
            credential::mint(&self.http, key.expose(), &account.email, &account.id).await?
        } else {
            // 尝试自动为该账号在官方铸一把长期 crsr_ API Key
            match self.accounts.mint_api_key(id).await {
                Ok(_minted) => {
                    let key = self
                        .accounts
                        .repo
                        .require_secret(id, AccountSecret::ApiKey)?;
                    credential::mint(&self.http, key.expose(), &account.email, &account.id).await?
                }
                Err(_) => {
                    // 若官方铸 Key 接口不可用，则直接复用账号的有效会话 JWT
                    if let Ok(session) = self.accounts.session(id).await {
                        let now = credential::now_ms();
                        let expires_at_ms =
                            credential::expires_at_ms_of(session.access_token.expose());
                        credential::CrsrCredential {
                            api_key: String::new(),
                            access_token: session.access_token.expose().to_string(),
                            expires_at_ms: Some(expires_at_ms),
                            account_email: Some(account.email.clone()),
                            account_id: Some(account.id.as_str().to_string()),
                            minted_at_ms: Some(now),
                            renewed_at_ms: None,
                            gateway_url: None,
                        }
                    } else {
                        return Err(AppError::invalid(
                            "该账号无可用 crsr_ API Key 或已过期会话，请先登录该账号。",
                        ));
                    }
                }
            }
        };

        // 原地保留已配置的 gateway_url（避免切换账号时清掉本地网关代理）
        let existing = CrsrCredential::load(&self.data_dir).ok().flatten();
        let mut final_cred = cred;
        if let Some(old) = existing {
            if old.gateway_url.is_some() {
                final_cred.gateway_url = old.gateway_url;
            }
        }

        final_cred.save(&self.data_dir)?;
        activity::info(
            &self.db,
            SCOPE,
            Some(&account.email),
            "Cursor 底座账号已切换（凭证已即时同步）",
        );
        Ok(final_cred.info())
    }

    pub fn clear_credential(&self) -> Result<()> {
        CrsrCredential::remove(&self.data_dir)
    }

    /// 更新网关重写地址（如 `http://127.0.0.1:15721/cursor/mock` 或 `/cursor/native`）。
    /// 如果之前已有凭证，原地保留 `api_key` 与 `access_token` 并更新 `gateway_url`；
    /// 如果尚未配置账号，则写入仅含 `gateway_url` 的接入凭证。
    pub fn set_gateway_url(
        &self,
        gateway_url: Option<String>,
    ) -> Result<Option<CrsrCredentialInfo>> {
        let existing = CrsrCredential::load(&self.data_dir)?;
        match (existing, gateway_url) {
            (Some(mut cred), Some(url)) => {
                cred.gateway_url = Some(url);
                cred.save(&self.data_dir)?;
                Ok(Some(cred.info()))
            }
            (Some(mut cred), None) => {
                if cred.api_key.is_empty() {
                    CrsrCredential::remove(&self.data_dir)?;
                    Ok(None)
                } else {
                    cred.gateway_url = None;
                    cred.save(&self.data_dir)?;
                    Ok(Some(cred.info()))
                }
            }
            (None, Some(url)) => {
                let cred = CrsrCredential {
                    api_key: String::new(),
                    access_token: String::new(),
                    expires_at_ms: None,
                    account_email: None,
                    account_id: None,
                    minted_at_ms: Some(crate::credential::now_ms()),
                    renewed_at_ms: None,
                    gateway_url: Some(url),
                };
                cred.save(&self.data_dir)?;
                Ok(Some(cred.info()))
            }
            (None, None) => Ok(None),
        }
    }

    // ------------------------------------------------------------------ 写

    pub fn install(&self, relaunch: bool, progress: &dyn Fn(SandProgress)) -> Result<CrsrOutcome> {
        let _guard = self.acquire()?;
        progress(SandProgress::new(
            SandStep::Preflight,
            "检查 Cursor 版本与锚点",
        ));
        let layout = SandLayout::resolve(&self.paths())?;
        if !layout.version_supported() {
            return Err(AppError::new(
                ErrorCode::SandUnsupportedVersion,
                format!(
                    "当前 Cursor 是 {}，挂点只适配 {SUPPORTED_CURSOR_VERSION}。",
                    layout.version
                ),
            )
            .with_hint("等待 Nexus 更新适配这个版本；Cursor 没有被改动。"));
        }
        let contents = read_targets(&layout)?;
        let agg = inspect(&layout, &contents);
        if let Some(reason) = agg.sand_conflict {
            return Err(
                AppError::invalid(format!("{reason} 不能与当前统一接入同时使用。")).with_hint(
                    "检测到旧版 Sand 补丁占用同一挂点，请先在接入卡片中点击一键升级清理旧补丁。",
                ),
            );
        }
        if agg.anchors < EXPECTED_HITS && !agg.complete {
            return Err(anchor_mismatch(agg.anchors));
        }

        let plan = build_plan(&layout, &contents, Mode::Apply)?;
        if plan.is_empty() {
            if agg.complete {
                let relaunched = if relaunch {
                    progress(SandProgress::new(SandStep::QuitCursor, "正在退出 Cursor"));
                    let _ = self.control().quit(QUIT_TIMEOUT);
                    self.maybe_launch(true, progress)
                } else {
                    false
                };
                progress(SandProgress::new(SandStep::Done, "完成"));
                return Ok(CrsrOutcome {
                    operation: Operation::Install,
                    wrote: false,
                    files_written: 0,
                    backup_id: None,
                    cursor_relaunched: relaunched,
                    status: self.status()?,
                });
            }
            return Err(anchor_mismatch(agg.anchors));
        }

        let projected = project(&contents, &plan);
        let after = inspect(&layout, &projected);
        if !after.complete {
            return Err(anchor_mismatch(after.hits));
        }
        if after.sand_conflict.is_some() {
            return Err(
                AppError::new(ErrorCode::SandAnchorMismatch, "打完补丁仍会和 Sand 冲突。")
                    .with_hint("Cursor 没有被改动。"),
            );
        }
        ensure_writable(&plan.iter().map(|f| f.path.clone()).collect::<Vec<_>>())?;

        let backups = Backups::for_app(&self.data_dir, SCOPE, &layout.app_root);
        progress(SandProgress::new(SandStep::QuitCursor, "正在退出 Cursor"));
        self.control().quit(QUIT_TIMEOUT)?;

        progress(SandProgress::new(
            SandStep::Write,
            format!("备份并写入 {} 个文件", plan.len()),
        ));
        let committed = commit_plan(
            &backups,
            &layout.app_root,
            &layout.version,
            Operation::Install,
            &plan,
            &|| {
                let contents = read_targets(&layout)?;
                let after = inspect(&layout, &contents);
                if !after.complete {
                    return Err(AppError::new(
                        ErrorCode::SandIntegrity,
                        format!(
                            "安装后状态校验失败：hits={} / 需 {EXPECTED_HITS}",
                            after.hits
                        ),
                    ));
                }
                verify_integrity(&layout)
            },
        )?;
        let _ = backups.prune(BACKUP_KEEP);
        activity::info(
            &self.db,
            SCOPE,
            None,
            format!(
                "已完成 Cursor 路由挂点接管（{} 个文件）",
                committed.files_written
            ),
        );

        let relaunched = self.maybe_launch(relaunch, progress);
        progress(SandProgress::new(SandStep::Done, "完成"));
        Ok(CrsrOutcome {
            operation: Operation::Install,
            wrote: true,
            files_written: committed.files_written,
            backup_id: Some(committed.backup_id),
            cursor_relaunched: relaunched,
            status: self.status()?,
        })
    }

    pub fn uninstall(
        &self,
        relaunch: bool,
        progress: &dyn Fn(SandProgress),
    ) -> Result<CrsrOutcome> {
        let _guard = self.acquire()?;
        progress(SandProgress::new(SandStep::Preflight, "检查已安装的标记"));
        let layout = SandLayout::resolve(&self.paths())?;
        let contents = read_targets(&layout)?;
        let agg = inspect(&layout, &contents);
        if !agg.installed {
            return Ok(CrsrOutcome {
                operation: Operation::Uninstall,
                wrote: false,
                files_written: 0,
                backup_id: None,
                cursor_relaunched: false,
                status: self.status()?,
            });
        }
        let plan = build_plan(&layout, &contents, Mode::Remove)?;
        if plan.is_empty() {
            return Ok(CrsrOutcome {
                operation: Operation::Uninstall,
                wrote: false,
                files_written: 0,
                backup_id: None,
                cursor_relaunched: false,
                status: self.status()?,
            });
        }
        ensure_writable(&plan.iter().map(|f| f.path.clone()).collect::<Vec<_>>())?;

        let backups = Backups::for_app(&self.data_dir, SCOPE, &layout.app_root);
        progress(SandProgress::new(SandStep::QuitCursor, "正在退出 Cursor"));
        self.control().quit(QUIT_TIMEOUT)?;
        progress(SandProgress::new(
            SandStep::Write,
            format!("备份并还原 {} 个文件", plan.len()),
        ));
        let committed = commit_plan(
            &backups,
            &layout.app_root,
            &layout.version,
            Operation::Uninstall,
            &plan,
            &|| {
                let contents = read_targets(&layout)?;
                let after = inspect(&layout, &contents);
                if after.installed {
                    return Err(AppError::new(
                        ErrorCode::SandIntegrity,
                        format!("还原后仍有 {} 处挂点标记。", after.hits),
                    ));
                }
                verify_integrity(&layout)
            },
        )?;
        let _ = backups.prune(BACKUP_KEEP);
        activity::info(&self.db, SCOPE, None, "已恢复 Cursor 官方路由设置");

        let relaunched = self.maybe_launch(relaunch, progress);
        progress(SandProgress::new(SandStep::Done, "完成"));
        Ok(CrsrOutcome {
            operation: Operation::Uninstall,
            wrote: true,
            files_written: committed.files_written,
            backup_id: Some(committed.backup_id),
            cursor_relaunched: relaunched,
            status: self.status()?,
        })
    }

    /// 把某份备份里的原始字节写回去。紧急刹车：不认锚点、不认 marker，只按字节还原。
    pub fn restore_backup(
        &self,
        id: &str,
        relaunch: bool,
        progress: &dyn Fn(SandProgress),
    ) -> Result<CrsrOutcome> {
        let _guard = self.acquire()?;
        progress(SandProgress::new(SandStep::Preflight, "读取备份"));
        let layout = SandLayout::resolve(&self.paths())?;
        let backups = Backups::for_app(&self.data_dir, SCOPE, &layout.app_root);
        let manifest = backups.manifest(id)?;
        let mut plan = Vec::with_capacity(manifest.files.len());
        for f in &manifest.files {
            let path = layout.app_root.join(&f.path);
            let current = std::fs::read(&path)?;
            let original = backups.original_bytes(id, &f.path)?;
            if current != original {
                plan.push(PlannedFile {
                    path,
                    original: current,
                    next: original,
                });
            }
        }
        if plan.is_empty() {
            return Ok(CrsrOutcome {
                operation: Operation::Restore,
                wrote: false,
                files_written: 0,
                backup_id: None,
                cursor_relaunched: false,
                status: self.status()?,
            });
        }
        ensure_writable(&plan.iter().map(|f| f.path.clone()).collect::<Vec<_>>())?;
        progress(SandProgress::new(SandStep::QuitCursor, "正在退出 Cursor"));
        self.control().quit(QUIT_TIMEOUT)?;
        progress(SandProgress::new(
            SandStep::Write,
            format!("还原 {} 个文件", plan.len()),
        ));
        let committed = commit_plan(
            &backups,
            &layout.app_root,
            &layout.version,
            Operation::Restore,
            &plan,
            &|| verify_integrity(&layout),
        )?;
        activity::info(
            &self.db,
            SCOPE,
            None,
            format!("已从备份 {id} 还原 {} 个文件", committed.files_written),
        );
        let relaunched = self.maybe_launch(relaunch, progress);
        progress(SandProgress::new(SandStep::Done, "完成"));
        Ok(CrsrOutcome {
            operation: Operation::Restore,
            wrote: true,
            files_written: committed.files_written,
            backup_id: Some(committed.backup_id),
            cursor_relaunched: relaunched,
            status: self.status()?,
        })
    }

    // ------------------------------------------------------------------ 内部

    fn acquire(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.busy.try_lock().map_err(|_| {
            AppError::new(ErrorCode::Busy, "已经有一个 Cursor 路由操作在进行。")
                .with_hint("等它结束再试。")
        })
    }

    fn maybe_launch(&self, relaunch: bool, progress: &dyn Fn(SandProgress)) -> bool {
        if !relaunch {
            return false;
        }
        progress(SandProgress::new(SandStep::Launch, "正在启动 Cursor"));
        match self.control().launch() {
            Ok(()) => true,
            Err(err) => {
                activity::warn(&self.db, SCOPE, None, format!("启动 Cursor 失败：{err}"));
                false
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Apply,
    Remove,
}

struct Aggregate {
    hits: u32,
    anchors: u32,
    installed: bool,
    complete: bool,
    patched_files: Vec<String>,
    sand_conflict: Option<&'static str>,
}

fn inspect(layout: &SandLayout, contents: &HashMap<PathBuf, String>) -> Aggregate {
    let mut hits = 0;
    let mut anchors = 0;
    let mut patched_files = Vec::new();
    let mut sand_conflict = None;
    for t in &layout.targets {
        let Some(c) = contents.get(t) else { continue };
        if sand_conflict.is_none() {
            sand_conflict = inject::sand_conflict_reason(c);
        }
        hits += inject::count_hits(c);
        anchors += inject::count_anchors(c);
        if c.contains(AUTH_MARKER) {
            patched_files.push(layout.relative(t));
        }
    }
    Aggregate {
        installed: !patched_files.is_empty(),
        complete: hits >= EXPECTED_HITS,
        hits,
        anchors,
        patched_files,
        sand_conflict,
    }
}

fn read_targets(layout: &SandLayout) -> Result<HashMap<PathBuf, String>> {
    let mut out = HashMap::with_capacity(layout.targets.len());
    for t in &layout.targets {
        let bytes = std::fs::read(t)?;
        let text = String::from_utf8(bytes).map_err(|_| {
            AppError::new(
                ErrorCode::SandIntegrity,
                format!("目标文件不是 UTF-8，拒绝修改：{}", t.display()),
            )
        })?;
        out.insert(t.clone(), text);
    }
    Ok(out)
}

fn build_plan(
    layout: &SandLayout,
    contents: &HashMap<PathBuf, String>,
    mode: Mode,
) -> Result<Vec<PlannedFile>> {
    let mut plan = Vec::new();
    for t in &layout.targets {
        let Some(content) = contents.get(t) else {
            continue;
        };
        let next = match mode {
            Mode::Apply => inject::apply(content),
            Mode::Remove => inject::remove(content),
        };
        let Some(next) = next else { continue };
        if next != *content {
            plan.push(PlannedFile {
                path: t.clone(),
                original: content.as_bytes().to_vec(),
                next: next.into_bytes(),
            });
        }
    }
    if plan.is_empty() {
        return Ok(plan);
    }
    sync_integrity_into_plan(layout, &mut plan)?;
    Ok(plan)
}

fn sync_integrity_into_plan(layout: &SandLayout, plan: &mut Vec<PlannedFile>) -> Result<()> {
    if let Some(ext_host) = &layout.ext_host {
        let changed: Vec<(&str, &[u8])> = plan
            .iter()
            .filter_map(|f| {
                layout
                    .extension_name_of(&f.path)
                    .map(|name| (name, f.next.as_slice()))
            })
            .collect();
        if !changed.is_empty() {
            let (orig_bytes, cur_text) = match plan.iter().find(|f| &f.path == ext_host) {
                Some(f) => (
                    f.original.clone(),
                    String::from_utf8_lossy(&f.next).into_owned(),
                ),
                None => {
                    let b = std::fs::read(ext_host)?;
                    let s = String::from_utf8_lossy(&b).into_owned();
                    (b, s)
                }
            };
            if let Some(updated) = integrity::update_extension_hashes(&cur_text, &changed)? {
                plan.retain(|f| &f.path != ext_host);
                plan.push(PlannedFile {
                    path: ext_host.clone(),
                    original: orig_bytes,
                    next: updated.into_bytes(),
                });
            }
        }
    }

    let product = std::fs::read(&layout.product_json)?;
    let planned: HashMap<PathBuf, Vec<u8>> = plan
        .iter()
        .map(|f| (f.path.clone(), f.next.clone()))
        .collect();
    let out_root = layout.app_root.join("out");
    if let Some(next) = integrity::sync_product_checksums(&product, &out_root, &planned)? {
        plan.push(PlannedFile {
            path: layout.product_json.clone(),
            original: product,
            next,
        });
    }
    Ok(())
}

fn project(contents: &HashMap<PathBuf, String>, plan: &[PlannedFile]) -> HashMap<PathBuf, String> {
    let mut out = contents.clone();
    for f in plan {
        if let Ok(s) = String::from_utf8(f.next.clone()) {
            out.insert(f.path.clone(), s);
        }
    }
    out
}

fn verify_integrity(layout: &SandLayout) -> Result<()> {
    if let Some(ext_host) = &layout.ext_host {
        let host = std::fs::read_to_string(ext_host)?;
        let mut actual: Vec<(&str, Vec<u8>)> = Vec::new();
        for t in &layout.targets {
            if let Some(name) = layout.extension_name_of(t) {
                actual.push((name, std::fs::read(t)?));
            }
        }
        let borrowed: Vec<(&str, &[u8])> = actual.iter().map(|(n, b)| (*n, b.as_slice())).collect();
        integrity::verify_extension_hashes(&host, &borrowed)?;
    }
    let product = std::fs::read(&layout.product_json)?;
    integrity::verify_product_checksums(&product, &layout.app_root.join("out"))?;
    Ok(())
}

fn anchor_mismatch(hits: u32) -> AppError {
    AppError::new(
        ErrorCode::SandAnchorMismatch,
        format!("Cursor 路由挂点命中 {hits} 处（需 {EXPECTED_HITS} 处）。"),
    )
    .with_hint("Cursor 没有被改动。这个版本的 applyAuthorization 结构与适配版本不同。")
}

#[allow(dead_code)]
fn _assert_send_sync()
where
    CrsrService: Send + Sync,
{
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_cursor::app::testing::FakeCursor;
    use nexus_store::MemorySecrets;
    use std::path::Path;

    const OTHER_VERSION: &str = "0.0.1";

    fn jwt() -> String {
        use base64::Engine;
        let enc = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        format!(
            "{}.{}.sig",
            enc(b"{\"alg\":\"none\"}"),
            enc(br#"{"exp":4102444800}"#)
        )
    }

    fn service(dir: &Path, app: &Path) -> (CrsrService, Arc<FakeCursor>) {
        let db = Arc::new(Db::open(dir.join("nexus.db")).unwrap());
        let secrets: Arc<dyn nexus_store::SecretStore> = Arc::new(MemorySecrets::default());
        let accounts = Arc::new(AccountsService::new(db.clone(), secrets));
        let mut paths = CursorPaths::from_user_dir(dir.join("user"));
        paths.app = Some(app.to_path_buf());
        let fake = Arc::new(FakeCursor::running());
        let s = CrsrService::new(db, dir, paths, fake.clone(), accounts);
        (s, fake)
    }

    fn fake_bundle(dir: &Path, version: &str, with_anchors: bool) -> PathBuf {
        let app = dir.join("Cursor.app");
        let root = app.join("Contents/Resources/app");
        std::fs::create_dir_all(root.join("out")).unwrap();
        std::fs::write(
            root.join("product.json"),
            format!(r#"{{"applicationName":"Cursor","version":"{version}"}}"#),
        )
        .unwrap();
        std::fs::write(root.join("out/main.js"), "plain").unwrap();
        if with_anchors {
            let host = root.join("extensions/cursor-agent-host/dist/main.js");
            let local = root.join("extensions/cursor-always-local/dist/main.js");
            std::fs::create_dir_all(host.parent().unwrap()).unwrap();
            std::fs::create_dir_all(local.parent().unwrap()).unwrap();
            std::fs::write(
                &host,
                format!(
                    "pre {} mid _getTransportForService(e,t,r=!0){{let n; post",
                    inject::original(inject::VAR_DECLS[0])
                ),
            )
            .unwrap();
            std::fs::write(
                &local,
                format!(
                    "pre {} mid _getTransportForService(t,e,r=!0){{let n; post",
                    inject::original(inject::VAR_DECLS[1])
                ),
            )
            .unwrap();
        }
        app
    }

    fn write_cred(dir: &Path) {
        CrsrCredential {
            api_key: "crsr_abc123DEF".into(),
            access_token: jwt(),
            expires_at_ms: Some(credential::now_ms() + 3_600_000),
            account_email: Some("a@example.com".into()),
            account_id: Some("acct_1".into()),
            minted_at_ms: Some(credential::now_ms()),
            renewed_at_ms: None,
            gateway_url: None,
        }
        .save(dir)
        .unwrap();
    }

    #[test]
    fn status_reports_unsupported_version() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(dir.path(), OTHER_VERSION, false);
        let (s, _) = service(dir.path(), &app);
        let st = s.status().unwrap();
        assert_eq!(st.cursor_version.as_deref(), Some(OTHER_VERSION));
        assert!(!st.version_supported);
        assert!(!st.installed);
        assert!(!st.complete);
        assert_eq!(st.hits, 0);
        assert_eq!(st.backups, 0);
        assert!(st.credential.is_none());
    }

    #[test]
    fn install_on_unsupported_version_does_not_quit_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(dir.path(), OTHER_VERSION, true);
        write_cred(dir.path());
        let (s, fake) = service(dir.path(), &app);
        let err = s.install(false, &|_| {}).unwrap_err();
        assert_eq!(err.code, ErrorCode::SandUnsupportedVersion);
        assert!(err.hint.unwrap().contains("没有被改动"));
        assert_eq!(fake.quit_count(), 0);
    }

    #[test]
    fn install_without_credential_still_writes_the_patch() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(dir.path(), SUPPORTED_CURSOR_VERSION, true);
        let (s, fake) = service(dir.path(), &app);
        let out = s.install(false, &|_| {}).unwrap();
        assert!(out.wrote);
        assert!(out.status.complete);
        assert!(out.status.credential.is_none());
        assert_eq!(fake.quit_count(), 1);
    }

    #[test]
    fn install_refuses_sand_conflict_without_quitting() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(dir.path(), SUPPORTED_CURSOR_VERSION, true);
        write_cred(dir.path());
        let host = app.join("Contents/Resources/app/extensions/cursor-agent-host/dist/main.js");
        let mut text = std::fs::read_to_string(&host).unwrap();
        text.push_str("/*SAND_CLIENT_MODE_V1*/");
        std::fs::write(&host, text).unwrap();
        let (s, fake) = service(dir.path(), &app);
        let err = s.install(false, &|_| {}).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.message.contains("Sand"));
        assert_eq!(fake.quit_count(), 0);
    }

    #[test]
    fn install_uninstall_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(dir.path(), SUPPORTED_CURSOR_VERSION, true);
        let (s, fake) = service(dir.path(), &app);
        let out = s.install(false, &|_| {}).unwrap();
        assert!(out.wrote);
        assert!(out.status.complete);
        assert_eq!(out.status.hits, EXPECTED_HITS);
        assert_eq!(fake.quit_count(), 1);
        assert!(out.status.credential.is_none());

        let host = std::fs::read_to_string(
            app.join("Contents/Resources/app/extensions/cursor-agent-host/dist/main.js"),
        )
        .unwrap();
        assert!(host.contains(AUTH_MARKER));
        assert!(!host.contains("x-cursor-client-type"));

        let undone = s.uninstall(false, &|_| {}).unwrap();
        assert!(undone.wrote);
        assert!(!undone.status.installed);
        assert_eq!(fake.quit_count(), 2);
        let host = std::fs::read_to_string(
            app.join("Contents/Resources/app/extensions/cursor-agent-host/dist/main.js"),
        )
        .unwrap();
        assert!(!host.contains(AUTH_MARKER));
    }

    #[test]
    fn set_gateway_url_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(dir.path(), SUPPORTED_CURSOR_VERSION, true);
        let (s, _) = service(dir.path(), &app);

        // 1. 无凭证时写入 gateway_url
        let info = s
            .set_gateway_url(Some("http://127.0.0.1:15721/cursor/mock".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            info.gateway_url.as_deref(),
            Some("http://127.0.0.1:15721/cursor/mock")
        );

        // 2. 查看 status 能够读出凭证与 gateway_url
        let st = s.status().unwrap();
        assert_eq!(
            st.credential.unwrap().gateway_url.as_deref(),
            Some("http://127.0.0.1:15721/cursor/mock")
        );

        // 3. 切换到 native 直通模式
        let info2 = s
            .set_gateway_url(Some("http://127.0.0.1:15721/cursor/native".into()))
            .unwrap()
            .unwrap();
        assert_eq!(
            info2.gateway_url.as_deref(),
            Some("http://127.0.0.1:15721/cursor/native")
        );

        // 4. 清除 gateway_url（纯 gateway 凭证应自动删除）
        let cleared = s.set_gateway_url(None).unwrap();
        assert!(cleared.is_none());
        assert!(s.status().unwrap().credential.is_none());
    }
}
