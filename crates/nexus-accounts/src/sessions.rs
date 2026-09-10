//! Cursor 活跃会话：列出 / 踢掉。
//!
//! 走 `aiserver.v1.AuthService`（Connect JSON unary），和 DashboardService 铸 API Key
//! 同一条路：`Bearer access_token` + `connect-protocol-version: 1`，不需要 IDE checksum。
//!
//! 「踢掉其它」时用 access JWT 里的 `workosSessionId` 认出当前这把，尽量留下；
//! 认不出就全踢，调用方再验证 refresh 还活不活。

use nexus_core::{AppError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

const AUTH_SERVICE: &str = "https://api2.cursor.sh/aiserver.v1.AuthService";
const TIMEOUT: Duration = Duration::from_secs(20);

/// 一条 Cursor 侧的活跃会话。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveSession {
    pub session_id: String,
    /// proto 枚举名（`SESSION_TYPE_CLIENT`）或数字；踢人时原样回传。
    pub session_type: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// 是不是我们手里这把（按 JWT 的 `workosSessionId` 对上的）。
    pub is_current: bool,
}

/// 一键踢会话的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KickOutcome {
    pub listed: u32,
    pub revoked: u32,
    pub kept: u32,
    pub failed: u32,
    /// 当前会话 id 没对上任何一条时为 true——这次等于全踢了。
    pub kept_current: bool,
    /// 踢完之后 refresh 是否还能换到 access。全踢时可能废掉，需要重新授权。
    pub refresh_alive: bool,
}

pub async fn list_active(
    http: &reqwest::Client,
    access_token: &str,
    current_session_id: Option<&str>,
) -> Result<Vec<ActiveSession>> {
    let json = auth_call(
        http,
        access_token,
        "ListActiveSessions",
        &Value::Object(Default::default()),
    )
    .await?;
    let rows = json
        .get("sessions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(obj) = row.as_object() else { continue };
        let session_id = pick_str(obj, &["sessionId", "session_id"]).unwrap_or_default();
        if session_id.is_empty() {
            continue;
        }
        let session_type = obj
            .get("type")
            .cloned()
            .unwrap_or(Value::String("SESSION_TYPE_UNSPECIFIED".into()));
        let is_current = current_session_id.is_some_and(|id| id == session_id);
        out.push(ActiveSession {
            session_id,
            session_type,
            created_at: pick_str(obj, &["createdAt", "created_at"]),
            expires_at: pick_str(obj, &["expiresAt", "expires_at"]),
            is_current,
        });
    }
    Ok(out)
}

pub async fn revoke_one(
    http: &reqwest::Client,
    access_token: &str,
    session_id: &str,
    session_type: &Value,
) -> Result<()> {
    let body = serde_json::json!({
        "sessionId": session_id,
        "type": session_type,
    });
    let json = auth_call(http, access_token, "RevokeSession", &body).await?;
    // 成功时常见 `{ "success": true }`；没有字段也当成功（HTTP 已过）。
    if let Some(ok) = json.get("success").and_then(Value::as_bool) {
        if !ok {
            return Err(AppError::upstream(format!(
                "踢会话 {session_id} 返回 success=false"
            )));
        }
    }
    Ok(())
}

/// 踢掉除当前之外的全部会话。
///
/// `current_session_id` 来自 access JWT 的 `workosSessionId`。对不上列表里任何一条时，
/// 会把列表里的全踢掉（`kept_current = false`），由调用方去验 refresh。
pub async fn kick_others(
    http: &reqwest::Client,
    access_token: &str,
    current_session_id: Option<&str>,
) -> Result<(KickOutcome, Vec<ActiveSession>)> {
    let sessions = list_active(http, access_token, current_session_id).await?;
    let listed = sessions.len() as u32;
    let keep = current_session_id
        .filter(|id| sessions.iter().any(|s| s.session_id == *id))
        .map(str::to_string);
    let kept_current = keep.is_some();

    let mut revoked = 0u32;
    let mut failed = 0u32;
    for s in &sessions {
        if keep.as_deref() == Some(s.session_id.as_str()) {
            continue;
        }
        match revoke_one(http, access_token, &s.session_id, &s.session_type).await {
            Ok(()) => revoked += 1,
            Err(err) => {
                tracing::warn!(
                    session_id = %s.session_id,
                    error = %err.message,
                    "踢会话失败"
                );
                failed += 1;
            }
        }
    }

    let outcome = KickOutcome {
        listed,
        revoked,
        kept: if kept_current { 1 } else { 0 },
        failed,
        kept_current,
        // 由调用方填：这里还没验 refresh。
        refresh_alive: true,
    };
    Ok((outcome, sessions))
}

async fn auth_call(
    http: &reqwest::Client,
    access_token: &str,
    method: &str,
    body: &Value,
) -> Result<Value> {
    let token = access_token.trim();
    if token.is_empty() {
        return Err(AppError::invalid("缺少 access_token。"));
    }

    let url = format!("{AUTH_SERVICE}/{method}");
    let res = http
        .post(&url)
        .timeout(TIMEOUT)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .header("accept", "application/json")
        .header("connect-protocol-version", "1")
        .json(body)
        .send()
        .await
        .map_err(|err| AppError::network(format!("{method} 请求失败：{err}")))?;

    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(AppError::unauthorized(format!(
            "{method} 被拒绝（{status}）。可能需要重新授权。"
        )));
    }
    if !status.is_success() {
        let head: String = text.chars().take(160).collect();
        return Err(AppError::upstream(format!(
            "{method} 返回 {status}：{head}"
        )));
    }
    if text.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_str(&text).map_err(|_| AppError::upstream(format!("{method} 响应不是 JSON。")))
}

fn pick_str(obj: &serde_json::Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| {
        let s = obj.get(*k)?.as_str()?.trim();
        (!s.is_empty()).then(|| s.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_camel_or_snake_session_id() {
        let mut m = serde_json::Map::new();
        m.insert("session_id".into(), Value::String("sid".into()));
        assert_eq!(
            pick_str(&m, &["sessionId", "session_id"]).as_deref(),
            Some("sid")
        );
    }
}
