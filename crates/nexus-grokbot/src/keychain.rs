//! Electron `safeStorage` 在 macOS 上的形态：钥匙串里存一段口令（service = "<App> Safe Storage"），
//! 数据 = base64("v10" + AES-128-CBC(pbkdf2_sha1(口令, "saltysalt", 1003, 16), IV = 16 个空格))。
//!
//! Grok Bot 是 Electron 应用，`sand-secrets.json` / `gateway-descriptor.json` 里的加密字段都是这一套。

use nexus_core::{AppError, ErrorCode, Result};

#[cfg(target_os = "macos")]
const SAFE_STORAGE_SERVICES: &[&str] = &["Grok Bot Safe Storage", "Grok Bot"];
const SALT: &[u8] = b"saltysalt";
const ITERATIONS: u32 = 1003;
const ENVELOPE_PREFIX: &[u8] = b"v10";

/// 从钥匙串取 safeStorage 口令。GUI 应用首次调用会弹「允许访问钥匙串」。
#[cfg(target_os = "macos")]
pub fn safe_storage_password() -> Result<String> {
    for service in SAFE_STORAGE_SERVICES {
        let out = std::process::Command::new("/usr/bin/security")
            .args(["find-generic-password", "-s", service, "-w"])
            .output();
        if let Ok(out) = out {
            if out.status.success() {
                let pw = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !pw.is_empty() {
                    return Ok(pw);
                }
            }
        }
    }
    Err(AppError::new(
        ErrorCode::SecretMissing,
        "钥匙串里没有 Grok Bot 的 safeStorage 口令。",
    )
    .with_hint("确认 Grok Bot 已安装并至少登录过一次；若刚弹了钥匙串授权，点「始终允许」后重试。"))
}

#[cfg(not(target_os = "macos"))]
pub fn safe_storage_password() -> Result<String> {
    Err(AppError::new(
        ErrorCode::UnsupportedPlatform,
        "读取 Grok Bot 凭证目前只实现了 macOS。",
    )
    .with_hint("Windows 走 DPAPI + AES-GCM，尚未移植。"))
}

/// 由口令派生 AES 密钥。
pub fn derive_key(password: &str) -> [u8; 16] {
    let mut key = [0u8; 16];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(password.as_bytes(), SALT, ITERATIONS, &mut key);
    key
}

/// 解一个 safeStorage 字段。
pub fn decrypt(value_b64: &str, key: &[u8; 16]) -> Result<String> {
    use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
    use base64::Engine;

    let raw = base64::engine::general_purpose::STANDARD
        .decode(value_b64.trim())
        .map_err(|_| AppError::internal("Grok Bot 加密字段不是合法 base64。"))?;
    if raw.len() < ENVELOPE_PREFIX.len() || &raw[..ENVELOPE_PREFIX.len()] != ENVELOPE_PREFIX {
        return Err(AppError::internal("Grok Bot 加密字段不是 v10 信封。"));
    }
    let iv = [0x20u8; 16];
    let dec = cbc::Decryptor::<aes::Aes128>::new(key.into(), &iv.into());
    let plain = dec
        .decrypt_padded_vec_mut::<Pkcs7>(&raw[ENVELOPE_PREFIX.len()..])
        .map_err(|_| {
            AppError::new(ErrorCode::SecretMissing, "Grok Bot 凭证解密失败。").with_hint(
                "钥匙串里的 safeStorage 口令与数据不匹配——多半是重装过 Grok Bot；重新登录一次即可。",
            )
        })?;
    String::from_utf8(plain).map_err(|_| AppError::internal("Grok Bot 解密结果不是 UTF-8。"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut, KeyIvInit};
    use base64::Engine;

    fn encrypt(plain: &str, key: &[u8; 16]) -> String {
        let iv = [0x20u8; 16];
        let enc = cbc::Encryptor::<aes::Aes128>::new(key.into(), &iv.into());
        let ct = enc.encrypt_padded_vec_mut::<Pkcs7>(plain.as_bytes());
        let mut raw = ENVELOPE_PREFIX.to_vec();
        raw.extend(ct);
        base64::engine::general_purpose::STANDARD.encode(raw)
    }

    #[test]
    fn round_trip_matches_electron_safe_storage_shape() {
        let key = derive_key("2+y7KT2VvjXoIN2O/zY5Lw==");
        let ct = encrypt(r#"{"email":"a@b.c"}"#, &key);
        assert_eq!(decrypt(&ct, &key).unwrap(), r#"{"email":"a@b.c"}"#);
    }

    #[test]
    fn wrong_key_is_reported_as_secret_missing() {
        let key = derive_key("right");
        let ct = encrypt("x", &key);
        let err = decrypt(&ct, &derive_key("wrong")).unwrap_err();
        assert_eq!(err.code, ErrorCode::SecretMissing);
    }

    #[test]
    fn non_v10_envelope_is_rejected() {
        let key = derive_key("k");
        let b64 = base64::engine::general_purpose::STANDARD.encode(b"v11zzzz");
        assert!(decrypt(&b64, &key).is_err());
    }
}
