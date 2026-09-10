//! proto 层的兼容性测试。
//!
//! 和 identity / headers 一样，这里的字节向量来自线上那份 `protocol.js`：`/tmp/vec-proto.mjs`
//! 调它的 `toInferenceMessages` 再 `toBinary()`。Rust 只负责解出来并逐字段核对——能解、
//! 字段对，就说明字段号 / wire type / oneof / Struct / Value 全部与真正在跑的编码器兼容。
//!
//! 单独成文件是因为 `proto.rs` 是生成的，重生成会把内嵌测试冲掉。

use crate::proto::inference_content_part::Part;
use crate::proto::inference_core_message::Content;
use crate::proto::inference_stream_response::Response;
use crate::proto::*;
use prost::Message;
use prost_types::value::Kind;

pub(crate) fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

// 由 /tmp/vec-proto.mjs 生成（2026-09-02）。输入依次是：system "SYS"（另加 extra "EXTRA"）、
// user "hello"、assistant "calling" + 一次 tool_call、tool 结果 "sunny"、user "look" + 一张 png。
// inference 模块的映射对拍也用这四条（`to_inference_messages_reproduces_what_protocol_js_encodes`）。
pub(crate) const USER_WITH_FOLDED_SYSTEM: &str = "080112115359530a0a45585452410a0a68656c6c6f";
pub(crate) const ASSISTANT_WITH_TOOL_CALL: &str = "0802120763616c6c696e6722500a0663616c6c5f31120b6765745f776561746865721a210a0f0a046369747912071a0550617269730a0e0a016e120911000000000000004022167b2263697479223a225061726973222c226e223a327d";
pub(crate) const TOOL_RESULT: &str =
    "080332200a1e0a0663616c6c5f31120b6765745f776561746865721a071a0573756e6e79";
pub(crate) const USER_WITH_IMAGE: &str =
    "08011a1f0a080a060a046c6f6f6b0a1312110a04414145431209696d6167652f706e67";

#[test]
fn decodes_a_user_message_with_system_folded_in() {
    let m = InferenceCoreMessage::decode(&unhex(USER_WITH_FOLDED_SYSTEM)[..]).unwrap();
    assert_eq!(m.role, InferenceMessageRole::User as i32);
    assert_eq!(
        m.content,
        Some(Content::Text("SYS\n\nEXTRA\n\nhello".into())),
        "system 折进首条 user 是 protocol.js 的既定行为，Rust 侧映射时也要照做"
    );
    assert!(m.tool_calls.is_empty());
}

