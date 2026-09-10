//! 生图：对着 OpenAI 形状的 `POST /v1/images/generations` 要一批图，把字节拿回来。
//!
//! 走真 HTTP 而不是绕开中转：用户在游乐场里出的图，和他照「接入」页配置后自己代码里
//! 出的图，必须是同一条链路、同一份账单。
//!
//! 上游回图有两种形态：`b64_json`（Cursor 一类只回 base64）和 `url`（多数第三方）。
//! 两种都收，并且 **URL 立刻下载落盘**：那些地址多半一小时内过期，存地址等于什么都没存。

use crate::model::ImageRequest;
use base64::Engine;
use nexus_core::AppError;
use std::time::Duration;

/// 一张拿到手的图：字节 + 上游顺手给的改写提示词（dall-e 一类会改写）。
#[derive(Debug, Clone)]
pub struct Fetched {
    pub bytes: Vec<u8>,
    pub revised_prompt: Option<String>,
}

/// 出图普遍要几十秒，2K 规格更久；整轮上限给足，别在上游还在画的时候放弃。
const TOTAL_TIMEOUT: Duration = Duration::from_secs(240);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);
/// 单张图的体积上限。超过这个多半不是图。
const MAX_IMAGE_BYTES: usize = 40 * 1024 * 1024;

pub async fn generate(
    base_url: &str,
    api_key: &str,
    model: &str,
    req: &ImageRequest,
) -> Result<Vec<Fetched>, AppError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(TOTAL_TIMEOUT)
        .build()
        .map_err(|e| AppError::internal(format!("http 客户端初始化失败：{e}")))?;

    let mut body = serde_json::json!({
        "model": model,
        "prompt": req.prompt,
        "n": req.n.clamp(1, 4),
    });
    // 固定规格的模型不发 size：那条链路没有这个字段，发了只是多一个被丢掉的参数。
    if let Some(size) = req.size.as_deref().filter(|s| !s.is_empty()) {
        body["size"] = serde_json::Value::String(size.to_string());
    }

    let res = client
        .post(format!(
            "{}/v1/images/generations",
            base_url.trim_end_matches('/')
        ))
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                AppError::network("上游在 4 分钟内没有出图，这次放弃了。")
                    .with_hint("换个更小的规格或稍后再试。")
            } else {
                AppError::network(format!("连不上中转：{e}"))
            }
        })?;

    let status = res.status();
    let text = res
        .text()
        .await
        .map_err(|e| AppError::network(format!("读响应中断：{e}")))?;
    if !status.is_success() {
        return Err(AppError::upstream(http_error_message(
            status.as_u16(),
            &text,
        )));
    }
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| AppError::upstream("中转返回的不是 JSON，拿不到图。"))?;
    let items = v["data"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or_else(|| AppError::upstream("请求成功，但响应里没有图片。"))?;

    let mut out = Vec::with_capacity(items.len());
    for item in items {
        let revised_prompt = item["revised_prompt"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let bytes = if let Some(b64) = item["b64_json"].as_str() {
            decode_b64(b64)?
        } else if let Some(url) = item["url"].as_str() {
            download(&client, url).await?
        } else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        out.push(Fetched {
            bytes,
            revised_prompt,
        });
    }
    if out.is_empty() {
        return Err(AppError::upstream(
            "请求成功，但响应里的图片既没有 b64_json 也没有 url。",
        ));
    }
    Ok(out)
}

fn decode_b64(raw: &str) -> Result<Vec<u8>, AppError> {
    // 有的上游把 data URL 前缀也带上了。
    let payload = raw.rsplit_once(',').map(|(_, p)| p).unwrap_or(raw);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|e| AppError::upstream(format!("图片的 base64 解不开：{e}")))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(AppError::upstream("上游给的图片超过 40MB，拒收。"));
    }
    Ok(bytes)
}

async fn download(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, AppError> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(AppError::upstream("上游给的图片地址不是 http(s)。"));
    }
    let res = tokio::time::timeout(DOWNLOAD_TIMEOUT, client.get(url).send())
        .await
        .map_err(|_| AppError::network("下载图片超时。"))?
        .map_err(|e| AppError::network(format!("下载图片失败：{e}")))?;
    if !res.status().is_success() {
        return Err(AppError::upstream(format!(
            "下载图片失败：上游返回 {}。",
            res.status().as_u16()
        )));
    }
    let bytes = res
        .bytes()
        .await
        .map_err(|e| AppError::network(format!("下载图片中断：{e}")))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(AppError::upstream("上游给的图片超过 40MB，拒收。"));
    }
    Ok(bytes.to_vec())
}

