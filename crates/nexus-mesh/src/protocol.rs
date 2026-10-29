//! Nexus Mesh 反向隧道多路复用传输帧协议。

use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use std::io;

pub const MAGIC: [u8; 4] = *b"NXMS";
pub const PROTOCOL_VERSION: u8 = 1;

pub type StreamId = u64;

/// 工具调用传输对象 (兼容 Claude Code / Agent 子代理派发等高级能力)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallWire {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// 反向隧道帧定义。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum TunnelFrame {
    /// 握手认证帧（建立连接后发送的首帧）
    Handshake {
        node_id: String,
        token: Option<String>,
        role: String,
        client_version: String,
        #[serde(default)]
        device_name: Option<String>,
    },
    /// 握手回执
    HandshakeAck {
        success: bool,
        node_id: String,
        error: Option<String>,
    },
    /// 请求通过 Provider 网卡向指定主机和端口建立出站 TCP 连接
    Connect {
        stream_id: StreamId,
        host: String,
        port: u16,
    },
    /// 出站 TCP 建立结果反馈
    Connected {
        stream_id: StreamId,
        success: bool,
        error: Option<String>,
    },
    /// 数据承载帧 (传输原始加密的 TLS 字节流)
    Data {
        stream_id: StreamId,
        #[serde(with = "base64_payload")]
        data: Bytes,
    },
    /// RFC 5.0: 调用特定 Channel（由 Provider 本地锁死目标端点，阻断 SSRF）
    Invoke {
        stream_id: StreamId,
        channel_id: String,
        model: String,
        payload_json: String,
    },
    /// 流式文本 Delta
    Delta {
        stream_id: StreamId,
        text: String,
        is_thinking: bool,
    },
    /// 流式完成
    Finish {
        stream_id: StreamId,
        total_tokens: Option<u32>,
        finish_reason: Option<String>,
        #[serde(default)]
        tool_calls: Option<Vec<ToolCallWire>>,
        #[serde(default)]
        thinking_signature: Option<String>,
    },
    /// 推理错误（如本地抢占、权限不足）
    Error {
        stream_id: StreamId,
        code: String,
        message: String,
    },
    /// 关闭指定虚拟流
    Close {
        stream_id: StreamId,
        reason: Option<String>,
    },
    /// 心跳保活探测
    Ping { timestamp: u64 },
    /// 心跳回执
    Pong { timestamp: u64 },
}

mod base64_payload {
    use base64::engine::general_purpose::STANDARD as BASE64;
    use base64::Engine;
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(data: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let encoded = BASE64.encode(data);
        serializer.serialize_str(&encoded)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let decoded = BASE64.decode(s).map_err(serde::de::Error::custom)?;
        Ok(Bytes::from(decoded))
    }
}

impl TunnelFrame {
    /// 序列化为带有 [NXMS 魔数 (4B)][版本 (1B)][载荷长度 (4B)] 前缀的安全字节流。
    pub fn encode(&self) -> io::Result<Bytes> {
        let json_bytes =
            serde_json::to_vec(self).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let len = json_bytes.len() as u32;
        let mut buf = BytesMut::with_capacity(9 + json_bytes.len());
        buf.put_slice(&MAGIC);
        buf.put_u8(PROTOCOL_VERSION);
        buf.put_u32(len);
        buf.put_slice(&json_bytes);
        Ok(buf.freeze())
    }

    /// 从缓冲区解码一个完整帧。
    /// 内置魔数与版本检查，非协议杂音或恶意探测会在 4 字节内立即报错并熔断。
    pub fn decode(src: &mut BytesMut) -> io::Result<Option<Self>> {
        if src.len() < 9 {
            return Ok(None);
        }

        // 校验 Magic 4 字节
        if src[0..4] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("非法协议魔数: {:?}", &src[0..4]),
            ));
        }

        // 校验协议版本
        let version = src[4];
        if version != PROTOCOL_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("协议版本不匹配: 期望 {PROTOCOL_VERSION}, 收到 {version}"),
            ));
        }

        let length = u32::from_be_bytes([src[5], src[6], src[7], src[8]]) as usize;
        if length > 16 * 1024 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("帧长度 {length} 超过 16MB 安全上限"),
            ));
        }
        if src.len() < 9 + length {
            return Ok(None);
        }

        src.advance(9);
        let frame_bytes = src.split_to(length);
        let frame: TunnelFrame = serde_json::from_slice(&frame_bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        Ok(Some(frame))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_encode_decode() {
        let frame = TunnelFrame::Handshake {
            node_id: "node_test_42".to_string(),
            token: Some("secret_token".to_string()),
            role: "provider".to_string(),
            client_version: "0.9.0".to_string(),
            device_name: Some("MacBook-Test".to_string()),
        };

        let encoded = frame.encode().unwrap();
        let mut buf = BytesMut::from(encoded.as_ref());

        let decoded = TunnelFrame::decode(&mut buf)
            .unwrap()
            .expect("frame decoded");
        assert_eq!(frame, decoded);
    }

    #[test]
    fn test_invalid_magic_bytes_rejected() {
        let mut buf = BytesMut::from(&b"GET / HTTP/1.1\r\n"[..]);
        let err = TunnelFrame::decode(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn test_data_frame_base64() {
        let data = Bytes::from_static(b"HELLO TLS ENCRYPTED CIPHERTEXT");
        let frame = TunnelFrame::Data {
            stream_id: 101,
            data: data.clone(),
        };

        let encoded = frame.encode().unwrap();
        let mut buf = BytesMut::from(encoded.as_ref());

        let decoded = TunnelFrame::decode(&mut buf)
            .unwrap()
            .expect("frame decoded");
        if let TunnelFrame::Data { stream_id, data: d } = decoded {
            assert_eq!(stream_id, 101);
            assert_eq!(d, data);
        } else {
            panic!("Expected Data frame");
        }
    }
}
