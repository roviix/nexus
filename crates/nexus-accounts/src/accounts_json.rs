//! 读一份**结构化的账号 JSON**：`{"accounts":[{"email":…,"refreshToken":…}]}`。
//!
//! 这正是 [`crate::export`] 写出来的形状（`nexus-accounts/1`），所以导出与导入是同一种文件的
//! 两个方向——换台机器把文件粘进「批量添加」就行。外部工具只要产出同样的字段名，也走这条路。
//!
//! 与 [`crate::import`] 里按形状猜的那套**刻意分开**，因为在这里猜不只是多余，而是错的：
//! Cursor 走 OAuth（`auth/poll`）换回来的 refresh token，其 JWT payload 里 `type`
//! 恰恰是 `"session"`，而且与同批返回的 access token 逐字节相同。让形状启发式去看它，
//! 必然判成「一次性会话票」而把整批真能用的 token 拒在门外。这份 JSON 的字段名就写着
//! `refreshToken`，来源是确定的，不存在可猜的余地。
//!
//! **只搬凭证和备注。** `usage`、`workosUserId`、`status`、`membership` 这些是派生量，
//! 一次刷新就会被权威地重算出来；搬一份过时的副本过来，只会让人对着旧数字做决定。

use crate::import::{is_email, merge, ParsedAccount};
use serde::Deserialize;
use std::collections::HashMap;

/// `{"accounts": [...]}`，也接受直接把那个数组抠出来粘过来的写法。
#[derive(Deserialize)]
#[serde(untagged)]
enum Dump {
    Wrapped { accounts: Vec<Entry> },
    Bare(Vec<Entry>),
}

/// 只声明搬得走的字段，其余交给 serde 忽略 —— 产出方加字段不该让这里解析失败。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    email: Option<String>,
    cursor_password: Option<String>,
    email_password: Option<String>,
    recovery_email: Option<String>,
    refresh_token: Option<String>,
    /// 按字段名收下，**不改判**。只用于解释「这个号为什么没 refresh 也收不进来」。
    access_token: Option<String>,
    note: Option<String>,
}

/// 认出并解析一份结构化账号 JSON。
///
/// 不是这个形状就返回 `None`，让调用方接着按自由格式清单去解析 —— 导入框只有一个，
/// 用户不该先被问「你粘的是哪种」。
///
/// 返回值与 [`crate::import::parse_dump`] 同形：（按邮箱合并后的记录，说不明白的条目）。
pub fn parse(text: &str) -> Option<(Vec<ParsedAccount>, Vec<String>)> {
    let text = text.trim();
    // 先看一眼首字符，省掉对每份粘进来的清单都试一次 JSON 解析。
    if !text.starts_with('{') && !text.starts_with('[') {
        return None;
    }
    let entries = match serde_json::from_str::<Dump>(text).ok()? {
        Dump::Wrapped { accounts } => accounts,
        Dump::Bare(list) => list,
    };

    let mut by_email: HashMap<String, ParsedAccount> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    // 报序号而不是原样回显整条：一条 JSON 里裹着 refresh token 和密码，
    // 而 `skipped` 是要显示在界面上的。
    for (i, entry) in entries.into_iter().enumerate() {
        match entry.into_parsed() {
            Ok(parsed) => merge(&mut by_email, &mut order, parsed),
            Err(why) => skipped.push(format!("第 {} 条：{why}", i + 1)),
        }
    }

    // 一条都没认出来，多半根本不是这个形状（比如另一份别的 JSON）。
    // 交回给清单解析去试，别拿一屏「缺邮箱」糊人一脸。
    if order.is_empty() {
        return None;
    }

    let accounts = order
        .into_iter()
        .filter_map(|e| by_email.remove(&e))
        .collect();
    Some((accounts, skipped))
}

impl Entry {
    fn into_parsed(self) -> Result<ParsedAccount, String> {
        let email = clean(self.email).ok_or("缺邮箱")?.to_ascii_lowercase();
        if !is_email(&email) {
            return Err(format!("{email} 不像邮箱"));
        }
        Ok(ParsedAccount {
            email,
            cursor_password: clean(self.cursor_password),
            email_password: clean(self.email_password),
            recovery_email: clean(self.recovery_email),
            refresh_token: clean(self.refresh_token),
            access_token: clean(self.access_token),
            api_key: None,
            note: clean(self.note),
        })
    }
}