#[test]
fn decodes_an_assistant_tool_call_with_struct_args() {
    let m = InferenceCoreMessage::decode(&unhex(ASSISTANT_WITH_TOOL_CALL)[..]).unwrap();
    assert_eq!(m.role, InferenceMessageRole::Assistant as i32);
    assert_eq!(m.content, Some(Content::Text("calling".into())));
    assert_eq!(m.tool_calls.len(), 1);
    let tc = &m.tool_calls[0];
    assert_eq!(tc.tool_call_id, "call_1");
    assert_eq!(tc.tool_name, "get_weather");
    assert_eq!(
        tc.raw_tool_call_args.as_deref(),
        Some(r#"{"city":"Paris","n":2}"#)
    );
    let args = tc.args.as_ref().expect("args 是 google.protobuf.Struct");
    assert_eq!(
        args.fields["city"].kind,
        Some(Kind::StringValue("Paris".into()))
    );
    assert_eq!(args.fields["n"].kind, Some(Kind::NumberValue(2.0)));
}

#[test]
fn decodes_a_tool_result_carried_as_a_value() {
    let m = InferenceCoreMessage::decode(&unhex(TOOL_RESULT)[..]).unwrap();
    assert_eq!(m.role, InferenceMessageRole::Tool as i32);
    let Some(Content::ToolContent(tc)) = m.content else {
        panic!("tool 结果该走 tool_content 这个 oneof 分支");
    };
    assert_eq!(tc.parts.len(), 1);
    let p = &tc.parts[0];
    assert_eq!(p.tool_call_id, "call_1");
    assert_eq!(p.tool_name, "get_weather");
    assert!(!p.is_error);
    assert_eq!(
        p.result
            .as_ref()
            .expect("result 是 google.protobuf.Value")
            .kind,
        Some(Kind::StringValue("sunny".into()))
    );
}

#[test]
fn decodes_a_user_message_with_text_and_image_parts() {
    let m = InferenceCoreMessage::decode(&unhex(USER_WITH_IMAGE)[..]).unwrap();
    assert_eq!(m.role, InferenceMessageRole::User as i32);
    let Some(Content::Parts(parts)) = m.content else {
        panic!("带图的消息该走 parts");
    };
    assert_eq!(parts.parts.len(), 2);
    assert_eq!(
        parts.parts[0].part,
        Some(Part::Text(InferenceTextPart {
            text: "look".into(),
            provider_options: None,
        }))
    );
    let Some(Part::Image(img)) = &parts.parts[1].part else {
        panic!("第二段该是图");
    };
    assert_eq!(img.data, "AAEC");
    assert_eq!(img.mime_type.as_deref(), Some("image/png"));
}

#[test]
fn stream_request_round_trips_through_the_wire() {
    let req = InferenceStreamRequest {
        messages: vec![InferenceCoreMessage {
            role: InferenceMessageRole::User as i32,
            content: Some(Content::Text("hi".into())),
            ..Default::default()
        }],
        tools: vec![InferenceAgentTool {
            name: "t".into(),
            description: "d".into(),
            parameters: Some(prost_types::Struct::default()),
            custom_tool_format: None,
        }],
        model_config: Some(InferenceModelConfig {
            max_tokens: Some(100),
            temperature: Some(0.5),
            top_p: None,
            stop_sequences: vec!["END".into()],
        }),
        requested_model: Some(InferenceRequestedModel {
            model_id: "claude-sonnet-5".into(),
            max_mode: false,
            ..Default::default()
        }),
        conversation_id: Some("conv-1".into()),
        ..Default::default()
    };
    let bytes = req.encode_to_vec();
    assert_eq!(InferenceStreamRequest::decode(&bytes[..]).unwrap(), req);
}

#[test]
fn response_oneof_tags_match_the_bundle() {
    // 每个分支的 tag 直接决定能不能读到上游发来的东西；把它们钉在 wire 上。
    let tag = |r: Response| InferenceStreamResponse { response: Some(r) }.encode_to_vec()[0] >> 3;
    assert_eq!(
        tag(Response::TextPart(InferenceTextStreamPart {
            text: "x".into(),
            is_final: false,
        })),
        1
    );
    assert_eq!(tag(Response::ToolCallPart(Default::default())), 2);
    assert_eq!(tag(Response::Usage(Default::default())), 3);
    assert_eq!(tag(Response::ResponseInfo(Default::default())), 4);
    assert_eq!(tag(Response::ExtendedUsage(Default::default())), 5);
    assert_eq!(tag(Response::Error(Default::default())), 8);
    assert_eq!(tag(Response::ThinkingPart(Default::default())), 9);
}

#[test]
fn role_and_error_type_enums_match_the_values_protocol_js_switches_on() {
    // INF_ROLE = { system: 4, user: 1, assistant: 2, tool: 3 }
    assert_eq!(InferenceMessageRole::User as i32, 1);
    assert_eq!(InferenceMessageRole::Assistant as i32, 2);
    assert_eq!(InferenceMessageRole::Tool as i32, 3);
    assert_eq!(InferenceMessageRole::System as i32, 4);
    // mapInferenceError：2/3 token 上限、4 限流、5 鉴权、6 权限、7 过载、8 内容过滤
    assert_eq!(InferenceStreamErrorType::InputTokenLimit as i32, 2);
    assert_eq!(InferenceStreamErrorType::OutputTokenLimit as i32, 3);
    assert_eq!(InferenceStreamErrorType::RateLimit as i32, 4);
    assert_eq!(InferenceStreamErrorType::Authentication as i32, 5);
    assert_eq!(InferenceStreamErrorType::Permission as i32, 6);
    assert_eq!(InferenceStreamErrorType::Overloaded as i32, 7);
    assert_eq!(InferenceStreamErrorType::ContentFilter as i32, 8);
    assert_eq!(
        InferenceStreamErrorType::try_from(4),
        Ok(InferenceStreamErrorType::RateLimit)
    );
}
