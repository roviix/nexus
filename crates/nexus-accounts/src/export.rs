//! 把「我的账号」导出成一份清单。
//!
//! 形状是 `{"accounts":[{"email":…,"refreshToken":…}]}`，
//! 所以 [`crate::parse_dump`] 能原样吃回去 —— 导出与导入是同一种文件的两个方向，用户不用
//! 记两种格式，换台机器把文件粘进「批量添加」就行。
//!
//! **只带凭证和备注。** 用量、状态、订阅档是派生量，导回去时一次刷新就重算出来（与
//! [`crate::accounts_json`] 的取舍一致）。来源与标签也不带：它们描述的是「在这个库里」的
//! 关系，不属于账号本身；要连这些一起搬，走整库备份（`nexus-store::backup`）。
//!
//! 内容是明文凭证。这里只负责生成文本；落到哪、权限收多紧、怎么跟用户说，是调用方的事。

use crate::repo::Accounts;
use nexus_core::{now_iso, AccountId, Result};
use nexus_store::keys::AccountSecret;
use serde::Serialize;
use std::collections::HashMap;

/// 文件头里的格式标记。`parse_dump` 不认识它，会忽略；写给打开文件的人看。
pub const FORMAT: &str = "nexus-accounts/1";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    email: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cursor_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    email_password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovery_email: Option<String>,
    /// 与导入侧同一字段名，导出去的文件能原样粘回来。
    #[serde(skip_serializing_if = "Option::is_none")]
    user_api_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    /// 只在「多选复制」里出现：界面排好的一行用量说明。整库导出不带 —— 那是派生量。
    #[serde(skip_serializing_if = "Option::is_none")]
    info: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Dump {
    format: &'static str,
    exported_at: String,
    accounts: Vec<Entry>,
}

/// 一次导出：JSON 文本与条数。**没有 `Debug`** —— 里面是凭证，不该顺手印进日志。
pub struct Export {
    pub json: String,
    pub count: usize,
}

impl Accounts {
    /// 把库里全部账号连凭证导出成 JSON。
    pub fn export_dump(&self) -> Result<Export> {
        let mut entries = Vec::new();
        for account in self.list()? {
            let secret = |kind| -> Result<Option<String>> {
                Ok(self
                    .secret(&account.id, kind)?
                    .map(|s| s.expose().to_string()))
            };
            entries.push(Entry {
                email: account.email.clone(),
                refresh_token: secret(AccountSecret::Refresh)?,
                cursor_password: secret(AccountSecret::CursorPassword)?,
                email_password: secret(AccountSecret::EmailPassword)?,
                recovery_email: secret(AccountSecret::RecoveryEmail)?,
                user_api_key: secret(AccountSecret::ApiKey)?,
                note: account.note.clone(),
                info: None,
            });
        }
        let count = entries.len();
        let json = serde_json::to_string_pretty(&Dump {
            format: FORMAT,
            exported_at: now_iso(),
            accounts: entries,
        })?;
        Ok(Export { json, count })
    }

    /// 多选账号按指定格式复制。
    ///
    /// `info` 是界面按账号 id 附的一行说明（API 余量、按需、积分、重置时间这类）。文本格式里
    /// 它跟在凭证行后面**另起一行**，不进 `----` 拼接 —— 那一行是给脚本吃的，多一段就解析不了；
    /// JSON 格式里落进 `info` 字段。文案与日期由界面按本地时区排好，这里只负责拼。
    pub fn copy_selected(
        &self,
        ids: &[AccountId],
        format: &str,
        info: &HashMap<String, String>,
    ) -> Result<String> {
        let secret = |id: &AccountId, kind| -> Result<Option<String>> {
            Ok(self.secret(id, kind)?.map(|s| s.expose().to_string()))
        };
        let info_for = |id: &AccountId| -> Option<String> {
            info.get(id.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };

        if format == "json" {
            let mut entries = Vec::new();
            for id in ids {
                let Ok(account) = self.get(id) else { continue };
                entries.push(Entry {
                    email: account.email.clone(),
                    refresh_token: secret(id, AccountSecret::Refresh)?,
                    cursor_password: secret(id, AccountSecret::CursorPassword)?,
                    email_password: secret(id, AccountSecret::EmailPassword)?,
                    recovery_email: secret(id, AccountSecret::RecoveryEmail)?,
                    user_api_key: secret(id, AccountSecret::ApiKey)?,
                    note: account.note.clone(),
                    info: info_for(id),
                });
            }
            return Ok(serde_json::to_string_pretty(&Dump {
                format: FORMAT,
                exported_at: now_iso(),
                accounts: entries,
            })?);
        }

        let paired = match format {
            "email" => Paired::Nothing,
            "email_password" => Paired::Secret(AccountSecret::CursorPassword),
            "email_refresh" => Paired::Secret(AccountSecret::Refresh),
            "email_session" => Paired::Session,
            _ => return Err(nexus_core::AppError::invalid("不认识的复制格式。")),
        };

        let mut blocks = Vec::new();
        let mut annotated = false;
        for id in ids {
            let Ok(account) = self.get(id) else { continue };
            let tail = match paired {
                Paired::Nothing => None,
                Paired::Secret(kind) => Some(secret(id, kind)?.unwrap_or_default()),
                Paired::Session => Some(self.session_token_string(&account)?.unwrap_or_default()),
            };
            let mut line = account.email;
            if let Some(tail) = tail {
                line.push_str("----");
                line.push_str(&tail);
            }
            if let Some(extra) = info_for(id) {
                annotated = true;
                line.push('\n');
                line.push_str(&extra);
            }
            blocks.push(line);
        }
        // 带了说明每个号就是两行，中间空一行才看得出哪行归哪个号；不带就还是一行一个。
        Ok(blocks.join(if annotated { "\n\n" } else { "\n" }))
    }

    /// 这个号的**会话 token**：`user_xxx::<access jwt>`，即 `WorkosCursorSessionToken` cookie 的值。
    ///
    /// 「复制 Session」给出去的必须是这个形状 —— 裸 access JWT 贴到别处登不进 cursor.com，
    /// 收的人只会以为号坏了。user_id 优先用库里记的，没有就从 JWT 的 `sub` 里算；两处都没有时
    /// 退成裸 JWT（总比一个空串强）。不去换新的：这里是同步读库，续期由调用方在之前做。
    fn session_token_string(&self, account: &crate::model::Account) -> Result<Option<String>> {
        let Some(access) = self.secret(&account.id, AccountSecret::Access)? else {
            return Ok(None);
        };
        let jwt = access.expose();
        let user_id = account
            .workos_user_id
            .clone()
            .or_else(|| crate::token::extract_user_id(jwt));
        Ok(Some(match user_id {
            Some(u) => format!("{u}::{jwt}"),
            None => jwt.to_string(),
        }))
    }
}

/// 文本格式里 `----` 后面跟的是什么。
#[derive(Clone, Copy)]
enum Paired {
    Nothing,
    Secret(AccountSecret),
    /// `user_xxx::<jwt>`，不是某一格凭证，是拼出来的。
    Session,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::preview;
    use crate::model::NewAccount;
    use nexus_store::{Db, MemorySecrets};
    use std::sync::Arc;

