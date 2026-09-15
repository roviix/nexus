//! 把卖家/运营那套自由格式的账号清单解析成结构化记录。
//!
//! 规格来源：`shop/src/lib/cursorpool/import.ts`（那份是在真实清单上磨出来的）。
//! 现实里清单极乱，同一批账号混着好几种写法：
//!
//! ```text
//! 邮箱----邮箱密码----Cursor密码
//! 邮箱----Cursor密码----额度重置时间
//! 邮箱----user_xxx::JWT
//! 邮箱----eyJ…            （裸 JWT，多为 session/access）
//! 邮箱----crsr_…          （长期 API Key）
//! 第3个：邮箱
//! 登录密码：X  邮箱密码：Y
//! ```
//!
//! 还夹着「卖了 / 在蹬 / 满了 / 超额」这类批注，长 token 会被换行截断成两行，
//! `::` 会被 URL 编码成 `%3A%3A`。
//!
//! 所以**不按列位死解析，而是逐段按形状分类**：像邮箱就是邮箱，像 `crsr_` 就是 key，
//! 像时间就是重置时间，剩下的纯文本按位置当密码。认不出的行丢进 `skipped`，不中断整批
//! ——一行看不懂就整批失败，对着一份两百行的清单是最没用的行为。

use crate::model::NewAccount;
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// 从清单里解析出来的一条。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ParsedAccount {
    pub email: String,
    pub cursor_password: Option<String>,
    pub email_password: Option<String>,
    pub recovery_email: Option<String>,
    pub refresh_token: Option<String>,
    /// session / access token（`user_xxx::<jwt>` 或裸 JWT）。单独也收——有效期内能用，
    /// 到期退回待登录；界面上标「仅会话」。
    pub access_token: Option<String>,
    /// 长期 API Key（`crsr_…`）。切不进 Cursor，能查基础用量。
    pub api_key: Option<String>,
    pub note: Option<String>,
}

/// 一条的去向。给界面做「导入前预览」用 —— 让人先看清会收哪些、为什么不收哪些，
/// 比导完再解释强。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ImportRow {
    pub email: String,
    /// 收不收。
    pub accepted: bool,
    /// 不收的原因，或收下时带了哪些凭证。
    pub reason: String,
    pub has_refresh: bool,
    pub has_password: bool,
    pub has_email_password: bool,
    /// 带了 session token（有效期内可用）。
    #[serde(default)]
    pub has_access: bool,
    /// 带了 `crsr_` User API Key。
    #[serde(default)]
    pub has_api_key: bool,
}

/// 一次解析的完整结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPreview {
    pub rows: Vec<ImportRow>,
    /// 完全认不出的行，原样带回去让人自己看。
    pub skipped: Vec<String>,
    pub accepted_count: u32,
    pub rejected_count: u32,
}

/// 解析 + 按托管门槛分流。**不写库** —— 预览与执行分开，用户得先看再决定。
pub fn preview(text: &str) -> (Vec<ParsedAccount>, ImportPreview) {
    let (parsed, skipped) = parse_dump(text);
    let mut out = ImportPreview {
        skipped,
        ..Default::default()
    };
    let mut accepted = Vec::new();

    for a in parsed {
        let has_refresh = non_empty(&a.refresh_token);
        let has_password = non_empty(&a.cursor_password);
        let has_email_password = non_empty(&a.email_password);
        let has_access = a
            .access_token
            .as_deref()
            .is_some_and(|t| crate::token::normalize_access(t).is_ok());

        let has_api_key = a
            .api_key
            .as_deref()
            .is_some_and(crate::token::looks_like_user_api_key);

        // 托管门槛：邮箱 + (refresh_token | Cursor 密码 | session token | crsr_)。
        let (ok, reason) = if has_refresh && has_password {
            (true, "refresh_token + 密码".to_string())
        } else if has_refresh {
            (true, "refresh_token".to_string())
        } else if has_password {
            (true, "Cursor 密码（可授权换 token）".to_string())
        } else if has_access {
            (
                true,
                "仅 session token（有效期内可查用量、可进网关，不能切号，到期需重新粘）"
                    .to_string(),
            )
        } else if has_api_key {
            (true, "仅 API Key（可查基础用量，不能切号）".to_string())
        } else if a.access_token.is_some() {
            (false, "session token 不是合法的 JWT".to_string())
        } else if a.api_key.is_some() {
            (false, "API Key 不是 crsr_ 开头".to_string())
        } else {
            (
                false,
                "缺凭证；至少要 Cursor 密码、refresh_token、session token 或 crsr_ API Key"
                    .to_string(),
            )
        };

        out.rows.push(ImportRow {
            email: a.email.clone(),
            accepted: ok,
            reason,
            has_refresh,
            has_password,
            has_email_password,
            has_access,
            has_api_key,
        });
        if ok {
            out.accepted_count += 1;
            accepted.push(a);
        } else {
            out.rejected_count += 1;
        }
    }
    (accepted, out)
}

