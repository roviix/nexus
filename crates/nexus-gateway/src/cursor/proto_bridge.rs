//! Cursor Connect-RPC Protobuf ⇄ Nexus 规范中间表示（ChatRequest）双向桥接。
//!
//! 实现两大核心能力：
//! 1. 入站请求反解：将 Cursor IDE 发出的 `InferenceStreamRequest` Connect-RPC 载荷反解为
//!    Nexus 统一规范对象 `ChatRequest`（完整还原系统提示词、多模态图片、上下文消息、工具声明与
//!    Anthropic Thinking 签名，保障多轮 Agent 连续性）。
//! 2. 出站流式封包：将任意上游（Claude / OpenAI / DeepSeek）的 `Delta` 流包装为
//!    `InferenceStreamResponse`，并切分为标准的 Connect 信封帧（Envelope Stream）。

use crate::connect::Envelope;
use crate::normalized::{
    ChatRequest, ImageInput, Message, Reasoning, Role, Sampling, ToolCall, ToolChoice, ToolDef,
    ToolResult,
};
use crate::proto::inference_content_part::Part;
use crate::proto::inference_core_message::Content;
use crate::proto::inference_stream_response::Response;
use crate::proto::{
    InferenceAgentTool, InferenceCoreMessage, InferenceMessageRole, InferenceModelConfig,
    InferenceStreamRequest, InferenceStreamResponse, InferenceTextStreamPart,
    InferenceThinkingStreamPart, InferenceToolCallStreamPart,
};
use prost_types::value::Kind;

/// 将 `InferenceStreamRequest` 反解为 Nexus 的统一中间表示 `ChatRequest`。
pub fn from_inference_request(req: &InferenceStreamRequest) -> ChatRequest {
    let model = resolve_requested_model(req);
    let messages = from_inference_messages(&req.messages);
    let tools: Vec<ToolDef> = req.tools.iter().map(from_inference_tool).collect();
    let tool_choice = if tools.is_empty() {
        ToolChoice::None
    } else {
        ToolChoice::Auto
    };

    let sampling = req
        .model_config
        .as_ref()
        .map(from_model_config)
        .unwrap_or_default();

    ChatRequest {
        model,
        messages,
        tools,
        tool_choice,
        sampling,
        conversation_id: req.conversation_id.clone(),
        raw_responses: None,
        client_headers: std::collections::HashMap::new(),
        raw_inbound: None,
    }
}

/// 从请求中提取用户指定的模型名，自动剥离 (Nexus) 或 [Nexus] 扩展标，
/// 并识别 Cursor 下发的推理深度（Reasoning Effort: high/medium/low/max）与变体参数。
pub fn resolve_requested_model(req: &InferenceStreamRequest) -> String {
    let raw = if let Some(ref m) = req.requested_model {
        if !m.model_id.trim().is_empty() {
            m.model_id.trim().to_string()
        } else {
            req.model_id.as_deref().unwrap_or("").trim().to_string()
        }
    } else {
        req.model_id.as_deref().unwrap_or("").trim().to_string()
    };

    let mut cleaned = raw
        .trim_end_matches("(Nexus)")
        .trim_end_matches("[Nexus]")
        .trim_end_matches("(nexus)")
        .trim_end_matches("[nexus]")
        .trim()
        .to_string();

    // 1. 如果 Cursor 采用了 variant 字符串表示，例如 "gpt-5.6-luna[reasoning=high,context=1m]"
    let mut variant_effort: Option<String> = None;
    if let Some(start_bracket) = cleaned.find('[') {
        if let Some(end_bracket) = cleaned.rfind(']') {
            if end_bracket > start_bracket {
                let inside = &cleaned[start_bracket + 1..end_bracket];
                for part in inside.split(',') {
                    if let Some((k, v)) = part.split_once('=') {
                        let k = k.trim().to_ascii_lowercase();
                        let v = v.trim().to_ascii_lowercase();
                        if k == "reasoning" || k == "effort" {
                            variant_effort = Some(v);
                        }
                    }
                }
                cleaned = cleaned[..start_bracket].trim().to_string();
            }
        }
    }

    // 2. 从 req.requested_model.parameters 及 max_mode 提取 effort
    let mut effort = variant_effort;
    if let Some(ref m) = req.requested_model {
        for p in &m.parameters {
            let id = p.id.trim().to_ascii_lowercase();
            if id == "reasoning" || id == "effort" {
                let val = p.value.trim().to_ascii_lowercase();
                if !val.is_empty() {
                    effort = Some(val);
                    break;
                }
            }
        }
        if effort.is_none() && m.max_mode {
            effort = Some("max".to_string());
        }
    }

    // 3. 标准化 effort 档位：low, medium, high, xhigh, max
    if let Some(eff) = effort {
        let norm_effort = match eff.as_str() {
            "extra_high" | "extra-high" | "xhigh" => "xhigh",
            "max" => "max",
            "high" => "high",
            "medium" | "med" => "medium",
            "low" => "low",
            _ => eff.as_str(),
        };

        // 如果模型名末尾尚未包含对应的 effort 后缀，自动追加（供 upstream_model 反解）
        if !cleaned.ends_with(&format!("-{norm_effort}")) {
            return format!("{cleaned}-{norm_effort}");
        }
    }

    cleaned
}

