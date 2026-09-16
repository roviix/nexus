//! Cursor 的机器码（设备指纹）。
//!
//! Cursor 靠 `telemetry.machineId` 认「这是哪台电脑」，官方桌面端一台机器终身一个。
//! 多个号在同一套机器码下轮着登录，服务端一眼就能关联到同一台设备——这是切号的主要
//! 风控风险。所以切号本给**每个档绑一套自己的机器码**，切号时一起切：一号一机。
//!
//! 字段名直接用 storage.json 里的键（`telemetry.*`），序列化出来就是能写回去的形状：
//! 导出的档拿去别处直接喂给 Cursor 也认，不用转换。
//!
//! 这些**不是秘密**：它们是随机数，泄露了也换一套就行，所以直接进业务表，不走 `SecretStore`。

use serde::{Deserialize, Serialize};

/// storage.json 里我们认识的键。除这些之外的内容写回时原样保留。
pub const TELEMETRY_KEYS: [&str; 4] = [
    "telemetry.machineId",
    "telemetry.macMachineId",
    "telemetry.devDeviceId",
    "telemetry.sqmId",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MachineProfile {
    /// 64 位 hex（sha256 长度）。
    #[serde(rename = "telemetry.machineId", default)]
    pub machine_id: String,
    /// 64 位 hex。
    #[serde(rename = "telemetry.macMachineId", default)]
    pub mac_machine_id: String,
    /// UUID。
    #[serde(rename = "telemetry.devDeviceId", default)]
    pub dev_device_id: String,
    /// macOS 上实测为空串；Windows 上是 UUID。保留原值，不自作主张填。
    #[serde(rename = "telemetry.sqmId", default)]
    pub sqm_id: String,
    /// `<Cursor>/machineid` 这个单独文件的内容，UUID。
    #[serde(
        rename = "machineidFile",
        default,
        skip_serializing_if = "String::is_empty"
    )]
    pub machine_id_file: String,
}

impl MachineProfile {
    /// 造一套全新的机器码，格式对齐真实值。
    pub fn generate() -> Self {
        Self {
            machine_id: random_hex_64(),
            mac_machine_id: random_hex_64(),
            dev_device_id: uuid::Uuid::new_v4().to_string(),
            sqm_id: new_sqm_id(),
            machine_id_file: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// 一个字段都没读到 = storage.json 不在或全是空，调用方要当「未知」处理而不是
    /// 把一套空值写回去。
    pub fn is_empty(&self) -> bool {
        self.machine_id.is_empty()
            && self.mac_machine_id.is_empty()
            && self.dev_device_id.is_empty()
            && self.machine_id_file.is_empty()
    }

    /// 界面上标识一套机器码用前 8 位就够，全长既没用又刺眼。
    pub fn short(&self) -> String {
        self.machine_id.chars().take(8).collect()
    }
}

/// 新档的 `telemetry.sqmId`。
///
/// 这个字段是平台相关的：macOS 上真实值就是空串，Windows 上是大写花括号 GUID
/// （SQM = Windows 的遥测组件）。造一套「一号一机」的机器码时必须跟着平台走 ——
/// 在 Windows 上留空等于给所有档发同一个可辨识的特征，正好抵消了换机器码的意义。
fn new_sqm_id() -> String {
    if cfg!(target_os = "windows") {
        format!("{{{}}}", uuid::Uuid::new_v4().to_string().to_uppercase())
    } else {
        String::new()
    }
}

fn random_hex_64() -> String {
    use rand::Rng;
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().fold(String::with_capacity(64), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_ids_match_the_real_shapes() {
        let m = MachineProfile::generate();
        assert_eq!(m.machine_id.len(), 64);
        assert_eq!(m.mac_machine_id.len(), 64);
        assert!(m.machine_id.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(uuid::Uuid::parse_str(&m.dev_device_id).is_ok());
        assert!(uuid::Uuid::parse_str(&m.machine_id_file).is_ok());
        assert_ne!(m.machine_id, MachineProfile::generate().machine_id);
    }

    /// sqmId 的形状随平台变：macOS 上真机就是空串，Windows 上是 `{大写 GUID}`。
    #[test]
    fn sqm_id_matches_the_shape_this_platform_really_uses() {
        let m = MachineProfile::generate();
        if cfg!(target_os = "windows") {
            let inner = m
                .sqm_id
                .strip_prefix('{')
                .and_then(|s| s.strip_suffix('}'))
                .expect("Windows 上应当是花括号包起来的 GUID");
            assert!(uuid::Uuid::parse_str(inner).is_ok());
            assert_eq!(inner, inner.to_uppercase(), "真实值是大写的");
            assert_ne!(m.sqm_id, MachineProfile::generate().sqm_id, "每档一个");
        } else {
            assert_eq!(m.sqm_id, "");
        }
    }

    #[test]
    fn json_keys_are_the_storage_json_keys() {
        let m = MachineProfile::generate();
        let v: serde_json::Value = serde_json::to_value(&m).unwrap();
        for key in TELEMETRY_KEYS {
            assert!(v.get(key).is_some(), "缺 {key}");
        }
        assert!(v.get("machineidFile").is_some());
    }

    #[test]
    fn round_trips_through_storage_json_shaped_json() {
        let raw = r#"{
            "telemetry.machineId": "aa",
            "telemetry.macMachineId": "bb",
            "telemetry.devDeviceId": "cc",
            "telemetry.sqmId": "",
            "machineidFile": "dd"
        }"#;
        let m: MachineProfile = serde_json::from_str(raw).unwrap();
        assert_eq!(m.machine_id, "aa");
        assert_eq!(m.machine_id_file, "dd");
        assert!(!m.is_empty());
    }

    #[test]
    fn empty_profile_is_recognised() {
        assert!(MachineProfile::default().is_empty());
    }
}
