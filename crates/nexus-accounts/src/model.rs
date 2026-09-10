//! 「我的账号」的数据形状。
//!
//! **托管门槛**（与 shop 侧 `qualifyCursorAccounts` 一致）：邮箱 + (refresh_token |
//! Cursor 密码)。只有一次性 session token 的不收——它会过期成死号，收进来只是给用户
//! 一个将来会失望的条目。

use crate::usage::AccountUsage;
use nexus_core::{AccountId, AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};

/// 这个号是从哪来的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// 用户自己加的 / 自己 OAuth 授权的 / 从清单或备份导进来的。
    Local,
    /// 商城买的，由用户从订单领进来。
    Purchased,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Local => "local",
            Source::Purchased => "purchased",
        }
    }

    /// 认不出的一律算自有号 —— 包括老库里的 `synced`（迁移 v4 已经把它们改写成 `local`，
    /// 这里只是兜底）。
    pub fn parse(raw: &str) -> Self {
        match raw {
            "purchased" => Source::Purchased,
            _ => Source::Local,
        }
    }
}

/// 状态**只反映「这个号现在能不能用」**。卖没卖、给谁了，那是备注，不是状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// 有 refresh_token，能查用量、能切进 Cursor。
    Active,
    /// 有密码但还没 refresh_token，需要 OAuth 登一次把它拿回来。
    NeedsLogin,
    /// refresh 被拒 / 账号失效。
    Dead,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Active => "active",
            Status::NeedsLogin => "needs_login",
            Status::Dead => "dead",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "active" => Status::Active,
            "dead" => Status::Dead,
            _ => Status::NeedsLogin,
        }
    }
}

/// 从凭证推初始状态：有 refresh、或有一把还没过期的 access → 可用；只有密码 → 待登录。
pub fn status_from_credentials(has_refresh: bool, has_live_access: bool) -> Status {
    if has_refresh || has_live_access {
        Status::Active
    } else {
        Status::NeedsLogin
    }
}

/// 一个账号。**这个结构里没有任何秘密**——秘密都在 `SecretStore`，这里只有「有没有」。
/// 它可以原样序列化给前端。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: AccountId,
    pub email: String,
    pub source: Source,
    pub status: Status,
    pub note: Option<String>,
    pub tags: Vec<String>,
    /// 订阅档，来自最近一次用量拉取。
    pub membership: Option<String>,
    pub signup_type: Option<String>,
    /// WorkOS 用户 id（`user_xxx`）。拼 session cookie 用，不算秘密。
    pub workos_user_id: Option<String>,
    pub usage: Option<AccountUsage>,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    /// `auto` 或某条具体渠道。
    pub code_channel: String,
    /// auto 模式下上次真正取到码的那条。
    pub code_channel_resolved: Option<String>,
    pub last_code_at: Option<String>,
    pub has_refresh: bool,
    /// 存着一把 access token（裸 JWT）。有 refresh 的号换 token 时顺手存下的；只靠 session
    /// token 撑着的号则全靠它。
    pub has_access: bool,
    /// 那把 access 的 `exp`，ISO。列表里判「还活着没」不用解密就能看。
    pub access_expires_at: Option<String>,
    pub has_password: bool,
    pub has_email_password: bool,
    pub has_recovery_email: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl Account {
    /// 手上那把 access 还没过期。
    pub fn has_live_access(&self) -> bool {
        self.has_access && !crate::token::session_expired(self.access_expires_at.as_deref())
    }

    /// 只靠 session token 撑着、没有 refresh 的号：到期就得重新粘一份。
    pub fn session_only(&self) -> bool {
        !self.has_refresh && self.has_access
    }

    /// 能不能拿到一把会话去干活（查用量、进网关、换 Grok 额度）：有 refresh 就永远行；
    /// 没 refresh 就看手上那把 access 还活着没。
    pub fn can_query_usage(&self) -> bool {
        self.has_refresh || self.has_live_access()
    }

    /// 能不能直接加进切号本 —— **必须有 refresh**。写进 Cursor 的登录态要能自己续期，
    /// 只给一把几小时的 access 等于让 Cursor 到点就掉线。
    pub fn can_switch(&self) -> bool {
        self.has_refresh && self.status != Status::Dead
    }
}

