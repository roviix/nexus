//! SSE 分帧。
//!
//! 三条上游（Codex 的 Responses、Grok、ZCode 的 Anthropic Messages）讲的事件不一样，
//! 但「怎么从字节流里切出一帧」是同一件事，所以切帧在这里，解释留在各自的模块。
//!
//! 按 `\n\n`（或 `\r\n\r\n`）切帧，`data:` 多行按规范用 `\n` 拼。

/// 一帧 SSE（已切好）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub event: String,
    pub data: String,
}

#[derive(Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some((frame_len, sep_len)) = find_frame_end(&self.buf) {
            let frame = self.buf[..frame_len].to_vec();
            self.buf.drain(..frame_len + sep_len);
            if let Some(ev) = parse_frame(&frame) {
                out.push(ev);
            }
        }
        out
    }

    /// 流结束时把残留的半帧交出来。上游不以空行收尾时最后一帧在这里。
    pub fn finish(&mut self) -> Vec<SseEvent> {
        if self.buf.is_empty() {
            return Vec::new();
        }
        let frame = std::mem::take(&mut self.buf);
        parse_frame(&frame).into_iter().collect()
    }
}

fn find_frame_end(buf: &[u8]) -> Option<(usize, usize)> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|i| (i, 2));
    let crlf = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| (i, 4));
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
        (a, b) => a.or(b),
    }
}

fn parse_frame(frame: &[u8]) -> Option<SseEvent> {
    let text = String::from_utf8_lossy(frame);
    let mut event = String::new();
    let mut data: Vec<&str> = Vec::new();
    for line in text.lines() {
        if line.starts_with(':') {
            continue;
        }
        if let Some(v) = line.strip_prefix("event:") {
            event = v.trim().to_string();
        } else if let Some(v) = line.strip_prefix("data:") {
            data.push(v.strip_prefix(' ').unwrap_or(v));
        }
    }
    if data.is_empty() {
        return None;
    }
    Some(SseEvent {
        event,
        data: data.join("\n"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_split_across_chunks_is_reassembled() {
        let mut d = SseDecoder::default();
        assert!(d.push(b"event: x\ndata: {\"a\":").is_empty());
        let out = d.push(b"1}\n\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "x");
        assert_eq!(out[0].data, r#"{"a":1}"#);
    }

    #[test]
    fn crlf_separators_and_comments_are_handled() {
        let mut d = SseDecoder::default();
        let out = d.push(b": keep-alive\r\nevent: y\r\ndata: hi\r\n\r\n");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].event, "y");
        assert_eq!(out[0].data, "hi");
    }

    #[test]
    fn multiline_data_joins_with_newlines() {
        let mut d = SseDecoder::default();
        let out = d.push(b"data: a\ndata: b\n\n");
        assert_eq!(out[0].data, "a\nb");
    }

    #[test]
    fn a_trailing_frame_without_a_blank_line_survives_finish() {
        let mut d = SseDecoder::default();
        assert!(d.push(b"data: last").is_empty());
        let out = d.finish();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].data, "last");
        assert!(d.finish().is_empty(), "finish 只交一次");
    }

    #[test]
    fn a_frame_with_no_data_line_is_dropped() {
        let mut d = SseDecoder::default();
        assert!(d.push(b"event: ping\n\n").is_empty());
    }
}
