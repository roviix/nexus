//! 登录态保活和一轮网页生成。
//!
//! `__Secure-1PSID` 是长期身份。真正会过期的是 `__Secure-1PSIDTS`（大约 30 分钟）。
//! 导入后先打 accounts.google.com 的哨兵轮转把它换出来，再去要页面里的 XSRF。

use crate::protocol::{
    extract_bl, extract_upstream_model, extract_xsrf, is_xsrf_error, merge_set_cookie,
    normalize_cookie, parse_follow, prepare_follow, reasoning_in_line, texts_in_line, DeltaTracker,
    FollowUp, CHROME_UA, DEFAULT_BL, PAGE_URL,
};
use reqwest::header::SET_COOKIE;

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .http1_only()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// 只有 1PSID 时也去换一张短票。失败不阻断：有的浏览器导出会被设备绑定拒绝。
pub async fn refresh_session(client: &reqwest::Client, cookie: &str) -> String {
    let cookie = normalize_cookie(cookie);
    if crate::protocol::cookie_value(&cookie, "__Secure-1PSID").is_none() {
        return cookie;
    }
    let subset = cookie_subset(&cookie, &["__Secure-1PSID", "__Secure-1PSIDTS"]);
    let resp = client
        .post("https://accounts.google.com/RotateCookies")
        .header("Accept", "*/*")
        .header("Content-Type", "application/json")
        .header("Origin", "https://accounts.google.com")
        .header("User-Agent", CHROME_UA)
        .header("Cookie", subset)
        .body("[000,\"-0000000000000000000\"]")
        .send()
        .await;
    let Ok(resp) = resp else { return cookie };
    if !resp.status().is_success() {
        return cookie;
    }
    let set_cookies = set_cookie_headers(&resp);
    merge_set_cookie(&cookie, &set_cookies)
}

pub async fn open_app(client: &reqwest::Client, cookie: &str) -> Result<(String, String, String), String> {
    let resp = client
        .get(PAGE_URL)
        .header("User-Agent", CHROME_UA)
        .header("Accept", "text/html")
        .header("Cookie", cookie)
        .send()
        .await
        .map_err(|err| format!("打开 Gemini 网页失败：{err}"))?;
    if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 {
        return Err("Cookie 已被 Gemini 拒绝".into());
    }
    let set_cookies = set_cookie_headers(&resp);
    let html = resp.text().await.map_err(|err| format!("读取 Gemini 页面失败：{err}"))?;
    let cookie = merge_set_cookie(cookie, &set_cookies);
    let xsrf = extract_xsrf(&html).ok_or_else(|| {
        "页面里没有登录令牌。只贴 __Secure-1PSID 时，需要先换到短票；若仍没有，这份票已经失效。".to_string()
    })?;
    let bl = extract_bl(&html).unwrap_or_else(|| DEFAULT_BL.to_string());
    Ok((cookie, xsrf, bl))
}

pub struct TurnOutput {
    pub text: String,
    pub thinking: String,
    pub upstream_model: Option<String>,
    pub cookie: String,
    pub follow: Option<FollowUp>,
}

pub async fn generate(
    client: &reqwest::Client,
    cookie: &str,
    model: &str,
    prompt: &str,
    follow: Option<&FollowUp>,
    on_piece: &mut (dyn FnMut(bool, String) + Send),
) -> Result<TurnOutput, String> {
    let (cookie, xsrf, bl) = open_app(client, cookie).await?;
    let mut prepared = prepare_follow(prompt, model, &cookie, &xsrf, &bl, follow);
    let mut resp = post(client, &prepared).await?;
    if resp.status().as_u16() == 400 {
        let set_cookies = set_cookie_headers(&resp);
        let cookie = merge_set_cookie(&cookie, &set_cookies);
        let body = resp.text().await.unwrap_or_default();
        if is_xsrf_error(&body) {
            let (cookie, xsrf, bl) = open_app(client, &cookie).await?;
            prepared = prepare_follow(prompt, model, &cookie, &xsrf, &bl, follow);
            resp = post(client, &prepared).await?;
        } else {
            return Err(format!("Gemini 网页返回 400：{}", clip(&body, 240)));
        }
    }
    let status = resp.status();
    if status.as_u16() == 401 || status.as_u16() == 403 {
        return Err("Cookie 已被 Gemini 拒绝".into());
    }
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("Gemini 网页返回 {status}：{}", clip(&body, 240)));
    }
    let set_cookies = set_cookie_headers(&resp);
    let cookie = merge_set_cookie(&cookie, &set_cookies);
    let mut text_tracker = DeltaTracker::default();
    let mut think_tracker = DeltaTracker::default();
    let mut text = String::new();
    let mut thinking = String::new();
    let mut upstream = None;
    let mut raw = String::new();
    let mut buf = String::new();
    let mut resp = resp;
    while let Some(chunk) = resp.chunk().await.map_err(|err| format!("读取网页流失败：{err}"))? {
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(idx) = buf.find('\n') {
            let line = buf[..idx].to_string();
            buf.drain(..=idx);
            raw.push_str(&line);
            raw.push('\n');
            ingest(&line, &mut text_tracker, &mut think_tracker, &mut text, &mut thinking, &mut upstream, on_piece);
        }
    }
    if !buf.trim().is_empty() {
        raw.push_str(&buf);
        ingest(&buf, &mut text_tracker, &mut think_tracker, &mut text, &mut thinking, &mut upstream, on_piece);
    }
    if text.is_empty() && thinking.is_empty() {
        return Err(format!("Gemini 网页没有返回内容帧（{} 字节）", raw.len()));
    }
    let next_turn = follow.map(|f| f.turn + 1).unwrap_or(1);
    Ok(TurnOutput {
        text,
        thinking,
        upstream_model: upstream,
        cookie,
        follow: parse_follow(&raw, next_turn),
    })
}

fn ingest(
    line: &str,
    text_tracker: &mut DeltaTracker,
    think_tracker: &mut DeltaTracker,
    text: &mut String,
    thinking: &mut String,
    upstream: &mut Option<String>,
    on_piece: &mut (dyn FnMut(bool, String) + Send),
) {
    if let Some(name) = extract_upstream_model(line) {
        *upstream = Some(name);
    }
    if let Some(full) = reasoning_in_line(line) {
        let delta = think_tracker.push(&full);
        if !delta.is_empty() {
            thinking.push_str(&delta);
            on_piece(true, delta);
        }
    }
    for full in texts_in_line(line) {
        let delta = text_tracker.push(&full);
        if !delta.is_empty() {
            text.push_str(&delta);
            on_piece(false, delta);
        }
    }
}

async fn post(client: &reqwest::Client, prepared: &crate::protocol::PreparedRequest) -> Result<reqwest::Response, String> {
    let mut req = client.post(&prepared.url).body(prepared.body.clone());
    for (key, value) in &prepared.headers {
        req = req.header(key, value);
    }
    req.send().await.map_err(|err| format!("连接 Gemini 网页失败：{err}"))
}

fn set_cookie_headers(resp: &reqwest::Response) -> Vec<String> {
    resp.headers()
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok().map(|s| s.to_string()))
        .collect()
}

fn cookie_subset(cookie: &str, names: &[&str]) -> String {
    names
        .iter()
        .filter_map(|name| crate::protocol::cookie_value(cookie, name).map(|value| format!("{name}={value}")))
        .collect::<Vec<_>>()
        .join("; ")
}

fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}
