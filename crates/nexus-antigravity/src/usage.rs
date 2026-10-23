//! Google 反重力额度状态与用量线索。

use crate::model::AntigravityUsage;

pub fn quota_hint(usage: Option<&AntigravityUsage>) -> (Option<f64>, bool) {
    let Some(u) = usage else {
        return (None, false);
    };
    if u.last_status.as_deref() == Some("forbidden") {
        return (Some(100.0), true);
    }

    let five_hour: Vec<f64> = u
        .quota_groups
        .iter()
        .flat_map(|group| group.buckets.iter())
        .filter(|bucket| is_five_hour(&bucket.window))
        .map(|bucket| bucket.remaining_fraction)
        .collect();
    if !five_hour.is_empty() {
        let min = five_hour.into_iter().fold(1.0_f64, f64::min);
        let used = ((1.0 - min).clamp(0.0, 1.0) * 100.0).clamp(0.0, 100.0);
        return (Some(used), min <= 0.001);
    }

    let exhausted = u.remaining_requests.map(|r| r <= 0).unwrap_or(false)
        || u.percent_used.map(|p| p >= 100.0).unwrap_or(false);

    (u.percent_used, exhausted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_is_per_model_and_auth_is_not() {
        assert_eq!(
            model_cooldown_ms(429, r#"{"retryDelay":"12s"}"#),
            Some(12_000)
        );
        assert_eq!(
            model_cooldown_ms(429, r#"{"error":{"details":[{"retryDelay":"2m"}]}}"#),
            Some(120_000)
        );
        assert!(model_cooldown_ms(401, "invalid_grant").is_none());
        assert!(model_cooldown_ms(403, "ACCESS_TOKEN_EXPIRED").is_none());
        assert_eq!(model_cooldown_ms(429, "RESOURCE_EXHAUSTED"), Some(60_000));
    }
}

fn is_five_hour(window: &str) -> bool {
    let lower = window.to_ascii_lowercase();
    lower.contains("5h") || lower.contains("5-h") || lower == "5"
}

/// 解析 429 报错，判断是否为配额耗尽。账号卡片用它看快照。
/// 调度不要用它把整个号移出接力：429 按模型冷却，见 [`model_cooldown_ms`]。
pub fn is_quota_exhausted(status: u16, error_body: &str) -> bool {
    if status == 429 {
        return true;
    }
    error_body.contains("RESOURCE_EXHAUSTED")
        || error_body.contains("quota exceeded")
        || error_body.contains("Quota exceeded")
}

/// 这个错误应该让「这个号 + 这个模型」冷却多久。
///
/// 401/403 不在这里，那是整个号的授权问题。429 和配额耗尽只挡住当前模型，
/// 同一个号的其他模型还可以接。
pub fn model_cooldown_ms(status: u16, error_body: &str) -> Option<u64> {
    if status == 401 || status == 403 {
        return None;
    }
    let quota_words = error_body.contains("RESOURCE_EXHAUSTED")
        || error_body.to_ascii_lowercase().contains("quota");
    if status != 429 && !quota_words {
        return None;
    }
    let wait = parse_retry_delay_ms(error_body).unwrap_or(60_000);
    Some(wait.min(6 * 3_600_000))
}

/// 从 `retryDelay: "12s"` 或 `"seconds": 12` 里读等待时间。
pub fn parse_retry_delay_ms(body: &str) -> Option<u64> {
    if let Some(tail) = body.split("retryDelay").nth(1) {
        if let Some(ms) = duration_after(tail) {
            return Some(ms);
        }
    }
    if let Some(idx) = body.find("\"seconds\"") {
        let rest = &body[idx + "\"seconds\"".len()..];
        let digits: String = rest
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|c| c.is_ascii_digit())
            .collect();
        let seconds = digits.parse::<u64>().ok()?;
        return seconds.checked_mul(1000);
    }
    None
}

fn duration_after(tail: &str) -> Option<u64> {
    let start = tail.find(|c: char| c.is_ascii_digit())?;
    let rest = &tail[start..];
    let number: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let seconds = number.parse::<f64>().ok()?;
    if seconds < 0.0 {
        return None;
    }
    let unit = rest[number.len()..].trim_start();
    let ms = if unit.starts_with('h') || unit.starts_with('H') {
        seconds * 3_600_000.0
    } else if unit.starts_with('m') && !unit.starts_with("ms") {
        seconds * 60_000.0
    } else {
        seconds * 1000.0
    };
    Some(ms as u64)
}

/// 解析 401/403 报错，判断是否为凭证失效。
pub fn is_auth_expired(status: u16, error_body: &str) -> bool {
    if status == 401 {
        return true;
    }
    status == 403
        && (error_body.contains("ACCESS_TOKEN_EXPIRED")
            || error_body.contains("invalid_grant")
            || error_body.contains("UNAUTHENTICATED"))
}
