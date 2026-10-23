//! 入站图片缩小后再塞进 Amazon Q。
//!
//! 单字段大约超过 400KB 的 base64 会 400（`CONTENT_LENGTH_EXCEEDS_THRESHOLD`）。
//! 长边按 Anthropic 的 1568 收，小图原样过；大 GIF 取静态首帧压成 JPEG。解码失败或压到
//! 256px 仍超限的图不会硬塞给上游，而是换成一句明确的省略说明。

use crate::normalized::ImageInput;
use base64::Engine;
use image::imageops::FilterType;
use image::{ImageFormat, ImageReader};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Cursor;

const DEFAULT_MAX_LONG_SIDE: u32 = 1568;
const DEFAULT_MAX_BYTES: usize = 400_000;
const DEFAULT_JPEG_QUALITY: u8 = 85;
const OMITTED: &str = "[image omitted: identical to an earlier screenshot]";
const TOO_LARGE: &str = "[image omitted: it could not be reduced below Kiro's size limit]";

#[derive(Clone, Copy)]
struct ResizeConfig {
    enabled: bool,
    max_long_side: u32,
    max_bytes: usize,
    jpeg_quality: u8,
}

impl ResizeConfig {
    fn from_env() -> Self {
        let enabled = !matches!(
            std::env::var("KIRO_RS_IMAGE_RESIZE")
                .unwrap_or_else(|_| "1".into())
                .to_ascii_lowercase()
                .as_str(),
            "0" | "false" | "no" | "off"
        );
        Self {
            enabled,
            max_long_side: env_u32("KIRO_RS_IMAGE_MAX_LONG_SIDE", DEFAULT_MAX_LONG_SIDE),
            max_bytes: env_usize("KIRO_RS_IMAGE_MAX_BYTES", DEFAULT_MAX_BYTES),
            jpeg_quality: env_u32("KIRO_RS_IMAGE_JPEG_QUALITY", DEFAULT_JPEG_QUALITY as u32) as u8,
        }
    }
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

pub struct Prepared {
    pub images: Vec<Value>,
    pub notes: Vec<String>,
}

/// 整段历史里同一张图只发一次。后面的换成一句占位，避免截图把 token 烧光。
pub fn prepare(images: &[ImageInput], seen: &mut HashSet<String>) -> Prepared {
    let cfg = ResizeConfig::from_env();
    let mut out = Vec::new();
    let mut notes = Vec::new();
    for image in images {
        if image.data.is_empty() {
            continue;
        }
        let digest = hex_sha256(image.data.as_bytes());
        if !seen.insert(digest) {
            notes.push(OMITTED.to_string());
            continue;
        }
        let declared = format_of(&image.mime_type);
        match maybe_shrink(cfg, &declared, &image.data) {
            Some(shrunk) => out.push(json!({
                "format": shrunk.format,
                "source": { "bytes": shrunk.data }
            })),
            None => notes.push(TOO_LARGE.to_string()),
        }
    }
    Prepared { images: out, notes }
}

struct Shrunk {
    format: String,
    data: String,
}

fn maybe_shrink(cfg: ResizeConfig, format: &str, data: &str) -> Option<Shrunk> {
    let format = format.to_ascii_lowercase();
    if !cfg.enabled {
        return Some(passthrough(format, data));
    }
    if data.len() <= cfg.max_bytes {
        if let Some((w, h)) = peek_dimensions(&format, data) {
            if w.max(h) <= cfg.max_long_side {
                return Some(passthrough(format, data));
            }
        } else {
            // 小而解不开的载荷让上游按原协议判断；只有超预算时必须本地拦住。
            return Some(passthrough(format, data));
        }
    }
    shrink_static(cfg, &format, data)
}

fn passthrough(format: String, data: &str) -> Shrunk {
    let format = detect_format(data).unwrap_or(format);
    let format = if format == "jpg" {
        "jpeg".into()
    } else {
        format
    };
    Shrunk {
        format,
        data: data.to_string(),
    }
}

fn shrink_static(cfg: ResizeConfig, format: &str, data: &str) -> Option<Shrunk> {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let cursor = Cursor::new(&raw);
    let mut reader = ImageReader::new(cursor);
    if let Some(fmt) = guess_format(format) {
        reader.set_format(fmt);
    } else {
        reader = reader.with_guessed_format().ok()?;
    }
    let img = reader.decode().ok()?;
    let (w, h) = (img.width(), img.height());
    let mut cur_long = w.max(h).min(cfg.max_long_side).max(1);
    const MIN_JPEG_QUALITY: u8 = 35;
    const MIN_LONG_SIDE: u32 = 256;
    let out = loop {
        let resized = if w.max(h) > cur_long {
            let scale = cur_long as f32 / w.max(h) as f32;
            let new_w = ((w as f32) * scale).round().max(1.0) as u32;
            let new_h = ((h as f32) * scale).round().max(1.0) as u32;
            img.resize_exact(new_w, new_h, FilterType::Lanczos3)
        } else {
            img.clone()
        };
        let rgb = resized.to_rgb8();
        let mut quality = cfg.jpeg_quality.max(MIN_JPEG_QUALITY);
        let encoded = loop {
            let mut buf = Vec::new();
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
            rgb.write_with_encoder(encoder).ok()?;
            if buf.len().saturating_mul(4) / 3 <= cfg.max_bytes || quality <= MIN_JPEG_QUALITY {
                break buf;
            }
            quality = quality.saturating_sub(10).max(MIN_JPEG_QUALITY);
        };
        let encoded_len = encoded.len().saturating_mul(4) / 3;
        if encoded_len <= cfg.max_bytes {
            break encoded;
        }
        if cur_long <= MIN_LONG_SIDE {
            return None;
        }
        cur_long = ((cur_long as f32 * 0.8) as u32).max(MIN_LONG_SIDE);
    };
    Some(Shrunk {
        format: "jpeg".into(),
        data: base64::engine::general_purpose::STANDARD.encode(out),
    })
}

fn format_of(mime: &str) -> String {
    let raw = mime
        .rsplit('/')
        .next()
        .unwrap_or("png")
        .split(';')
        .next()
        .unwrap_or("png")
        .trim()
        .to_ascii_lowercase();
    match raw.as_str() {
        "jpg" | "jpeg" | "pjpeg" => "jpeg".into(),
        "png" | "gif" | "webp" => raw,
        _ => "png".into(),
    }
}

fn detect_format(data: &str) -> Option<String> {
    let head: String = data.chars().take(32).collect();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(head.as_bytes())
        .ok()?;
    match image::guess_format(&bytes).ok()? {
        ImageFormat::Png => Some("png".into()),
        ImageFormat::Jpeg => Some("jpeg".into()),
        ImageFormat::Gif => Some("gif".into()),
        ImageFormat::WebP => Some("webp".into()),
        _ => None,
    }
}

fn peek_dimensions(format: &str, data: &str) -> Option<(u32, u32)> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    let cursor = Cursor::new(&bytes);
    let mut reader = ImageReader::new(cursor);
    if let Some(fmt) = guess_format(format) {
        reader.set_format(fmt);
    } else {
        reader = reader.with_guessed_format().ok()?;
    }
    reader.into_dimensions().ok()
}

