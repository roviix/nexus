//! ConnectRPC 的调用（binary protobuf over HTTP/2）：服务端流式 + 一元。
//!
//! Connect 协议比 gRPC 朴素得多，这里只实现我们要用的那两小片：
//!
//! - 流式请求：`POST {base}/{Service}/{Method}`，`content-type: application/connect+proto`，
//!   body 是**一个**信封（5 字节前缀 + protobuf）。
//! - 流式响应：一串信封。前缀第 1 字节是 flags（bit0 压缩、bit1 流结束），随后 4 字节大端长度。
//!   最后一个信封带 END_STREAM，载荷是 JSON：`{"error": {...}, "metadata": {...}}`，
//!   正常结束时 `error` 缺省。HTTP 状态几乎总是 200——业务错误在流尾那个 JSON 里。
//! - 一元（生图那条路）：**没有信封**，`content-type: application/proto`，请求体和响应体都是
//!   裸 protobuf；错误反过来走 HTTP 状态码 + JSON 体（`{"code","message","details"}`）。
//!   两者的错误位置不同，是协议本身如此，不是我们两套写法。
//!
//! 我们不声明 `connect-accept-encoding`，按协议服务端就不会压缩响应；万一它压了，
//! [`ConnectFailure::CompressedFrame`] 会把这件事说出来而不是解出一堆乱码。

use crate::headers::HeaderList;
use serde::Deserialize;
use std::fmt;

pub const CONTENT_TYPE_STREAM: &str = "application/connect+proto";
pub const CONTENT_TYPE_UNARY: &str = "application/proto";

/// 单个信封能有多大。上游一条流式消息不会到这个量级，超过只可能是分帧错位。
const MAX_ENVELOPE_LEN: usize = 64 * 1024 * 1024;
/// 非 200 时最多留多少响应体给人看。
const HTTP_BODY_PREVIEW: usize = 2000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub flags: u8,
    pub data: Vec<u8>,
}

impl Envelope {
    pub const FLAG_COMPRESSED: u8 = 0b01;
    pub const FLAG_END_STREAM: u8 = 0b10;

    pub fn message(data: Vec<u8>) -> Self {
        Self { flags: 0, data }
    }

    pub fn is_compressed(&self) -> bool {
        self.flags & Self::FLAG_COMPRESSED != 0
    }

    pub fn is_end_stream(&self) -> bool {
        self.flags & Self::FLAG_END_STREAM != 0
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(5 + self.data.len());
        out.push(self.flags);
        out.extend_from_slice(&(self.data.len() as u32).to_be_bytes());
        out.extend_from_slice(&self.data);
        out
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FramingError {
    /// 长度字段离谱，几乎肯定是分帧错位或不是 Connect 响应。
    OversizedEnvelope(usize),
    /// 连接在一个信封中间断了。
    Truncated { pending: usize },
}

impl fmt::Display for FramingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FramingError::OversizedEnvelope(n) => write!(f, "信封声明长度 {n} 字节，超出上限"),
            FramingError::Truncated { pending } => {
                write!(f, "流在信封中间断开（还差数据，已缓存 {pending} 字节）")
            }
        }
    }
}

/// 把网络上零散到达的字节切成信封。分片边界和信封边界毫无关系，所以必须攒。
#[derive(Debug, Default)]
pub struct EnvelopeDecoder {
    buf: Vec<u8>,
}

impl EnvelopeDecoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn pending_bytes(&self) -> usize {
        self.buf.len()
    }

    /// 取下一个完整信封；`Ok(None)` 表示还要更多字节。
    pub fn next_envelope(&mut self) -> Result<Option<Envelope>, FramingError> {
        if self.buf.len() < 5 {
            return Ok(None);
        }
        let flags = self.buf[0];
        let len = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
        if len > MAX_ENVELOPE_LEN {
            return Err(FramingError::OversizedEnvelope(len));
        }
        if self.buf.len() < 5 + len {
            return Ok(None);
        }
        let data = self.buf[5..5 + len].to_vec();
        self.buf.drain(..5 + len);
        Ok(Some(Envelope { flags, data }))
    }
}

/// 流尾的 `EndStreamResponse`。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct EndStream {
    #[serde(default)]
    pub error: Option<ConnectError>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Connect 的错误体。`code` 是 gRPC 风格的小写字符串（`unauthenticated` / `resource_exhausted` …）。
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
pub struct ConnectError {
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub details: Vec<serde_json::Value>,
}

/// Cursor 塞进 `details` 的应用层错误：
///
/// ```text
/// { error: "ERROR_CUSTOM_MESSAGE", details: { title, detail }, isExpected }
/// ```
///
/// 以前只取 `error` 枚举名，线上就只剩一句空壳，排障只能猜。title / detail 才是
/// 「循环检测 / 模型报错 / 上下文过长」的原文。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CursorErrorInfo {
    pub code: String,
    pub title: String,
    pub detail: String,
    pub expected: Option<bool>,
}