impl From<ParsedAccount> for NewAccount {
    fn from(a: ParsedAccount) -> Self {
        NewAccount {
            email: a.email,
            refresh_token: a.refresh_token,
            access_token: a.access_token,
            cursor_password: a.cursor_password,
            email_password: a.email_password,
            recovery_email: a.recovery_email,
            api_key: a.api_key,
            note: a.note,
            source: None,
            tags: Vec::new(),
        }
    }
}

fn non_empty(v: &Option<String>) -> bool {
    v.as_deref().is_some_and(|s| !s.trim().is_empty())
}

// ── 解析 ────────────────────────────────────────────────────────────────────

pub(crate) fn is_email(s: &str) -> bool {
    let s = s.trim();
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.contains(char::is_whitespace)
        && s.matches('@').count() == 1
}

/// 从一段文本里揪出第一个邮箱（用于「第3个：xxx@yyy.com」这类行）。
fn find_email(s: &str) -> Option<String> {
    s.split(|c: char| c.is_whitespace() || "：:，,；;（）()[]【】".contains(c))
        .find(|t| is_email(t))
        .map(|t| t.trim().to_ascii_lowercase())
}

fn is_timeish(s: &str) -> bool {
    let b = s.as_bytes();
    // 2026-08-07 …
    let ymd = b.len() >= 10
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-';
    // 21:53:08 / 21:53
    let hms = {
        let parts: Vec<&str> = s.split(':').collect();
        (parts.len() == 2 || parts.len() == 3)
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.len() <= 2 && p.bytes().all(|c| c.is_ascii_digit()))
    };
    // 08/11 16:34
    let mdhm = b.len() >= 8
        && b[..2].iter().all(u8::is_ascii_digit)
        && b[2] == b'/'
        && b[3..5].iter().all(u8::is_ascii_digit)
        && s.contains(':');
    ymd || hms || mdhm
}

/// 清单里全是中文，按字节切片会在多字节字符中间 panic。一律用 `get(..)`。
fn is_api_key(s: &str) -> bool {
    s.len() > 5 && s.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("crsr_"))
}

fn is_tokenish(s: &str) -> bool {
    s.contains("::") || s.starts_with("eyJ") || is_api_key(s)
}

fn decode_jwt_payload(jwt: &str) -> Option<serde_json::Value> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 把一段 token 归类写进目标记录。
///
/// `type=web/session` 是 WorkOS 会话（access 侧，配 cookie 用）；否则当 refresh。
/// 卖家给的裸 JWT 绝大多数是会话 token。
fn apply_token(target: &mut ParsedAccount, raw: &str) {
    let tok = raw.replace("%3A%3A", "::").replace("%3a%3a", "::");
    let tok = tok.trim();

    if is_api_key(tok) {
        target.api_key = Some(tok.to_string());
        return;
    }

    let jwt = match tok.split_once("::") {
        Some((_left, right)) => right,
        None => tok,
    };
    if !jwt.starts_with("eyJ") {
        return;
    }

    let payload = decode_jwt_payload(jwt);
    let kind = payload
        .as_ref()
        .and_then(|p| p.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    if kind == "web" || kind == "session" || tok.contains("::") {
        target.access_token = Some(jwt.to_string());
    } else {
        target.refresh_token = Some(jwt.to_string());
    }
}

fn is_token_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-' || c == b'.'
}

