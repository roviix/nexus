//! 可逆真机探针：创建临时 BOX harness agent，验证其 turn 是否真正走 Sand host，随后删除。
//!
//! 这是有副作用且会消耗少量额度的研究入口，必须显式设置 `NEXUS_GROKBOT_BOX_PROBE=1`。
//! 输出只含 HTTP 状态和布尔判据，不输出 gateway token、network token、agent/request id 或正文。

use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::time::Duration;

fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

async fn gateway_post(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    network_token: &str,
    method: &str,
    body: &Value,
) -> Result<(u16, Value), String> {
    let response = client
        .post(format!("{}/{method}", base.trim_end_matches('/')))
        .bearer_auth(token)
        .header("x-anyrun-network-token", network_token)
        .json(body)
        .send()
        .await
        .map_err(|error| format!("gateway {method} 请求失败：{error}"))?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .await
        .map_err(|error| format!("gateway {method} 响应读取失败：{error}"))?;
    let body = serde_json::from_str(&text).unwrap_or_else(|_| {
        json!({
            "nonJson": true,
            "bodyLength": text.len(),
            "bodyPrefix": text.chars().take(160).collect::<String>()
        })
    });
    Ok((status, body))
}

fn response_error(body: &Value) -> Option<String> {
    body.pointer("/error/message")
        .or_else(|| body.get("error"))
        .or_else(|| body.get("message"))
        .and_then(Value::as_str)
        .map(|message| message.chars().take(200).collect())
}

fn visit_agents(value: &Value, visit: &mut impl FnMut(&str, Option<&str>)) {
    match value {
        Value::Array(values) => {
            for value in values {
                visit_agents(value, visit);
            }
        }
        Value::Object(object) => {
            let id = object
                .get("id")
                .or_else(|| object.get("agentId"))
                .and_then(Value::as_str);
            let harness = object.get("harness").and_then(Value::as_str);
            if let Some(id) = id.filter(|id| !id.is_empty()) {
                if harness.is_some()
                    || object.contains_key("name")
                    || object.contains_key("createdAt")
                {
                    visit(id, harness);
                }
            }
            for value in object.values() {
                visit_agents(value, visit);
            }
        }
        _ => {}
    }
}

fn agent_ids(body: &Value) -> BTreeSet<String> {
    let mut ids = BTreeSet::new();
    visit_agents(body, &mut |id, _| {
        ids.insert(id.to_string());
    });
    ids
}

fn agent_harness(body: &Value, wanted: &str) -> Option<String> {
    let mut found = None;
    visit_agents(body, &mut |id, harness| {
        if id == wanted && found.is_none() {
            found = harness.map(str::to_string);
        }
    });
    found
}

#[derive(Default)]
struct TurnEvidence {
    user_request_id: Option<String>,
    reply_request_id: Option<String>,
    reply_seen: bool,
}

async fn watch_turn(
    client: reqwest::Client,
    base: String,
    token: String,
    network_token: String,
    agent_id: String,
    prompt: String,
    expected_reply: String,
) -> Result<TurnEvidence, String> {
    let mut response = client
        .get(format!("{}/events", base.trim_end_matches('/')))
        .bearer_auth(token)
        .header("x-anyrun-network-token", network_token)
        .header("accept", "text/event-stream")
        .send()
        .await
        .map_err(|error| format!("连接 gateway SSE 失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("gateway SSE HTTP {}", response.status()));
    }

    let mut buffer = String::new();
    let mut evidence = TurnEvidence::default();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("读取 gateway SSE 失败：{error}"))?
    {
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buffer.find("\n\n") {
            let block = buffer[..end].to_string();
            buffer.drain(..end + 2);
            let Some(data) = block
                .lines()
                .find_map(|line| line.strip_prefix("data:").map(str::trim))
            else {
                continue;
            };
            let Ok(event) = serde_json::from_str::<Value>(data) else {
                continue;
            };
            if event.get("channel").and_then(Value::as_str) != Some("transcript")
                || event.pointer("/payload/agentId").and_then(Value::as_str)
                    != Some(agent_id.as_str())
            {
                continue;
            }
            let entry = &event["payload"]["entry"];
            let request_id = entry.get("requestId").and_then(Value::as_str);
            match (
                entry.get("kind").and_then(Value::as_str),
                entry.get("role").and_then(Value::as_str),
            ) {
                (Some("message"), Some("user"))
                    if entry.get("content").and_then(Value::as_str) == Some(&prompt) =>
                {
                    evidence.user_request_id = request_id.map(str::to_string);
                }
                (Some("send-message"), _) => {
                    if entry.pointer("/message/content").and_then(Value::as_str)
                        == Some(&expected_reply)
                    {
                        evidence.reply_seen = true;
                        evidence.reply_request_id = request_id.map(str::to_string);
                    }
                }
                (Some("message"), Some("assistant"))
                    if entry.get("content").and_then(Value::as_str) == Some(&expected_reply) =>
                {
                    evidence.reply_seen = true;
                    evidence.reply_request_id = request_id.map(str::to_string);
                }
                _ => {}
            }
            if evidence.user_request_id.is_some() && evidence.reply_seen {
                return Ok(evidence);
            }
        }
    }
    Ok(evidence)
}

