//! 账号锚点与设备身份。
//!
//! api2 按 `machineId`（拼进 `x-cursor-checksum`）数「这个账号 24 小时内用过几台电脑」，
//! 超限即 `ERROR_CUSTOM_MESSAGE "Too many computers"`，整号锁死。所以设备身份必须绑
//! **账号**这个跨刷新不变的量，绝不能绑会轮换的 access token——云端 gateway 早期正是
//! 这么翻的车：每刷一次 token 就凭空多出一台「电脑」，几十次后必锁
//! （`docs/relay/CURSOR-FULL-ARCHITECTURE.md` §I.8）。
//!
//! 这里每个函数都是 `gateway/src/cursor/protocol.js` 同名逻辑的**逐字节移植**，测试向量由
//! 那份 JS 直接跑出来，不凭记忆。有一处要特别说明：[`checksum`] 里连 JS 位移运算的
//! 32 位截断怪癖都一起搬了——服务端认的是那个形态，"修正"它等于换了一套 checksum。

use base64::Engine;
use sha2::{Digest, Sha256};

/// Cursor "Jyh cipher" 的起点字节。
const OBFUSCATE_SEED: u8 = 165;

/// 小写十六进制。
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

pub fn sha256_hex(input: impl AsRef<[u8]>) -> String {
    let mut h = Sha256::new();
    h.update(input.as_ref());
    hex(&h.finalize())
}

/// `x-cursor-checksum` 的字节混淆：逐字节先 XOR 上一个**输出**再加下标（mod 256），
/// 链式反馈取的是输出本身而不是输入。改一位服务端就 401 且不说为什么。
pub fn obfuscate(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len());
    let mut prev = OBFUSCATE_SEED;
    for (i, &b) in input.iter().enumerate() {
        let v = (b ^ prev).wrapping_add((i % 256) as u8);
        out.push(v);
        prev = v;
    }
    out
}

/// `x-cursor-checksum` = `urlsafe_b64_nopad(obfuscate(6 字节时间戳)) + machine_id`。
///
/// 时间戳是 `floor(now_ms / 1e6)`。JS 原文用六次位移取「6 字节大端」：
/// `(ts >> 40) & 255, (ts >> 32) & 255, (ts >> 24) & 255, …`。但 JS 的 `>>` 会把位移量截到
/// 5 位，所以 `>> 40` 实际是 `>> 8`、`>> 32` 实际是 `>> 0`——前两个字节其实是低 16 位的
/// 重复。这里**照搬**这个形态而不做"正确的"48 位大端：官方客户端和线上 gateway 发的都是
/// 这个，服务端接受的也是这个。测试向量 `Vfb45Bi9deadbeef` 钉住了它。
pub fn checksum(machine_id: &str, now_ms: u64) -> String {
    let ts = (now_ms / 1_000_000) as u32;
    let b = [
        (ts >> 8) as u8, // JS `>> 40` → `>> 8`
        ts as u8,        // JS `>> 32` → `>> 0`
        (ts >> 24) as u8,
        (ts >> 16) as u8,
        (ts >> 8) as u8,
        ts as u8,
    ];
    let prefix = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(obfuscate(&b));
    format!("{prefix}{machine_id}")
}

/// UUID v5（SHA-1，DNS 命名空间）。`x-session-id` / `x-cursor-config-version` 用它按账号算，
/// 这样它们也不会各自变成一维随 token 漂移的「设备指纹」。
pub fn uuid5_dns(name: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes()).to_string()
}

/// 账号的稳定锚点。
///
/// 取 JWT `sub` 里最后一段（`authkit|user_xxx` → `user_xxx`），跨刷新不变、一个账号一个。
/// 不是 JWT、解不出、或 `sub` 为空，才退回 `tok:<sha256(token)>`——那种号本就少见，
/// 退化行为不比从前差。
pub fn stable_account_id(access_token: &str) -> String {
    if let Some(sub) = jwt_subject(access_token) {
        let id = sub.rsplit('|').next().unwrap_or(&sub);
        if !id.is_empty() {
            return id.to_string();
        }
    }
    format!("tok:{}", sha256_hex(access_token))
}

