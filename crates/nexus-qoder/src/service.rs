//! Qoder 账号用例：贴 PAT、启停、取能签名的 job token。
//!
//! 导入时就把 PAT 换成 job token 并问一次 userinfo。换不成的 PAT 不入库——
//! 否则列表里会出现一个永远发不出请求的号。短票过期后再拿 PAT 换，不走刷新接口。

use crate::auth;
use crate::model::{QoderAccount, QoderBackend, QoderIdentity, QoderStatus};
use crate::protocol;
use crate::repo::{QoderAccounts, StoredPat, Upserted};
use nexus_core::{AppError, ErrorCode, QoderAccountId, Result, Secret};
use nexus_store::{Db, QoderSecret, SecretStore};
use std::sync::Arc;
use std::time::Duration;

pub struct QoderService {
    pub repo: QoderAccounts,
    http: reqwest::Client,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub accounts: Vec<QoderAccount>,
    pub created: usize,
    pub updated: usize,
    pub skipped: Vec<String>,
}

impl QoderService {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self {
            repo: QoderAccounts::new(db, secrets),
            http,
        }
    }

    pub fn models(&self) -> Vec<String> {
        protocol::catalog()
    }

    pub fn list(&self) -> Result<Vec<QoderAccount>> {
        self.repo.list()
    }

    pub fn get(&self, id: &QoderAccountId) -> Result<QoderAccount> {
        self.repo.get(id)
    }

    pub fn remove(&self, id: &QoderAccountId) -> Result<()> {
        self.repo.remove(id)
    }

    pub fn set_enabled(&self, id: &QoderAccountId, enabled: bool) -> Result<QoderAccount> {
        self.repo.set_enabled(id, enabled)
    }

    pub fn set_note(&self, id: &QoderAccountId, note: Option<&str>) -> Result<QoderAccount> {
        self.repo.set_note(id, note)
    }

    pub fn identity_of_label(&self, label: &str) -> Option<QoderIdentity> {
        let account = self.repo.by_label(label).ok()??;
        Some(self.repo.identity_of(&account))
    }

    /// 聊天被拒（登录过期）之后调用。下一轮取 token 会重新交换 PAT。
    pub fn invalidate_label(&self, label: &str) {
        let Ok(Some(account)) = self.repo.by_label(label) else {
            return;
        };
        let _ = self.repo.expire_token(&account.id);
        let _ = self.repo.record_failure(
            &account.id,
            "Qoder 拒绝了这把登录态",
            Some(QoderStatus::NeedsLogin),
        );
    }

    /// 一行一个 PAT。`backend` 是默认边；某一行写成 `cn …` 或 `global …` 就改这一行。
    pub async fn import_text(
        &self,
        text: &str,
        backend: QoderBackend,
        note: Option<&str>,
    ) -> Result<ImportReport> {
        let lines = parse_lines(text, backend)?;
        let mut report = ImportReport {
            accounts: Vec::new(),
            created: 0,
            updated: 0,
            skipped: Vec::new(),
        };
        for (line_backend, pat) in lines {
            match self.exchange_and_store(line_backend, &pat, note).await {
                Ok(Upserted { account, created }) => {
                    if created {
                        report.created += 1;
                    } else {
                        report.updated += 1;
                    }
                    report.accounts.push(account);
                }
                Err(err) => report.skipped.push(err.message),
            }
        }
        if report.accounts.is_empty() {
            let why = report.skipped.join("；");
            return Err(AppError::invalid(if why.is_empty() {
                "没有导入到任何 Qoder 账号。".to_string()
            } else {
                format!("没有导入到任何 Qoder 账号：{why}")
            })
            .with_hint(
                "到 qoder.com/account/integrations（国内版是 qoder.com.cn）创建一把 Personal Access Token，整行贴进来。",
            ));
        }
        Ok(report)
    }

    async fn exchange_and_store(
        &self,
        backend: QoderBackend,
        pat: &str,
        note: Option<&str>,
    ) -> Result<Upserted> {
        let exchanged = auth::exchange_pat(&self.http, backend, pat)
            .await
            .map_err(|message| {
                AppError::invalid(message)
                    .with_hint("确认这把 PAT 还有效，并且选对了国际版 / 国内版。")
            })?;
        if exchanged.user_id.trim().is_empty() {
            return Err(AppError::invalid(
                "PAT 换到了 job token，但没有拿到 user id。没有 user id 签不出聊天请求。",
            ));
        }
        let machine_id = uuid::Uuid::new_v4().to_string();
        self.repo.upsert(
            &StoredPat {
                backend,
                pat: pat.to_string(),
                user_id: exchanged.user_id,
                email: exchanged.email,
                name: exchanged.name,
                machine_id,
                job_token: exchanged.job_token,
                job_refresh: exchanged.job_refresh,
                expires_at: exchanged.expires_at,
            },
            note,
        )
    }

    /// 网关要的短票。快过期就用 PAT 再换一把。
    pub async fn credential(&self, id: &QoderAccountId) -> Result<Secret> {
        let account = self.repo.get(id)?;
        if auth::expired(account.access_expires_at.as_deref()) {
            self.refresh(&account).await?;
        }
        match self.repo.secret(id, QoderSecret::JobToken)? {
            Some(token) if !token.expose().is_empty() => Ok(token),
            _ => {
                let _ =
                    self.repo
                        .record_failure(id, "没有 job token", Some(QoderStatus::NeedsLogin));
                Err(AppError::new(
                    ErrorCode::SecretMissing,
                    "这个 Qoder 账号没有可用的登录态。",
                )
                .with_hint("重新贴一次 Personal Access Token。"))
            }
        }
    }

    async fn refresh(&self, account: &QoderAccount) -> Result<()> {
        let pat = self
            .repo
            .secret(&account.id, QoderSecret::Pat)?
            .filter(|s| !s.expose().is_empty())
            .ok_or_else(|| {
                let _ = self.repo.record_failure(
                    &account.id,
                    "PAT 不在了",
                    Some(QoderStatus::NeedsLogin),
                );
                AppError::new(ErrorCode::SecretMissing, "这个 Qoder 账号的 PAT 不在了。")
                    .with_hint("重新贴一次 Personal Access Token。")
            })?;
        let exchanged = auth::exchange_pat(&self.http, account.backend, pat.expose())
            .await
            .map_err(|message| {
                let _ =
                    self.repo
                        .record_failure(&account.id, &message, Some(QoderStatus::NeedsLogin));
                AppError::invalid(message)
            })?;
        let machine_id = self.repo.identity_of(account).machine_id;
        self.repo.store_secrets(
            &account.id,
            &StoredPat {
                backend: account.backend,
                pat: pat.expose().to_string(),
                user_id: if exchanged.user_id.is_empty() {
                    account.user_id.clone().unwrap_or_default()
                } else {
                    exchanged.user_id
                },
                email: if exchanged.email.is_empty() {
                    account.email.clone().unwrap_or_default()
                } else {
                    exchanged.email
                },
                name: if exchanged.name.is_empty() {
                    account.display_name.clone().unwrap_or_default()
                } else {
                    exchanged.name
                },
                machine_id,
                job_token: exchanged.job_token,
                job_refresh: exchanged.job_refresh,
                expires_at: exchanged.expires_at,
            },
        )
    }
}

fn parse_lines(text: &str, default_backend: QoderBackend) -> Result<Vec<(QoderBackend, String)>> {
    let mut out = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (backend, token) = match line.split_once(char::is_whitespace) {
            Some((head, rest))
                if matches!(
                    head.to_ascii_lowercase().as_str(),
                    "cn" | "global" | "qoder-cn"
                ) =>
            {
                (QoderBackend::parse(head), rest.trim())
            }
            _ => (default_backend, line),
        };
        if token.chars().any(char::is_whitespace) || token.chars().count() < 8 {
            return Err(AppError::invalid("每一行应该是一把 PAT，不要夹空格。").with_hint(
                "国际版在 qoder.com/account/integrations 创建，国内版在 qoder.com.cn 的同一页。",
            ));
        }
        out.push((backend, token.to_string()));
    }
    if out.is_empty() {
        return Err(AppError::invalid("没有贴到 PAT。").with_hint(
            "国际版在 qoder.com/account/integrations 创建，国内版在 qoder.com.cn 的同一页。",
        ));
    }
    Ok(out)
}