impl ConnectError {
    pub fn cursor_info(&self) -> CursorErrorInfo {
        let mut info = CursorErrorInfo::default();
        for d in &self.details {
            let Some(dbg) = d
                .get("debug")
                .or_else(|| d.get("value").and_then(|v| v.get("debug")))
                .filter(|v| v.is_object())
            else {
                continue;
            };
            if let Some(code) = dbg.get("error").and_then(|v| v.as_str()) {
                if !code.is_empty() {
                    info.code = code.to_string();
                }
            }
            let inner = dbg.get("details").filter(|v| v.is_object()).unwrap_or(dbg);
            if let Some(t) = inner.get("title").and_then(|v| v.as_str()) {
                if !t.is_empty() {
                    info.title = t.to_string();
                }
            }
            match inner.get("detail").and_then(|v| v.as_str()) {
                Some(dt) if !dt.is_empty() => info.detail = dt.to_string(),
                _ => {
                    if info.detail.is_empty() {
                        if let Some(m) = inner.get("message").and_then(|v| v.as_str()) {
                            if !m.is_empty() {
                                info.detail = m.to_string();
                            }
                        }
                    }
                }
            }
            if let Some(b) = dbg.get("isExpected").and_then(|v| v.as_bool()) {
                info.expected = Some(b);
            }
            if !info.code.is_empty() || !info.title.is_empty() || !info.detail.is_empty() {
                break;
            }
        }
        if info.code.is_empty() && info.title.is_empty() && info.detail.is_empty() {
            info.detail = self.message.clone();
        }
        info
    }
}

impl CursorErrorInfo {
    /// `CODE: title: detail`，去重、限长，给人看也给分类器看。
    pub fn format(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if !self.code.is_empty() {
            parts.push(&self.code);
        }
        if !self.title.is_empty() && self.title != self.code {
            parts.push(&self.title);
        }
        if !self.detail.is_empty() && self.detail != self.title && self.detail != self.code {
            parts.push(&self.detail);
        }
        let s = parts.join(": ");
        if s.is_empty() {
            return "unknown error".into();
        }
        if s.chars().count() > 500 {
            let cut: String = s.chars().take(497).collect();
            return format!("{cut}…");
        }
        s
    }
}

#[derive(Debug)]
pub enum ConnectFailure {
    /// 连不上 / TLS / 读流中断。
    Transport(reqwest::Error),
    /// 服务端没按 Connect 讲话，直接回了非 200（多半是前面的网关或鉴权层）。
    Http {
        status: u16,
        body: String,
    },
    /// 一元调用的应用层错误：Connect 把它放在非 200 的 JSON 体里（流式那条放在流尾信封）。
    /// 和 [`ConnectFailure::Http`] 分开，是因为这一种能拿到 Cursor 的错误枚举与原文，
    /// 归类质量完全不同。
    Rpc {
        status: u16,
        error: ConnectError,
    },
    Framing(FramingError),
    /// 我们没声明接受压缩，服务端却压了。
    CompressedFrame,
    /// 流尾 JSON 解不开。
    BadEndStream(String),
}

impl fmt::Display for ConnectFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectFailure::Transport(e) => write!(f, "传输失败：{e}"),
            ConnectFailure::Http { status, body } => {
                write!(
                    f,
                    "HTTP {status}：{}",
                    body.chars().take(200).collect::<String>()
                )
            }
            ConnectFailure::Rpc { status, error } => {
                write!(f, "HTTP {status}：{}", error.cursor_info().format())
            }
            ConnectFailure::Framing(e) => write!(f, "分帧错误：{e}"),
            ConnectFailure::CompressedFrame => write!(f, "收到压缩信封，但未声明接受压缩"),
            ConnectFailure::BadEndStream(e) => write!(f, "流尾 JSON 解析失败：{e}"),
        }
    }
}

impl std::error::Error for ConnectFailure {}

impl From<reqwest::Error> for ConnectFailure {
    fn from(e: reqwest::Error) -> Self {
        ConnectFailure::Transport(e)
    }
}

impl From<FramingError> for ConnectFailure {
    fn from(e: FramingError) -> Self {
        ConnectFailure::Framing(e)
    }
}

pub enum StreamItem {
    /// 一条 protobuf 消息（未解码）。
    Message(Vec<u8>),
    /// 流正常走到尾。
    End(EndStream),
    /// 连接关了但没见到 END_STREAM 信封。
    Eof,
}

/// 一次进行中的服务端流。
pub struct ServerStream {
    resp: reqwest::Response,
    decoder: EnvelopeDecoder,
    finished: bool,
}