/// 只解 payload，不验签——我们要的是里面的字段，不是信任它。
fn jwt_payload(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    // JWT 规范是 base64url 无填充，但 protocol.js 对标准字母表 / 带填充也宽容；两边都收。
    let normalized: String = payload
        .chars()
        .filter(|c| *c != '=')
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            c => c,
        })
        .collect();
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(normalized)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn jwt_subject(token: &str) -> Option<String> {
    jwt_payload(token)?.get("sub")?.as_str().map(str::to_string)
}

/// JWT 的 `exp`。不是 JWT 或没有 `exp` 返回 `None`——调用方自己决定「不知道」算不算过期。
pub fn jwt_expiry(token: &str) -> Option<std::time::SystemTime> {
    let exp = jwt_payload(token)?.get("exp")?.as_f64()?;
    if exp <= 0.0 {
        return None;
    }
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs_f64(exp))
}

/// 一个账号在 api2 眼里是谁、在哪台「电脑」上。
///
/// `machine_id` 有两种来路，选错就是 `Too many computers`：
/// - **派生**（[`DeviceIdentity::derived`]）：按账号算一个恒定值。给中转号、给不是本机
///   Cursor 正登着的号——它们本来就没有真机语境。
/// - **钉死**（[`DeviceIdentity::pinned`]）：用真机的 `telemetry.machineId`。给 Cursor 里此刻
///   正登着的那个号——它已经用真机码上过线了，网关再给它派生一个就成了第二台电脑。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    pub account: String,
    pub machine_id: String,
}

impl DeviceIdentity {
    pub fn derived(access_token: &str) -> Self {
        let account = stable_account_id(access_token);
        let machine_id = sha256_hex(format!("{account}machineId"));
        Self {
            account,
            machine_id,
        }
    }

    pub fn pinned(access_token: &str, machine_id: impl Into<String>) -> Self {
        Self {
            account: stable_account_id(access_token),
            machine_id: machine_id.into(),
        }
    }

    pub fn client_key(&self) -> String {
        sha256_hex(&self.account)
    }

    pub fn session_id(&self) -> String {
        uuid5_dns(&self.account)
    }

    pub fn config_version(&self) -> String {
        uuid5_dns(&format!("{}:config", self.account))
    }

    pub fn checksum(&self, now_ms: u64) -> String {
        checksum(&self.machine_id, now_ms)
    }
}

// 下面的期望值全部由 /tmp/vec.mjs 跑 gateway/src/cursor/protocol.js 得到（2026-09-02），
// 不是手算。改任何一个函数之前先想清楚：这些数字是服务端认的形态。
#[cfg(test)]
mod tests {
    use super::*;

    const JWT_PREFIXED: &str =
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhdXRoa2l0fHVzZXJfVEVTVDEyMyIsImV4cCI6OTk5OTk5OTk5OX0.sig";
    const JWT_BARE: &str = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJ1c2VyX0JBUkU5In0.sig";
    const OPAQUE_KEY: &str = "crsr_someopaquekey";

    #[test]
    fn obfuscate_matches_protocol_js_byte_for_byte() {
        assert_eq!(hex(&obfuscate(b"ABC")), "e4a7e6");
        assert_eq!(hex(&obfuscate(b"")), "");
        assert_eq!(hex(&obfuscate(&[0, 1, 2, 3, 4, 5])), "a5a5a9adadad");
    }

    #[test]
    fn obfuscate_feeds_back_the_output_not_the_input() {
        // 第二字节：0x42 ^ 0xe4（上一个**输出**）= 0xa6，+1 = 0xa7。
        // 若错误地反馈输入 0x41：0x42 ^ 0x41 = 0x03，+1 = 0x04——那就是另一套密码了。
        assert_eq!(obfuscate(b"AB")[1], 0xa7);
    }

    #[test]
    fn checksum_reproduces_the_js_shift_quirk() {
        // 若按"正确的"48 位大端算，前缀会是别的值；这里钉住的是 JS 实际发出的形态。
        assert_eq!(checksum("deadbeef", 1_700_000_000_000), "Vfb45Bi9deadbeef");
        assert_eq!(
            checksum("", 1_700_000_000_000).len(),
            8,
            "6 字节 → 8 个 b64url 字符"
        );
    }