/// 将 Cursor 的 `InferenceCoreMessage` 数组反解为 Nexus 规范 `Message` 数组。
pub fn from_inference_messages(core_msgs: &[InferenceCoreMessage]) -> Vec<Message> {
    let mut out = Vec::with_capacity(core_msgs.len());

    for msg in core_msgs {
        let role = match msg.role() {
            InferenceMessageRole::System => Role::System,
            InferenceMessageRole::User => Role::User,
            InferenceMessageRole::Assistant => Role::Assistant,
            InferenceMessageRole::Tool => Role::Tool,
            InferenceMessageRole::Unspecified => Role::User,
        };

        let mut text = String::new();
        let mut images = Vec::new();
        let mut tool_results = Vec::new();

        if let Some(ref content) = msg.content {
            match content {
                Content::Text(ref s) => {
                    text = s.clone();
                }
                Content::Parts(ref parts) => {
                    for p in &parts.parts {
                        match &p.part {
                            Some(Part::Text(ref t)) => {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(&t.text);
                            }
                            Some(Part::Image(ref img)) => {
                                if !img.data.is_empty() {
                                    images.push(ImageInput {
                                        data: img.data.clone(),
                                        mime_type: img.mime_type.clone().unwrap_or_else(|| "image/png".into()),
                                    });
                                }
                            }
                            Some(Part::File(_)) => {}
                            None => {}
                        }
                    }
                }
                Content::ToolContent(ref tool_content) => {
                    for part in &tool_content.parts {
                        let result_text = part
                            .result
                            .as_ref()
                            .and_then(|v| match &v.kind {
                                Some(Kind::StringValue(s)) => Some(s.clone()),
                                _ => None,
                            })
                            .unwrap_or_default();

                        let mut res_images = Vec::new();
                        for exp in &part.experimental_content {
                            if let Some(Part::Image(ref img)) = exp.part {
                                if !img.data.is_empty() {
                                    res_images.push(ImageInput {
                                        data: img.data.clone(),
                                        mime_type: img.mime_type.clone().unwrap_or_else(|| "image/png".into()),
                                    });
                                }
                            }
                        }

                        tool_results.push(ToolResult {
                            tool_call_id: part.tool_call_id.clone(),
                            tool_name: part.tool_name.clone(),
                            text: result_text,
                            is_error: part.is_error,
                            images: res_images,
                        });
                    }
                }
            }
        }

        let tool_calls = msg
            .tool_calls
            .iter()
            .map(|tc| ToolCall {
                id: tc.tool_call_id.clone(),
                name: tc.tool_name.clone(),
                arguments: tc
                    .raw_tool_call_args
                    .clone()
                    .unwrap_or_else(|| {
                        tc.args
                            .as_ref()
                            .map(json_from_struct)
                            .map(|v| v.to_string())
                            .unwrap_or_else(|| "{}".into())
                    }),
            })
            .collect();

        // 深度思考与签名关键还原点：保留上游下发的 signature 与 redacted_data
        let reasoning = msg
            .reasoning_parts
            .iter()
            .map(|rp| Reasoning {
                text: rp.text.clone(),
                signature: rp.signature.clone().filter(|s| !s.is_empty()),
                redacted: rp.redacted_data.clone().filter(|d| !d.is_empty()),
            })
            .collect();

        out.push(Message {
            role,
            text,
            images,
            tool_calls,
            tool_results,
            reasoning,
        });
    }

    out
}