/// 新建 / 导入一个账号时提供的凭证。
#[derive(Debug, Clone, Default)]
pub struct NewAccount {
    pub email: String,
    pub refresh_token: Option<String>,
    /// session / access token：`user_xxx::<jwt>` 或裸 JWT，入库前经 `token::normalize_access`。
    pub access_token: Option<String>,
    pub cursor_password: Option<String>,
    pub email_password: Option<String>,
    pub recovery_email: Option<String>,
    pub note: Option<String>,
    pub source: Option<Source>,
}

impl NewAccount {
    /// 托管门槛：邮箱 + (refresh_token | Cursor 密码 | session token)。
    ///
    /// 挡在这里而不是入库后再说，是因为一个「只有邮箱」的条目对用户毫无用处，
    /// 却会一直占着列表位置让人以为它有用。
    ///
    /// session token 单独也收：手里确实有一批只拿得到它的号，不收就完全用不起来。代价是
    /// 它几小时到几天就过期，到期后这个号退回「待登录」，得重新粘一份——界面上会把这类号
    /// 标成「仅会话」，让人知道它不是长期的。
    pub fn qualify(&self) -> Result<()> {
        let given = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.trim().is_empty());
        if given(&self.refresh_token) || given(&self.cursor_password) {
            return Ok(());
        }
        if given(&self.access_token) {
            // 形状不对的当场拒，别存进去一串永远拼不出 cookie 的东西。
            crate::token::normalize_access(self.access_token.as_deref().unwrap_or(""))?;
            return Ok(());
        }
        Err(AppError::new(
            ErrorCode::InvalidInput,
            format!("{} 缺少凭证，收不进来。", self.email),
        )
        .with_hint("至少要有 refresh_token、Cursor 登录密码、session token 之一。"))
    }
}