    #[test]
    fn checksum_prefix_only_moves_once_per_1e6_ms() {
        let a = checksum("m", 1_700_000_000_000);
        let b = checksum("m", 1_700_000_999_999);
        let c = checksum("m", 1_700_001_000_000);
        assert_eq!(a, b, "同一个 1e6 ms 窗口内 checksum 不变");
        assert_ne!(a, c);
    }

    #[test]
    fn sha256_hex_is_lowercase_hex() {
        assert_eq!(
            sha256_hex("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn stable_account_id_takes_the_last_segment_of_a_prefixed_subject() {
        assert_eq!(stable_account_id(JWT_PREFIXED), "user_TEST123");
    }

    #[test]
    fn stable_account_id_accepts_a_bare_subject() {
        assert_eq!(stable_account_id(JWT_BARE), "user_BARE9");
    }

    #[test]
    fn stable_account_id_falls_back_to_a_token_hash_for_non_jwts() {
        assert_eq!(
            stable_account_id(OPAQUE_KEY),
            "tok:e773745473af4016965a8c6930d40d4f0cd349fb71989f6c544d7ba55e48556a"
        );
        assert!(stable_account_id("").starts_with("tok:"));
        assert!(stable_account_id("a.!!notbase64!!.c").starts_with("tok:"));
    }

    #[test]
    fn jwt_expiry_reads_exp_and_tolerates_non_jwts() {
        let exp = jwt_expiry(JWT_PREFIXED).unwrap();
        assert_eq!(
            exp.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs(),
            9_999_999_999
        );
        assert!(jwt_expiry(JWT_BARE).is_none(), "没有 exp");
        assert!(jwt_expiry(OPAQUE_KEY).is_none());
    }

    #[test]
    fn stable_account_id_survives_a_token_refresh() {
        // 同一个 sub、不同的 exp / 签名 = 刷新后的新 token。锚点必须相同——这就是它存在的理由。
        let refreshed =
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiJhdXRoa2l0fHVzZXJfVEVTVDEyMyIsImV4cCI6MX0.othersig";
        assert_eq!(
            stable_account_id(JWT_PREFIXED),
            stable_account_id(refreshed)
        );
    }

    #[test]
    fn derived_identity_matches_protocol_js_headers() {
        let id = DeviceIdentity::derived(JWT_PREFIXED);
        assert_eq!(id.account, "user_TEST123");
        assert_eq!(
            id.machine_id,
            "472863d1b0bf416aec181ad7721c730801a3c10ec4418cbfdb7e494b2e6045db"
        );
        assert_eq!(
            id.client_key(),
            "14bf27c057322f2115b7e183b2df2bded0fb6fac73537af7624bf98793f8a293"
        );
        assert_eq!(id.session_id(), "c4bbe3fc-3b68-5816-a637-881411c33b49");
        assert_eq!(id.config_version(), "1f91df2b-ed39-592c-9b0c-1ad75928fea3");
    }

    #[test]
    fn pinned_identity_keeps_the_real_machine_id_but_the_same_account_anchor() {
        let pinned = DeviceIdentity::pinned(JWT_PREFIXED, "realmachine");
        let derived = DeviceIdentity::derived(JWT_PREFIXED);
        assert_eq!(pinned.account, derived.account);
        assert_eq!(pinned.machine_id, "realmachine");
        assert_eq!(
            pinned.client_key(),
            derived.client_key(),
            "client-key 只看账号"
        );
        assert_eq!(pinned.session_id(), derived.session_id());
        assert!(pinned.checksum(1_700_000_000_000).ends_with("realmachine"));
    }

    #[test]
    fn two_accounts_are_two_computers() {
        let a = DeviceIdentity::derived(JWT_PREFIXED);
        let b = DeviceIdentity::derived(JWT_BARE);
        assert_ne!(a.machine_id, b.machine_id);
        assert_ne!(a.client_key(), b.client_key());
        assert_ne!(a.session_id(), b.session_id());
    }
}
