//! 读写机器码。
//!
//! 值本身的语义在 `nexus_core::MachineProfile`；这里只管落盘：
//!   - `User/globalStorage/storage.json` 里的四个 `telemetry.*`；
//!   - `<user_dir>/machineid` 这个单独文件。
//!
//! 写 storage.json 是**读-改-写整份 + 原子 rename**：那份 JSON 里还有十几个与我们
//! 无关的键（本机实测 17 个），全量覆盖会把 Cursor 的其它状态一起抹掉；不用 rename
//! 的话，写一半断电就留下一个坏 JSON，Cursor 下次起不来。

use nexus_core::{AppError, ErrorCode, MachineProfile, Result};
use std::path::{Path, PathBuf};

/// 一台机器上的机器码读写口。
#[derive(Debug, Clone)]
pub struct MachineIds {
    storage_json: PathBuf,
    machine_id_file: PathBuf,
}

impl MachineIds {
    pub fn new(storage_json: impl Into<PathBuf>, machine_id_file: impl Into<PathBuf>) -> Self {
        Self {
            storage_json: storage_json.into(),
            machine_id_file: machine_id_file.into(),
        }
    }

    /// 读当前这套。文件缺失或坏掉都不报错，返回空 profile —— 调用方用
    /// `is_empty()` 判断，比抛错好处理。
    pub fn read(&self) -> MachineProfile {
        let mut profile = std::fs::read_to_string(&self.storage_json)
            .ok()
            .and_then(|raw| serde_json::from_str::<MachineProfile>(&raw).ok())
            .unwrap_or_default();
        if let Ok(raw) = std::fs::read_to_string(&self.machine_id_file) {
            profile.machine_id_file = raw.trim().to_string();
        }
        profile
    }

    /// 写入一套机器码。**调用方必须先确保 Cursor 已退出**（它启动时读一次就缓存）。
    ///
    /// 只改我们认识的键，storage.json 里其它内容原样保留。
    pub fn write(&self, profile: &MachineProfile) -> Result<()> {
        if profile.is_empty() {
            return Err(AppError::invalid("这套机器码是空的，拒绝写入。"));
        }
        let mut doc: serde_json::Map<String, serde_json::Value> =
            match std::fs::read_to_string(&self.storage_json) {
                // **解析失败 ≠ 文件不存在。** 一份读得到但读不懂的 storage.json（写到一半
                // 断电、磁盘满、同步了半个文件）如果被当成空对象，下面的全量写回就会把
                // Cursor 其余十几个键连同窗口布局、主题一起抹掉。宁可这次不写。
                Ok(raw) => serde_json::from_str(&raw).map_err(|err| {
                    AppError::new(
                        ErrorCode::CursorSchemaDrift,
                        format!("Cursor 的 storage.json 解析不了：{err}"),
                    )
                    .with_hint("没有改动它。这个文件可能损坏了，可以让 Cursor 重建后再试。")
                })?,
                // 文件不在才从空对象起：Cursor 会把缺的键补齐。
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
                Err(err) => return Err(err.into()),
            };
        let incoming = serde_json::to_value(profile)?;
        for key in nexus_core::TELEMETRY_KEYS {
            // 空值不覆盖已有值：`sqmId` 在 macOS 上本来就是空串，但 Windows 上是真 UUID；
            // 一套在 macOS 上造的（或老版本存的）profile 带的是空串，别拿它去抹掉一个真值。
            match incoming.get(key) {
                Some(v) if !v.as_str().is_some_and(str::is_empty) => {
                    doc.insert(key.to_string(), v.clone());
                }
                _ => {
                    doc.entry(key.to_string()).or_insert_with(|| "".into());
                }
            }
        }

        if let Some(dir) = self.storage_json.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // 缩进 4 空格：和 Cursor 自己写出来的一致，diff 时不会整份变绿。
        let body = serde_json::to_string_pretty(&doc)?.replace("\n  ", "\n    ");
        atomic_write(&self.storage_json, body.as_bytes())?;

        if !profile.machine_id_file.is_empty() {
            if let Some(dir) = self.machine_id_file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            atomic_write(&self.machine_id_file, profile.machine_id_file.as_bytes())?;
        }
        Ok(())
    }
}

