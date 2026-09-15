//! 秘密的命名。形如 `acct/<id>/refresh`，按模块分段。
//!
//! ref 只在这里构造。散落各处手写字符串的后果是拼错一个字符就"秘密丢了"，
//! 而且 grep 不出来谁在读谁在写。

use nexus_core::{AccountId, BackupId, ChatGptAccountId, GrokAccountId, KiroAccountId, ProfileId};

/// 一条秘密的引用。SQLite 里存的就是它的字符串形式。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SecretRef(String);

impl SecretRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 从库里读回来的 ref。不校验形状：老版本写下的 ref 也得能读。
    pub fn from_raw(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }
}

impl std::fmt::Display for SecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// 一个账号身上可能存在的几种秘密。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountSecret {
    /// Cursor refresh_token —— 长期凭证，能换 session、查用量。
    Refresh,
    /// Cursor access_token（refresh 换来的缓存，会过期）。
    Access,
    /// Cursor 登录密码。
    CursorPassword,
    /// 邮箱密码（少数渠道要）。
    EmailPassword,
    /// 辅助邮箱。严格说不是秘密，但它和密码一起构成找回路径，同等对待。
    RecoveryEmail,
    /// 长期 User API Key（`crsr_…`）。不能登回 IDE，能兑短期 access 查基础用量。
    ApiKey,
}

impl AccountSecret {
    fn slug(self) -> &'static str {
        match self {
            AccountSecret::Refresh => "refresh",
            AccountSecret::Access => "access",
            AccountSecret::CursorPassword => "cursor_pw",
            AccountSecret::EmailPassword => "email_pw",
            AccountSecret::RecoveryEmail => "recovery_email",
            AccountSecret::ApiKey => "api_key",
        }
    }

    /// 全部种类，删账号时逐个清。
    pub const ALL: [AccountSecret; 6] = [
        AccountSecret::Refresh,
        AccountSecret::Access,
        AccountSecret::CursorPassword,
        AccountSecret::EmailPassword,
        AccountSecret::RecoveryEmail,
        AccountSecret::ApiKey,
    ];
}

pub fn account_secret(id: &AccountId, kind: AccountSecret) -> SecretRef {
    SecretRef(format!("acct/{}/{}", id.as_str(), kind.slug()))
}

/// 一个 ChatGPT 账号身上的几种秘密。三样都是 OAuth 产物，来自 `codex login` 或我们自己的授权流程。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChatGptSecret {
    /// refresh_token —— 长期凭证，会轮换（每次刷新换一把，旧的立刻作废）。
    Refresh,
    /// access_token（约十天有效，刷出来的缓存）。推理请求头 `Authorization` 就是它。
    Access,
    /// id_token。只为读身份 claims（邮箱、套餐、组织）；重新授权才会更新。
    IdToken,
}

impl ChatGptSecret {
    fn slug(self) -> &'static str {
        match self {
            ChatGptSecret::Refresh => "refresh",
            ChatGptSecret::Access => "access",
            ChatGptSecret::IdToken => "id_token",
        }
    }

    pub const ALL: [ChatGptSecret; 3] = [
        ChatGptSecret::Refresh,
        ChatGptSecret::Access,
        ChatGptSecret::IdToken,
    ];
}

pub fn chatgpt_secret(id: &ChatGptAccountId, kind: ChatGptSecret) -> SecretRef {
    SecretRef(format!("chatgpt/{}/{}", id.as_str(), kind.slug()))
}

/// Grok Build（xAI CLI）账号的 OAuth 产物。形状和 ChatGPT 一样三样，命名空间分开。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GrokSecret {
    Refresh,
    Access,
    IdToken,
    /// xAI 开发者 API Key（`xai-…`）：按 token 计费的那种号，没有 refresh。
    ApiKey,
}

impl GrokSecret {
    fn slug(self) -> &'static str {
        match self {
            GrokSecret::Refresh => "refresh",
            GrokSecret::Access => "access",
            GrokSecret::IdToken => "id_token",
            GrokSecret::ApiKey => "api_key",
        }
    }

    pub const ALL: [GrokSecret; 4] = [
        GrokSecret::Refresh,
        GrokSecret::Access,
        GrokSecret::IdToken,
        GrokSecret::ApiKey,
    ];
}

