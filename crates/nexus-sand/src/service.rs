//! 编排：status / install / uninstall / restore。
//!
//! 顺序是硬约束（和切号一样）：**预检 → 备份 → 退出 Cursor → 写入 → 校验 → 启动**。
//! 预检不过就什么都不碰；写入失败 `commit_plan` 会回滚。任何时刻只允许一个操作在跑
//! （`ErrorCode::Busy`）——两个 install 并发会把备份和回滚搅在一起。

use nexus_core::{AppError, ErrorCode, Result};
use nexus_cursor::{CursorControl, CursorPaths};
use nexus_store::{activity, Db};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::backup::{Backups, PlannedFile};
use crate::commit::{commit_plan, ensure_writable};
use crate::engine::{self, ApplyReport};
use crate::grokbot::GrokBotService;
use crate::integrity;
use crate::layout::SandLayout;
use crate::model::{
    DryRun, GrokBotAuthMode, InstallOptions, MarkerCounts, Operation, SandBackup, SandOutcome,
    SandProgress, SandStatus, SandStep, SUPPORTED_CURSOR_VERSION,
};
use crate::rules::{self, LayoutProfile, PatchRule, RuleId};

/// 排障用：把本机 Cursor 的推理改道到 `endpoint`（一般是本机的 passthrough），好在不改动
/// 官方客户端任何逻辑的前提下，录一份它**真实发出去**的 `InferenceStreamRequest`。
///
/// 平时本机不需要它——直连 api2 就好。它存在只为回答「官方客户端和我们发的到底差在哪」
/// 这类问题，而那个问题靠读 bundle 猜了太多轮。
///
/// 改道挂在 `_overrideServiceNameToTransportMapLowerPriorityThanMethodOverrides` 上，只对
/// Direct 引擎走的 `InferenceService/Stream` 生效；官方 `RunInference` 有 method 级 transport
/// 覆盖会绕过它——这也是当年 Session 引擎不能配改道的原因，现在 Session 已下线，不再需要强制。
pub fn with_inference_endpoint(
    mut options: InstallOptions,
    endpoint: Option<&str>,
) -> Result<InstallOptions> {
    let Some(raw) = endpoint.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(options);
    };
    options.inference_endpoint = Some(rules::validate_inference_endpoint(raw)?);
    Ok(options)
}

/// 备份保留份数。
const BACKUP_KEEP: usize = 10;
/// 等 Cursor 优雅退出的上限，超过就强制结束。
const QUIT_TIMEOUT: Duration = Duration::from_secs(20);
const SCOPE: &str = "sand";

pub struct SandService {
    db: Arc<Db>,
    data_dir: PathBuf,
    paths: CursorPaths,
    control: Arc<dyn CursorControl>,
    /// Grok Bot 桥：Box Relay 描述符 / 直连凭证都由它准备。与网关共享同一个实例
    /// （解过的钥匙串口令缓存在里面，别让两边各弹一次授权）。
    grokbot: Arc<GrokBotService>,
    busy: Mutex<()>,
}

impl SandService {
    pub fn new(
        db: Arc<Db>,
        data_dir: impl Into<PathBuf>,
        paths: CursorPaths,
        control: Arc<dyn CursorControl>,
    ) -> Self {
        let data_dir: PathBuf = data_dir.into();
        let grokbot = Arc::new(GrokBotService::new(&data_dir));
        Self::with_grokbot(db, data_dir, paths, control, grokbot)
    }

    pub fn with_grokbot(
        db: Arc<Db>,
        data_dir: impl Into<PathBuf>,
        paths: CursorPaths,
        control: Arc<dyn CursorControl>,
        grokbot: Arc<GrokBotService>,
    ) -> Self {
        Self {
            db,
            data_dir: data_dir.into(),
            paths,
            control,
            grokbot,
            busy: Mutex::new(()),
        }
    }

    pub fn grokbot(&self) -> &Arc<GrokBotService> {
        &self.grokbot
    }

    // ------------------------------------------------------------------ 只读

