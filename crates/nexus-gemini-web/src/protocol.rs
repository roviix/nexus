//! Gemini 网页 `StreamGenerate` 协议。
//!
//! 形状对齐浏览器抓包：97 槽 inner 数组、模型 header 压过 inner[79]、
//! 登录态必须带 `at`（页面里的 SNlM0e）。响应是 wrb.fr 累积全文，要自己做前缀差分。

use serde::Serialize;
use serde_json::{json, Value};
use sha1::{Digest, Sha1};

pub const DEFAULT_BL: &str = "boq_gemini-web-uiserver_20261007.01_p0";
pub const PAGE_URL: &str = "https://gemini.google.com/app";
pub const CHROME_UA: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.0.0 Safari/537.36";

const HEX_FLASH_36: &str = "fbb127bbb056c959";
const HEX_FLASH_LITE: &str = "cf41b0e0dd7d53e5";
const HEX_PRO_31: &str = "9d8ca3786ebdfbea";
const HEX_FLASH_38: &str = "56fdd199312815e2";
const HEX_ARGON_4: &str = "95a4c0abcba24364";

const THINK_NORMAL: i64 = 1;
const THINK_EXTENDED: i64 = 2;
const TOOL_IMAGE: i64 = 14;
const TOOL_MUSIC: i64 = 21;
const TOOL_CANVAS: i64 = 2;
const TOOL_VIDEO: i64 = 11;
const INNER_SLOTS: usize = 97;

pub const ROUTE_PREFIXES: &[&str] = &["gemini-web/", "gweb/"];

#[derive(Debug, Clone, Copy)]
pub struct ModelSpec {
    pub id: &'static str,
    pub hex: &'static str,
    pub mode: i64,
    pub thinking: bool,
    pub tool: i64,
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "gemini-3.6-flash",
        hex: HEX_FLASH_36,
        mode: 1,
        thinking: false,
        tool: 0,
    },
    ModelSpec {
        id: "gemini-3.5-flash-lite",
        hex: HEX_FLASH_LITE,
        mode: 6,
        thinking: false,
        tool: 0,
    },
    ModelSpec {
        id: "gemini-3.1-pro",
        hex: HEX_PRO_31,
        mode: 3,
        thinking: false,
        tool: 0,
    },
    ModelSpec {
        id: "gemini-3.8-flash",
        hex: HEX_FLASH_38,
        mode: 1,
        thinking: false,
        tool: 0,
    },
    ModelSpec {
        id: "gemini-3.7-flash",
        hex: HEX_FLASH_38,
        mode: 1,
        thinking: false,
        tool: 0,
    },
    ModelSpec {
        id: "gemini-4-argon",
        hex: HEX_ARGON_4,
        mode: 8,
        thinking: false,
        tool: 0,
    },
    ModelSpec {
        id: "gemini-3.6-flash-thinking",
        hex: HEX_FLASH_36,
        mode: 1,
        thinking: true,
        tool: 0,
    },
    ModelSpec {
        id: "gemini-3.5-flash-lite-thinking",
        hex: HEX_FLASH_LITE,
        mode: 6,
        thinking: true,
        tool: 0,
    },
    ModelSpec {
        id: "gemini-3.1-pro-thinking",
        hex: HEX_PRO_31,
        mode: 3,
        thinking: true,
        tool: 0,
    },
    ModelSpec {
        id: "gemini-3.8-flash-thinking",
        hex: HEX_FLASH_38,
        mode: 1,
        thinking: true,
        tool: 0,
    },
    ModelSpec {
        id: "gemini-4-argon-thinking",
        hex: HEX_ARGON_4,
        mode: 8,
        thinking: true,
        tool: 0,
    },
    ModelSpec {
        id: "gemini-image",
        hex: HEX_FLASH_36,
        mode: 1,
        thinking: false,
        tool: TOOL_IMAGE,
    },
    ModelSpec {
        id: "gemini-music",
        hex: HEX_FLASH_36,
        mode: 1,
        thinking: false,
        tool: TOOL_MUSIC,
    },
    ModelSpec {
        id: "gemini-video",
        hex: HEX_FLASH_36,
        mode: 1,
        thinking: false,
        tool: TOOL_VIDEO,
    },
    ModelSpec {
        id: "gemini-canvas",
        hex: HEX_FLASH_36,
        mode: 1,
        thinking: false,
        tool: TOOL_CANVAS,
    },
];

