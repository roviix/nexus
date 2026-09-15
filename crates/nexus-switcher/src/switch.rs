//! 切号编排。
//!
//! 两条路径（R6）：
//!
//! ```text
//! 热切（默认）：备份当前 → deep link 交给 Cursor → 轮询确认 accessToken 已变
//! 冷切（兜底）：备份 → 退出 Cursor → [单事务] 清旧+写新 auth → [可选] 写机器码 → 启动
//! ```
//!
//! 热切条件：`prefer_hot && Cursor 在跑 && !switch_machine_ids`。
//! 机器码启动时读一次就缓存，要换指纹只能冷切。
//!
//! 冷切每一步为什么在这个位置：
//!   - **备份先做**：后面任何一步出问题都能原样回到切换前；
//!   - **退出再写**：Cursor 在跑会用内存状态把库覆盖回去；
//!   - **清旧+写新同一事务**：不存在「已清未写」的未登录态；
//!   - **最后启动**：让它带着新状态冷启。
//!
//! `switch_to` **只由用户点击触发**：这个 crate 里没有任何定时器或后台任务会调到它。

use crate::backup::{Backups, DEFAULT_KEEP};
use crate::book::SwitchBook;
use crate::model::{
    AuthBackup, BackupReason, Overview, SwitchOptions, SwitchOutcome, SwitchProgress,
};
use nexus_core::{now_iso, AppError, BackupId, ErrorCode, MachineProfile, ProfileId, Result};
use nexus_cursor::{AuthBundle, Cursor, CursorControl};
use nexus_store::{activity, settings, Db, SecretStore};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// 单飞闸的守卫。析构即放闸。
struct RunGuard<'a>(&'a AtomicBool);

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// 等 Cursor 退出的上限。超过就强制结束。
const QUIT_TIMEOUT: Duration = Duration::from_secs(20);

/// 热切后等 Cursor 把新 token 写进磁盘的上限。
const HOT_CONFIRM_TIMEOUT: Duration = Duration::from_secs(12);
const HOT_CONFIRM_POLL: Duration = Duration::from_millis(250);

/// 进度回调。`nexus-switcher` 不认识 Tauri，事件怎么发是上层的事。
pub type ProgressSink<'a> = &'a dyn Fn(SwitchProgress);

/// 什么都不做的回调，非交互调用（测试、脚本）用。
pub fn ignore_progress(_: SwitchProgress) {}

pub struct Switcher {
    db: Arc<Db>,
    cursor: Cursor,
    /// 进程控制注入进来而不是现构造：切号编排是这个应用里最不能出错、又最难手工回归的
    /// 一段，它必须能在不真的关掉用户编辑器的前提下被测到。
    control: Arc<dyn CursorControl>,
    secrets: Arc<dyn SecretStore>,
    /// 单飞闸。两个切号同时跑会把 A 的 token 和 B 的机器码配到一起。
    running: AtomicBool,
    pub book: SwitchBook,
    pub backups: Backups,
}

