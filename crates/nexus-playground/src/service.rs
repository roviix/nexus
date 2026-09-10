//! 游乐场的业务层：会话的增删改、发一轮对话、出一批图、管进行中的请求。
//!
//! 持久化归这里而不是前端：前端每次只交「thread_id + 新的一句话」，历史从库里读、
//! 拼成 OpenAI 的 `messages[]` 发上游、流式回字经回调推出去、走完再把回复整条落库。
//! 好处是密钥从不经过 IPC、切页面不丢进度（`active()` 能把半截回复接回去），
//! 也没有「前端一份历史、库里一份历史」两处对不上的可能。
//!
//! 号源解析（本地网关的地址与口令 / 云端的地址与密钥）**不在这一层**：那要认识
//! `GatewayService` 和 shop 会话，由 Tauri 层解成 [`Endpoint`] 再交进来。

use crate::images;
use crate::model::{
    title_from, Asset, Attachment, Endpoint, ImageRef, ImageRequest, Kind, Message, Role, Source,
    Thread, ThreadDetail, ThreadSummary, Usage, VideoRequest,
};
use crate::store;
use crate::videos;
use base64::Engine;
use nexus_core::{now_iso, AppError, ErrorCode, Result};
use nexus_gateway::playground::{self, ChatMessage, Content, ImageUrl, Part, TryEvent};
use nexus_store::{Db, SqlExt};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::Notify;

/// 标题最多取第一句话的这么多字符。
const TITLE_CHARS: usize = 40;

/// 一条消息的附件解码后总共不能超过这么多。上游对请求体也有限，与其让它回一个
/// 看不懂的 413，不如在这里就说清楚。
const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;

/// 一次进行中的请求。`state` 是它到目前为止攒下的东西，`active()` 拿它给切回来的页面补画面。
struct Running {
    thread_id: String,
    kind: Kind,
    cancel: Notify,
    state: Mutex<Acc>,
}

/// 流式过程中攒下来的东西。既是最终落库的原料，也是切页面回来时的快照。
#[derive(Debug, Default, Clone)]
struct Acc {
    text: String,
    thinking: String,
    routed: Option<String>,
    finish: Option<String>,
    usage: Option<Usage>,
    error: Option<String>,
    first_byte_ms: Option<u64>,
}

impl Acc {
    fn apply(&mut self, ev: &TryEvent, started: Instant) {
        match ev {
            TryEvent::Routed { model } => self.routed = Some(model.clone()),
            TryEvent::Delta { text } => {
                self.text.push_str(text);
                self.first_byte_ms
                    .get_or_insert_with(|| started.elapsed().as_millis() as u64);
            }
            TryEvent::Thinking { text } => {
                self.thinking.push_str(text);
                self.first_byte_ms
                    .get_or_insert_with(|| started.elapsed().as_millis() as u64);
            }
            TryEvent::Done { finish, usage } => {
                self.finish = finish.clone();
                if let Some(u) = usage {
                    self.usage = Some(Usage {
                        prompt_tokens: u.prompt_tokens,
                        completion_tokens: u.completion_tokens,
                    });
                }
            }
            TryEvent::Usage { usage } => {
                self.usage = Some(Usage {
                    prompt_tokens: usage.prompt_tokens,
                    completion_tokens: usage.completion_tokens,
                });
            }
            TryEvent::Error { message } => self.error = Some(message.clone()),
        }
    }
}

/// 给前端的「这个会话正在生成」快照。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveRun {
    pub request_id: String,
    pub thread_id: String,
    pub kind: Kind,
    pub text: String,
    pub thinking: String,
    pub routed: Option<String>,
    pub started_ms_ago: u64,
}

pub struct PlaygroundService {
    db: Arc<Db>,
    images_dir: PathBuf,
    running: Mutex<HashMap<String, (Arc<Running>, Instant)>>,
}

impl std::fmt::Debug for PlaygroundService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaygroundService")
            .field("images_dir", &self.images_dir)
            .finish()
    }
}

/// 用户这一轮的输入。消息 id 在写库之前就要有——图片行得挂在它上面。
struct Turn<'a> {
    id: &'a str,
    text: &'a str,
    files: &'a [(ImageRef, String)],
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn busy() -> AppError {
    AppError::new(ErrorCode::Busy, "这个会话正在生成，等它结束或先停止。")
}

fn file_names(files: &[(ImageRef, String)]) -> Vec<String> {
    files.iter().map(|(_, f)| f.clone()).collect()
}

/// 解开前端交来的附件，顺手把总量卡住。返回原件与它的字节，配对着用。
fn decode_attachments(list: &[Attachment]) -> Result<Vec<(&Attachment, Vec<u8>)>> {
    let mut out = Vec::with_capacity(list.len());
    let mut total = 0usize;
    for a in list {
        // 前端给的可能是 `data:image/png;base64,…` 整串，也可能只有载荷。
        let payload = a
            .data_base64
            .rsplit_once(',')
            .map(|(_, p)| p)
            .unwrap_or(&a.data_base64);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(payload.trim())
            .map_err(|e| AppError::invalid(format!("附件「{}」的内容解不开：{e}", a.name)))?;
        total = total.saturating_add(bytes.len());
        if total > MAX_ATTACHMENT_BYTES {
            return Err(AppError::invalid("这条消息的附件加起来超过 20MB。")
                .with_hint("少带几张，或先把图压小一点。"));
        }
        out.push((a, bytes));
    }
    Ok(out)
}

