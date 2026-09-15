//! 候选号从哪来。
//!
//! 两个来源，机器码的来路不同、也不能搞混（搞混就是 `Too many computers`，见 `identity`）：
//!
//! - [`CursorLoginSource`]：Cursor 里**正登着**的那个号。token 从它自己的登录态库读，机器码
//!   钉成真机的 `telemetry.machineId`——它已经用这台电脑上过线，网关再派生一个就成了两台。
//!   **绝不刷它的 refresh_token**：Cursor 会轮换，从外面刷一次可能把 IDE 手里那把作废、把用户
//!   登出。access token 过期就只能等 IDE 自己刷。
//! - [`StoredAccountsSource`]：`nexus-accounts` 里的号。这些是我们托管的，
//!   `AccountsService::session` 负责刷 token 并把轮换后的 refresh 存回去；机器码按账号派生。
//!
//! 同一个号可能从两边都来（用户把正登着的号也加进了「我的账号」），由 `RelayLane` 按邮箱去重，
//! Cursor 登录那份优先——因为它的机器码是真的。

use crate::error::{UpstreamError, UpstreamKind};
use crate::identity::jwt_expiry;
use crate::lane::BoxFuture;
use nexus_accounts::model::Status;
use nexus_accounts::usage::AccountUsage;
use nexus_accounts::AccountsService;
use nexus_core::{AccountId, ErrorCode};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateKind {
    CursorLogin,
    Stored(AccountId),
    /// 订阅通道（ChatGPT / Grok / Kiro …）里的账号。走各自的 lane 与后端，不和 Cursor 的号混队。
    /// `channel` 是通道 id，`id` 是该平台账号表里的主键。
    Subscription {
        channel: &'static str,
        id: String,
    },
}

/// 从用量快照读出来的额度线索。**只是线索**：快照可能过期，真相由上游的错误说了算。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QuotaHint {
    pub percent_used: Option<f64>,
    pub exhausted: bool,
}

#[derive(Debug, Clone)]
pub struct Candidate {
    /// 邮箱。Lane 内部的键（小写比较）。
    pub label: String,
    pub kind: CandidateKind,
    /// `Some` = 用真机码；`None` = 按账号派生。
    pub pinned_machine_id: Option<String>,
    pub quota: QuotaHint,
}

pub struct ResolvedToken {
    pub access_token: String,
    pub expires_at: Option<SystemTime>,
}

impl std::fmt::Debug for ResolvedToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedToken")
            .field("access_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

pub trait Source: Send + Sync {
    fn name(&self) -> &'static str;
    /// 此刻有哪些候选。要便宜——每个请求都会问一次。
    fn candidates(&self) -> Vec<Candidate>;
    /// 把候选变成能用的 token。可能刷 token（联网）。
    fn resolve<'a>(
        &'a self,
        candidate: &'a Candidate,
    ) -> BoxFuture<'a, Result<ResolvedToken, UpstreamError>>;
}

/// 到线的判据。留 0.5% 余量：到 100% 再换，最后那一两个请求会先撞一次错。
const QUOTA_LINE: f64 = 99.5;

/// 从 usage 快照判断额度是不是到线。
///
/// `sand` 通道看 Bot 周额；其余看总量——Auto / API 是分桶计量的，某一桶满了只影响那一类模型，
/// 这里不替上游猜哪个请求落哪个桶，宁可保守（只在总量到线时判耗尽），剩下的交给反应式接力。
pub fn quota_hint(usage: Option<&AccountUsage>, client_type: &str) -> QuotaHint {
    let Some(u) = usage else {
        return QuotaHint::default();
    };
    if client_type == "sand" {
        let Some(bot) = &u.bot else {
            return QuotaHint::default();
        };
        return QuotaHint {
            percent_used: bot.percent_used,
            exhausted: bot.has_available == Some(false)
                || bot.percent_used.is_some_and(|p| p >= QUOTA_LINE),
        };
    }
    QuotaHint {
        percent_used: u.total_percent_used,
        exhausted: u.total_percent_used.is_some_and(|p| p >= QUOTA_LINE),
    }
}

// ---------- Cursor 里正登着的号 ----------

#[derive(Debug, Clone)]
pub struct CursorLogin {
    pub email: String,
    pub access_token: String,
    /// 真机 `telemetry.machineId`（64 位 hex）；读不到就 `None`。
    pub machine_id: Option<String>,
}

