//! 生图：`aiserver.v1.AiService/RunGenerateImage`（一元调用，api2）。
//!
//! 和聊天完全是两条路：聊天是 `InferenceService/Stream`（服务端流），生图是 `AiService` 的
//! 一元调用；共用的只有身份头那一套（api2 的 IDE 端点强校验 checksum / client-key /
//! session-id / config-version，缺一个就 `unauthenticated`）。协议移植自
//! `gateway/src/cursor/protocol.js` 的 `generateCursorImage`——那份是线上验证过的真值，
//! 字段号由 `/tmp/vec-image.mjs` 用同一个 protobuf-es 编码器跑出字节向量对拍（见文件末尾的
//! 测试），不凭理解重写。
//!
//! 字段表（从 cursor-agent bundle 逆向）：
//!
//! ```text
//! RunGenerateImageRequest      { 1 description, 2 reference_images[], 3 model_id, 4 max_mode }
//! GenerateImageReferenceImage  { 1 data(base64), 2 mime_type }   ← img2img 参考图
//! RunGenerateImageResponse     oneof result { 1 success, 2 error }
//! RunGenerateImageSuccess      { 1 image_data(base64), 2 mime_type }
//! RunGenerateImageError        { 1 error, 2 model_restricted }
//! ```
//!
//! 三件事值得先知道，否则会以为是我们哪里漏了：
//!
//! 1. **一次一张**。协议没有张数字段，`n` 张就是串行调 `n` 次。
//! 2. **尺寸卖不了**。请求里没有 size / aspect_ratio，出图固定 1536×1024（3:2）。写进 prompt
//!    也没用：云端那份用 `scripts/probe-image-size.mjs` 实测过六种说法（16:9 / 9:16 / 1:1 /
//!    4K / 2K+16:9 / 不提），线上六次全部回 1536×1024。所以客户端传来的 `size` 只能如实忽略，
//!    出图的真实尺寸从字节里量（[`measure`]）——报客户要的那个值等于拿愿望冒充事实。
//! 3. **账号级门禁**。认证凑齐之后服务端回的是 `permission_denied`「Developer or Sand access
//!    required」：生图是 Developer / Sand 计划才有的能力，普通号（能聊天）一律没有。代码是通的，
//!    但要有带这个权限的号才出得了图。

use crate::connect;
use crate::error::{UpstreamError, UpstreamKind};
use crate::headers::{ai_headers, RequestNonce};
use crate::identity::DeviceIdentity;
use crate::inference::{failure_to_error, StreamConfig};
use base64::Engine as _;
use prost::Message as _;
use regex::Regex;
use serde_json::{json, Value};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const RUN_GENERATE_IMAGE_PATH: &str = "/aiserver.v1.AiService/RunGenerateImage";
/// 出图一律以这个 client-type 出现，**不跟聊天的额度通道设置走**。
///
/// 服务端对这个端点的门禁是「Developer or Sand access required」：只有 sand 通道（Cursor 内部
/// agent 的那条 bot 额度线）放行，cli / ide 通道直接 `permission_denied`。云端 TS 网关的
/// `generateCursorImage` 也是默认 sand。用户把聊天设成 CLI 是为了走稳定额度，那个选择和
/// 「能不能出图」没关系——若照搬过来，出图永远 403，而他看不出是自己在网关页点的哪一下害的。
pub const IMAGE_CLIENT_TYPE: &str = "sand";
/// 一张图的等待上限。出图普遍十几到几十秒，超过两分钟基本是上游卡死而不是画得慢。
pub const IMAGE_TIMEOUT: Duration = Duration::from_secs(120);
/// 一次请求最多几张。上限不是协议给的，是**时间**给的：一张一次串行，四张已经要好几分钟，
/// 再多只会让客户端先超时，拿不到任何一张。
pub const MAX_N: u32 = 4;