pub fn owns_model(model: &str) -> bool {
    let id = strip_prefix(model);
    MODELS.iter().any(|m| m.id.eq_ignore_ascii_case(id))
}

pub fn resolve_model(model: &str) -> ModelSpec {
    let id = strip_prefix(model);
    MODELS
        .iter()
        .copied()
        .find(|m| m.id.eq_ignore_ascii_case(id))
        .unwrap_or(MODELS[0])
}

fn strip_prefix(model: &str) -> &str {
    let trimmed = model.trim();
    let lower = trimmed.to_ascii_lowercase();
    for prefix in ROUTE_PREFIXES {
        if lower.starts_with(prefix) {
            return &trimmed[prefix.len()..];
        }
    }
    trimmed
}

pub fn catalog_ids() -> Vec<String> {
    MODELS.iter().map(|m| m.id.to_string()).collect()
}

/// `SAPISIDHASH <unix>_<sha1>`。明文是 `{秒} {SAPISID} https://gemini.google.com`。
pub fn sapisid_hash(sapisid: &str, unix_secs: i64) -> String {
    let mut hasher = Sha1::new();
    hasher.update(format!("{unix_secs} {sapisid} https://gemini.google.com").as_bytes());
    let digest = hasher.finalize();
    format!("SAPISIDHASH {unix_secs}_{digest:x}")
}

