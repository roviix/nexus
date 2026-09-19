//! 从本机官方 ZCode 客户端导入凭证。
//!
//! 为什么不做 OAuth：官方客户端登录后把凭证写在 `~/.zcode/v2/credentials.json`，
//! 加解密方式就在它自己的 bundle 里。用户在官方客户端登录一次，这里直接读，
//! 比复刻一遍「device-poll 轮询 + 三跳 biz API 换 key」既短又不会因为对方改流程而烂掉。
//!
//! 文件格式（ZCode 3.12.3 `createCredentialCipherProvider`）：
//!
//! ```text
//! { "<key>": "enc:v1:{iv}.{tag}.{ciphertext}" }   三段都是 base64url
//! 算法  aes-256-gcm，iv 12 字节，tag 16 字节
//! 密钥  sha256( $ZCODE_CREDENTIAL_SECRET
//!               ?? "zcode-credential-fallback:{platform}:{homedir}:{username}" )
//! ```
//!
//! 不以 `enc:v1:` 开头的值按明文原样返回 —— 官方客户端就是这么兼容旧文件的。
//!
//! 密钥种子里有 homedir 和 username，所以这个文件**跨机器不通用**。导入必须在用户
//! 本机做，不能让用户把文件拷过来。

use crate::model::{ZcodePlan, ZcodeProvider};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use nexus_core::{AppError, ErrorCode, Result};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const PREFIX: &str = "enc:v1:";
const IV_LEN: usize = 12;
const TAG_LEN: usize = 16;
const ENV_SECRET: &str = "ZCODE_CREDENTIAL_SECRET";

/// 官方客户端的凭证文件位置。`ZCODE_HOME` 可以改（测试和非常规安装用）。
pub fn credentials_path() -> PathBuf {
    if let Some(dir) = std::env::var_os("ZCODE_HOME") {
        return PathBuf::from(dir).join("v2").join("credentials.json");
    }
    home_dir()
        .unwrap_or_default()
        .join(".zcode")
        .join("v2")
        .join("credentials.json")
}

// ---------------------------------------------------------------------------
// 密钥派生
// ---------------------------------------------------------------------------

/// node 的 `os.homedir()`：先看 `$HOME`，再退到 passwd 里的 `pw_dir`。
fn home_dir() -> Option<PathBuf> {
    #[cfg(unix)]
    {
        if let Some(h) = std::env::var_os("HOME").filter(|h| !h.is_empty()) {
            return Some(PathBuf::from(h));
        }
        passwd_field(|pw| pw.pw_dir).map(PathBuf::from)
    }
    #[cfg(not(unix))]
    {
        std::env::var_os("USERPROFILE")
            .filter(|h| !h.is_empty())
            .map(PathBuf::from)
    }
}

/// node 的 `os.userInfo().username`：来自 passwd，**不是** `$USER`。
/// 两者在 sudo / launchd 下会不一样，而这里差一个字符就解不开。
fn user_name() -> Option<String> {
    #[cfg(unix)]
    {
        if let Some(n) = passwd_field(|pw| pw.pw_name) {
            return Some(n);
        }
        std::env::var("USER").ok().filter(|u| !u.is_empty())
    }
    #[cfg(not(unix))]
    {
        std::env::var("USERNAME").ok().filter(|u| !u.is_empty())
    }
}

#[cfg(unix)]
fn passwd_field(pick: impl Fn(&libc::passwd) -> *mut libc::c_char) -> Option<String> {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() {
            return None;
        }
        let ptr = pick(&*pw);
        if ptr.is_null() {
            return None;
        }
        std::ffi::CStr::from_ptr(ptr)
            .to_str()
            .ok()
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    }
}

fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// 默认种子。和官方客户端的 `defaultCredentialSecret` 逐字节一致。
fn default_secret() -> String {
    let home = home_dir().unwrap_or_default();
    let user = user_name().unwrap_or_else(|| "unknown".into());
    format!(
        "zcode-credential-fallback:{}:{}:{}",
        node_platform(),
        home.display(),
        user
    )
}

fn cipher_key(secret: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(secret.as_bytes()));
    out
}