// ---------- protobuf ----------

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct GenerateImageReferenceImage {
    #[prost(string, tag = "1")]
    pub data: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub mime_type: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RunGenerateImageRequest {
    #[prost(string, tag = "1")]
    pub description: ::prost::alloc::string::String,
    /// img2img。本地网关这一版只做文生图，字段留着是为了 wire 形状与上游一致——
    /// 少一个字段不会报错，但下次要加参考图时得先确认它到底是几号，那次确认本可以不必。
    #[prost(message, repeated, tag = "2")]
    pub reference_images: ::prost::alloc::vec::Vec<GenerateImageReferenceImage>,
    #[prost(string, tag = "3")]
    pub model_id: ::prost::alloc::string::String,
    #[prost(bool, tag = "4")]
    pub max_mode: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RunGenerateImageSuccess {
    /// base64（不是裸字节）——上游就是这么给的，我们原样透传成 OpenAI 的 `b64_json`。
    #[prost(string, tag = "1")]
    pub image_data: ::prost::alloc::string::String,
    #[prost(string, tag = "2")]
    pub mime_type: ::prost::alloc::string::String,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RunGenerateImageError {
    #[prost(string, tag = "1")]
    pub error: ::prost::alloc::string::String,
    #[prost(bool, tag = "2")]
    pub model_restricted: bool,
}

#[derive(Clone, PartialEq, ::prost::Message)]
pub struct RunGenerateImageResponse {
    #[prost(oneof = "run_generate_image_response::Result", tags = "1, 2")]
    pub result: ::core::option::Option<run_generate_image_response::Result>,
}

/// `RunGenerateImageResponse` 的 oneof。
pub mod run_generate_image_response {
    #[derive(Clone, PartialEq, ::prost::Oneof)]
    pub enum Result {
        #[prost(message, tag = "1")]
        Success(super::RunGenerateImageSuccess),
        #[prost(message, tag = "2")]
        Error(super::RunGenerateImageError),
    }
}

// ---------- 出图 ----------

/// 一次出图。协议没有张数字段，所以这里就是**一张**；`n` 张由调用方串行发 `n` 次。
///
/// `size` / `quality` / `background` / `output_format` 是 OpenAI images API 的字段，原样带着：
/// Cursor 那条路收不下、静默忽略（server 会在响应头里说「已忽略」）；ChatGPT 的
/// `image_generation` 工具认它们。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageRequest {
    pub model: String,
    pub prompt: String,
    pub size: Option<String>,
    pub quality: Option<String>,
    pub background: Option<String>,
    pub output_format: Option<String>,
    /// 图片编辑（`/v1/images/edits`）的参考图，data URL。空 = 文生图。
    /// 只有 ChatGPT 那条路认；Cursor 这一版只做文生图，带了它会被明确拒掉而不是静默当文生图。
    pub references: Vec<String>,
    /// 编辑时的遮罩（data URL）。
    pub mask: Option<String>,
}

/// 出来的一张图。`b64` 原样来自上游，不重新编码——多一次解码再编码只会多一次出错的机会。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GeneratedImage {
    pub b64: String,
    pub mime: String,
    /// 从图片字节里**量**出来的宽高；认不出格式就是 None。
    pub size: Option<(u32, u32)>,
    /// 上游改写后的提示词（gpt-image 会回）。Cursor 不回，就没有。
    pub revised_prompt: Option<String>,
}

/// 客户端要的模型名 → 发给上游的 `model_id`。
///
/// 空 / `auto` 发空串（和 `protocol.js` 一致，让上游自己挑）。其余原样带上：这个端点服务端
/// 其实**忽略 model_id**（背后固定是 Nano Banana 2 一档），但如实把客户要的名字带过去，
/// 万一哪天它开始认，我们不需要改这里。
pub fn upstream_model_id(model: &str) -> String {
    let m = model.trim();
    if m.is_empty() || m.eq_ignore_ascii_case("auto") {
        String::new()
    } else {
        m.to_string()
    }
}

/// 出一张图。身份头与聊天同一套 [`ai_headers`]，只有 client-type 固定为 [`IMAGE_CLIENT_TYPE`]。
///
/// `cfg.force_model` 在这里**不生效**：那个开关是给「账号只跑得了某一档聊天模型」用的，
/// 把一个聊天模型名塞进生图请求没有任何意义。`cfg.client_type` 同样不生效，见 [`IMAGE_CLIENT_TYPE`]。
pub async fn generate(
    client: &reqwest::Client,
    cfg: &StreamConfig,
    access_token: &str,
    identity: &DeviceIdentity,
    request: &ImageRequest,
) -> Result<GeneratedImage, UpstreamError> {
    if !request.references.is_empty() {
        // 静默当成文生图会让用户拿到一张和参考图毫无关系的图，还以为是模型没理解。
        return Err(UpstreamError::new(
            UpstreamKind::BadRequest,
            400,
            "Cursor 这条出图链路只做文生图，不支持图片编辑；用 gpt-image-2（需要 ChatGPT 账号）",
        ));
    }
    let req = RunGenerateImageRequest {
        description: request.prompt.clone(),
        model_id: upstream_model_id(&request.model),
        max_mode: false,
        reference_images: Vec::new(),
    };
    let headers = ai_headers(
        access_token,
        identity,
        IMAGE_CLIENT_TYPE,
        RequestNonce::now(),
    );
    let url = format!(
        "{}{}",
        cfg.base_url.trim_end_matches('/'),
        RUN_GENERATE_IMAGE_PATH
    );

    let wire = req.encode_to_vec();
    let call = connect::call_unary(client, &url, &headers, &wire);
    let bytes = match tokio::time::timeout(IMAGE_TIMEOUT, call).await {
        Ok(r) => r.map_err(|f| clarify_permission(failure_to_error(f)))?,
        Err(_) => {
            return Err(UpstreamError::new(
                UpstreamKind::Timeout,
                504,
                format!("上游 {} 秒还没把图画出来", IMAGE_TIMEOUT.as_secs()),
            ))
        }
    };
    let resp = RunGenerateImageResponse::decode(&bytes[..]).map_err(|e| {
        UpstreamError::new(UpstreamKind::Upstream, 502, format!("响应解码失败：{e}"))
    })?;

    match resp.result {
        Some(run_generate_image_response::Result::Success(s)) if !s.image_data.is_empty() => {
            let size = base64::engine::general_purpose::STANDARD
                .decode(&s.image_data)
                .ok()
                .and_then(|b| measure(&b));
            let mime = if s.mime_type.is_empty() {
                "image/png".to_string()
            } else {
                s.mime_type
            };
            tracing::info!(
                mime = %mime,
                size = %size.map(|(w, h)| format!("{w}x{h}")).unwrap_or_else(|| "未知".into()),
                "出图"
            );
            Ok(GeneratedImage {
                b64: s.image_data,
                mime,
                size,
                revised_prompt: None,
            })
        }
        Some(run_generate_image_response::Result::Error(e)) => {
            Err(map_image_refusal(&e.error, e.model_restricted))
        }
        // 成功分支但没有字节，和干脆没有 result 一样：调用「成功」了却什么都没给。
        _ => Err(UpstreamError::new(
            UpstreamKind::Upstream,
            502,
            "上游既没给图也没说原因",
        )),
    }
}

/// 上游 error 分支的归类。移植自 `protocol.js` 的 `mapImageRefusal`。
///
/// Cursor 只带回一句话（多为它后端 axios 的原文）+ `model_restricted` 标志：
/// - `model_restricted`：这个号出不了这个模型 → 换号有戏，但别罚这个号的其他模型。
/// - 「Request failed with status code 400」：Cursor 拿我们的 prompt 去调 Google 被 400。
///   请求形状是固定的、对良性 prompt 一直成功，能变的只有内容——线上实测名人 / NSFW prompt
///   稳定复现。所以归 bad_request：换号必然重现，号也没错，不该罚。
/// - 只认 400，不把 4xx 一锅端：403 / 429 是 Cursor 自己的钥匙或配额出毛病，那值得换号。
pub fn map_image_refusal(error: &str, model_restricted: bool) -> UpstreamError {
    static RE_STATUS_400: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"status code 400\b").expect("正则"));
    if model_restricted {
        return UpstreamError::new(
            UpstreamKind::ModelUnsupported,
            400,
            format!("这个号出不了这个生图模型：{error}"),
        );
    }
    if RE_STATUS_400.is_match(error) {
        return UpstreamError::new(
            UpstreamKind::BadRequest,
            400,
            format!("上游拒绝了这个提示词（多半是内容策略）：{error}"),
        );
    }
    UpstreamError::new(
        UpstreamKind::Provider,
        502,
        format!("上游出图失败：{error}"),
    )
}

