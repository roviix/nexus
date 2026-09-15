//! 对外的一个门面：状态一眼看全、两个动作（刷 Box Relay 描述符、生成 / 续期直连凭证）。
//!
//! 这些动作按需触发（Sand 页按钮、网关取 token 时），不在后台常驻轮询——Grok Bot 是可选的
//! 额度来源，不该在用户没开相关模式时就去弹钥匙串授权。

use crate::app::{self, AppStatus};
use crate::credential::{self, StreamCredential};
use crate::descriptor::{self, BoxRelayDescriptor};
use crate::pod::{self, ExecTarget};
use crate::secrets::{self, GrokBotSecrets};
use nexus_core::{AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Box Relay 描述符的落盘位置——**故意与 v135 脚本同一路径**：两边互认，用户用哪个刷都行。
pub fn relay_config_path() -> PathBuf {
    let dir = {
        #[cfg(target_os = "windows")]
        {
            std::env::var_os("LOCALAPPDATA")
                .or_else(|| std::env::var_os("APPDATA"))
                .map(PathBuf::from)
                .unwrap_or_else(|| app::home_dir().join("AppData/Local"))
        }
        #[cfg(target_os = "macos")]
        {
            app::home_dir().join("Library/Application Support")
        }
        #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
        {
            app::home_dir().join(".config")
        }
    };
    dir.join("SandClientModeStream")
        .join("sand-client-cli")
        .join("grok-box-relay.json")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayInfo {
    pub base_url: String,
    pub relay_path: String,
    pub account_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectCredentialInfo {
    pub expires_at_ms: Option<u64>,
    pub expired: bool,
    pub can_renew: bool,
    pub account_email: Option<String>,
    /// `grokbot_app` / `library`；老文件没有为 `None`。
    pub source: Option<credential::CredentialSource>,
    pub minted_at_ms: Option<u64>,
    pub renewed_at_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokBotStatus {
    pub app: AppStatus,
    /// 活跃账号 email（不解密拿不到；钥匙串没授权、凭证又不是当前槽的时为 `None`，不算错）。
    pub active_email: Option<String>,
    /// 活跃账号槽 id（明文键，前端用它判断「换号了」）。
    pub active_slot: Option<String>,
    pub relay: Option<RelayInfo>,
    pub direct: Option<DirectCredentialInfo>,
    /// 直连凭证是上一个号生成的：还能用，但花的是旧号的额度，要重生成。
    pub direct_stale: bool,
}

/// Grok Bot 客户端此刻登着谁。**不含秘密**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokBotIdentity {
    pub email: Option<String>,
    pub name: Option<String>,
    /// WorkOS `sub`（`auth0|user_…`）。
    pub subject: Option<String>,
    /// 有 refresh token 才能收进「我的账号」（托管门槛）。
    pub has_refresh: bool,
}

/// 从 Grok Bot 导出的可托管凭证。**含 refresh token**，只在 Rust 侧流动。
pub struct ExportedAccount {
    pub email: String,
    pub refresh_token: String,
    pub subject: Option<String>,
}

/// 某个号打 `sand-cua` 实际落到哪个模型。不含 token。
///
/// Bot 通道上 grok 4.7 不在目录里，只能看这个别名的分片：有灰度的号落到
/// `grok-4-7-0910-xhigh`，多数号仍是 `gpt-5.6-luna-high`。
pub const CUA_PROBE_FILENAME: &str = "grokbot-cua-probes.json";
pub const CUA_PROBE_MODEL: &str = "sand-cua";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CuaProbe {
    pub email: String,
    pub requested_model: String,
    pub resolved_model: Option<String>,
    pub has_grok47: bool,
    pub probed_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CuaProbeStore {
    #[serde(default)]
    probes: BTreeMap<String, CuaProbe>,
}

pub struct GrokBotService {
    data_dir: PathBuf,
    /// 解过一次的秘密留在内存里，避免每个动作都弹钥匙串。换账号 / 失败时清掉。
    secrets: Mutex<Option<GrokBotSecrets>>,
    /// `fresh_stream_credential` 发现没凭证 / 凭证是旧号的时，能不能自己去 Grok Bot 生成。
    /// 测试用 [`Self::offline`] 关掉——否则单测会真的去碰钥匙串和 pod。
    auto_mint: bool,
}

impl GrokBotService {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            data_dir: data_dir.to_path_buf(),
            secrets: Mutex::new(None),
            auto_mint: true,
        }
    }

    /// 只读盘上文件、只打续期接口，绝不碰钥匙串 / pod。给测试与「不想弹授权」的场景。
    pub fn offline(data_dir: &Path) -> Self {
        Self {
            auto_mint: false,
            ..Self::new(data_dir)
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    fn secrets(&self) -> Result<GrokBotSecrets> {
        let mut guard = self.secrets.lock().expect("grokbot secrets lock");
        if let Some(s) = guard.as_ref() {
            // 活跃槽换了就重读。
            if let Ok(fresh_slot) = active_slot_cheap() {
                if fresh_slot == s.account.slot {
                    return Ok(clone_secrets(s));
                }
            } else {
                return Ok(clone_secrets(s));
            }
        }
        let s = secrets::load()?;
        let out = clone_secrets(&s);
        *guard = Some(s);
        Ok(out)
    }

    pub fn forget_secrets(&self) {
        *self.secrets.lock().expect("grokbot secrets lock") = None;
    }

    // ---------- 只读 ----------

    /// 不触碰钥匙串的状态：装没装、登没登录、本地两份配置文件长什么样。
    ///
    /// Grok Bot 换了号（活跃槽 id 变了）这里要立刻反映出来：缓存的秘密作废、凭证标成「旧号的」。
    /// 槽 id 是 `sand-secrets.json` 里的明文键，比它不需要钥匙串。
    pub fn status(&self) -> GrokBotStatus {
        let active_slot = active_slot_cheap().ok();
        let cached_email = {
            let mut guard = self.secrets.lock().expect("grokbot secrets lock");
            if let (Some(s), Some(active)) = (guard.as_ref(), active_slot.as_deref()) {
                if s.account.slot != active {
                    *guard = None;
                }
            }
            guard.as_ref().and_then(|s| s.account.email.clone())
        };
        let relay = load_relay().map(|d| RelayInfo {
            base_url: d.base_url,
            relay_path: d.relay_path,
            account_fingerprint: d.account_fingerprint,
        });
        let cred = StreamCredential::load(&self.data_dir).ok().flatten();
        let direct_stale = cred
            .as_ref()
            .map(|c| !self.direct_matches_active(c))
            .unwrap_or(false);
        let direct = cred.as_ref().map(direct_info);
        // 缓存没有时，凭证文件若正是当前槽生成的，它记的 email 就是现在登着的号——不用解钥匙串。
        let active_email = cached_email.or_else(|| {
            cred.as_ref()
                .filter(|c| !direct_stale && c.account_slot.is_some())
                .and_then(|c| c.account_email.clone())
        });
        GrokBotStatus {
            app: app::status(),
            active_email,
            active_slot,
            relay,
            direct,
            direct_stale,
        }
    }

    pub fn launch(&self) -> Result<()> {
        app::launch()
    }

    /// Grok Bot 登着谁。要解钥匙串（首次弹授权），所以是显式动作而不是随 `status()` 一起做。
    pub fn identify(&self) -> Result<GrokBotIdentity> {
        let s = self.secrets()?;
        Ok(GrokBotIdentity {
            email: s.account.email.clone(),
            name: s.account.name.clone(),
            subject: s.account.subject.clone(),
            has_refresh: s
                .refresh_token
                .as_deref()
                .is_some_and(|t| t.split('.').count() == 3),
        })
    }

    /// 把 Grok Bot 当前账号的 email + refresh token 交给调用方（收进 `nexus-accounts`）。
    /// Grok Bot 登的就是 Cursor 账号，refresh token 与 OAuth 拿到的是同一种东西。
    pub fn export_active_account(&self) -> Result<ExportedAccount> {
        let s = self.secrets()?;
        let email = s.account.email.clone().ok_or_else(|| {
            AppError::new(ErrorCode::NotLoggedIn, "Grok Bot 账号没有 email 信息。")
                .with_hint("在 Grok Bot 里重新登录一次让它写入账号资料。")
        })?;
        let refresh_token = s
            .refresh_token
            .clone()
            .filter(|t| t.split('.').count() == 3)
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::SecretMissing,
                    "Grok Bot 这个账号槽里没有 refresh token。",
                )
                .with_hint(
                    "只有一次性 session 的号收进来会过期成死号；到「我的账号」用 OAuth 授权它。",
                )
            })?;
        Ok(ExportedAccount {
            email,
            refresh_token,
            subject: s.account.subject.clone(),
        })
    }

    pub fn relay_descriptor(&self) -> Option<BoxRelayDescriptor> {
        load_relay()
    }

    pub fn stream_credential(&self) -> Result<Option<StreamCredential>> {
        StreamCredential::load(&self.data_dir)
    }

    // ---------- 动作 ----------

    /// 从 Grok Bot 读 descriptor 并写 `grok-box-relay.json`。
    pub fn refresh_relay(&self) -> Result<BoxRelayDescriptor> {
        let secrets = self.secrets()?;
        let d = descriptor::load(&secrets)?;
        save_relay(&d)?;
        Ok(d)
    }

    pub fn clear_relay(&self) -> Result<()> {
        let p = relay_config_path();
        if p.is_file() {
            std::fs::remove_file(p)?;
        }
        Ok(())
    }

    /// 生成直连凭证：descriptor → pod exec 读 `sbi_*` → 续期 → 落盘。
    ///
    /// exec 地址优先从 descriptor 推（不需要 session token 参与任何 api2 调用）；推不出或 pod 在睡，
    /// 退回 `EnsureSandBox(wake)`。
    pub async fn mint_direct(&self) -> Result<StreamCredential> {
        let secrets = self.secrets()?;
        let descriptor = descriptor::load(&secrets).ok();
        if let Some(d) = &descriptor {
            let _ = save_relay(d);
        }

        let mut last_err: Option<AppError> = None;
        let mut sbi: Option<String> = None;
        if let Some(target) = descriptor.as_ref().and_then(ExecTarget::from_descriptor) {
            match pod::read_renewal_credential(&target).await {
                Ok(v) => sbi = Some(v),
                Err(e) => {
                    tracing::warn!(%e, "descriptor 推出的 exec daemon 读 sbi 失败，改走 EnsureSandBox");
                    last_err = Some(e);
                }
            }
        }
        if sbi.is_none() {
            let ensured = pod::ensure_sandbox(&secrets.session_token, &secrets.machine_id).await?;
            let target = ExecTarget::from_ensure(&ensured)
                .ok_or_else(|| AppError::upstream("EnsureSandBox 没返回 exec daemon 地址。"))?;
            sbi =
                Some(pod::read_renewal_credential(&target).await.map_err(
                    |e| match last_err.take() {
                        Some(first) => first.with_hint(format!("兜底路径也失败：{e}")),
                        None => e,
                    },
                )?);
        }
        let sbi = sbi.expect("sbi resolved");
        let renewed = credential::renew(&sbi).await?;
        let now = credential::now_ms();
        let cred = StreamCredential {
            grok_bot_token: renewed.grok_bot_token,
            machine_id: secrets.machine_id.clone(),
            renewal_credential: Some(sbi),
            expires_at_ms: Some(renewed.expires_at_ms),
            client_version: credential::DEFAULT_CLIENT_VERSION.into(),
            namespace: credential::DEFAULT_NAMESPACE.into(),
            account_email: secrets.account.email.clone(),
            account_slot: Some(secrets.account.slot.clone()),
            source: Some(credential::CredentialSource::GrokbotApp),
            minted_at_ms: Some(now),
            renewed_at_ms: None,
        };
        cred.save(&self.data_dir)?;
        Ok(cred)
    }

    /// **不经 Grok Bot 客户端**：用「我的账号」里某个号的 session token 直接换到 grokBotToken。
    ///
    /// 链路：`EnsureSandBox(wake)`（这个号自己的 Box pod，没有就建）→ exec daemon 读 `sbi_*` → 续期。
    /// 2026-09-09 用一个从未登过 Grok Bot 的账号实测通过（pod 新建、Stream 出流 4s）。
    /// 生成出来的凭证不绑 Grok Bot 客户端登着谁（`account_slot = None`），换号 = 再对另一个号调一次。
    pub async fn mint_direct_for_account(
        &self,
        email: &str,
        session_token: &str,
        machine_id: &str,
    ) -> Result<StreamCredential> {
        self.mint_direct_for_account_inner(email, session_token, machine_id, true)
            .await
    }

    /// 同上，但**不覆盖**本机那份正在用的直连凭证。给「探这个号有没有 4.7 灰度」用。
    pub async fn mint_direct_for_account_ephemeral(
        &self,
        email: &str,
        session_token: &str,
        machine_id: &str,
    ) -> Result<StreamCredential> {
        self.mint_direct_for_account_inner(email, session_token, machine_id, false)
            .await
    }

    async fn mint_direct_for_account_inner(
        &self,
        email: &str,
        session_token: &str,
        machine_id: &str,
        persist: bool,
    ) -> Result<StreamCredential> {
        let ensured = pod::ensure_sandbox(session_token, machine_id).await?;
        let target = ExecTarget::from_ensure(&ensured)
            .ok_or_else(|| AppError::upstream("EnsureSandBox 没返回 exec daemon 地址。"))?;
        let sbi = pod::read_renewal_credential(&target).await?;
        let renewed = credential::renew(&sbi).await?;
        let cred = StreamCredential {
            grok_bot_token: renewed.grok_bot_token,
            machine_id: machine_id.to_string(),
            renewal_credential: Some(sbi),
            expires_at_ms: Some(renewed.expires_at_ms),
            client_version: credential::DEFAULT_CLIENT_VERSION.into(),
            namespace: credential::DEFAULT_NAMESPACE.into(),
            account_email: Some(email.to_string()),
            account_slot: None,
            source: Some(credential::CredentialSource::Library),
            minted_at_ms: Some(credential::now_ms()),
            renewed_at_ms: None,
        };
        if persist {
            cred.save(&self.data_dir)?;
        }
        Ok(cred)
    }

    /// 本机正在用的直连凭证如果就是这个号、还能用，就续一下拿来探——不必再 mint。
    pub async fn stream_credential_for_probe(
        &self,
        email: &str,
    ) -> Result<Option<StreamCredential>> {
        let Some(mut cred) = StreamCredential::load(&self.data_dir)? else {
            return Ok(None);
        };
        let owner = cred.account_email.as_deref().unwrap_or("");
        if email_key(owner) != email_key(email) {
            return Ok(None);
        }
        let now = credential::now_ms();
        if cred.is_expired(now) && !cred.can_renew() {
            return Ok(None);
        }
        if cred.renew_if_needed().await? {
            cred.save(&self.data_dir)?;
        }
        Ok(Some(cred))
    }

    pub fn cua_probe_for(&self, email: &str) -> Option<CuaProbe> {
        load_cua_probes(&self.data_dir)
            .probes
            .get(&email_key(email))
            .cloned()
    }

    pub fn save_cua_probe(&self, probe: &CuaProbe) -> Result<()> {
        let mut store = load_cua_probes(&self.data_dir);
        store.probes.insert(email_key(&probe.email), probe.clone());
        save_cua_probes(&self.data_dir, &store)
    }

    /// 本机直连凭证是不是 Grok Bot **当前**登着的号的。不解密：比槽 id。
    /// 读不到活跃槽（Grok Bot 没装 / 没登录）按「不过期」算——没有新号可比。
    pub fn direct_matches_active(&self, cred: &StreamCredential) -> bool {
        match (active_slot_cheap().ok(), cred.account_slot.as_deref()) {
            (Some(active), Some(slot)) => active == slot,
            // 老凭证没记槽：无法判断，不打扰。
            _ => true,
        }
    }

    /// 直连凭证还能不能用：存在、未过期或可续、且属于当前活跃账号。
    pub fn direct_usable(&self) -> Result<bool> {
        Ok(self
            .stream_credential()?
            .map(|c| {
                (!c.is_expired(credential::now_ms()) || c.can_renew())
                    && self.direct_matches_active(&c)
            })
            .unwrap_or(false))
    }

    /// 给运行时用：拿一份**当前有效、属于当前账号**的凭证。快过期就续（只需要种子，不碰钥匙串）；
    /// Grok Bot 换了号就重生成（要钥匙串，首次授权过「始终允许」后静默）；没有文件也重生成。
    pub async fn fresh_stream_credential(&self) -> Result<StreamCredential> {
        let existing = StreamCredential::load(&self.data_dir)?;
        let missing = || {
            AppError::new(ErrorCode::SecretMissing, "还没有 Grok Bot 直连凭证。")
                .with_hint("到账号抽屉的「Grok Bot」页点「生成」。")
        };
        let mut cred = match existing {
            Some(c) if self.direct_matches_active(&c) => c,
            Some(_) if self.auto_mint => {
                tracing::info!("Grok Bot 换了号，重生成直连凭证");
                return self.mint_direct().await;
            }
            Some(_) => {
                return Err(AppError::new(
                    ErrorCode::Unauthorized,
                    "本机直连凭证是上一个 Grok Bot 账号的。",
                )
                .with_hint("到账号抽屉的「Grok Bot」页点「重新生成」。"))
            }
            None if self.auto_mint => return self.mint_direct().await,
            None => return Err(missing()),
        };
        if cred.renew_if_needed().await? {
            cred.save(&self.data_dir)?;
        }
        Ok(cred)
    }

    pub fn clear_direct(&self) -> Result<()> {
        StreamCredential::remove(&self.data_dir)
    }
}

fn direct_info(c: &StreamCredential) -> DirectCredentialInfo {
    let now = credential::now_ms();
    DirectCredentialInfo {
        expires_at_ms: c.expires_at_ms(),
        expired: c.is_expired(now),
        can_renew: c.can_renew(),
        account_email: c.account_email.clone(),
        source: c.source,
        minted_at_ms: c.minted_at_ms,
        renewed_at_ms: c.renewed_at_ms,
    }
}

fn load_relay() -> Option<BoxRelayDescriptor> {
    let raw = std::fs::read_to_string(relay_config_path()).ok()?;
    let d: BoxRelayDescriptor = serde_json::from_str(&raw).ok()?;
    d.validate().ok()?;
    Some(d)
}

fn save_relay(d: &BoxRelayDescriptor) -> Result<PathBuf> {
    d.validate()?;
    let p = relay_config_path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(d)?))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(p)
}

