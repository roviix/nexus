//! 真实端到端验证：针对运行中的本地 Nexus 网关发起带有子代理 (dispatch_subagent / task) 的请求验证。

use bytes::Bytes;
use futures_util::StreamExt;
use nexus_gateway::connect::Envelope;
use nexus_gateway::cursor::proto::agent::v1 as agent;
use prost::Message;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Nexus Cursor Subagent 真实端到端探针 ===");

    let base_url = "http://127.0.0.1:8693/cursor/route";
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(180))
        .build()?;

    let conv_id = format!("subagent-probe-{}", uuid::Uuid::new_v4());
    let req_id = format!("subagent-req-{}", uuid::Uuid::new_v4());

    println!("1. 发起指令要求模型调用 dispatch_subagent 执行独立子任务...");
    let run_req = agent::AgentRunRequest {
        requested_model: Some(agent::RequestedModel {
            model_id: "chatgpt/gpt-5.6-luna (Nexus)".to_string(),
            ..Default::default()
        }),
        conversation_id: Some(conv_id.clone()),
        action: Some(agent::ConversationAction {
            action: Some(agent::conversation_action::Action::UserMessageAction(
                agent::UserMessageAction {
                    user_message: Some(agent::UserMessage {
                        text: "请使用 dispatch_subagent 工具派发一个子任务：读取当前目录下的 Cargo.toml 并汇报 package name 与 workspace 成员。".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )),
            ..Default::default()
        }),
        ..Default::default()
    };

    let client_msg = agent::AgentClientMessage {
        message: Some(agent::agent_client_message::Message::RunRequest(run_req)),
    };

    let mut buf = Vec::new();
    client_msg.encode(&mut buf)?;
    let req_envelope = Envelope::message(buf).encode();

    let resp = client
        .post(format!("{}/agent.v1.AgentService/Run", base_url))
        .header("content-type", "application/connect+proto")
        .header("connect-protocol-version", "1")
        .header("x-request-id", &req_id)
        .header("x-conversation-id", &conv_id)
        .body(req_envelope)
        .send()
        .await?;

    println!("   HTTP 响应状态码: {}", resp.status());
    let mut stream = resp.bytes_stream();
    let mut decoder = nexus_gateway::connect::EnvelopeDecoder::default();

    while let Some(chunk_res) = stream.next().await {
        let chunk: Bytes = chunk_res?;
        decoder.push(&chunk);

        while let Some(env) = decoder.next_envelope().map_err(|e| format!("{:?}", e))? {
            if env.is_end_stream() {
                println!("\n🏁 收到 Connect 终结帧！子代理闭环完整结束。");
                return Ok(());
            }

            if let Ok(server_msg) = agent::AgentServerMessage::decode(&env.data[..]) {
                if let Some(msg) = server_msg.message {
                    if let agent::agent_server_message::Message::InteractionUpdate(u) = msg {
                        if let Some(update) = u.message {
                            match update {
                                agent::interaction_update::Message::ToolCallStarted(started) => {
                                    println!("\n[UI 轨] 🛠️ Subagent ToolCallStarted: has_call={}", started.tool_call.is_some());
                                }
                                agent::interaction_update::Message::ToolCallCompleted(completed) => {
                                    println!("\n[UI 轨] ✅ Subagent ToolCallCompleted: has_call={}", completed.tool_call.is_some());
                                }
                                agent::interaction_update::Message::TextDelta(t) => {
                                    print!("{}", t.text);
                                    use std::io::Write;
                                    let _ = std::io::stdout().flush();
                                }
                                agent::interaction_update::Message::TurnEnded(_) => {
                                    println!("\n[UI 轨] 🔚 TurnEnded 收到！");
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}