/// 「这个号没有生图权限」翻成人话，并**改归类**。
///
/// 服务端对没权限的号回 `permission_denied`「Developer or Sand access required」，
/// 直译过来是 [`UpstreamKind::Forbidden`]——而那一类会让 lane 把整个号标成耗尽半小时。
/// 用户只是点了一次生图，结果连聊天都换号了，这个代价完全不对：没有生图权限是
/// 「这个号 × 这个能力」的事实，它照样能聊天。所以归 [`UpstreamKind::ModelUnsupported`]，
/// lane 只冷却（号 × 生图模型）这一对、下次自动换别的号试；HTTP 状态仍然是 403，
/// 客户端要看到的是权限问题。
fn clarify_permission(err: UpstreamError) -> UpstreamError {
    static RE_IMAGE_PERMISSION: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)(developer|sand)\s+(or\s+\w+\s+)?access").expect("正则"));
    if err.kind != UpstreamKind::Forbidden && !RE_IMAGE_PERMISSION.is_match(&err.message) {
        return err;
    }
    let mut out = UpstreamError::new(
        UpstreamKind::ModelUnsupported,
        403,
        format!(
            "这个 Cursor 号没有生图权限（上游只对 Developer 或 Sand 通道放行，号还得有 Bot 额度）。换一个有 Bot 额度的号，或切到云端中转。上游原话：{}",
            err.message
        ),
    );
    out.cursor_code = err.cursor_code;
    out
}

