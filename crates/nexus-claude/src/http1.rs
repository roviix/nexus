//! Claude 通道自己写的 HTTP/1.1。
//!
//! wreq 0.15 把头发成全小写，也没法在控制面去掉 ALPN。这里在 BoringSSL 的流上
//! 按 CLIProxyAPI 抓到的顺序和大小写组请求，再按 `Content-Length` / chunked
//! 把响应解出来。推理长流一截一截交出去，不把整段 SSE 攒在内存里。

use std::collections::VecDeque;
use std::io::{self, Write};
use std::time::Instant;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_boring2::SslStream;

/// 推理和控制面的头顺序不一样，握手也不一样。
#[derive(Clone, Copy, Debug)]
pub enum Profile {
    Inference,
    Control,
}

#[derive(Debug)]
pub struct TransportError {
    pub timeout: bool,
    pub message: String,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for TransportError {}

pub(crate) fn fail(message: impl Into<String>) -> TransportError {
    TransportError {
        timeout: false,
        message: message.into(),
    }
}

pub(crate) fn timed_out(message: impl Into<String>) -> TransportError {
    TransportError {
        timeout: true,
        message: message.into(),
    }
}

/// 已经读完状态行的响应。正文按调用方要的粒度往外给。
pub struct Response {
    status: u16,
    body: HttpBody,
}

impl Response {
    pub fn status(&self) -> u16 {
        self.status
    }

    pub async fn chunk(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        self.body.chunk().await
    }

    pub async fn bytes(mut self) -> Result<Vec<u8>, TransportError> {
        let mut all = Vec::new();
        while let Some(part) = self.chunk().await? {
            all.extend(part);
        }
        Ok(all)
    }