/// 空串等同没有。`undefined` 会被 `JSON.stringify` 丢掉，
/// 但手工编辑过的文件里常留下 `""`，那不是一个凭证。
fn clean(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::{parse_dump, preview};
    use base64::Engine;

    /// Cursor OAuth 换回来的那种 token：`type` 是 `"session"`，带 `offline_access`。
    fn cursor_oauth_token() -> String {
        let enc = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        let payload = serde_json::json!({
            "sub": "auth0|user_01EXAMPLE0000000000000000",
            "exp": 1_793_506_140_i64,
            "iss": "https://authentication.cursor.sh",
            "scope": "openid profile email offline_access",
            "type": "session",
        });
        format!(
            "{}.{}.{}",
            enc(br#"{"alg":"HS256"}"#),
            enc(payload.to_string().as_bytes()),
            enc(b"sig")
        )
    }

    fn dump_of(entries: serde_json::Value) -> String {
        serde_json::json!({ "accounts": entries }).to_string()
    }

    #[test]
    fn a_cursor_refresh_token_survives_the_trip() {
        // 这是这个模块存在的全部理由：同一个 token 交给形状启发式会被判成
        // 一次性会话票而拒收，交给字段名就原样收下。
        let token = cursor_oauth_token();
        let text = dump_of(serde_json::json!([{
            "email": "a@example.com",
            "refreshToken": token,
            "accessToken": token,
            "cursorPassword": "pw",
            "emailPassword": "epw",
        }]));

        let (accounts, skipped) = parse(&text).expect("认得出是结构化账号 JSON");
        assert!(skipped.is_empty());
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].refresh_token.as_deref(), Some(token.as_str()));
        assert_eq!(accounts[0].cursor_password.as_deref(), Some("pw"));
        assert_eq!(accounts[0].email_password.as_deref(), Some("epw"));

        // 对照组：同一个 token 走清单那条路会被判成 access。
        let (guessed, _) = parse_dump(&format!("a@example.com----{token}"));
        assert!(
            guessed[0].refresh_token.is_none() && guessed[0].access_token.is_some(),
            "形状启发式的行为变了，这个模块的理由需要重新说一遍"
        );
    }

    #[test]
    fn the_whole_export_reaches_the_repository_shape() {
        let token = cursor_oauth_token();
        let text = dump_of(serde_json::json!([
            { "email": "a@example.com", "refreshToken": token, "cursorPassword": "pw1" },
            // 只有密码：收下，待授权。
            { "email": "b@example.com", "cursorPassword": "pw2", "note": "UI 检查" },
            // 只有会话票：按「仅会话」收下，有效期内能用。
            { "email": "c@example.com", "accessToken": token },
        ]));

        let (accepted, report) = preview(&text);
        assert_eq!(report.accepted_count, 3);
        assert_eq!(report.rejected_count, 0);

        let by = |e: &str| report.rows.iter().find(|r| r.email == e).unwrap().clone();
        assert!(by("a@example.com").has_refresh);
        assert!(by("b@example.com").accepted && !by("b@example.com").has_refresh);
        assert!(by("c@example.com").has_access && !by("c@example.com").has_refresh);

        let n: crate::NewAccount = accepted.into_iter().next().unwrap().into();
        assert_eq!(n.refresh_token.as_deref(), Some(token.as_str()));
        assert!(n.qualify().is_ok());
    }

    #[test]
    fn a_recovery_email_is_carried_rather_than_dropped() {
        let text = dump_of(serde_json::json!([{
            "email": "a@example.com",
            "cursorPassword": "pw",
            "recoveryEmail": "backup@example.com",
        }]));
        let (accounts, _) = parse(&text).unwrap();
        assert_eq!(
            accounts[0].recovery_email.as_deref(),
            Some("backup@example.com")
        );
        let n: crate::NewAccount = accounts.into_iter().next().unwrap().into();
        assert_eq!(n.recovery_email.as_deref(), Some("backup@example.com"));
    }

    #[test]
    fn derived_fields_are_ignored_rather_than_breaking_the_parse() {
        // usage / workosUserId / tags / status 都不搬，但它们的存在不该让解析失败。
        let text = dump_of(serde_json::json!([{
            "id": "3c7cc50f-1a88-4078-a4e4-9c64392abe94",
            "email": "a@example.com",
            "cursorPassword": "pw",
            "workosUserId": "user_01EXAMPLE0000000000000000",
            "status": "active",
            "tags": [],
            "usage": { "plan": "pro", "totalPercentUsed": 57.3 },
            "accessTokenExpiresAt": "2026-11-01T04:09:00.000Z",
            "codeChannel": "auto",
        }]));
        let (accounts, skipped) = parse(&text).unwrap();
        assert!(skipped.is_empty());
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].cursor_password.as_deref(), Some("pw"));
    }

    #[test]
    fn a_bare_array_works_too() {
        let text =
            serde_json::json!([{ "email": "a@example.com", "cursorPassword": "pw" }]).to_string();
        let (accounts, _) = parse(&text).unwrap();
        assert_eq!(accounts[0].email, "a@example.com");
    }

    #[test]
    fn emails_are_normalised_and_duplicates_collapse() {
        let token = cursor_oauth_token();
        let text = dump_of(serde_json::json!([
            { "email": "  A@Example.COM ", "cursorPassword": "pw", "emailPassword": "epw" },
            // 后一条更新，但缺的字段不该把前一条的抹掉。
            { "email": "a@example.com", "refreshToken": token },
        ]));
        let (accounts, _) = parse(&text).unwrap();
        assert_eq!(accounts.len(), 1, "同一个号只该出现一次");
        assert_eq!(accounts[0].email, "a@example.com");
        assert!(accounts[0].refresh_token.is_some());
        assert_eq!(accounts[0].email_password.as_deref(), Some("epw"));
    }

    #[test]
    fn blank_strings_are_not_credentials() {
        let text = dump_of(serde_json::json!([{
            "email": "a@example.com",
            "cursorPassword": "pw",
            "refreshToken": "",
            "emailPassword": "   ",
        }]));
        let (accounts, _) = parse(&text).unwrap();
        assert!(accounts[0].refresh_token.is_none());
        assert!(accounts[0].email_password.is_none());
    }

    #[test]
    fn entries_without_a_usable_email_are_reported_without_echoing_secrets() {
        let text = dump_of(serde_json::json!([
            { "email": "a@example.com", "cursorPassword": "pw" },
            { "cursorPassword": "super-secret" },
            { "email": "not-an-email", "cursorPassword": "super-secret" },
        ]));
        let (accounts, skipped) = parse(&text).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(
            skipped,
            vec!["第 2 条：缺邮箱", "第 3 条：not-an-email 不像邮箱"]
        );
        assert!(
            !skipped.join("").contains("super-secret"),
            "报错信息会显示在界面上，不能带出凭证"
        );
    }

    #[test]
    fn other_shapes_fall_through_to_the_list_parser() {
        // 不是 JSON。
        assert!(parse("a@example.com----pw").is_none());
        // 是 JSON，但不是这个形状。
        assert!(parse(r#"{"hello":"world"}"#).is_none());
        // 是数组，但里面一条账号都认不出来。
        assert!(parse(r#"[{"foo":1},{"bar":2}]"#).is_none());
        // 截断的 JSON 不该 panic，也不该吞掉这段文本。
        assert!(parse(r#"{"accounts":[{"email":"a@exam"#).is_none());
        // 空的。
        assert!(parse("").is_none() && parse("  ").is_none());
    }

    #[test]
    fn the_list_parser_still_owns_everything_that_is_not_json() {
        // 走 `parse_dump` 的老路径不受影响。
        let (accounts, skipped) = parse_dump("a@example.com----epw----pw");
        assert!(skipped.is_empty());
        assert_eq!(accounts[0].cursor_password.as_deref(), Some("pw"));
    }
}