/// `measure` 的 base64 版：上游给的就是 base64，解不开也是 None。
pub fn probe_dimensions_b64(b64: &str) -> Option<(u32, u32)> {
    base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .ok()
        .and_then(|b| measure(&b))
}

/// 从图片字节里量真实宽高。只认 PNG 和 JPEG——上游回的是 PNG，JPEG 顺手支持；
/// 认不出宁可返回 None：空值在日志里是「不知道」，猜一个是「知道，且错了」。
pub fn measure(bytes: &[u8]) -> Option<(u32, u32)> {
    let be32 = |b: &[u8]| u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    // PNG：8 字节签名 + IHDR 长度 / 类型，之后是两个大端 uint32。
    if bytes.len() >= 24 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return Some((be32(&bytes[16..20]), be32(&bytes[20..24])));
    }
    // JPEG：没有固定头，得顺着段走到 SOFn（C0–CF，除 C4 / C8 / CC）。段长含自己那两字节。
    if bytes.len() >= 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let mut i = 2usize;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            let marker = bytes[i + 1];
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                let h = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 7], bytes[i + 8]]) as u32;
                return Some((w, h));
            }
            if marker == 0xD8 || (0xD0..=0xD9).contains(&marker) {
                i += 2;
                continue;
            }
            i += 2 + u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        }
    }
    None
}

// ---------- OpenAI `POST /v1/images/generations` 的形状 ----------

/// 请求体里我们认的那几项。其余字段（`style`、`user`…）哪条上游都收不下，
/// 静默忽略即可——为收不下的参数报错，只会让客户端换个 SDK 就调不通。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRequest {
    pub model: String,
    pub prompt: String,
    /// 已经夹到 `1..=MAX_N`。
    pub n: u32,
    /// 客户要的规格。Cursor 那条链路给不了，调用方要说一句「已忽略」，
    /// 而不是让用户对着一张 3:2 的图猜自己那个 1:1 去哪了；ChatGPT 的 gpt-image 认它。
    pub size: Option<String>,
    /// gpt-image 系列认的几个：`quality`（low / medium / high / auto）、`background`
    /// （transparent / opaque / auto）、`output_format`（png / jpeg / webp）。Cursor 忽略。
    pub quality: Option<String>,
    pub background: Option<String>,
    pub output_format: Option<String>,
}

/// 解析请求体。失败返回一句给客户看的话（调用方包成 400），不抛。
pub fn parse_generation(body: &Value) -> Result<GenerationRequest, String> {
    let prompt = body
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if prompt.is_empty() {
        return Err("missing required parameter: prompt".into());
    }

    let n = match body.get("n") {
        None | Some(Value::Null) => 1,
        Some(v) => {
            let raw = v.as_f64().filter(|f| f.is_finite());
            let Some(raw) = raw else {
                return Err("n 必须是数字".into());
            };
            (raw.floor() as i64).clamp(1, i64::from(MAX_N)) as u32
        }
    };

    // `url` 得由我们把图存到某个能公开访问的地方才给得出来，本机网关没有那个地方。
    match body.get("response_format").and_then(Value::as_str) {
        None | Some("") | Some("b64_json") => {}
        Some(other) => return Err(format!("response_format 只支持 b64_json，不支持 {other}")),
    }

    let opt = |k: &str| {
        body.get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    Ok(GenerationRequest {
        model: opt("model").unwrap_or_default(),
        prompt,
        n,
        size: opt("size"),
        quality: opt("quality"),
        background: opt("background"),
        output_format: opt("output_format"),
    })
}

/// `POST /v1/images/edits`：文生图的那几项 + 参考图 + 遮罩。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRequest {
    pub base: GenerationRequest,
    /// data URL（`data:image/png;base64,…`）。至少一张。
    pub references: Vec<String>,
    pub mask: Option<String>,
}

