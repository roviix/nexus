//! protobuf 线格式的最小读写：只认字段边界，不认 schema。
//!
//! 给「只改一个字段、其余字节原样保留」的场景用。prost 解码再编码会把它**不认识**的字段悄悄
//! 丢掉——而我们要改写的是官方客户端按比 `proto.rs` 更新的 proto 发出的请求（3.19.7 的 sand 路径
//! 就带着 `proto.rs` 生成时还没有的字段）。整包 decode → encode 等于把那些字段抹了，上游也不会
//! 报错，只会安静地少掉一段语义。所以改写走这里：顶层按字段切开，只重编码要改的那一个，别的
//! 字段连 key 字节都不碰。
//!
//! 支持 varint / 64 位 / LEN / 32 位四种 wire type；start/end group（3 / 4）protobuf-es 不会发，
//! 碰到当作错误而不是猜。

use std::fmt;

pub const WT_VARINT: u8 = 0;
pub const WT_I64: u8 = 1;
pub const WT_LEN: u8 = 2;
pub const WT_I32: u8 = 5;

/// 一个顶层字段。`raw` 是它在原 buffer 里的完整字节（key + 长度前缀 + 值），回写时直接拼。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field<'a> {
    pub tag: u32,
    pub wire_type: u8,
    pub raw: &'a [u8],
    /// LEN：载荷本身；varint：那串 varint 字节；定长：8 / 4 字节。
    pub value: &'a [u8],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireError {
    Truncated,
    BadVarint,
    BadTag,
    UnsupportedWireType(u8),
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WireError::Truncated => write!(f, "字节在字段中间断了"),
            WireError::BadVarint => write!(f, "varint 超过 10 字节"),
            WireError::BadTag => write!(f, "字段号为 0 或超过 2^29"),
            WireError::UnsupportedWireType(w) => write!(f, "不支持的 wire type {w}"),
        }
    }
}

impl std::error::Error for WireError {}

/// 从 `pos` 读一个 varint，读完 `pos` 停在下一个字节。
pub fn read_varint(buf: &[u8], pos: &mut usize) -> Result<u64, WireError> {
    let mut out: u64 = 0;
    for i in 0..10 {
        let Some(&b) = buf.get(*pos) else {
            return Err(WireError::Truncated);
        };
        *pos += 1;
        if i == 9 && b > 1 {
            return Err(WireError::BadVarint);
        }
        out |= u64::from(b & 0x7f) << (7 * i);
        if b & 0x80 == 0 {
            return Ok(out);
        }
    }
    Err(WireError::BadVarint)
}

pub fn put_varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push((v as u8 & 0x7f) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// 写一个 LEN 字段（key + 长度 + 载荷）。字符串 / 子消息 / packed 都是这一种。
pub fn put_len_field(tag: u32, payload: &[u8], out: &mut Vec<u8>) {
    put_varint((u64::from(tag) << 3) | u64::from(WT_LEN), out);
    put_varint(payload.len() as u64, out);
    out.extend_from_slice(payload);
}

/// 把一段消息切成顶层字段，顺序与出现顺序一致（同一字段号可以出现多次——repeated 就是这样）。
pub fn fields(buf: &[u8]) -> Result<Vec<Field<'_>>, WireError> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos < buf.len() {
        let start = pos;
        let key = read_varint(buf, &mut pos)?;
        let wire_type = (key & 0x7) as u8;
        let tag = key >> 3;
        if tag == 0 || tag > u64::from(u32::MAX >> 3) {
            return Err(WireError::BadTag);
        }
        let tag = tag as u32;
        let (value_start, value_end) = match wire_type {
            WT_VARINT => {
                let vs = pos;
                read_varint(buf, &mut pos)?;
                (vs, pos)
            }
            WT_I64 => advance(buf, &mut pos, 8)?,
            WT_I32 => advance(buf, &mut pos, 4)?,
            WT_LEN => {
                let len = read_varint(buf, &mut pos)?;
                let len = usize::try_from(len).map_err(|_| WireError::Truncated)?;
                advance(buf, &mut pos, len)?
            }
            other => return Err(WireError::UnsupportedWireType(other)),
        };
        out.push(Field {
            tag,
            wire_type,
            raw: &buf[start..pos],
            value: &buf[value_start..value_end],
        });
    }
    Ok(out)
}

fn advance(buf: &[u8], pos: &mut usize, n: usize) -> Result<(usize, usize), WireError> {
    let start = *pos;
    let end = start.checked_add(n).ok_or(WireError::Truncated)?;
    if end > buf.len() {
        return Err(WireError::Truncated);
    }
    *pos = end;
    Ok((start, end))
}

