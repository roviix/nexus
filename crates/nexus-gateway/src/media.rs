//! 异步媒体任务（生视频）的公共形状与登记簿。
//!
//! 视频和图不一样：图是一次请求等到字节；视频是「提交 → 拿 `request_id` → 轮询」。轮询必须回到
//! **创建它的那个号**——任务归属于账号，换号去查是 404。所以 `request_id → (通道, 账号)` 要落库，
//! 进程重启后客户端还在轮询的任务才查得到。登记簿只记 id、通道、账号标签、模型、状态，没有
//! 提示词、没有图片、没有凭证。
//!
//! 请求形状对齐 xAI 的 Imagine API（`docs.x.ai` → Videos）：`prompt` / `image{url}` /
//! `reference_images[]` / `video{url}` / `duration` / `aspect_ratio` / `resolution`。这也是
//! OpenAI 兼容客户端（sub2api 一类）之间事实上的公共写法；`seconds`、`image_url` 这些别名
//! 收进来归一。

use crate::error::{UpstreamError, UpstreamKind};
use nexus_store::Db;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_VIDEO_MODEL: &str = "grok-imagine-video-1.5";
/// 上游只认 1–15 秒。
pub const MAX_DURATION_SECS: u32 = 15;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoOp {
    Generate,
    Edit,
    Extend,
}

impl VideoOp {
    pub fn as_str(self) -> &'static str {
        match self {
            VideoOp::Generate => "generate",
            VideoOp::Edit => "edit",
            VideoOp::Extend => "extend",
        }
    }
}

/// 一次视频任务的输入。三种操作共用：`Generate` 看 `prompt` / `image` / `reference_images`；
/// `Edit` / `Extend` 还要 `video`。图片与视频一律是 URL（公网地址或 data URL）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoRequest {
    pub op: Option<VideoOp>,
    pub model: String,
    pub prompt: Option<String>,
    /// 图生视频的首帧。
    pub image: Option<String>,
    /// 参考图（reference-to-video）。
    pub reference_images: Vec<String>,
    /// 编辑 / 延长时的源视频。
    pub video: Option<String>,
    pub duration: Option<u32>,
    pub aspect_ratio: Option<String>,
    pub resolution: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoJob {
    pub request_id: String,
}

/// 上游的任务状态。`status` 原样带（`pending` / `done` / `failed` / `expired`）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VideoStatus {
    pub status: String,
    pub video_url: Option<String>,
    pub duration_secs: Option<u32>,
    pub resolution: Option<String>,
    pub error: Option<String>,
    /// 上游给的整个对象。透传给客户端时不丢它没见过的字段。
    pub raw: Option<Value>,
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn str_of(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `{"url": …}` / `{"image_url": …}` / `"https://…"` 三种写法都收，归一成 URL 字符串。
fn media_ref(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Object(_) => str_of(v, "url")
            .or_else(|| str_of(v, "image_url"))
            .or_else(|| str_of(v, "video_url"))
            .or_else(|| {
                v.pointer("/image_url/url")
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            }),
        _ => None,
    }
}

fn media_refs(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(items)) => items.iter().filter_map(media_ref).collect(),
        Some(other) => media_ref(other).into_iter().collect(),
        None => Vec::new(),
    }
}

pub fn parse_video_request(op: VideoOp, body: &Value) -> Result<VideoRequest, String> {
    let obj = body.as_object().ok_or("请求体要是 JSON 对象")?;
    let prompt = str_of(body, "prompt");
    let image = obj
        .get("image")
        .and_then(media_ref)
        .or_else(|| str_of(body, "image_url"));
    let mut reference_images = media_refs(obj.get("reference_images"));
    if reference_images.is_empty() {
        reference_images = media_refs(obj.get("images"));
    }
    let video = obj
        .get("video")
        .and_then(media_ref)
        .or_else(|| str_of(body, "video_url"));
    let duration = obj
        .get("duration")
        .or_else(|| obj.get("seconds"))
        .and_then(|d| match d {
            Value::Number(n) => n.as_u64().map(|x| x as u32),
            Value::String(s) => s.trim().parse::<u32>().ok(),
            _ => None,
        });
    if let Some(d) = duration {
        if d == 0 || d > MAX_DURATION_SECS {
            return Err(format!("duration 要在 1–{MAX_DURATION_SECS} 秒之间"));
        }
    }
    let aspect_ratio = str_of(body, "aspect_ratio")
        .or_else(|| str_of(body, "size").and_then(|s| size_to_aspect(&s)));
    let resolution = str_of(body, "resolution");
    match op {
        VideoOp::Generate => {
            if prompt.is_none() && image.is_none() {
                return Err("prompt 和 image 至少要有一个".into());
            }
        }
        VideoOp::Edit | VideoOp::Extend => {
            if video.is_none() {
                return Err("video 是必填的（URL 或 data URL）".into());
            }
        }
    }
    Ok(VideoRequest {
        op: Some(op),
        model: str_of(body, "model").unwrap_or_default(),
        prompt,
        image,
        reference_images,
        video,
        duration,
        aspect_ratio,
        resolution,
    })
}