impl PlaygroundService {
    /// `data_dir` 是应用数据目录；图片落在它下面的 `playground/images/`。
    pub fn new(db: Arc<Db>, data_dir: &Path) -> Self {
        Self {
            db,
            images_dir: data_dir.join("playground").join("images"),
            running: Mutex::new(HashMap::new()),
        }
    }

    pub fn images_dir(&self) -> &Path {
        &self.images_dir
    }

    // ── 会话 ───────────────────────────────────────────────────────────────

    pub fn threads(&self, kind: Option<Kind>) -> Result<Vec<ThreadSummary>> {
        store::list_threads(&self.db, kind)
    }

    pub fn thread(&self, id: &str) -> Result<ThreadDetail> {
        let thread = store::get_thread(&self.db, id)?;
        let messages = store::list_messages(&self.db, id)?;
        Ok(ThreadDetail { thread, messages })
    }

    pub fn create_thread(
        &self,
        kind: Kind,
        source: Source,
        model: &str,
        token_id: Option<&str>,
    ) -> Result<Thread> {
        let model = model.trim();
        if model.is_empty() {
            return Err(AppError::invalid("先选一个模型。"));
        }
        let now = now_iso();
        let t = Thread {
            id: new_id(),
            kind,
            title: String::new(),
            source,
            model: model.to_string(),
            token_id: token_id.map(str::to_string).filter(|t| !t.is_empty()),
            created_at: now.clone(),
            updated_at: now,
        };
        store::insert_thread(&self.db, &t)?;
        Ok(t)
    }

    pub fn rename_thread(&self, id: &str, title: &str) -> Result<Thread> {
        let title = title_from(title, TITLE_CHARS * 2);
        store::rename_thread(&self.db, id, &title, &now_iso())?;
        store::get_thread(&self.db, id)
    }

    pub fn set_target(
        &self,
        id: &str,
        source: Source,
        model: &str,
        token_id: Option<&str>,
    ) -> Result<Thread> {
        let model = model.trim();
        if model.is_empty() {
            return Err(AppError::invalid("先选一个模型。"));
        }
        store::set_target(
            &self.db,
            id,
            source,
            model,
            token_id.filter(|t| !t.is_empty()),
        )?;
        store::get_thread(&self.db, id)
    }

    pub fn delete_thread(&self, id: &str) -> Result<()> {
        if self.active(id).is_some() {
            return Err(busy());
        }
        let files = store::delete_thread(&self.db, id)?;
        self.remove_files(&files);
        Ok(())
    }

    pub fn delete_message(&self, id: &str) -> Result<()> {
        let files = store::delete_message(&self.db, id)?;
        self.remove_files(&files);
        Ok(())
    }

    // ── 资产 ───────────────────────────────────────────────────────────────

    /// 全部生成过的图，新的在前。
    pub fn assets(&self) -> Result<Vec<Asset>> {
        store::list_assets(&self.db)
    }

    /// 删一张图：库里的行和盘上的文件一起。已经没有了也不算错——用户可能连点了两下。
    pub fn delete_image(&self, id: &str) -> Result<()> {
        if let Some(file) = store::delete_image(&self.db, id)? {
            self.remove_files(&[file]);
        }
        Ok(())
    }