/// 更新一个账号的可编辑字段。`None` = 不动。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountPatch {
    pub note: Option<String>,
    pub tags: Option<Vec<String>>,
    pub code_channel: Option<String>,
    pub status: Option<Status>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip_through_their_stored_form() {
        for s in [Source::Local, Source::Purchased] {
            assert_eq!(Source::parse(s.as_str()), s);
        }
        for s in [Status::Active, Status::NeedsLogin, Status::Dead] {
            assert_eq!(Status::parse(s.as_str()), s);
        }
        // 老的售卖态不再是状态，一律按「待登录」处理而不是崩掉。
        assert_eq!(Status::parse("sold"), Status::NeedsLogin);
        assert_eq!(Source::parse("import"), Source::Local);
        // 老库里的 `synced` 是用户自己的号，按自有号算。
        assert_eq!(Source::parse("synced"), Source::Local);
    }

    #[test]
    fn status_is_derived_from_whether_there_is_a_usable_credential() {
        assert_eq!(status_from_credentials(true, false), Status::Active);
        assert_eq!(status_from_credentials(false, true), Status::Active);
        assert_eq!(status_from_credentials(false, false), Status::NeedsLogin);
    }

    #[test]
    fn a_session_token_alone_qualifies_if_it_is_a_jwt() {
        use base64::Engine;
        let enc = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        let jwt = format!(
            "{}.{}.{}",
            enc(br#"{"alg":"HS256"}"#),
            enc(br#"{"sub":"auth0|user_1","exp":1}"#),
            enc(b"sig")
        );
        let ok = NewAccount {
            email: "a@example.com".into(),
            access_token: Some(format!("user_1::{jwt}")),
            ..Default::default()
        };
        assert!(ok.qualify().is_ok());
        let junk = NewAccount {
            email: "a@example.com".into(),
            access_token: Some("crsr_not_a_session".into()),
            ..Default::default()
        };
        assert!(junk.qualify().is_err());
    }

    #[test]
    fn a_refresh_token_alone_qualifies() {
        let a = NewAccount {
            email: "a@example.com".into(),
            refresh_token: Some("rt".into()),
            ..Default::default()
        };
        assert!(a.qualify().is_ok());
    }

    #[test]
    fn a_password_alone_qualifies() {
        let a = NewAccount {
            email: "a@example.com".into(),
            cursor_password: Some("pw".into()),
            ..Default::default()
        };
        assert!(a.qualify().is_ok());
    }

    #[test]
    fn an_email_alone_does_not_qualify() {
        let a = NewAccount {
            email: "a@example.com".into(),
            ..Default::default()
        };
        let err = a.qualify().unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.hint.unwrap().contains("refresh_token"));
    }

    #[test]
    fn blank_credentials_do_not_qualify() {
        let a = NewAccount {
            email: "a@example.com".into(),
            refresh_token: Some("   ".into()),
            cursor_password: Some(String::new()),
            ..Default::default()
        };
        assert!(a.qualify().is_err());
    }

    #[test]
    fn an_email_password_alone_does_not_qualify() {
        // 邮箱密码只能收验证码，登不进 Cursor。
        let a = NewAccount {
            email: "a@example.com".into(),
            email_password: Some("pw".into()),
            ..Default::default()
        };
        assert!(a.qualify().is_err());
    }

    #[test]
    fn serialized_account_uses_camel_case() {
        let a = Account {
            id: AccountId::from_raw("x"),
            email: "a@example.com".into(),
            source: Source::Purchased,
            status: Status::Active,
            note: None,
            tags: vec![],
            membership: None,
            signup_type: None,
            workos_user_id: None,
            usage: None,
            last_checked_at: None,
            last_error: None,
            code_channel: "auto".into(),
            code_channel_resolved: None,
            last_code_at: None,
            has_refresh: true,
            has_access: false,
            access_expires_at: None,
            has_password: false,
            has_email_password: false,
            has_recovery_email: false,
            created_at: "2026-09-02T00:00:00Z".into(),
            updated_at: "2026-09-02T00:00:00Z".into(),
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["hasRefresh"], true);
        assert_eq!(v["hasAccess"], false);
        assert_eq!(v["codeChannel"], "auto");
        assert_eq!(v["source"], "purchased");
        assert_eq!(v["status"], "active");
        assert!(a.can_query_usage() && a.can_switch());
    }

    #[test]
    fn a_dead_account_cannot_be_switched_into_even_with_a_refresh_token() {
        let mut a = Account {
            id: AccountId::from_raw("x"),
            email: "a@example.com".into(),
            source: Source::Local,
            status: Status::Dead,
            note: None,
            tags: vec![],
            membership: None,
            signup_type: None,
            workos_user_id: None,
            usage: None,
            last_checked_at: None,
            last_error: None,
            code_channel: "auto".into(),
            code_channel_resolved: None,
            last_code_at: None,
            has_refresh: true,
            has_access: false,
            access_expires_at: None,
            has_password: false,
            has_email_password: false,
            has_recovery_email: false,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!(!a.can_switch());
        a.status = Status::Active;
        assert!(a.can_switch());
    }

    #[test]
    fn a_session_only_account_works_until_its_access_expires_but_never_switches() {
        let mut a = Account {
            id: AccountId::from_raw("x"),
            email: "a@example.com".into(),
            source: Source::Local,
            status: Status::Active,
            note: None,
            tags: vec![],
            membership: None,
            signup_type: None,
            workos_user_id: Some("user_1".into()),
            usage: None,
            last_checked_at: None,
            last_error: None,
            code_channel: "auto".into(),
            code_channel_resolved: None,
            last_code_at: None,
            has_refresh: false,
            has_access: true,
            access_expires_at: Some("2099-01-01T00:00:00Z".into()),
            has_password: false,
            has_email_password: false,
            has_recovery_email: false,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!(a.session_only());
        assert!(a.can_query_usage(), "有效期内拿它查用量 / 进网关都行");
        assert!(!a.can_switch(), "写进 Cursor 的登录态必须能自己续期");
        a.access_expires_at = Some("2020-01-01T00:00:00Z".into());
        assert!(!a.can_query_usage());
    }
}