/// OpenAI 的 `size`（`1024x1024`）→ xAI 的 `aspect_ratio`。不认识的比例给 `None`，让上游自选。
pub fn size_to_aspect(size: &str) -> Option<String> {
    let lower = size.trim().to_ascii_lowercase();
    let (w, h) = lower.split_once('x')?;
    let (w, h): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
    if w == 0 || h == 0 {
        return None;
    }
    let g = gcd(w, h);
    let (a, b) = (w / g, h / g);
    let known = ["1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3"];
    let candidate = format!("{a}:{b}");
    if known.contains(&candidate.as_str()) {
        return Some(candidate);
    }
    // 1792x1024 这类接近 16:9 的按最近的算。
    let ratio = w as f64 / h as f64;
    known
        .iter()
        .map(|k| {
            let (x, y) = k.split_once(':').unwrap();
            let r = x.parse::<f64>().unwrap() / y.parse::<f64>().unwrap();
            ((r - ratio).abs(), *k)
        })
        .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap())
        .filter(|(d, _)| *d < 0.08)
        .map(|(_, k)| k.to_string())
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 {
        a
    } else {
        gcd(b, a % b)
    }
}

/// 对客户端的状态响应：上游原始对象打底，再钉上我们认得的几个字段。
pub fn status_body(request_id: &str, s: &VideoStatus) -> Value {
    let mut out = s.raw.clone().unwrap_or_else(|| json!({}));
    if !out.is_object() {
        out = json!({});
    }
    let obj = out.as_object_mut().expect("刚确认过是对象");
    obj.insert("request_id".into(), json!(request_id));
    obj.insert("status".into(), json!(s.status));
    if let Some(url) = &s.video_url {
        let video = obj.entry("video").or_insert_with(|| json!({}));
        if let Some(v) = video.as_object_mut() {
            v.insert("url".into(), json!(url));
            if let Some(d) = s.duration_secs {
                v.entry("duration").or_insert(json!(d));
            }
            if let Some(r) = &s.resolution {
                v.entry("resolution").or_insert(json!(r));
            }
        }
    }
    if let Some(e) = &s.error {
        obj.entry("error").or_insert(json!({ "message": e }));
    }
    out
}

/// 把成片拉回来。签名 URL 自带鉴权，不加任何头。
pub async fn download(url: &str) -> Result<(String, Vec<u8>), UpstreamError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| UpstreamError::new(UpstreamKind::Upstream, 502, e.to_string()))?;
    let res = client.get(url).send().await.map_err(|e| {
        UpstreamError::new(UpstreamKind::Upstream, 502, format!("下载视频失败：{e}"))
    })?;
    let status = res.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(UpstreamError::new(
            UpstreamKind::Upstream,
            502,
            format!("下载视频 HTTP {status}"),
        ));
    }
    let mime = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("video/mp4")
        .to_string();
    let bytes = res.bytes().await.map_err(|e| {
        UpstreamError::new(UpstreamKind::Upstream, 502, format!("读取视频失败：{e}"))
    })?;
    Ok((mime, bytes.to_vec()))
}

// ---------- 登记簿 ----------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaJob {
    pub request_id: String,
    pub channel: String,
    pub account: String,
    pub model: String,
    pub op: String,
    pub status: String,
    pub video_url: Option<String>,
    pub duration_secs: Option<u32>,
    pub resolution: Option<String>,
    pub created_ms: i64,
    pub updated_ms: i64,
}

pub struct MediaJobs {
    db: Arc<Db>,
}

/// 任务只保留这么久：上游的成片 URL 本来就是临时的，几天后再查也是 expired。
const KEEP_MS: i64 = 7 * 86_400_000;

impl MediaJobs {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    pub fn record(&self, job: &MediaJob) {
        let r = self.db.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO gateway_media_jobs
                   (request_id, channel, account, model, op, status, video_url, duration_secs, resolution, created_ms, updated_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    job.request_id,
                    job.channel,
                    job.account.trim().to_lowercase(),
                    job.model,
                    job.op,
                    job.status,
                    job.video_url,
                    job.duration_secs.map(i64::from),
                    job.resolution,
                    job.created_ms,
                    job.updated_ms,
                ],
            )
        });
        if let Err(err) = r {
            tracing::warn!(%err, "媒体任务登记失败");
        }
    }

    pub fn update_status(&self, request_id: &str, s: &VideoStatus) {
        let r = self.db.with(|c| {
            c.execute(
                "UPDATE gateway_media_jobs
                   SET status = ?2, video_url = COALESCE(?3, video_url),
                       duration_secs = COALESCE(?4, duration_secs), resolution = COALESCE(?5, resolution),
                       updated_ms = ?6
                 WHERE request_id = ?1",
                params![
                    request_id,
                    s.status,
                    s.video_url,
                    s.duration_secs.map(i64::from),
                    s.resolution,
                    now_ms(),
                ],
            )
        });
        if let Err(err) = r {
            tracing::warn!(%err, "媒体任务状态更新失败");
        }
    }

    pub fn get(&self, request_id: &str) -> Option<MediaJob> {
        self.db
            .with(|c| {
                c.query_row(
                    "SELECT request_id, channel, account, model, op, status, video_url, duration_secs, resolution, created_ms, updated_ms
                     FROM gateway_media_jobs WHERE request_id = ?1",
                    [request_id],
                    row_to_job,
                )
                .optional()
            })
            .ok()
            .flatten()
    }

    /// 最近的任务，新的在前。给游乐场 / 状态页看。
    pub fn recent(&self, limit: usize) -> Vec<MediaJob> {
        self.db
            .with(|c| {
                let mut stmt = c.prepare(
                    "SELECT request_id, channel, account, model, op, status, video_url, duration_secs, resolution, created_ms, updated_ms
                     FROM gateway_media_jobs ORDER BY created_ms DESC LIMIT ?1",
                )?;
                let rows = stmt.query_map([limit as i64], row_to_job)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()
            })
            .unwrap_or_default()
    }

    pub fn prune(&self) {
        let cutoff = now_ms() - KEEP_MS;
        let _ = self.db.with(|c| {
            c.execute(
                "DELETE FROM gateway_media_jobs WHERE created_ms < ?1",
                [cutoff],
            )
        });
    }
}