/// 网关拒绝时的正文是 `{"error":{"message":…}}`。这条路两种号源都走（本地网关 / 云端中转），
/// 文案不能写死「中转」；最常见的几种换成人话并给下一步，其余把网关自己的话带出来——
/// 本地网关的 403 / 400 已经是解释过的人话（没生图权限、提示词被内容策略拒），别再包一层。
pub fn http_error_message(status: u16, body: &str) -> String {
    let upstream = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v["error"]["message"]
                .as_str()
                .or_else(|| v["error"].as_str())
                .or_else(|| v["message"].as_str())
                .map(str::to_string)
        })
        .filter(|m| !m.trim().is_empty());
    match (status, upstream) {
        (401, _) => {
            "钥匙被拒（401）。本地网关换过口令就重开这一页；云端密钥可能已停用或删除。".into()
        }
        (402, _) => "积分不足（402）。充值后再试。".into(),
        (429, _) => "上游正忙（429），稍等几秒再试。".into(),
        (403, Some(m)) => format!("没有权限（403）：{m}"),
        (403, None) => "没有权限（403）。这个号出不了图，换一个号或切到云端中转。".into(),
        (400, Some(m)) => m,
        (_, Some(m)) => format!("网关返回 {status}：{m}"),
        (_, None) => format!("网关返回 {status}。"),
    }
}

// ── 图片格式嗅探 ─────────────────────────────────────────────────────────────
// 不引 image crate：只需要认出格式和尺寸，几十行就够，而那个 crate 会让编译时间翻倍。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    pub mime: &'static str,
    pub ext: &'static str,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

/// 按魔数认格式，能读到尺寸就读。认不出的按 PNG 存——上游默认就是 PNG。
/// 视频也从这里认（`ftyp` 盒 → MP4，EBML → WebM）：它们和图共用一张表、一个协议口。
pub fn probe(bytes: &[u8]) -> Probe {
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return Probe {
            mime: "video/mp4",
            ext: "mp4",
            width: None,
            height: None,
        };
    }
    if bytes.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return Probe {
            mime: "video/webm",
            ext: "webm",
            width: None,
            height: None,
        };
    }
    if bytes.len() >= 24 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Probe {
            mime: "image/png",
            ext: "png",
            width: Some(be32(&bytes[16..20])),
            height: Some(be32(&bytes[20..24])),
        };
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        let (w, h) = jpeg_dims(bytes).map_or((None, None), |(w, h)| (Some(w), Some(h)));
        return Probe {
            mime: "image/jpeg",
            ext: "jpg",
            width: w,
            height: h,
        };
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let (w, h) = webp_dims(bytes).map_or((None, None), |(w, h)| (Some(w), Some(h)));
        return Probe {
            mime: "image/webp",
            ext: "webp",
            width: w,
            height: h,
        };
    }
    if bytes.len() >= 10 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Probe {
            mime: "image/gif",
            ext: "gif",
            width: Some(u16::from_le_bytes([bytes[6], bytes[7]]) as u32),
            height: Some(u16::from_le_bytes([bytes[8], bytes[9]]) as u32),
        };
    }
    Probe {
        mime: "image/png",
        ext: "png",
        width: None,
        height: None,
    }
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// 扫 JPEG 段，找到第一个 SOF（C0–CF，跳过 C4 / C8 / CC）读高宽。
fn jpeg_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 4 <= bytes.len() {
        if bytes[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = bytes[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        // 无长度段：SOI / EOI / RSTn / TEM。
        if marker == 0xD8 || marker == 0xD9 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            if i + 9 > bytes.len() {
                return None;
            }
            let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
            let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
            return Some((w, h));
        }
        i += 2 + len;
    }
    None
}