impl Switcher {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>, cursor: Cursor) -> Self {
        let control = Arc::new(cursor.control());
        Self::with_control(db, secrets, cursor, control)
    }

    pub fn with_control(
        db: Arc<Db>,
        secrets: Arc<dyn SecretStore>,
        cursor: Cursor,
        control: Arc<dyn CursorControl>,
    ) -> Self {
        Self {
            book: SwitchBook::new(db.clone(), secrets.clone()),
            backups: Backups::new(db.clone(), secrets.clone()),
            secrets,
            running: AtomicBool::new(false),
            db,
            cursor,
            control,
        }
    }

    /// 拿下单飞闸。守卫析构时自动放开，所以中途 `?` 提前返回也不会把闸卡死。
    fn begin(&self) -> Result<RunGuard<'_>> {
        if self.running.swap(true, Ordering::SeqCst) {
            return Err(
                AppError::new(ErrorCode::Busy, "已经有一个切号操作在进行中。")
                    .with_hint("等它跑完再试。同时切两次会把两个号的状态搅在一起。"),
            );
        }
        Ok(RunGuard(&self.running))
    }

    /// 备份存不存得住。
    ///
    /// 切号会清掉 Cursor 里当前那个号的 token，刚存的备份是它唯一的另一份。备份只在
    /// 内存里的话，切一次号就等于永久弄丢上一个号 —— 那比不让切严重得多。
    fn require_durable_secrets(&self) -> Result<()> {
        if self.secrets.durable() {
            return Ok(());
        }
        Err(
            AppError::new(ErrorCode::Internal, "秘密存储留不住备份，因此不能切号。")
                .with_hint("现在切号会永久丢失当前登录的凭证。这是装配上的问题，请报告。"),
        )
    }

    /// 本机 Cursor 的路径与自检口。设置页和状态页要读它。
    pub fn cursor(&self) -> &Cursor {
        &self.cursor
    }

    /// 切号页顶部那一栏。任何一项读失败都降级成「未知」而不是让整页打不开。
    pub fn overview(&self) -> Result<Overview> {
        let check = self.cursor.check();
        let current = self.cursor.state.current_account().unwrap_or(None);
        let ids = self.cursor.machine.read();
        Ok(Overview {
            machine_id_owner: self.book.owner_of_machine(&ids.machine_id).unwrap_or(None),
            machine_id_short: ids.short(),
            has_original_machine: self.original_machine()?.is_some(),
            cursor_running: self.control.is_running().unwrap_or(false),
            current,
            check,
        })
    }

    /// 带上「谁是当前登录」的切号本列表。
    pub fn list(&self) -> Result<Vec<crate::model::SwitchProfile>> {
        let current = self
            .cursor
            .state
            .current_account()
            .unwrap_or(None)
            .and_then(|s| s.email);
        self.book.list(current.as_deref())
    }

    /// 把「Cursor 里当前登录的这个号」收进切号本。
    ///
    /// 这是最自然的入口：你现在登着的号先存起来，以后随时切回来。机器码用**当前**这套
    /// ——它就是这个号一直在用的设备指纹，不新造。
    pub fn capture_current(&self, note: Option<&str>) -> Result<crate::model::SwitchProfile> {
        let auth = self.cursor.state.read_auth()?;
        if auth.email().is_none() {
            return Err(
                AppError::new(ErrorCode::ProfileIncomplete, "Cursor 当前没有登录账号。")
                    .with_hint("先在 Cursor 里登录一个号，再回来收录。"),
            );
        }
        // 第一次动手之前，把这台真机的原始机器码存下来。
        self.ensure_original_saved()?;
        let ids = self.cursor.machine.read();
        let ids = (!ids.is_empty()).then_some(ids);

        let profile = self.book.upsert(&auth, note, ids)?;
        activity::info(
            &self.db,
            "switcher",
            Some(&profile.email),
            "已收录当前登录的账号",
        );
        Ok(profile)
    }

    /// 手动存一份当前登录态的备份。
    pub fn backup_now(&self) -> Result<Option<AuthBackup>> {
        let auth = self.cursor.state.read_auth()?;
        self.backups
            .create(&auth, BackupReason::Manual, self.keep())
    }

    /// 切到某一档。
    ///
    /// 失败时**停在原地**：已经完成的步骤不回滚（回滚本身也可能失败，反而更乱），
    /// 而是把「卡在哪一步」和「切换前那份备份」一起报出去，让用户决定是重试还是还原。
    pub fn switch_to(
        &self,
        id: &ProfileId,
        options: SwitchOptions,
        progress: ProgressSink<'_>,
    ) -> Result<SwitchOutcome> {
        let _guard = self.begin()?;
        let profile = self.book.get(id, None)?;
        let target = self.book.auth(id)?;

        // ── 前置检查。全部在动手之前做完，不要走到一半才发现切不了。 ──
        self.require_durable_secrets()?;
        if !target.is_switchable() {
            return Err(AppError::new(
                ErrorCode::ProfileIncomplete,
                format!(
                    "{} 缺 accessToken / refreshToken，切不进去。",
                    profile.email
                ),
            )
            .with_hint("到「我的账号」里给它重新授权，再加入切号本。"));
        }
        let check = self.cursor.check();
        if !check.writable() {
            return Err(AppError::new(
                ErrorCode::CursorSchemaDrift,
                check
                    .explain()
                    .unwrap_or_else(|| "Cursor 状态库不可写。".into()),
            )
            .with_hint("切号已降级为只读，避免写坏你的登录态。"));
        }

        progress(SwitchProgress::Started {
            email: profile.email.clone(),
        });
        let mut backup_id: Option<BackupId> = None;
        // 「登录态改了没有」决定失败时该跟用户怎么说。
        let mut wrote_auth = false;

        let running = self.control.is_running().unwrap_or(false);
        let hot = options.use_hot(running);

        let outcome = if hot {
            self.switch_hot(
                &profile.email,
                id,
                &target,
                progress,
                &mut backup_id,
                &mut wrote_auth,
            )
        } else {
            self.switch_cold(
                &profile.email,
                id,
                &target,
                &profile.machine_ids,
                options,
                progress,
                &mut backup_id,
                &mut wrote_auth,
            )
        };

        match outcome {
            Ok(done) => {
                activity::info(
                    &self.db,
                    "switcher",
                    Some(&profile.email),
                    if done.hot {
                        "已热切换到该账号"
                    } else {
                        "已切换到该账号"
                    },
                );
                Ok(done)
            }
            Err(err) => {
                let failed_at = step_of(&err);
                progress(SwitchProgress::Failed {
                    failed_at,
                    message: err.message.clone(),
                    backup_id: backup_id.clone(),
                });
                activity::error(
                    &self.db,
                    "switcher",
                    Some(&profile.email),
                    format!("切换失败（{failed_at}）：{}", err.message),
                );
                // 措辞必须看失败发生在写库之前还是之后。写完之后再失败，Cursor 里已经
                // 是新号了，这时说「没有被改坏」是在误导用户。
                Err(match (wrote_auth, &backup_id) {
                    (false, Some(_)) => {
                        err.with_hint("登录态没有被改动；需要的话可以从刚才那份备份还原回切换前。")
                    }
                    (false, None) => err,
                    (true, _) => err.with_hint(
                        "登录态**已经切成新号了**，只是后面的步骤没做完。重新切一次即可；\
                         也可以从刚才那份备份还原回切换前。",
                    ),
                })
            }
        }
    }

    /// 热切：不退出 Cursor，把 token 交给它自己的 deep-link 登录路由。
    fn switch_hot(
        &self,
        email: &str,
        id: &ProfileId,
        target: &AuthBundle,
        progress: ProgressSink<'_>,
        backup_id: &mut Option<BackupId>,
        wrote_auth: &mut bool,
    ) -> Result<SwitchOutcome> {
        let access = target
            .access_token()
            .ok_or_else(|| AppError::new(ErrorCode::ProfileIncomplete, "缺 accessToken。"))?;
        let refresh = target
            .refresh_token()
            .ok_or_else(|| AppError::new(ErrorCode::ProfileIncomplete, "缺 refreshToken。"))?;

        // 1) 备份当前登录态（只读，Cursor 在跑也安全）。
        let current = self.cursor.state.read_auth()?;
        match self
            .backups
            .create(&current, BackupReason::PreSwitch, self.keep())?
        {
            Some(b) => {
                *backup_id = Some(b.id.clone());
                progress(SwitchProgress::BackedUp {
                    backup_id: b.id,
                    email: b.email,
                });
            }
            None => progress(SwitchProgress::BackupSkipped),
        }

        // 2) deep link。Cursor 自己 storeAccessRefreshToken —— 写盘 + 刷内存。
        self.control
            .inject_login(access, refresh)
            .map_err(at("hot-login"))?;
        progress(SwitchProgress::HotLoginSent);

        // 3) 等磁盘上的 accessToken 变成目标值。Cursor 的 storageService.store 是同步落盘的。
        self.wait_for_access_token(access)
            .map_err(at("hot-confirm"))?;
        *wrote_auth = true;
        progress(SwitchProgress::HotLoginConfirmed);

        // 4) 补写展示键。深链只换 token 和订阅档，`cachedEmail` / `cachedScopedProfile` 留着上一个
        //    号的值，而 Cursor 读这些是「缓存里有就不再问服务端」——不补，菜单里的名字就一直是旧号。
        //    这一步失败不算切换失败：token 已经是新号了，只是名字可能要等下次登出登录才对。
        match self.cursor.state.write_display_keys(target) {
            Ok((written, removed)) => {
                progress(SwitchProgress::HotProfileWritten { written, removed });
            }
            Err(err) => {
                tracing::warn!(%err, "热切已完成，但补写账号展示缓存失败；Cursor 里的名字可能仍显示上一个号");
            }
        }

        if let Err(err) = self.book.mark_switched(id) {
            tracing::warn!(%err, "热切已完成，但没记下切换时间");
        }

        progress(SwitchProgress::Done {
            email: email.to_string(),
        });
        Ok(SwitchOutcome {
            email: email.to_string(),
            backup_id: backup_id.clone(),
            machine_switched: false,
            cursor_relaunched: false,
            hot: true,
        })
    }

    /// 冷切：退出 → 写库 → 可选换机器码 → 启动。机器码要换、或 Cursor 没在跑时走这里。
    #[allow(clippy::too_many_arguments)]
    fn switch_cold(
        &self,
        email: &str,
        id: &ProfileId,
        target: &AuthBundle,
        machine_ids: &MachineProfile,
        options: SwitchOptions,
        progress: ProgressSink<'_>,
        backup_id: &mut Option<BackupId>,
        wrote_auth: &mut bool,
    ) -> Result<SwitchOutcome> {
        // 0) 原始机器码只在第一次动手前存，之后永不覆盖。
        if options.switch_machine_ids {
            self.ensure_original_saved()?;
        }

        // 1) 备份当前登录态。
        let current = self.cursor.state.read_auth()?;
        match self
            .backups
            .create(&current, BackupReason::PreSwitch, self.keep())?
        {
            Some(b) => {
                *backup_id = Some(b.id.clone());
                progress(SwitchProgress::BackedUp {
                    backup_id: b.id,
                    email: b.email,
                });
            }
            None => progress(SwitchProgress::BackupSkipped),
        }

        // 2) 退出 Cursor，等进程真的消失。
        let quit = self.control.quit(QUIT_TIMEOUT).map_err(at("quit"))?;
        progress(SwitchProgress::CursorQuit {
            was_running: quit.was_running,
            forced: quit.forced,
        });

        // 3) 写登录态。清旧 + 写新在同一事务里。
        let written = self
            .cursor
            .state
            .write_auth(target, true)
            .map_err(at("write-auth"))?;
        *wrote_auth = true;
        progress(SwitchProgress::AuthWritten { keys: written });

        // 4) 机器码。
        let mut machine_switched = false;
        if options.switch_machine_ids && !machine_ids.is_empty() {
            self.cursor
                .machine
                .write(machine_ids)
                .map_err(at("write-machine"))?;
            machine_switched = true;
            progress(SwitchProgress::MachineSwitched {
                machine_id_short: machine_ids.short(),
            });
        }

        if let Err(err) = self.book.mark_switched(id) {
            tracing::warn!(%err, "切换已完成，但没记下切换时间");
        }

        // 5) 启动。起不来不算切号失败 —— 登录态已经是对的了，手动打开即可。
        let mut relaunched = false;
        if options.relaunch {
            match self.control.launch() {
                Ok(()) => {
                    relaunched = true;
                    progress(SwitchProgress::CursorLaunched);
                }
                Err(err) => {
                    tracing::warn!(%err, "切换完成但 Cursor 没起来");
                    activity::warn(
                        &self.db,
                        "switcher",
                        Some(email),
                        format!("登录态已切换，但启动 Cursor 失败：{err}"),
                    );
                }
            }
        }

        progress(SwitchProgress::Done {
            email: email.to_string(),
        });
        Ok(SwitchOutcome {
            email: email.to_string(),
            backup_id: backup_id.clone(),
            machine_switched,
            cursor_relaunched: relaunched,
            hot: false,
        })
    }

    /// 轮询 state.vscdb，直到 accessToken 变成期望值（或超时）。
    fn wait_for_access_token(&self, expected: &str) -> Result<()> {
        let deadline = Instant::now() + HOT_CONFIRM_TIMEOUT;
        loop {
            match self.cursor.state.read_auth() {
                Ok(auth) if auth.access_token() == Some(expected) => return Ok(()),
                Ok(_) | Err(_) => {}
            }
            if Instant::now() >= deadline {
                return Err(AppError::new(
                    ErrorCode::CursorControl,
                    "热登录已发出，但 Cursor 没有在限时内吃进新登录态。",
                )
                .with_hint(
                    "确认 Cursor 在前台且 `cursor://` 协议可用；也可以打开设置里的\
                     「切换时同时切机器码」改走冷切（会重启 Cursor）。",
                ));
            }
            std::thread::sleep(HOT_CONFIRM_POLL);
        }
    }

    /// 从某份备份还原登录态（不动机器码）。
    ///
    /// 还原之前先把「还原前」也存一份：撤销操作本身也得能撤销。
    pub fn restore_backup(
        &self,
        id: &BackupId,
        relaunch: bool,
        progress: ProgressSink<'_>,
    ) -> Result<SwitchOutcome> {
        let _guard = self.begin()?;
        self.require_durable_secrets()?;
        let bundle = self.backups.read(id)?;
        let email = bundle.email().unwrap_or_else(|| "未知账号".to_string());
        progress(SwitchProgress::Started {
            email: email.clone(),
        });

        // 读不到当前登录态就**停下**，不要 `unwrap_or_default()` 蒙混过去：那会把一次
        // 真实的读失败伪装成「本来就没登录」，于是跳过安全备份，然后照样覆盖掉它。
        // 这里和 `switch_to` 必须对称。
        let before = self.cursor.state.read_auth()?;
        let safety = self
            .backups
            .create(&before, BackupReason::PreRestore, self.keep())?;
        match &safety {
            Some(b) => progress(SwitchProgress::BackedUp {
                backup_id: b.id.clone(),
                email: b.email.clone(),
            }),
            None => progress(SwitchProgress::BackupSkipped),
        }

        let quit = self.control.quit(QUIT_TIMEOUT).map_err(at("quit"))?;
        progress(SwitchProgress::CursorQuit {
            was_running: quit.was_running,
            forced: quit.forced,
        });

        let written = self
            .cursor
            .state
            .write_auth(&bundle, true)
            .map_err(at("write-auth"))?;
        progress(SwitchProgress::AuthWritten { keys: written });

        let mut relaunched = false;
        if relaunch && self.control.launch().is_ok() {
            relaunched = true;
            progress(SwitchProgress::CursorLaunched);
        }
        progress(SwitchProgress::Done {
            email: email.clone(),
        });
        activity::info(&self.db, "switcher", Some(&email), "已从备份还原登录态");

        Ok(SwitchOutcome {
            email,
            backup_id: safety.map(|b| b.id),
            machine_switched: false,
            cursor_relaunched: relaunched,
            hot: false,
        })
    }

    /// 把机器码还原成这台真机的原始值。
    pub fn restore_original_machine(&self, relaunch: bool) -> Result<MachineProfile> {
        let _guard = self.begin()?;
        let original = self.original_machine()?.ok_or_else(|| {
            AppError::new(
                ErrorCode::BackupNotFound,
                "没有保存过这台机器的原始机器码。",
            )
            .with_hint("原始值在第一次切号或收录时才会记下来。")
        })?;
        self.control.quit(QUIT_TIMEOUT).map_err(at("quit"))?;
        self.cursor.machine.write(&original)?;
        if relaunch {
            let _ = self.control.launch();
        }
        activity::info(&self.db, "switcher", None, "已还原本机原始机器码");
        Ok(original)
    }

    /// 第一次动机器码之前，把这台真机的原始值存下来。**已存过就不覆盖。**
    pub fn ensure_original_saved(&self) -> Result<Option<MachineProfile>> {
        if let Some(existing) = self.original_machine()? {
            return Ok(Some(existing));
        }
        let current = self.cursor.machine.read();
        if current.is_empty() {
            // 读不到就别存一份空的占住 singleton 位 —— 那会让真正的原始值永远存不进来。
            return Ok(None);
        }
        let json = serde_json::to_string(&current)?;
        self.db.with(|c| {
            c.execute(
                "INSERT OR IGNORE INTO machine_original (singleton, ids_json, saved_at)
                 VALUES (1, ?1, ?2)",
                rusqlite::params![json, now_iso()],
            )
        })?;
        activity::info(&self.db, "switcher", None, "已保存本机原始机器码");
        Ok(Some(current))
    }

    pub fn original_machine(&self) -> Result<Option<MachineProfile>> {
        let raw: Option<String> = self.db.with(|c| {
            c.query_row(
                "SELECT ids_json FROM machine_original WHERE singleton = 1",
                [],
                |r| r.get(0),
            )
            .map(Some)
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
        })?;
        Ok(raw.and_then(|json| serde_json::from_str(&json).ok()))
    }

    /// 直接把一套登录态收进切号本（不经过 Cursor）。
    /// 「我的账号 → 加入切号本」走这条 —— 那次跨模块的显式拷贝（R1）。
    pub fn adopt(
        &self,
        auth: &AuthBundle,
        note: Option<&str>,
    ) -> Result<crate::model::SwitchProfile> {
        let profile = self.book.upsert(auth, note, None)?;
        activity::info(&self.db, "switcher", Some(&profile.email), "已加入切号本");
        Ok(profile)
    }

    fn keep(&self) -> u32 {
        settings::get_or(&self.db, settings::BACKUP_KEEP, DEFAULT_KEEP)
    }
}