    pub async fn text(self) -> Result<String, TransportError> {
        let bytes = self.bytes().await?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }
}

struct HttpBody {
    stream: SslStream<TcpStream>,
    decoder: Decoder,
    deadline: Option<Instant>,
}

impl HttpBody {
    async fn chunk(&mut self) -> Result<Option<Vec<u8>>, TransportError> {
        loop {
            if let Some(out) = self.decoder.take(8192) {
                return Ok(Some(out));
            }
            if self.decoder.done {
                return Ok(None);
            }
            let mut tmp = [0u8; 8192];
            let n = read_some(&mut self.stream, &mut tmp, self.deadline).await?;
            self.decoder.feed(&tmp[..n], n == 0).map_err(fail)?;
        }
    }
}

pub(crate) async fn exchange(
    mut stream: SslStream<TcpStream>,
    profile: Profile,
    method: &str,
    host: &str,
    port: u16,
    path: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
    deadline: Option<Instant>,
) -> Result<Response, TransportError> {
    let host_value = if port == 443 {
        host.to_string()
    } else {
        format!("{host}:{port}")
    };
    let bytes = encode_request(profile, method, path, &host_value, headers, body).map_err(fail)?;
    write_all(&mut stream, &bytes, deadline).await?;
    let (status, resp_headers, rest) = read_head(&mut stream, deadline).await?;
    let mut decoder = Decoder::new(&resp_headers).map_err(fail)?;
    decoder.feed(&rest, false).map_err(fail)?;
    Ok(Response {
        status,
        body: HttpBody {
            stream,
            decoder,
            deadline,
        },
    })
}

const INFERENCE_HEAD: &[&str] = &[
    "accept",
    "authorization",
    "content-type",
    "user-agent",
    "x-claude-code-session-id",
    "x-stainless-arch",
    "x-stainless-lang",
    "x-stainless-os",
    "x-stainless-package-version",
    "x-stainless-retry-count",
    "x-stainless-runtime",
    "x-stainless-runtime-version",
    "x-stainless-timeout",
    "x-api-key",
    "anthropic-beta",
    "anthropic-dangerous-direct-browser-access",
    "anthropic-version",
    "x-app",
    "x-client-request-id",
];

const CONTROL_HEAD: &[&str] = &[
    "accept",
    "content-type",
    "authorization",
    "cache-control",
    "user-agent",
    "anthropic-beta",
    "x-app",
];

/// 推理末尾是 Connection、Host、Accept-Encoding、Content-Length。
/// 控制面（axios）是 Content-Length、Accept-Encoding、Host、Connection。
const INFERENCE_TAIL: &[&str] = &["connection", "host", "accept-encoding", "content-length"];
const CONTROL_TAIL: &[&str] = &["content-length", "accept-encoding", "host", "connection"];

pub(crate) fn encode_request(
    profile: Profile,
    method: &str,
    path: &str,
    host: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let (head, tail) = match profile {
        Profile::Inference => (INFERENCE_HEAD, INFERENCE_TAIL),
        Profile::Control => (CONTROL_HEAD, CONTROL_TAIL),
    };
    let mut rows: Vec<(String, String)> = headers
        .iter()
        .filter(|(name, _)| {
            let lower = name.to_ascii_lowercase();
            lower != "host" && lower != "connection" && lower != "content-length"
        })
        .map(|(name, value)| sanitize(value).map(|value| (name.clone(), value)))
        .collect::<Result<Vec<_>, String>>()?;
    rows.push(("host".into(), host.to_string()));
    rows.push(("connection".into(), "close".into()));
    if let Some(body) = body {
        rows.push(("content-length".into(), body.len().to_string()));
    }
    rows.sort_by(|a, b| rank(&a.0, head, tail).cmp(&rank(&b.0, head, tail)));

    let mut out = format!("{method} {path} HTTP/1.1\r\n").into_bytes();
    for (name, value) in rows {
        out.extend(wire_name(&name).as_bytes());
        out.extend(b": ");
        out.extend(value.as_bytes());
        out.extend(b"\r\n");
    }
    out.extend(b"\r\n");
    if let Some(body) = body {
        out.extend(body);
    }
    Ok(out)
}

fn rank(name: &str, head: &[&str], tail: &[&str]) -> u16 {
    let lower = name.to_ascii_lowercase();
    if let Some(i) = head.iter().position(|item| *item == lower) {
        return i as u16;
    }
    if let Some(i) = tail.iter().position(|item| *item == lower) {
        return 1000 + i as u16;
    }
    500
}

fn wire_name(name: &str) -> String {
    match name.to_ascii_lowercase().as_str() {
        "accept" => "Accept".into(),
        "accept-encoding" => "Accept-Encoding".into(),
        "authorization" => "Authorization".into(),
        "cache-control" => "Cache-Control".into(),
        "connection" => "Connection".into(),
        "content-length" => "Content-Length".into(),
        "content-type" => "Content-Type".into(),
        "host" => "Host".into(),
        "user-agent" => "User-Agent".into(),
        "x-claude-code-session-id" => "X-Claude-Code-Session-Id".into(),
        "x-stainless-arch" => "X-Stainless-Arch".into(),
        "x-stainless-lang" => "X-Stainless-Lang".into(),
        "x-stainless-os" => "X-Stainless-OS".into(),
        "x-stainless-package-version" => "X-Stainless-Package-Version".into(),
        "x-stainless-retry-count" => "X-Stainless-Retry-Count".into(),
        "x-stainless-runtime" => "X-Stainless-Runtime".into(),
        "x-stainless-runtime-version" => "X-Stainless-Runtime-Version".into(),
        "x-stainless-timeout" => "X-Stainless-Timeout".into(),
        other => other.to_string(),
    }
}

fn sanitize(value: &str) -> Result<String, String> {
    if value.bytes().any(|b| b == b'\r' || b == b'\n') {
        return Err("请求头里不能换行".into());
    }
    Ok(value.to_string())
}

async fn write_all(
    stream: &mut SslStream<TcpStream>,
    bytes: &[u8],
    deadline: Option<Instant>,
) -> Result<(), TransportError> {
    let write = stream.write_all(bytes);
    wait(write, deadline, "写出请求超时").await
}

async fn read_head(
    stream: &mut SslStream<TcpStream>,
    deadline: Option<Instant>,
) -> Result<(u16, Vec<(String, String)>, Vec<u8>), TransportError> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        if let Some(end) = find_sub(&buf, b"\r\n\r\n") {
            let head = buf[..end].to_vec();
            let rest = buf[end + 4..].to_vec();
            let (status, headers) = parse_head(&head).map_err(fail)?;
            return Ok((status, headers, rest));
        }
        if buf.len() > 64 * 1024 {
            return Err(fail("响应头过长"));
        }
        let n = read_some(stream, &mut tmp, deadline).await?;
        if n == 0 {
            return Err(fail("连接在响应头之前关掉了"));
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

fn parse_head(head: &[u8]) -> Result<(u16, Vec<(String, String)>), String> {
    let text = std::str::from_utf8(head).map_err(|_| "响应头不是 UTF-8".to_string())?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| format!("认不出状态行：{status_line}"))?;
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(format!("认不出响应头：{line}"));
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    Ok((status, headers))
}

async fn read_some(
    stream: &mut SslStream<TcpStream>,
    buf: &mut [u8],
    deadline: Option<Instant>,
) -> Result<usize, TransportError> {
    wait(stream.read(buf), deadline, "读取响应超时").await
}

async fn wait<F, T>(
    fut: F,
    deadline: Option<Instant>,
    timeout_msg: &str,
) -> Result<T, TransportError>
where
    F: std::future::Future<Output = io::Result<T>>,
{
    if let Some(deadline) = deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timed_out(timeout_msg));
        }
        match tokio::time::timeout(left, fut).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(err)) => Err(fail(err.to_string())),
            Err(_) => Err(timed_out(timeout_msg)),
        }
    } else {
        fut.await.map_err(|err| fail(err.to_string()))
    }
}