/// varint 字段的数值（enum / int32 / bool 都是它）。
pub fn varint_value(field: &Field<'_>) -> Result<u64, WireError> {
    if field.wire_type != WT_VARINT {
        return Err(WireError::UnsupportedWireType(field.wire_type));
    }
    let mut pos = 0;
    read_varint(field.value, &mut pos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use prost::Message;

    #[test]
    fn varint_round_trips_boundaries() {
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64, u64::MAX] {
            let mut buf = Vec::new();
            put_varint(v, &mut buf);
            let mut pos = 0;
            assert_eq!(read_varint(&buf, &mut pos).unwrap(), v);
            assert_eq!(pos, buf.len());
        }
    }

    #[test]
    fn truncated_and_overlong_varints_are_errors() {
        let mut pos = 0;
        assert_eq!(read_varint(&[0x80], &mut pos), Err(WireError::Truncated));
        let mut pos = 0;
        assert_eq!(
            read_varint(&[0xff; 11], &mut pos),
            Err(WireError::BadVarint)
        );
    }

    /// 用 prost 编一条真消息，再用这里的扫描器切开，字段号 / 类型 / 载荷要对得上，
    /// 且 `raw` 逐段拼回去就是原字节。
    #[test]
    fn fields_split_a_prost_encoded_message_and_concatenate_back() {
        let msg = crate::proto::InferenceStreamRequest {
            messages: vec![
                crate::proto::InferenceCoreMessage {
                    role: 1,
                    content: Some(crate::proto::inference_core_message::Content::Text(
                        "hi".into(),
                    )),
                    ..Default::default()
                },
                crate::proto::InferenceCoreMessage {
                    role: 2,
                    content: Some(crate::proto::inference_core_message::Content::Text(
                        "yo".into(),
                    )),
                    ..Default::default()
                },
            ],
            model_id: Some("m".into()),
            conversation_id: Some("c-1".into()),
            ..Default::default()
        };
        let bytes = msg.encode_to_vec();
        let fs = fields(&bytes).unwrap();
        let tags: Vec<(u32, u8)> = fs.iter().map(|f| (f.tag, f.wire_type)).collect();
        assert_eq!(
            tags,
            vec![(1, WT_LEN), (1, WT_LEN), (5, WT_LEN), (8, WT_LEN)]
        );
        assert_eq!(fs[3].value, b"c-1");
        let glued: Vec<u8> = fs.iter().flat_map(|f| f.raw.iter().copied()).collect();
        assert_eq!(glued, bytes);
        // 子消息再切一层：role 是 varint。
        let inner = fields(fs[1].value).unwrap();
        assert_eq!(inner[0].tag, 1);
        assert_eq!(varint_value(&inner[0]).unwrap(), 2);
        assert_eq!(inner[1].tag, 2);
        assert_eq!(inner[1].value, b"yo");
    }

    #[test]
    fn fixed_width_fields_and_unknown_tags_are_preserved_verbatim() {
        // tag 9 I64、tag 10 I32、tag 200 varint：全是 schema 里没有的，也要能原样切出来。
        let mut buf = Vec::new();
        put_varint((9 << 3) | u64::from(WT_I64), &mut buf);
        buf.extend_from_slice(&7u64.to_le_bytes());
        put_varint((10 << 3) | u64::from(WT_I32), &mut buf);
        buf.extend_from_slice(&3u32.to_le_bytes());
        put_varint((200 << 3) | u64::from(WT_VARINT), &mut buf);
        put_varint(300, &mut buf);
        let fs = fields(&buf).unwrap();
        assert_eq!(fs.len(), 3);
        assert_eq!(fs[0].value.len(), 8);
        assert_eq!(fs[1].value.len(), 4);
        assert_eq!(varint_value(&fs[2]).unwrap(), 300);
        let glued: Vec<u8> = fs.iter().flat_map(|f| f.raw.iter().copied()).collect();
        assert_eq!(glued, buf);
    }

    #[test]
    fn truncated_len_field_and_group_wire_types_are_rejected() {
        let mut buf = Vec::new();
        put_varint((1 << 3) | u64::from(WT_LEN), &mut buf);
        put_varint(10, &mut buf);
        buf.extend_from_slice(b"short");
        assert_eq!(fields(&buf), Err(WireError::Truncated));

        let mut group = Vec::new();
        put_varint((1 << 3) | 3, &mut group);
        assert_eq!(fields(&group), Err(WireError::UnsupportedWireType(3)));

        assert_eq!(fields(&[0x00]), Err(WireError::BadTag));
    }

    #[test]
    fn put_len_field_matches_prost_for_a_string_field() {
        let msg = crate::proto::InferenceStreamRequest {
            conversation_id: Some("abc".into()),
            ..Default::default()
        };
        let mut ours = Vec::new();
        put_len_field(8, b"abc", &mut ours);
        assert_eq!(ours, msg.encode_to_vec());
    }
}