/// 读当前登录态。抽成 trait 是为了能用假的测 Source 逻辑；真实现就是 [`nexus_cursor::Cursor`]。
pub trait LoginReader: Send + Sync {
    fn read(&self) -> Option<CursorLogin>;
}

impl LoginReader for nexus_cursor::Cursor {
    fn read(&self) -> Option<CursorLogin> {
        let auth = self.state.read_auth().ok()?;
        let access = auth
            .get("cursorAuth/accessToken")
            .filter(|t| !t.is_empty())?
            .to_string();
        let email = auth.email()?;
        let machine = self.machine.read();
        Some(CursorLogin {
            email,
            access_token: access,
            machine_id: (machine.machine_id.len() == 64).then_some(machine.machine_id),
        })
    }
}

pub struct CursorLoginSource {
    reader: Arc<dyn LoginReader>,
    /// 登录态库是本地 SQLite，读一次不贵但也不该每个请求都读；IDE 刷了 token 我们要能跟上，
    /// 所以缓存很短。
    cache: Mutex<Option<(Instant, Option<CursorLogin>)>>,
    ttl: Duration,
}

impl CursorLoginSource {
    pub fn new(reader: Arc<dyn LoginReader>) -> Self {
        Self {
            reader,
            cache: Mutex::new(None),
            ttl: Duration::from_secs(30),
        }
    }

    #[cfg(test)]
    fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    fn current(&self) -> Option<CursorLogin> {
        let mut cache = self.cache.lock().expect("login cache");
        if let Some((at, login)) = &*cache {
            if at.elapsed() < self.ttl {
                return login.clone();
            }
        }
        let fresh = self.reader.read();
        *cache = Some((Instant::now(), fresh.clone()));
        fresh
    }
}

fn token_expired(token: &str) -> bool {
    jwt_expiry(token).is_some_and(|exp| exp <= SystemTime::now() + Duration::from_secs(60))
}

impl Source for CursorLoginSource {
    fn name(&self) -> &'static str {
        "cursor_login"
    }

    fn candidates(&self) -> Vec<Candidate> {
        let Some(login) = self.current() else {
            return Vec::new();
        };
        if token_expired(&login.access_token) {
            // 不刷它（见模块注释）。等 IDE 自己刷；在那之前这个号就不是候选。
            tracing::debug!(email = %login.email, "Cursor 登录态的 access token 已过期，等 IDE 刷新");
            return Vec::new();
        }
        vec![Candidate {
            label: login.email,
            kind: CandidateKind::CursorLogin,
            pinned_machine_id: login.machine_id,
            quota: QuotaHint::default(),
        }]
    }

    fn resolve<'a>(
        &'a self,
        candidate: &'a Candidate,
    ) -> BoxFuture<'a, Result<ResolvedToken, UpstreamError>> {
        Box::pin(async move {
            let login = self
                .current()
                .filter(|l| l.email.eq_ignore_ascii_case(&candidate.label))
                .ok_or_else(|| {
                    UpstreamError::new(UpstreamKind::Auth, 401, "Cursor 已不是这个号在登录")
                })?;
            if token_expired(&login.access_token) {
                return Err(UpstreamError::new(
                    UpstreamKind::Auth,
                    401,
                    "Cursor 登录态的 access token 已过期，在 Cursor 里用一下让它自己刷新",
                ));
            }
            Ok(ResolvedToken {
                expires_at: jwt_expiry(&login.access_token),
                access_token: login.access_token,
            })
        })
    }
}

// ---------- 存在 nexus-accounts 里的号 ----------

pub struct StoredAccountsSource {
    accounts: Arc<AccountsService>,
    client_type: String,
    /// 按账号缓存 access token，快过期才重新 `session()`——那是一次联网的 refresh，
    /// 不能每个请求都做。
    tokens: Mutex<HashMap<AccountId, ResolvedToken>>,
}

impl StoredAccountsSource {
    pub fn new(accounts: Arc<AccountsService>, client_type: impl Into<String>) -> Self {
        Self {
            accounts,
            client_type: client_type.into(),
            tokens: Mutex::new(HashMap::new()),
        }
    }

    fn cached(&self, id: &AccountId) -> Option<String> {
        let tokens = self.tokens.lock().expect("token cache");
        let t = tokens.get(id)?;
        let fresh = match t.expires_at {
            Some(exp) => exp > SystemTime::now() + Duration::from_secs(60),
            None => true,
        };
        fresh.then(|| t.access_token.clone())
    }
}