    pub fn status(&self) -> Result<SandStatus> {
        let layout = SandLayout::resolve(&self.paths)?;
        let contents = read_targets(&layout)?;
        let inference_endpoint = installed_endpoint(&contents);
        let grokbot_auth = installed_grokbot_auth(&contents);
        // 规则表构建失败（`catalog` 返回 Err）时 status 也要能用：markers 只依赖常量；
        // `remaining_ide` 退化为 0，且不给 dry-run（没规则就没法算「装了会命中什么」）。
        // 盘上装着端点改道 / 某种 Grok 鉴权时规则表要按盘上的来，否则 dry-run 会把它们算成
        // 「多出来的」或「要迁移的」。
        let catalog = rules::catalog_with_installed(
            &InstallOptions {
                grokbot_auth: if grokbot_auth.is_on() {
                    grokbot_auth
                } else {
                    InstallOptions::default().grokbot_auth
                },
                ..InstallOptions::default()
            },
            inference_endpoint.as_deref(),
        );
        let rules: &[PatchRule] = catalog.as_deref().unwrap_or(&[]);
        let agg = aggregate(&layout, &contents, rules);
        let complete = is_complete(&agg, layout.profile);
        let dry_run = if complete || catalog.is_err() {
            None
        } else {
            match build_plan(&layout, &contents, rules, Mode::Apply) {
                Ok((plan, report)) => {
                    let after = projected(&layout, &contents, &plan, rules);
                    Some(dry_run(&after, &report, plan.len() as u32, layout.profile))
                }
                Err(_) => None,
            }
        };
        let backups = Backups::new(&self.data_dir, &layout.app_root).list()?.len() as u32;
        let self_summary = contents
            .values()
            .find_map(|c| rules::installed_self_summary(c));
        let grok45_via_cua = contents
            .values()
            .find_map(|c| rules::installed_grok45_via_cua(c));
        let gb = self.grokbot.status();
        let grokbot_relay_configured = gb.relay.is_some();
        let grokbot_direct_configured = !gb.direct_stale
            && gb
                .direct
                .as_ref()
                .map(|d| !d.expired || d.can_renew)
                .unwrap_or(false);
        Ok(SandStatus {
            cursor_version: Some(layout.version.clone()),
            supported_version: SUPPORTED_CURSOR_VERSION.into(),
            version_supported: layout.version_supported(),
            installed: agg.markers.total() + agg.legacy > 0,
            complete,
            markers: agg.markers,
            remaining_ide: agg.remaining_ide,
            foreign_markers: agg.foreign,
            legacy_markers: agg.legacy,
            patched_files: agg.patched_files,
            dry_run,
            backups,
            self_summary,
            grok45_via_cua,
            inference_endpoint,
            grokbot_auth,
            grokbot_relay_configured,
            grokbot_direct_configured,
        })
    }

    pub fn backups(&self) -> Result<Vec<SandBackup>> {
        let layout = SandLayout::resolve(&self.paths)?;
        Backups::new(&self.data_dir, &layout.app_root).list()
    }

    pub fn remove_backup(&self, id: &str) -> Result<()> {
        let layout = SandLayout::resolve(&self.paths)?;
        Backups::new(&self.data_dir, &layout.app_root).remove(id)
    }

    /// 选了某种 Grok 鉴权就得有它的前提：Box Relay 要 `grok-box-relay.json`，直连要一份还能用
    /// （未过期或可续期）的 `grokbot-stream-credential.json`。没有就当场去 Grok Bot 拿——拿不到
    /// 才报错，让用户知道缺的是哪一步，而不是装完发现 Agent 面板报 16。
    fn ensure_grokbot_prerequisites(&self, mode: GrokBotAuthMode) -> Result<()> {
        match mode {
            GrokBotAuthMode::Off => Ok(()),
            GrokBotAuthMode::BoxRelay => {
                if self.grokbot.relay_descriptor().is_some() {
                    return Ok(());
                }
                self.grokbot.refresh_relay().map(|_| ()).map_err(|e| {
                    e.with_hint(
                        "Box Relay 模式需要 Grok Bot 已登录并连上 Box，且 Bot 端装过 relay（教程第 3 步）。",
                    )
                })
            }
            GrokBotAuthMode::Direct => {
                // 过期不可续、或 Grok Bot 已换号（凭证是旧号的）都算不可用 → 重生成。
                if self.grokbot.direct_usable()? {
                    return Ok(());
                }
                let gb = self.grokbot.clone();
                nexus_grokbot::run_sync(async move { gb.mint_direct().await })
                    .map(|_| ())
                    .map_err(|e| {
                        e.with_hint("直连模式需要 Grok Bot 已登录、Box 在线；先打开 Grok Bot 发一句话让 pod 醒来。")
                    })
            }
        }
    }

    // ------------------------------------------------------------------ 写