fn row_to_job(r: &rusqlite::Row<'_>) -> rusqlite::Result<MediaJob> {
    Ok(MediaJob {
        request_id: r.get(0)?,
        channel: r.get(1)?,
        account: r.get(2)?,
        model: r.get(3)?,
        op: r.get(4)?,
        status: r.get(5)?,
        video_url: r.get(6)?,
        duration_secs: r.get::<_, Option<i64>>(7)?.map(|d| d as u32),
        resolution: r.get(8)?,
        created_ms: r.get(9)?,
        updated_ms: r.get(10)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_generate_with_aliases() {
        let r = parse_video_request(
            VideoOp::Generate,
            &json!({ "model": "grok-imagine-video", "prompt": "a cat", "seconds": "8",
                     "image_url": "data:image/png;base64,AAA", "size": "1792x1024" }),
        )
        .unwrap();
        assert_eq!(r.duration, Some(8));
        assert_eq!(r.image.as_deref(), Some("data:image/png;base64,AAA"));
        assert_eq!(r.aspect_ratio.as_deref(), Some("16:9"));
    }

    #[test]
    fn edit_needs_a_video_and_duration_is_bounded() {
        assert!(parse_video_request(VideoOp::Edit, &json!({ "prompt": "x" })).is_err());
        assert!(
            parse_video_request(VideoOp::Generate, &json!({ "prompt": "x", "duration": 40 }))
                .is_err()
        );
        let r = parse_video_request(
            VideoOp::Extend,
            &json!({ "video": { "url": "https://v/1.mp4" } }),
        )
        .unwrap();
        assert_eq!(r.video.as_deref(), Some("https://v/1.mp4"));
    }

    #[test]
    fn size_maps_to_known_aspect_ratios() {
        assert_eq!(size_to_aspect("1024x1024").as_deref(), Some("1:1"));
        assert_eq!(size_to_aspect("1536x1024").as_deref(), Some("3:2"));
        assert_eq!(size_to_aspect("1024x1536").as_deref(), Some("2:3"));
        assert_eq!(size_to_aspect("1792x1024").as_deref(), Some("16:9"));
        assert_eq!(size_to_aspect("nonsense"), None);
    }

    #[test]
    fn status_body_pins_our_fields_over_raw() {
        let s = VideoStatus {
            status: "done".into(),
            video_url: Some("https://x/v.mp4".into()),
            duration_secs: Some(8),
            resolution: Some("720p".into()),
            error: None,
            raw: Some(
                json!({ "status": "done", "video": { "url": "https://x/v.mp4", "extra": 1 } }),
            ),
        };
        let b = status_body("req_1", &s);
        assert_eq!(b["request_id"], "req_1");
        assert_eq!(b["video"]["extra"], 1);
        assert_eq!(b["video"]["duration"], 8);
    }

    #[test]
    fn jobs_round_trip_through_sqlite() {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let jobs = MediaJobs::new(db);
        jobs.record(&MediaJob {
            request_id: "r1".into(),
            channel: "grok".into(),
            account: "A@x.ai".into(),
            model: "grok-imagine-video".into(),
            op: "generate".into(),
            status: "pending".into(),
            video_url: None,
            duration_secs: Some(6),
            resolution: None,
            created_ms: 1,
            updated_ms: 1,
        });
        let got = jobs.get("r1").unwrap();
        assert_eq!(got.account, "a@x.ai");
        jobs.update_status(
            "r1",
            &VideoStatus {
                status: "done".into(),
                video_url: Some("https://v".into()),
                ..Default::default()
            },
        );
        let got = jobs.get("r1").unwrap();
        assert_eq!(got.status, "done");
        assert_eq!(got.video_url.as_deref(), Some("https://v"));
        assert_eq!(got.duration_secs, Some(6), "COALESCE 不覆盖已有值");
        assert_eq!(jobs.recent(10).len(), 1);
    }
}
