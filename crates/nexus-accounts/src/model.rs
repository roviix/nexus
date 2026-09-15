//! 「我的账号」的数据形状。
//!
//! **托管门槛**（与 shop 侧 `qualifyCursorAccounts` 一致）：邮箱 + (refresh_token |
//! Cursor 密码)。只有一次性 session token 的不收——它会过期成死号，收进来只是给用户
//! 一个将来会失望的条目。

use crate::billing::AccountBilling;
use crate::usage::AccountUsage;
use nexus_core::{AccountId, AppError, ErrorCode, Result};
use serde::{Deserialize, Serialize};

/// 这个号是从哪来的。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    /// 用户自己加的 / 自己 OAuth 授权的 / 从清单或备份导进来的。
    Local,
    /// 从外面买来的号（批量导入进来的那些），和自己注册的分开标。
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

/// 「这个号**此刻**能不能用」的唯一答案。
///
/// 以前这个问题散在三处各算一套：卡片看 `status`，筛子看 `has_refresh` + access 是否过期，
/// 抽屉再看一遍 `session_only`。三处口径不同就会出现「卡上写待登录、筛子归仅会话」这种打架。
/// 现在只在 [`Account::availability`] 里算一次，随 `Account` 一起序列化，前端所有地方只认它。
///
/// 它和额度（用了多少）是两个维度：一个 Pro 号额度满满、refresh 掉了照样进不了池，
/// 所以它不并进额度那套颜色里。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    /// 有 refresh，能自己续期：切号、进网关、查用量都行。
    LongLived,
    /// 只靠一把还活着的 session token 撑着：此刻能用，到期就掉；切不了号。
    Session,
    /// 只有 `crsr_` User API Key：查得了花费、走得了 CRSR 通道，但拿不出会话——切号、网关都不行。
    ApiKey,
    /// 掉登录了：上游拒了凭证 / session 过期 / 只有密码还没授权。授权一次或粘一份新 token 能救回来。
    LoggedOut,
    /// refresh 被拒又没密码，救不回来了。
    Dead,
}

impl Availability {
    pub fn as_str(self) -> &'static str {
        match self {
            Availability::LongLived => "long_lived",
            Availability::Session => "session",
            Availability::ApiKey => "api_key",
            Availability::LoggedOut => "logged_out",
            Availability::Dead => "dead",
        }
    }
}