    fn library() -> Accounts {
        let db = Arc::new(Db::open_in_memory().unwrap());
        Accounts::new(db, Arc::new(MemorySecrets::new()))
    }

    #[test]
    fn an_empty_library_exports_an_empty_list() {
        let export = library().export_dump().unwrap();
        assert_eq!(export.count, 0);
        let v: serde_json::Value = serde_json::from_str(&export.json).unwrap();
        assert_eq!(v["format"], FORMAT);
        assert_eq!(v["accounts"], serde_json::json!([]));
    }

    #[test]
    fn an_export_round_trips_through_the_importer() {
        // 这是这个模块存在的全部理由：导出去的文件粘回「批量添加」，一个号都不能少、
        // 一条凭证都不能变。
        let accounts = library();
        accounts
            .upsert(NewAccount {
                email: "a@example.com".into(),
                refresh_token: Some("rt-a".into()),
                cursor_password: Some("pw-a".into()),
                email_password: Some("epw-a".into()),
                recovery_email: Some("r@example.com".into()),
                note: Some("主力".into()),
                ..Default::default()
            })
            .unwrap();
        accounts
            .upsert(NewAccount {
                email: "b@example.com".into(),
                cursor_password: Some("pw-b".into()),
                ..Default::default()
            })
            .unwrap();
        accounts
            .upsert(NewAccount {
                email: "k@example.com".into(),
                api_key: Some("crsr_abc123DEF".into()),
                ..Default::default()
            })
            .unwrap();

        let export = accounts.export_dump().unwrap();
        assert_eq!(export.count, 3);
        let dumped: serde_json::Value = serde_json::from_str(&export.json).unwrap();
        assert!(dumped["accounts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["userApiKey"] == "crsr_abc123DEF"));

        let (accepted, report) = preview(&export.json);
        assert_eq!(report.accepted_count, 3, "{report:?}");
        assert!(report.skipped.is_empty());
        let a = accepted
            .iter()
            .find(|p| p.email == "a@example.com")
            .unwrap();
        assert_eq!(a.refresh_token.as_deref(), Some("rt-a"));
        assert_eq!(a.cursor_password.as_deref(), Some("pw-a"));
        assert_eq!(a.email_password.as_deref(), Some("epw-a"));
        assert_eq!(a.recovery_email.as_deref(), Some("r@example.com"));
        assert!(a.api_key.is_none());
        assert_eq!(a.note.as_deref(), Some("主力"));
        let k = accepted
            .iter()
            .find(|p| p.email == "k@example.com")
            .unwrap();
        assert_eq!(k.api_key.as_deref(), Some("crsr_abc123DEF"));
        let b = accepted
            .iter()
            .find(|p| p.email == "b@example.com")
            .unwrap();
        assert_eq!(b.cursor_password.as_deref(), Some("pw-b"));
        assert!(b.refresh_token.is_none());

        // 导回一个空库，三个号都要能收下。
        let fresh = library();
        for parsed in accepted {
            fresh.upsert(parsed.into()).unwrap();
        }
        assert_eq!(fresh.list().unwrap().len(), 3);
        assert!(fresh.list().unwrap().iter().any(|x| x.has_api_key));
    }

    #[test]
    fn an_export_carries_only_credentials_and_notes() {
        let accounts = library();
        accounts
            .upsert(NewAccount {
                email: "a@example.com".into(),
                refresh_token: Some("rt".into()),
                ..Default::default()
            })
            .unwrap();
        let export = accounts.export_dump().unwrap();
        let v: serde_json::Value = serde_json::from_str(&export.json).unwrap();
        let entry = v["accounts"][0].as_object().unwrap();
        // 没有的凭证不写 null 占位；派生量（状态、用量、来源）一个都不该出现。
        let keys: Vec<&str> = entry.keys().map(String::as_str).collect();
        assert_eq!(keys, vec!["email", "refreshToken"]);
    }
}
