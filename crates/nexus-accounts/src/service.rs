//! 把仓库、刷新、用量串起来的那一层。
//!
//! 上层（Tauri 命令）只跟它打交道，不用自己记「先刷 token 再查用量、刷出来的新
//! refresh 要存回去」这套顺序。

use crate::billing::{self, AccountBilling};
use crate::model::{Account, NewAccount, Source};
use crate::oauth::OauthTokens;
use crate::repo::Accounts;
use crate::token::{self, RefreshedSession};
use crate::usage::{self, AccountUsage};
use nexus_core::{AccountId, AppError, ErrorCode, Result};
use nexus_store::keys::AccountSecret;
use nexus_store::{Db, SecretStore};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// 批量刷号时，两个号之间的间隔。
///
/// 一个号要打 7 个请求（1 次 token 交换 + 6 个 dashboard 接口）。几十个号连着不喘气地发，
/// 就是几百个请求从同一个 IP 冲出去——被限流、被当成抓取，都是从这儿来的。歇一下的代价
/// 是几十个号多花十几秒，而这本来就是个后台动作。
const ACCOUNT_GAP: Duration = Duration::from_millis(350);

/// 单飞闸的守卫。析构即放闸，中途 `?` 提前返回也不会把闸卡死。
#[derive(Debug)]
struct RunGuard<'a>(&'a AtomicBool);

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

pub struct AccountsService {
    pub repo: Accounts,
    http: reqwest::Client,
    /// 批量刷的单飞闸。前端的 busy 标记是组件局部的（切个 tab 再回来就重置了），
    /// 拦不住第二次点击；两批同时跑等于把请求量翻倍，正是最该避免的那种突发。
    refreshing: AtomicBool,
}

