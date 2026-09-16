//! 切号本与备份的数据形状。
//!
//! 这里的每个类型都是**可以给前端看的**：秘密都在 `SecretStore` 那边，结构里只有
//! ref 能推出来的布尔量和元信息。

use nexus_core::{BackupId, MachineProfile, ProfileId};
use serde::{Deserialize, Serialize};

/// 切号本里的一档。
///
/// 与 `accounts` 表**没有外键**，也没有任何字段指向它（R1）：这两个模块的数据只在
/// UI 层通过「加入切号本」显式拷贝一次。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SwitchProfile {
    pub id: ProfileId,
    pub email: String,
    /// 订阅档（`ultra` / `pro` / `free`…），从登录态里带过来，只用于显示。
    pub membership: Option<String>,
    pub signup_type: Option<String>,
    pub note: Option<String>,
    /// 这一档专属的机器码（收录时记下的那套）。R6 默认切号**不**换机器码；
    /// 仅当设置里打开「切换时同时切机器码」才会写回磁盘。
    pub machine_ids: MachineProfile,
    pub created_at: String,
    pub updated_at: String,
    pub last_switched_at: Option<String>,
    /// 这一档的登录态还在不在。没有 = 切不进去，界面要标出来。
    pub has_auth: bool,
    /// 是不是 Cursor 当前登录的这个号。
    pub is_current: bool,
}

/// 一份登录态备份的索引。值（整套 `cursorAuth/*`）在 `SecretStore` 里。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AuthBackup {
    pub id: BackupId,
    /// 备份时登录的是谁。从没登录过时为 `None`。
    pub email: Option<String>,
    pub created_at: String,
    pub reason: BackupReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackupReason {
    /// 切号前自动存的。
    PreSwitch,
    /// 从备份还原之前，把「还原前」的状态也存一份 —— 还原本身也要能撤销。
    PreRestore,
    /// 用户手动点的。
    Manual,
}

impl BackupReason {
    pub fn as_str(self) -> &'static str {
        match self {
            BackupReason::PreSwitch => "pre-switch",
            BackupReason::PreRestore => "pre-restore",
            BackupReason::Manual => "manual",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "pre-switch" => BackupReason::PreSwitch,
            "pre-restore" => BackupReason::PreRestore,
            _ => BackupReason::Manual,
        }
    }
}

/// 切号页顶部那一栏：现在登着谁、机器码是谁的。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Overview {
    /// Cursor 当前登录的账号。没登录为 `None`。
    pub current: Option<nexus_cursor::AuthSummary>,
    /// 当前机器码的前 8 位。
    pub machine_id_short: String,
    /// 当前这套机器码属于切号本里的哪一档。都不属于 = 还是这台真机的原始机器码。
    pub machine_id_owner: Option<String>,
    /// 这台真机的原始机器码存没存过。
    pub has_original_machine: bool,
    /// Cursor 此刻在不在跑。热切需要它在跑；界面据此决定确认文案。
    pub cursor_running: bool,
    /// 自检结果：不通过时切号降级为只读。
    pub check: nexus_cursor::SchemaCheck,
}

/// 切号过程中的一步。通过 Tauri 事件推给界面（§4.3：长任务用事件，不轮询）。
///
/// `rename_all` 只管变体名，变体**里面**的字段要靠 `rename_all_fields`——少了它
/// 前端收到的是 `backup_id` 而不是 `backupId`。下面的测试就是为了钉住这一点。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "step",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum SwitchProgress {
    Started {
        email: String,
    },
    BackedUp {
        backup_id: BackupId,
        email: Option<String>,
    },
    /// 备份被跳过：Cursor 里本来就没有登录态，没什么可备份的。
    BackupSkipped,
    /// 热切：deep link 已交给 Cursor。
    HotLoginSent,
    /// 热切：读盘确认 accessToken 已变成目标号。
    HotLoginConfirmed,
    /// 热切：把目标号的邮箱 / 档位 / 显示名缓存补写进库。深链那条路不碰这几把键，
    /// 不补的话 Cursor 菜单里还是上一个号的名字（`nexus_cursor::DISPLAY_KEYS`）。
    HotProfileWritten {
        written: usize,
        removed: usize,
    },
    CursorQuit {
        was_running: bool,
        forced: bool,
    },
    AuthWritten {
        keys: usize,
    },
    MachineSwitched {
        machine_id_short: String,
    },
    CursorLaunched,
    Done {
        email: String,
    },
    /// 失败。`failedAt` 是卡在哪一步，`backupId` 是可以还原回去的那份。
    Failed {
        failed_at: &'static str,
        message: String,
        backup_id: Option<BackupId>,
    },
}

/// 切号成功后的回执。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchOutcome {
    pub email: String,
    /// 切换前那一刻的备份，用户随时可以回到这里。
    pub backup_id: Option<BackupId>,
    pub machine_switched: bool,
    pub cursor_relaunched: bool,
    /// 是否走了热切（不退出 Cursor）。
    pub hot: bool,
}

/// 切号选项。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchOptions {
    /// 完成后是否启动 Cursor（仅冷切路径有意义）。
    pub relaunch: bool,
    /// 是否同时切机器码。**默认关**（R6 一机一码）；打开则强制冷切。
    pub switch_machine_ids: bool,
    /// 优先热切。Cursor 在跑且不切机器码时生效；否则自动退回冷切。
    pub prefer_hot: bool,
}

impl Default for SwitchOptions {
    fn default() -> Self {
        Self {
            relaunch: true,
            switch_machine_ids: false,
            prefer_hot: true,
        }
    }
}

impl SwitchOptions {
    /// 这次切换实际走热切还是冷切。
    pub fn use_hot(self, cursor_running: bool) -> bool {
        self.prefer_hot && cursor_running && !self.switch_machine_ids
    }

    /// 冷切且要换机器码时的选项（测试 / 高级开关）。
    pub fn cold_with_machine_ids() -> Self {
        Self {
            relaunch: true,
            switch_machine_ids: true,
            prefer_hot: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_serializes_as_a_tagged_union() {
        let v = serde_json::to_value(SwitchProgress::BackedUp {
            backup_id: BackupId::from_raw("b1"),
            email: Some("a@example.com".into()),
        })
        .unwrap();
        assert_eq!(v["step"], "backedUp");
        assert_eq!(v["backupId"], "b1");

        let v = serde_json::to_value(SwitchProgress::Failed {
            failed_at: "quit",
            message: "Cursor 无法结束".into(),
            backup_id: None,
        })
        .unwrap();
        assert_eq!(v["step"], "failed");
        assert_eq!(v["failedAt"], "quit");
    }

    #[test]
    fn backup_reason_round_trips_through_the_database_form() {
        for r in [
            BackupReason::PreSwitch,
            BackupReason::PreRestore,
            BackupReason::Manual,
        ] {
            assert_eq!(BackupReason::parse(r.as_str()), r);
        }
        assert_eq!(BackupReason::parse("从未见过"), BackupReason::Manual);
    }

    #[test]
    fn switching_machine_ids_is_off_by_default() {
        let o = SwitchOptions::default();
        assert!(!o.switch_machine_ids && o.relaunch && o.prefer_hot);
        assert!(o.use_hot(true));
        assert!(!o.use_hot(false));
        assert!(!SwitchOptions::cold_with_machine_ids().use_hot(true));
    }
}