/// WebP 三种子格式：VP8X（扩展头，24 位宽高减一）、VP8（有损，14 位）、VP8L（无损，14 位）。
fn webp_dims(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 30 {
        return None;
    }
    match &bytes[12..16] {
        b"VP8X" => {
            let w = (bytes[24] as u32 | (bytes[25] as u32) << 8 | (bytes[26] as u32) << 16) + 1;
            let h = (bytes[27] as u32 | (bytes[28] as u32) << 8 | (bytes[29] as u32) << 16) + 1;
            Some((w, h))
        }
        b"VP8 " => {
            // 帧头 3 字节 + 起始码 9D 01 2A，然后 14 位宽、14 位高（各自低 14 位）。
            if bytes[23..26] != [0x9D, 0x01, 0x2A] {
                return None;
            }
            let w = (u16::from_le_bytes([bytes[26], bytes[27]]) & 0x3FFF) as u32;
            let h = (u16::from_le_bytes([bytes[28], bytes[29]]) & 0x3FFF) as u32;
            Some((w, h))
        }
        b"VP8L" => {
            if bytes[20] != 0x2F {
                return None;
            }
            let b = u32::from_le_bytes([bytes[21], bytes[22], bytes[23], bytes[24]]);
            let w = (b & 0x3FFF) + 1;
            let h = ((b >> 14) & 0x3FFF) + 1;
            Some((w, h))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v
    }

    #[test]
    fn png_header_yields_dimensions() {
        let p = probe(&png(1024, 1536));
        assert_eq!(p.mime, "image/png");
        assert_eq!(p.ext, "png");
        assert_eq!((p.width, p.height), (Some(1024), Some(1536)));
    }

    #[test]
    fn jpeg_sof0_yields_dimensions() {
        // SOI, APP0 (len 4), SOF0: len 11, precision 8, height 768, width 1024.
        let mut v = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        v.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x03, 0x00, 0x04, 0x00, 0x03]);
        v.extend_from_slice(&[0x01, 0x11, 0x00]);
        let p = probe(&v);
        assert_eq!(p.mime, "image/jpeg");
        assert_eq!((p.width, p.height), (Some(1024), Some(768)));
    }

    #[test]
    fn webp_vp8x_yields_dimensions() {
        let mut v = b"RIFF\0\0\0\0WEBPVP8X".to_vec();
        v.extend_from_slice(&[10, 0, 0, 0]); // chunk size
        v.extend_from_slice(&[0, 0, 0, 0]); // flags + reserved
        v.extend_from_slice(&[0xFF, 0x03, 0x00]); // width-1 = 1023
        v.extend_from_slice(&[0xFF, 0x07, 0x00]); // height-1 = 2047
        let p = probe(&v);
        assert_eq!(p.mime, "image/webp");
        assert_eq!((p.width, p.height), (Some(1024), Some(2048)));
    }

    #[test]
    fn gif_and_unknown() {
        let mut g = b"GIF89a".to_vec();
        g.extend_from_slice(&[0x40, 0x01, 0xF0, 0x00]);
        let p = probe(&g);
        assert_eq!(p.mime, "image/gif");
        assert_eq!((p.width, p.height), (Some(320), Some(240)));

        let p = probe(b"not an image at all");
        assert_eq!(p.mime, "image/png");
        assert_eq!(p.width, None);
    }

    #[test]
    fn b64_accepts_bare_and_data_url_forms() {
        let raw = base64::engine::general_purpose::STANDARD.encode(b"hello");
        assert_eq!(decode_b64(&raw).unwrap(), b"hello");
        assert_eq!(
            decode_b64(&format!("data:image/png;base64,{raw}")).unwrap(),
            b"hello"
        );
        assert!(decode_b64("!!!").is_err());
    }

    #[test]
    fn errors_are_translated_without_naming_a_source() {
        assert!(http_error_message(401, "").contains("401"));
        assert!(http_error_message(402, "").contains("积分"));
        assert!(http_error_message(429, "").contains("忙"));
        // 400 / 403 网关自己已经说了人话（内容策略、没生图权限），原样带出，别再包一层。
        assert_eq!(
            http_error_message(400, r#"{"error":{"message":"上游拒绝了这个提示词"}}"#),
            "上游拒绝了这个提示词"
        );
        assert_eq!(
            http_error_message(403, r#"{"error":{"message":"这个 Cursor 号没有生图权限"}}"#),
            "没有权限（403）：这个 Cursor 号没有生图权限"
        );
        assert!(http_error_message(403, "").contains("切到云端"));
        assert_eq!(
            http_error_message(500, r#"{"error":{"message":"boom"}}"#),
            "网关返回 500：boom"
        );
        assert_eq!(http_error_message(503, "<html>"), "网关返回 503。");
        for m in [
            http_error_message(401, ""),
            http_error_message(500, ""),
            http_error_message(403, ""),
        ] {
            assert!(
                !m.contains("中转返回"),
                "两种号源共用这条路，别写死「中转」：{m}"
            );
        }
    }
}