/// 先把被换行截断的长 token 拼回去（清单里 JWT 常断成两行）。
fn stitch_wrapped_tokens(lines: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for raw in lines {
        let line = raw.trim_end();
        let trimmed = line.trim();
        // `.` 也算：JWT 由 `.` 分三段，断行断在哪一段里都有可能。
        let is_fragment = trimmed.len() >= 20 && trimmed.bytes().all(is_token_byte);
        // 只看上一行**最后一段**像不像 token —— 断行只可能断在行尾那一段上。
        // 拿整行去判会误伤：`a@example.com----eyJ…` 整行含 `@`，但断掉的确实是它的尾巴。
        let prev_has_token = out
            .last()
            .map(|p| p.rsplit("----").next().unwrap_or(p))
            .and_then(|seg| seg.split_whitespace().next_back())
            .is_some_and(|tail| {
                let tail = tail.trim();
                is_tokenish(tail) || (tail.len() >= 20 && tail.bytes().all(is_token_byte))
            });
        if is_fragment && prev_has_token {
            let last = out.last_mut().expect("prev_has_token 保证非空");
            last.push_str(trimmed);
        } else {
            out.push((*raw).to_string());
        }
    }
    out
}

/// 一行 `----` 分隔的自包含记录。
fn parse_delimited(line: &str) -> Option<ParsedAccount> {
    let segs: Vec<&str> = line
        .split("----")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let email = segs.iter().find(|s| is_email(s))?.to_ascii_lowercase();

    let mut acc = ParsedAccount {
        email: email.clone(),
        ..Default::default()
    };
    let mut plains: Vec<&str> = Vec::new();
    for seg in segs {
        if seg.eq_ignore_ascii_case(&email) {
            continue;
        }
        if is_tokenish(seg) {
            apply_token(&mut acc, seg);
        } else if is_timeish(seg) {
            // 重置时间是运营批注，不进凭证。
        } else {
            plains.push(seg);
        }
    }
    // 纯文本段按位置当密码：1 段 = Cursor 密码；≥2 段 = 邮箱密码 + Cursor 密码。
    match plains.len() {
        0 => {}
        1 => acc.cursor_password = Some(plains[0].to_string()),
        _ => {
            acc.email_password = Some(plains[0].to_string());
            acc.cursor_password = Some(plains[1].to_string());
        }
    }
    Some(acc)
}

/// `登录密码：X  邮箱密码：Y` 这种带标签的行。
fn apply_labeled(target: &mut ParsedAccount, line: &str) -> bool {
    let normalized = line.replace('：', ":");
    let mut hit = false;
    // 一行里可能同时有两个标签，按空白切开逐段看。
    for chunk in normalized.split_whitespace() {
        let Some((label, value)) = chunk.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if label.contains("登录密码") || label.contains("密码") && label.contains("cursor") {
            target.cursor_password = Some(value.to_string());
            hit = true;
        } else if label.contains("邮箱密码") {
            target.email_password = Some(value.to_string());
            hit = true;
        }
    }
    hit
}