/// 单张参考图的字节上限。OpenAI 自己的限制是 50MB；base64 进 JSON 再进上游，20MB 已经够大。
pub const MAX_REFERENCE_BYTES: usize = 20 * 1024 * 1024;

/// 一个 `image` 值：data URL 原样；`http(s)` 链接我们不去下载（本机网关不该替客户端出网拉图，
/// 也没法保证那个地址上游能访问），让客户端自己内联成 data URL。
fn reference_from_value(v: &Value) -> Result<String, String> {
    let Some(s) = v.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
        return Err("image 要是 data URL 字符串".into());
    };
    let lower = s.to_ascii_lowercase();
    if lower.starts_with("data:image/") && s.contains(";base64,") {
        if s.len() > MAX_REFERENCE_BYTES * 4 / 3 + 64 {
            return Err(format!(
                "参考图太大（上限 {} MB）",
                MAX_REFERENCE_BYTES / 1024 / 1024
            ));
        }
        return Ok(s.to_string());
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        return Err("image 只接受内联的 data URL，不支持图片链接：把图读成 base64 再发".into());
    }
    // 裸 base64：当 PNG 收下。
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'\n' | b'\r'))
        && s.len() >= 16
    {
        return Ok(format!(
            "data:image/png;base64,{}",
            s.replace(['\n', '\r'], "")
        ));
    }
    Err("image 要是 data URL（data:image/png;base64,…）".into())
}

/// JSON 形态的编辑请求：`image` 是一个 data URL 或一组；`mask` 是一个 data URL。
pub fn parse_edit_json(body: &Value) -> Result<EditRequest, String> {
    let base = parse_generation(body)?;
    let mut references = Vec::new();
    match body.get("image") {
        Some(Value::Array(items)) => {
            for it in items {
                references.push(reference_from_value(it)?);
            }
        }
        Some(v @ Value::String(_)) => references.push(reference_from_value(v)?),
        _ => {}
    }
    if references.is_empty() {
        return Err("missing required parameter: image".into());
    }
    let mask = match body.get("mask") {
        None | Some(Value::Null) => None,
        Some(v) => Some(reference_from_value(v)?),
    };
    Ok(EditRequest {
        base,
        references,
        mask,
    })
}

/// multipart 形态：OpenAI SDK 发的就是它。`image` / `image[]` 是文件（可多张），`mask` 是文件，
/// 其余都是文本字段。文件按 part 自带的 content-type 编成 data URL，没有就当 PNG。
pub async fn parse_edit_multipart(
    mut form: axum::extract::Multipart,
) -> Result<EditRequest, String> {
    use base64::Engine;
    let mut fields: serde_json::Map<String, Value> = serde_json::Map::new();
    let mut references = Vec::new();
    let mut mask = None;
    while let Some(field) = form
        .next_field()
        .await
        .map_err(|e| format!("multipart 解析失败：{e}"))?
    {
        let name = field.name().unwrap_or("").to_string();
        let is_file = field.file_name().is_some();
        let mime = field
            .content_type()
            .map(str::to_string)
            .filter(|m| m.starts_with("image/"))
            .unwrap_or_else(|| "image/png".to_string());
        let bytes = field
            .bytes()
            .await
            .map_err(|e| format!("读取 multipart 字段 {name} 失败：{e}"))?;
        match name.as_str() {
            "image" | "image[]" if is_file || !bytes.is_empty() => {
                if bytes.len() > MAX_REFERENCE_BYTES {
                    return Err(format!(
                        "参考图太大（上限 {} MB）",
                        MAX_REFERENCE_BYTES / 1024 / 1024
                    ));
                }
                references.push(format!(
                    "data:{mime};base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(&bytes)
                ));
            }
            "mask" if is_file || !bytes.is_empty() => {
                mask = Some(format!(
                    "data:{mime};base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(&bytes)
                ));
            }
            "n" => {
                let text = String::from_utf8_lossy(&bytes).trim().to_string();
                fields.insert(
                    "n".into(),
                    text.parse::<f64>().map(Value::from).unwrap_or(Value::Null),
                );
            }
            other if !other.is_empty() => {
                fields.insert(
                    other.to_string(),
                    Value::String(String::from_utf8_lossy(&bytes).trim().to_string()),
                );
            }
            _ => {}
        }
    }
    if references.is_empty() {
        return Err("missing required parameter: image".into());
    }
    let base = parse_generation(&Value::Object(fields))?;
    Ok(EditRequest {
        base,
        references,
        mask,
    })
}

