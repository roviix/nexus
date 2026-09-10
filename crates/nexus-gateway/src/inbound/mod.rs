//! 入站方言 ⇄ 统一表示。
//!
//! 移植自 `gateway/src/inbound/{parse,serialize}.ts`——那两份是对着真实客户端（Claude Code /
//! Codex / 各家 SDK / Cursor IDE）磨出来的，字段形状与事件顺序照搬。这里做三种方言：
//! OpenAI Chat Completions、Anthropic Messages（Cursor IDE local mode 讲的那一种）、
//! OpenAI Responses（Codex 主用的那一种）。
//!
//! 序列化器**不做 I/O**：每个动作返回一组 [`SseFrame`]，由 server 层写出去。这样每个事件序列
//! 都能不起服务就测到。

pub mod parse;
pub mod serialize;

pub use parse::{parse_request, ParseError, ParsedRequest};
pub use serialize::{protocol_error, Serializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dialect {
    OpenAiChat,
    AnthropicMessages,
    OpenAiResponses,
}

/// 一帧 SSE。`event` 为 `None` 时只写 `data:`（OpenAI 的写法）；`data` 已经是最终文本
/// （JSON 或 `[DONE]`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    pub event: Option<String>,
    pub data: String,
}

impl SseFrame {
    pub fn data(data: impl Into<String>) -> Self {
        Self {
            event: None,
            data: data.into(),
        }
    }

    pub fn event(event: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            event: Some(event.into()),
            data: data.into(),
        }
    }

    pub fn json(event: Option<&str>, value: &serde_json::Value) -> Self {
        Self {
            event: event.map(str::to_string),
            data: value.to_string(),
        }
    }

    /// 注释行。SSE 解析器会忽略它，只用来在首字节之前保活。
    pub fn comment(text: &str) -> String {
        format!(": {text}\n\n")
    }

    pub fn to_wire(&self) -> String {
        match &self.event {
            Some(e) => format!("event: {e}\ndata: {}\n\n", self.data),
            None => format!("data: {}\n\n", self.data),
        }
    }
}

/// `prefix_<随机>`；形状与 gateway 的 `randomId` 一致（客户端只要求唯一，不认格式）。
pub fn random_id(prefix: &str) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &u[..20])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_render_to_sse_wire_format() {
        assert_eq!(SseFrame::data("[DONE]").to_wire(), "data: [DONE]\n\n");
        assert_eq!(
            SseFrame::event("ping", r#"{"type":"ping"}"#).to_wire(),
            "event: ping\ndata: {\"type\":\"ping\"}\n\n"
        );
        assert_eq!(SseFrame::comment("keepalive"), ": keepalive\n\n");
    }

    #[test]
    fn random_ids_carry_the_prefix_and_differ() {
        let a = random_id("chatcmpl");
        let b = random_id("chatcmpl");
        assert!(a.starts_with("chatcmpl_"));
        assert_ne!(a, b);
    }
}