/// 先写临时文件再 rename。同目录内的 rename 在 POSIX 上是原子的，
/// 所以读者要么看到旧的完整内容，要么看到新的完整内容，不会看到写了一半的。
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("nexus")
    ));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).map_err(|err| {
        let _ = std::fs::remove_file(&tmp);
        AppError::new(
            ErrorCode::Io,
            format!("写入 {} 失败：{err}", path.display()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, MachineIds) {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("User/globalStorage/storage.json");
        std::fs::create_dir_all(storage.parent().unwrap()).unwrap();
        let ids = MachineIds::new(&storage, dir.path().join("machineid"));
        (dir, ids)
    }

    /// 本机实测的 storage.json 形状：四个 telemetry 键，外加一堆别的。
    fn seed(ids: &MachineIds) {
        std::fs::write(
            &ids.storage_json,
            r#"{
    "telemetry.machineId": "old-machine",
    "telemetry.macMachineId": "old-mac",
    "telemetry.devDeviceId": "old-dev",
    "telemetry.sqmId": "",
    "windowsState": {"lastActiveWindow": {"folder": "file:///x"}},
    "theme": "dark"
}"#,
        )
        .unwrap();
        std::fs::write(&ids.machine_id_file, "old-file-uuid").unwrap();
    }

    #[test]
    fn reads_all_five_values() {
        let (_dir, ids) = fixture();
        seed(&ids);
        let p = ids.read();
        assert_eq!(p.machine_id, "old-machine");
        assert_eq!(p.mac_machine_id, "old-mac");
        assert_eq!(p.dev_device_id, "old-dev");
        assert_eq!(p.sqm_id, "");
        assert_eq!(p.machine_id_file, "old-file-uuid");
        assert!(!p.is_empty());
    }

    #[test]
    fn write_preserves_unrelated_keys() {
        let (_dir, ids) = fixture();
        seed(&ids);
        ids.write(&MachineProfile::generate()).unwrap();
        let raw = std::fs::read_to_string(&ids.storage_json).unwrap();
        let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(doc["theme"], "dark");
        assert_eq!(
            doc["windowsState"]["lastActiveWindow"]["folder"],
            "file:///x"
        );
        assert_ne!(doc["telemetry.machineId"], "old-machine");
    }

    #[test]
    fn write_then_read_round_trips() {
        let (_dir, ids) = fixture();
        seed(&ids);
        let fresh = MachineProfile::generate();
        ids.write(&fresh).unwrap();
        assert_eq!(ids.read(), fresh);
    }

    #[test]
    fn missing_files_read_as_empty_not_error() {
        let (_dir, ids) = fixture();
        assert!(ids.read().is_empty());
    }

    #[test]
    fn a_corrupt_storage_json_is_refused_not_overwritten() {
        // 读不懂 ≠ 不存在。当成空对象写回去会把 Cursor 其余的状态全抹掉。
        let (_dir, ids) = fixture();
        std::fs::write(&ids.storage_json, r#"{"theme":"dark","telemetry.mach"#).unwrap();
        assert!(ids.read().is_empty());

        let err = ids.write(&MachineProfile::generate()).unwrap_err();
        assert_eq!(err.code, ErrorCode::CursorSchemaDrift);
        assert!(
            std::fs::read_to_string(&ids.storage_json)
                .unwrap()
                .contains("dark"),
            "文件必须原样留着，等 Cursor 自己修"
        );
    }

    #[test]
    fn a_missing_storage_json_is_created_from_scratch() {
        let (_dir, ids) = fixture();
        let fresh = MachineProfile::generate();
        ids.write(&fresh).unwrap();
        assert_eq!(ids.read().machine_id, fresh.machine_id);
    }

    #[test]
    fn an_empty_incoming_value_does_not_wipe_a_real_one() {
        // 机器上 sqmId 是真 UUID，而来的这套（macOS 造的、或老版本的）sqmId 是空串。
        // 显式清空而不是依赖 generate()：它在 Windows 上会给一个真 GUID。
        let (_dir, ids) = fixture();
        std::fs::write(
            &ids.storage_json,
            r#"{"telemetry.machineId":"m","telemetry.sqmId":"{REAL-SQM-GUID}"}"#,
        )
        .unwrap();
        let mut incoming = MachineProfile::generate();
        incoming.sqm_id.clear();
        ids.write(&incoming).unwrap();
        assert_eq!(
            ids.read().sqm_id,
            "{REAL-SQM-GUID}",
            "空的 sqmId 不该把机器上真的那个抹掉"
        );
    }

    #[test]
    fn refuses_to_write_an_empty_profile() {
        let (_dir, ids) = fixture();
        seed(&ids);
        assert_eq!(
            ids.write(&MachineProfile::default()).unwrap_err().code,
            ErrorCode::InvalidInput
        );
        assert_eq!(ids.read().machine_id, "old-machine", "原值必须还在");
    }

    #[test]
    fn write_leaves_no_temp_file_behind() {
        let (dir, ids) = fixture();
        seed(&ids);
        ids.write(&MachineProfile::generate()).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(ids.storage_json.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "临时文件没清掉：{leftovers:?}");
        drop(dir);
    }

    #[test]
    fn machine_id_file_is_written_without_trailing_whitespace() {
        let (_dir, ids) = fixture();
        let fresh = MachineProfile::generate();
        ids.write(&fresh).unwrap();
        let raw = std::fs::read_to_string(&ids.machine_id_file).unwrap();
        assert_eq!(raw, fresh.machine_id_file);
        assert!(!raw.ends_with('\n'));
    }
}