/// 将 `InferenceAgentTool` 还原为通用 `ToolDef`。
fn from_inference_tool(tool: &InferenceAgentTool) -> ToolDef {
    let parameters = tool
        .parameters
        .as_ref()
        .map(json_from_struct)
        .and_then(|mut v| {
            // Cursor 官方下发的 parameters 经常套一层 {"jsonSchema": {...}}
            if let Some(inner) = v.get_mut("jsonSchema").map(serde_json::Value::take) {
                Some(inner)
            } else {
                Some(v)
            }
        })
        .unwrap_or_else(|| serde_json::json!({ "type": "object", "properties": {} }));

    ToolDef {
        name: tool.name.clone(),
        description: tool.description.clone(),
        parameters,
        grammar: false,
        namespace: None,
    }
}

/// 采样参数提取。
fn from_model_config(config: &InferenceModelConfig) -> Sampling {
    Sampling {
        max_output_tokens: config.max_tokens.filter(|&t| t > 0).map(|t| t as u32),
        temperature: config.temperature,
        top_p: config.top_p,
        stop_sequences: config.stop_sequences.clone(),
    }
}

// ======================== 出站 Connect-RPC 编码器 ========================

/// 将文本增量编码为 Connect 信封。
pub fn encode_text_part(text: &str) -> Vec<u8> {
    let resp = InferenceStreamResponse {
        response: Some(Response::TextPart(InferenceTextStreamPart {
            text: text.to_string(),
            is_final: false,
        })),
    };
    encode_envelope(&resp)
}

/// 将思考增量（或最终思考签名）编码为 Connect 信封。
pub fn encode_thinking_part(text: &str, signature: Option<&str>, is_final: bool) -> Vec<u8> {
    let resp = InferenceStreamResponse {
        response: Some(Response::ThinkingPart(InferenceThinkingStreamPart {
            text: text.to_string(),
            signature: signature.map(str::to_string),
            is_final,
        })),
    };
    encode_envelope(&resp)
}

/// 将工具调用增量编码为 Connect 信封。
pub fn encode_tool_call_part(tc: &ToolCall, is_complete: bool, index: Option<i32>) -> Vec<u8> {
    let resp = InferenceStreamResponse {
        response: Some(Response::ToolCallPart(InferenceToolCallStreamPart {
            tool_call_id: tc.id.clone(),
            tool_name: tc.name.clone(),
            args: tc.arguments.clone(),
            is_complete,
            tool_index: index,
        })),
    };
    encode_envelope(&resp)
}

/// 流式正常结束信封（FLAG_END_STREAM，载荷为 JSON "{}"）。
pub fn encode_end_stream() -> Vec<u8> {
    Envelope {
        flags: Envelope::FLAG_END_STREAM,
        data: b"{}".to_vec(),
    }
    .encode()
}

/// 流式异常结束信封（FLAG_END_STREAM，载荷为 Connect-RPC 错误 JSON）。
pub fn encode_stream_error(code: &str, message: &str) -> Vec<u8> {
    let err_json = serde_json::json!({
        "error": {
            "code": code,
            "message": message
        }
    });
    Envelope {
        flags: Envelope::FLAG_END_STREAM,
        data: serde_json::to_vec(&err_json).unwrap_or_default(),
    }
    .encode()
}

/// 内部 Protobuf 消息序列化为单条 Connect 信封。
fn encode_envelope<M: prost::Message>(msg: &M) -> Vec<u8> {
    let mut data = Vec::with_capacity(msg.encoded_len());
    msg.encode(&mut data).expect("protobuf encode");
    Envelope::message(data).encode()
}