/// 发起一次服务端流式调用。非 200 直接失败，不去猜它的 body 是什么形状。
pub async fn call_server_stream(
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderList,
    request: &[u8],
) -> Result<ServerStream, ConnectFailure> {
    let mut req = client
        .post(url)
        .header("content-type", CONTENT_TYPE_STREAM)
        .body(Envelope::message(request.to_vec()).encode());
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let resp = req.send().await?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let body = body.chars().take(HTTP_BODY_PREVIEW).collect();
        return Err(ConnectFailure::Http {
            status: status.as_u16(),
            body,
        });
    }
    Ok(ServerStream {
        resp,
        decoder: EnvelopeDecoder::default(),
        finished: false,
    })
}

/// 发起一次一元调用，拿回响应的裸 protobuf 字节。
///
/// 与流式那条的差别全在协议本身：请求体不裹信封，成功是 200 + 裸 protobuf，失败是非 200 +
/// JSON 错误体。JSON 解得开就当 [`ConnectFailure::Rpc`]（有 Cursor 的错误枚举与原文可归类），
/// 解不开才退回 [`ConnectFailure::Http`]——那多半是前置网关的 HTML 错误页，硬当 Connect 解
/// 只会得到一句更糊涂的话。
pub async fn call_unary(
    client: &reqwest::Client,
    url: &str,
    headers: &HeaderList,
    request: &[u8],
) -> Result<Vec<u8>, ConnectFailure> {
    let mut req = client
        .post(url)
        .header("content-type", CONTENT_TYPE_UNARY)
        .body(request.to_vec());
    for (k, v) in headers {
        req = req.header(*k, v);
    }
    let resp = req.send().await?;
    let status = resp.status();
    if status.is_success() {
        return Ok(resp.bytes().await?.to_vec());
    }
    let body = resp.text().await.unwrap_or_default();
    match serde_json::from_str::<ConnectError>(&body) {
        Ok(error) if !error.code.is_empty() || !error.message.is_empty() => {
            Err(ConnectFailure::Rpc {
                status: status.as_u16(),
                error,
            })
        }
        _ => Err(ConnectFailure::Http {
            status: status.as_u16(),
            body: body.chars().take(HTTP_BODY_PREVIEW).collect(),
        }),
    }
}