    pub fn install(
        &self,
        options: InstallOptions,
        progress: &dyn Fn(SandProgress),
    ) -> Result<SandOutcome> {
        let _guard = self.acquire()?;
        progress(SandProgress::new(
            SandStep::Preflight,
            "检查 Cursor 版本与锚点",
        ));
        let layout = SandLayout::resolve(&self.paths)?;
        if !layout.version_supported() {
            return Err(AppError::new(
                ErrorCode::SandUnsupportedVersion,
                format!(
                    "当前 Cursor 是 {}，Sand 补丁只适配 {SUPPORTED_CURSOR_VERSION}。",
                    layout.version
                ),
            )
            .with_hint("等待 Nexus 更新适配这个版本；Cursor 没有被改动。"));
        }
        let contents = read_targets(&layout)?;
        check_preflight_anchors(&contents)?;
        if contents.values().any(|c| c.contains("/*CRSR_AUTH_V1*/")) {
            return Err(
                AppError::invalid("CRSR 通道占用了同一处鉴权挂点，不能和 Sand 同时装。").with_hint(
                    "先到「CRSR 通道」页卸载，再装 Sand。两条补丁改的是同一段 applyAuthorization。",
                ),
            );
        }
        if options.grokbot_auth == GrokBotAuthMode::BoxRelay && options.inference_endpoint.is_some()
        {
            return Err(AppError::invalid(
                "Box Relay 会把 Stream 直接改道到 Grok Bot 的 Box，和「推理经本机网关」互斥。",
            )
            .with_hint(
                "要经本机网关，把 Grok Bot 鉴权设成「关」（由网关用 Grok Bot 额度）或「直连」。",
            ));
        }
        self.ensure_grokbot_prerequisites(options.grokbot_auth)?;
        let installed_ep = installed_endpoint(&contents);
        let installed_grokbot = installed_grokbot_auth(&contents);
        let mut rules = rules::catalog_with_installed(&options, installed_ep.as_deref())?;
        if options.grokbot_auth.is_on() {
            // 第一版直连 interceptor（从未生效）还在盘上时先把它还原成 stock 锚点，再打新块。
            // 排最前：它占着的正是端点改道那条规则的锚点。只进 Apply 的表，不进 uninstall 的
            // （Literal 的 remove 会把 stock 锚点反向改回旧 interceptor）。
            let mut migrated = rules::grokbot_legacy_migration_rules();
            migrated.append(&mut rules);
            rules = migrated;
        }
        // 选项里关掉了端点改道、盘上却装着：Apply 语义下 Literal 只会「original → patched」，
        // 不会把 patched 还回去，所以先把旧端点那两处剥掉（同一次写入、同一份备份），再打其余补丁。
        let strip = match (&options.inference_endpoint, &installed_ep) {
            (None, Some(old)) => rules::endpoint_rules(old, None)?,
            _ => Vec::new(),
        };
        // 同理：Grok 鉴权选了「关」而盘上装着任一形态（含旧 interceptor）→ 剥掉。
        // 形态之间的切换（Box Relay ↔ 直连）不走 strip，规则表里互为 legacy，Apply 原地换。
        let grokbot_strip = if !options.grokbot_auth.is_on() && installed_grokbot.is_on() {
            rules::grokbot_auth_rules(installed_grokbot)
                .into_iter()
                .chain(rules::grokbot_legacy_interceptor_strip_rules())
                .collect()
        } else {
            Vec::new()
        };
        let strip: Vec<PatchRule> = strip.into_iter().chain(grokbot_strip).collect();
        let before = aggregate(&layout, &contents, &rules);
        if before.foreign > 0 {
            return Err(AppError::new(
                ErrorCode::SandForeignMarkers,
                format!("检测到 {} 处其它工具留下的 Sand 标记。", before.foreign),
            )
            .with_hint("先用原来的工具卸载，再回来安装；本工具不会覆盖别人的改动。"));
        }
        let (plan, report) =
            build_plan_with_strip(&layout, &contents, &rules, Mode::Apply, &strip)?;
        if plan.is_empty() {
            if is_complete(&before, layout.profile) {
                // 文件已是目标状态，但 Cursor 可能还攥着装之前的 4884.js。
                // 不重启的话，界面「盘上：开」和 Agent 实际发出去的模型会对不上。
                let relaunched = if options.relaunch {
                    progress(SandProgress::new(SandStep::QuitCursor, "正在退出 Cursor"));
                    let _ = self.control.quit(QUIT_TIMEOUT);
                    self.maybe_launch(true, progress)
                } else {
                    false
                };
                progress(SandProgress::new(SandStep::Done, "完成"));
                let status = self.status()?;
                return Ok(SandOutcome {
                    operation: Operation::Install,
                    wrote: false,
                    files_written: 0,
                    backup_id: None,
                    cursor_relaunched: relaunched,
                    status,
                });
            }
            return Err(anchor_mismatch(&dry_run(
                &before,
                &report,
                0,
                layout.profile,
            )));
        }
        let after = projected(&layout, &contents, &plan, &rules);
        let dr = dry_run(&after, &report, plan.len() as u32, layout.profile);
        if !dr.anchors_complete {
            return Err(anchor_mismatch(&dr));
        }
        // 端点改道是可选项、不进通用硬校验，所以在这里单独确认它会落成选项要的样子：要装就得
        // 两处都在（建 transport + 挂路由），要关就得一处不剩。静默漏掉的话表现是「装完了但 Agent
        // 一发推理就连不上」或「关了开关推理还在绕网关」，两种都最难查。
        let want_endpoint_hits = if options.inference_endpoint.is_some() {
            2
        } else {
            0
        };
        if after.markers.inference_endpoint != want_endpoint_hits {
            return Err(AppError::new(
                ErrorCode::SandAnchorMismatch,
                format!(
                    "推理端点改道会命中 {} 处（需 {want_endpoint_hits} 处）。",
                    after.markers.inference_endpoint
                ),
            )
            .with_hint("Cursor 没有被改动。这个版本的 transport 装配处结构与适配版本不同。"));
        }
        ensure_writable(&plan.iter().map(|f| f.path.clone()).collect::<Vec<_>>())?;

        let backups = Backups::new(&self.data_dir, &layout.app_root);
        progress(SandProgress::new(SandStep::QuitCursor, "正在退出 Cursor"));
        self.control.quit(QUIT_TIMEOUT)?;

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
                let after = aggregate(&layout, &contents, &rules);
                if !is_complete(&after, layout.profile)
                    || after.markers.inference_endpoint != want_endpoint_hits
                {
                    return Err(AppError::new(
                        ErrorCode::SandIntegrity,
                        format!(
                            "安装后状态校验失败：remainingIde={}，foreign={}，legacy={}，endpoint={}（需 {want_endpoint_hits}）",
                            after.remaining_ide,
                            after.foreign,
                            after.legacy,
                            after.markers.inference_endpoint
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
            format!("已安装 Sand 补丁（{} 个文件）", committed.files_written),
        );

        let relaunched = self.maybe_launch(options.relaunch, progress);
        progress(SandProgress::new(SandStep::Done, "完成"));
        Ok(SandOutcome {
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
    ) -> Result<SandOutcome> {
        let _guard = self.acquire()?;
        progress(SandProgress::new(SandStep::Preflight, "检查已安装的标记"));
        let layout = SandLayout::resolve(&self.paths)?;
        let contents = read_targets(&layout)?;
        // 端点 URL / Grok 鉴权形态是规则文本的一部分：卸载要用装的时候那一个才反向得了。
        let grokbot_auth = installed_grokbot_auth(&contents);
        let rules = rules::catalog_with_installed(
            &InstallOptions {
                grokbot_auth: if grokbot_auth.is_on() {
                    grokbot_auth
                } else {
                    InstallOptions::default().grokbot_auth
                },
                ..InstallOptions::default()
            },
            installed_endpoint(&contents).as_deref(),
        )?;
        let before = aggregate(&layout, &contents, &rules);
        if before.foreign > 0 {
            return Err(AppError::new(
                ErrorCode::SandForeignMarkers,
                "检测到无法识别的 Sand 标记，拒绝修改。",
            )
            .with_hint("请先用原来的工具卸载。"));
        }
        let (plan, _) = build_plan(&layout, &contents, &rules, Mode::Remove)?;
        if plan.is_empty() {
            let status = self.status()?;
            return Ok(SandOutcome {
                operation: Operation::Uninstall,
                wrote: false,
                files_written: 0,
                backup_id: None,
                cursor_relaunched: false,
                status,
            });
        }
        ensure_writable(&plan.iter().map(|f| f.path.clone()).collect::<Vec<_>>())?;

        let backups = Backups::new(&self.data_dir, &layout.app_root);
        progress(SandProgress::new(SandStep::QuitCursor, "正在退出 Cursor"));
        self.control.quit(QUIT_TIMEOUT)?;
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
                let after = aggregate(&layout, &contents, &rules);
                if after.markers.total() + after.legacy + after.foreign > 0 {
                    return Err(AppError::new(
                        ErrorCode::SandIntegrity,
                        format!(
                            "卸载后仍有 {} 处 Sand 标记（{}）。",
                            after.markers.total() + after.legacy + after.foreign,
                            leftover_marker_labels(&after)
                        ),
                    ));
                }
                verify_integrity(&layout)
            },
        )?;
        let _ = backups.prune(BACKUP_KEEP);
        activity::info(&self.db, SCOPE, None, "已卸载 Sand 补丁，Cursor 恢复原版");

        let relaunched = self.maybe_launch(relaunch, progress);
        progress(SandProgress::new(SandStep::Done, "完成"));
        Ok(SandOutcome {
            operation: Operation::Uninstall,
            wrote: true,
            files_written: committed.files_written,
            backup_id: Some(committed.backup_id),
            cursor_relaunched: relaunched,
            status: self.status()?,
        })
    }

    /// 把某份备份里的原始字节写回去。这是「紧急刹车」：不认锚点、不认 marker，只按字节还原。
    /// 还原前照样先备一份（撤销本身也得能撤销）。
    pub fn restore_backup(
        &self,
        id: &str,
        relaunch: bool,
        progress: &dyn Fn(SandProgress),
    ) -> Result<SandOutcome> {
        let _guard = self.acquire()?;
        progress(SandProgress::new(SandStep::Preflight, "读取备份"));
        let layout = SandLayout::resolve(&self.paths)?;
        let backups = Backups::new(&self.data_dir, &layout.app_root);
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
            let status = self.status()?;
            return Ok(SandOutcome {
                operation: Operation::Restore,
                wrote: false,
                files_written: 0,
                backup_id: None,
                cursor_relaunched: false,
                status,
            });
        }
        ensure_writable(&plan.iter().map(|f| f.path.clone()).collect::<Vec<_>>())?;
        progress(SandProgress::new(SandStep::QuitCursor, "正在退出 Cursor"));
        self.control.quit(QUIT_TIMEOUT)?;
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
        Ok(SandOutcome {
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
            AppError::new(ErrorCode::Busy, "已经有一个 Sand 操作在进行。")
                .with_hint("等它结束再试。")
        })
    }

    fn maybe_launch(&self, relaunch: bool, progress: &dyn Fn(SandProgress)) -> bool {
        if !relaunch {
            return false;
        }
        progress(SandProgress::new(SandStep::Launch, "正在启动 Cursor"));
        match self.control.launch() {
            Ok(()) => true,
            Err(err) => {
                activity::warn(&self.db, SCOPE, None, format!("启动 Cursor 失败：{err}"));
                false
            }
        }
    }
}

// ---------------------------------------------------------------------- 纯函数

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    Apply,
    Remove,
}

/// 盘上装着的推理端点（`None` = 没改道）。status 显示它，install / uninstall 据它组规则表。
pub(crate) fn installed_endpoint(contents: &HashMap<PathBuf, String>) -> Option<String> {
    contents
        .values()
        .find_map(|c| rules::installed_inference_endpoint(c))
}

/// 盘上装着哪种 Grok 鉴权（任一目标文件命中即算）。
pub(crate) fn installed_grokbot_auth(contents: &HashMap<PathBuf, String>) -> GrokBotAuthMode {
    contents
        .values()
        .map(|c| rules::installed_grokbot_auth(c))
        .find(|m| m.is_on())
        .unwrap_or(GrokBotAuthMode::Off)
}

/// 全部目标文件的内容（UTF-8）。
pub(crate) fn read_targets(layout: &SandLayout) -> Result<HashMap<PathBuf, String>> {
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

pub(crate) struct Aggregate {
    pub(crate) markers: MarkerCounts,
    pub(crate) remaining_ide: u32,
    pub(crate) foreign: u32,
    pub(crate) legacy: u32,
    pub(crate) patched_files: Vec<String>,
}

pub(crate) fn aggregate(
    layout: &SandLayout,
    contents: &HashMap<PathBuf, String>,
    rules: &[PatchRule],
) -> Aggregate {
    let mut agg = Aggregate {
        markers: MarkerCounts::default(),
        remaining_ide: 0,
        foreign: 0,
        legacy: 0,
        patched_files: Vec::new(),
    };
    for t in &layout.targets {
        let Some(c) = contents.get(t) else { continue };
        let ins = engine::inspect(c, rules);
        agg.markers = agg.markers.plus(&ins.markers);
        agg.remaining_ide += ins.remaining_ide;
        agg.foreign += ins.foreign;
        agg.legacy += ins.legacy;
        if ins.touched() {
            agg.patched_files.push(layout.relative(t));
        }
    }
    agg
}

fn leftover_marker_labels(after: &Aggregate) -> String {
    let mut bits: Vec<String> = RuleId::ALL
        .iter()
        .filter_map(|id| {
            let n = id.get(&after.markers);
            (n > 0).then(|| format!("{} {n} 处", id.name()))
        })
        .collect();
    if after.legacy > 0 {
        bits.push(format!("旧版 {} 处", after.legacy));
    }
    if after.foreign > 0 {
        bits.push(format!("外部 {} 处", after.foreign));
    }
    if bits.is_empty() {
        "未知".into()
    } else {
        bits.join("、")
    }
}

/// 等价于安装器的 `stream_mode_installed`。期望值随安装形态走（remote server 少 4 个目标文件）。
pub(crate) fn is_complete(agg: &Aggregate, profile: LayoutProfile) -> bool {
    agg.remaining_ide == 0
        && agg.foreign == 0
        && agg.legacy == 0
        && RuleId::ALL.iter().all(|id| {
            id.expected_for(profile)
                .is_none_or(|n| id.get(&agg.markers) == n)
        })
}

/// 「装了之后会是什么样」——直接对计划写入的内容做 inspect，而不是 `before + hits + migrated`
/// 那样做加法。加法在「同一个 marker 的档位互换」（action route 三档、session V1→当前）上会
/// 双计：迁移前 marker 已在（before=1），迁移又记 1，合计 2 ≠ 1，于是重装换档被误拒。
/// 用真实内容算，dry-run 预测的就是写完后 `is_complete` 看到的那份。
pub(crate) fn projected(
    layout: &SandLayout,
    contents: &HashMap<PathBuf, String>,
    plan: &[PlannedFile],
    rules: &[PatchRule],
) -> Aggregate {
    let mut after: HashMap<PathBuf, String> = contents.clone();
    for f in plan {
        if let Ok(text) = String::from_utf8(f.next.clone()) {
            after.insert(f.path.clone(), text);
        }
    }
    aggregate(layout, &after, rules)
}

pub(crate) fn dry_run(
    after: &Aggregate,
    report: &ApplyReport,
    files: u32,
    profile: LayoutProfile,
) -> DryRun {
    let mut missing: Vec<String> = RuleId::ALL
        .iter()
        .filter(|id| {
            id.expected_for(profile)
                .is_some_and(|n| id.get(&after.markers) != n)
        })
        .map(|id| {
            format!(
                "{}（{} / 需 {}）",
                id.name(),
                id.get(&after.markers),
                id.expected_for(profile).unwrap_or(0)
            )
        })
        .collect();
    if after.remaining_ide > 0 {
        missing.push(format!(
            "残留未打的 client-type 位置（{}）",
            after.remaining_ide
        ));
    }
    if after.legacy > 0 {
        missing.push(format!("未能迁移的旧版 marker（{}）", after.legacy));
    }
    if after.foreign > 0 {
        missing.push(format!("外部工具 marker（{}）", after.foreign));
    }
    DryRun {
        would_hit: report.hits.plus(&report.migrated),
        files_to_change: files,
        anchors_complete: missing.is_empty(),
        missing,
    }
}

pub(crate) fn anchor_mismatch(dr: &DryRun) -> AppError {
    AppError::new(
        ErrorCode::SandAnchorMismatch,
        format!("这个 Cursor 的锚点与预期不符：{}", dr.missing.join("；")),
    )
    .with_hint("Cursor 没有被改动。可能是 bundle 被其它工具改过，或该版本细节有差异；等待适配。")
}

pub(crate) fn check_preflight_anchors(contents: &HashMap<PathBuf, String>) -> Result<()> {
    for a in rules::preflight_anchors() {
        let n = a.count(contents.values().map(String::as_str));
        if n != a.expect {
            return Err(AppError::new(
                ErrorCode::SandAnchorMismatch,
                format!("预检锚点「{}」命中 {n} 次（需 {}）。", a.name, a.expect),
            )
            .with_hint("Cursor 没有被改动。该版本的 bundle 结构与适配版本不同。"));
        }
    }
    Ok(())
}

/// 算出要写哪些文件。含扩展 hash 同步与 product.json checksum 同步。
pub(crate) fn build_plan(
    layout: &SandLayout,
    contents: &HashMap<PathBuf, String>,
    rules: &[PatchRule],
    mode: Mode,
) -> Result<(Vec<PlannedFile>, ApplyReport)> {
    build_plan_with_strip(layout, contents, rules, mode, &[])
}

/// [`build_plan`] 加一步前置剥离：Apply 之前先把 `strip` 里的规则 **remove** 掉。
///
/// 给「选项关掉了某个可选补丁、盘上却还装着」用（今天只有端点改道一种）：Literal 的 Apply 只会
/// `original → patched`，不会反向；而单独跑一次 Remove 又是另一次退出 Cursor、另一份备份。剥离
/// 和打补丁放进同一份计划里，`PlannedFile.original` 仍是磁盘上的原字节，备份 / 回滚不受影响。
pub(crate) fn build_plan_with_strip(
    layout: &SandLayout,
    contents: &HashMap<PathBuf, String>,
    rules: &[PatchRule],
    mode: Mode,
    strip: &[PatchRule],
) -> Result<(Vec<PlannedFile>, ApplyReport)> {
    let mut plan: Vec<PlannedFile> = Vec::new();
    let mut report = ApplyReport::default();
    // 剥掉的那几条不能再出现在 Apply 的规则表里：`catalog_with_installed` 为了让 status / uninstall
    // 认得盘上的端点会把它的规则带上，Apply 遇到刚被 remove 还原出来的 original 锚点就会**原地装回去**。
    // 2026-09-08 在 3.19.13 的 server bundle 上抓到：strip 删 2 处、apply 又命中 2 处，端点关不掉。
    let apply_rules: Vec<&PatchRule> = rules
        .iter()
        .filter(|r| !strip.iter().any(|s| s.id == r.id))
        .collect();
    for t in &layout.targets {
        let Some(content) = contents.get(t) else {
            continue;
        };
        let (next, hits, migrated) = match mode {
            Mode::Apply => {
                let stripped = if strip.is_empty() {
                    content.clone()
                } else {
                    engine::remove(content, strip).0
                };
                let (n, r) = engine::apply_refs(&stripped, apply_rules.iter().copied());
                (n, r.hits, r.migrated)
            }
            Mode::Remove => {
                let (n, removed) = engine::remove(content, rules);
                (n, removed, MarkerCounts::default())
            }
        };
        report.hits = report.hits.plus(&hits);
        report.migrated = report.migrated.plus(&migrated);
        if next != *content {
            plan.push(PlannedFile {
                path: t.clone(),
                original: content.as_bytes().to_vec(),
                next: next.into_bytes(),
            });
        }
    }
    if plan.is_empty() {
        return Ok((plan, report));
    }

    // 扩展 main.js 改了 → 同步 extensionHostProcess.js 里的内嵌 hash。
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

    // product.json 的 checksums。
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
    Ok((plan, report))
}

/// 写完之后：扩展 hash 与 product checksums 都得对得上磁盘。
pub(crate) fn verify_integrity(layout: &SandLayout) -> Result<()> {
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

#[allow(dead_code)]
fn _assert_send_sync()
where
    SandService: Send + Sync,
{
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_cursor::app::testing::FakeCursor;
    use std::path::Path;

    /// 任何不等于 `SUPPORTED_CURSOR_VERSION` 的版本号。不写死真实旧版本号：Cursor 一升级它就不「旧」了。
    const OTHER_VERSION: &str = "0.0.1";

    fn service(dir: &Path, app: &Path) -> (SandService, Arc<FakeCursor>) {
        let db = Arc::new(Db::open(dir.join("nexus.db")).unwrap());
        let mut paths = CursorPaths::from_user_dir(dir.join("user"));
        paths.app = Some(app.to_path_buf());
        let fake = Arc::new(FakeCursor::running());
        let s = SandService::new(db, dir, paths, fake.clone());
        (s, fake)
    }

    fn fake_bundle(dir: &Path, version: &str) -> PathBuf {
        let app = dir.join("Cursor.app");
        let root = app.join("Contents/Resources/app");
        std::fs::create_dir_all(root.join("out")).unwrap();
        std::fs::write(
            root.join("product.json"),
            format!(r#"{{"applicationName":"Cursor","version":"{version}"}}"#),
        )
        .unwrap();
        std::fs::write(root.join("out/main.js"), "plain").unwrap();
        app
    }

    #[test]
    fn status_reports_unsupported_version_and_a_dry_run_that_finds_no_anchors() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(dir.path(), OTHER_VERSION);
        let (s, _) = service(dir.path(), &app);
        let st = s.status().unwrap();
        assert_eq!(st.cursor_version.as_deref(), Some(OTHER_VERSION));
        assert!(!st.version_supported);
        assert!(!st.installed);
        assert!(!st.complete);
        // 版本不对的 bundle 上一个锚点都命不中：dry-run 如实说「锚点不齐、不会改任何文件」。
        let dr = st.dry_run.expect("未完整安装时给 dry-run");
        assert!(!dr.anchors_complete);
        assert_eq!(dr.files_to_change, 0);
        assert_eq!(dr.would_hit.total(), 0);
        assert_eq!(st.backups, 0);
    }

    #[test]
    fn install_on_unsupported_version_is_refused_without_touching_cursor() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(dir.path(), OTHER_VERSION);
        let (s, fake) = service(dir.path(), &app);
        let err = s.install(InstallOptions::default(), &|_| {}).unwrap_err();
        assert_eq!(err.code, ErrorCode::SandUnsupportedVersion);
        assert!(err.hint.unwrap().contains("没有被改动"));
        assert_eq!(fake.quit_count(), 0, "预检失败不能去关用户的编辑器");
    }

    #[test]
    fn install_on_supported_version_but_unrecognized_bundle_stops_at_preflight_without_quitting_cursor(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(dir.path(), SUPPORTED_CURSOR_VERSION);
        let (s, fake) = service(dir.path(), &app);
        let err = s.install(InstallOptions::default(), &|_| {}).unwrap_err();
        // 版本号对、但 bundle 内容认不出（预检锚点命中 0）→ 锚点不齐，什么都不碰。
        assert_eq!(err.code, ErrorCode::SandAnchorMismatch);
        assert!(err.hint.unwrap().contains("没有被改动"));
        assert_eq!(fake.quit_count(), 0, "预检失败不能去关用户的编辑器");
    }

    #[test]
    fn a_local_inference_redirect_only_sets_the_endpoint() {
        let routed =
            with_inference_endpoint(InstallOptions::default(), Some(" http://127.0.0.1:8790 "))
                .unwrap();
        assert_eq!(
            routed.inference_endpoint.as_deref(),
            Some("http://127.0.0.1:8790")
        );
        assert_eq!(
            InstallOptions {
                inference_endpoint: None,
                ..routed
            },
            InstallOptions::default(),
            "除端点外其它选项原样保留"
        );
    }

    #[test]
    fn without_an_endpoint_the_options_are_untouched() {
        for endpoint in [None, Some(""), Some("   ")] {
            let same = with_inference_endpoint(InstallOptions::default(), endpoint).unwrap();
            assert_eq!(same, InstallOptions::default(), "endpoint={endpoint:?}");
        }
    }

    #[test]
    fn a_malformed_endpoint_is_refused_rather_than_silently_dropped() {
        // 静默忽略等于「装完了但录不到」，那种沉默比报错难查得多。
        let err = with_inference_endpoint(InstallOptions::default(), Some("127.0.0.1:8790"));
        assert!(err.is_err(), "缺 scheme 要报错");
    }

    #[test]
    fn restore_of_unknown_backup_is_backup_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let app = fake_bundle(dir.path(), SUPPORTED_CURSOR_VERSION);
        let (s, _) = service(dir.path(), &app);
        let err = s.restore_backup("nope", false, &|_| {}).unwrap_err();
        assert_eq!(err.code, ErrorCode::BackupNotFound);
    }

    fn empty_aggregate() -> Aggregate {
        Aggregate {
            markers: MarkerCounts::default(),
            remaining_ide: 0,
            foreign: 0,
            legacy: 0,
            patched_files: Vec::new(),
        }
    }

    #[test]
    fn dry_run_lists_every_missing_anchor_by_name() {
        let dr = dry_run(
            &empty_aggregate(),
            &ApplyReport::default(),
            0,
            LayoutProfile::Desktop,
        );
        assert!(!dr.anchors_complete);
        let skip = RuleId::ALL
            .iter()
            .filter(|id| id.expected_for(LayoutProfile::Desktop).is_none())
            .count();
        assert_eq!(dr.missing.len(), RuleId::ALL.len() - skip);
        assert!(dr
            .missing
            .iter()
            .any(|m| m.starts_with("client-type（0 / 需 23）")));
    }

    #[test]
    fn dry_run_flags_leftover_ide_legacy_and_foreign() {
        let mut agg = empty_aggregate();
        agg.remaining_ide = 2;
        agg.legacy = 1;
        agg.foreign = 3;
        let dr = dry_run(&agg, &ApplyReport::default(), 0, LayoutProfile::Desktop);
        assert!(dr
            .missing
            .iter()
            .any(|m| m.contains("残留未打") && m.contains('2')));
        assert!(dr
            .missing
            .iter()
            .any(|m| m.contains("旧版") && m.contains('1')));
        assert!(dr
            .missing
            .iter()
            .any(|m| m.contains("外部") && m.contains('3')));
    }

    /// 回归：同一个 marker 的档位互换（action route agent → agent_plan）不能被算成 2 个。
    /// 旧实现 `before + hits + migrated` 会得 2 ≠ 1 而拒装；现在按计划内容 inspect，得 1。
    #[test]
    fn switching_mode_tier_on_an_installed_bundle_projects_exactly_one_marker() {
        use crate::rules::{RuleKind, SAND_MANAGED_ACTION_ROUTE_MARKER};
        let dir = tempfile::tempdir().unwrap();
        let app_root = dir.path().join("app");
        std::fs::create_dir_all(&app_root).unwrap();
        let target = app_root.join("x.js");
        // build_plan 末尾要读 product.json 同步 checksums；给一个没有 checksums 的最小文件。
        std::fs::write(
            app_root.join("product.json"),
            r#"{"applicationName":"Cursor","version":"0.0.0"}"#,
        )
        .unwrap();
        // 盘上已是 agent 档
        let agent = format!("return{SAND_MANAGED_ACTION_ROUTE_MARKER}AGENT_ONLY;");
        let plan_tier = format!("return{SAND_MANAGED_ACTION_ROUTE_MARKER}AGENT_PLAN;");
        std::fs::write(&target, &agent).unwrap();
        let layout = SandLayout {
            install_root: app_root.clone(),
            app_root: app_root.clone(),
            product_json: app_root.join("product.json"),
            targets: vec![target.clone()],
            ext_host: None,
            version: SUPPORTED_CURSOR_VERSION.into(),
            profile: LayoutProfile::Desktop,
        };
        // 只有 action route 一条规则，当前档 agent_plan，legacy 含 agent 档
        let rules = vec![PatchRule {
            id: RuleId::ManagedActionRoute,
            kind: RuleKind::Literal {
                original: "return ORIGINAL;".into(),
                patched: plan_tier.clone(),
                legacy: vec![agent.clone()],
            },
        }];
        let mut contents = HashMap::new();
        contents.insert(target.clone(), agent.clone());
        let before = aggregate(&layout, &contents, &rules);
        assert_eq!(before.markers.managed_action_route, 1);

        let (plan, report) = build_plan(&layout, &contents, &rules, Mode::Apply).unwrap();
        assert_eq!(report.migrated.managed_action_route, 1, "应记为迁移");
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].next, plan_tier.as_bytes());

        // 旧算法：1 + 0 + 1 = 2 → 错。新算法看计划内容：仍是 1。
        let after = projected(&layout, &contents, &plan, &rules);
        assert_eq!(after.markers.managed_action_route, 1);
        let dr = dry_run(&after, &report, plan.len() as u32, LayoutProfile::Desktop);
        assert!(
            !dr.missing.iter().any(|m| m.starts_with("action route")),
            "换档不该被算成锚点不齐：{:?}",
            dr.missing
        );
    }
}