// ======================== Protobuf Struct ⇄ JSON 工具 ========================

/// 将 `prost_types::Struct` 转为 `serde_json::Value`。
pub fn json_from_struct(s: &prost_types::Struct) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    for (k, v) in &s.fields {
        map.insert(k.clone(), json_from_value(v));
    }
    serde_json::Value::Object(map)
}

/// 将 `prost_types::Value` 转为 `serde_json::Value`。
pub fn json_from_value(v: &prost_types::Value) -> serde_json::Value {
    match &v.kind {
        Some(Kind::NullValue(_)) | None => serde_json::Value::Null,
        Some(Kind::NumberValue(n)) => serde_json::Number::from_f64(*n)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        Some(Kind::StringValue(s)) => serde_json::Value::String(s.clone()),
        Some(Kind::BoolValue(b)) => serde_json::Value::Bool(*b),
        Some(Kind::StructValue(s)) => json_from_struct(s),
        Some(Kind::ListValue(l)) => {
            serde_json::Value::Array(l.values.iter().map(json_from_value).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::InferenceReasoningPart;

    #[test]
    fn roundtrip_reasoning_signature() {
        let req = InferenceStreamRequest {
            messages: vec![InferenceCoreMessage {
                role: InferenceMessageRole::Assistant as i32,
                reasoning_parts: vec![InferenceReasoningPart {
                    text: "thinking thoughts...".into(),
                    signature: Some("claude_sig_xyz123".into()),
                    ..Default::default()
                }],
                content: Some(Content::Text("hello".into())),
                ..Default::default()
            }],
            ..Default::default()
        };

        let normalized = from_inference_request(&req);
        assert_eq!(normalized.messages.len(), 1);
        let msg = &normalized.messages[0];
        assert_eq!(msg.reasoning.len(), 1);
        assert_eq!(msg.reasoning[0].signature.as_deref(), Some("claude_sig_xyz123"));
        assert_eq!(msg.reasoning[0].text, "thinking thoughts...");
    }

    #[test]
    fn encodes_valid_connect_envelopes() {
        let text_chunk = encode_text_part("hello world");
        assert!(text_chunk.len() > 5);
        assert_eq!(text_chunk[0], 0); // message flags = 0

        let end_chunk = encode_end_stream();
        assert_eq!(end_chunk[0], Envelope::FLAG_END_STREAM);
        assert_eq!(&end_chunk[5..], b"{}");
    }

    #[test]
    fn resolves_requested_model_with_effort_parameters() {
        use crate::proto::{InferenceModelParameterValue, InferenceRequestedModel};

        // 1. 普通模型无 effort
        let req1 = InferenceStreamRequest {
            requested_model: Some(InferenceRequestedModel {
                model_id: "gpt-5.6-luna (Nexus)".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(resolve_requested_model(&req1), "gpt-5.6-luna");

        // 2. 带 parameters 中的 reasoning=high
        let req2 = InferenceStreamRequest {
            requested_model: Some(InferenceRequestedModel {
                model_id: "gpt-5.6-luna (Nexus)".into(),
                parameters: vec![
                    InferenceModelParameterValue {
                        id: "reasoning".into(),
                        value: "high".into(),
                    },
                    InferenceModelParameterValue {
                        id: "context".into(),
                        value: "1m".into(),
                    },
                ],
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(resolve_requested_model(&req2), "gpt-5.6-luna-high");

        // 3. 带 max_mode
        let req3 = InferenceStreamRequest {
            requested_model: Some(InferenceRequestedModel {
                model_id: "gpt-5.6-luna".into(),
                max_mode: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(resolve_requested_model(&req3), "gpt-5.6-luna-max");

        // 4. 带 variant 字符串表示
        let req4 = InferenceStreamRequest {
            requested_model: Some(InferenceRequestedModel {
                model_id: "gpt-5.6-luna[reasoning=medium,context=200k] (Nexus)".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(resolve_requested_model(&req4), "gpt-5.6-luna-medium");
    }
}
