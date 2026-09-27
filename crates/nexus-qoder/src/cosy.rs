//! COSY 请求签名。
//!
//! 官方客户端把身份（uid + job token）用一把一次性 AES-128-CBC 钥匙包起来，
//! 再用写死的 RSA 公钥包住这把钥匙。`Authorization` 是
//! `Bearer COSY.{payload}.{md5}`，MD5 的输入是 payload、钥匙、时间戳、**编码后的**
//! 请求体和签名路径，用换行拼起来。
//!
//! 机器系统报 `*_linux` 是有意的：官方 CLI 在 macOS 上也报 linux，网关按这套身份
//! 放行。改成 darwin 会拿到一份缩过的模型目录，或者直接被拒。

use crate::protocol::{CLIENT_TYPE, GATEWAY_COSY_VERSION};
use aes::Aes128;
use base64::Engine;
use cbc::cipher::{block_padding::Pkcs7, KeyIvInit};
use cbc::Encryptor;
use cipher::BlockEncryptMut;
use md5::{Digest, Md5};
use rsa::pkcs8::DecodePublicKey;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey};
use std::sync::OnceLock;

const RSA_PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----\n\
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc\n\
4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l\n\
6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17\n\
XcW+ML9FoCI6AOvOzwIDAQAB\n\
-----END PUBLIC KEY-----";

const STD_ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const CUSTOM_ALPHABET: &[u8] = b"_doRTgHZBKcGVjlvpC,@aFSx#DPuNJme&i*MzLOEn)sUrthbf%Y^w.(kIQyXqWA!";

/// 签名时用到的身份。`auth_token` 是 job token，不是 PAT。
pub struct CosyIdentity<'a> {
    pub user_id: &'a str,
    pub auth_token: &'a str,
    pub name: &'a str,
    pub email: &'a str,
    pub machine_id: &'a str,
}

struct Nonce {
    aes_key: String,
    request_id: String,
    timestamp: String,
    x_request_id: String,
}

pub fn machine_os() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "aarch64") => "aarch64_windows",
        ("windows", _) => "x86_64_windows",
        (_, "aarch64") => "aarch64_linux",
        _ => "x86_64_linux",
    }
}

/// `Encode=1` 的请求体：标准 base64，字母表换成官方那张，再按三分之一旋转。
pub fn encode_body(plaintext: &[u8]) -> Vec<u8> {
    let std = base64::engine::general_purpose::STANDARD.encode(plaintext);
    let bytes = std.as_bytes();
    let n = bytes.len();
    let a = n / 3;
    let table = encode_table();
    let mut out = Vec::with_capacity(n);
    for &b in &bytes[n - a..] {
        out.push(table[b as usize]);
    }
    for &b in &bytes[a..n - a] {
        out.push(table[b as usize]);
    }
    for &b in &bytes[..a] {
        out.push(table[b as usize]);
    }
    out
}

pub fn auth_headers(
    body: &[u8],
    request_url: &str,
    ident: &CosyIdentity<'_>,
) -> Result<Vec<(String, String)>, String> {
    if ident.user_id.is_empty() {
        return Err("这个 Qoder 账号没有 user id，重新导入一次 PAT。".into());
    }
    if ident.auth_token.is_empty() {
        return Err("这个 Qoder 账号没有 job token。".into());
    }
    let nonce = Nonce {
        aes_key: fresh_aes_key(),
        request_id: uuid::Uuid::new_v4().to_string(),
        timestamp: unix_secs().to_string(),
        x_request_id: uuid::Uuid::new_v4().to_string(),
    };
    auth_headers_with(body, request_url, ident, &nonce)
}

fn auth_headers_with(
    body: &[u8],
    request_url: &str,
    ident: &CosyIdentity<'_>,
    nonce: &Nonce,
) -> Result<Vec<(String, String)>, String> {
    let user_info = serde_json::json!({
        "uid": ident.user_id,
        "security_oauth_token": ident.auth_token,
        "name": ident.name,
        "aid": "",
        "email": ident.email,
    });
    let info = aes_cbc_base64(
        &serde_json::to_string(&user_info).map_err(|e| e.to_string())?,
        &nonce.aes_key,
    )?;
    let cosy_key = rsa_encrypt(nonce.aes_key.as_bytes())?;
    let payload = serde_json::json!({
        "version": "v1",
        "requestId": nonce.request_id,
        "info": info,
        "cosyVersion": GATEWAY_COSY_VERSION,
        "ideVersion": "",
    });
    let payload_b64 = base64::engine::general_purpose::STANDARD
        .encode(serde_json::to_string(&payload).map_err(|e| e.to_string())?);
    let path = crate::protocol::sig_path(request_url);
    let sig = signature_hex(&payload_b64, &cosy_key, &nonce.timestamp, body, &path);
    let body_hash = md5_hex(body);
    let machine = ident.machine_id;
    Ok(vec![
        (
            "Authorization".into(),
            format!("Bearer COSY.{payload_b64}.{sig}"),
        ),
        ("Cosy-Key".into(), cosy_key),
        ("Cosy-User".into(), ident.user_id.to_string()),
        ("Cosy-Date".into(), nonce.timestamp.clone()),
        ("Cosy-Version".into(), GATEWAY_COSY_VERSION.into()),
        ("Cosy-Machineid".into(), machine.to_string()),
        ("Cosy-Machinetoken".into(), machine.to_string()),
        ("Cosy-Machinetype".into(), "5".into()),
        ("Cosy-Machineos".into(), machine_os().into()),
        ("Cosy-Clienttype".into(), CLIENT_TYPE.into()),
        ("Cosy-Clientip".into(), "127.0.0.1".into()),
        ("Cosy-Bodyhash".into(), body_hash),
        ("Cosy-Bodylength".into(), body.len().to_string()),
        ("Cosy-Sigpath".into(), path),
        ("Cosy-Data-Policy".into(), "disagree".into()),
        ("Cosy-Organization-Id".into(), String::new()),
        ("Cosy-Organization-Tags".into(), String::new()),
        ("Login-Version".into(), "v2".into()),
        ("X-Request-Id".into(), nonce.x_request_id.clone()),
    ])
}

