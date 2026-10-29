//! 上游错误的分类。
//!
//! 分类不是为了好看，每一类对应一种处置：`Auth` / `Quota` 该把号标掉，`RateLimit` 该让
//! 这个号冷却，`Provider` 是上游整体在抖、换号无用该退避，`BadRequest` 是这轮对话的问题、
//! 换哪个号都必然重现、不该罚号也不该重试。混为一谈的代价云端 gateway 付过：一次上游抖动
//! 被当成限流，打空了整个号池。

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamKind {
    /// 401：token 失效 / 未登录。
    Auth,
    /// 403：这个号没这个权限。
    Forbidden,
    /// 402：额度 / 账单。
    Quota,
    /// 429：这个号这个模型现在别用。
    RateLimit,
    /// 429：上游模型供应商自己在抖，每个号都会撞上。
    Provider,
    /// 400：请求本身的问题（模型名、参数、上下文超长、循环检测）。
    BadRequest,
    /// 404：这个号出不了这个模型（别的号可能能）。
    ModelUnsupported,
    /// 499：没人要这个结果了。
    Canceled,
    /// 504：我们自己掐的——客户端还在等，必须把原因告诉它。
    Timeout,
    /// 502：其余。
    Upstream,
}

impl UpstreamKind {
    pub fn as_str(self) -> &'static str {
        match self {
            UpstreamKind::Auth => "auth",
            UpstreamKind::Forbidden => "forbidden",
            UpstreamKind::Quota => "quota",
            UpstreamKind::RateLimit => "rate_limit",
            UpstreamKind::Provider => "provider",
            UpstreamKind::BadRequest => "bad_request",
            UpstreamKind::ModelUnsupported => "model_unsupported",
            UpstreamKind::Canceled => "canceled",
            UpstreamKind::Timeout => "timeout",
            UpstreamKind::Upstream => "upstream",
        }
    }

    /// 出问题的是号（值得换一个 / 罚它）还是这轮请求本身。
    pub fn blames_account(self) -> bool {
        matches!(
            self,
            UpstreamKind::Auth
                | UpstreamKind::Forbidden
                | UpstreamKind::Quota
                | UpstreamKind::RateLimit
                | UpstreamKind::ModelUnsupported
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamError {
    pub kind: UpstreamKind,
    /// HTTP 语义的状态码，给入站层回客户端用。
    pub status: u16,
    /// 给人看的原文，不带上游名字。
    pub message: String,
    /// Cursor 自己的错误枚举名（`ERROR_CUSTOM_MESSAGE` 这类），有就带上，排障用。
    pub cursor_code: Option<String>,
    /// 上游明确告知的恢复时刻（Unix 毫秒）。ChatGPT 的 5 小时 / 7 天窗口用满时会给；
    /// lane 据它决定冷却多久，而不是一律按固定时长猜。
    pub reset_at_ms: Option<i64>,
}

impl UpstreamError {
    pub fn new(kind: UpstreamKind, status: u16, message: impl Into<String>) -> Self {
        Self {
            kind,
            status,
            message: message.into(),
            cursor_code: None,
            reset_at_ms: None,
        }
    }

    pub fn with_cursor_code(mut self, code: impl Into<String>) -> Self {
        let code = code.into();
        if !code.is_empty() {
            self.cursor_code = Some(code);
        }
        self
    }

    pub fn with_reset_at_ms(mut self, at: i64) -> Self {
        if at > 0 {
            self.reset_at_ms = Some(at);
        }
        self
    }

    /// 距上游说的恢复时刻还有几秒（向上取整，至少 1）。上游没说就是 `None`。
    pub fn retry_after_secs(&self) -> Option<u64> {
        let at = self.reset_at_ms?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let left = (at - now).max(0);
        Some(((left + 999) / 1000).max(1) as u64)
    }
}

/// 上游说的是不是「上下文太长」。各家措辞不同，收成一份：客户端的自动压缩挂在专门的错误码 /
/// 措辞上（OpenAI 的 `context_length_exceeded`、Anthropic 的 `prompt is too long`），
/// 网关得把它认出来、按客户端的方言说。不收 `too many tokens`：那也是每分钟 token 限流的说法。
pub fn is_context_overflow(message: &str) -> bool {
    let lower = message.to_lowercase();
    [
        "prompt is too long",
        "input is too long",
        "context length",
        "context_length",
        "context window",
        "context_window",
        "maximum context",
        "reduce the length of",
        "content_length_exceeds_threshold",
        "exceeds the context",
        "上下文过长",
        "上下文超长",
        "超出上下文",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

impl fmt::Display for UpstreamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} error: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for UpstreamError {}