impl Source for StoredAccountsSource {
    fn name(&self) -> &'static str {
        "stored"
    }

    fn candidates(&self) -> Vec<Candidate> {
        let list = match self.accounts.repo.list() {
            Ok(l) => l,
            Err(err) => {
                tracing::warn!(%err, "读账号列表失败");
                return Vec::new();
            }
        };
        list.into_iter()
            // 能切号的才进网关：有 refresh，或手上那把 session 还活着。
            // `can_query_usage` 还包含仅有 `crsr_` 的号——那把 key 查基础用量可以，
            // 兑出来的 JWT 登不回 Cursor，不能拿去接力。
            .filter(|a| a.status == Status::Active && a.can_switch())
            .map(|a| Candidate {
                label: a.email,
                kind: CandidateKind::Stored(a.id),
                pinned_machine_id: None,
                quota: quota_hint(a.usage.as_ref(), &self.client_type),
            })
            .collect()
    }

    fn resolve<'a>(
        &'a self,
        candidate: &'a Candidate,
    ) -> BoxFuture<'a, Result<ResolvedToken, UpstreamError>> {
        Box::pin(async move {
            let CandidateKind::Stored(id) = &candidate.kind else {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    500,
                    "候选不属于这个来源",
                ));
            };
            if let Some(access) = self.cached(id) {
                return Ok(ResolvedToken {
                    expires_at: jwt_expiry(&access),
                    access_token: access,
                });
            }
            let session = self.accounts.session(id).await.map_err(|err| {
                let kind = match err.code {
                    ErrorCode::Unauthorized | ErrorCode::SecretMissing => UpstreamKind::Auth,
                    _ => UpstreamKind::Upstream,
                };
                let status = if kind == UpstreamKind::Auth { 401 } else { 502 };
                UpstreamError::new(
                    kind,
                    status,
                    format!("{}：{}", candidate.label, err.message),
                )
            })?;
            let access = session.access_token.expose().to_string();
            let token = ResolvedToken {
                expires_at: jwt_expiry(&access),
                access_token: access.clone(),
            };
            self.tokens
                .lock()
                .expect("token cache")
                .insert(id.clone(), token);
            Ok(ResolvedToken {
                expires_at: jwt_expiry(&access),
                access_token: access,
            })
        })
    }
}

// ---------- ChatGPT 账号 ----------

// ---------- 订阅通道（ChatGPT / Grok / Kiro …）----------

/// 一个订阅平台的账号服务在网关眼里的样子。`nexus-chatgpt` / `nexus-grok` / `nexus-kiro`
/// 各实现一份（见 `subscriptions.rs`），lane 这边只需要一个 [`SubscriptionSource`]。
///
/// 机器码这里没有意义——这些协议的设备标识要么在请求体里、要么根本没有；一律按账号派生。
pub trait SubscriptionAccounts: Send + Sync {
    /// 通道 id（`chatgpt` / `grok` / `kiro`）。
    fn channel(&self) -> &'static str;
    /// 此刻能进队的号：开着、有凭证、没被判失效。`label` 给人看也当键；`id` 是账号表主键。
    fn candidates(&self) -> Vec<SubscriptionCandidate>;
    /// 拿一份能用的 access token（可能联网续期）。
    fn access_token<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, nexus_core::Result<nexus_core::Secret>>;
    /// 从 token 本身读过期时刻（JWT `exp`）；读不出给 `None`。
    fn token_expiry(&self, access_token: &str) -> Option<SystemTime>;
}

#[derive(Debug, Clone)]
pub struct SubscriptionCandidate {
    pub id: String,
    pub label: String,
    pub quota: QuotaHint,
}

pub struct SubscriptionSource {
    accounts: Arc<dyn SubscriptionAccounts>,
}

impl SubscriptionSource {
    pub fn new(accounts: Arc<dyn SubscriptionAccounts>) -> Self {
        Self { accounts }
    }
}

/// 从 ChatGPT 额度快照读线索。任一窗口用满就是耗尽——但只在重置时刻还没到时算：快照会过期。
pub fn chatgpt_quota_hint(usage: Option<&nexus_chatgpt::CodexUsage>) -> QuotaHint {
    let Some(u) = usage else {
        return QuotaHint::default();
    };
    let now_ms = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let exhausted = u
        .exhausted()
        .is_some_and(|e| e.reset_at_ms.is_none_or(|at| at > now_ms));
    QuotaHint {
        percent_used: u.percent_used(),
        exhausted,
    }
}