/// 不解密、只看 `active` 槽 id——用来判断缓存的秘密是不是已经换号。
fn active_slot_cheap() -> Result<String> {
    let raw = std::fs::read_to_string(secrets::secrets_path())?;
    let v: serde_json::Value = serde_json::from_str(&raw)?;
    let accounts = v
        .get("cursor-accounts")
        .ok_or_else(|| AppError::internal("no accounts"))?;
    let blob: serde_json::Value = match accounts {
        serde_json::Value::String(s) => serde_json::from_str(s)?,
        other => other.clone(),
    };
    blob.get("active")
        .and_then(|a| a.as_str())
        .map(str::to_string)
        .ok_or_else(|| AppError::internal("no active slot"))
}

fn email_key(email: &str) -> String {
    email.trim().to_ascii_lowercase()
}

fn cua_probe_path(data_dir: &Path) -> PathBuf {
    data_dir.join(CUA_PROBE_FILENAME)
}

fn load_cua_probes(data_dir: &Path) -> CuaProbeStore {
    let raw = std::fs::read_to_string(cua_probe_path(data_dir)).ok();
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_cua_probes(data_dir: &Path, store: &CuaProbeStore) -> Result<()> {
    let p = cua_probe_path(data_dir);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(store)?))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn clone_secrets(s: &GrokBotSecrets) -> GrokBotSecrets {
    GrokBotSecrets {
        machine_id: s.machine_id.clone(),
        session_token: s.session_token.clone(),
        refresh_token: s.refresh_token.clone(),
        account: s.account.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_without_anything_is_all_none() {
        let dir = tempfile::tempdir().unwrap();
        let svc = GrokBotService::new(dir.path());
        let s = svc.status();
        assert!(s.direct.is_none());
    }

    #[test]
    fn direct_info_reflects_expiry() {
        let c = StreamCredential {
            grok_bot_token: "a.b.c".into(),
            machine_id: "m".into(),
            renewal_credential: Some("sbi_1".into()),
            expires_at_ms: Some(1),
            client_version: "0.44.0".into(),
            namespace: "prod".into(),
            account_email: Some("x@y".into()),
            account_slot: None,
            source: None,
            minted_at_ms: None,
            renewed_at_ms: None,
        };
        let i = direct_info(&c);
        assert!(i.expired && i.can_renew);
        assert_eq!(i.account_email.as_deref(), Some("x@y"));
    }

    #[test]
    fn cua_probe_is_keyed_by_email_case_insensitively_and_stores_no_token() {
        let dir = tempfile::tempdir().unwrap();
        let svc = GrokBotService::offline(dir.path());
        assert!(svc.cua_probe_for("A@B.com").is_none());
        svc.save_cua_probe(&CuaProbe {
            email: "A@B.com".into(),
            requested_model: CUA_PROBE_MODEL.into(),
            resolved_model: Some("grok-4-7-0910-xhigh".into()),
            has_grok47: true,
            probed_at_ms: 1,
            error: None,
        })
        .unwrap();
        let p = svc.cua_probe_for("a@b.com").expect("cached");
        assert!(p.has_grok47);
        assert_eq!(p.resolved_model.as_deref(), Some("grok-4-7-0910-xhigh"));
        let raw = std::fs::read_to_string(dir.path().join(CUA_PROBE_FILENAME)).unwrap();
        assert!(!raw.contains("sbi_"));
        assert!(!raw.contains("grokBotToken"));
    }
}