/// 出图结果 → OpenAI 的响应体。
///
/// `revised_prompt` 只在上游真回了的时候带（gpt-image 会回，Cursor 不回）：没有的东西不编一个出来。
pub fn generations_body(images: &[GeneratedImage]) -> Value {
    let created = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    json!({
        "created": created,
        "data": images
            .iter()
            .map(|i| {
                let mut item = json!({ "b64_json": i.b64 });
                if let Some(rp) = &i.revised_prompt {
                    item["revised_prompt"] = Value::String(rp.clone());
                }
                item
            })
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto_tests::unhex;

    // 由 gateway/ 下的 /tmp/vec-image.mjs 用 protobuf-es（线上 protocol.js 同一个编码器）
    // 生成。钉住字节而不是只钉字段号：wire type、oneof 归属、proto3 默认值不编码，
    // 任何一处漂了这四条都会当场变红。
    const REQUEST: &str =
        "0a0b61207265642070616e64611a1667656d696e692d332e312d666c6173682d696d616765";
    const REQUEST_WITH_REF_AND_MAX: &str = "0a0b61207265642070616e646112110a04414145431209696d6167652f706e671a1667656d696e692d332e312d666c6173682d696d6167652001";
    const RESPONSE_SUCCESS: &str = "0a110a04414145431209696d6167652f706e67";
    const RESPONSE_ERROR: &str =
        "12270a2352657175657374206661696c656420776974682073746174757320636f6465203430301001";

    #[test]
    fn request_encodes_byte_for_byte_like_protocol_js() {
        let req = RunGenerateImageRequest {
            description: "a red panda".into(),
            model_id: "gemini-3.1-flash-image".into(),
            max_mode: false,
            reference_images: vec![],
        };
        assert_eq!(req.encode_to_vec(), unhex(REQUEST));

        let with_ref = RunGenerateImageRequest {
            description: "a red panda".into(),
            model_id: "gemini-3.1-flash-image".into(),
            max_mode: true,
            reference_images: vec![GenerateImageReferenceImage {
                data: "AAEC".into(),
                mime_type: "image/png".into(),
            }],
        };
        assert_eq!(with_ref.encode_to_vec(), unhex(REQUEST_WITH_REF_AND_MAX));

        // proto3 不编码默认值：空请求就是零字节，和 protobuf-es 一致。
        assert!(RunGenerateImageRequest::default()
            .encode_to_vec()
            .is_empty());
    }

    #[test]
    fn response_oneof_decodes_both_branches() {
        let ok = RunGenerateImageResponse::decode(&unhex(RESPONSE_SUCCESS)[..]).unwrap();
        let Some(run_generate_image_response::Result::Success(s)) = ok.result else {
            panic!("1 号分支是 success");
        };
        assert_eq!(s.image_data, "AAEC");
        assert_eq!(s.mime_type, "image/png");

        let bad = RunGenerateImageResponse::decode(&unhex(RESPONSE_ERROR)[..]).unwrap();
        let Some(run_generate_image_response::Result::Error(e)) = bad.result else {
            panic!("2 号分支是 error");
        };
        assert_eq!(e.error, "Request failed with status code 400");
        assert!(e.model_restricted);

        // 往返一趟，编码端也钉住。
        let round = RunGenerateImageResponse {
            result: Some(run_generate_image_response::Result::Error(
                RunGenerateImageError {
                    error: "Request failed with status code 400".into(),
                    model_restricted: true,
                },
            )),
        };
        assert_eq!(round.encode_to_vec(), unhex(RESPONSE_ERROR));
    }

    #[test]
    fn auto_and_empty_model_go_upstream_as_an_empty_id() {
        assert_eq!(upstream_model_id(""), "");
        assert_eq!(upstream_model_id("  "), "");
        assert_eq!(upstream_model_id("auto"), "");
        assert_eq!(upstream_model_id("AUTO"), "");
        assert_eq!(
            upstream_model_id(" gemini-3.1-flash-image "),
            "gemini-3.1-flash-image"
        );
    }

    #[test]
    fn refusals_split_into_change_account_content_and_provider() {
        let restricted = map_image_refusal("no access to this model", true);
        assert_eq!(restricted.kind, UpstreamKind::ModelUnsupported);
        assert_eq!(restricted.status, 400);

        let content = map_image_refusal("Request failed with status code 400", false);
        assert_eq!(content.kind, UpstreamKind::BadRequest);
        assert!(!content.kind.blames_account(), "内容的问题不该罚号");

        // 403 / 429 是 Cursor 自己的钥匙或配额出毛病，不归 bad_request。
        let provider = map_image_refusal("Request failed with status code 429", false);
        assert_eq!(provider.kind, UpstreamKind::Provider);
        assert_eq!(provider.status, 502);
    }

    #[test]
    fn the_developer_or_sand_gate_cools_one_model_not_the_whole_account() {
        let denied = UpstreamError::new(
            UpstreamKind::Forbidden,
            403,
            "Developer or Sand access required",
        );
        let mapped = clarify_permission(denied);
        assert_eq!(mapped.status, 403, "客户端看到的仍是权限问题");
        assert_eq!(
            mapped.kind,
            UpstreamKind::ModelUnsupported,
            "lane 只该冷却这一对（号 × 生图模型），不该把号整个标掉——它还能聊天"
        );
        assert!(mapped.message.contains("Developer 或 Sand"));

        // 不是权限那一类的错原样带过去。
        let other = UpstreamError::new(UpstreamKind::RateLimit, 429, "slow down");
        assert_eq!(clarify_permission(other.clone()), other);
    }

    #[test]
    fn measures_png_and_jpeg_and_admits_when_it_cannot() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1536u32.to_be_bytes());
        png.extend_from_slice(&1024u32.to_be_bytes());
        png.extend_from_slice(&[8, 6, 0, 0, 0]);
        assert_eq!(measure(&png), Some((1536, 1024)));

        // SOI + APP0(len 4) + SOF0：精度 8、高 768、宽 1024。
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x03, 0x00, 0x04, 0x00, 0x03]);
        jpeg.extend_from_slice(&[0x01, 0x11, 0x00]);
        assert_eq!(measure(&jpeg), Some((1024, 768)));

        assert_eq!(measure(b"not an image"), None);
        assert_eq!(measure(&[]), None);
    }

    #[test]
    fn parses_the_openai_body_and_clamps_n() {
        let r =
            parse_generation(&json!({ "model": " nano-banana-2 ", "prompt": " a cat " })).unwrap();
        assert_eq!(r.model, "nano-banana-2");
        assert_eq!(r.prompt, "a cat");
        assert_eq!(r.n, 1, "不带 n 就是一张");
        assert_eq!(r.size, None);

        let clamped = parse_generation(&json!({ "prompt": "x", "n": 9 })).unwrap();
        assert_eq!(clamped.n, MAX_N, "超出夹到上限，不报错");
        assert_eq!(
            parse_generation(&json!({ "prompt": "x", "n": 0 }))
                .unwrap()
                .n,
            1
        );
        assert_eq!(
            parse_generation(&json!({ "prompt": "x", "n": 2.7 }))
                .unwrap()
                .n,
            2
        );

        // size 收下但给不了，调用方要能说出「已忽略」。
        let sized = parse_generation(&json!({ "prompt": "x", "size": "1024x1024" })).unwrap();
        assert_eq!(sized.size.as_deref(), Some("1024x1024"));

        // 认不出的字段不拦：quality / style 这类静默忽略。
        assert!(parse_generation(&json!({ "prompt": "x", "quality": "hd" })).is_ok());
        assert!(parse_generation(&json!({ "prompt": "x", "response_format": "b64_json" })).is_ok());
    }

    #[test]
    fn rejects_an_empty_prompt_a_bad_n_and_url_output() {
        assert!(parse_generation(&json!({ "model": "m" }))
            .unwrap_err()
            .contains("prompt"));
        assert!(parse_generation(&json!({ "prompt": "   " })).is_err());
        assert!(parse_generation(&json!({ "prompt": "x", "n": "two" }))
            .unwrap_err()
            .contains("数字"));
        assert!(
            parse_generation(&json!({ "prompt": "x", "response_format": "url" }))
                .unwrap_err()
                .contains("b64_json")
        );
    }

    #[test]
    fn the_response_body_is_openai_shaped() {
        let body = generations_body(&[
            GeneratedImage {
                b64: "AAEC".into(),
                mime: "image/png".into(),
                size: Some((1536, 1024)),
                revised_prompt: None,
            },
            GeneratedImage {
                b64: "BBBB".into(),
                mime: "image/png".into(),
                size: None,
                revised_prompt: None,
            },
        ]);
        assert!(body["created"].as_u64().unwrap() > 1_700_000_000);
        assert_eq!(body["data"].as_array().unwrap().len(), 2);
        assert_eq!(body["data"][0]["b64_json"], "AAEC");
        assert!(
            body["data"][0]["revised_prompt"].is_null(),
            "上游不回改写后的提示词，就别编一个"
        );
        assert_eq!(generations_body(&[])["data"], json!([]));

        let with_rp = generations_body(&[GeneratedImage {
            b64: "CCCC".into(),
            mime: "image/png".into(),
            size: None,
            revised_prompt: Some("a red panda".into()),
        }]);
        assert_eq!(
            with_rp["data"][0]["revised_prompt"], "a red panda",
            "上游回了就带上"
        );
    }

    #[tokio::test]
    async fn the_cursor_path_refuses_edits_before_touching_the_network() {
        let req = ImageRequest {
            model: "nano-banana-2".into(),
            prompt: "add a hat".into(),
            references: vec!["data:image/png;base64,AAAA".into()],
            ..ImageRequest::default()
        };
        let cfg = StreamConfig {
            base_url: "http://127.0.0.1:9".into(),
            ..StreamConfig::default()
        };
        let err = generate(
            &reqwest::Client::new(),
            &cfg,
            "tok",
            &DeviceIdentity::derived("tok"),
            &req,
        )
        .await
        .unwrap_err();
        assert_eq!(err.kind, UpstreamKind::BadRequest);
        assert!(err.message.contains("不支持图片编辑"), "{}", err.message);
    }

    #[test]
    fn edit_json_takes_inline_images_only_and_a_mask() {
        let e = parse_edit_json(&json!({
            "model": "gpt-image-2", "prompt": "make it night",
            "image": ["data:image/png;base64,AAAA", "QUJDREVGR0hJSktMTU5PUA=="],
            "mask": "data:image/png;base64,MMMM",
            "size": "1024x1024", "quality": "high",
        }))
        .unwrap();
        assert_eq!(e.references.len(), 2);
        assert_eq!(e.references[0], "data:image/png;base64,AAAA");
        assert_eq!(
            e.references[1], "data:image/png;base64,QUJDREVGR0hJSktMTU5PUA==",
            "裸 base64 当 PNG"
        );
        assert_eq!(e.mask.as_deref(), Some("data:image/png;base64,MMMM"));
        assert_eq!(e.base.quality.as_deref(), Some("high"));
        assert_eq!(e.base.size.as_deref(), Some("1024x1024"));

        let single =
            parse_edit_json(&json!({ "prompt": "x", "image": "data:image/jpeg;base64,BBBB" }))
                .unwrap();
        assert_eq!(
            single.references,
            vec!["data:image/jpeg;base64,BBBB".to_string()]
        );

        assert!(parse_edit_json(&json!({ "prompt": "x" }))
            .unwrap_err()
            .contains("image"));
        assert!(
            parse_edit_json(&json!({ "prompt": "x", "image": "https://example.com/a.png" }))
                .unwrap_err()
                .contains("链接"),
            "不替客户端出网拉图"
        );
        assert!(parse_edit_json(&json!({ "prompt": "x", "image": 12 })).is_err());
        assert!(
            parse_edit_json(&json!({ "image": "data:image/png;base64,AAAA" }))
                .unwrap_err()
                .contains("prompt")
        );
    }
}