/// 解一个字段。不带 `enc:v1:` 前缀的原样返回。
pub fn decrypt_with(value: &str, secret: &str) -> Result<String> {
    let Some(rest) = value.strip_prefix(PREFIX) else {
        return Ok(value.to_string());
    };
    let parts: Vec<&str> = rest.split('.').collect();
    if parts.len() != 3 {
        return Err(AppError::invalid("ZCode 凭证密文格式不对（不是三段）。"));
    }
    let de = |s: &str| {
        URL_SAFE_NO_PAD
            .decode(s)
            .map_err(|_| AppError::invalid("ZCode 凭证密文不是合法的 base64url。"))
    };
    let (iv, tag, ct) = (de(parts[0])?, de(parts[1])?, de(parts[2])?);
    if iv.len() != IV_LEN {
        return Err(AppError::invalid("ZCode 凭证的 IV 长度不对。"));
    }
    if tag.len() != TAG_LEN {
        return Err(AppError::invalid("ZCode 凭证的 AuthTag 长度不对。"));
    }

    // ring 要求密文和 tag 连在一起，而文件里它们是分开的两段。
    let mut buf = ct;
    buf.extend_from_slice(&tag);

    let key = UnboundKey::new(&AES_256_GCM, &cipher_key(secret))
        .map(LessSafeKey::new)
        .map_err(|_| AppError::internal("AES 密钥装载失败"))?;
    let nonce = Nonce::try_assume_unique_for_key(&iv)
        .map_err(|_| AppError::internal("AES nonce 装载失败"))?;
    let plain = key
        .open_in_place(nonce, Aad::empty(), &mut buf)
        .map_err(|_| {
            AppError::new(
                ErrorCode::InvalidInput,
                "解不开 ZCode 客户端的凭证：密钥不匹配或密文已损坏。",
            )
            .with_hint(
                "凭证加密时绑定了本机用户名与主目录，不能从别的机器拷过来。\
                 如果官方客户端那边设过 ZCODE_CREDENTIAL_SECRET，这里也要设同一个值。",
            )
        })?;
    String::from_utf8(plain.to_vec())
        .map_err(|_| AppError::invalid("ZCode 凭证解出来不是合法的 UTF-8。"))
}

pub fn decrypt(value: &str) -> Result<String> {
    let secret = std::env::var(ENV_SECRET)
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(default_secret);
    decrypt_with(value, &secret)
}

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

/// 从官方客户端读出来的一个号。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedAccount {
    pub provider: ZcodeProvider,
    pub plan: ZcodePlan,
    /// `zai-individual-coding-plan` 这种套餐族名。start-plan 没有。
    pub family: Option<String>,
    /// 官方客户端里的账号 uuid（coding）或 user_id（start）。
    pub account_uuid: Option<String>,
    pub email: Option<String>,
    /// coding-plan 的 `{apiKeyId}.{apiKeySecret}`。
    pub api_key: Option<String>,
    /// start-plan 的 JWT。
    pub jwt: Option<String>,
}

impl ImportedAccount {
    /// 判重用的稳定身份。
    pub fn account_ref(&self) -> String {
        let who = self.account_uuid.as_deref().unwrap_or("default");
        match self.plan {
            ZcodePlan::CodingPlan => format!(
                "{}:{}:{who}",
                self.provider.as_str(),
                self.family.as_deref().unwrap_or("coding-plan")
            ),
            ZcodePlan::StartPlan => format!("{}:start-plan:{who}", self.provider.as_str()),
        }
    }
}

/// `account-provider:coding-plan:account:zai-individual-coding-plan:account:{uuid}:api-key`
fn parse_api_key_name(key: &str) -> Option<(ZcodePlan, String, String)> {
    let p: Vec<&str> = key.split(':').collect();
    if p.len() != 7 || p[0] != "account-provider" || p[2] != "account" || p[4] != "account" {
        return None;
    }
    if p[6] != "api-key" {
        return None;
    }
    let family = p[3].trim();
    if family.is_empty() {
        return None;
    }
    Some((ZcodePlan::parse(p[1]), family.to_string(), p[5].to_string()))
}

