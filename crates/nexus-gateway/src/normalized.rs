//! 入站协议与上游之间的中间表示。
//!
//! 移植自 `gateway/src/normalized.ts`，按本地网关裁剪：不做 N×M 的协议直转，OpenAI / Anthropic
//! 两种入站方言都先变成这一份，再喂给 Cursor 的 Stream。形状按 Anthropic Messages 建模——它有
//! 明确的内容块、tool_use / tool_result 配对、独立的 thinking，表达力最强，往 OpenAI 降级无损；
//! 反过来以 OpenAI Chat 为中心会丢掉 thinking 和多块结构，丢了就补不回来。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// 内联图片。只收 base64——Cursor 这条路把图塞进 protobuf，给链接的没法处理，
/// 入站层要在更早的地方诚实拒绝，而不是静默丢图让模型对着空气作答。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImageInput {
    /// base64，不含 data URL 前缀。
    pub data: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON 字符串。保持字符串而不是解析成对象：上游给的可能不是合法 JSON，
    /// 提前 parse 会把「原样透传」变成「解析失败」。
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub tool_name: String,
    pub text: String,
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub role: Role,
    /// 不含工具标记的纯文本。Cursor Stream 有一等工具协议，工具调用 / 结果走各自的字段。
    pub text: String,
    pub images: Vec<ImageInput>,
    /// 只对 assistant 有意义。
    pub tool_calls: Vec<ToolCall>,
    /// 只对 tool 有意义。
    pub tool_results: Vec<ToolResult>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema。
    pub parameters: serde_json::Value,
    /// 客户端按 `type: "custom"` 声明的语法工具（Codex 的 `exec` / `apply_patch`）。它没有
    /// JSON 参数，只有一段原文：给上游时套 `{ input }` 的壳，回程必须拆壳还原成
    /// `custom_tool_call`。按 `function_call` 发回去的话，Codex 的 handler 认不出这种载荷，
    /// 把整次调用当致命错误吞掉、不记任何结果，模型下一轮看到的就是一个「aborted」。
    pub grammar: bool,
    /// 声明时所属的非默认命名空间（Responses 的 namespace 工具组）。Codex 按
    /// `{namespace, name}` 找 handler，回程丢了它就是 unsupported call。默认空间
    /// `functions` 不记，和 OpenAI 官方行为一致。
    pub namespace: Option<String>,
}

/// 客户端对「要不要调工具」的要求。三种方言写法各异，语义只有这四种。丢掉它的后果
/// 很具体：客户端要求「必须调工具」，模型却回了段文本，客户端解析不到工具调用，一轮白跑。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ToolChoice {
    #[default]
    Auto,
    None,
    Required,
    Tool(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sampling {
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub stop_sequences: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChatRequest {
    /// 客户端要的模型名；空或 `auto` 由上游自选。
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub tool_choice: ToolChoice,
    pub sampling: Sampling,
    /// 会话标识，透传给上游做对话缓存；缺省每次随机。
    pub conversation_id: Option<String>,
    /// 客户端讲的是 Responses 方言时，原始请求体。
    ///
    /// 中间表示是有损的：reasoning 的回放凭据、`compaction_trigger`、工具命名空间、
    /// `client_metadata` 这些进不来。上游恰好也讲 Responses 时（ChatGPT 直连），后端拿它
    /// 原样透传，只改上游会拒收的地方——到达上游的东西和真实 Codex 客户端几乎无法区分。
    /// 其他方言恒为 `None`。
    pub raw_responses: Option<std::sync::Arc<serde_json::Value>>,
    /// 客户端请求头里与协议有关的那几个（小写键，白名单见 `server::pick_client_headers`）。
    /// Codex CLI 每个请求都带一组 `x-codex-*`，转给上游前按账号收敛。
    pub client_headers: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Error,
}

impl FinishReason {
    pub fn as_str(self) -> &'static str {
        match self {
            FinishReason::Stop => "stop",
            FinishReason::Length => "length",
            FinishReason::ToolCalls => "tool_calls",
            FinishReason::ContentFilter => "content_filter",
            FinishReason::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_write_tokens: u32,
    pub reasoning_tokens: u32,
}

/// 流式增量。刻意比任何单一协议都窄：各方言的花样（OpenAI 的 index、Anthropic 的
/// content_block_start）由序列化层自己补。工具调用不流式下发——Stream 按 `tool_index`
/// 分片攒 args，攒完整了才交出去，半截 JSON 对客户端没有用。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delta {
    Text(String),
    Thinking(String),
    /// 上游与客户端讲同一种方言时原样透传的一帧（Responses 事件）。序列化层不参与：server
    /// 直接写出去。只有透传后端会发它，而且发了它就不再发 `Text` / `Thinking`。
    Raw {
        event: String,
        data: String,
    },
    /// 上游响应头里值得转给客户端的那几个（会话状态印、额度）。在任何内容帧之前、至多一次；
    /// 它不是给客户端的字节，换号重试的判据不算它。
    Headers(Vec<(String, String)>),
}

/// 一轮跑完的结果。
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    pub text: String,
    pub thinking: String,
    pub tool_calls: Vec<ToolCall>,
    pub finish_reason: FinishReason,
    pub usage: Usage,
    /// usage 是上游报的（true）还是我们估的（false）。估出来的缓存读写恒为 0——那是
    /// 「没测到」不是「没命中」，做统计时要按它过滤。
    pub usage_measured: bool,
    /// 上游自称实际路由到的模型。和请求的比对，能抓到降级 / 掺水。
    pub routed_model: Option<String>,
    pub ttft_ms: Option<u64>,
    pub turn_ms: u64,
    /// 透传时上游最终的 `response` 对象（`response.completed` 里那份）。非流式请求直接回它，
    /// 而不是照着中间表示再拼一遍。其他情况 `None`。
    pub raw_response: Option<serde_json::Value>,
}

/// 粗估 token 数：中日韩 1.5 字符一个、其余 4 字符一个，至少 1。只在上游没报用量时兜底，
/// 以及本地执行输出上限时判断——真实用量以上游 `extended_usage` 为准。
pub fn estimate_tokens(text: &str) -> u32 {
    if text.is_empty() {
        return 0;
    }
    let (mut ascii, mut cjk) = (0u32, 0u32);
    for ch in text.chars() {
        let c = ch as u32;
        if (0x2e80..=0x9fff).contains(&c) || (0xac00..=0xd7a3).contains(&c) {
            cjk += 1;
        } else {
            ascii += 1;
        }
    }
    ((f64::from(ascii) / 4.0 + f64::from(cjk) / 1.5).round() as u32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_matches_protocol_js() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1, "非空至少 1");
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("hello world"), 3); // round(11 / 4) = 3
        assert_eq!(estimate_tokens("你好"), 1); // round(2 / 1.5) = 1
        assert_eq!(estimate_tokens("你好世界"), 3); // round(4 / 1.5) = 3
        assert_eq!(estimate_tokens("hi 你好"), 2); // round(3/4 + 2/1.5) = round(2.08) = 2
    }

    #[test]
    fn finish_reason_strings_are_the_openai_ones() {
        assert_eq!(FinishReason::ToolCalls.as_str(), "tool_calls");
        assert_eq!(FinishReason::Length.as_str(), "length");
    }
}