pub fn cookie_value(cookie: &str, name: &str) -> Option<String> {
    for part in cookie.split(';') {
        let part = part.trim();
        let (key, value) = part.split_once('=')?;
        if key.trim() == name {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

pub fn account_ref_from_cookie(cookie: &str) -> Option<String> {
    cookie_value(cookie, "__Secure-1PSID")
}

#[derive(Debug, Clone, Serialize)]
pub struct PreparedRequest {
    pub url: String,
    pub body: String,
    pub headers: Vec<(String, String)>,
    pub model_id: String,
}

pub fn prepare_request(
    prompt: &str,
    model: &str,
    cookie: &str,
    xsrf: &str,
    bl: &str,
) -> PreparedRequest {
    prepare_follow(prompt, model, cookie, xsrf, bl, None)
}

/// 续接一轮已有网页会话。`turn` 从 1 起，只发新消息，历史留在服务端。
#[derive(Debug, Clone)]
pub struct FollowUp {
    pub cid: String,
    pub rid: String,
    pub rcid: String,
    pub tok26: String,
    pub turn: i64,
}

pub fn prepare_follow(
    prompt: &str,
    model: &str,
    cookie: &str,
    xsrf: &str,
    bl: &str,
    follow: Option<&FollowUp>,
) -> PreparedRequest {
    let spec = resolve_model(model);
    let req_uuid = uuid::Uuid::new_v4().to_string();
    let header_uuid = uuid::Uuid::new_v4().to_string();
    let think = if spec.thinking {
        THINK_EXTENDED
    } else {
        THINK_NORMAL
    };
    let inner = build_inner(prompt, &spec, &req_uuid, think, follow);
    let outer = json!([Value::Null, inner.to_string()]);
    let mut body = format!("f.req={}", urlencoding(&outer.to_string()));
    if !xsrf.is_empty() {
        body.push_str("&at=");
        body.push_str(&urlencoding(xsrf));
    }
    let reqid = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() % 1_000_000)
        .unwrap_or(1);
    let bl = if bl.is_empty() { DEFAULT_BL } else { bl };
    let url = format!(
        "https://gemini.google.com/_/BardChatUi/data/assistant.lamda.BardFrontendService/StreamGenerate?bl={bl}&hl=en&_reqid={reqid}&rt=c"
    );
    let mut headers = vec![
        ("Accept".into(), "*/*".into()),
        ("Accept-Language".into(), "en-US,en;q=0.9".into()),
        (
            "Content-Type".into(),
            "application/x-www-form-urlencoded;charset=UTF-8".into(),
        ),
        ("Origin".into(), "https://gemini.google.com".into()),
        ("Referer".into(), "https://gemini.google.com/app".into()),
        ("User-Agent".into(), CHROME_UA.into()),
        (
            "Sec-CH-UA".into(),
            "\"Chromium\";v=\"146\", \"Google Chrome\";v=\"146\", \"Not?A_Brand\";v=\"24\"".into(),
        ),
        ("Sec-CH-UA-Mobile".into(), "?0".into()),
        ("Sec-CH-UA-Platform".into(), "\"macOS\"".into()),
        ("Sec-Fetch-Dest".into(), "empty".into()),
        ("Sec-Fetch-Mode".into(), "cors".into()),
        ("Sec-Fetch-Site".into(), "same-origin".into()),
        ("X-Same-Domain".into(), "1".into()),
        ("X-Goog-AuthUser".into(), "0".into()),
        ("x-goog-ext-73010989-jspb".into(), "[0]".into()),
        ("x-goog-ext-73010990-jspb".into(), "[0,0,0]".into()),
        (
            "x-goog-ext-525001261-jspb".into(),
            format!(
                r#"[1,null,null,null,"{}",null,null,0,[4,5,6,8],null,null,1,null,null,{},{},"{}"]"#,
                spec.hex, spec.mode, think, header_uuid
            ),
        ),
        (
            "x-goog-ext-525005358-jspb".into(),
            format!(r#"["{req_uuid}",1]"#),
        ),
    ];
    if !cookie.is_empty() {
        headers.push(("Cookie".into(), cookie.to_string()));
    }
    if let Some(sapisid) = cookie_value(cookie, "SAPISID") {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        headers.push(("Authorization".into(), sapisid_hash(&sapisid, secs)));
    }
    PreparedRequest {
        url,
        body,
        headers,
        model_id: spec.id.to_string(),
    }
}

fn build_inner(
    prompt: &str,
    spec: &ModelSpec,
    req_uuid: &str,
    think: i64,
    follow: Option<&FollowUp>,
) -> Value {
    let mut inner = vec![Value::Null; INNER_SLOTS];
    inner[0] = json!([
        prompt,
        0,
        Value::Null,
        Value::Null,
        Value::Null,
        Value::Null,
        0
    ]);
    inner[1] = json!(["en"]);
    let turn = follow.map(|f| f.turn).unwrap_or(0);
    if let Some(follow) = follow.filter(|f| f.turn > 0 && !f.cid.is_empty()) {
        inner[2] = json!([
            follow.cid,
            follow.rid,
            follow.rcid,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            follow.tok26
        ]);
    } else {
        inner[2] = json!([
            "",
            "",
            "",
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            Value::Null,
            ""
        ]);
    }
    inner[6] = json!([0]);
    inner[7] = json!(1);
    inner[10] = json!(1);
    inner[11] = json!(0);
    inner[17] = json!([[turn]]);
    inner[18] = json!(0);
    inner[27] = json!(1);
    inner[30] = json!([4]);
    inner[41] = json!([1]);
    inner[53] = json!(0);
    inner[59] = json!(req_uuid);
    inner[61] = json!([]);
    inner[68] = json!(1);
    inner[79] = json!(spec.mode);
    inner[80] = json!(think);
    inner[91] = json!(0);
    inner[96] = json!(if spec.thinking { 1 } else { 0 });
    if spec.tool > 0 {
        inner[49] = json!(spec.tool);
    }
    if spec.tool == TOOL_VIDEO {
        inner[55] = json!([[16]]);
    }
    Value::Array(inner)
}

pub fn extract_xsrf(html: &str) -> Option<String> {
    capture(html, "\"SNlM0e\":\"", 10, 200)
}

pub fn extract_bl(html: &str) -> Option<String> {
    let value = capture(html, "\"cfb2h\":\"", 10, 120)?;
    if value.starts_with("boq_") {
        Some(value)
    } else {
        None
    }
}

fn capture(html: &str, marker: &str, min: usize, max: usize) -> Option<String> {
    let start = html.find(marker)? + marker.len();
    let rest = &html[start..];
    let end = rest.find('"')?;
    let value = &rest[..end];
    if value.len() < min || value.len() > max {
        return None;
    }
    Some(value.to_string())
}

/// 累积全文 → 增量。上游每一帧都是目前为止的全文。
#[derive(Default)]
pub struct DeltaTracker {
    emitted: String,
}

impl DeltaTracker {
    pub fn push(&mut self, full: &str) -> String {
        if full.starts_with(&self.emitted) {
            let delta = full[self.emitted.len()..].to_string();
            self.emitted = full.to_string();
            delta
        } else if full.len() > self.emitted.len() {
            self.emitted = full.to_string();
            full.to_string()
        } else {
            String::new()
        }
    }
}

pub fn texts_in_line(line: &str) -> Vec<String> {
    walk_frame(line, |inner| {
        let parts = inner.get(4)?.as_array()?;
        let mut texts = Vec::new();
        for part in parts {
            let Some(list) = part
                .as_array()
                .and_then(|p| p.get(1))
                .and_then(|v| v.as_array())
            else {
                continue;
            };
            for item in list {
                if let Some(text) = item.as_str() {
                    if !text.is_empty() {
                        texts.push(text.to_string());
                    }
                }
            }
        }
        Some(texts)
    })
    .unwrap_or_default()
}

pub fn reasoning_in_line(line: &str) -> Option<String> {
    walk_frame(line, |inner| {
        let text = inner.get(4)?.get(0)?.get(37)?.get(0)?.get(0)?.as_str()?;
        if text.is_empty() {
            None
        } else {
            Some(text.to_string())
        }
    })
}

fn walk_frame<T>(line: &str, pick: impl FnOnce(&Vec<Value>) -> Option<T>) -> Option<T> {
    if !line.contains("wrb.fr") || line.len() < 40 {
        return None;
    }
    let arr: Vec<Value> = serde_json::from_str(line).ok()?;
    let first = arr.first()?.as_array()?;
    let inner_str = first.get(2)?.as_str()?;
    let inner: Vec<Value> = serde_json::from_str(inner_str).ok()?;
    pick(&inner)
}

pub fn is_xsrf_error(body: &str) -> bool {
    body.contains("xsrf") || body.contains("48448350")
}

/// 用户只粘了 `__Secure-1PSID` 的值时，补成 Cookie 头。
pub fn normalize_cookie(raw: &str) -> String {
    let raw = raw.trim().trim_matches('"');
    if raw.contains('=') {
        return raw.to_string();
    }
    if raw.is_empty() {
        return String::new();
    }
    format!("__Secure-1PSID={raw}")
}

pub fn merge_set_cookie(cookie: &str, set_cookies: &[String]) -> String {
    if set_cookies.is_empty() {
        return cookie.to_string();
    }
    let mut updates = std::collections::HashMap::new();
    for header in set_cookies {
        let first = header.split(';').next().unwrap_or("").trim();
        let Some((name, value)) = first.split_once('=') else {
            continue;
        };
        if value.is_empty()
            && header
                .to_ascii_lowercase()
                .contains("expires=thu, 01 jan 1970")
        {
            continue;
        }
        updates.insert(name.to_string(), value.to_string());
    }
    if updates.is_empty() {
        return cookie.to_string();
    }
    let mut parts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for pair in cookie.split(';') {
        let pair = pair.trim();
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        seen.insert(name.to_string());
        if let Some(updated) = updates.get(name) {
            parts.push(format!("{name}={updated}"));
        } else {
            parts.push(format!("{name}={value}"));
        }
    }
    for (name, value) in &updates {
        if !seen.contains(name) {
            parts.push(format!("{name}={value}"));
        }
    }
    parts.join("; ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTool {
    pub name: String,
    pub arguments: String,
}

/// 网页协议没有原生 function call。模型按约定吐 ```tool_call``` 围栏，这里拆出来。
pub fn parse_tool_calls(text: &str) -> (String, Vec<ParsedTool>) {
    let mut calls = Vec::new();
    let mut clean = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("```tool_call") {
        clean.push_str(&rest[..start]);
        let after = &rest[start + "```tool_call".len()..];
        let Some(end) = after.find("```") else {
            clean.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let body = after[..end].trim();
        if let Ok(value) = serde_json::from_str::<Value>(body) {
            if let Some(name) = value.get("name").and_then(|v| v.as_str()) {
                let arguments = value.get("arguments").cloned().unwrap_or(json!({}));
                calls.push(ParsedTool {
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                });
            }
        }
        rest = &after[end + 3..];
    }
    clean.push_str(rest);
    (clean.trim().to_string(), calls)
}

pub fn parse_follow(raw: &str, turn: i64) -> Option<FollowUp> {
    let mut found: Option<FollowUp> = None;
    for line in raw.lines() {
        let line = line.trim();
        if !line.starts_with("[[") {
            continue;
        }
        let Ok(arr) = serde_json::from_str::<Vec<Value>>(line) else {
            continue;
        };
        for item in arr {
            let Some(row) = item.as_array() else { continue };
            if row.first().and_then(|v| v.as_str()) != Some("wrb.fr") || row.len() < 3 {
                continue;
            }
            let Some(payload) = row.get(2).and_then(|v| v.as_str()) else {
                continue;
            };
            let Ok(frame) = serde_json::from_str::<Vec<Value>>(payload) else {
                continue;
            };
            let mut follow = found.clone().unwrap_or(FollowUp {
                cid: String::new(),
                rid: String::new(),
                rcid: String::new(),
                tok26: String::new(),
                turn,
            });
            if let Some(meta) = frame.get(1).and_then(|v| v.as_array()) {
                if let Some(cid) = meta.first().and_then(|v| v.as_str()) {
                    if !cid.is_empty() {
                        follow.cid = cid.to_string();
                    }
                }
                if let Some(rid) = meta.get(1).and_then(|v| v.as_str()) {
                    if !rid.is_empty() {
                        follow.rid = rid.to_string();
                    }
                }
            }
            if let Some(tok) = frame.get(26).and_then(|v| v.as_str()) {
                follow.tok26 = tok.to_string();
            }
            found = Some(follow);
        }
    }
    let mut follow = found?;
    if follow.cid.is_empty() {
        return None;
    }
    if let Some(rcid) = raw.split("rc_").nth(1) {
        let id: String = rcid
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
            .collect();
        if id.len() >= 6 {
            follow.rcid = format!("rc_{id}");
        }
    }
    follow.turn = turn;
    Some(follow)
}

pub fn extract_upstream_model(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let needle = b"\\\",null,null,\\\"";
    let mut last = None;
    let mut i = 0;
    while i + 16 < bytes.len() {
        if bytes[i..].starts_with(b"\\\"") {
            let hex_start = i + 2;
            let hex_end = hex_start + 16;
            if hex_end + needle.len() <= bytes.len()
                && bytes[hex_start..hex_end]
                    .iter()
                    .all(|b| b.is_ascii_hexdigit())
                && bytes[hex_end..hex_end + needle.len()] == *needle
            {
                let name_start = hex_end + needle.len();
                if let Some(rel) = raw[name_start..].find("\\\"") {
                    let name = &raw[name_start..name_start + rel];
                    if !name.is_empty() && name.len() <= 40 {
                        last = Some(name.to_string());
                    }
                }
            }
        }
        i += 1;
    }
    last
}

fn urlencoding(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pro_and_flash_keep_their_hex() {
        assert_eq!(resolve_model("gemini-web/gemini-3.1-pro").hex, HEX_PRO_31);
        assert_eq!(resolve_model("gemini-3.6-flash").mode, 1);
        assert!(resolve_model("gemini-3.1-pro-thinking").thinking);
        assert_eq!(resolve_model("gemini-image").tool, TOOL_IMAGE);
        assert!(!owns_model("claude-opus-4-6"));
        assert!(owns_model("gweb/gemini-4-argon"));
    }

    #[test]
    fn wrb_line_yields_text_and_reasoning() {
        let mut part = vec![Value::Null; 38];
        part[1] = json!(["hello"]);
        part[37] = json!([["ponder"]]);
        let inner = json!([Value::Null, Value::Null, Value::Null, Value::Null, [part]]);
        let line = json!([["wrb.fr", Value::Null, inner.to_string()]]).to_string();
        assert_eq!(texts_in_line(&line), vec!["hello".to_string()]);
        assert_eq!(reasoning_in_line(&line).as_deref(), Some("ponder"));
    }

    #[test]
    fn tracker_emits_only_the_suffix() {
        let mut tracker = DeltaTracker::default();
        assert_eq!(tracker.push("hel"), "hel");
        assert_eq!(tracker.push("hello"), "lo");
        assert_eq!(tracker.push("hello"), "");
    }

    #[test]
    fn cookie_and_tools_round_trip() {
        assert_eq!(normalize_cookie("abc"), "__Secure-1PSID=abc");
        assert_eq!(
            merge_set_cookie(
                "SID=a; __Secure-1PSIDTS=old",
                &["__Secure-1PSIDTS=new; Path=/".into()]
            ),
            "SID=a; __Secure-1PSIDTS=new"
        );
        let (text, calls) = parse_tool_calls(
            "before\n```tool_call\n{\"name\":\"exec\",\"arguments\":{\"cmd\":\"ls\"}}\n```\nafter",
        );
        assert!(text.contains("before") && text.contains("after"));
        assert!(!text.contains("tool_call"));
        assert_eq!(calls[0].name, "exec");
    }
}