impl AccountsService {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            repo: Accounts::new(db, secrets),
            http: usage::http_client(),
            refreshing: AtomicBool::new(false),
        }
    }

    /// 拿一把能用的会话：**先看手上那把还有效没有**，没有才去换。
    ///
    /// 原先每次都无条件换一次 token，于是查一次用量固定是 7 个请求。access token 本身
    /// 有效期不短，存都存了却不用，等于白发一半的请求。
    pub async fn session(&self, id: &AccountId) -> Result<RefreshedSession> {
        if let Some(reused) = self.reusable_session(id)? {
            return Ok(reused);
        }
        self.exchange(id).await
    }

    /// 手上那把会话还能不能直接用。没 access、或 access 过期就当没有，交给调用方去换。
    ///
    /// refresh 不是必需的：只靠 session token 撑着的号就是没有它，有效期内照样能拼会话。
    fn reusable_session(&self, id: &AccountId) -> Result<Option<RefreshedSession>> {
        let account = self.repo.get(id)?;
        let Some(access) = self.repo.secret(id, AccountSecret::Access)? else {
            return Ok(None);
        };
        // 老库里 OAuth 进来的号可能没落 user_id；JWT 的 sub 里有，算一个出来比放弃复用强。
        let user_id = match account.workos_user_id.as_deref() {
            Some(u) => u.to_string(),
            None => match token::extract_user_id(access.expose()) {
                Some(u) => u,
                None => return Ok(None),
            },
        };
        let refresh = self.repo.secret(id, AccountSecret::Refresh)?;
        Ok(token::reuse_session(&user_id, &access, refresh.as_ref()))
    }

    /// 用这个号手上那把 access 铸一把长期 `crsr_` API Key，落库。
    ///
    /// **这是「只有一把 session token」的号唯一的保命动作。** 那批号没有 refresh、接不了
    /// 验证码，授权链整条走不通；access 的 `exp` 一到，这个号就再也拿不回来了。趁它还活着
    /// 铸一把 `crsr_`，之后查用量、进网关、走 CRSR 通道都不再依赖那把会死的 access。
    ///
    /// 复用手上那把 access（`session()`）而不是强制换新：仅会话的号本来就换不出新的，而
    /// 有 refresh 的号也没必要为铸一把 key 多轮换一次 refresh。
    pub async fn mint_api_key(&self, id: &AccountId) -> Result<token::MintedApiKeyInfo> {
        let session = self.session(id).await?;
        let minted = token::mint_user_api_key(
            &self.http,
            session.access_token.expose(),
            token::DEFAULT_API_KEY_NAME,
        )
        .await?;
        self.repo
            .put_secret(id, AccountSecret::ApiKey, Some(minted.api_key.expose()))?;
        Ok(minted.info())
    }

    /// 强制换一把新的，不复用。
    ///
    /// 给「要把 token 交出去」的场合用 —— 典型是写进 Cursor 的登录态。复用的那把可能只剩
    /// 一分钟寿命，交出去等于让 Cursor 一启动就得先去续期，而那正是用户在切号的当口。
    /// 这类操作本来就不频繁，多发一个请求换一把足寿的 token 是划算的。
    pub async fn fresh_session(&self, id: &AccountId) -> Result<RefreshedSession> {
        self.exchange(id).await
    }

    /// 真去换一把新的 session token。
    ///
    /// 顺带把两件事落库：Cursor 轮换过的 refresh_token（不存回去，下次刷新就废了），
    /// 以及 `user_xxx`（拼 cookie 用，不算秘密）。
    async fn exchange(&self, id: &AccountId) -> Result<RefreshedSession> {
        let account = self.repo.get(id)?;
        let Some(refresh) = self.repo.secret(id, AccountSecret::Refresh)? else {
            if account.has_access {
                if account.has_live_access() {
                    // 调用方要的是「一把全新的」（踢会话后验活、有 refresh 时写进 Cursor）。
                    // 仅会话的号给不了新的，切号那条路应走 `session()` 复用手上这把。
                    // 不是号坏了，别记失败。
                    return Err(AppError::new(
                        ErrorCode::SecretMissing,
                        format!("{} 只有 session token，换不出新会话。", account.email),
                    )
                    .with_hint("这个操作需要 refresh_token；用密码授权一次就有了。"));
                }
                // 只靠 session token 撑着的号，token 到期了。这是预期内的到期，不是号坏了：
                // 退回待登录，让人粘一份新的。
                let err = AppError::new(
                    ErrorCode::Unauthorized,
                    format!("{} 的 session token 已过期。", account.email),
                )
                .with_hint("这个号没有 refresh_token，续不了；到凭证页粘一份新的 session token、crsr_ API Key，或用密码授权一次拿到 refresh_token。");
                // 还有 crsr_ 时别退回待登录：切号这条路确实走不通，但基础用量还能查。
                if !account.has_api_key {
                    self.repo.record_failure(id, &err.message, true)?;
                }
                return Err(err);
            }
            return Err(AppError::new(
                ErrorCode::SecretMissing,
                format!("{} 还没有 refresh_token。", account.email),
            )
            .with_hint("点「登录」走一次授权就能拿到。"));
        };

        let session = match token::refresh_to_session(&self.http, refresh.expose()).await {
            Ok(s) => s,
            Err(err) => {
                let fatal = err.code == ErrorCode::Unauthorized;
                self.repo.record_failure(id, &err.message, fatal)?;
                return Err(err);
            }
        };

        if let Some(next) = session.refresh_token.as_ref() {
            if next.expose() != refresh.expose() {
                self.repo
                    .put_secret(id, AccountSecret::Refresh, Some(next.expose()))?;
            }
        }
        self.repo.put_secret(
            id,
            AccountSecret::Access,
            Some(session.access_token.expose()),
        )?;
        if account.workos_user_id.as_deref() != Some(session.user_id.as_str()) {
            self.repo.set_workos_user_id(id, &session.user_id)?;
        }
        Ok(session)
    }

    /// 刷一个号的用量，并写回库。
    ///
    /// 复用的会话被拒时**换一把新的重试一次**再下结论：那多半只说明这把会话提前失效了，
    /// 不代表 refresh_token 废了。少了这一步，省请求这件事就会以「偶尔误判一个号已失效」
    /// 为代价——那比多发一个请求糟得多。
    ///
    /// `day_start_ms` 是调用方（前端）的本地零点：给了才有「今天 / 近 7 天」两个时间窗。
    pub async fn refresh_usage(
        &self,
        id: &AccountId,
        day_start_ms: Option<i64>,
    ) -> Result<AccountUsage> {
        let account = self.repo.get(id)?;
        let can_session = account.has_refresh || account.has_live_access();

        if can_session {
            match self.dashboard_usage(id, day_start_ms).await {
                Ok(u) => {
                    self.repo.record_usage(id, &u)?;
                    return Ok(u);
                }
                Err(err) => {
                    if !account.has_api_key {
                        return Err(err);
                    }
                    tracing::info!(
                        email = %account.email,
                        error = %err.message,
                        "dashboard 用量失败，改走 crsr_ API Key"
                    );
                }
            }
        }

        if account.has_api_key {
            return self.api_key_usage(id, day_start_ms).await;
        }

        if !can_session {
            // 没有会话、也没有 API Key：把原因说清，别绕去 exchange 报「缺 refresh」。
            if account.session_only() {
                let err = AppError::new(
                    ErrorCode::Unauthorized,
                    format!("{} 的 session token 已过期。", account.email),
                )
                .with_hint("这个号没有 refresh_token，续不了；到凭证页粘一份新的 session token、crsr_ API Key，或用密码授权一次。");
                self.repo.record_failure(id, &err.message, true)?;
                return Err(err);
            }
            return Err(AppError::new(
                ErrorCode::SecretMissing,
                format!("{} 还没有 refresh_token。", account.email),
            )
            .with_hint("点「登录」走一次授权，或到凭证页填一把 crsr_ API Key 查基础用量。"));
        }

        Err(AppError::upstream("拉取用量失败。"))
    }

    async fn dashboard_usage(
        &self,
        id: &AccountId,
        day_start_ms: Option<i64>,
    ) -> Result<AccountUsage> {
        let session = self.session(id).await?;
        let mut outcome =
            usage::fetch(&self.http, session.session_token.expose(), day_start_ms).await;

        if session.reused && matches!(&outcome, Err(e) if e.code == ErrorCode::Unauthorized) {
            if session.refresh_token.is_none() {
                let err = AppError::unauthorized("session token 已被上游拒绝，需要重新粘一份。")
                    .with_hint(
                        "这个号没有 refresh_token；到凭证页更新 session token，或用密码授权一次。",
                    );
                self.repo.record_failure(id, &err.message, true)?;
                return Err(err);
            }
            let fresh = self.exchange(id).await?;
            outcome = usage::fetch(&self.http, fresh.session_token.expose(), day_start_ms).await;
        }

        match outcome {
            Ok(u) => Ok(u),
            Err(err) => {
                self.repo
                    .record_failure(id, &err.message, err.code == ErrorCode::Unauthorized)?;
                Err(err)
            }
        }
    }

    async fn api_key_usage(
        &self,
        id: &AccountId,
        day_start_ms: Option<i64>,
    ) -> Result<AccountUsage> {
        let key = self.repo.require_secret(id, AccountSecret::ApiKey)?;
        match usage::fetch_via_api_key(&self.http, key.expose(), day_start_ms).await {
            Ok(u) => {
                self.repo.record_usage(id, &u)?;
                Ok(u)
            }
            Err(err) => {
                self.repo
                    .record_failure(id, &err.message, err.code == ErrorCode::Unauthorized)?;
                Err(err)
            }
        }
    }

    /// 刷一个号的订阅账单（标价 / 折扣 / 发票），并写回库。
    ///
    /// 复用会话被拒时的重试规矩跟 `refresh_usage` 一样：先换一把再下结论，
    /// 免得一把提前失效的 access 把号误判成已失效。门户读失败（没有个人账单、
    /// 页面改了）不是凭证废了，不记 fatal。
    pub async fn refresh_billing(&self, id: &AccountId) -> Result<AccountBilling> {
        let session = self.session(id).await?;
        let mut outcome = billing::fetch(&self.http, session.session_token.expose()).await;

        if session.reused && matches!(&outcome, Err(e) if e.code == ErrorCode::Unauthorized) {
            if session.refresh_token.is_none() {
                let err = AppError::unauthorized("session token 已被上游拒绝，需要重新粘一份。")
                    .with_hint(
                        "这个号没有 refresh_token；到凭证页更新 session token，或用密码授权一次。",
                    );
                self.repo.record_failure(id, &err.message, true)?;
                return Err(err);
            }
            let fresh = self.exchange(id).await?;
            outcome = billing::fetch(&self.http, fresh.session_token.expose()).await;
        }

        match outcome {
            Ok(b) => {
                self.repo.record_billing(id, &b)?;
                Ok(b)
            }
            Err(err) => {
                if err.code == ErrorCode::Unauthorized {
                    self.repo.record_failure(id, &err.message, true)?;
                }
                Err(err)
            }
        }
    }

    /// 改按需计费，成功后再刷一遍用量，让抽屉立刻看到新上限。
    pub async fn set_on_demand(
        &self,
        id: &AccountId,
        enabled: bool,
        limit_cents: Option<f64>,
        day_start_ms: Option<i64>,
    ) -> Result<AccountUsage> {
        let session = self.session(id).await?;
        let mut outcome = usage::set_on_demand(
            &self.http,
            session.session_token.expose(),
            enabled,
            limit_cents,
        )
        .await;

        if session.reused && matches!(&outcome, Err(e) if e.code == ErrorCode::Unauthorized) {
            if session.refresh_token.is_none() {
                let err = AppError::unauthorized("session token 已被上游拒绝，需要重新粘一份。")
                    .with_hint(
                        "这个号没有 refresh_token；到凭证页更新 session token，或用密码授权一次。",
                    );
                self.repo.record_failure(id, &err.message, true)?;
                return Err(err);
            }
            let fresh = self.exchange(id).await?;
            outcome = usage::set_on_demand(
                &self.http,
                fresh.session_token.expose(),
                enabled,
                limit_cents,
            )
            .await;
        }

        outcome?;
        self.refresh_usage(id, day_start_ms).await
    }

    /// 批量刷。**一个失败不影响其余**——刷一批号时中途报错整批停掉是最没用的行为。
    ///
    /// 串行而不是并发：六个 dashboard 接口本身已经是并发的，再叠一层会被 Cursor 限流。
    /// 号与号之间还要再歇 `ACCOUNT_GAP`，把一次「全刷」摊平成细水长流而不是一记突发。
    pub async fn refresh_all(
        &self,
        ids: &[AccountId],
        day_start_ms: Option<i64>,
        on_each: &(dyn Fn(&AccountId, std::result::Result<(), &AppError>) + Sync),
    ) -> Result<Vec<(AccountId, Result<AccountUsage>)>> {
        let _guard = self.begin_refresh()?;
        let mut out = Vec::with_capacity(ids.len());
        for (i, id) in ids.iter().enumerate() {
            if i > 0 {
                tokio::time::sleep(ACCOUNT_GAP).await;
            }
            let result = self.refresh_usage(id, day_start_ms).await;
            match &result {
                Ok(_) => on_each(id, Ok(())),
                Err(err) => on_each(id, Err(err)),
            }
            out.push((id.clone(), result));
        }
        Ok(out)
    }

    fn begin_refresh(&self) -> Result<RunGuard<'_>> {
        if self.refreshing.swap(true, Ordering::SeqCst) {
            return Err(AppError::new(ErrorCode::Busy, "已经有一批用量在刷了。")
                .with_hint("等这一批跑完再点；同时刷两批只会更慢，还容易被上游限流。"));
        }
        Ok(RunGuard(&self.refreshing))
    }

    /// OAuth 拿到 token 之后的落点：存 refresh，账号转「可用」。
    ///
    /// 邮箱从 access_token 里读不到（Cursor 的 JWT 只带 user id），所以由调用方给——
    /// 通常是用户在弹窗里填的那个。
    pub async fn complete_oauth(&self, email: &str, tokens: &OauthTokens) -> Result<Account> {
        let account = self.repo.upsert(NewAccount {
            email: email.to_string(),
            refresh_token: Some(tokens.refresh_token.expose().to_string()),
            source: Some(Source::Local),
            ..Default::default()
        })?;
        self.repo.put_secret(
            &account.id,
            AccountSecret::Access,
            Some(tokens.access_token.expose()),
        )?;
        if let Some(user_id) = token::extract_user_id(tokens.access_token.expose()) {
            self.repo.set_workos_user_id(&account.id, &user_id)?;
        }
        self.repo.get(&account.id)
    }

    /// 列出这个号在 Cursor 侧的活跃会话。
    pub async fn list_sessions(
        &self,
        id: &AccountId,
    ) -> Result<Vec<crate::sessions::ActiveSession>> {
        let session = self.session(id).await?;
        let current = token::extract_session_id(session.access_token.expose());
        crate::sessions::list_active(
            &self.http,
            session.access_token.expose(),
            current.as_deref(),
        )
        .await
    }

    /// 踢掉其它活跃会话，尽量保留 Nexus 手里这把。
    ///
    /// 对不上当前会话 id 时会全踢，然后验一把 refresh：还活着就继续用，废了就标
    /// 「需要重新授权」。
    pub async fn kick_other_sessions(
        &self,
        id: &AccountId,
    ) -> Result<crate::sessions::KickOutcome> {
        let session = self.fresh_session(id).await?;
        let current = token::extract_session_id(session.access_token.expose());
        let (mut outcome, _) = crate::sessions::kick_others(
            &self.http,
            session.access_token.expose(),
            current.as_deref(),
        )
        .await?;

        // 验 refresh：全踢或踢错当前时，refresh 可能一起废。（`fresh_session` 走的是真交换，
        // 一定带 refresh；这里的兜底只是不让类型上的 Option 变成一个 unwrap。）
        let Some(refresh) = session.refresh_token.as_ref() else {
            return Ok(outcome);
        };
        let still = token::refresh_to_session(&self.http, refresh.expose()).await;
        match still {
            Ok(next) => {
                outcome.refresh_alive = true;
                if let Some(rotated) = next.refresh_token.as_ref() {
                    if rotated.expose() != refresh.expose() {
                        self.repo
                            .put_secret(id, AccountSecret::Refresh, Some(rotated.expose()))?;
                    }
                }
                self.repo.put_secret(
                    id,
                    AccountSecret::Access,
                    Some(next.access_token.expose()),
                )?;
            }
            Err(err) if err.code == ErrorCode::Unauthorized => {
                outcome.refresh_alive = false;
                self.repo.record_failure(
                    id,
                    "踢会话后 refresh_token 已失效，需要重新授权。",
                    true,
                )?;
            }
            Err(err) => {
                // 网络抖动别误判成废号——没拿到明确的 unauthorized 就当还活着。
                tracing::warn!(error = %err.message, "踢会话后校验 refresh 失败（未判死）");
            }
        }

        Ok(outcome)
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_store::MemorySecrets;

    fn service() -> AccountsService {
        AccountsService::new(
            Arc::new(Db::open_in_memory().unwrap()),
            Arc::new(MemorySecrets::new()),
        )
    }

    #[tokio::test]
    async fn an_account_without_a_refresh_token_says_what_to_do_next() {
        let svc = service();
        let a = svc
            .repo
            .upsert(NewAccount {
                email: "a@example.com".into(),
                cursor_password: Some("pw".into()),
                ..Default::default()
            })
            .unwrap();
        let err = svc.session(&a.id).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::SecretMissing);
        assert!(err.hint.unwrap().contains("授权"));
    }

    #[tokio::test]
    async fn a_missing_account_reports_account_not_found() {
        let svc = service();
        let err = svc.session(&AccountId::from_raw("nope")).await.unwrap_err();
        assert_eq!(err.code, ErrorCode::AccountNotFound);
    }

    #[tokio::test]
    async fn completing_oauth_creates_an_active_account() {
        let svc = service();
        let tokens = OauthTokens {
            access_token: nexus_core::Secret::new("at"),
            refresh_token: nexus_core::Secret::new("rt"),
            auth_id: None,
        };
        let a = svc
            .complete_oauth("New@Example.com", &tokens)
            .await
            .unwrap();
        assert_eq!(a.email, "new@example.com");
        assert_eq!(a.status, crate::model::Status::Active);
        assert!(a.has_refresh);
        assert_eq!(
            svc.repo
                .require_secret(&a.id, AccountSecret::Refresh)
                .unwrap()
                .expose(),
            "rt"
        );
    }

    #[tokio::test]
    async fn completing_oauth_on_an_existing_account_upgrades_it_in_place() {
        let svc = service();
        let before = svc
            .repo
            .upsert(NewAccount {
                email: "a@example.com".into(),
                cursor_password: Some("pw".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(before.status, crate::model::Status::NeedsLogin);

        let tokens = OauthTokens {
            access_token: nexus_core::Secret::new("at"),
            refresh_token: nexus_core::Secret::new("rt"),
            auth_id: None,
        };
        let after = svc.complete_oauth("a@example.com", &tokens).await.unwrap();
        assert_eq!(after.id, before.id, "不该建出第二个号");
        assert_eq!(after.status, crate::model::Status::Active);
        assert!(after.has_password, "原有的密码要留着");
    }

    #[tokio::test]
    async fn refresh_all_keeps_going_past_a_failure() {
        let svc = service();
        // 两个都没有 refresh，两个都会失败——重点是第二个也被处理到了。
        let a = svc
            .repo
            .upsert(NewAccount {
                email: "a@example.com".into(),
                cursor_password: Some("pw".into()),
                ..Default::default()
            })
            .unwrap();
        let b = svc
            .repo
            .upsert(NewAccount {
                email: "b@example.com".into(),
                cursor_password: Some("pw".into()),
                ..Default::default()
            })
            .unwrap();

        let seen = std::sync::Mutex::new(Vec::new());
        let out = svc
            .refresh_all(&[a.id.clone(), b.id.clone()], None, &|id, r| {
                seen.lock().unwrap().push((id.clone(), r.is_ok()))
            })
            .await
            .expect("闸没人占着，这一批该跑起来");

        assert_eq!(out.len(), 2);
        assert_eq!(
            seen.lock().unwrap().len(),
            2,
            "第一个失败不该让第二个被跳过"
        );
        assert!(out.iter().all(|(_, r)| r.is_err()));
    }

    #[tokio::test]
    async fn a_second_batch_is_refused_while_one_is_still_running() {
        // 前端的 busy 标记是组件局部的（切个 tab 再回来就重置了），拦不住第二次点击；
        // 两批同时跑等于把请求量翻倍，正是最该避免的那种突发。
        let svc = service();
        let guard = svc.begin_refresh().expect("第一批该拿到闸");

        let err = svc.begin_refresh().unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
        assert!(err.hint.is_some(), "被拒了要说下一步怎么办");

        // 守卫析构即放闸，中途出错提前返回也不会把闸卡死。
        drop(guard);
        assert!(svc.begin_refresh().is_ok(), "闸没放开");
    }

    #[tokio::test]
    async fn an_empty_batch_still_releases_the_gate() {
        let svc = service();
        assert!(svc
            .refresh_all(&[], None, &|_, _| {})
            .await
            .unwrap()
            .is_empty());
        assert!(svc.begin_refresh().is_ok());
    }
}