/// 给错误打上「卡在哪一步」的标记。用 hint 的前缀承载，避免为此扩 `AppError` 的字段。
fn at(step: &'static str) -> impl Fn(AppError) -> AppError {
    move |err| {
        let mut err = err;
        err.hint = Some(match err.hint {
            Some(h) => format!("[{step}] {h}"),
            None => format!("[{step}]"),
        });
        err
    }
}

fn step_of(err: &AppError) -> &'static str {
    let hint = err.hint.as_deref().unwrap_or_default();
    for step in [
        "hot-login",
        "hot-confirm",
        "quit",
        "write-auth",
        "write-machine",
    ] {
        if hint.starts_with(&format!("[{step}]")) {
            return step;
        }
    }
    "prepare"
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_cursor::app::testing::FakeCursor;
    use nexus_cursor::{CursorPaths, StateDb};
    use nexus_store::MemorySecrets;
    use std::path::PathBuf;

    /// 一整套临时环境：假的 Cursor 目录（真 SQLite 库 + 真 storage.json）+ 内存秘密存储
    /// + 假的进程控制。除了「关不关得掉真的 Cursor」，其余全是真实实现。
    struct Harness {
        _dir: tempfile::TempDir,
        switcher: Switcher,
        cursor_dir: PathBuf,
        fake: Arc<FakeCursor>,
    }

    impl Harness {
        fn new() -> Self {
            Self::with_fake(Arc::new(FakeCursor::running()))
        }

        fn with_fake(fake: Arc<FakeCursor>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let cursor_dir = dir.path().join("Cursor");
            let paths = CursorPaths::from_user_dir(&cursor_dir);
            std::fs::create_dir_all(paths.state_db.parent().unwrap()).unwrap();

            // 和真实 Cursor 同构的库（schema 取自本机 3.18.9 实测）。
            let conn = rusqlite::Connection::open(&paths.state_db).unwrap();
            conn.execute_batch(
                "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);",
            )
            .unwrap();
            drop(conn);

            std::fs::write(
                &paths.storage_json,
                r#"{"telemetry.machineId":"real-machine","telemetry.macMachineId":"real-mac",
                    "telemetry.devDeviceId":"real-dev","telemetry.sqmId":"","theme":"dark"}"#,
            )
            .unwrap();
            std::fs::write(&paths.machine_id_file, "real-file").unwrap();

            // 热登录：假 Cursor 「收到 deep link」后自己把 token 写进库，模拟真机行为。
            let state_path = paths.state_db.clone();
            fake.on_inject(Arc::new(move |access, refresh| {
                let db = StateDb::new(state_path.clone());
                let mut bundle = db.read_auth().unwrap_or_default();
                bundle.insert("cursorAuth/accessToken", access);
                bundle.insert("cursorAuth/refreshToken", refresh);
                let _ = db.write_auth(&bundle, false);
            }));

            let switcher = Switcher::with_control(
                Arc::new(Db::open_in_memory().unwrap()),
                // 谎报可持久化：不然每个用例都会撞上「秘密存不住就不许切号」那道闸，
                // 而那道闸自己有专门的用例。
                Arc::new(MemorySecrets::durable_for_tests()),
                Cursor::from_paths(paths),
                fake.clone(),
            );
            Self {
                _dir: dir,
                switcher,
                cursor_dir,
                fake,
            }
        }

        fn state(&self) -> StateDb {
            StateDb::new(self.cursor_dir.join("User/globalStorage/state.vscdb"))
        }

        /// 让 Cursor 处于「登录着 email」的状态。
        fn login_as(&self, email: &str) {
            self.state().write_auth(&auth(email), true).unwrap();
        }
    }

    fn auth(email: &str) -> AuthBundle {
        let mut b = AuthBundle::new();
        b.insert("cursorAuth/accessToken", format!("at-{email}"));
        b.insert("cursorAuth/refreshToken", format!("rt-{email}"));
        b.insert("cursorAuth/cachedEmail", email);
        b.insert("cursorAuth/stripeMembershipType", "ultra");
        b
    }

    fn names(events: &[SwitchProgress]) -> Vec<String> {
        events
            .iter()
            .map(|e| {
                serde_json::to_value(e).unwrap()["step"]
                    .as_str()
                    .unwrap()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn capture_current_records_the_live_login_with_its_existing_machine_ids() {
        let h = Harness::new();
        h.login_as("a@example.com");

        let profile = h.switcher.capture_current(Some("自用")).unwrap();
        assert_eq!(profile.email, "a@example.com");
        assert_eq!(profile.membership.as_deref(), Some("ultra"));
        // 用当前这套，不新造：它就是这个号一直在用的指纹。
        assert_eq!(profile.machine_ids.machine_id, "real-machine");
        assert_eq!(profile.machine_ids.machine_id_file, "real-file");
        // 顺带把真机原始值存住了。
        assert_eq!(
            h.switcher.original_machine().unwrap().unwrap().machine_id,
            "real-machine"
        );
    }

    #[test]
    fn capture_current_refuses_when_cursor_is_logged_out() {
        let h = Harness::new();
        let err = h.switcher.capture_current(None).unwrap_err();
        assert_eq!(err.code, ErrorCode::ProfileIncomplete);
        assert!(err.hint.unwrap().contains("先在 Cursor 里登录"));
    }

    #[test]
    fn cold_switch_runs_every_step_in_the_mandated_order() {
        let h = Harness::new();
        h.login_as("a@example.com");
        let from = h.switcher.capture_current(None).unwrap();
        h.state().write_auth(&auth("b@example.com"), true).unwrap();
        let to = h.switcher.capture_current(None).unwrap();
        h.state()
            .write_auth(&h.switcher.book.auth(&from.id).unwrap(), true)
            .unwrap();

        let events = std::cell::RefCell::new(Vec::new());
        let out = h
            .switcher
            .switch_to(&to.id, SwitchOptions::cold_with_machine_ids(), &|e| {
                events.borrow_mut().push(e)
            })
            .unwrap();

        assert_eq!(
            names(&events.borrow()),
            vec![
                "started",
                "backedUp",
                "cursorQuit",
                "authWritten",
                "machineSwitched",
                "cursorLaunched",
                "done",
            ],
            "冷切顺序是硬约束：备份 → 退出 → 写 auth → 写机器码 → 启动"
        );
        assert_eq!(out.email, "b@example.com");
        assert!(out.backup_id.is_some());
        assert!(out.machine_switched);
        assert!(!out.hot);
    }

    #[test]
    fn hot_switch_does_not_quit_or_relaunch_cursor() {
        let h = Harness::new();
        h.login_as("a@example.com");
        h.switcher.capture_current(None).unwrap();
        h.state().write_auth(&auth("b@example.com"), true).unwrap();
        let to = h.switcher.capture_current(None).unwrap();
        h.login_as("a@example.com");

        let events = std::cell::RefCell::new(Vec::new());
        let out = h
            .switcher
            .switch_to(&to.id, SwitchOptions::default(), &|e| {
                events.borrow_mut().push(e)
            })
            .unwrap();

        assert_eq!(
            names(&events.borrow()),
            vec![
                "started",
                "backedUp",
                "hotLoginSent",
                "hotLoginConfirmed",
                "hotProfileWritten",
                "done",
            ],
            "热切：备份 → deep link → 确认 → 补写展示键，不退出"
        );
        assert!(out.hot);
        assert!(!out.machine_switched);
        assert!(!out.cursor_relaunched);
        assert_eq!(h.fake.quit_count(), 0);
        assert_eq!(h.fake.launch_count(), 0);
        assert_eq!(h.fake.inject_count(), 1);
        let after = h.state().read_auth().unwrap();
        assert_eq!(after.access_token(), Some("at-b@example.com"));
        // 假 Cursor 只像真机一样写了 token；邮箱 / 档位是我们补写的——这正是「名字还是旧号」那个 bug。
        assert_eq!(after.email().as_deref(), Some("b@example.com"));
        assert_eq!(after.get("cursorAuth/stripeMembershipType"), Some("ultra"));
        // 一机一码：热切不碰机器码。
        let ids = nexus_cursor::MachineIds::new(
            h.cursor_dir.join("User/globalStorage/storage.json"),
            h.cursor_dir.join("machineid"),
        )
        .read();
        assert_eq!(ids.machine_id, "real-machine");
    }

    /// 复现线上现象：深链换了 token，但 Cursor 菜单里的名字还是上一个号。
    /// 原因是 `cachedEmail` / `cachedScopedProfile` 深链不碰、Cursor 读时又只认缓存。
    /// 热切收尾必须把旧号的这几把键换成目标号的（目标号没有的删掉，让 Cursor 重新拉）。
    #[test]
    fn hot_switch_replaces_stale_display_cache_of_the_previous_account() {
        let h = Harness::new();
        // 目标号 b 先收录：它只有邮箱 + 档位，没有 scoped profile。
        h.state().write_auth(&auth("b@example.com"), true).unwrap();
        let b = h.switcher.capture_current(None).unwrap();

        // 当前登着 a，且 Cursor 已经把 a 的显示名缓存下来了。
        let mut a = auth("a@example.com");
        a.insert("cursorAuth/cachedSignUpType", "Google");
        a.insert(
            "cursorAuth/cachedScopedProfile",
            r#"{"displayName":"Alice A"}"#,
        );
        a.insert("cursorAuth/stripeMembershipType", "free");
        h.state().write_auth(&a, true).unwrap();

        h.switcher
            .switch_to(&b.id, SwitchOptions::default(), &ignore_progress)
            .unwrap();

        let after = h.state().read_auth().unwrap();
        assert_eq!(after.access_token(), Some("at-b@example.com"));
        assert_eq!(
            after.email().as_deref(),
            Some("b@example.com"),
            "邮箱要换成新号"
        );
        assert_eq!(
            after.get("cursorAuth/stripeMembershipType"),
            Some("ultra"),
            "档位要换成新号"
        );
        assert_eq!(
            after.get("cursorAuth/cachedScopedProfile"),
            None,
            "旧号的显示名不能留着；删掉后 Cursor 会自己重新拉"
        );
        assert_eq!(after.get("cursorAuth/cachedSignUpType"), None);
    }

    #[test]
    fn cold_switch_actually_replaces_the_login_and_the_machine_ids() {
        let h = Harness::new();
        h.login_as("a@example.com");
        h.switcher.capture_current(None).unwrap();
        h.state().write_auth(&auth("b@example.com"), true).unwrap();
        let b = h.switcher.capture_current(None).unwrap();
        h.login_as("a@example.com");

        h.switcher
            .switch_to(
                &b.id,
                SwitchOptions::cold_with_machine_ids(),
                &ignore_progress,
            )
            .unwrap();

        let now = h.state().read_auth().unwrap();
        assert_eq!(now.email().unwrap(), "b@example.com");
        assert_eq!(
            now.get("cursorAuth/refreshToken").unwrap(),
            "rt-b@example.com"
        );
        let ids = nexus_cursor::MachineIds::new(
            h.cursor_dir.join("User/globalStorage/storage.json"),
            h.cursor_dir.join("machineid"),
        )
        .read();
        assert_eq!(ids.machine_id, b.machine_ids.machine_id);
    }

    #[test]
    fn cold_switching_never_leaves_a_half_written_login() {
        // 目标号只有必需的三个键；切完之后旧号的订阅档不能残留。
        let h = Harness::new();
        h.login_as("a@example.com");
        h.switcher.capture_current(None).unwrap();

        let mut lean = AuthBundle::new();
        lean.insert("cursorAuth/accessToken", "at-lean");
        lean.insert("cursorAuth/refreshToken", "rt-lean");
        lean.insert("cursorAuth/cachedEmail", "lean@example.com");
        let target = h.switcher.adopt(&lean, None).unwrap();
        h.login_as("a@example.com");

        h.switcher
            .switch_to(
                &target.id,
                SwitchOptions {
                    relaunch: true,
                    switch_machine_ids: false,
                    prefer_hot: false,
                },
                &ignore_progress,
            )
            .unwrap();

        let now = h.state().read_auth().unwrap();
        assert_eq!(now.email().unwrap(), "lean@example.com");
        assert!(
            now.get("cursorAuth/stripeMembershipType").is_none(),
            "旧号的订阅档必须被清掉，否则界面显示的是上一个号的档位"
        );
    }

    #[test]
    fn default_hot_switch_leaves_the_real_machine_ids_alone() {
        let h = Harness::new();
        h.login_as("a@example.com");
        h.switcher.capture_current(None).unwrap();
        h.state().write_auth(&auth("b@example.com"), true).unwrap();
        let b = h.switcher.capture_current(None).unwrap();
        h.login_as("a@example.com");

        let events = std::cell::RefCell::new(Vec::new());
        let out = h
            .switcher
            .switch_to(&b.id, SwitchOptions::default(), &|e| {
                events.borrow_mut().push(e)
            })
            .unwrap();

        assert!(out.hot);
        assert!(!out.machine_switched);
        assert!(!out.cursor_relaunched);
        let steps = names(&events.borrow());
        assert!(!steps.contains(&"machineSwitched".to_string()));
        assert!(!steps.contains(&"cursorQuit".to_string()));
        let ids = nexus_cursor::MachineIds::new(
            h.cursor_dir.join("User/globalStorage/storage.json"),
            h.cursor_dir.join("machineid"),
        )
        .read();
        assert_eq!(ids.machine_id, "real-machine");
    }

    #[test]
    fn a_profile_missing_its_tokens_is_refused_before_anything_is_touched() {
        let h = Harness::new();
        h.login_as("a@example.com");
        let mut broken = AuthBundle::new();
        broken.insert("cursorAuth/cachedEmail", "broken@example.com");
        let p = h.switcher.adopt(&broken, None).unwrap();

        let err = h
            .switcher
            .switch_to(&p.id, SwitchOptions::default(), &ignore_progress)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProfileIncomplete);
        // 当前登录一点没动。
        assert_eq!(
            h.state().read_auth().unwrap().email().unwrap(),
            "a@example.com"
        );
        assert!(
            h.switcher.backups.list().unwrap().is_empty(),
            "还没到备份那一步"
        );
    }

    #[test]
    fn switching_is_refused_when_backups_cannot_survive_the_process() {
        // 秘密只在内存里 → 切号会永久弄丢当前登录的号。
        // 这时宁可不让切（也不该让用户以为「切换前会自动备份」这句话还成立）。
        let dir = tempfile::tempdir().unwrap();
        let cursor_dir = dir.path().join("Cursor");
        let paths = CursorPaths::from_user_dir(&cursor_dir);
        std::fs::create_dir_all(paths.state_db.parent().unwrap()).unwrap();
        rusqlite::Connection::open(&paths.state_db)
            .unwrap()
            .execute_batch(
                "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);",
            )
            .unwrap();

        let volatile = Arc::new(MemorySecrets::new());
        assert!(!volatile.durable());
        let switcher = Switcher::with_control(
            Arc::new(Db::open_in_memory().unwrap()),
            volatile,
            Cursor::from_paths(paths.clone()),
            Arc::new(FakeCursor::running()),
        );

        let state = StateDb::new(&paths.state_db);
        state.write_auth(&auth("a@example.com"), true).unwrap();
        let p = switcher.capture_current(None).unwrap();

        let err = switcher
            .switch_to(&p.id, SwitchOptions::default(), &ignore_progress)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(err.hint.unwrap().contains("永久丢失"));
        // 一个字节都没动。
        assert_eq!(state.read_auth().unwrap().email().unwrap(), "a@example.com");
    }

    #[test]
    fn a_failure_after_the_write_does_not_claim_nothing_changed() {
        // 写完 auth 之后才失败：Cursor 里已经是新号了。这时说「登录态没有被改动」
        // 会让用户以为切换没生效，然后再切一遍。
        let h = Harness::new();
        h.login_as("a@example.com");
        h.switcher.capture_current(None).unwrap();
        h.state().write_auth(&auth("b@example.com"), true).unwrap();
        let b = h.switcher.capture_current(None).unwrap();
        h.login_as("a@example.com");

        // 让写机器码失败：把 storage.json 弄成读得到但读不懂。
        std::fs::write(&h.switcher.cursor.paths.storage_json, "{坏掉的 JSON").unwrap();

        let err = h
            .switcher
            .switch_to(
                &b.id,
                SwitchOptions::cold_with_machine_ids(),
                &ignore_progress,
            )
            .unwrap_err();

        assert_eq!(
            h.state().read_auth().unwrap().email().unwrap(),
            "b@example.com",
            "auth 事务已经提交了"
        );
        let hint = err.hint.unwrap();
        assert!(hint.contains("已经切成新号"), "措辞要如实：{hint}");
        assert!(!hint.contains("没有被改动"));
    }

    #[test]
    fn a_second_switch_is_refused_while_one_is_running() {
        // 两个切号同时跑会把 A 的 token 和 B 的机器码配到一起。
        let h = Harness::new();
        h.login_as("a@example.com");
        let a = h.switcher.capture_current(None).unwrap();

        // 在进度回调里再切一次 —— 等价于用户在第一次还没跑完时又点了一下。
        let reentrant = std::cell::Cell::new(None);
        h.switcher
            .switch_to(&a.id, SwitchOptions::default(), &|p| {
                if matches!(p, SwitchProgress::Started { .. }) {
                    reentrant.set(Some(
                        h.switcher
                            .switch_to(&a.id, SwitchOptions::default(), &ignore_progress)
                            .unwrap_err()
                            .code,
                    ));
                }
            })
            .unwrap();

        assert_eq!(reentrant.get(), Some(ErrorCode::Busy));
        // 闸放开了，之后还能正常切。
        assert!(h
            .switcher
            .switch_to(&a.id, SwitchOptions::default(), &ignore_progress)
            .is_ok());
    }

    #[test]
    fn a_drifted_state_db_degrades_to_read_only_instead_of_writing() {
        let h = Harness::new();
        h.login_as("a@example.com");
        let p = h.switcher.capture_current(None).unwrap();
        // 模拟 Cursor 改了结构。
        rusqlite::Connection::open(h.state().path())
            .unwrap()
            .execute_batch("DROP TABLE ItemTable; CREATE TABLE Other (k TEXT);")
            .unwrap();

        let err = h
            .switcher
            .switch_to(&p.id, SwitchOptions::default(), &ignore_progress)
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::CursorSchemaDrift);
    }

    #[test]
    fn a_failed_quit_stops_before_writing_and_points_at_the_backup() {
        let h = Harness::new();
        h.login_as("a@example.com");
        h.switcher.capture_current(None).unwrap();
        h.state().write_auth(&auth("b@example.com"), true).unwrap();
        let b = h.switcher.capture_current(None).unwrap();
        h.login_as("a@example.com");

        // Cursor 关不掉 —— 这是冷切路上最常见的真实失败。
        h.fake.fail_quit();

        let events = std::cell::RefCell::new(Vec::new());
        let err = h
            .switcher
            .switch_to(
                &b.id,
                SwitchOptions {
                    relaunch: true,
                    switch_machine_ids: false,
                    prefer_hot: false,
                },
                &|e| events.borrow_mut().push(e),
            )
            .unwrap_err();

        assert_eq!(err.code, ErrorCode::CursorControl);
        // 停在原地：登录态一个字节都没改。
        assert_eq!(
            h.state().read_auth().unwrap().email().unwrap(),
            "a@example.com",
            "退出失败后绝不能已经写了新号"
        );

        let steps = names(&events.borrow());
        assert_eq!(steps, vec!["started", "backedUp", "failed"]);
        // 界面要知道卡在哪一步、能还原到哪里。
        let last = events.borrow().last().cloned().unwrap();
        match last {
            SwitchProgress::Failed {
                failed_at,
                backup_id,
                ..
            } => {
                assert_eq!(failed_at, "quit");
                assert!(backup_id.is_some(), "失败事件要带上可还原的备份");
            }
            other => panic!("最后一个事件应当是 failed，实际是 {other:?}"),
        }
        assert!(err.hint.unwrap().contains("还原"));
    }

    #[test]
    fn a_failed_launch_still_counts_as_a_successful_switch() {
        // 登录态已经是对的了，手动打开 Cursor 即可 —— 不该报成切号失败让用户再切一次。
        let fake = Arc::new(FakeCursor::running());
        fake.fail_launch();
        let h = Harness::with_fake(fake);
        h.login_as("a@example.com");
        let a = h.switcher.capture_current(None).unwrap();

        let out = h
            .switcher
            .switch_to(&a.id, SwitchOptions::default(), &ignore_progress)
            .unwrap();
        assert!(!out.cursor_relaunched);
        assert_eq!(
            h.state().read_auth().unwrap().email().unwrap(),
            "a@example.com"
        );
    }

    #[test]
    fn cursor_is_quit_before_the_login_is_written() {
        let h = Harness::new();
        h.login_as("a@example.com");
        let a = h.switcher.capture_current(None).unwrap();
        assert!(h.fake.is_running().unwrap());

        h.switcher
            .switch_to(
                &a.id,
                SwitchOptions {
                    relaunch: false,
                    switch_machine_ids: true,
                    prefer_hot: false,
                },
                &ignore_progress,
            )
            .unwrap();

        assert_eq!(h.fake.quit_count(), 1);
        assert_eq!(h.fake.launch_count(), 0);
        assert!(!h.fake.is_running().unwrap());
    }

    #[test]
    fn restoring_a_backup_puts_the_previous_login_back() {
        let h = Harness::new();
        h.login_as("a@example.com");
        h.switcher.capture_current(None).unwrap();
        h.state().write_auth(&auth("b@example.com"), true).unwrap();
        let b = h.switcher.capture_current(None).unwrap();
        h.login_as("a@example.com");

        let switched = h
            .switcher
            .switch_to(
                &b.id,
                SwitchOptions {
                    relaunch: false,
                    switch_machine_ids: false,
                    prefer_hot: false,
                },
                &ignore_progress,
            )
            .unwrap();
        assert_eq!(
            h.state().read_auth().unwrap().email().unwrap(),
            "b@example.com"
        );

        h.switcher
            .restore_backup(&switched.backup_id.unwrap(), false, &ignore_progress)
            .unwrap();
        assert_eq!(
            h.state().read_auth().unwrap().email().unwrap(),
            "a@example.com",
            "备份还原后应当回到切换前那个号"
        );
    }

    #[test]
    fn restoring_also_snapshots_the_state_it_replaced() {
        let h = Harness::new();
        h.login_as("a@example.com");
        let manual = h.switcher.backup_now().unwrap().unwrap();
        h.state().write_auth(&auth("b@example.com"), true).unwrap();

        let out = h
            .switcher
            .restore_backup(&manual.id, false, &ignore_progress)
            .unwrap();
        // 还原前的 b 也被存了一份 —— 撤销要能撤销。
        let safety = out.backup_id.expect("还原前应当也存一份");
        assert_eq!(
            h.switcher.backups.read(&safety).unwrap().email().unwrap(),
            "b@example.com"
        );
    }

    #[test]
    fn the_real_machine_ids_are_saved_once_and_never_overwritten() {
        let h = Harness::new();
        h.login_as("a@example.com");
        h.switcher.ensure_original_saved().unwrap();

        // 之后机器码被切成别的。
        h.switcher
            .cursor
            .machine
            .write(&MachineProfile::generate())
            .unwrap();
        h.switcher.ensure_original_saved().unwrap();

        assert_eq!(
            h.switcher.original_machine().unwrap().unwrap().machine_id,
            "real-machine",
            "原始值一旦记下就不该被后来的机器码覆盖"
        );
    }

    #[test]
    fn restoring_the_original_machine_ids_works() {
        let h = Harness::new();
        h.login_as("a@example.com");
        h.switcher.ensure_original_saved().unwrap();
        h.switcher
            .cursor
            .machine
            .write(&MachineProfile::generate())
            .unwrap();

        let back = h.switcher.restore_original_machine(false).unwrap();
        assert_eq!(back.machine_id, "real-machine");
        assert_eq!(h.switcher.cursor.machine.read().machine_id, "real-machine");
    }

    #[test]
    fn restoring_the_original_machine_without_a_snapshot_is_an_error() {
        let h = Harness::new();
        let err = h.switcher.restore_original_machine(false).unwrap_err();
        assert_eq!(err.code, ErrorCode::BackupNotFound);
    }

    #[test]
    fn overview_reports_who_owns_the_current_machine_ids() {
        let h = Harness::new();
        h.login_as("a@example.com");
        h.switcher.capture_current(None).unwrap();

        let o = h.switcher.overview().unwrap();
        assert_eq!(o.current.unwrap().email.unwrap(), "a@example.com");
        assert_eq!(o.machine_id_owner.as_deref(), Some("a@example.com"));
        assert_eq!(o.machine_id_short, "real-mac"); // "real-machine" 的前 8 位
        assert!(o.has_original_machine);
        assert!(o.check.writable());
    }

    #[test]
    fn overview_works_on_a_machine_where_cursor_was_never_used() {
        let h = Harness::new();
        let o = h.switcher.overview().unwrap();
        assert!(o.current.is_none());
        assert!(o.machine_id_owner.is_none());
        // 从没登录过 ≠ 结构不对。干净机器上必须能切进去，否则「买号 → 切入」走不通。
        assert!(!o.check.logged_in());
        assert!(o.check.writable());
        assert!(o.check.explain().is_none());
    }

    #[test]
    fn a_profile_can_be_switched_into_on_a_never_logged_in_cursor() {
        // §2.4 在干净机器上的完整路径：买到的号 → 加入切号本 → 切进去。
        // Cursor 从没登录过时通常也没在跑 → 走冷切，整包 auth（含邮箱）写进库。
        let h = Harness::with_fake(Arc::new(FakeCursor::stopped()));
        let bought = h.switcher.adopt(&auth("bought@example.com"), None).unwrap();

        let out = h
            .switcher
            .switch_to(&bought.id, SwitchOptions::default(), &ignore_progress)
            .unwrap();

        assert!(!out.hot, "Cursor 没在跑时应走冷切");
        assert_eq!(
            h.state().read_auth().unwrap().email().unwrap(),
            "bought@example.com"
        );
    }

    #[test]
    fn adopting_an_account_from_the_other_module_is_a_plain_copy() {
        // R1：数据只在这一次显式拷贝里流动，两个模块不互相依赖。
        let h = Harness::new();
        let p = h
            .switcher
            .adopt(&auth("bought@example.com"), Some("外面买来的号"))
            .unwrap();
        assert_eq!(p.email, "bought@example.com");
        assert!(p.has_auth);
        assert!(
            !p.machine_ids.machine_id.is_empty(),
            "新档要分到一套专属机器码"
        );
    }

    #[test]
    fn switch_marks_the_profile_as_recently_used() {
        let h = Harness::new();
        h.login_as("a@example.com");
        let a = h.switcher.capture_current(None).unwrap();
        assert!(a.last_switched_at.is_none());
        h.switcher
            .switch_to(
                &a.id,
                SwitchOptions {
                    relaunch: false,
                    switch_machine_ids: true,
                    prefer_hot: false,
                },
                &ignore_progress,
            )
            .unwrap();
        assert!(h.switcher.list().unwrap()[0].last_switched_at.is_some());
    }

    #[test]
    fn activity_log_records_switches_without_leaking_tokens() {
        let h = Harness::new();
        h.login_as("a@example.com");
        let a = h.switcher.capture_current(None).unwrap();
        h.switcher
            .switch_to(
                &a.id,
                SwitchOptions {
                    relaunch: false,
                    switch_machine_ids: true,
                    prefer_hot: false,
                },
                &ignore_progress,
            )
            .unwrap();

        let log = activity::recent(&h.switcher.db, 20).unwrap();
        let text = serde_json::to_string(&log).unwrap();
        assert!(text.contains("已切换到该账号"));
        assert!(
            !text.contains("rt-a@example.com"),
            "日志里不能出现 token：{text}"
        );
    }
}