    fn remove_files(&self, files: &[String]) {
        for f in files {
            let path = self.images_dir.join(f);
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(path = %path.display(), %e, "删图片文件失败"),
            }
        }
    }

    // ── 进行中的请求 ───────────────────────────────────────────────────────

    fn register(&self, request_id: &str, thread_id: &str, kind: Kind) -> Result<Arc<Running>> {
        let mut map = self.running.lock().expect("playground running lock");
        if map.values().any(|(r, _)| r.thread_id == thread_id) {
            return Err(busy());
        }
        let run = Arc::new(Running {
            thread_id: thread_id.to_string(),
            kind,
            cancel: Notify::new(),
            state: Mutex::new(Acc::default()),
        });
        map.insert(request_id.to_string(), (run.clone(), Instant::now()));
        Ok(run)
    }

    fn unregister(&self, request_id: &str) {
        self.running
            .lock()
            .expect("playground running lock")
            .remove(request_id);
    }

    /// 停掉一次进行中的请求。返回有没有这么一次。
    pub fn stop(&self, request_id: &str) -> bool {
        let map = self.running.lock().expect("playground running lock");
        match map.get(request_id) {
            Some((run, _)) => {
                run.cancel.notify_one();
                true
            }
            None => false,
        }
    }

    /// 这个会话此刻有没有请求在跑。有就把攒到现在的半截回复一起给。
    pub fn active(&self, thread_id: &str) -> Option<ActiveRun> {
        let map = self.running.lock().expect("playground running lock");
        map.iter()
            .find(|(_, (r, _))| r.thread_id == thread_id)
            .map(|(id, (r, at))| {
                let s = r.state.lock().expect("playground acc lock");
                ActiveRun {
                    request_id: id.clone(),
                    thread_id: r.thread_id.clone(),
                    kind: r.kind,
                    text: s.text.clone(),
                    thinking: s.thinking.clone(),
                    routed: s.routed.clone(),
                    started_ms_ago: at.elapsed().as_millis() as u64,
                }
            })
    }

    // ── 对话 ───────────────────────────────────────────────────────────────

    /// 发一轮。`prompt` 为 `Some` 是新问一句；为 `None` 是**重新生成**——末尾那条回复
    /// （若有）被删掉，用剩下的历史再问一遍。两种都以「回复整条落库」结束，包括失败：
    /// 失败也是一条带 `error` 的回复，用户回来能看见「上次是哪一句没成」。
    ///
    /// `attachments` 是随这一句发上去的图。它们落盘、连 user 消息一起写库，之后
    /// 每一轮都会作为 `image_url` 内联进历史——和用户看到的气泡是同一批图。
    ///
    /// `on_event` 按时间顺序收到流里的每一帧；连网关都没打通时，会补一帧 `Error`，
    /// 被停止时补一帧 `Done{finish:"cancelled"}`——任何监听者都能等到一个收尾帧。
    pub async fn chat(
        &self,
        request_id: &str,
        thread_id: &str,
        prompt: Option<&str>,
        attachments: &[Attachment],
        endpoint: &Endpoint,
        mut on_event: impl FnMut(TryEvent),
    ) -> Result<Message> {
        let thread = store::get_thread(&self.db, thread_id)?;
        if thread.kind != Kind::Chat {
            return Err(AppError::invalid("这是图片会话，发不了对话。"));
        }
        // 解码在占位之前：附件不合法这一轮根本没开始，不必先注册再收摊。
        let decoded = decode_attachments(attachments)?;
        if prompt.is_none() && !decoded.is_empty() {
            return Err(
                AppError::invalid("重新生成用的是已有的历史，不能再加附件。")
                    .with_hint("要带新的图，就重新问一句。"),
            );
        }
        // 先占位再写库：两次并发的「发送」不能都写进同一个会话。
        let run = self.register(request_id, thread_id, Kind::Chat)?;

        // 图片先落盘、再连消息一个事务写库；库没写成就把文件收回，不留孤儿。
        let user_id = new_id();
        let files = match self.save_attachments(&user_id, &decoded) {
            Ok(files) => files,
            Err(e) => {
                self.unregister(request_id);
                return Err(e);
            }
        };
        let turn = prompt.map(|text| Turn {
            id: &user_id,
            text,
            files: &files,
        });
        if let Err(e) = self.prepare_turn(thread_id, turn) {
            self.remove_files(&file_names(&files));
            self.unregister(request_id);
            return Err(e);
        }
        let history: Vec<ChatMessage> = match self.history(thread_id) {
            Ok(h) => h,
            Err(e) => {
                self.unregister(request_id);
                return Err(e);
            }
        };

        let started = Instant::now();
        let outcome = {
            let fut = playground::run_messages(
                &endpoint.base_url,
                &endpoint.api_key,
                &thread.model,
                &history,
                |ev| {
                    run.state
                        .lock()
                        .expect("playground acc lock")
                        .apply(&ev, started);
                    on_event(ev);
                },
            );
            tokio::select! {
                r = fut => Some(r),
                _ = run.cancel.notified() => None,
            }
        };
        self.unregister(request_id);

        let acc = run.state.lock().expect("playground acc lock").clone();
        let error = match outcome {
            Some(Ok(())) => acc.error.clone(),
            Some(Err(e)) => {
                let msg = e.to_string();
                on_event(TryEvent::Error {
                    message: msg.clone(),
                });
                Some(msg)
            }
            None => {
                on_event(TryEvent::Done {
                    finish: Some("cancelled".into()),
                    usage: None,
                });
                Some("已停止。".into())
            }
        };
        let error = error.or_else(|| {
            // 流正常走完却一个字都没有：如实说，别让人对着一个空气泡猜。
            (acc.text.is_empty() && acc.thinking.is_empty()).then(|| "模型没有返回内容。".into())
        });

        let message = Message {
            id: new_id(),
            thread_id: thread_id.to_string(),
            seq: 0,
            role: Role::Assistant,
            content: acc.text,
            thinking: Some(acc.thinking).filter(|t| !t.is_empty()),
            model: Some(thread.model.clone()),
            routed: acc.routed,
            usage: acc.usage,
            error,
            duration_ms: Some(started.elapsed().as_millis() as u64),
            ttft_ms: acc.first_byte_ms,
            created_at: now_iso(),
            images: Vec::new(),
        };
        self.persist_reply(message, &[])
    }

    /// 从库里拼这一轮要发上去的 `messages[]`。
    ///
    /// 带图的 user 消息摊成 OpenAI 的 content parts，图片按 data URL 内联——上游取不到
    /// 我们本机的文件。读不出来的图（手动删过、库来自别的机器）跳过：剩下的话还是要问出去。
    fn history(&self, thread_id: &str) -> Result<Vec<ChatMessage>> {
        let list = store::list_messages(&self.db, thread_id)?;
        Ok(list
            .into_iter()
            // 失败的空回复不发上游：有的上游拒收空 content，而且它也不是对话的一部分。
            .filter(|m| !(m.role == Role::Assistant && m.content.is_empty()))
            .map(|m| self.as_chat_message(m))
            .collect())
    }

    fn as_chat_message(&self, m: Message) -> ChatMessage {
        if m.role != Role::User || m.images.is_empty() {
            return ChatMessage {
                role: m.role.as_str().to_string(),
                content: Content::Text(m.content),
            };
        }
        let mut parts: Vec<Part> = Vec::with_capacity(m.images.len() + 1);
        if !m.content.is_empty() {
            parts.push(Part::Text {
                text: m.content.clone(),
            });
        }
        for img in &m.images {
            match self.data_url(img) {
                Some(url) => parts.push(Part::ImageUrl {
                    image_url: ImageUrl { url },
                }),
                None => tracing::warn!(id = %img.id, "附件图片读不出来，这一轮不带它"),
            }
        }
        if parts.iter().all(|p| matches!(p, Part::Text { .. })) {
            // 一张都没读出来：退回纯文本，别发一个只剩文字块的数组给不认 parts 的上游。
            return ChatMessage {
                role: m.role.as_str().to_string(),
                content: Content::Text(m.content),
            };
        }
        ChatMessage::user_parts(parts)
    }

    fn data_url(&self, img: &ImageRef) -> Option<String> {
        let (path, mime) = self.image_path(&img.id).ok()??;
        let bytes = std::fs::read(path).ok()?;
        Some(format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        ))
    }

    /// 附件落盘，返回可以连消息一起写库的图片行。中途写失败就把已经写下的收回。
    fn save_attachments(
        &self,
        message_id: &str,
        decoded: &[(&Attachment, Vec<u8>)],
    ) -> Result<Vec<(ImageRef, String)>> {
        if decoded.is_empty() {
            return Ok(Vec::new());
        }
        std::fs::create_dir_all(&self.images_dir)
            .map_err(|e| AppError::internal(format!("建不了图片目录：{e}")))?;
        let now = now_iso();
        let mut files: Vec<(ImageRef, String)> = Vec::with_capacity(decoded.len());
        for (a, bytes) in decoded {
            // 认格式看字节而不是前端报的 MIME：文件名和 type 都是可以随便填的。
            let probe = images::probe(bytes);
            let id = new_id();
            let file = format!("{id}.{}", probe.ext);
            if let Err(e) = std::fs::write(self.images_dir.join(&file), bytes) {
                self.remove_files(&file_names(&files));
                return Err(AppError::internal(format!(
                    "附件「{}」写不进磁盘：{e}",
                    a.name
                )));
            }
            files.push((
                ImageRef {
                    id,
                    message_id: message_id.to_string(),
                    mime: probe.mime.to_string(),
                    width: probe.width,
                    height: probe.height,
                    bytes: bytes.len() as u64,
                    // size 是「问上游要的规格」，附件没有这回事。
                    size: None,
                    created_at: now.clone(),
                },
                file,
            ));
        }
        Ok(files)
    }

    /// 写下用户这一句（连它带的图），或为重新生成清掉尾部回复；
    /// 两种都保证历史以一条 user 消息收尾。
    fn prepare_turn(&self, thread_id: &str, turn: Option<Turn<'_>>) -> Result<()> {
        match turn {
            Some(t) => {
                let text = t.text.trim();
                if text.is_empty() {
                    return Err(AppError::invalid("说点什么再发。"));
                }
                let now = now_iso();
                let title = title_from(text, TITLE_CHARS);
                self.db.tx(|tx| {
                    let seq = store::next_seq_in(tx, thread_id).sql()?;
                    let m = Message {
                        id: t.id.to_string(),
                        thread_id: thread_id.to_string(),
                        seq,
                        role: Role::User,
                        content: text.to_string(),
                        thinking: None,
                        model: None,
                        routed: None,
                        usage: None,
                        error: None,
                        duration_ms: None,
                        ttft_ms: None,
                        created_at: now.clone(),
                        images: Vec::new(),
                    };
                    store::insert_message_in(tx, &m).sql()?;
                    for (img, file) in t.files {
                        store::insert_image_in(tx, img, thread_id, file).sql()?;
                    }
                    store::touch_thread_in(tx, thread_id, Some(&title), &now).sql()?;
                    Ok(())
                })
            }
            None => {
                let last = self.db.with(|c| store::last_message_in(c, thread_id))?;
                let Some(last) = last else {
                    return Err(AppError::invalid("会话里还没有内容。"));
                };
                if last.role == Role::Assistant {
                    self.delete_message(&last.id)?;
                }
                let tail = self.db.with(|c| store::last_message_in(c, thread_id))?;
                match tail {
                    Some(m) if m.role == Role::User => Ok(()),
                    _ => Err(AppError::invalid("没有可以重新生成的问题。")),
                }
            }
        }
    }

    /// 回复落库：分配 seq、连它的图一起写、把会话顶到列表最上面。返回带 seq 的消息。
    fn persist_reply(&self, mut message: Message, files: &[(ImageRef, String)]) -> Result<Message> {
        let thread_id = message.thread_id.clone();
        let now = message.created_at.clone();
        self.db.tx(|tx| {
            message.seq = store::next_seq_in(tx, &thread_id).sql()?;
            store::insert_message_in(tx, &message).sql()?;
            for (img, file) in files {
                store::insert_image_in(tx, img, &thread_id, file).sql()?;
            }
            store::touch_thread_in(tx, &thread_id, None, &now).sql()?;
            Ok(())
        })?;
        message.images = files.iter().map(|(img, _)| img.clone()).collect();
        Ok(message)
    }

    // ── 图片 ───────────────────────────────────────────────────────────────

    /// 出一批图。提示词记成 user 消息，图挂在紧随其后的 assistant 消息上；失败也是一条
    /// 带 `error` 的回复。图片字节先落盘、再连消息一个事务写库；库没写成就把文件收回。
    pub async fn generate_image(
        &self,
        request_id: &str,
        thread_id: &str,
        req: ImageRequest,
        endpoint: &Endpoint,
    ) -> Result<Message> {
        let thread = store::get_thread(&self.db, thread_id)?;
        if thread.kind != Kind::Image {
            return Err(AppError::invalid("这是对话会话，出不了图。"));
        }
        let req = ImageRequest {
            prompt: req.prompt.trim().to_string(),
            size: req.size.filter(|s| !s.trim().is_empty()),
            n: req.n.clamp(1, 4),
        };
        let run = self.register(request_id, thread_id, Kind::Image)?;
        let user_id = new_id();
        let turn = Turn {
            id: &user_id,
            text: &req.prompt,
            files: &[],
        };
        if let Err(e) = self.prepare_turn(thread_id, Some(turn)) {
            self.unregister(request_id);
            return Err(e);
        }

        let started = Instant::now();
        let outcome = tokio::select! {
            r = images::generate(&endpoint.base_url, &endpoint.api_key, &thread.model, &req) => Some(r),
            _ = run.cancel.notified() => None,
        };
        self.unregister(request_id);
        let duration_ms = Some(started.elapsed().as_millis() as u64);

        let mut message = Message {
            id: new_id(),
            thread_id: thread_id.to_string(),
            seq: 0,
            role: Role::Assistant,
            content: String::new(),
            thinking: None,
            model: Some(thread.model.clone()),
            routed: None,
            usage: None,
            error: None,
            duration_ms,
            ttft_ms: None,
            created_at: now_iso(),
            images: Vec::new(),
        };

        let fetched = match outcome {
            Some(Ok(list)) => list,
            Some(Err(e)) => {
                message.error = Some(e.to_string());
                return self.persist_reply(message, &[]);
            }
            None => {
                message.error = Some("已停止。".into());
                return self.persist_reply(message, &[]);
            }
        };

        if let Err(e) = std::fs::create_dir_all(&self.images_dir) {
            message.error = Some(format!("建不了图片目录：{e}"));
            return self.persist_reply(message, &[]);
        }
        let mut files: Vec<(ImageRef, String)> = Vec::with_capacity(fetched.len());
        let mut revised: Vec<String> = Vec::new();
        for f in fetched {
            let probe = images::probe(&f.bytes);
            let id = new_id();
            let file = format!("{id}.{}", probe.ext);
            if let Err(e) = std::fs::write(self.images_dir.join(&file), &f.bytes) {
                self.remove_files(&file_names(&files));
                message.error = Some(format!("图片写不进磁盘：{e}"));
                return self.persist_reply(message, &[]);
            }
            files.push((
                ImageRef {
                    id,
                    message_id: message.id.clone(),
                    mime: probe.mime.to_string(),
                    width: probe.width,
                    height: probe.height,
                    bytes: f.bytes.len() as u64,
                    size: req.size.clone(),
                    created_at: message.created_at.clone(),
                },
                file,
            ));
            if let Some(r) = f.revised_prompt {
                if !revised.contains(&r) {
                    revised.push(r);
                }
            }
        }
        message.content = revised.join("\n\n");

        match self.persist_reply(message, &files) {
            Ok(m) => Ok(m),
            Err(e) => {
                self.remove_files(&file_names(&files));
                Err(e)
            }
        }
    }

    // ── 视频 ───────────────────────────────────────────────────────────────

    /// 出一段视频。和出图同一套落库形状：提示词是 user 消息，成片挂在 assistant 消息上
    /// （`ImageRef` 的 MIME 是 `video/mp4`，`size` 记「8s · 720p」）。上游是异步任务，
    /// 这里等它做完；用户点「停」就不再轮询，任务在上游那边会自己过期。
    pub async fn generate_video(
        &self,
        request_id: &str,
        thread_id: &str,
        req: VideoRequest,
        endpoint: &Endpoint,
    ) -> Result<Message> {
        let thread = store::get_thread(&self.db, thread_id)?;
        if thread.kind != Kind::Video {
            return Err(AppError::invalid("这不是视频会话。"));
        }
        let req = VideoRequest {
            prompt: req.prompt.trim().to_string(),
            image_base64: req.image_base64.filter(|s| !s.trim().is_empty()),
            image_mime: req.image_mime.filter(|s| !s.trim().is_empty()),
            duration: req.duration.map(|d| d.clamp(1, 15)),
            aspect_ratio: req.aspect_ratio.filter(|s| !s.trim().is_empty()),
            resolution: req.resolution.filter(|s| !s.trim().is_empty()),
        };
        if req.prompt.is_empty() && req.image_base64.is_none() {
            return Err(AppError::invalid("说点什么，或给一张首帧图。"));
        }
        let run = self.register(request_id, thread_id, Kind::Video)?;
        let user_id = new_id();
        let turn = Turn {
            id: &user_id,
            text: &req.prompt,
            files: &[],
        };
        if let Err(e) = self.prepare_turn(thread_id, Some(turn)) {
            self.unregister(request_id);
            return Err(e);
        }

        let started = Instant::now();
        let outcome = tokio::select! {
            r = videos::generate(&endpoint.base_url, &endpoint.api_key, &thread.model, &req) => Some(r),
            _ = run.cancel.notified() => None,
        };
        self.unregister(request_id);
        let duration_ms = Some(started.elapsed().as_millis() as u64);

        let mut message = Message {
            id: new_id(),
            thread_id: thread_id.to_string(),
            seq: 0,
            role: Role::Assistant,
            content: String::new(),
            thinking: None,
            model: Some(thread.model.clone()),
            routed: None,
            usage: None,
            error: None,
            duration_ms,
            ttft_ms: None,
            created_at: now_iso(),
            images: Vec::new(),
        };

        let fetched = match outcome {
            Some(Ok(v)) => v,
            Some(Err(e)) => {
                message.error = Some(e.to_string());
                return self.persist_reply(message, &[]);
            }
            None => {
                message.error = Some("已停止。".into());
                return self.persist_reply(message, &[]);
            }
        };

        if let Err(e) = std::fs::create_dir_all(&self.images_dir) {
            message.error = Some(format!("建不了媒体目录：{e}"));
            return self.persist_reply(message, &[]);
        }
        let probe = images::probe(&fetched.bytes);
        let (mime, ext) = if probe.mime.starts_with("video/") {
            (probe.mime, probe.ext)
        } else {
            ("video/mp4", "mp4")
        };
        let id = new_id();
        let file = format!("{id}.{ext}");
        if let Err(e) = std::fs::write(self.images_dir.join(&file), &fetched.bytes) {
            message.error = Some(format!("视频写不进磁盘：{e}"));
            return self.persist_reply(message, &[]);
        }
        let spec = match (fetched.duration_secs, fetched.resolution.as_deref()) {
            (Some(d), Some(r)) => Some(format!("{d}s · {r}")),
            (Some(d), None) => Some(format!("{d}s")),
            (None, Some(r)) => Some(r.to_string()),
            (None, None) => None,
        };
        let files = vec![(
            ImageRef {
                id,
                message_id: message.id.clone(),
                mime: mime.to_string(),
                width: None,
                height: None,
                bytes: fetched.bytes.len() as u64,
                size: spec,
                created_at: message.created_at.clone(),
            },
            file,
        )];
        message.content = format!("任务 {}", fetched.request_id);
        match self.persist_reply(message, &files) {
            Ok(m) => Ok(m),
            Err(e) => {
                self.remove_files(&file_names(&files));
                Err(e)
            }
        }
    }

    /// 图片在盘上的位置与 MIME：`nexus-image://` 协议按 id 取图用。
    pub fn image_path(&self, id: &str) -> Result<Option<(PathBuf, String)>> {
        let Some((file, mime)) = store::image_file(&self.db, id)? else {
            return Ok(None);
        };
        // 文件名是我们自己生成的 `{uuid}.{ext}`；再挡一道，库被人改过也出不了目录。
        if file.contains('/') || file.contains('\\') || file.contains("..") {
            return Ok(None);
        }
        Ok(Some((self.images_dir.join(file), mime)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service() -> (PlaygroundService, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = Arc::new(Db::open_in_memory().unwrap());
        (PlaygroundService::new(db, dir.path()), dir)
    }

    /// 「用户说了一句」：`prepare_turn` 最常见的形态——新 id、不带附件。
    fn say(s: &PlaygroundService, thread_id: &str, text: &str) -> Result<()> {
        let id = new_id();
        s.prepare_turn(
            thread_id,
            Some(Turn {
                id: &id,
                text,
                files: &[],
            }),
        )
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        v.extend_from_slice(&13u32.to_be_bytes());
        v.extend_from_slice(b"IHDR");
        v.extend_from_slice(&w.to_be_bytes());
        v.extend_from_slice(&h.to_be_bytes());
        v.extend_from_slice(&[8, 6, 0, 0, 0]);
        v
    }

    fn attachment(name: &str, bytes: &[u8]) -> Attachment {
        Attachment {
            name: name.into(),
            mime: "image/png".into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    #[test]
    fn create_rename_retarget_delete() {
        let (s, _d) = service();
        let t = s
            .create_thread(Kind::Chat, Source::Local, "auto", None)
            .unwrap();
        assert_eq!(t.title, "");
        let t = s.rename_thread(&t.id, "  我的会话  ").unwrap();
        assert_eq!(t.title, "我的会话");
        let t = s
            .set_target(&t.id, Source::Cloud, "claude-sonnet-5", Some("tok"))
            .unwrap();
        assert_eq!(t.source, Source::Cloud);
        assert_eq!(t.token_id.as_deref(), Some("tok"));
        assert!(s
            .create_thread(Kind::Chat, Source::Local, "  ", None)
            .is_err());
        s.delete_thread(&t.id).unwrap();
        assert!(s.thread(&t.id).is_err());
    }

    #[test]
    fn prepare_turn_records_the_prompt_and_titles_the_thread() {
        let (s, _d) = service();
        let t = s
            .create_thread(Kind::Chat, Source::Local, "auto", None)
            .unwrap();
        say(&s, &t.id, "  给我讲个笑话\n第二行 ").unwrap();
        let d = s.thread(&t.id).unwrap();
        assert_eq!(d.thread.title, "给我讲个笑话");
        assert_eq!(d.messages.len(), 1);
        assert_eq!(d.messages[0].role, Role::User);
        assert_eq!(d.messages[0].content, "给我讲个笑话\n第二行");
        assert!(say(&s, &t.id, "   ").is_err());
    }

    #[test]
    fn regenerate_drops_the_trailing_reply_only() {
        let (s, _d) = service();
        let t = s
            .create_thread(Kind::Chat, Source::Local, "auto", None)
            .unwrap();
        assert!(s.prepare_turn(&t.id, None).is_err(), "空会话没得重来");
        say(&s, &t.id, "问").unwrap();
        let reply = Message {
            id: "r1".into(),
            thread_id: t.id.clone(),
            seq: 0,
            role: Role::Assistant,
            content: "答".into(),
            thinking: None,
            model: Some("auto".into()),
            routed: None,
            usage: None,
            error: None,
            duration_ms: None,
            ttft_ms: None,
            created_at: now_iso(),
            images: Vec::new(),
        };
        s.persist_reply(reply, &[]).unwrap();
        assert_eq!(s.thread(&t.id).unwrap().messages.len(), 2);
        s.prepare_turn(&t.id, None).unwrap();
        let d = s.thread(&t.id).unwrap();
        assert_eq!(d.messages.len(), 1);
        assert_eq!(d.messages[0].role, Role::User);
        // 再来一次：尾巴已经是 user 了，什么都不删。
        s.prepare_turn(&t.id, None).unwrap();
        assert_eq!(s.thread(&t.id).unwrap().messages.len(), 1);
    }

    #[test]
    fn persist_reply_assigns_seq_and_attaches_images() {
        let (s, _d) = service();
        let t = s
            .create_thread(Kind::Image, Source::Cloud, "gpt-image-1", Some("k"))
            .unwrap();
        say(&s, &t.id, "画只猫").unwrap();
        let m = Message {
            id: "m".into(),
            thread_id: t.id.clone(),
            seq: 0,
            role: Role::Assistant,
            content: String::new(),
            thinking: None,
            model: Some("gpt-image-1".into()),
            routed: None,
            usage: None,
            error: None,
            duration_ms: Some(1),
            ttft_ms: None,
            created_at: now_iso(),
            images: Vec::new(),
        };
        let img = ImageRef {
            id: "i".into(),
            message_id: "m".into(),
            mime: "image/png".into(),
            width: Some(1),
            height: Some(1),
            bytes: 3,
            size: None,
            created_at: now_iso(),
        };
        let saved = s.persist_reply(m, &[(img, "i.png".into())]).unwrap();
        assert_eq!(saved.seq, 2);
        assert_eq!(saved.images.len(), 1);
        let (path, mime) = s.image_path("i").unwrap().unwrap();
        assert!(path.ends_with("playground/images/i.png"));
        assert_eq!(mime, "image/png");
        assert!(s.image_path("nope").unwrap().is_none());
        let list = s.threads(Some(Kind::Image)).unwrap();
        assert_eq!(list[0].cover_image_id.as_deref(), Some("i"));
        assert_eq!(list[0].message_count, 2);
    }

    #[test]
    fn deleting_a_thread_removes_its_files_from_disk() {
        let (s, dir) = service();
        let t = s
            .create_thread(Kind::Image, Source::Cloud, "m", None)
            .unwrap();
        let m = Message {
            id: "m".into(),
            thread_id: t.id.clone(),
            seq: 0,
            role: Role::Assistant,
            content: String::new(),
            thinking: None,
            model: None,
            routed: None,
            usage: None,
            error: None,
            duration_ms: None,
            ttft_ms: None,
            created_at: now_iso(),
            images: Vec::new(),
        };
        let img = ImageRef {
            id: "i".into(),
            message_id: "m".into(),
            mime: "image/png".into(),
            width: None,
            height: None,
            bytes: 3,
            size: None,
            created_at: now_iso(),
        };
        std::fs::create_dir_all(s.images_dir()).unwrap();
        let file = dir.path().join("playground/images/i.png");
        std::fs::write(&file, b"png").unwrap();
        s.persist_reply(m, &[(img, "i.png".into())]).unwrap();
        assert!(file.exists());
        s.delete_thread(&t.id).unwrap();
        assert!(!file.exists());
    }

    #[test]
    fn one_run_per_thread_and_stop_is_idempotent() {
        let (s, _d) = service();
        let run = s.register("r1", "t1", Kind::Chat).unwrap();
        assert!(
            s.register("r2", "t1", Kind::Chat).is_err(),
            "同一会话不能两路并发"
        );
        assert!(s.register("r3", "t2", Kind::Chat).is_ok());
        run.state.lock().unwrap().apply(
            &TryEvent::Delta {
                text: "半截".into(),
            },
            Instant::now(),
        );
        let a = s.active("t1").unwrap();
        assert_eq!(a.request_id, "r1");
        assert_eq!(a.text, "半截");
        assert!(s.stop("r1"));
        assert!(!s.stop("nope"));
        s.unregister("r1");
        assert!(s.active("t1").is_none());
        assert!(s.delete_thread("t2").is_err(), "有请求在跑的会话不让删");
    }

    #[tokio::test]
    async fn chat_against_a_dead_endpoint_still_records_a_failed_reply() {
        let (s, _d) = service();
        let t = s
            .create_thread(Kind::Chat, Source::Local, "auto", None)
            .unwrap();
        // 127.0.0.1:9 是 discard 端口，没人听：连接被拒，走「连网关都没打通」那条路。
        let ep = Endpoint {
            base_url: "http://127.0.0.1:9".into(),
            api_key: "k".into(),
        };
        let mut seen = Vec::new();
        let m = s
            .chat("req", &t.id, Some("你好"), &[], &ep, |ev| seen.push(ev))
            .await
            .unwrap();
        assert_eq!(m.role, Role::Assistant);
        assert!(m.error.is_some());
        assert!(m.content.is_empty());
        assert!(
            matches!(seen.last(), Some(TryEvent::Error { .. })),
            "监听者要等到一个收尾帧"
        );
        let d = s.thread(&t.id).unwrap();
        assert_eq!(d.messages.len(), 2);
        assert_eq!(d.thread.title, "你好");
        assert!(s.active(&t.id).is_none(), "跑完要注销");
    }

    #[tokio::test]
    async fn attachments_land_on_the_user_message_and_go_into_the_history_as_parts() {
        let (s, dir) = service();
        let t = s
            .create_thread(Kind::Chat, Source::Cloud, "m", Some("tok"))
            .unwrap();
        let ep = Endpoint {
            base_url: "http://127.0.0.1:9".into(),
            api_key: "k".into(),
        };
        let files = [attachment("猫.png", &png(8, 6))];
        s.chat("req", &t.id, Some("这是什么"), &files, &ep, |_| {})
            .await
            .unwrap();

        let d = s.thread(&t.id).unwrap();
        let user = &d.messages[0];
        assert_eq!(user.role, Role::User);
        assert_eq!(user.images.len(), 1, "图挂在 user 消息上");
        assert_eq!(user.images[0].mime, "image/png");
        assert_eq!(
            (user.images[0].width, user.images[0].height),
            (Some(8), Some(6))
        );
        assert!(user.images[0].size.is_none(), "附件没有「问上游要的规格」");
        let (path, _) = s.image_path(&user.images[0].id).unwrap().unwrap();
        assert!(path.starts_with(dir.path()) && path.is_file());

        let history = s.history(&t.id).unwrap();
        let Content::Parts(parts) = &history[0].content else {
            panic!("带图的 user 消息该摊成 parts");
        };
        assert_eq!(
            parts[0],
            Part::Text {
                text: "这是什么".into()
            }
        );
        assert!(
            matches!(&parts[1], Part::ImageUrl { image_url } if image_url.url.starts_with("data:image/png;base64,"))
        );
        // 文件被人删掉之后不该整轮发不出去，只是这一次不带图。
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(
            &s.history(&t.id).unwrap()[0].content,
            Content::Text(t) if t == "这是什么"
        ));
        // 删会话时附件跟着走。
        s.delete_thread(&t.id).unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn oversized_or_unreadable_attachments_are_refused_before_anything_is_written() {
        let (s, _d) = service();
        let t = s
            .create_thread(Kind::Chat, Source::Local, "m", None)
            .unwrap();
        let ep = Endpoint {
            base_url: "http://127.0.0.1:9".into(),
            api_key: "k".into(),
        };
        let huge = attachment("大.png", &vec![0u8; MAX_ATTACHMENT_BYTES + 1]);
        let e = s
            .chat("r1", &t.id, Some("看"), &[huge], &ep, |_| {})
            .await
            .unwrap_err();
        assert!(e.to_string().contains("20MB"));
        let bad = Attachment {
            name: "坏.png".into(),
            mime: "image/png".into(),
            data_base64: "!!!".into(),
        };
        assert!(s
            .chat("r2", &t.id, Some("看"), &[bad], &ep, |_| {})
            .await
            .is_err());
        // 重新生成不收附件。
        assert!(s
            .chat(
                "r3",
                &t.id,
                None,
                &[attachment("猫.png", &png(1, 1))],
                &ep,
                |_| {}
            )
            .await
            .is_err());
        assert!(
            s.thread(&t.id).unwrap().messages.is_empty(),
            "什么都不该落下"
        );
        assert!(s.active(&t.id).is_none());
    }

    #[tokio::test]
    async fn image_generation_rejects_chat_threads_and_records_failures() {
        let (s, _d) = service();
        let chat = s
            .create_thread(Kind::Chat, Source::Cloud, "m", None)
            .unwrap();
        let ep = Endpoint {
            base_url: "http://127.0.0.1:9".into(),
            api_key: "k".into(),
        };
        let req = ImageRequest {
            prompt: "猫".into(),
            size: Some("1024x1024".into()),
            n: 1,
        };
        assert!(s
            .generate_image("r", &chat.id, req.clone(), &ep)
            .await
            .is_err());

        let img = s
            .create_thread(Kind::Image, Source::Cloud, "m", None)
            .unwrap();
        let m = s.generate_image("r", &img.id, req, &ep).await.unwrap();
        assert!(m.error.is_some());
        assert!(m.images.is_empty());
        assert_eq!(s.thread(&img.id).unwrap().messages.len(), 2);
    }
}
