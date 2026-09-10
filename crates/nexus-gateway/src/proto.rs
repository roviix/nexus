//! `aiserver.v1.Inference*` 的 protobuf 类型。
//!
//! **生成文件，不要手改。** 由 `scripts/gen-proto.py` 从 `proto/inference-3.18.9.json` 生成——那份 JSON 是
//! `gateway/scripts/extract-inference-proto.py` 对 Cursor desktop bundle 机器提取的结果，
//! 字段号一个都没经过人手。Cursor 升级后按 `scripts/gen-proto.py` 头部的步骤重跑，
//! 拿 git diff 看协议动了什么。
//!
//! 只有两处是人给的知识（都在生成器里、按 (消息, 字段) 索引）：哪些字段是
//! google.protobuf 的 Struct / Value，哪些 enum 字段对应哪个 enum。
#![allow(clippy::large_enum_variant, clippy::enum_variant_names)]

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceAgentTool {
    #[prost(string, tag = "1")]
    pub name: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub description: ::prost::alloc::string::String,
    #[prost(message, optional, tag = "3")]
    pub parameters: ::core::option::Option<::prost_types::Struct>,
    #[prost(message, optional, tag = "4")]
    pub custom_tool_format: ::core::option::Option<InferenceCustomToolFormat>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceAnthropicOptions {
    #[prost(message, optional, tag = "1")]
    pub cache_control: ::core::option::Option<InferenceCacheControl>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceCacheControl {
    #[prost(string, tag = "1")]
    pub r#type: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceContentPart {
    #[prost(oneof = "inference_content_part::Part", tags = "1, 2, 3")]
    pub part: ::core::option::Option<inference_content_part::Part>,
}
/// `InferenceContentPart` 的 oneof。
pub mod inference_content_part {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Part {
        #[prost(message, tag = "1")]
        Text(super::InferenceTextPart),
        #[prost(message, tag = "2")]
        Image(super::InferenceImagePart),
        #[prost(message, tag = "3")]
        File(super::InferenceFilePart),
    }
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceContentParts {
    #[prost(message, repeated, tag = "1")]
    pub parts: ::prost::alloc::vec::Vec<InferenceContentPart>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceCoreMessage {
    #[prost(enumeration = "InferenceMessageRole", tag = "1")]
    pub role: i32,
    #[prost(message, repeated, tag = "4")]
    pub tool_calls: ::prost::alloc::vec::Vec<InferenceToolCall>,
    #[prost(message, repeated, tag = "7")]
    pub reasoning_parts: ::prost::alloc::vec::Vec<InferenceReasoningPart>,
    #[prost(string, optional, tag = "8")]
    pub model_provider_message_id: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(string, optional, tag = "9")]
    pub openai_phase: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(bool, optional, tag = "10")]
    pub openai_phase_null: ::core::option::Option<bool>,
    #[prost(string, optional, tag = "11")]
    pub cursor_inference_reason: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(string, optional, tag = "12")]
    pub cursor_feature_type: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(oneof = "inference_core_message::Content", tags = "2, 3, 6")]
    pub content: ::core::option::Option<inference_core_message::Content>,
}
/// `InferenceCoreMessage` 的 oneof。
pub mod inference_core_message {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Content {
        #[prost(string, tag = "2")]
        Text(::prost::alloc::string::String),
        #[prost(message, tag = "3")]
        Parts(super::InferenceContentParts),
        #[prost(message, tag = "6")]
        ToolContent(super::InferenceToolResultContent),
    }
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceCursorOptions {
    #[prost(string, optional, tag = "1")]
    pub image_description: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(map = "int32, string", tag = "2")]
    pub image_descriptions: ::std::collections::HashMap<i32, ::prost::alloc::string::String>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceCustomToolFormat {
    #[prost(string, tag = "1")]
    pub r#type: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub definition: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub syntax: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceExtendedUsageInfo {
    #[prost(int32, tag = "1")]
    pub input_tokens: i32,
    #[prost(int32, tag = "2")]
    pub output_tokens: i32,
    #[prost(int32, tag = "3")]
    pub cache_read_tokens: i32,
    #[prost(int32, tag = "4")]
    pub cache_write_tokens: i32,
    #[prost(int32, tag = "5")]
    pub max_tokens: i32,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceExtraData {
    #[prost(message, repeated, tag = "1")]
    pub token_logprobs: ::prost::alloc::vec::Vec<InferenceTokenLogprobs>,
    #[prost(message, repeated, tag = "2")]
    pub token_ids: ::prost::alloc::vec::Vec<InferenceTokenIds>,
    #[prost(message, repeated, tag = "3")]
    pub prompt_token_ids: ::prost::alloc::vec::Vec<InferenceTokenIds>,
    #[prost(message, repeated, tag = "4")]
    pub extra_tokens: ::prost::alloc::vec::Vec<InferenceTokenIds>,
    #[prost(message, repeated, tag = "5")]
    pub extra_logprobs: ::prost::alloc::vec::Vec<InferenceTokenLogprobs>,
    #[prost(message, repeated, tag = "6")]
    pub routing_matrix: ::prost::alloc::vec::Vec<InferenceRoutingRow>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceFilePart {
    #[prost(string, tag = "1")]
    pub data: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub media_type: ::prost::alloc::string::String,
    #[prost(string, optional, tag = "3")]
    pub filename: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(message, optional, tag = "4")]
    pub provider_options: ::core::option::Option<InferenceProviderOptions>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceImageDescription {
    #[prost(int32, tag = "1")]
    pub message_index: i32,
    #[prost(int32, tag = "2")]
    pub part_index: i32,
    #[prost(int32, optional, tag = "3")]
    pub exp_content_index: ::core::option::Option<i32>,
    #[prost(string, tag = "4")]
    pub description: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceImageDescriptionsInfo {
    #[prost(message, repeated, tag = "1")]
    pub descriptions: ::prost::alloc::vec::Vec<InferenceImageDescription>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceImagePart {
    #[prost(string, tag = "1")]
    pub data: ::prost::alloc::string::String,
    #[prost(string, optional, tag = "2")]
    pub mime_type: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(message, optional, tag = "3")]
    pub provider_options: ::core::option::Option<InferenceProviderOptions>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceInvocationIdInfo {
    #[prost(string, tag = "1")]
    pub invocation_id: ::prost::alloc::string::String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum InferenceMessageRole {
    Unspecified = 0,
    User = 1,
    Assistant = 2,
    Tool = 3,
    System = 4,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceModelConfig {
    #[prost(int32, optional, tag = "1")]
    pub max_tokens: ::core::option::Option<i32>,
    #[prost(float, optional, tag = "2")]
    pub temperature: ::core::option::Option<f32>,
    #[prost(float, optional, tag = "3")]
    pub top_p: ::core::option::Option<f32>,
    #[prost(string, repeated, tag = "4")]
    pub stop_sequences: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceModelParameterValue {
    #[prost(string, tag = "1")]
    pub id: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub value: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceNamedProviderDefinedTool {
    #[prost(string, tag = "1")]
    pub name: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub id: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub r#type: ::prost::alloc::string::String,
    #[prost(message, optional, tag = "4")]
    pub options: ::core::option::Option<::prost_types::Struct>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceProviderMetadataInfo {
    #[prost(message, optional, tag = "1")]
    pub metadata: ::core::option::Option<::prost_types::Struct>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceProviderOptions {
    #[prost(message, optional, tag = "1")]
    pub anthropic: ::core::option::Option<InferenceAnthropicOptions>,
    #[prost(message, optional, tag = "2")]
    pub cursor: ::core::option::Option<InferenceCursorOptions>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceProviderWarning {
    #[prost(string, tag = "1")]
    pub message: ::prost::alloc::string::String,
    #[prost(string, repeated, tag = "2")]
    pub affected_models: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    #[prost(string, optional, tag = "3")]
    pub fallback_model: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(enumeration = "InferenceProviderWarningTrigger", tag = "4")]
    pub trigger: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum InferenceProviderWarningTrigger {
    Unspecified = 0,
    OnSelect = 1,
    OnError = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum InferenceReason {
    Unspecified = 0,
    GeminiVideoSubagent = 1,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceReasoningPart {
    #[prost(bool, tag = "1")]
    pub is_redacted: bool,
    #[prost(string, tag = "2")]
    pub text: ::prost::alloc::string::String,
    #[prost(string, optional, tag = "3")]
    pub signature: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(string, optional, tag = "4")]
    pub redacted_data: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(string, optional, tag = "5")]
    pub model_name: ::core::option::Option<::prost::alloc::string::String>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceRequestedModel {
    #[prost(string, tag = "1")]
    pub model_id: ::prost::alloc::string::String,
    #[prost(bool, tag = "2")]
    pub max_mode: bool,
    #[prost(message, repeated, tag = "3")]
    pub parameters: ::prost::alloc::vec::Vec<InferenceModelParameterValue>,
    #[prost(bool, tag = "4")]
    pub built_in_model: bool,
    #[prost(bool, tag = "5")]
    pub is_variant_string_representation: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceResponseInfo {
    #[prost(string, tag = "1")]
    pub id: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub model: ::prost::alloc::string::String,
    #[prost(int64, tag = "3")]
    pub created_at: i64,
    #[prost(message, repeated, tag = "4")]
    pub messages: ::prost::alloc::vec::Vec<InferenceResponseMessage>,
    #[prost(string, optional, tag = "5")]
    pub error_message: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(message, optional, tag = "6")]
    pub inference_extra_data: ::core::option::Option<InferenceExtraData>,
    #[prost(bool, optional, tag = "7")]
    pub supports_self_summary: ::core::option::Option<bool>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceResponseMessage {
    #[prost(string, tag = "1")]
    pub id: ::prost::alloc::string::String,
    #[prost(enumeration = "InferenceMessageRole", tag = "2")]
    pub role: i32,
    #[prost(string, optional, tag = "3")]
    pub content: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(message, repeated, tag = "4")]
    pub tool_calls: ::prost::alloc::vec::Vec<InferenceToolCall>,
    #[prost(message, optional, tag = "5")]
    pub tool_result: ::core::option::Option<InferenceToolResultContent>,
    #[prost(message, repeated, tag = "6")]
    pub reasoning_parts: ::prost::alloc::vec::Vec<InferenceReasoningPart>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceRoutingRow {
    #[prost(string, repeated, tag = "1")]
    pub values: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceStreamError {
    #[prost(string, tag = "1")]
    pub message: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub code: ::prost::alloc::string::String,
    #[prost(bool, tag = "3")]
    pub is_input_token_limit_error: bool,
    #[prost(bool, tag = "4")]
    pub is_output_token_limit_error: bool,
    #[prost(enumeration = "InferenceStreamErrorType", tag = "5")]
    pub error_type: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
#[repr(i32)]
pub enum InferenceStreamErrorType {
    Unspecified = 0,
    Unknown = 1,
    InputTokenLimit = 2,
    OutputTokenLimit = 3,
    RateLimit = 4,
    Authentication = 5,
    Permission = 6,
    Overloaded = 7,
    ContentFilter = 8,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceStreamRequest {
    #[prost(message, repeated, tag = "1")]
    pub messages: ::prost::alloc::vec::Vec<InferenceCoreMessage>,
    #[prost(message, repeated, tag = "2")]
    pub tools: ::prost::alloc::vec::Vec<InferenceAgentTool>,
    #[prost(message, repeated, tag = "3")]
    pub provider_defined_tools: ::prost::alloc::vec::Vec<InferenceNamedProviderDefinedTool>,
    #[prost(message, optional, tag = "4")]
    pub model_config: ::core::option::Option<InferenceModelConfig>,
    #[prost(string, optional, tag = "5")]
    pub model_id: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(string, optional, tag = "6")]
    pub invocation_id: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(message, optional, tag = "7")]
    pub requested_model: ::core::option::Option<InferenceRequestedModel>,
    #[prost(string, optional, tag = "8")]
    pub conversation_id: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(string, repeated, tag = "9")]
    pub accepted_unadvertised_tool_names: ::prost::alloc::vec::Vec<::prost::alloc::string::String>,
    #[prost(string, optional, tag = "10")]
    pub automation_id: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(enumeration = "InferenceReason", optional, tag = "11")]
    pub inference_reason: ::core::option::Option<i32>,
    #[prost(string, optional, tag = "12")]
    pub conversation_group_id: ::core::option::Option<::prost::alloc::string::String>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceStreamResponse {
    #[prost(
        oneof = "inference_stream_response::Response",
        tags = "1, 2, 3, 4, 5, 6, 7, 8, 9, 10"
    )]
    pub response: ::core::option::Option<inference_stream_response::Response>,
}
/// `InferenceStreamResponse` 的 oneof。
pub mod inference_stream_response {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Response {
        #[prost(message, tag = "1")]
        TextPart(super::InferenceTextStreamPart),
        #[prost(message, tag = "2")]
        ToolCallPart(super::InferenceToolCallStreamPart),
        #[prost(message, tag = "3")]
        Usage(super::InferenceUsageInfo),
        #[prost(message, tag = "4")]
        ResponseInfo(super::InferenceResponseInfo),
        #[prost(message, tag = "5")]
        ExtendedUsage(super::InferenceExtendedUsageInfo),
        #[prost(message, tag = "6")]
        ProviderMetadata(super::InferenceProviderMetadataInfo),
        #[prost(message, tag = "7")]
        InvocationId(super::InferenceInvocationIdInfo),
        #[prost(message, tag = "8")]
        Error(super::InferenceStreamError),
        #[prost(message, tag = "9")]
        ThinkingPart(super::InferenceThinkingStreamPart),
        #[prost(message, tag = "10")]
        ImageDescriptions(super::InferenceImageDescriptionsInfo),
    }
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceTextPart {
    #[prost(string, tag = "1")]
    pub text: ::prost::alloc::string::String,
    #[prost(message, optional, tag = "2")]
    pub provider_options: ::core::option::Option<InferenceProviderOptions>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceTextStreamPart {
    #[prost(string, tag = "1")]
    pub text: ::prost::alloc::string::String,
    #[prost(bool, tag = "2")]
    pub is_final: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceThinkingStreamPart {
    #[prost(string, tag = "1")]
    pub text: ::prost::alloc::string::String,
    #[prost(string, optional, tag = "2")]
    pub signature: ::core::option::Option<::prost::alloc::string::String>,
    #[prost(bool, tag = "3")]
    pub is_final: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceTokenIds {
    #[prost(int64, repeated, tag = "1")]
    pub values: ::prost::alloc::vec::Vec<i64>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceTokenLogprobs {
    #[prost(double, repeated, tag = "1")]
    pub values: ::prost::alloc::vec::Vec<f64>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceToolCall {
    #[prost(string, tag = "1")]
    pub tool_call_id: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub tool_name: ::prost::alloc::string::String,
    #[prost(message, optional, tag = "3")]
    pub args: ::core::option::Option<::prost_types::Struct>,
    #[prost(string, optional, tag = "4")]
    pub raw_tool_call_args: ::core::option::Option<::prost::alloc::string::String>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceToolCallStreamPart {
    #[prost(string, tag = "1")]
    pub tool_call_id: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub tool_name: ::prost::alloc::string::String,
    #[prost(string, tag = "3")]
    pub args: ::prost::alloc::string::String,
    #[prost(bool, tag = "4")]
    pub is_complete: bool,
    #[prost(int32, optional, tag = "5")]
    pub tool_index: ::core::option::Option<i32>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceToolResultContent {
    #[prost(message, repeated, tag = "1")]
    pub parts: ::prost::alloc::vec::Vec<InferenceToolResultPart>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceToolResultPart {
    #[prost(string, tag = "1")]
    pub tool_call_id: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub tool_name: ::prost::alloc::string::String,
    #[prost(message, optional, tag = "3")]
    pub result: ::core::option::Option<::prost_types::Value>,
    #[prost(bool, tag = "4")]
    pub is_error: bool,
    #[prost(message, repeated, tag = "5")]
    pub experimental_content: ::prost::alloc::vec::Vec<InferenceContentPart>,
    #[prost(message, optional, tag = "6")]
    pub provider_options: ::core::option::Option<InferenceProviderOptions>,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct InferenceUsageInfo {
    #[prost(int32, tag = "1")]
    pub prompt_tokens: i32,
    #[prost(int32, tag = "2")]
    pub completion_tokens: i32,
    #[prost(int32, optional, tag = "3")]
    pub total_tokens: ::core::option::Option<i32>,
}