fn guess_format(format: &str) -> Option<ImageFormat> {
    match format {
        "png" => Some(ImageFormat::Png),
        "jpeg" | "jpg" => Some(ImageFormat::Jpeg),
        "webp" => Some(ImageFormat::WebP),
        "gif" => Some(ImageFormat::Gif),
        _ => None,
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xf) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    fn png(w: u32, h: u32) -> String {
        let mut img = RgbImage::new(w, h);
        for y in 0..h {
            for x in 0..w {
                img.put_pixel(x, y, Rgb([(x % 256) as u8, (y % 256) as u8, 40]));
            }
        }
        let mut buf = Vec::new();
        img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)
            .unwrap();
        base64::engine::general_purpose::STANDARD.encode(buf)
    }

    #[test]
    fn small_png_passes_through_and_a_repeat_is_omitted() {
        let data = png(32, 32);
        let images = [
            ImageInput {
                data: data.clone(),
                mime_type: "image/png".into(),
            },
            ImageInput {
                data: data.clone(),
                mime_type: "image/png".into(),
            },
        ];
        let mut seen = HashSet::new();
        let prepared = prepare(&images, &mut seen);
        assert_eq!(prepared.images.len(), 1);
        assert_eq!(prepared.images[0]["format"], "png");
        assert_eq!(prepared.notes, vec![OMITTED]);
    }

    #[test]
    fn wide_image_is_reencoded_as_jpeg() {
        let data = png(2000, 40);
        let images = [ImageInput {
            data,
            mime_type: "image/png".into(),
        }];
        let mut seen = HashSet::new();
        let prepared = prepare(&images, &mut seen);
        assert_eq!(prepared.images[0]["format"], "jpeg");
        assert!(prepared.notes.is_empty());
    }

    #[test]
    fn oversized_gif_uses_a_static_jpeg_frame() {
        let img = RgbImage::from_pixel(2000, 40, Rgb([20, 40, 60]));
        let mut raw = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut raw), ImageFormat::Gif)
            .unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(raw);
        let shrunk = maybe_shrink(
            ResizeConfig {
                enabled: true,
                max_long_side: 1568,
                max_bytes: 400_000,
                jpeg_quality: 85,
            },
            "gif",
            &data,
        )
        .unwrap();
        assert_eq!(shrunk.format, "jpeg");
        assert!(shrunk.data.len() <= 400_000);
    }

    #[test]
    fn oversized_undecodable_data_is_not_sent_upstream() {
        assert!(maybe_shrink(
            ResizeConfig {
                enabled: true,
                max_long_side: 1568,
                max_bytes: 8,
                jpeg_quality: 85,
            },
            "png",
            "this-is-not-an-image",
        )
        .is_none());
    }
}