async fn delete_agent(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    network_token: &str,
    agent_id: &str,
) -> (u16, Option<String>, bool) {
    let (status, body) = gateway_post(
        client,
        base,
        token,
        network_token,
        "api/deleteAgents",
        &json!({ "ids": [agent_id] }),
    )
    .await
    .unwrap_or((0, Value::Null));
    let error = response_error(&body);
    let listed = gateway_post(
        client,
        base,
        token,
        network_token,
        "api/listAgents",
        &json!({}),
    )
    .await
    .ok()
    .map(|(_, body)| body)
    .unwrap_or(Value::Null);
    let deleted = (200..300).contains(&status) && !agent_ids(&listed).contains(agent_id);
    (status, error, deleted)
}

async fn run() -> Value {
    let secrets = match nexus_grokbot::secrets::load() {
        Ok(secrets) => secrets,
        Err(error) => return json!({ "ok": false, "stage": "secrets", "error": error.message }),
    };
    let box_info =
        match nexus_grokbot::pod::ensure_sandbox(&secrets.session_token, &secrets.machine_id).await
        {
            Ok(info) => info,
            Err(error) => return json!({ "ok": false, "stage": "ensure", "error": error.message }),
        };
    if box_info.gateway_url.is_empty() || box_info.gateway_token.is_empty() {
        return json!({ "ok": false, "stage": "ensure", "error": "EnsureSandBox 没返回 gateway 凭证" });
    }

    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(25))
        .build()
        .expect("probe client");
    let before = gateway_post(
        &client,
        &box_info.gateway_url,
        &box_info.gateway_token,
        &box_info.network_token,
        "api/listAgents",
        &json!({}),
    )
    .await
    .ok()
    .map(|(_, body)| agent_ids(&body))
    .unwrap_or_default();

    let stamp = stamp();
    let (create_http, create_body) = match gateway_post(
        &client,
        &box_info.gateway_url,
        &box_info.gateway_token,
        &box_info.network_token,
        "api/createAgent",
        &json!({
            "name": format!("Nexus Stream Probe {}", stamp % 1_000_000),
            "description": "Temporary BOX harness agent for a reversible Stream admission probe.",
            "creationRoute": { "kind": "box" },
            "harness": "box",
            "isIntroductionSuppressed": true,
            "isKickstartRequested": false,
            "clientNonce": format!("nexus-box-probe-{stamp}")
        }),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return json!({ "ok": false, "stage": "create", "error": error }),
    };

    let listed = gateway_post(
        &client,
        &box_info.gateway_url,
        &box_info.gateway_token,
        &box_info.network_token,
        "api/listAgents",
        &json!({}),
    )
    .await
    .ok()
    .map(|(_, body)| body)
    .unwrap_or(Value::Null);
    let after_ids = agent_ids(&listed);
    let created_ids: Vec<String> = after_ids.difference(&before).cloned().collect();
    let agent_id = ["/agent/id", "/agent/agentId", "/id", "/agentId"]
        .into_iter()
        .find_map(|pointer| create_body.pointer(pointer).and_then(Value::as_str))
        .map(str::to_string)
        .or_else(|| (created_ids.len() == 1).then(|| created_ids[0].clone()));
    let Some(agent_id) = agent_id else {
        return json!({
            "ok": false,
            "stage": "create",
            "createHttp": create_http,
            "newAgentCount": created_ids.len(),
            "responseKeys": create_body.as_object().map(|object| object.keys().cloned().collect::<Vec<_>>()),
            "error": response_error(&create_body)
        });
    };
    let harness = agent_harness(&create_body, &agent_id)
        .or_else(|| agent_harness(&listed, &agent_id))
        .unwrap_or_else(|| "unknown".into());

    let mut send_http = None;
    let mut accepted = false;
    let mut evidence = TurnEvidence::default();
    let mut run_error = None;
    if harness == "box" {
        let expected_reply = format!("BOX-{}", stamp % 1_000_000);
        let prompt = format!("Reply with exactly {expected_reply} and nothing else.");
        let mut watcher = tokio::spawn(watch_turn(
            client.clone(),
            box_info.gateway_url.clone(),
            box_info.gateway_token.clone(),
            box_info.network_token.clone(),
            agent_id.clone(),
            prompt.clone(),
            expected_reply,
        ));
        tokio::time::sleep(Duration::from_millis(800)).await;
        match gateway_post(
            &client,
            &box_info.gateway_url,
            &box_info.gateway_token,
            &box_info.network_token,
            "api/sendPrompt",
            &json!({
                "agentId": agent_id,
                "clientNonce": format!("nexus-box-turn-{stamp}"),
                "prompt": prompt,
                "isFork": false
            }),
        )
        .await
        {
            Ok((status, body)) => {
                send_http = Some(status);
                accepted = (200..300).contains(&status)
                    && body.get("accepted").and_then(Value::as_bool) == Some(true);
                if !accepted {
                    run_error = response_error(&body);
                }
            }
            Err(error) => run_error = Some(error),
        }
        if accepted {
            match tokio::time::timeout(Duration::from_secs(60), &mut watcher).await {
                Ok(Ok(Ok(found))) => evidence = found,
                Ok(Ok(Err(error))) => run_error = Some(error),
                Ok(Err(error)) => run_error = Some(format!("SSE task failed: {error}")),
                Err(_) => {
                    watcher.abort();
                    run_error = Some("BOX agent reply timeout".into());
                }
            }
        } else {
            watcher.abort();
        }
    } else {
        run_error = Some(format!(
            "createAgent 返回的 harness 是 {harness}，未发送消息"
        ));
    }

    let (delete_http, delete_error, deleted) = delete_agent(
        &client,
        &box_info.gateway_url,
        &box_info.gateway_token,
        &box_info.network_token,
        &agent_id,
    )
    .await;
    let request_id_match =
        evidence.user_request_id.is_some() && evidence.user_request_id == evidence.reply_request_id;
    json!({
        "ok": harness == "box" && accepted && evidence.reply_seen && request_id_match && deleted,
        "createHttp": create_http,
        "harness": harness,
        "sendHttp": send_http,
        "accepted": accepted,
        "userSeen": evidence.user_request_id.is_some(),
        "replySeen": evidence.reply_seen,
        "requestIdMatch": request_id_match,
        "deleteHttp": delete_http,
        "deleted": deleted,
        "deleteError": delete_error,
        "error": run_error
    })
}

#[tokio::main]
async fn main() {
    if std::env::var("NEXUS_GROKBOT_BOX_PROBE").ok().as_deref() != Some("1") {
        eprintln!(
            "这会创建临时 BOX agent、发送一次真实 turn、消耗少量额度并删除。确认后设 NEXUS_GROKBOT_BOX_PROBE=1。"
        );
        std::process::exit(2);
    }
    let result = run().await;
    println!("BOX agent Stream 验证：{result}");
    std::process::exit(if result.get("ok").and_then(Value::as_bool) == Some(true) {
        0
    } else {
        1
    });
}
