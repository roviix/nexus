//! ZCode 账号用例：导入、启停、取凭证。
//!
//! 这里没有「刷新」：编码套餐的 API key 是永久的，体验套餐的 JWT 连 `exp` 都没有
//! （官方客户端不按时效拒绝，八天前签发的照样能用）。凭证失效只会以上游 401 的形式
//! 出现，那时候要做的是让用户在官方客户端重登一次再导入，而不是在这里续期。

use crate::import::{self, ImportedAccount};
use crate::model::{ZcodeAccount, ZcodePlan, ZcodeProvider, ZcodeStatus};
use crate::protocol;
use crate::repo::{Upserted, ZcodeAccounts};
use nexus_core::{AppError, ErrorCode, Result, Secret, ZcodeAccountId};
use nexus_store::{Db, SecretStore, ZcodeSecret};
use std::path::PathBuf;
use std::sync::Arc;

pub struct ZcodeService {
    pub repo: ZcodeAccounts,
}

/// 一次导入的结果。逐条报，因为一份凭证文件里常常同时有个人版、团队版和体验套餐。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub accounts: Vec<ZcodeAccount>,
    pub created: usize,
    pub updated: usize,
    /// 跳过的条目（解不开、形状不对），给用户一个能看懂的交代。
    pub skipped: Vec<String>,
}