/// 解析整份清单。返回（按邮箱合并后的记录，认不出的行）。
pub fn parse_dump(text: &str) -> (Vec<ParsedAccount>, Vec<String>) {
    // 结构化 JSON 的字段名自带含义 —— 先给它一次机会，
    // 别让下面按形状猜的那套去猜一份本来就标注清楚了的东西（见 `accounts_json`）。
    if let Some(parsed) = crate::accounts_json::parse(text) {
        return parsed;
    }

    let normalized = text.replace("%3A%3A", "::").replace("%3a%3a", "::");
    let raw_lines: Vec<&str> = normalized
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    let lines = stitch_wrapped_tokens(&raw_lines);

    let mut by_email: HashMap<String, ParsedAccount> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut current: Option<String> = None;

    for line in &lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // 1) 一行搞定的分隔式记录。
        if trimmed.contains("----") {
            if let Some(parsed) = parse_delimited(trimmed) {
                // 记住是谁：紧跟其后的批注行（「满了」「卖了」）该挂在这个号上，
                // 而不是被当成认不出的垃圾丢掉。
                current = Some(parsed.email.clone());
                merge(&mut by_email, &mut order, parsed);
                continue;
            }
        }

        // 2) 「第N个：邮箱」或任何含邮箱的行 —— 开一个新块。
        if let Some(email) = find_email(trimmed) {
            let entry = by_email.entry(email.clone()).or_insert_with(|| {
                order.push(email.clone());
                ParsedAccount {
                    email: email.clone(),
                    ..Default::default()
                }
            });
            // 同一行里可能还带着 token（`邮箱 eyJ...`）。
            for tok in trimmed.split_whitespace().filter(|t| is_tokenish(t)) {
                apply_token(entry, tok);
            }
            current = Some(email);
            continue;
        }

        // 3) 块内的后续行：带标签的密码、裸 token、批注。
        if let Some(email) = current.clone() {
            let entry = by_email.get_mut(&email).expect("current 一定已建过");
            if apply_labeled(entry, trimmed) {
                continue;
            }
            let tokens: Vec<&str> = trimmed
                .split_whitespace()
                .filter(|t| is_tokenish(t))
                .collect();
            if !tokens.is_empty() {
                for tok in tokens {
                    apply_token(entry, tok);
                }
                continue;
            }
            // 剩下的当批注。
            let note = trimmed.trim();
            entry.note = Some(match entry.note.take() {
                Some(prev) => format!("{prev}；{note}"),
                None => note.to_string(),
            });
            continue;
        }

        skipped.push(trimmed.to_string());
    }

    let accounts = order
        .into_iter()
        .filter_map(|e| by_email.remove(&e))
        .collect();
    (accounts, skipped)
}

