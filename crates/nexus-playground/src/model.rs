//! 游乐场的领域类型。全部 `camelCase` 序列化，前端 `ipc/playground.ts` 是它们的镜像。
//!
//! 这里没有一个字段装着凭证：会话只记「用哪把云端密钥」的 **id**，密钥本身由 Tauri 层在
//! 发请求那一刻从 shop 取，用完即弃。

use serde::{Deserialize, Serialize};

/// 会话的种类。决定主区长什么样（对话流 / 图片流 / 视频流）和能选哪些模型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Chat,
    Image,
    Video,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Chat => "chat",
            Kind::Image => "image",
            Kind::Video => "video",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "chat" => Some(Kind::Chat),
            "image" => Some(Kind::Image),
            "video" => Some(Kind::Video),
            _ => None,
        }
    }
}

/// 号源。字符串形式与前端 `Source` 一致；`commands::relay` 也用这一份。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Local,
    Cloud,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Local => "local",
            Source::Cloud => "cloud",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "local" => Some(Source::Local),
            "cloud" => Some(Source::Cloud),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Assistant => "assistant",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "assistant" => Role::Assistant,
            _ => Role::User,
        }
    }
}

/// 一个会话。`model` / `source` / `token_id` 是**下一次发送**要用的目标；
/// 每条回复自己也记着产出它时用的模型，中途换模型对比时每条都对得上号。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Thread {
    pub id: String,
    pub kind: Kind,
    /// 空串 = 还没标题（第一条消息进来时会用它的开头当标题）。
    pub title: String,
    pub source: Source,
    pub model: String,
    /// 云端密钥 id；本地号源为空。
    pub token_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 会话列表里的一行：会话本身 + 几样给列表用的摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSummary {
    #[serde(flatten)]
    pub thread: Thread,
    pub message_count: u32,
    /// 最后一条消息的开头，给列表的第二行。
    pub preview: Option<String>,
    /// 图片会话的封面：最近一张图。
    pub cover_image_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// 一张生成的图。字节在盘上，前端凭 `id` 经 `nexus-image://` 协议取。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRef {
    pub id: String,
    pub message_id: String,
    pub mime: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub bytes: u64,
    /// 请求时要的规格（`1024x1024`）；上游固定出图的模型没有。
    pub size: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub thread_id: String,
    pub seq: u32,
    pub role: Role,
    pub content: String,
    pub thinking: Option<String>,
    /// 产出这条回复时请求的模型（只有 assistant 有）。
    pub model: Option<String>,
    /// 上游实际路由到的模型（`auto` 时才和 `model` 不同）。
    pub routed: Option<String>,
    pub usage: Option<Usage>,
    /// 这一轮失败的原因。有内容也可能有它（流中途断掉、被停止）。
    pub error: Option<String>,
    pub duration_ms: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub created_at: String,
    pub images: Vec<ImageRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDetail {
    pub thread: Thread,
    pub messages: Vec<Message>,
}

/// 资产页里的一张图：图片本身 + 它是在哪个会话、由哪个模型、照哪句提示词出的。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asset {
    #[serde(flatten)]
    pub image: ImageRef,
    pub thread_id: String,
    pub thread_title: String,
    pub model: Option<String>,
    /// 出这张图的那句提示词（紧挨在前面的 user 消息）。
    pub prompt: Option<String>,
}

/// 随一句话一起发上去的附件（目前只有图片）。
///
/// 字节以 base64 进来（`data:` 前缀带不带都收），落盘之后库里只留文件名——
/// 和生成的图走同一张 `playground_images`，区别只在它挂的是 **user** 消息。
/// 文本文件不到这里：前端把它内联进提示词，对上游而言就是一段普通的话。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    pub name: String,
    pub mime: String,
    pub data_base64: String,
}

/// 发请求的目标：地址 + 口令。由 Tauri 层按号源解出来，**只活在一次调用里**。
#[derive(Clone)]
pub struct Endpoint {
    pub base_url: String,
    pub api_key: String,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 口令不进日志。
        f.debug_struct("Endpoint")
            .field("base_url", &self.base_url)
            .field("api_key", &"…")
            .finish()
    }
}

/// 一次生图请求的参数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageRequest {
    pub prompt: String,
    /// `1024x1024` 这类；缺省不发，让上游按默认规格出。
    pub size: Option<String>,
    /// 一次几张。上游多半串行出，n 越大等得越久。
    pub n: u32,
}

/// 一次生视频请求的参数。上游是异步任务（提交 → 轮询），这里等它做完再落库——对游乐场来说
/// 和出图一样是「一次点击一条回复」。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct VideoRequest {
    pub prompt: String,
    /// 图生视频的首帧：base64（不带 `data:` 前缀）+ MIME。
    pub image_base64: Option<String>,
    pub image_mime: Option<String>,
    /// 1–15 秒。
    pub duration: Option<u32>,
    /// `16:9` 这类。
    pub aspect_ratio: Option<String>,
    /// `480p` / `720p` / `1080p`。
    pub resolution: Option<String>,
}

/// 把一段文字裁成列表里放得下的标题：取第一行、去掉首尾空白、最多 `max` 个字符。
pub fn title_from(text: &str, max: usize) -> String {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let mut out: String = line.chars().take(max).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_and_sources_round_trip_as_lowercase_words() {
        assert_eq!(serde_json::to_string(&Kind::Image).unwrap(), "\"image\"");
        assert_eq!(
            serde_json::from_str::<Kind>("\"chat\"").unwrap(),
            Kind::Chat
        );
        assert_eq!(Kind::parse("image"), Some(Kind::Image));
        assert_eq!(Kind::parse("video"), Some(Kind::Video));
        assert_eq!(Kind::parse("audio"), None);
        assert_eq!(
            serde_json::from_str::<Source>("\"cloud\"").unwrap(),
            Source::Cloud
        );
        assert_eq!(Source::parse("local"), Some(Source::Local));
    }

    #[test]
    fn summary_flattens_the_thread() {
        let s = ThreadSummary {
            thread: Thread {
                id: "t1".into(),
                kind: Kind::Chat,
                title: "hi".into(),
                source: Source::Local,
                model: "auto".into(),
                token_id: None,
                created_at: "2026-09-03T00:00:00Z".into(),
                updated_at: "2026-09-03T00:00:00Z".into(),
            },
            message_count: 2,
            preview: Some("hello".into()),
            cover_image_id: None,
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["id"], "t1");
        assert_eq!(v["kind"], "chat");
        assert_eq!(v["messageCount"], 2);
        assert_eq!(v["tokenId"], serde_json::Value::Null);
    }

    #[test]
    fn titles_take_the_first_non_empty_line_and_cap_length() {
        assert_eq!(title_from("\n\n  写一首诗  \n第二行", 10), "写一首诗");
        assert_eq!(title_from("一二三四五六", 4), "一二三四…");
        assert_eq!(title_from("   ", 10), "");
    }

    #[test]
    fn attachments_come_in_camel_case() {
        let a: Attachment = serde_json::from_value(serde_json::json!({
            "name": "猫.png",
            "mime": "image/png",
            "dataBase64": "AAAA",
        }))
        .unwrap();
        assert_eq!(a.name, "猫.png");
        assert_eq!(a.data_base64, "AAAA");
    }

    #[test]
    fn endpoint_debug_hides_the_key() {
        let e = Endpoint {
            base_url: "http://127.0.0.1:8787".into(),
            api_key: "sk-secret".into(),
        };
        let s = format!("{e:?}");
        assert!(s.contains("127.0.0.1"));
        assert!(!s.contains("sk-secret"));
    }
}