impl ZcodeService {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            repo: ZcodeAccounts::new(db, secrets),
        }
    }

    pub fn models(&self) -> Vec<String> {
        protocol::catalog()
    }

    pub fn list(&self) -> Result<Vec<ZcodeAccount>> {
        self.repo.list()
    }

    pub fn get(&self, id: &ZcodeAccountId) -> Result<ZcodeAccount> {
        self.repo.get(id)
    }

    pub fn remove(&self, id: &ZcodeAccountId) -> Result<()> {
        self.repo.remove(id)
    }

    pub fn set_enabled(&self, id: &ZcodeAccountId, enabled: bool) -> Result<ZcodeAccount> {
        self.repo.set_enabled(id, enabled)
    }

    pub fn set_note(&self, id: &ZcodeAccountId, note: Option<&str>) -> Result<ZcodeAccount> {
        self.repo.set_note(id, note)
    }

    /// 本机官方客户端的凭证文件在哪。界面上要显示这个路径，找不到时用户才知道去哪看。
    pub fn client_credentials_path(&self) -> PathBuf {
        import::credentials_path()
    }

    pub fn client_credentials_present(&self) -> bool {
        self.client_credentials_path().is_file()
    }

    /// 从本机官方 ZCode 客户端导入。
    pub fn import_from_client(&self, note: Option<&str>) -> Result<ImportReport> {
        let path = self.client_credentials_path();
        let found = import::read_local(&path)?;
        self.absorb(found, note.or(Some("来自本机 ZCode 客户端")))
    }

    /// 粘贴导入：一行 `{apiKeyId}.{apiKeySecret}`、一个 JWT，或一整份 credentials.json。
    pub fn import_text(
        &self,
        text: &str,
        provider: ZcodeProvider,
        note: Option<&str>,
    ) -> Result<ImportReport> {
        let found = import::parse_pasted(text, provider)?;
        self.absorb(found, note)
    }

    fn absorb(&self, found: Vec<ImportedAccount>, note: Option<&str>) -> Result<ImportReport> {
        let mut report = ImportReport {
            accounts: Vec::new(),
            created: 0,
            updated: 0,
            skipped: Vec::new(),
        };
        for item in found {
            match self.repo.upsert(&item, note) {
                Ok(Upserted { account, created }) => {
                    if created {
                        report.created += 1;
                    } else {
                        report.updated += 1;
                    }
                    report.accounts.push(account);
                }
                Err(err) => {
                    // 一条坏的不该挡住好的：多档套餐是常态，逐条记下来交给界面显示。
                    tracing::warn!(%err, "这条 ZCode 凭证存不进去");
                    report.skipped.push(err.message);
                }
            }
        }
        if report.accounts.is_empty() {
            let why = report.skipped.join("；");
            return Err(AppError::invalid(if why.is_empty() {
                "没有导入到任何 ZCode 账号。".to_string()
            } else {
                format!("没有导入到任何 ZCode 账号：{why}")
            }));
        }
        Ok(report)
    }

    /// 取这个号发请求要用的凭证。编码套餐给 API key，体验套餐给 JWT。
    ///
    /// 是 async 只为了和别的平台一条签名（它们要刷 token）；这里没有网络往返。
    pub async fn credential(&self, id: &ZcodeAccountId) -> Result<Secret> {
        let account = self.repo.get(id)?;
        let kind = match account.plan {
            ZcodePlan::CodingPlan => ZcodeSecret::ApiKey,
            ZcodePlan::StartPlan => ZcodeSecret::Jwt,
        };
        match self.repo.secret(id, kind)? {
            Some(s) if !s.is_empty() => Ok(s),
            _ => {
                let _ = self
                    .repo
                    .record_failure(id, "凭证缺失", Some(ZcodeStatus::NeedsLogin));
                Err(AppError::new(
                    ErrorCode::SecretMissing,
                    match account.plan {
                        ZcodePlan::CodingPlan => "这个 ZCode 账号没有 API key。",
                        ZcodePlan::StartPlan => "这个 ZCode 账号没有 JWT。",
                    },
                )
                .with_hint("在官方 ZCode 客户端里登录一次，然后重新导入。"))
            }
        }
    }

    /// 网关把这个号拿去发请求要知道的三件事：地址、套餐、服务商。
    pub fn dispatch_of(&self, id: &ZcodeAccountId) -> Result<(ZcodeProvider, ZcodePlan)> {
        let a = self.repo.get(id)?;
        Ok((a.provider, a.plan))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nexus_store::MemorySecrets;

    fn service() -> ZcodeService {
        ZcodeService::new(
            Arc::new(Db::open_in_memory().unwrap()),
            Arc::new(MemorySecrets::new()),
        )
    }

    #[tokio::test]
    async fn a_pasted_api_key_becomes_a_usable_coding_plan_account() {
        let s = service();
        let report = s
            .import_text("abc12345.def67890", ZcodeProvider::Zai, None)
            .unwrap();
        assert_eq!(report.created, 1);
        let account = &report.accounts[0];
        assert_eq!(account.plan, ZcodePlan::CodingPlan);
        assert!(account.has_credential());
        assert_eq!(
            s.credential(&account.id).await.unwrap().expose(),
            "abc12345.def67890"
        );
        assert_eq!(s.dispatch_of(&account.id).unwrap().1, ZcodePlan::CodingPlan);
    }

    #[tokio::test]
    async fn a_start_plan_account_hands_back_the_jwt_not_a_key() {
        let s = service();
        let report = s
            .import_text("head.body.sig", ZcodeProvider::Zai, None)
            .unwrap();
        let account = &report.accounts[0];
        assert_eq!(account.plan, ZcodePlan::StartPlan);
        assert!(account.has_jwt && !account.has_api_key);
        assert_eq!(
            s.credential(&account.id).await.unwrap().expose(),
            "head.body.sig"
        );
    }

    #[tokio::test]
    async fn an_account_without_its_credential_says_why() {
        let s = service();
        let report = s
            .import_text("abc12345.def67890", ZcodeProvider::Zai, None)
            .unwrap();
        let id = report.accounts[0].id.clone();
        s.repo
            .store_credentials(&id, None, None)
            .expect("no-op update");
        // 直接把秘密删掉，模拟秘密库和表不同步。
        s.repo.remove(&id).unwrap();
        assert!(s.credential(&id).await.is_err());
    }

    #[test]
    fn garbage_input_is_refused_with_a_hint() {
        let s = service();
        let err = s
            .import_text("这不是凭证", ZcodeProvider::Zai, None)
            .unwrap_err();
        assert!(err.hint.is_some());
    }

    #[test]
    fn the_catalog_is_not_empty_and_is_all_glm() {
        let s = service();
        let m = s.models();
        assert!(!m.is_empty());
        assert!(m.iter().all(|id| id.starts_with("glm-")));
    }
}