pub fn grok_secret(id: &GrokAccountId, kind: GrokSecret) -> SecretRef {
    SecretRef(format!("grok/{}/{}", id.as_str(), kind.slug()))
}

/// Kiro 账号：access / refresh，以及 AWS SSO 刷新要用的 client 对。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KiroSecret {
    Refresh,
    Access,
    ClientId,
    ClientSecret,
}

impl KiroSecret {
    fn slug(self) -> &'static str {
        match self {
            KiroSecret::Refresh => "refresh",
            KiroSecret::Access => "access",
            KiroSecret::ClientId => "client_id",
            KiroSecret::ClientSecret => "client_secret",
        }
    }

    pub const ALL: [KiroSecret; 4] = [
        KiroSecret::Refresh,
        KiroSecret::Access,
        KiroSecret::ClientId,
        KiroSecret::ClientSecret,
    ];
}

pub fn kiro_secret(id: &KiroAccountId, kind: KiroSecret) -> SecretRef {
    SecretRef(format!("kiro/{}/{}", id.as_str(), kind.slug()))
}

/// 一个切号档的整套 `cursorAuth/*`（JSON）。
pub fn profile_auth(id: &ProfileId) -> SecretRef {
    SecretRef(format!("switch/{}/auth", id.as_str()))
}

/// 一份登录态备份。
pub fn backup_auth(id: &BackupId) -> SecretRef {
    SecretRef(format!("backup/{}", id.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refs_are_namespaced_per_module() {
        let acct = AccountId::from_raw("A1");
        assert_eq!(
            account_secret(&acct, AccountSecret::Refresh).as_str(),
            "acct/A1/refresh"
        );
        assert_eq!(
            account_secret(&acct, AccountSecret::CursorPassword).as_str(),
            "acct/A1/cursor_pw"
        );
        assert_eq!(
            account_secret(&acct, AccountSecret::ApiKey).as_str(),
            "acct/A1/api_key"
        );
        assert_eq!(
            profile_auth(&ProfileId::from_raw("P1")).as_str(),
            "switch/P1/auth"
        );
        assert_eq!(backup_auth(&BackupId::from_raw("B1")).as_str(), "backup/B1");
    }

    #[test]
    fn chatgpt_secrets_live_in_their_own_namespace() {
        let id = ChatGptAccountId::from_raw("C1");
        assert_eq!(
            chatgpt_secret(&id, ChatGptSecret::Refresh).as_str(),
            "chatgpt/C1/refresh"
        );
        assert_ne!(
            chatgpt_secret(&id, ChatGptSecret::Refresh),
            account_secret(&AccountId::from_raw("C1"), AccountSecret::Refresh),
            "同一个 id 串在两个平台下是两条秘密"
        );
        let refs: std::collections::HashSet<_> = ChatGptSecret::ALL
            .iter()
            .map(|k| chatgpt_secret(&id, *k))
            .collect();
        assert_eq!(refs.len(), ChatGptSecret::ALL.len());
    }

    #[test]
    fn grok_and_kiro_secrets_do_not_collide_with_chatgpt() {
        let same = "X1";
        assert_eq!(
            grok_secret(&GrokAccountId::from_raw(same), GrokSecret::Refresh).as_str(),
            "grok/X1/refresh"
        );
        assert_eq!(
            kiro_secret(&KiroAccountId::from_raw(same), KiroSecret::Refresh).as_str(),
            "kiro/X1/refresh"
        );
        assert_ne!(
            grok_secret(&GrokAccountId::from_raw(same), GrokSecret::Access),
            chatgpt_secret(&ChatGptAccountId::from_raw(same), ChatGptSecret::Access)
        );
        assert_eq!(KiroSecret::ALL.len(), 4);
    }

    #[test]
    fn every_account_secret_kind_has_a_distinct_slug() {
        let id = AccountId::from_raw("x");
        let refs: std::collections::HashSet<_> = AccountSecret::ALL
            .iter()
            .map(|k| account_secret(&id, *k))
            .collect();
        assert_eq!(refs.len(), AccountSecret::ALL.len());
    }
}