/// 从凭证推初始状态：有 refresh、还活着的 access、或 crsr_ API Key → 可用；只有密码 → 待登录。
pub fn status_from_credentials(
    has_refresh: bool,
    has_live_access: bool,
    has_api_key: bool,
) -> Status {
    if has_refresh || has_live_access || has_api_key {
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
    /// Stripe 门户读到的订阅标价 / 折扣 / 发票。密钥不在这里。
    pub billing: Option<AccountBilling>,
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
    /// 长期 `crsr_…` User API Key。不能切号，session 过期后仍能查基础用量。
    pub has_api_key: bool,
    pub created_at: String,
    pub updated_at: String,
    /// 入库的先后序号（SQLite rowid）。`created_at` 只到秒，一批导入的几十个号共用一个时刻，
    /// 「按添加时间排」就得靠它才能在同一秒内保持导入清单的顺序，而不是按随机 id 乱排。
    pub seq: i64,
    /// 归档时刻；`None` = 没归档。归档的号默认不列、不参与批量刷新、不进网关候选，
    /// 但凭证一个字节都不动——这是「先收起来」，不是删除。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<String>,
    /// 见 [`Availability`]。由 [`Account::availability`] 算出，读库时填好。
    pub availability: Availability,
}

impl Account {
    /// 此刻能不能用。这是唯一的判断入口，别在别处再拼一套。
    ///
    /// 顺序有讲究：`status` 是上游给过的判决（refresh 被拒、`shouldLogout`），比手上那把
    /// access 还没到期这种本地事实更可信——上游说你掉了，JWT 没过期也是掉了。
    pub fn availability(&self) -> Availability {
        if self.status == Status::Dead {
            return Availability::Dead;
        }
        if self.status == Status::NeedsLogin {
            return Availability::LoggedOut;
        }
        if self.has_refresh {
            return Availability::LongLived;
        }
        if self.has_live_access() {
            return Availability::Session;
        }
        if self.has_api_key {
            return Availability::ApiKey;
        }
        Availability::LoggedOut
    }

    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }

    /// 手上那把 access 还没过期。
    pub fn has_live_access(&self) -> bool {
        self.has_access && !crate::token::session_expired(self.access_expires_at.as_deref())
    }

    /// 只靠 session token 撑着、没有 refresh 的号：到期就得重新粘一份。
    pub fn session_only(&self) -> bool {
        !self.has_refresh && self.has_access
    }

    /// 能不能查用量：有 refresh 就永远行；没 refresh 看手上 access 还活着没；
    /// 再不行还有 `crsr_` —— 只能拉逐条花费，没有额度百分比。
    pub fn can_query_usage(&self) -> bool {
        self.has_refresh || self.has_live_access() || self.has_api_key
    }

    /// 此刻拿得出一把能用的会话吗——刷用量、进网关号池看这个。
    ///
    /// 有 refresh 就永远拿得出；没有时手上那把 access 还活着也算。只有 `crsr_` 的号**不算**：
    /// 那把 key 兑出来的是 `api_key_token`，查用量可以，当不了会话。
    pub fn has_usable_session(&self) -> bool {
        self.status != Status::Dead && (self.has_refresh || self.has_live_access())
    }

    /// 能不能把登录态写进本机 Cursor（加进切号本）。**必须有 refresh。**
    ///
    /// 仅会话的号曾经也放行，代价是：Cursor 的登录态要成对 token，没有 refresh 就只能把
    /// access 复制一份填进 `cursorAuth/refreshToken` 占位。Cursor 拿这个假 refresh 去续期
    /// 必然 401，然后掉登录——而这批号（token 导入、没密码、接不了验证码）掉了就找不回来。
    /// 与其让它死在一次切号上，不如一开始就不让进：这类号该走 CRSR 通道 / 网关用额度，
    /// 那两条路都不需要写 Cursor 的登录态。
    pub fn can_write_cursor_login(&self) -> bool {
        self.status != Status::Dead && self.has_refresh
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
    /// 长期 User API Key（`crsr_…`）。
    pub api_key: Option<String>,
    pub note: Option<String>,
    pub source: Option<Source>,
    pub tags: Vec<String>,
}

impl NewAccount {
    /// 托管门槛：邮箱 + (refresh_token | Cursor 密码 | session token | crsr_ API Key)。
    ///
    /// 挡在这里而不是入库后再说，是因为一个「只有邮箱」的条目对用户毫无用处，
    /// 却会一直占着列表位置让人以为它有用。
    ///
    /// session token 单独也收：手里确实有一批只拿得到它的号，不收就完全用不起来。代价是
    /// 它几小时到几天就过期，到期后这个号退回「待登录」，得重新粘一份——界面上会把这类号
    /// 标成「仅会话」，让人知道它不是长期的。
    ///
    /// `crsr_` 单独也收：切不进 Cursor，但 session 过期后还能查基础用量。
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
        if given(&self.api_key) {
            if crate::token::looks_like_user_api_key(self.api_key.as_deref().unwrap_or("")) {
                return Ok(());
            }
            return Err(AppError::new(
                ErrorCode::InvalidInput,
                format!("{} 的 API Key 不是 crsr_ 开头。", self.email),
            )
            .with_hint("User API Key 形如 crsr_…"));
        }
        Err(AppError::new(
            ErrorCode::InvalidInput,
            format!("{} 缺少凭证，收不进来。", self.email),
        )
        .with_hint("至少要有 refresh_token、Cursor 登录密码、session token、crsr_ API Key 之一。"))
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
        assert_eq!(status_from_credentials(true, false, false), Status::Active);
        assert_eq!(status_from_credentials(false, true, false), Status::Active);
        assert_eq!(status_from_credentials(false, false, true), Status::Active);
        assert_eq!(
            status_from_credentials(false, false, false),
            Status::NeedsLogin
        );
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
    fn an_api_key_alone_qualifies() {
        let a = NewAccount {
            email: "a@example.com".into(),
            api_key: Some("crsr_abc123DEF".into()),
            ..Default::default()
        };
        assert!(a.qualify().is_ok());
        let junk = NewAccount {
            email: "a@example.com".into(),
            api_key: Some("not-a-key".into()),
            ..Default::default()
        };
        assert!(junk.qualify().is_err());
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
            billing: None,
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
            has_api_key: false,
            created_at: "2026-09-02T00:00:00Z".into(),
            updated_at: "2026-09-02T00:00:00Z".into(),
            seq: 1,
            archived_at: None,
            availability: Availability::LongLived,
        };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v["hasRefresh"], true);
        assert_eq!(v["hasAccess"], false);
        assert_eq!(v["hasApiKey"], false);
        assert_eq!(v["codeChannel"], "auto");
        assert_eq!(v["source"], "purchased");
        assert_eq!(v["status"], "active");
        assert!(a.can_query_usage() && a.has_usable_session() && a.can_write_cursor_login());
    }

    /// 真实库里踩到的形态：上游拒了 session（status 已是待登录）但那把 JWT 还没到期。
    /// 以前卡片说「待登录」、筛子说「仅会话」；现在只有一个答案：掉登录。
    #[test]
    fn availability_is_one_answer_and_the_upstream_verdict_wins() {
        let base = Account {
            id: AccountId::from_raw("x"),
            email: "a@example.com".into(),
            source: Source::Local,
            status: Status::Active,
            note: None,
            tags: vec![],
            membership: None,
            signup_type: None,
            workos_user_id: None,
            usage: None,
            billing: None,
            last_checked_at: None,
            last_error: None,
            code_channel: "auto".into(),
            code_channel_resolved: None,
            last_code_at: None,
            has_refresh: false,
            has_access: false,
            access_expires_at: None,
            has_password: false,
            has_email_password: false,
            has_recovery_email: false,
            has_api_key: false,
            created_at: String::new(),
            updated_at: String::new(),
            seq: 0,
            archived_at: None,
            availability: Availability::LoggedOut,
        };
        let with = |f: &dyn Fn(&mut Account)| {
            let mut a = base.clone();
            f(&mut a);
            a.availability()
        };
        assert_eq!(with(&|a| a.has_refresh = true), Availability::LongLived);
        assert_eq!(
            with(&|a| {
                a.has_access = true;
                a.access_expires_at = Some("2099-01-01T00:00:00Z".into());
            }),
            Availability::Session
        );
        // access 过期 = 掉登录，不用等一次刷新失败来翻 status。
        assert_eq!(
            with(&|a| {
                a.has_access = true;
                a.access_expires_at = Some("2000-01-01T00:00:00Z".into());
            }),
            Availability::LoggedOut
        );
        // 上游判决优先于本地那把还没到期的 JWT。
        assert_eq!(
            with(&|a| {
                a.status = Status::NeedsLogin;
                a.has_access = true;
                a.access_expires_at = Some("2099-01-01T00:00:00Z".into());
            }),
            Availability::LoggedOut
        );
        assert_eq!(
            with(&|a| {
                a.status = Status::Dead;
                a.has_refresh = true;
            }),
            Availability::Dead
        );
        // 只有 crsr_ 的号：能查花费、走 CRSR 通道，但拿不出会话，单独一档，别混进掉登录。
        assert_eq!(with(&|a| a.has_api_key = true), Availability::ApiKey);
        // 有会话时 crsr_ 只是附加物，不改变答案。
        assert_eq!(
            with(&|a| {
                a.has_api_key = true;
                a.has_refresh = true;
            }),
            Availability::LongLived
        );
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
            billing: None,
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
            has_api_key: false,
            created_at: String::new(),
            updated_at: String::new(),
            seq: 0,
            archived_at: None,
            availability: Availability::LoggedOut,
        };
        assert!(!a.can_write_cursor_login());
        assert!(!a.has_usable_session());
        a.status = Status::Active;
        assert!(a.can_write_cursor_login());
        assert!(a.has_usable_session());
    }

    #[test]
    fn a_session_only_account_serves_requests_but_never_writes_the_cursor_login() {
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
            billing: None,
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
            has_api_key: false,
            created_at: String::new(),
            updated_at: String::new(),
            seq: 0,
            archived_at: None,
            availability: Availability::LoggedOut,
        };
        assert!(a.session_only());
        assert!(a.can_query_usage(), "有效期内拿它查用量 / 进网关都行");
        assert!(a.has_usable_session(), "有效期内拿得出一把会话");
        // 这是这批号的命门：写进 Cursor 就要拿 access 去占 refresh 那一格，
        // Cursor 续期 401 就掉登录，而它们没密码、接不了码，掉了找不回来。
        assert!(
            !a.can_write_cursor_login(),
            "没有 refresh 就绝不写 Cursor 登录态"
        );
        a.access_expires_at = Some("2020-01-01T00:00:00Z".into());
        assert!(!a.can_query_usage());
        assert!(!a.has_usable_session(), "过期之后连会话都拿不出来");
        a.has_api_key = true;
        assert!(a.can_query_usage(), "session 过期后 crsr_ 还能查基础用量");
        assert!(!a.has_usable_session(), "crsr_ 兑出来的 JWT 当不了会话");
        assert!(!a.can_write_cursor_login(), "API Key 登不回 Cursor");
    }
}
