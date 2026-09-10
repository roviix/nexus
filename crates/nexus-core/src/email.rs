//! 规范化过的邮箱地址。
//!
//! 邮箱在这个应用里是**主键**：切号本按它去重，账号表按它唯一，导入按它合并。
//! 大小写不一致就会变成两个号，所以规范化不能靠调用方自觉——构造时就压成小写、
//! 去空白，之后类型系统保证拿到的都是规范形式。

use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct Email(String);

impl Email {
    /// 解析并规范化。只做最基本的形状检查：有且只有一个 `@`，两侧都非空。
    /// 不做 RFC 5322 全量校验——那会把一堆真实可用的地址挡在外面。
    pub fn parse(raw: impl AsRef<str>) -> Result<Self> {
        let trimmed = raw.as_ref().trim();
        if trimmed.is_empty() {
            return Err(AppError::invalid("邮箱不能为空。"));
        }
        let mut parts = trimmed.split('@');
        let local = parts.next().unwrap_or_default();
        let domain = parts.next().unwrap_or_default();
        if parts.next().is_some() || local.is_empty() || domain.is_empty() || !domain.contains('.')
        {
            return Err(AppError::invalid(format!("邮箱格式不对：{trimmed}")));
        }
        Ok(Self(trimmed.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }

    /// 展示用的打码形式：`ab****cd@example.com`。活动日志和默认视图用它。
    pub fn masked(&self) -> String {
        let (local, domain) = self.0.split_once('@').unwrap_or((self.0.as_str(), ""));
        let keep = if local.len() <= 2 { 1 } else { 2 };
        let head: String = local.chars().take(keep).collect();
        let tail: String = if local.len() > keep + 2 {
            local.chars().skip(local.chars().count() - 2).collect()
        } else {
            String::new()
        };
        format!("{head}****{tail}@{domain}")
    }
}

impl fmt::Display for Email {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Email {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> std::result::Result<Self, D::Error> {
        let raw = String::deserialize(de)?;
        Email::parse(raw).map_err(serde::de::Error::custom)
    }
}

impl TryFrom<String> for Email {
    type Error = AppError;
    fn try_from(value: String) -> Result<Self> {
        Email::parse(value)
    }
}

impl AsRef<str> for Email {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case_and_whitespace() {
        let a = Email::parse("  Foo.Bar@Example.COM ").unwrap();
        assert_eq!(a.as_str(), "foo.bar@example.com");
        assert_eq!(a, Email::parse("foo.bar@example.com").unwrap());
    }

    #[test]
    fn rejects_malformed() {
        for bad in ["", "   ", "nobody", "a@b", "a@@b.com", "@example.com", "a@"] {
            assert!(Email::parse(bad).is_err(), "应当拒绝 {bad:?}");
        }
    }

    #[test]
    fn masks_without_leaking_the_local_part() {
        assert_eq!(
            Email::parse("alexander@example.com").unwrap().masked(),
            "al****er@example.com"
        );
        assert_eq!(
            Email::parse("ab@example.com").unwrap().masked(),
            "a****@example.com"
        );
    }

    #[test]
    fn deserializes_through_the_same_validation() {
        let e: Email = serde_json::from_str("\"USER@Example.com\"").unwrap();
        assert_eq!(e.as_str(), "user@example.com");
        assert!(serde_json::from_str::<Email>("\"nope\"").is_err());
    }
}