fn provider_of_family(family: &str, fallback: ZcodeProvider) -> ZcodeProvider {
    if family.starts_with("bigmodel") {
        ZcodeProvider::Bigmodel
    } else if family.starts_with("zai") {
        ZcodeProvider::Zai
    } else {
        fallback
    }
}

/// 一把 coding-plan 的 key 长这样：`{32 位 id}.{16 位 secret}`。
fn looks_like_api_key(s: &str) -> bool {
    let mut it = s.split('.');
    match (it.next(), it.next(), it.next()) {
        (Some(id), Some(secret), None) => {
            !id.is_empty()
                && !secret.is_empty()
                && id.bytes().all(|b| b.is_ascii_alphanumeric())
                && secret.bytes().all(|b| b.is_ascii_alphanumeric())
        }
        _ => false,
    }
}

/// 解析整份 credentials.json。`secret` 为 `None` 时用默认种子。
///
/// 单个字段解不开不会让整次导入失败 —— 用户可能只有其中一档套餐，
/// 而一条坏记录不该挡住好的那条。
pub fn parse_credentials(raw: &str, secret: Option<&str>) -> Result<Vec<ImportedAccount>> {
    let doc: serde_json::Map<String, serde_json::Value> = serde_json::from_str(raw)
        .map_err(|e| AppError::invalid(format!("ZCode 凭证文件不是合法 JSON：{e}")))?;

    let owned_secret;
    let secret = match secret {
        Some(s) => s,
        None => {
            owned_secret = std::env::var(ENV_SECRET)
                .ok()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(default_secret);
            &owned_secret
        }
    };
    let field = |name: &str| -> Option<String> {
        let v = doc.get(name)?.as_str()?;
        match decrypt_with(v, secret) {
            Ok(p) => Some(p),
            Err(err) => {
                tracing::debug!(field = name, %err, "ZCode 凭证字段解不开，跳过");
                None
            }
        }
    };

    let active = field("oauth:active_provider")
        .map(|p| ZcodeProvider::parse(&p))
        .unwrap_or(ZcodeProvider::Zai);

    // 邮箱在 `oauth:{provider}:user_info` 里，形如 {user_id,email,avatar,name}。
    let mut email = None;
    let mut user_id = None;
    for p in [ZcodeProvider::Zai, ZcodeProvider::Bigmodel] {
        let Some(raw) = field(&format!("oauth:{}:user_info", p.as_str())) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let pick = |k: &str| {
            v.get(k)
                .and_then(|x| x.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        email = email.or_else(|| pick("email").map(|e| e.to_ascii_lowercase()));
        user_id = user_id.or_else(|| pick("user_id").or_else(|| pick("userId")));
        if email.is_some() && p == active {
            break;
        }
    }

    let mut out = Vec::new();

    // coding-plan：每把 key 一个号。个人版和团队版是两条。
    let mut names: Vec<&String> = doc.keys().filter(|k| k.contains(":api-key")).collect();
    names.sort(); // 稳定顺序，导入两次的账号顺序才一致
    for name in names {
        let Some((plan, family, uuid)) = parse_api_key_name(name) else {
            continue;
        };
        let Some(key) = field(name) else { continue };
        if !looks_like_api_key(&key) {
            tracing::debug!(family, "这把 key 不是 {{id}}.{{secret}} 的形状，跳过");
            continue;
        }
        out.push(ImportedAccount {
            provider: provider_of_family(&family, active),
            plan,
            family: Some(family),
            account_uuid: Some(uuid),
            email: email.clone(),
            api_key: Some(key),
            jwt: None,
        });
    }

    // start-plan：一份 JWT 一个号。
    if let Some(jwt) = field("zcodejwttoken").filter(|j| j.split('.').count() == 3) {
        out.push(ImportedAccount {
            provider: active,
            plan: ZcodePlan::StartPlan,
            family: None,
            account_uuid: user_id.clone(),
            email: email.clone(),
            api_key: None,
            jwt: Some(jwt),
        });
    }

    Ok(out)
}

/// 读本机官方客户端的凭证文件。
pub fn read_local(path: &Path) -> Result<Vec<ImportedAccount>> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        AppError::new(
            ErrorCode::Io,
            format!("读不到 ZCode 客户端的凭证：{}（{e}）", path.display()),
        )
        .with_hint("先装官方 ZCode 客户端并登录一次，或者用「粘贴导入」直接填 API key。")
    })?;
    let found = parse_credentials(&raw, None)?;
    if found.is_empty() {
        return Err(
            AppError::invalid("ZCode 客户端的凭证文件里没有可用的套餐凭证。")
                .with_hint("在官方客户端里登录一次，确认能正常发消息，再回来导入。"),
        );
    }
    Ok(found)
}