struct Decoder {
    raw: Vec<u8>,
    pos: usize,
    frame: Frame,
    inflate: Inflate,
    decoded: VecDeque<u8>,
    eof: bool,
    done: bool,
}

impl Decoder {
    fn new(headers: &[(String, String)]) -> Result<Self, String> {
        let chunked = header(headers, "transfer-encoding")
            .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"));
        let frame = if chunked {
            Frame::Chunk(Chunk::Size)
        } else if let Some(len) = header(headers, "content-length") {
            let len = len
                .trim()
                .parse::<u64>()
                .map_err(|_| format!("Content-Length 不是数字：{len}"))?;
            Frame::Len(len)
        } else {
            Frame::Close
        };
        Ok(Self {
            raw: Vec::new(),
            pos: 0,
            frame,
            inflate: Inflate::parse(header(headers, "content-encoding").unwrap_or(""))?,
            decoded: VecDeque::new(),
            eof: false,
            done: false,
        })
    }

    fn feed(&mut self, bytes: &[u8], eof: bool) -> Result<(), String> {
        if !bytes.is_empty() {
            self.compact();
            self.raw.extend_from_slice(bytes);
        }
        if eof {
            self.eof = true;
        }
        self.drain()
    }

    fn take(&mut self, max: usize) -> Option<Vec<u8>> {
        if self.decoded.is_empty() {
            return None;
        }
        let n = max.min(self.decoded.len());
        Some(self.decoded.drain(..n).collect())
    }

    fn drain(&mut self) -> Result<(), String> {
        loop {
            match step(&mut self.frame, &mut self.raw, &mut self.pos, self.eof)? {
                Step::Need => return Ok(()),
                Step::Data(bytes) => self.inflate.push(&bytes, &mut self.decoded)?,
                Step::End => {
                    self.inflate.finish(&mut self.decoded)?;
                    self.done = true;
                    return Ok(());
                }
            }
        }
    }

