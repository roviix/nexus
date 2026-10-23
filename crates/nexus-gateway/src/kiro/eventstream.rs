//! AWS event stream 帧。校验 prelude CRC 和 message CRC，坏帧按字节或整帧跳过。

pub const MAX_MESSAGE_SIZE: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub enum FrameStep {
    NeedMore,
    Ready {
        consumed: usize,
        event_type: String,
        payload: Vec<u8>,
    },
    /// 这一截对不上，调用方丢掉这么多字节再试。连续失败由上层封顶。
    Skip(usize),
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

pub fn next_frame(buf: &[u8]) -> FrameStep {
    if buf.len() < 16 {
        return FrameStep::NeedMore;
    }
    let total = u32::from_be_bytes(buf[0..4].try_into().unwrap_or([0; 4])) as usize;
    let headers_len = u32::from_be_bytes(buf[4..8].try_into().unwrap_or([0; 4])) as usize;
    if !(16..=MAX_MESSAGE_SIZE).contains(&total) {
        return FrameStep::Skip(1);
    }
    if buf.len() < total {
        return FrameStep::NeedMore;
    }
    let prelude_crc = u32::from_be_bytes(buf[8..12].try_into().unwrap_or([0; 4]));
    if crc32(&buf[..8]) != prelude_crc {
        return FrameStep::Skip(1);
    }
    let message_crc = u32::from_be_bytes(buf[total - 4..total].try_into().unwrap_or([0; 4]));
    if crc32(&buf[..total - 4]) != message_crc {
        return FrameStep::Skip(total);
    }
    let payload_start = match 12usize.checked_add(headers_len) {
        Some(n) if n <= total - 4 => n,
        _ => return FrameStep::Skip(total),
    };
    let mut event_type = String::new();
    let mut exception_type = String::new();
    let mut message_type = String::new();
    let mut i = 12;
    while i < payload_start {
        let Some(name_len) = buf.get(i).copied().map(|n| n as usize) else {
            return FrameStep::Skip(total);
        };
        let name_end = i + 1 + name_len;
        let Some(name) = buf
            .get(i + 1..name_end)
            .and_then(|b| std::str::from_utf8(b).ok())
        else {
            return FrameStep::Skip(total);
        };
        let Some(ty) = buf.get(name_end).copied() else {
            return FrameStep::Skip(total);
        };
        i = name_end + 1;
        let value_len = match ty {
            0 | 1 => 0,
            2 => 1,
            3 => 2,
            4 => 4,
            5 | 8 => 8,
            6 | 7 => {
                let Some(bytes) = buf.get(i..i + 2) else {
                    return FrameStep::Skip(total);
                };
                let l = u16::from_be_bytes(bytes.try_into().unwrap_or([0; 2])) as usize;
                i += 2;
                l
            }
            9 => 16,
            _ => return FrameStep::Skip(total),
        };
        if ty == 7 {
            if let Some(v) = buf
                .get(i..i + value_len)
                .and_then(|b| std::str::from_utf8(b).ok())
            {
                match name {
                    ":event-type" => event_type = v.to_string(),
                    ":exception-type" => exception_type = v.to_string(),
                    ":message-type" => message_type = v.to_string(),
                    _ => {}
                }
            }
        }
        i += value_len;
    }
    if message_type == "exception" || message_type == "error" {
        event_type = if exception_type.is_empty() {
            "errorEvent".into()
        } else {
            exception_type
        };
    }
    FrameStep::Ready {
        consumed: total,
        event_type,
        payload: buf[payload_start..total - 4].to_vec(),
    }
}

/// 测试用：给载荷算上真实的 prelude CRC 和 message CRC。
#[cfg(test)]
pub fn encode_frame(event_type: Option<&str>, payload: &[u8]) -> Vec<u8> {
    let mut headers = Vec::new();
    if let Some(t) = event_type {
        headers.push(b":event-type".len() as u8);
        headers.extend_from_slice(b":event-type");
        headers.push(7);
        headers.extend_from_slice(&(t.len() as u16).to_be_bytes());
        headers.extend_from_slice(t.as_bytes());
    }
    let total: u32 = 12 + headers.len() as u32 + payload.len() as u32 + 4;
    let mut buf = Vec::new();
    buf.extend_from_slice(&total.to_be_bytes());
    buf.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    buf.extend_from_slice(&crc32(&buf).to_be_bytes());
    buf.extend_from_slice(&headers);
    buf.extend_from_slice(payload);
    let msg_crc = crc32(&buf);
    buf.extend_from_slice(&msg_crc.to_be_bytes());
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_iso_hdlc() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn a_good_frame_roundtrips_and_a_bad_crc_is_skipped() {
        let buf = encode_frame(Some("assistantResponseEvent"), br#"{"content":"hi"}"#);
        match next_frame(&buf) {
            FrameStep::Ready {
                consumed,
                event_type,
                payload,
            } => {
                assert_eq!(consumed, buf.len());
                assert_eq!(event_type, "assistantResponseEvent");
                assert!(std::str::from_utf8(&payload).unwrap().contains("hi"));
            }
            other => panic!("expected a frame, got skip/need {other:?}"),
        }
        let mut bad = buf.clone();
        bad[10] ^= 0xff;
        assert!(matches!(next_frame(&bad), FrameStep::Skip(1)));
    }
}