/// 同一个号在清单里常出现多次，越靠后越新 → 后者覆盖（但不用空值覆盖非空）。
pub(crate) fn merge(
    by_email: &mut HashMap<String, ParsedAccount>,
    order: &mut Vec<String>,
    incoming: ParsedAccount,
) {
    let email = incoming.email.clone();
    match by_email.get_mut(&email) {
        Some(existing) => {
            if incoming.cursor_password.is_some() {
                existing.cursor_password = incoming.cursor_password;
            }
            if incoming.email_password.is_some() {
                existing.email_password = incoming.email_password;
            }
            if incoming.recovery_email.is_some() {
                existing.recovery_email = incoming.recovery_email;
            }
            if incoming.refresh_token.is_some() {
                existing.refresh_token = incoming.refresh_token;
            }
            if incoming.access_token.is_some() {
                existing.access_token = incoming.access_token;
            }
            if incoming.api_key.is_some() {
                existing.api_key = incoming.api_key;
            }
            if let Some(note) = incoming.note {
                existing.note = Some(match existing.note.take() {
                    Some(prev) => format!("{prev}；{note}"),
                    None => note,
                });
            }
        }
        None => {
            order.push(email.clone());
            by_email.insert(email, incoming);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn jwt(payload: serde_json::Value) -> String {
        let enc = |v: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        format!(
            "{}.{}.{}",
            enc(br#"{"alg":"HS256"}"#),
            enc(payload.to_string().as_bytes()),
            enc(b"sig")
        )
    }

    #[test]
    fn parses_the_common_three_column_form() {
        let (accounts, skipped) =
            parse_dump("a@example.com----邮箱密码1----Cursor密码1\nb@example.com----仅密码");
        assert!(skipped.is_empty());
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].email, "a@example.com");
        assert_eq!(accounts[0].email_password.as_deref(), Some("邮箱密码1"));
        assert_eq!(accounts[0].cursor_password.as_deref(), Some("Cursor密码1"));
        // 只有一段纯文本时按 Cursor 密码算 —— 那是更有用的那个。
        assert_eq!(accounts[1].cursor_password.as_deref(), Some("仅密码"));
        assert!(accounts[1].email_password.is_none());
    }

    #[test]
    fn classifies_segments_by_shape_not_by_position() {
        let refresh = jwt(serde_json::json!({ "sub": "auth0|user_1" }));
        let text = format!("a@example.com----2026-08-07 01:01----{refresh}----pw");
        let (accounts, _) = parse_dump(&text);
        let a = &accounts[0];
        // 时间段被认出来丢掉，token 段进 token，剩下的纯文本才是密码。
        assert_eq!(a.refresh_token.as_deref(), Some(refresh.as_str()));
        assert_eq!(a.cursor_password.as_deref(), Some("pw"));
    }

    #[test]
    fn a_session_token_is_not_mistaken_for_a_refresh_token() {
        // `user_xxx::jwt` 和 type=web 的裸 JWT 都是会话侧的，收进来会过期成死号。
        let session = jwt(serde_json::json!({ "sub": "auth0|user_1", "type": "web" }));
        let (accounts, _) = parse_dump(&format!(
            "a@example.com----user_1::{session}\nb@example.com----{session}"
        ));
        for a in &accounts {
            assert!(a.access_token.is_some(), "{} 应当归为 access", a.email);
            assert!(a.refresh_token.is_none());
        }
    }

    #[test]
    fn an_api_key_is_recognised_separately() {
        let (accounts, _) = parse_dump("a@example.com----crsr_abc123DEF");
        assert_eq!(accounts[0].api_key.as_deref(), Some("crsr_abc123DEF"));
        assert!(accounts[0].cursor_password.is_none(), "key 不该被当成密码");
    }

    #[test]
    fn url_encoded_separators_are_decoded() {
        let session = jwt(serde_json::json!({ "sub": "auth0|user_1", "type": "web" }));
        let (accounts, _) = parse_dump(&format!("a@example.com----user_1%3A%3A{session}"));
        assert_eq!(accounts[0].access_token.as_deref(), Some(session.as_str()));
    }

    #[test]
    fn a_token_wrapped_across_two_lines_is_stitched_back() {
        let refresh = jwt(serde_json::json!({ "sub": "auth0|user_1" }));
        let (head, tail) = refresh.split_at(refresh.len() - 24);
        let text = format!("a@example.com----{head}\n{tail}");
        let (accounts, skipped) = parse_dump(&text);
        assert_eq!(accounts.len(), 1);
        assert!(skipped.is_empty());
        assert_eq!(accounts[0].refresh_token.as_deref(), Some(refresh.as_str()));
    }

    #[test]
    fn parses_the_numbered_block_form() {
        let text = "第1个：a@example.com\n登录密码：pw1  邮箱密码：epw1\n重置时间：2026-08-07\n\n第2个：b@example.com\n登录密码：pw2";
        let (accounts, skipped) = parse_dump(text);
        assert!(skipped.is_empty(), "块式清单不该有认不出的行：{skipped:?}");
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].email, "a@example.com");
        assert_eq!(accounts[0].cursor_password.as_deref(), Some("pw1"));
        assert_eq!(accounts[0].email_password.as_deref(), Some("epw1"));
        assert_eq!(accounts[1].cursor_password.as_deref(), Some("pw2"));
    }

    #[test]
    fn later_entries_win_but_do_not_erase_what_they_lack() {
        let text = "a@example.com----epw----oldpw\na@example.com----newpw";
        let (accounts, _) = parse_dump(text);
        assert_eq!(accounts.len(), 1, "同一个号只该出现一次");
        assert_eq!(accounts[0].cursor_password.as_deref(), Some("newpw"));
        assert_eq!(
            accounts[0].email_password.as_deref(),
            Some("epw"),
            "旧的邮箱密码要留着"
        );
    }

    #[test]
    fn annotations_become_notes_rather_than_being_dropped() {
        let text = "第1个：a@example.com\n登录密码：pw\n满了 超额 20%";
        let (accounts, _) = parse_dump(text);
        assert!(accounts[0].note.as_deref().unwrap().contains("超额"));
    }

    #[test]
    fn unrecognisable_lines_are_reported_not_fatal() {
        let text = "这是一份清单\n\na@example.com----pw\n随便写点什么";
        let (accounts, skipped) = parse_dump(text);
        assert_eq!(accounts.len(), 1, "一行看不懂不该让整批失败");
        assert_eq!(skipped, vec!["这是一份清单"]);
    }

    #[test]
    fn preview_splits_by_the_hosting_bar() {
        let refresh = jwt(serde_json::json!({ "sub": "auth0|user_1" }));
        let session = jwt(serde_json::json!({ "sub": "auth0|user_1", "type": "web" }));
        let text = format!(
            "ok1@example.com----{refresh}\n\
             ok2@example.com----pw\n\
             ok3@example.com----{session}\n\
             ok4@example.com----crsr_key123\n\
             第9个：bad3@example.com"
        );
        let (accepted, report) = preview(&text);

        assert_eq!(report.accepted_count, 4);
        assert_eq!(report.rejected_count, 1);
        assert_eq!(accepted.len(), 4);

        let by = |e: &str| report.rows.iter().find(|r| r.email == e).unwrap().clone();
        assert!(by("ok1@example.com").accepted && by("ok1@example.com").has_refresh);
        assert!(by("ok2@example.com").accepted && by("ok2@example.com").has_password);
        // 只有会话票也收：有效期内能用，界面标「仅会话」；到期退回待登录。
        let s = by("ok3@example.com");
        assert!(s.accepted && s.has_access && !s.has_refresh);
        assert!(s.reason.contains("仅 session token"));
        let k = by("ok4@example.com");
        assert!(k.accepted && k.has_api_key && !k.has_refresh);
        assert!(k.reason.contains("API Key"));
        assert!(by("bad3@example.com").reason.contains("缺凭证"));
    }

    #[test]
    fn an_empty_dump_yields_nothing_rather_than_erroring() {
        let (accepted, report) = preview("   \n\n  ");
        assert!(accepted.is_empty());
        assert!(report.rows.is_empty());
        assert!(report.skipped.is_empty());
    }

    #[test]
    fn email_detection_rejects_near_misses() {
        assert!(is_email("a@b.com"));
        assert!(!is_email("a@b"));
        assert!(!is_email("@b.com"));
        assert!(!is_email("a@@b.com"));
        assert!(!is_email("a b@c.com"));
        assert!(!is_email("a@b.com."));
    }

    #[test]
    fn timeish_covers_the_forms_seen_in_real_lists() {
        assert!(is_timeish("2026-08-07 01:01"));
        assert!(is_timeish("2026-08-07"));
        assert!(is_timeish("21:53:08"));
        assert!(is_timeish("08/11 16:34"));
        assert!(!is_timeish("pw123"));
        assert!(!is_timeish("Passw0rd"));
    }

    #[test]
    fn parsed_accounts_convert_into_the_repository_shape() {
        let (accepted, _) = preview("a@example.com----epw----pw");
        let n: NewAccount = accepted.into_iter().next().unwrap().into();
        assert_eq!(n.email, "a@example.com");
        assert_eq!(n.cursor_password.as_deref(), Some("pw"));
        assert_eq!(n.email_password.as_deref(), Some("epw"));
        assert!(n.qualify().is_ok());
    }
}
