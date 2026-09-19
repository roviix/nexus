//! 各类实体 id。
//!
//! 全是 UUID 字符串，但类型不同 —— 把 `AccountId` 传进要 `ProfileId` 的地方会编译不过。
//! 这不是洁癖：切号本和我的账号是两张互不关联的表（R1），它们的 id 混用就是数据错乱。

use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! id_type {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new() -> Self {
                Self(uuid::Uuid::new_v4().to_string())
            }

            pub fn from_raw(raw: impl Into<String>) -> Self {
                Self(raw.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl From<String> for $name {
            fn from(raw: String) -> Self {
                Self(raw)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }
    };
}

id_type!(AccountId, "「我的账号」里一个 Cursor 账号的 id。");
id_type!(ProfileId, "切号本里一个档的 id。");
id_type!(BackupId, "一份登录态备份的 id。");
id_type!(
    ChatGptAccountId,
    "一个 ChatGPT 账号的 id。和 `AccountId` 是两张表、两种平台，同一个邮箱在两边是两个账号。"
);
id_type!(
    GrokAccountId,
    "一个 Grok Build（xAI CLI 订阅）账号的 id。和 Cursor / ChatGPT 分表，同一个邮箱是另一条账号。"
);
id_type!(
    KiroAccountId,
    "一个 Kiro（Amazon Q / Builder ID）账号的 id。和 Cursor / ChatGPT / Grok 分表。"
);
id_type!(
    ZcodeAccountId,
    "一个 ZCode（智谱 GLM 编码套餐）账号的 id。同一个邮箱下的个人版 / 团队版 / 体验套餐是三条账号——\
     它们各有各的凭证和额度。"
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_round_trip() {
        let a = AccountId::new();
        assert_ne!(a, AccountId::new());
        let json = serde_json::to_string(&a).unwrap();
        assert_eq!(json, format!("\"{a}\""));
        assert_eq!(serde_json::from_str::<AccountId>(&json).unwrap(), a);
    }
}
