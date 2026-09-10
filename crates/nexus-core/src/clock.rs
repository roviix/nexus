//! 时间。
//!
//! 全应用的时间戳统一是 RFC 3339 字符串（`2026-09-02T06:11:00Z`）：SQLite 里可直接
//! 按字典序排、JSON 里前端 `new Date()` 直接吃、和 shop 的 `toISOString()` 同形。
//!
//! 取「现在」走 `Clock` 而不是直接 `OffsetDateTime::now_utc()`，测试里才能把时间钉死。

use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> OffsetDateTime;

    fn now_iso(&self) -> String {
        self.now().format(&Rfc3339).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// 当前时刻的 RFC 3339 串。不需要注入时钟的地方（写 `updated_at` 之类）直接用它。
pub fn now_iso() -> String {
    SystemClock.now_iso()
}

/// 解析 RFC 3339，失败返回 `None`（上游时间字段不可信，不该 panic）。
pub fn parse_iso(raw: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(raw.trim(), &Rfc3339).ok()
}

/// Unix 毫秒 → RFC 3339。Cursor 的用量接口给的是毫秒时间戳。
pub fn iso_from_millis(ms: i64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp_nanos((ms as i128) * 1_000_000)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
}

/// `SystemTime` → RFC 3339。文件的修改时间之类走这里，别在各处各写一遍换算。
pub fn iso_from_system_time(t: std::time::SystemTime) -> Option<String> {
    OffsetDateTime::from(t).format(&Rfc3339).ok()
}

/// 适合放进文件名的时间戳：`20260903T142233Z`。
///
/// RFC 3339 里的 `:` 在 Windows 文件名里不合法，`-` 又和分隔用途撞车，所以只留数字和
/// `T` / `Z`。仍按字典序即时间序，目录里 `ls` 出来就是按时间排的。
pub fn file_stamp(t: OffsetDateTime) -> String {
    let fmt = time::macros::format_description!("[year][month][day]T[hour][minute][second]Z");
    t.to_offset(time::UtcOffset::UTC)
        .format(&fmt)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_iso_is_parseable_utc() {
        let s = now_iso();
        assert!(s.ends_with('Z'), "应当是 UTC：{s}");
        assert!(parse_iso(&s).is_some());
    }

    #[test]
    fn iso_strings_sort_chronologically() {
        let mut v = [
            "2026-09-02T10:00:00Z",
            "2026-01-02T10:00:00Z",
            "2026-09-02T09:59:59Z",
        ];
        v.sort();
        assert_eq!(v[0], "2026-01-02T10:00:00Z");
        assert_eq!(v[2], "2026-09-02T10:00:00Z");
    }

    #[test]
    fn millis_convert_to_iso() {
        assert_eq!(iso_from_millis(0).unwrap(), "1970-01-01T00:00:00Z");
        assert!(iso_from_millis(1_756_800_000_000)
            .unwrap()
            .starts_with("2025-"));
    }

    #[test]
    fn bad_timestamps_are_none_not_panic() {
        assert!(parse_iso("").is_none());
        assert!(parse_iso("昨天").is_none());
    }

    #[test]
    fn system_time_converts_to_iso() {
        assert_eq!(
            iso_from_system_time(std::time::UNIX_EPOCH).unwrap(),
            "1970-01-01T00:00:00Z"
        );
    }

    #[test]
    fn file_stamps_are_filename_safe_and_sort_chronologically() {
        let t = parse_iso("2026-09-03T14:22:33+08:00").unwrap();
        let stamp = file_stamp(t);
        // 转成了 UTC，且没有 `:` 和 `-`。
        assert_eq!(stamp, "20260903T062233Z");
        assert!(stamp.chars().all(|c| c.is_ascii_alphanumeric()));
        let later = file_stamp(parse_iso("2026-09-03T14:22:34+08:00").unwrap());
        assert!(later > stamp);
    }
}
