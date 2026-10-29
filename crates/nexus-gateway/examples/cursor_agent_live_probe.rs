//! 真实端到端验证：针对运行中的本地 Nexus 网关发起 Cursor agent.v1.AgentService/Run 真实请求。
//! 验证工具调用双轨下发、ExecClientMessage 结果回传、模型内部循环与思考链完整性。

use bytes::Bytes;
use futures_util::StreamExt;
use nexus_gateway::connect::Envelope;
use nexus_gateway::cursor::proto::agent::v1 as agent;
use nexus_gateway::cursor::proto::aiserver::v1 as aiserver;
use prost::Message;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("=== Nexus Cursor Agent 真实端到端协议探针 ===");

    let port = std::env::var("NEXUS_PORT").unwrap_or_else(|_| "8694".to_string());
    let model = std::env::var("NEXUS_MODEL")
        .unwrap_or_else(|_| "kiro-claude-opus-5.5-thinking (Nexus)".to_string());
    let base_url = format!("http://127.0.0.1:{port}/cursor/route");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(60))
        .build()?;

    let conv_id = format!("live-probe-{}", uuid::Uuid::new_v4());
    let req_id = format!("live-req-{}", uuid::Uuid::new_v4());

    println!(
        "1. 构造 AgentRunRequest (conv_id: {}, model: {})",
        conv_id, model
    );
    let run_req = agent::AgentRunRequest {
        requested_model: Some(agent::RequestedModel {
            model_id: model,
            ..Default::default()
        }),
        conversation_id: Some(conv_id.clone()),
        action: Some(agent::ConversationAction {
            action: Some(agent::conversation_action::Action::UserMessageAction(
                agent::UserMessageAction {
                    user_message: Some(agent::UserMessage {
                        text: "请使用 run_terminal_command 查看 git status --short".to_string(),
                        selected_context: Some(agent::SelectedContext {
                            files: vec![agent::SelectedFile {
                                path: "test.txt".to_string(),
                                content: "test".to_string(),
                                ..Default::default()
                            }],
                            ..Default::default()
                        }),
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

    println!("2. 发起 POST {}/agent.v1.AgentService/Run...", base_url);
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
    if !resp.status().is_success() {
        let err_text = resp.text().await?;
        eprintln!("   请求失败: {}", err_text);
        return Ok(());
    }

    println!("3. 开始监听下行 Connect-RPC 双向流事件...");
    let mut stream = resp.bytes_stream();
    let mut decoder = nexus_gateway::connect::EnvelopeDecoder::default();
    let mut step_count = 0;

    let bidi_client = client.clone();
    let conv_id_clone = conv_id.clone();
    let base_url_str = base_url.to_string();

    while let Some(chunk_res) = stream.next().await {
        let chunk: Bytes = chunk_res?;
        decoder.push(&chunk);

        while let Some(env) = decoder.next_envelope().map_err(|e| format!("{:?}", e))? {
            if env.is_end_stream() {
                println!("\n🏁 收到 Connect 终结帧 (FLAG_END_STREAM)！");
                if let Ok(json_str) = std::str::from_utf8(&env.data) {
                    println!("   终结元数据: {}", json_str);
                }
                return Ok(());
            }

            match agent::AgentServerMessage::decode(&env.data[..]) {
                Ok(server_msg) => {
                    step_count += 1;
                    if let Some(msg) = server_msg.message {
                        match msg {
                            agent::agent_server_message::Message::InteractionUpdate(u) => {
                                if let Some(update) = u.message {
                                    match update {
                                        agent::interaction_update::Message::ToolCallStarted(
                                            started,
                                        ) => {
                                            println!("\n[UI 轨] 🛠️ ToolCallStarted 收到！");
                                            println!(
                                                "   ToolCall: {:?}",
                                                started.tool_call.is_some()
                                            );
                                        }
                                        agent::interaction_update::Message::ToolCallCompleted(
                                            completed,
                                        ) => {
                                            println!("\n[UI 轨] ✅ ToolCallCompleted 收到！ToolCall: {:?}", completed.tool_call.is_some());
                                        }
                                        agent::interaction_update::Message::ThinkingDelta(td) => {
                                            print!("{}", td.text);
                                            use std::io::Write;
                                            let _ = std::io::stdout().flush();
                                        }
                                        agent::interaction_update::Message::ThinkingCompleted(
                                            _,
                                        ) => {
                                            println!("\n[UI 轨] 💡 ThinkingCompleted 思考链完成！");
                                        }
                                        agent::interaction_update::Message::TextDelta(t) => {
                                            print!("{}", t.text);
                                            use std::io::Write;
                                            let _ = std::io::stdout().flush();
                                        }
                                        agent::interaction_update::Message::TurnEnded(te) => {
                                            println!("\n[UI 轨] 🔚 TurnEnded 收到！tokens: in={:?}, out={:?}, reasoning={:?}", 
                                                te.input_tokens, te.output_tokens, te.reasoning_tokens);
                                        }
                                        other => {
                                            println!(
                                                "[UI 轨] 收到其他 InteractionUpdate: {:?}",
                                                std::mem::discriminant(&other)
                                            );
                                        }
                                    }
                                }
                            }
                            agent::agent_server_message::Message::ExecServerMessage(exec_msg) => {
                                println!(
                                    "\n[执行轨] ⚡ ExecServerMessage 触发！id={}",
                                    exec_msg.id
                                );
                                let exec_id = exec_msg.id;
                                if let Some(exec_payload) = exec_msg.message {
                                    match exec_payload {
                                        agent::exec_server_message::Message::ShellArgs(
                                            shell_args,
                                        ) => {
                                            println!(
                                                "   [本地终端执行] 命令: {:?}",
                                                shell_args.command
                                            );
                                            println!(
                                                "   [本地终端执行] 工作目录: {:?}",
                                                shell_args.working_directory
                                            );

                                            // 本地实际执行 git status --short
                                            let output = std::process::Command::new("sh")
                                                .arg("-c")
                                                .arg(&shell_args.command)
                                                .output();

                                            let (stdout, stderr, exit_code) = match output {
                                                Ok(out) => (
                                                    String::from_utf8_lossy(&out.stdout)
                                                        .to_string(),
                                                    String::from_utf8_lossy(&out.stderr)
                                                        .to_string(),
                                                    out.status.code().unwrap_or(0),
                                                ),
                                                Err(e) => ("".into(), e.to_string(), 1),
                                            };

                                            println!("   [本地终端执行完成] exit_code={}, stdout 长度={}", exit_code, stdout.len());

                                            // 构造 ExecClientMessage 并通过 BidiAppend 回传
                                            let reply_exec = agent::ExecClientMessage {
                                                id: exec_id,
                                                exec_id: exec_id.to_string(),
                                                message: Some(agent::exec_client_message::Message::ShellResult(
                                                    agent::ShellResult {
                                                        result: Some(agent::shell_result::Result::Success(agent::ShellSuccess {
                                                            command: shell_args.command,
                                                            working_directory: shell_args.working_directory,
                                                            exit_code,
                                                            signal: "".into(),
                                                            stdout,
                                                            stderr,
                                                            execution_time: 10,
                                                            output_location: None,
                                                            shell_id: None,
                                                            interleaved_output: None,
                                                            pid: Some(9999),
                                                            ms_to_wait: None,
                                                            local_execution_time_ms: Some(10),
                                                            background_reason: None,
                                                            output_head: None,
                                                            output_tail: None,
                                                            elided_chars: None,
                                                        })),
                                                        sandbox_policy: None,
                                                        is_background: None,
                                                        terminals_folder: None,
                                                        pid: None,
                                                    },
                                                )),
                                                ..Default::default()
                                            };

                                            let client_reply_msg = agent::AgentClientMessage {
                                                message: Some(agent::agent_client_message::Message::ExecClientMessage(reply_exec)),
                                            };

                                            let mut rep_buf = Vec::new();
                                            client_reply_msg.encode(&mut rep_buf)?;
                                            let hex_data: String = rep_buf
                                                .iter()
                                                .map(|b| format!("{:02x}", b))
                                                .collect();

                                            let bidi_req = aiserver::BidiAppendRequest {
                                                data: hex_data,
                                                request_id: Some(aiserver::BidiRequestId {
                                                    request_id: conv_id_clone.clone(),
                                                }),
                                                append_seqno: 1,
                                                data_binary: rep_buf,
                                            };

                                            let mut bidi_buf = Vec::new();
                                            bidi_req.encode(&mut bidi_buf)?;
                                            let bidi_env = Envelope::message(bidi_buf).encode();

                                            let b_client = bidi_client.clone();
                                            let b_url = format!(
                                                "{}/aiserver.v1.BidiService/BidiAppend",
                                                base_url_str
                                            );
                                            tokio::spawn(async move {
                                                println!("   --> 正在投递 ExecClientMessage 至 BidiAppend...");
                                                let r = b_client
                                                    .post(b_url)
                                                    .header(
                                                        "content-type",
                                                        "application/connect+proto",
                                                    )
                                                    .header("connect-protocol-version", "1")
                                                    .body(bidi_env)
                                                    .send()
                                                    .await;
                                                match r {
                                                    Ok(res) => println!(
                                                        "   <-- BidiAppend 投递成功，状态: {}",
                                                        res.status()
                                                    ),
                                                    Err(err) => eprintln!(
                                                        "   <-- BidiAppend 投递出错: {}",
                                                        err
                                                    ),
                                                }
                                            });
                                        }
                                        other => {
                                            println!(
                                                "   [执行轨] 收到其他 Exec: {:?}",
                                                std::mem::discriminant(&other)
                                            );
                                        }
                                    }
                                }
                            }
                            other => {
                                println!("[服务端消息] 收到: {:?}", std::mem::discriminant(&other));
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("解析 AgentServerMessage 出错: {}", e);
                }
            }
        }
    }

    println!("\n✅ 探针流接收完成！总计解码帧数: {}", step_count);
    Ok(())
}