impl ServerStream {
    pub async fn next(&mut self) -> Result<StreamItem, ConnectFailure> {
        if self.finished {
            return Ok(StreamItem::Eof);
        }
        loop {
            if let Some(env) = self.decoder.next_envelope()? {
                if env.is_compressed() {
                    return Err(ConnectFailure::CompressedFrame);
                }
                if env.is_end_stream() {
                    self.finished = true;
                    let end = if env.data.is_empty() {
                        EndStream::default()
                    } else {
                        serde_json::from_slice(&env.data)
                            .map_err(|e| ConnectFailure::BadEndStream(e.to_string()))?
                    };
                    return Ok(StreamItem::End(end));
                }
                return Ok(StreamItem::Message(env.data));
            }
            match self.resp.chunk().await? {
                Some(bytes) => self.decoder.push(&bytes),
                None => {
                    self.finished = true;
                    let pending = self.decoder.pending_bytes();
                    if pending > 0 {
                        return Err(FramingError::Truncated { pending }.into());
                    }
                    return Ok(StreamItem::Eof);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_encodes_a_five_byte_prefix() {
        let e = Envelope::message(vec![1, 2, 3]);
        assert_eq!(e.encode(), vec![0, 0, 0, 0, 3, 1, 2, 3]);
        let end = Envelope {
            flags: Envelope::FLAG_END_STREAM,
            data: b"{}".to_vec(),
        };
        assert_eq!(end.encode(), vec![2, 0, 0, 0, 2, b'{', b'}']);
    }

    #[test]
    fn decoder_reassembles_frames_split_at_arbitrary_points() {
        let a = Envelope::message(vec![9; 7]).encode();
        let b = Envelope {
            flags: Envelope::FLAG_END_STREAM,
            data: b"{}".to_vec(),
        }
        .encode();
        let wire: Vec<u8> = a.iter().chain(b.iter()).copied().collect();

        // 一个字节一个字节喂，任何切点都不能让它解错。
        let mut dec = EnvelopeDecoder::default();
        let mut got = Vec::new();
        for byte in wire {
            dec.push(&[byte]);
            while let Some(env) = dec.next_envelope().unwrap() {
                got.push(env);
            }
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].data, vec![9; 7]);
        assert!(!got[0].is_end_stream());
        assert!(got[1].is_end_stream());
        assert_eq!(got[1].data, b"{}");
        assert_eq!(dec.pending_bytes(), 0);
    }

    #[test]
    fn decoder_handles_two_frames_in_one_push_and_an_empty_frame() {
        let mut dec = EnvelopeDecoder::default();
        let mut wire = Envelope::message(vec![]).encode();
        wire.extend(Envelope::message(vec![1]).encode());
        dec.push(&wire);
        assert_eq!(dec.next_envelope().unwrap().unwrap().data, Vec::<u8>::new());
        assert_eq!(dec.next_envelope().unwrap().unwrap().data, vec![1]);
        assert!(dec.next_envelope().unwrap().is_none());
    }

    #[test]
    fn decoder_waits_for_the_rest_of_a_partial_frame() {
        let mut dec = EnvelopeDecoder::default();
        dec.push(&[0, 0, 0, 0, 4, 1, 2]);
        assert!(dec.next_envelope().unwrap().is_none());
        assert_eq!(dec.pending_bytes(), 7);
        dec.push(&[3, 4]);
        assert_eq!(dec.next_envelope().unwrap().unwrap().data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn decoder_rejects_absurd_lengths_instead_of_buffering_forever() {
        let mut dec = EnvelopeDecoder::default();
        dec.push(&[0, 0xff, 0xff, 0xff, 0xff]);
        assert_eq!(
            dec.next_envelope(),
            Err(FramingError::OversizedEnvelope(0xffff_ffff))
        );
    }

    #[test]
    fn compressed_flag_is_recognised() {
        let e = Envelope {
            flags: Envelope::FLAG_COMPRESSED,
            data: vec![],
        };
        assert!(e.is_compressed());
        assert!(!e.is_end_stream());
    }

    #[test]
    fn end_stream_json_parses_with_and_without_an_error() {
        let ok: EndStream = serde_json::from_str("{}").unwrap();
        assert!(ok.error.is_none());
        let ok2: EndStream = serde_json::from_str(r#"{"metadata":{"x":["1"]}}"#).unwrap();
        assert!(ok2.error.is_none());
        let bad: EndStream = serde_json::from_str(
            r#"{"error":{"code":"unauthenticated","message":"nope","details":[]}}"#,
        )
        .unwrap();
        let err = bad.error.unwrap();
        assert_eq!(err.code, "unauthenticated");
        assert_eq!(err.message, "nope");
    }

    #[test]
    fn cursor_info_reads_the_debug_payload_cursor_puts_in_details() {
        let err: ConnectError = serde_json::from_str(
            r#"{"code":"resource_exhausted","message":"x","details":[{
                "type":"aiserver.v1.ErrorDetails","value":"AAA",
                "debug":{"error":"ERROR_CUSTOM_MESSAGE","details":{"title":"Too many computers","detail":"Sign out elsewhere."},"isExpected":true}
            }]}"#,
        )
        .unwrap();
        let info = err.cursor_info();
        assert_eq!(info.code, "ERROR_CUSTOM_MESSAGE");
        assert_eq!(info.title, "Too many computers");
        assert_eq!(info.detail, "Sign out elsewhere.");
        assert_eq!(info.expected, Some(true));
        assert_eq!(
            info.format(),
            "ERROR_CUSTOM_MESSAGE: Too many computers: Sign out elsewhere."
        );
    }

    #[test]
    fn cursor_info_accepts_debug_nested_under_value_and_message_as_detail() {
        let err: ConnectError = serde_json::from_str(
            r#"{"code":"internal","message":"raw","details":[{"value":{"debug":{"error":"ERROR_PROVIDER_ERROR","message":"provider down"}}}]}"#,
        )
        .unwrap();
        let info = err.cursor_info();
        assert_eq!(info.code, "ERROR_PROVIDER_ERROR");
        assert_eq!(info.detail, "provider down");
        assert_eq!(info.format(), "ERROR_PROVIDER_ERROR: provider down");
    }

    #[test]
    fn cursor_info_falls_back_to_the_connect_message() {
        let err = ConnectError {
            code: "unavailable".into(),
            message: "upstream unavailable".into(),
            details: vec![],
        };
        let info = err.cursor_info();
        assert_eq!(info.detail, "upstream unavailable");
        assert_eq!(info.format(), "upstream unavailable");
        assert_eq!(CursorErrorInfo::default().format(), "unknown error");
    }

    #[test]
    fn format_dedups_repeated_pieces_and_caps_length() {
        let same = CursorErrorInfo {
            code: "E".into(),
            title: "E".into(),
            detail: "E".into(),
            expected: None,
        };
        assert_eq!(same.format(), "E");
        let long = CursorErrorInfo {
            detail: "x".repeat(600),
            ..Default::default()
        };
        let f = long.format();
        assert_eq!(f.chars().count(), 498);
        assert!(f.ends_with('…'));
    }
}