/// 手工粘贴：一行 `{apiKeyId}.{apiKeySecret}`，或者一整份 credentials.json。
pub fn parse_pasted(text: &str, provider: ZcodeProvider) -> Result<Vec<ImportedAccount>> {
    let raw = text.trim();
    if raw.is_empty() {
        return Err(AppError::invalid("没有内容。"));
    }
    if raw.starts_with('{') {
        return parse_credentials(raw, None);
    }
    let line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if looks_like_api_key(line) {
        return Ok(vec![ImportedAccount {
            provider,
            plan: ZcodePlan::CodingPlan,
            family: None,
            account_uuid: None,
            email: None,
            api_key: Some(line.to_string()),
            jwt: None,
        }]);
    }
    if line.split('.').count() == 3 {
        return Ok(vec![ImportedAccount {
            provider,
            plan: ZcodePlan::StartPlan,
            family: None,
            account_uuid: None,
            email: None,
            api_key: None,
            jwt: Some(line.to_string()),
        }]);
    }
    Err(AppError::invalid("认不出这段内容。").with_hint(
        "支持一行 `{apiKeyId}.{apiKeySecret}`、一份 ZCode 的 credentials.json，或一个 JWT。",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::aead::{Nonce, NONCE_LEN};

    /// 按官方客户端的格式加密一段文本，用来喂解密路径。
    fn seal(plain: &str, secret: &str) -> String {
        let key = UnboundKey::new(&AES_256_GCM, &cipher_key(secret))
            .map(LessSafeKey::new)
            .unwrap();
        let iv = [7u8; NONCE_LEN];
        let mut buf = plain.as_bytes().to_vec();
        let tag = key
            .seal_in_place_separate_tag(Nonce::assume_unique_for_key(iv), Aad::empty(), &mut buf)
            .unwrap();
        format!(
            "{PREFIX}{}.{}.{}",
            URL_SAFE_NO_PAD.encode(iv),
            URL_SAFE_NO_PAD.encode(tag.as_ref()),
            URL_SAFE_NO_PAD.encode(&buf),
        )
    }

    const SECRET: &str = "test-seed";

    #[test]
    fn round_trips_the_official_envelope() {
        let c = seal("zai", SECRET);
        assert!(c.starts_with("enc:v1:"));
        assert_eq!(c.split('.').count(), 3);
        assert_eq!(decrypt_with(&c, SECRET).unwrap(), "zai");
    }

    #[test]
    fn plaintext_passes_through_untouched() {
        // 官方客户端就是这么兼容旧文件的：没有前缀就是明文。
        assert_eq!(decrypt_with("zai", SECRET).unwrap(), "zai");
    }

    #[test]
    fn a_wrong_secret_fails_loudly_instead_of_returning_garbage() {
        let c = seal("zai", SECRET);
        let err = decrypt_with(&c, "other-seed").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.hint.is_some(), "解不开要告诉用户为什么");
    }

    #[test]
    fn parses_both_coding_plans_and_the_start_plan_jwt() {
        let uuid = "a0485d81-70b2-4bc7-9ffe-259af76f8dfd";
        let doc = serde_json::json!({
            "oauth:active_provider": seal("zai", SECRET),
            "oauth:zai:user_info": seal(
                r#"{"user_id":"u-1","email":"Me@Example.COM","name":"me"}"#, SECRET),
            "zcodejwttoken": seal("aaa.bbb.ccc", SECRET),
            format!("account-provider:coding-plan:account:zai-individual-coding-plan:account:{uuid}:api-key"):
                seal("b5aef1abcdefabcdefabcdefabcdef12.0123456789abcdef", SECRET),
            format!("account-provider:coding-plan:account:zai-team-coding-plan:account:{uuid}:api-key"):
                seal("b5aef1abcdefabcdefabcdefabcdef12.fedcba9876543210", SECRET),
        });
        let got = parse_credentials(&doc.to_string(), Some(SECRET)).unwrap();

        assert_eq!(got.len(), 3, "个人版 + 团队版 + 体验套餐");
        let coding: Vec<&ImportedAccount> = got
            .iter()
            .filter(|a| a.plan == ZcodePlan::CodingPlan)
            .collect();
        assert_eq!(coding.len(), 2);
        // 邮箱统一小写，两条 coding 号的 account_ref 必须不同，否则会互相覆盖。
        assert_eq!(coding[0].email.as_deref(), Some("me@example.com"));
        assert_ne!(coding[0].account_ref(), coding[1].account_ref());
        assert!(coding[0].account_ref().contains("individual"));

        let start = got.iter().find(|a| a.plan == ZcodePlan::StartPlan).unwrap();
        assert_eq!(start.jwt.as_deref(), Some("aaa.bbb.ccc"));
        assert!(start.api_key.is_none());
    }

    #[test]
    fn one_unreadable_field_does_not_sink_the_whole_import() {
        let doc = serde_json::json!({
            "oauth:active_provider": seal("zai", SECRET),
            "account-provider:coding-plan:account:zai-individual-coding-plan:account:u:api-key":
                seal("abc123.def456", SECRET),
            // 别的机器写进来的一条，用这台机器的种子解不开。
            "account-provider:coding-plan:account:zai-team-coding-plan:account:v:api-key":
                seal("abc123.def456", "a-different-machine"),
        });
        let got = parse_credentials(&doc.to_string(), Some(SECRET)).unwrap();
        assert_eq!(got.len(), 1, "解不开的那条跳过，好的那条要留下");
    }

    #[test]
    fn a_malformed_api_key_is_skipped_rather_than_stored() {
        let doc = serde_json::json!({
            "account-provider:coding-plan:account:zai-individual-coding-plan:account:u:api-key":
                seal("not-a-key-at-all", SECRET),
        });
        assert!(parse_credentials(&doc.to_string(), Some(SECRET))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn pasted_input_tells_a_key_from_a_jwt() {
        let key = parse_pasted("abc123.def456", ZcodeProvider::Zai).unwrap();
        assert_eq!(key[0].plan, ZcodePlan::CodingPlan);
        assert_eq!(key[0].api_key.as_deref(), Some("abc123.def456"));

        let jwt = parse_pasted("aaa.bbb.ccc", ZcodeProvider::Zai).unwrap();
        assert_eq!(jwt[0].plan, ZcodePlan::StartPlan);

        assert!(parse_pasted("   ", ZcodeProvider::Zai).is_err());
    }

    /// 对着本机官方客户端的真凭证跑一遍。默认不跑（CI 上没有这个文件，
    /// 而且它的解密密钥绑死在当前机器的用户名和主目录上）。
    ///
    /// `cargo test -p nexus-zcode -- --ignored --nocapture reads_the_real_client`
    #[test]
    #[ignore = "要本机装过官方 ZCode 客户端并登录过"]
    fn reads_the_real_client_credentials() {
        let path = credentials_path();
        let found = read_local(&path).expect("读本机 ZCode 凭证");
        for a in &found {
            // 只打不敏感的部分：秘密本身不进日志。
            println!(
                "{:>10} {:<32} key={} jwt={} ref={}",
                a.provider.as_str(),
                a.family.as_deref().unwrap_or("-"),
                a.api_key.is_some(),
                a.jwt.is_some(),
                a.account_ref(),
            );
            assert!(a.api_key.is_some() || a.jwt.is_some());
        }
        assert!(!found.is_empty());
    }

    #[test]
    fn the_key_name_grammar_is_exact() {
        assert!(parse_api_key_name(
            "account-provider:coding-plan:account:zai-team-coding-plan:account:u:api-key"
        )
        .is_some());
        // 段数不对、或者不是 api-key 结尾的都不认。
        assert!(parse_api_key_name("account-provider:coding-plan:account:x:api-key").is_none());
        assert!(parse_api_key_name("oauth:zai:access_token").is_none());
    }
}
