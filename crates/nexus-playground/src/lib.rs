//! `nexus-playground` —— 游乐场：多轮对话 + 生图，会话与图片都落本地。
//!
//! 它站在中转 API 那副面孔的**使用**一侧：模型广场告诉你能调什么，接入页告诉你客户端
//! 怎么配，这里直接让你用起来——用的是和客户端完全相同的地址、钥匙与链路。
//!
//! 分层：
//! - [`model`]：领域类型（前端 `ipc/playground.ts` 的镜像）；
//! - [`store`]：三张表的读写（表结构在 `nexus-store` 迁移 v3）；
//! - [`images`]：`/v1/images/generations` 客户端 + 图片 / 视频格式嗅探；
//! - [`videos`]：`/v1/videos/*` 客户端（提交 → 轮询 → 拉成片）；
//! - [`service`]：把以上串起来——发一轮、出一批图、出一段视频、停、切回来接着看。
//!
//! 视频和图共用 `playground_images` 表与 `nexus-image://` 协议口：区别只在 MIME。
//! 对话的 SSE 解析借 `nexus_gateway::playground`，不再写第二份。
//! 号源怎么解成地址与口令不在这里（那要认识网关服务和 shop 会话），由 Tauri 层做。

pub mod images;
pub mod model;
pub mod service;
pub mod store;
pub mod videos;

pub use model::{
    Asset, Attachment, Endpoint, ImageRef, ImageRequest, Kind, Message, Role, Source, Thread,
    ThreadDetail, ThreadSummary, Usage, VideoRequest,
};
pub use service::{ActiveRun, PlaygroundService};
