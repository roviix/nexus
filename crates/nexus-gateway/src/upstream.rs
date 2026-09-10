//! 推理后端的抽象。
//!
//! server 只认这个 trait：给凭证和统一请求，要回增量与结局。真实现是 Cursor 的
//! `InferenceService/Stream`（聊天）与 `AiService/RunGenerateImage`（生图）；测试用一个照本
//! 宣科的假后端，这样 server 的 SSE 管道、错误路径、非流式收尾都能不联网跑通。

use crate::error::{UpstreamError, UpstreamKind};
use crate::images::{self, GeneratedImage, ImageRequest};
use crate::inference::{self, StreamConfig};
use crate::lane::{BoxFuture, Credential};
use crate::media::{VideoJob, VideoRequest, VideoStatus};
use crate::normalized::{ChatRequest, Completion, Delta};

/// 增量回调。`Send` 是因为流跑在独立任务里。
pub type DeltaSink<'a> = &'a mut (dyn FnMut(Delta) + Send);

pub trait Upstream: Send + Sync {
    fn stream<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ChatRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, UpstreamError>>;

    /// 出**一张**图（协议一次一张，`n` 张由 server 串行调 `n` 次）。
    ///
    /// 默认实现是「这个后端不出图」而不是让每个实现都写一遍：聊天是所有后端都得有的能力，
    /// 生图不是。归 `bad_request` 是因为换号也没用——同一个模型只会派到同一类后端。
    fn image<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ImageRequest,
    ) -> BoxFuture<'a, Result<GeneratedImage, UpstreamError>> {
        let _ = (credential, request);
        Box::pin(async {
            Err(UpstreamError::new(
                UpstreamKind::BadRequest,
                400,
                "这个后端不支持生图",
            ))
        })
    }

    /// 提交一个视频任务，拿回 `request_id`。默认「不支持」，同上。
    fn video_start<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a VideoRequest,
    ) -> BoxFuture<'a, Result<VideoJob, UpstreamError>> {
        let _ = (credential, request);
        Box::pin(async {
            Err(UpstreamError::new(
                UpstreamKind::BadRequest,
                400,
                "这个后端不支持生视频",
            ))
        })
    }

    /// 查一个视频任务。
    fn video_status<'a>(
        &'a self,
        credential: &'a Credential,
        request_id: &'a str,
    ) -> BoxFuture<'a, Result<VideoStatus, UpstreamError>> {
        let _ = (credential, request_id);
        Box::pin(async {
            Err(UpstreamError::new(
                UpstreamKind::BadRequest,
                400,
                "这个后端不支持生视频",
            ))
        })
    }
}

/// Cursor `aiserver.v1.InferenceService/Stream`。
pub struct CursorUpstream {
    client: reqwest::Client,
    cfg: StreamConfig,
}

impl CursorUpstream {
    pub fn new(cfg: StreamConfig) -> Self {
        Self {
            client: inference::http_client(),
            cfg,
        }
    }

    pub fn config(&self) -> &StreamConfig {
        &self.cfg
    }
}

impl Upstream for CursorUpstream {
    fn stream<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ChatRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
        Box::pin(inference::stream(
            &self.client,
            &self.cfg,
            &credential.access_token,
            &credential.identity,
            request,
            on_delta,
        ))
    }

    fn image<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ImageRequest,
    ) -> BoxFuture<'a, Result<GeneratedImage, UpstreamError>> {
        Box::pin(images::generate(
            &self.client,
            &self.cfg,
            &credential.access_token,
            &credential.identity,
            request,
        ))
    }
}
