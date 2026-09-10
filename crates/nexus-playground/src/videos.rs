//! 生视频：`POST /v1/videos/generations` → 拿 `request_id` → 轮询 `GET /v1/videos/{id}` →
//! 把成片字节拉回来。
//!
//! 和出图一样走真 HTTP、走网关自己的口：用户在游乐场里出的视频，和他照「接入」页配置后
//! 自己代码里出的，是同一条链路、同一份账。成片先试网关的代下载口
//! （`/v1/videos/{id}/content`，本地网关有），没有再直接拉上游给的临时 URL——那地址会过期，
//! 所以**一定落盘**，不存链接。

use crate::images::http_error_message;
use crate::model::VideoRequest;
use nexus_core::AppError;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct FetchedVideo {
    pub bytes: Vec<u8>,
    pub request_id: String,
    pub duration_secs: Option<u32>,
    pub resolution: Option<String>,
}

/// 提交请求的等待上限。
const START_TIMEOUT: Duration = Duration::from_secs(60);
/// 一段 15 秒的 1080p 上游要几分钟；官方客户端给 300 秒，这里放宽一点。
pub const GENERATE_TIMEOUT: Duration = Duration::from_secs(420);
const POLL_INTERVAL: Duration = Duration::from_secs(4);
const POLL_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);
/// 成片体积上限。15 秒 1080p 也就几十 MB。
const MAX_VIDEO_BYTES: usize = 400 * 1024 * 1024;

pub async fn generate(
    base_url: &str,
    api_key: &str,
    model: &str,
    req: &VideoRequest,
) -> Result<FetchedVideo, AppError> {
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| AppError::internal(format!("http 客户端初始化失败：{e}")))?;
    let base = base_url.trim_end_matches('/');

    let mut body = serde_json::json!({ "model": model, "prompt": req.prompt });
    if let (Some(b64), Some(mime)) = (req.image_base64.as_deref(), req.image_mime.as_deref()) {
        if !b64.is_empty() {
            body["image"] = serde_json::json!({ "url": format!("data:{mime};base64,{b64}") });
        }
    }
    if let Some(d) = req.duration {
        body["duration"] = serde_json::json!(d);
    }
    if let Some(a) = req.aspect_ratio.as_deref().filter(|s| !s.is_empty()) {
        body["aspect_ratio"] = serde_json::json!(a);
    }
    if let Some(r) = req.resolution.as_deref().filter(|s| !s.is_empty()) {
        body["resolution"] = serde_json::json!(r);
    }

    let res = client
        .post(format!("{base}/v1/videos/generations"))
        .bearer_auth(api_key)
        .timeout(START_TIMEOUT)
        .json(&body)
        .send()
        .await
        .map_err(|e| AppError::network(format!("连不上网关：{e}")))?;
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
        .map_err(|_| AppError::upstream("网关返回的不是 JSON，拿不到任务号。"))?;
    let request_id = v["request_id"]
        .as_str()
        .or_else(|| v["id"].as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| AppError::upstream("请求成功，但响应里没有 request_id。"))?
        .to_string();

    let started = Instant::now();
    let done = loop {
        if started.elapsed() > GENERATE_TIMEOUT {
            return Err(AppError::network(format!(
                "上游在 {} 分钟内没有把视频做完（任务 {request_id}），这次放弃了。",
                GENERATE_TIMEOUT.as_secs() / 60
            ))
            .with_hint("缩短时长或降低分辨率再试。"));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
        let res = client
            .get(format!("{base}/v1/videos/{request_id}"))
            .bearer_auth(api_key)
            .timeout(POLL_TIMEOUT)
            .send()
            .await
            .map_err(|e| AppError::network(format!("查任务状态失败：{e}")))?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if status.as_u16() == 202 && text.trim().is_empty() {
            continue;
        }
        if !status.is_success() {
            return Err(AppError::upstream(http_error_message(
                status.as_u16(),
                &text,
            )));
        }
        let st: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| AppError::upstream("任务状态不是 JSON。"))?;
        match st["status"].as_str().unwrap_or("pending") {
            "done" | "completed" | "succeeded" => break st,
            "failed" | "error" => {
                let why = st["error"]["message"]
                    .as_str()
                    .or_else(|| st["error"].as_str())
                    .unwrap_or("上游没说原因");
                return Err(AppError::upstream(format!("视频生成失败：{why}")));
            }
            "expired" => return Err(AppError::upstream("视频任务已过期，上游没有保留结果。")),
            _ => {}
        }
    };

    let url = done["video"]["url"]
        .as_str()
        .or_else(|| done["url"].as_str())
        .filter(|s| !s.is_empty());
    let bytes = match download(
        &client,
        &format!("{base}/v1/videos/{request_id}/content"),
        Some(api_key),
    )
    .await
    {
        Ok(b) => b,
        Err(first) => match url {
            Some(u) => download(&client, u, None).await?,
            None => return Err(first),
        },
    };
    Ok(FetchedVideo {
        bytes,
        request_id,
        duration_secs: done["video"]["duration"]
            .as_f64()
            .map(|d| d.round() as u32)
            .or(req.duration),
        resolution: done["video"]["resolution"]
            .as_str()
            .map(str::to_string)
            .or_else(|| req.resolution.clone()),
    })
}

async fn download(
    client: &reqwest::Client,
    url: &str,
    bearer: Option<&str>,
) -> Result<Vec<u8>, AppError> {
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(AppError::upstream("成片地址不是 http(s)。"));
    }
    let mut req = client.get(url).timeout(DOWNLOAD_TIMEOUT);
    if let Some(k) = bearer {
        req = req.bearer_auth(k);
    }
    let res = req
        .send()
        .await
        .map_err(|e| AppError::network(format!("下载视频失败：{e}")))?;
    if !res.status().is_success() {
        return Err(AppError::upstream(format!(
            "下载视频失败：返回 {}。",
            res.status().as_u16()
        )));
    }
    let bytes = res
        .bytes()
        .await
        .map_err(|e| AppError::network(format!("下载视频中断：{e}")))?;
    if bytes.is_empty() {
        return Err(AppError::upstream("成片是空的。"));
    }
    if bytes.len() > MAX_VIDEO_BYTES {
        return Err(AppError::upstream("成片超过 400MB，拒收。"));
    }
    Ok(bytes.to_vec())
}