    fn compact(&mut self) {
        if self.pos > 4096 && self.pos * 2 > self.raw.len() {
            self.raw.drain(..self.pos);
            self.pos = 0;
        }
    }
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

enum Frame {
    Len(u64),
    Close,
    Chunk(Chunk),
    Done,
}

enum Chunk {
    Size,
    Data(u64),
    Crlf,
    Trailers,
}

enum Step {
    Need,
    Data(Vec<u8>),
    End,
}

fn step(frame: &mut Frame, raw: &mut Vec<u8>, pos: &mut usize, eof: bool) -> Result<Step, String> {
    let rest = &raw[*pos..];
    match frame {
        Frame::Done => Ok(Step::End),
        Frame::Len(left) => {
            if *left == 0 {
                *frame = Frame::Done;
                return Ok(Step::End);
            }
            if rest.is_empty() {
                return if eof {
                    Err("响应正文比 Content-Length 短".into())
                } else {
                    Ok(Step::Need)
                };
            }
            let take = (*left as usize).min(rest.len());
            let bytes = rest[..take].to_vec();
            *pos += take;
            *left -= take as u64;
            Ok(Step::Data(bytes))
        }
        Frame::Close => {
            if rest.is_empty() {
                if eof {
                    *frame = Frame::Done;
                    Ok(Step::End)
                } else {
                    Ok(Step::Need)
                }
            } else {
                let bytes = rest.to_vec();
                *pos += bytes.len();
                Ok(Step::Data(bytes))
            }
        }
        Frame::Chunk(_) => {
            let step = {
                let Frame::Chunk(phase) = frame else {
                    unreachable!();
                };
                chunk_step(phase, raw, pos, eof)?
            };
            if matches!(step, Step::End) {
                *frame = Frame::Done;
            }
            Ok(step)
        }
    }
}

fn chunk_step(
    phase: &mut Chunk,
    raw: &mut Vec<u8>,
    pos: &mut usize,
    eof: bool,
) -> Result<Step, String> {
    let rest = &raw[*pos..];
    match phase {
        Chunk::Size => {
            let Some(i) = find_sub(rest, b"\r\n") else {
                return if eof {
                    Err("分块长度没写完".into())
                } else {
                    Ok(Step::Need)
                };
            };
            let size = parse_chunk_size(&rest[..i])?;
            *pos += i + 2;
            if size == 0 {
                *phase = Chunk::Trailers;
            } else {
                *phase = Chunk::Data(size);
            }
            chunk_step(phase, raw, pos, eof)
        }
        Chunk::Data(left) => {
            if rest.is_empty() {
                return if eof {
                    Err("分块正文没写完".into())
                } else {
                    Ok(Step::Need)
                };
            }
            let take = (*left as usize).min(rest.len());
            let bytes = rest[..take].to_vec();
            *pos += take;
            *left -= take as u64;
            if *left == 0 {
                *phase = Chunk::Crlf;
            }
            Ok(Step::Data(bytes))
        }
        Chunk::Crlf => {
            if rest.len() < 2 {
                return if eof {
                    Err("分块结尾不完整".into())
                } else {
                    Ok(Step::Need)
                };
            }
            if &rest[..2] != b"\r\n" {
                return Err("分块结尾不是 CRLF".into());
            }
            *pos += 2;
            *phase = Chunk::Size;
            chunk_step(phase, raw, pos, eof)
        }
        Chunk::Trailers => {
            let Some(i) = find_sub(rest, b"\r\n\r\n") else {
                // 只有一个空行：`0\r\n\r\n` 在吃掉长度行之后剩下 `\r\n`。
                if rest.starts_with(b"\r\n") {
                    *pos += 2;
                    return Ok(Step::End);
                }
                return if eof {
                    Err("分块尾部没写完".into())
                } else {
                    Ok(Step::Need)
                };
            };
            *pos += i + 4;
            Ok(Step::End)
        }
    }
}

fn parse_chunk_size(line: &[u8]) -> Result<u64, String> {
    let hex = line.split(|b| *b == b';').next().unwrap_or(line);
    let text = std::str::from_utf8(hex)
        .map_err(|_| "分块长度不是文本".to_string())?
        .trim();
    let size = u64::from_str_radix(text, 16).map_err(|_| format!("分块长度无效：{text}"))?;
    if size > 32 * 1024 * 1024 {
        return Err("单个分块超过 32MB".into());
    }
    Ok(size)
}

fn find_sub(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

enum Inflate {
    Plain,
    Gzip(flate2::write::GzDecoder<Vec<u8>>),
    Zlib(flate2::write::ZlibDecoder<Vec<u8>>),
    Brotli(brotli::DecompressorWriter<Vec<u8>>),
    Zstd(zstd::stream::write::Decoder<'static, Vec<u8>>),
    Done,
}

impl Inflate {
    fn parse(encoding: &str) -> Result<Self, String> {
        let coding = encoding
            .split(',')
            .map(str::trim)
            .find(|part| !part.is_empty() && !part.eq_ignore_ascii_case("identity"))
            .unwrap_or("");
        match coding.to_ascii_lowercase().as_str() {
            "" | "identity" => Ok(Self::Plain),
            "gzip" | "x-gzip" => Ok(Self::Gzip(flate2::write::GzDecoder::new(Vec::new()))),
            "deflate" => Ok(Self::Zlib(flate2::write::ZlibDecoder::new(Vec::new()))),
            "br" => Ok(Self::Brotli(brotli::DecompressorWriter::new(
                Vec::new(),
                4096,
            ))),
            "zstd" => zstd::stream::write::Decoder::new(Vec::new())
                .map(Self::Zstd)
                .map_err(|err| format!("zstd 解压初始化失败：{err}")),
            "compress" => Err("不支持 content-encoding: compress".into()),
            other => Err(format!("不支持的 content-encoding：{other}")),
        }
    }

    fn push(&mut self, data: &[u8], out: &mut VecDeque<u8>) -> Result<(), String> {
        if data.is_empty() {
            return Ok(());
        }
        match self {
            Self::Plain => out.extend(data),
            Self::Gzip(dec) => {
                dec.write_all(data).map_err(|err| err.to_string())?;
                out.extend(dec.get_mut().drain(..));
            }
            Self::Zlib(dec) => {
                dec.write_all(data).map_err(|err| err.to_string())?;
                out.extend(dec.get_mut().drain(..));
            }
            Self::Brotli(dec) => {
                dec.write_all(data).map_err(|err| err.to_string())?;
                out.extend(dec.get_mut().drain(..));
            }
            Self::Zstd(dec) => {
                dec.write_all(data).map_err(|err| err.to_string())?;
                out.extend(dec.get_mut().drain(..));
            }
            Self::Done => {}
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut VecDeque<u8>) -> Result<(), String> {
        match std::mem::replace(self, Self::Done) {
            Self::Plain | Self::Done => Ok(()),
            Self::Gzip(dec) => {
                let buf = dec.finish().map_err(|err| err.to_string())?;
                out.extend(buf);
                Ok(())
            }
            Self::Zlib(dec) => {
                let buf = dec.finish().map_err(|err| err.to_string())?;
                out.extend(buf);
                Ok(())
            }
            Self::Brotli(mut dec) => {
                dec.flush().map_err(|err| err.to_string())?;
                match dec.into_inner() {
                    Ok(buf) | Err(buf) => out.extend(buf),
                }
                Ok(())
            }
            Self::Zstd(mut dec) => {
                dec.flush().map_err(|err| err.to_string())?;
                out.extend(dec.into_inner());
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inference_headers_keep_claude_code_casing_and_order() {
        let headers = vec![
            ("x-stainless-os".into(), "MacOS".into()),
            ("anthropic-beta".into(), "claude-code-20250219".into()),
            ("user-agent".into(), "claude-cli".into()),
            ("accept".into(), "application/json".into()),
            ("x-app".into(), "cli".into()),
            ("anthropic-version".into(), "2023-06-01".into()),
            ("authorization".into(), "Bearer t".into()),
            ("content-type".into(), "application/json".into()),
            ("accept-encoding".into(), "gzip, deflate, br, zstd".into()),
        ];
        let raw = encode_request(
            Profile::Inference,
            "POST",
            "/v1/messages",
            "api.anthropic.com",
            &headers,
            Some(b"{}"),
        )
        .unwrap();
        let text = String::from_utf8(raw).unwrap();
        let lines: Vec<&str> = text.split("\r\n").collect();
        assert_eq!(
            &lines[..12],
            &[
                "POST /v1/messages HTTP/1.1",
                "Accept: application/json",
                "Authorization: Bearer t",
                "Content-Type: application/json",
                "User-Agent: claude-cli",
                "X-Stainless-OS: MacOS",
                "anthropic-beta: claude-code-20250219",
                "anthropic-version: 2023-06-01",
                "x-app: cli",
                "Connection: close",
                "Host: api.anthropic.com",
                "Accept-Encoding: gzip, deflate, br, zstd",
            ]
        );
        assert!(text.contains("Content-Length: 2\r\n\r\n{}"));
        assert!(text.contains("X-Stainless-OS:"));
        assert!(!text.contains("x-stainless-os:"));
    }

    #[test]
    fn control_token_headers_follow_axios_order() {
        let headers = vec![
            ("user-agent".into(), "axios/1.15.2".into()),
            ("accept".into(), "application/json, text/plain, */*".into()),
            ("content-type".into(), "application/json".into()),
            ("accept-encoding".into(), "gzip, deflate, br".into()),
        ];
        let raw = encode_request(
            Profile::Control,
            "POST",
            "/v1/oauth/token",
            "platform.claude.com",
            &headers,
            Some(b"{}"),
        )
        .unwrap();
        let text = String::from_utf8(raw).unwrap();
        let lines: Vec<&str> = text.split("\r\n").take(8).collect();
        assert_eq!(
            lines,
            vec![
                "POST /v1/oauth/token HTTP/1.1",
                "Accept: application/json, text/plain, */*",
                "Content-Type: application/json",
                "User-Agent: axios/1.15.2",
                "Content-Length: 2",
                "Accept-Encoding: gzip, deflate, br",
                "Host: platform.claude.com",
                "Connection: close",
            ]
        );
    }

    #[test]
    fn chunked_gzip_body_comes_back_whole() {
        let plain = b"hello-sse";
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(plain).unwrap();
        let gzip = enc.finish().unwrap();
        let mut raw = format!("{:x}\r\n", gzip.len()).into_bytes();
        raw.extend(&gzip);
        raw.extend(b"\r\n0\r\n\r\n");
        let headers = vec![
            ("transfer-encoding".into(), "chunked".into()),
            ("content-encoding".into(), "gzip".into()),
        ];
        let mut decoder = Decoder::new(&headers).unwrap();
        decoder.feed(&raw, true).unwrap();
        let mut out = Vec::new();
        while let Some(part) = decoder.take(64) {
            out.extend(part);
        }
        assert!(decoder.done);
        assert_eq!(out, plain);
    }
}
