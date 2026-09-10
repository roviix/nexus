//! 秘密字符串。
//!
//! 三件事，都是为了「明文不外泄」这一条（§8）：
//!   - `Debug` / `Display` 只打印 `Secret(<hidden>)`，所以随手 `dbg!` 或把结构体塞进
//!     日志都不会漏 token；
//!   - `Serialize` 故意**没有**实现——秘密不该顺着 IPC / 事件 / SQLite 溜出去，
//!     真要给出去必须显式 `.expose()`，那一行在 review 里看得见；
//!   - 析构时 zeroize，减少内存里残留的窗口。

use serde::{Deserialize, Deserializer};
use std::fmt;
use zeroize::Zeroize;

#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 显式取明文。调用点应当少而显眼。
    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn into_inner(mut self) -> String {
        std::mem::take(&mut self.0)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 给界面看的打码形式：留头尾各 4 位，够对账、不够复用。
    pub fn masked(&self) -> String {
        let n = self.0.chars().count();
        if n <= 8 {
            return "•".repeat(n.max(4));
        }
        let head: String = self.0.chars().take(4).collect();
        let tail: String = self.0.chars().skip(n - 4).collect();
        format!("{head}…{tail}（{n} 字符）")
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<hidden>)")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<hidden>")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self(value)
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self(value.to_string())
    }
}

/// 反序列化是允许的（要从上游 JSON 里读 token 进来），序列化不允许。
impl<'de> Deserialize<'de> for Secret {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        Ok(Secret(String::deserialize(de)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_prints_the_plaintext() {
        let s = Secret::new("super-secret-refresh-token");
        assert_eq!(format!("{s:?}"), "Secret(<hidden>)");
        assert_eq!(format!("{s}"), "<hidden>");
        assert!(!format!("{s:?} {s}").contains("super-secret"));
    }

    #[test]
    fn masking_keeps_enough_to_identify_not_to_reuse() {
        let s = Secret::new("abcdefghijklmnop");
        let m = s.masked();
        assert!(m.starts_with("abcd"));
        assert!(m.contains("mnop"));
        assert!(!m.contains("efghijkl"));
    }

    #[test]
    fn short_secrets_are_fully_masked() {
        assert_eq!(Secret::new("abc").masked(), "••••");
        assert_eq!(Secret::new("abcdefgh").masked(), "••••••••");
    }
}
