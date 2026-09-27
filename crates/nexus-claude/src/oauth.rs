//! 跟 Anthropic 交换 OAuth、读资料和订阅额度。请求体对齐 Claude Code 的公开流程。
//!
//! 这几条走控制面那套握手（没有 ALPN），不是推理那条。换票、读资料、读额度
//! 都按 Claude Code 的 axios 客户端发头：`axios/1.15.2`，`Cache-Control: no-cache`。

use crate::protocol::{self, ClaudeQuota};
use crate::transport::{self, Profile, Response};
use nexus_core::{AppError, Result};
use serde_json::Value;

pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: i64,
    pub email: Option<String>,
    pub account_uuid: Option<String>,
}

pub async fn exchange_code(code: &str, verifier: &str, state: &str) -> Result<TokenSet> {
    // 字段顺序对齐 Claude Code 2.1.220 打到 platform.claude.com 的那份请求体。
    // 走结构体序列化，不经过会按键名排序的 Map。
    let body = ExchangeBody {
        grant_type: "authorization_code",
        code,
        redirect_uri: protocol::REDIRECT_URI,
        client_id: protocol::OAUTH_CLIENT_ID,
        code_verifier: verifier,
        state,
    };
    let value = post_token(&body, "交换 Claude 授权码").await?;
    token_set(&value)
}

pub async fn refresh(refresh_token: &str) -> Result<TokenSet> {
    let body = RefreshBody {
        client_id: protocol::OAUTH_CLIENT_ID,
        grant_type: "refresh_token",
        refresh_token,
        scope: &protocol::OAUTH_SCOPES.join(" "),
    };
    let value = post_token(&body, "刷新 Claude 登录").await?;
    token_set(&value)
}

#[derive(serde::Serialize)]
struct ExchangeBody<'a> {
    grant_type: &'a str,
    code: &'a str,
    redirect_uri: &'a str,
    client_id: &'a str,
    code_verifier: &'a str,
    state: &'a str,
}

#[derive(serde::Serialize)]
struct RefreshBody<'a> {
    client_id: &'a str,
    grant_type: &'a str,
    refresh_token: &'a str,
    scope: &'a str,
}

pub async fn profile(access_token: &str) -> Result<Value> {
    let resp = api_get(protocol::PROFILE_URL, access_token)
        .await
        .map_err(|e| AppError::upstream(format!("读取 Claude 资料失败：{e}")))?;
    read_json(resp, "读取 Claude 资料").await
}

/// 换票之后 Claude Code 会再查一次 `claude_cli/roles`。内容不参与登录，调用方失败就记下。
pub async fn roles(access_token: &str) -> Result<Value> {
    let resp = api_get(protocol::ROLES_URL, access_token)
        .await
        .map_err(|e| AppError::upstream(format!("读取 Claude 角色失败：{e}")))?;
    read_json(resp, "读取 Claude 角色").await
}

pub async fn usage(access_token: &str) -> Result<ClaudeQuota> {
    let resp = api_get(protocol::USAGE_URL, access_token)
        .await
        .map_err(|e| AppError::upstream(format!("读取 Claude 额度失败：{e}")))?;
    let value = read_json(resp, "读取 Claude 额度").await?;
    Ok(protocol::parse_quota(&value))
}

async fn api_get(url: &str, access_token: &str) -> Result<Response> {
    let headers = vec![
        ("accept".into(), "application/json, text/plain, */*".into()),
        ("content-type".into(), "application/json".into()),
        ("authorization".into(), format!("Bearer {access_token}")),
        ("cache-control".into(), "no-cache".into()),
        ("user-agent".into(), "axios/1.15.2".into()),
        (
            "accept-encoding".into(),
            "gzip, compress, deflate, br".into(),
        ),
    ];
    transport::request(Profile::Control, "GET", url, &headers, None)
        .await
        .map_err(|e| AppError::upstream(e.to_string()))
}

async fn post_token(body: &impl serde::Serialize, what: &str) -> Result<Value> {
    // 换票走 Claude Code 的 axios 客户端，不是推理那套 claude-cli 头。
    let bytes = serde_json::to_vec(body)
        .map_err(|e| AppError::upstream(format!("{what}失败：请求体编码失败：{e}")))?;
    let headers = vec![
        ("accept".into(), "application/json, text/plain, */*".into()),
        ("content-type".into(), "application/json".into()),
        ("user-agent".into(), "axios/1.15.2".into()),
        ("accept-encoding".into(), "gzip, deflate, br".into()),
    ];
    let resp = transport::request(
        Profile::Control,
        "POST",
        protocol::TOKEN_URL,
        &headers,
        Some(&bytes),
    )
    .await
    .map_err(|e| AppError::upstream(format!("{what}失败：{e}")))?;
    read_json(resp, what).await
}

async fn read_json(resp: Response, what: &str) -> Result<Value> {
    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        let detail = text.chars().take(280).collect::<String>();
        return Err(
            AppError::upstream(format!("{what}失败：HTTP {status} {detail}"))
                .with_hint("授权码只能用一次。过期了就重新打开授权页。"),
        );
    }
    serde_json::from_str(&text)
        .map_err(|e| AppError::upstream(format!("{what}的响应不是 JSON（{e}）")))
}

fn token_set(value: &Value) -> Result<TokenSet> {
    let access = value
        .get("access_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::upstream("Claude 没有返回 access_token。"))?;
    let refresh = value
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let expires_in = value
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(3600);
    let email = value
        .pointer("/account/email_address")
        .or_else(|| value.pointer("/account/email"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let account_uuid = value
        .pointer("/account/uuid")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(TokenSet {
        access_token: access.to_string(),
        refresh_token: refresh,
        expires_in,
        email,
        account_uuid,
    })
}