fn signature_hex(
    payload_b64: &str,
    cosy_key: &str,
    timestamp: &str,
    body: &[u8],
    sig_path: &str,
) -> String {
    let mut hasher = Md5::new();
    hasher.update(payload_b64.as_bytes());
    hasher.update(b"\n");
    hasher.update(cosy_key.as_bytes());
    hasher.update(b"\n");
    hasher.update(timestamp.as_bytes());
    hasher.update(b"\n");
    hasher.update(body);
    hasher.update(b"\n");
    hasher.update(sig_path.as_bytes());
    hex(&hasher.finalize())
}

fn md5_hex(bytes: &[u8]) -> String {
    hex(&Md5::digest(bytes))
}

fn aes_cbc_base64(plaintext: &str, key: &str) -> Result<String, String> {
    if key.len() != 16 {
        return Err("AES 钥匙必须是 16 字节。".into());
    }
    let enc = Encryptor::<Aes128>::new_from_slices(key.as_bytes(), key.as_bytes())
        .map_err(|e| e.to_string())?;
    let cipher = enc.encrypt_padded_vec_mut::<Pkcs7>(plaintext.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(cipher))
}

fn rsa_encrypt(data: &[u8]) -> Result<String, String> {
    let key = public_key().map_err(|e| format!("Qoder RSA 公钥解析失败：{e}"))?;
    let mut rng = rand_core_os();
    let encrypted = key
        .encrypt(&mut rng, Pkcs1v15Encrypt, data)
        .map_err(|e| format!("Qoder RSA 加密失败：{e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(encrypted))
}

fn public_key() -> Result<RsaPublicKey, rsa::pkcs8::spki::Error> {
    static KEY: OnceLock<RsaPublicKey> = OnceLock::new();
    if let Some(key) = KEY.get() {
        return Ok(key.clone());
    }
    let key = RsaPublicKey::from_public_key_pem(RSA_PUBLIC_KEY)?;
    let _ = KEY.set(key.clone());
    Ok(key)
}

fn rand_core_os() -> impl rsa::rand_core::CryptoRng + rsa::rand_core::RngCore {
    rsa::rand_core::OsRng
}

fn fresh_aes_key() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_string()
}

fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn encode_table() -> &'static [u8; 256] {
    static TABLE: OnceLock<[u8; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u8; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            *slot = i as u8;
        }
        for (i, std) in STD_ALPHABET.iter().enumerate() {
            table[*std as usize] = CUSTOM_ALPHABET[i];
        }
        table[b'=' as usize] = b'$';
        table
    })
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsa_public_key_is_the_gateway_key() {
        assert!(RSA_PUBLIC_KEY.contains("ERw9t6Pv5Nc"));
        let body = RSA_PUBLIC_KEY
            .lines()
            .filter(|line| !line.contains("PUBLIC KEY"))
            .collect::<String>();
        assert_eq!(body.len(), 216);
        assert!(
            RsaPublicKey::from_public_key_pem(RSA_PUBLIC_KEY).is_ok(),
            "Qoder RSA 公钥解析失败"
        );
    }

    #[test]
    fn body_encoding_matches_the_official_alphabet() {
        assert_eq!(std::str::from_utf8(&encode_body(b"{}")).unwrap(), "$kwm");
        assert_eq!(
            std::str::from_utf8(&encode_body(br#"{"a":1}"#)).unwrap(),
            "ep$$BMn%mYKi"
        );
        assert_eq!(
            std::str::from_utf8(&encode_body(b"hello")).unwrap(),
            "q$FruHPH"
        );
        assert_eq!(
            std::str::from_utf8(&encode_body("你好".as_bytes())).unwrap(),
            "SW(&QPQG"
        );
    }

    #[test]
    fn aes_key_is_also_the_iv() {
        let cipher = aes_cbc_base64(r#"{"uid":"u"}"#, "0123456789abcdef").unwrap();
        assert_eq!(cipher, "CmO7ppxytZs+wV9oQj2fpg==");
    }

    #[test]
    fn signature_feeds_md5_in_the_legacy_order() {
        let payload = base64::engine::general_purpose::STANDARD.encode(br#"{"v":1}"#);
        let sig = signature_hex(
            &payload,
            "COSKEY",
            "1700000000",
            b"encoded-body",
            "/api/v2/service/pro/sse/agent_chat_generation",
        );
        assert_eq!(sig, "a2b27bb2dc1e8b4c60bd911c08064a85");
        assert_eq!(md5_hex(b"encoded-body"), "f76226cd366680c0d545239af52099b8");
    }
}