impl Source for SubscriptionSource {
    fn name(&self) -> &'static str {
        self.accounts.channel()
    }

    fn candidates(&self) -> Vec<Candidate> {
        let channel = self.accounts.channel();
        self.accounts
            .candidates()
            .into_iter()
            .map(|c| Candidate {
                label: c.label,
                quota: c.quota,
                kind: CandidateKind::Subscription { channel, id: c.id },
                pinned_machine_id: None,
            })
            .collect()
    }

    fn resolve<'a>(
        &'a self,
        candidate: &'a Candidate,
    ) -> BoxFuture<'a, Result<ResolvedToken, UpstreamError>> {
        Box::pin(async move {
            let CandidateKind::Subscription { channel, id } = &candidate.kind else {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    500,
                    "候选不属于这个来源",
                ));
            };
            if *channel != self.accounts.channel() {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    500,
                    "候选属于另一条通道",
                ));
            }
            let token = self.accounts.access_token(id).await.map_err(|err| {
                let kind = match err.code {
                    ErrorCode::Unauthorized | ErrorCode::SecretMissing => UpstreamKind::Auth,
                    _ => UpstreamKind::Upstream,
                };
                let status = if kind == UpstreamKind::Auth { 401 } else { 502 };
                UpstreamError::new(
                    kind,
                    status,
                    format!("{}：{}", candidate.label, err.message),
                )
            })?;
            let access = token.expose().to_string();
            Ok(ResolvedToken {
                expires_at: self.accounts.token_expiry(&access),
                access_token: access,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_accounts::usage::BotQuota;

    fn jwt(exp: i64) -> String {
        use base64::Engine;
        let enc = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s.as_bytes());
        format!(
            "{}.{}.sig",
            enc(r#"{"alg":"HS256"}"#),
            enc(&format!(r#"{{"sub":"authkit|user_T","exp":{exp}}}"#))
        )
    }

    fn usage(total: Option<f64>, bot_pct: Option<f64>, bot_avail: Option<bool>) -> AccountUsage {
        AccountUsage {
            total_percent_used: total,
            bot: Some(BotQuota {
                percent_used: bot_pct,
                has_available: bot_avail,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn quota_hint_reads_total_for_user_lanes_and_bot_for_sand() {
        assert_eq!(quota_hint(None, "cli"), QuotaHint::default());
        let u = usage(Some(42.0), Some(100.0), Some(false));
        let cli = quota_hint(Some(&u), "cli");
        assert_eq!(cli.percent_used, Some(42.0));
        assert!(!cli.exhausted, "cli 看总量，不看 bot");
        let sand = quota_hint(Some(&u), "sand");
        assert_eq!(sand.percent_used, Some(100.0));
        assert!(sand.exhausted);

        assert!(quota_hint(Some(&usage(Some(99.5), None, None)), "cli").exhausted);
        assert!(!quota_hint(Some(&usage(Some(99.4), None, None)), "cli").exhausted);
        assert!(
            quota_hint(Some(&usage(None, Some(10.0), Some(false))), "sand").exhausted,
            "has_available=false 就是没了"
        );
    }

    struct FakeReader(Mutex<Option<CursorLogin>>);
    impl LoginReader for FakeReader {
        fn read(&self) -> Option<CursorLogin> {
            self.0.lock().unwrap().clone()
        }
    }

    fn login(exp: i64) -> CursorLogin {
        CursorLogin {
            email: "Me@Example.com".into(),
            access_token: jwt(exp),
            machine_id: Some("a".repeat(64)),
        }
    }

    #[tokio::test]
    async fn cursor_login_source_offers_the_logged_in_account_pinned_to_the_real_machine() {
        let reader = Arc::new(FakeReader(Mutex::new(Some(login(9_999_999_999)))));
        let src = CursorLoginSource::new(reader);
        let cands = src.candidates();
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].kind, CandidateKind::CursorLogin);
        assert_eq!(
            cands[0].pinned_machine_id.as_deref(),
            Some("a".repeat(64).as_str())
        );
        let tok = src.resolve(&cands[0]).await.unwrap();
        assert_eq!(tok.access_token, jwt(9_999_999_999));
        assert!(tok.expires_at.is_some());
    }

    #[tokio::test]
    async fn cursor_login_source_hides_an_expired_token_and_never_refreshes_it() {
        let reader = Arc::new(FakeReader(Mutex::new(Some(login(1)))));
        let src = CursorLoginSource::new(reader);
        assert!(src.candidates().is_empty(), "过期的登录号不是候选");
        let stale = Candidate {
            label: "me@example.com".into(),
            kind: CandidateKind::CursorLogin,
            pinned_machine_id: None,
            quota: QuotaHint::default(),
        };
        let err = src.resolve(&stale).await.unwrap_err();
        assert_eq!(err.kind, UpstreamKind::Auth);
        assert!(err.message.contains("让它自己刷新"));
    }

    #[tokio::test]
    async fn cursor_login_source_caches_reads_but_follows_a_relogin_after_ttl() {
        let reader = Arc::new(FakeReader(Mutex::new(Some(login(9_999_999_999)))));
        let src = CursorLoginSource::new(reader.clone()).with_ttl(Duration::from_millis(30));
        assert_eq!(src.candidates()[0].label, "Me@Example.com");
        *reader.0.lock().unwrap() = Some(CursorLogin {
            email: "other@example.com".into(),
            ..login(9_999_999_999)
        });
        assert_eq!(src.candidates()[0].label, "Me@Example.com", "TTL 内用缓存");
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(
            src.candidates()[0].label,
            "other@example.com",
            "过了 TTL 跟上换号"
        );
        // 换了号之后，旧候选拿不到 token。
        let old = Candidate {
            label: "me@example.com".into(),
            kind: CandidateKind::CursorLogin,
            pinned_machine_id: None,
            quota: QuotaHint::default(),
        };
        assert_eq!(
            src.resolve(&old).await.unwrap_err().kind,
            UpstreamKind::Auth
        );
    }

    #[test]
    fn cursor_login_source_is_empty_when_nobody_is_logged_in() {
        let src = CursorLoginSource::new(Arc::new(FakeReader(Mutex::new(None))));
        assert!(src.candidates().is_empty());
    }

    #[test]
    fn stored_source_lists_only_active_accounts_with_refresh_tokens() {
        use nexus_accounts::model::NewAccount;
        use nexus_store::{Db, MemorySecrets};
        let svc = Arc::new(AccountsService::new(
            Arc::new(Db::open_in_memory().unwrap()),
            Arc::new(MemorySecrets::new()),
        ));
        svc.repo
            .upsert(NewAccount {
                email: "active@x.com".into(),
                refresh_token: Some("rt".into()),
                ..Default::default()
            })
            .unwrap();
        svc.repo
            .upsert(NewAccount {
                email: "needslogin@x.com".into(),
                cursor_password: Some("pw".into()),
                ..Default::default()
            })
            .unwrap();
        svc.repo
            .upsert(NewAccount {
                email: "apikey@x.com".into(),
                api_key: Some("crsr_abc123DEF".into()),
                ..Default::default()
            })
            .unwrap();
        let src = StoredAccountsSource::new(svc, "cli");
        let cands = src.candidates();
        assert_eq!(cands.len(), 1);
        assert_eq!(cands[0].label, "active@x.com");
        assert!(matches!(cands[0].kind, CandidateKind::Stored(_)));
        assert!(
            cands[0].pinned_machine_id.is_none(),
            "托管号按账号派生机器码"
        );
    }

    #[test]
    fn real_cursor_reader_reads_a_temp_state_db_and_storage_json() {
        // 用真的 nexus_cursor::Cursor 对着临时目录读：登录态库 + storage.json 都是我们造的。
        let dir = tempfile::tempdir().unwrap();
        let gs = dir.path().join("User/globalStorage");
        std::fs::create_dir_all(&gs).unwrap();
        let conn = rusqlite::Connection::open(gs.join("state.vscdb")).unwrap();
        conn.execute_batch(
            "CREATE TABLE ItemTable (key TEXT UNIQUE ON CONFLICT REPLACE, value BLOB);",
        )
        .unwrap();
        let token = jwt(9_999_999_999);
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES (?1, ?2), (?3, ?4)",
            rusqlite::params![
                "cursorAuth/accessToken",
                token,
                "cursorAuth/cachedEmail",
                "Who@Example.com"
            ],
        )
        .unwrap();
        std::fs::write(
            gs.join("storage.json"),
            format!(r#"{{"telemetry.machineId":"{}"}}"#, "b".repeat(64)),
        )
        .unwrap();

        let cursor = nexus_cursor::Cursor::at(dir.path());
        let login = cursor.read().expect("能读到登录态");
        assert_eq!(
            login.email, "who@example.com",
            "nexus-cursor 把邮箱统一成小写"
        );
        assert_eq!(login.access_token, token);
        assert_eq!(login.machine_id.as_deref(), Some("b".repeat(64).as_str()));
    }
}
