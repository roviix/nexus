//! 游乐场的三张表（`playground_threads` / `playground_messages` / `playground_images`）。
//! 表结构在 `nexus-store` 的迁移 v3 里，这里只有读写。
//!
//! 写入都拿 `&Connection`：既能在 `Db::with` 里单条用，也能塞进 `Db::tx` 一个事务里——
//! 「记用户那句话 + 建会话标题」「记回复 + 记它带的图」都不允许出现只写了一半的中间态。

use crate::model::{Asset, ImageRef, Kind, Message, Role, Source, Thread, ThreadSummary, Usage};
use nexus_core::{AppError, Result};
use nexus_store::{Db, SqlExt};
use rusqlite::{params, Connection, OptionalExtension, Row};

/// 列表第二行最多留这么多字符。
const PREVIEW_CHARS: usize = 80;

pub fn not_found() -> AppError {
    AppError::invalid("这个会话已经不存在了。").with_hint("刷新列表再试。")
}

fn thread_from_row(r: &Row<'_>) -> rusqlite::Result<Thread> {
    let kind: String = r.get("kind")?;
    let source: String = r.get("source")?;
    Ok(Thread {
        id: r.get("id")?,
        kind: Kind::parse(&kind).unwrap_or(Kind::Chat),
        title: r.get("title")?,
        source: Source::parse(&source).unwrap_or(Source::Local),
        model: r.get("model")?,
        token_id: r.get("token_id")?,
        created_at: r.get("created_at")?,
        updated_at: r.get("updated_at")?,
    })
}

pub fn list_threads(db: &Db, kind: Option<Kind>) -> Result<Vec<ThreadSummary>> {
    db.with(|c| {
        let mut stmt = c.prepare(
            r#"
SELECT t.id, t.kind, t.title, t.source, t.model, t.token_id, t.created_at, t.updated_at,
       (SELECT COUNT(*) FROM playground_messages m WHERE m.thread_id = t.id) AS n,
       (SELECT m.content FROM playground_messages m
          WHERE m.thread_id = t.id AND m.content <> '' ORDER BY m.seq DESC LIMIT 1) AS preview,
       (SELECT i.id FROM playground_images i
          WHERE i.thread_id = t.id ORDER BY i.created_at DESC, i.rowid DESC LIMIT 1) AS cover
FROM playground_threads t
WHERE (?1 IS NULL OR t.kind = ?1)
ORDER BY t.updated_at DESC
"#,
        )?;
        let rows = stmt.query_map([kind.map(Kind::as_str)], |r| {
            let n: i64 = r.get("n")?;
            let preview: Option<String> = r.get("preview")?;
            Ok(ThreadSummary {
                thread: thread_from_row(r)?,
                message_count: n.max(0) as u32,
                preview: preview.map(|p| crate::model::title_from(&p, PREVIEW_CHARS)),
                cover_image_id: r.get("cover")?,
            })
        })?;
        rows.collect()
    })
}

pub fn get_thread(db: &Db, id: &str) -> Result<Thread> {
    db.with(|c| get_thread_in(c, id))?.ok_or_else(not_found)
}

pub fn get_thread_in(c: &Connection, id: &str) -> rusqlite::Result<Option<Thread>> {
    c.query_row(
        "SELECT id, kind, title, source, model, token_id, created_at, updated_at
         FROM playground_threads WHERE id = ?1",
        [id],
        thread_from_row,
    )
    .optional()
}

pub fn insert_thread(db: &Db, t: &Thread) -> Result<()> {
    db.with(|c| {
        c.execute(
            "INSERT INTO playground_threads (id, kind, title, source, model, token_id, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                t.id,
                t.kind.as_str(),
                t.title,
                t.source.as_str(),
                t.model,
                t.token_id,
                t.created_at,
                t.updated_at
            ],
        )
    })?;
    Ok(())
}

pub fn rename_thread(db: &Db, id: &str, title: &str, now: &str) -> Result<()> {
    let n = db.with(|c| {
        c.execute(
            "UPDATE playground_threads SET title = ?2, updated_at = ?3 WHERE id = ?1",
            params![id, title, now],
        )
    })?;
    if n == 0 {
        return Err(not_found());
    }
    Ok(())
}

pub fn set_target(
    db: &Db,
    id: &str,
    source: Source,
    model: &str,
    token_id: Option<&str>,
) -> Result<()> {
    // 换目标不算「有新内容」，不动 updated_at——否则挑一圈模型就把会话顶到列表最上面。
    let n = db.with(|c| {
        c.execute(
            "UPDATE playground_threads SET source = ?2, model = ?3, token_id = ?4 WHERE id = ?1",
            params![id, source.as_str(), model, token_id],
        )
    })?;
    if n == 0 {
        return Err(not_found());
    }
    Ok(())
}

/// 会话里第一条消息进来时给它一个标题（还没有的话），并把 updated_at 顶到现在。
pub fn touch_thread_in(
    c: &Connection,
    id: &str,
    title_if_empty: Option<&str>,
    now: &str,
) -> rusqlite::Result<()> {
    if let Some(t) = title_if_empty {
        c.execute(
            "UPDATE playground_threads SET title = ?2 WHERE id = ?1 AND title = ''",
            params![id, t],
        )?;
    }
    c.execute(
        "UPDATE playground_threads SET updated_at = ?2 WHERE id = ?1",
        params![id, now],
    )?;
    Ok(())
}

/// 删会话。级联把消息和图片行一起带走；返回图片文件名，让调用方去盘上清。
pub fn delete_thread(db: &Db, id: &str) -> Result<Vec<String>> {
    db.tx(|tx| {
        let files = image_files_in(tx, "thread_id", id).sql()?;
        let n = tx
            .execute("DELETE FROM playground_threads WHERE id = ?1", [id])
            .sql()?;
        if n == 0 {
            return Err(not_found());
        }
        Ok(files)
    })
}

/// 删一条消息（连它的图）。返回图片文件名。
pub fn delete_message(db: &Db, id: &str) -> Result<Vec<String>> {
    db.tx(|tx| {
        let files = image_files_in(tx, "message_id", id).sql()?;
        tx.execute("DELETE FROM playground_messages WHERE id = ?1", [id])
            .sql()?;
        Ok(files)
    })
}

fn image_files_in(c: &Connection, col: &str, id: &str) -> rusqlite::Result<Vec<String>> {
    // 列名来自代码里的两个字面量，不是用户输入。
    let mut stmt = c.prepare(&format!(
        "SELECT file FROM playground_images WHERE {col} = ?1"
    ))?;
    let rows = stmt.query_map([id], |r| r.get::<_, String>(0))?;
    rows.collect()
}

fn message_from_row(r: &Row<'_>) -> rusqlite::Result<Message> {
    let role: String = r.get("role")?;
    let usage_json: Option<String> = r.get("usage_json")?;
    let seq: i64 = r.get("seq")?;
    let duration_ms: Option<i64> = r.get("duration_ms")?;
    let ttft_ms: Option<i64> = r.get("ttft_ms")?;
    Ok(Message {
        id: r.get("id")?,
        thread_id: r.get("thread_id")?,
        seq: seq.max(0) as u32,
        role: Role::parse(&role),
        content: r.get("content")?,
        thinking: r.get("thinking")?,
        model: r.get("model")?,
        routed: r.get("routed")?,
        usage: usage_json.and_then(|u| serde_json::from_str::<Usage>(&u).ok()),
        error: r.get("error")?,
        duration_ms: duration_ms.map(|d| d.max(0) as u64),
        ttft_ms: ttft_ms.map(|d| d.max(0) as u64),
        created_at: r.get("created_at")?,
        images: Vec::new(),
    })
}

fn image_from_row(r: &Row<'_>) -> rusqlite::Result<ImageRef> {
    let width: Option<i64> = r.get("width")?;
    let height: Option<i64> = r.get("height")?;
    let bytes: i64 = r.get("bytes")?;
    Ok(ImageRef {
        id: r.get("id")?,
        message_id: r.get("message_id")?,
        mime: r.get("mime")?,
        width: width.map(|w| w.max(0) as u32),
        height: height.map(|h| h.max(0) as u32),
        bytes: bytes.max(0) as u64,
        size: r.get("size")?,
        created_at: r.get("created_at")?,
    })
}

const MESSAGE_COLS: &str = "id, thread_id, seq, role, content, thinking, model, routed, usage_json, error, duration_ms, ttft_ms, created_at";
const IMAGE_COLS: &str = "id, message_id, mime, width, height, bytes, size, created_at";

/// 一个会话的全部消息，按顺序，图片已挂到各自的消息上。
pub fn list_messages(db: &Db, thread_id: &str) -> Result<Vec<Message>> {
    db.with(|c| {
        let mut stmt = c.prepare(&format!(
            "SELECT {MESSAGE_COLS} FROM playground_messages WHERE thread_id = ?1 ORDER BY seq"
        ))?;
        let mut messages: Vec<Message> = stmt
            .query_map([thread_id], message_from_row)?
            .collect::<rusqlite::Result<_>>()?;

        let mut stmt = c.prepare(&format!(
            "SELECT {IMAGE_COLS} FROM playground_images WHERE thread_id = ?1 ORDER BY created_at, rowid"
        ))?;
        let images: Vec<ImageRef> = stmt
            .query_map([thread_id], image_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        for img in images {
            if let Some(m) = messages.iter_mut().find(|m| m.id == img.message_id) {
                m.images.push(img);
            }
        }
        Ok(messages)
    })
}

pub fn get_message(db: &Db, id: &str) -> Result<Option<Message>> {
    db.with(|c| {
        let Some(mut m) = c
            .query_row(
                &format!("SELECT {MESSAGE_COLS} FROM playground_messages WHERE id = ?1"),
                [id],
                message_from_row,
            )
            .optional()?
        else {
            return Ok(None);
        };
        let mut stmt = c.prepare(&format!(
            "SELECT {IMAGE_COLS} FROM playground_images WHERE message_id = ?1 ORDER BY created_at, rowid"
        ))?;
        m.images = stmt
            .query_map([id], image_from_row)?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Some(m))
    })
}

/// 会话里最后一条消息（不带图）。
pub fn last_message_in(c: &Connection, thread_id: &str) -> rusqlite::Result<Option<Message>> {
    c.query_row(
        &format!(
            "SELECT {MESSAGE_COLS} FROM playground_messages WHERE thread_id = ?1 ORDER BY seq DESC LIMIT 1"
        ),
        [thread_id],
        message_from_row,
    )
    .optional()
}

pub fn next_seq_in(c: &Connection, thread_id: &str) -> rusqlite::Result<u32> {
    let max: Option<i64> = c.query_row(
        "SELECT MAX(seq) FROM playground_messages WHERE thread_id = ?1",
        [thread_id],
        |r| r.get(0),
    )?;
    Ok((max.unwrap_or(0).max(0) as u32) + 1)
}

pub fn insert_message_in(c: &Connection, m: &Message) -> rusqlite::Result<()> {
    let usage_json = m
        .usage
        .as_ref()
        .map(|u| serde_json::to_string(u).unwrap_or_default());
    c.execute(
        "INSERT INTO playground_messages
           (id, thread_id, seq, role, content, thinking, model, routed, usage_json, error, duration_ms, ttft_ms, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            m.id,
            m.thread_id,
            m.seq as i64,
            m.role.as_str(),
            m.content,
            m.thinking,
            m.model,
            m.routed,
            usage_json,
            m.error,
            m.duration_ms.map(|d| d as i64),
            m.ttft_ms.map(|d| d as i64),
            m.created_at,
        ],
    )?;
    Ok(())
}

pub fn insert_image_in(
    c: &Connection,
    img: &ImageRef,
    thread_id: &str,
    file: &str,
) -> rusqlite::Result<()> {
    c.execute(
        "INSERT INTO playground_images
           (id, message_id, thread_id, file, mime, width, height, bytes, size, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            img.id,
            img.message_id,
            thread_id,
            file,
            img.mime,
            img.width.map(|w| w as i64),
            img.height.map(|h| h as i64),
            img.bytes as i64,
            img.size,
            img.created_at,
        ],
    )?;
    Ok(())
}

/// 全部图片，新的在前，带上会话标题、模型与出图的那句提示词。资产页用。
pub fn list_assets(db: &Db) -> Result<Vec<Asset>> {
    db.with(|c| {
        let mut stmt = c.prepare(
            r#"
SELECT i.id, i.message_id, i.mime, i.width, i.height, i.bytes, i.size, i.created_at,
       i.thread_id, t.title AS thread_title, m.model,
       (SELECT u.content FROM playground_messages u
          WHERE u.thread_id = i.thread_id AND u.role = 'user' AND u.seq < m.seq
          ORDER BY u.seq DESC LIMIT 1) AS prompt
FROM playground_images i
JOIN playground_messages m ON m.id = i.message_id
JOIN playground_threads t ON t.id = i.thread_id
ORDER BY i.created_at DESC, i.rowid DESC
"#,
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(Asset {
                image: image_from_row(r)?,
                thread_id: r.get("thread_id")?,
                thread_title: r.get("thread_title")?,
                model: r.get("model")?,
                prompt: r.get("prompt")?,
            })
        })?;
        rows.collect()
    })
}

/// 删一张图（只删这一张，消息留着）。返回文件名让调用方去盘上清；没有这张图返回 None。
pub fn delete_image(db: &Db, id: &str) -> Result<Option<String>> {
    db.tx(|tx| {
        let file: Option<String> = tx
            .query_row(
                "SELECT file FROM playground_images WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .optional()
            .sql()?;
        if file.is_some() {
            tx.execute("DELETE FROM playground_images WHERE id = ?1", [id])
                .sql()?;
        }
        Ok(file)
    })
}

/// 图片的文件名与 MIME：`nexus-image://` 协议按 id 取图用。
pub fn image_file(db: &Db, id: &str) -> Result<Option<(String, String)>> {
    db.with(|c| {
        c.query_row(
            "SELECT file, mime FROM playground_images WHERE id = ?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(db: &Db, kind: Kind) -> Thread {
        let t = Thread {
            id: uuid::Uuid::new_v4().to_string(),
            kind,
            title: String::new(),
            source: Source::Local,
            model: "auto".into(),
            token_id: None,
            created_at: "2026-09-03T00:00:00Z".into(),
            updated_at: "2026-09-03T00:00:00Z".into(),
        };
        insert_thread(db, &t).unwrap();
        t
    }

    fn message(thread_id: &str, seq: u32, role: Role, content: &str) -> Message {
        Message {
            id: uuid::Uuid::new_v4().to_string(),
            thread_id: thread_id.into(),
            seq,
            role,
            content: content.into(),
            thinking: None,
            model: None,
            routed: None,
            usage: None,
            error: None,
            duration_ms: None,
            ttft_ms: None,
            created_at: format!("2026-09-03T00:00:{seq:02}Z"),
            images: Vec::new(),
        }
    }

    #[test]
    fn threads_list_newest_first_with_counts_and_previews() {
        let db = Db::open_in_memory().unwrap();
        let a = thread(&db, Kind::Chat);
        let b = thread(&db, Kind::Image);
        db.with(|c| {
            insert_message_in(c, &message(&a.id, 1, Role::User, "第一句话，很长很长很长"))?;
            insert_message_in(c, &message(&a.id, 2, Role::Assistant, "回答"))?;
            touch_thread_in(c, &a.id, Some("第一句话"), "2026-09-03T01:00:00Z")?;
            insert_message_in(c, &message(&b.id, 1, Role::User, "画只猫"))?;
            touch_thread_in(c, &b.id, Some("画只猫"), "2026-09-03T02:00:00Z")
        })
        .unwrap();

        let all = list_threads(&db, None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].thread.id, b.id, "updated_at 新的在前");
        assert_eq!(all[0].thread.title, "画只猫");
        assert_eq!(all[1].message_count, 2);
        assert_eq!(all[1].preview.as_deref(), Some("回答"));

        let chats = list_threads(&db, Some(Kind::Chat)).unwrap();
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].thread.kind, Kind::Chat);
    }

    #[test]
    fn title_is_only_set_while_empty() {
        let db = Db::open_in_memory().unwrap();
        let t = thread(&db, Kind::Chat);
        db.with(|c| touch_thread_in(c, &t.id, Some("一"), "2026-09-03T01:00:00Z"))
            .unwrap();
        db.with(|c| touch_thread_in(c, &t.id, Some("二"), "2026-09-03T02:00:00Z"))
            .unwrap();
        let got = get_thread(&db, &t.id).unwrap();
        assert_eq!(got.title, "一");
        assert_eq!(got.updated_at, "2026-09-03T02:00:00Z");
    }

    #[test]
    fn messages_come_back_in_order_with_their_images() {
        let db = Db::open_in_memory().unwrap();
        let t = thread(&db, Kind::Image);
        let u = message(&t.id, 1, Role::User, "画只猫");
        let mut a = message(&t.id, 2, Role::Assistant, "");
        a.model = Some("gpt-image-1".into());
        a.duration_ms = Some(12_000);
        let img = ImageRef {
            id: "img1".into(),
            message_id: a.id.clone(),
            mime: "image/png".into(),
            width: Some(1024),
            height: Some(1024),
            bytes: 12345,
            size: Some("1024x1024".into()),
            created_at: "2026-09-03T00:00:03Z".into(),
        };
        db.tx(|tx| {
            insert_message_in(tx, &u).sql()?;
            insert_message_in(tx, &a).sql()?;
            insert_image_in(tx, &img, &t.id, "img1.png").sql()?;
            Ok(())
        })
        .unwrap();

        let list = list_messages(&db, &t.id).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].role, Role::User);
        assert_eq!(list[1].images.len(), 1);
        assert_eq!(list[1].images[0].width, Some(1024));
        assert_eq!(list[1].duration_ms, Some(12_000));
        assert_eq!(
            image_file(&db, "img1").unwrap(),
            Some(("img1.png".into(), "image/png".into()))
        );
        assert_eq!(
            list_threads(&db, None).unwrap()[0]
                .cover_image_id
                .as_deref(),
            Some("img1")
        );
        assert_eq!(db.with(|c| next_seq_in(c, &t.id)).unwrap(), 3);
    }

    #[test]
    fn usage_round_trips_through_json() {
        let db = Db::open_in_memory().unwrap();
        let t = thread(&db, Kind::Chat);
        let mut a = message(&t.id, 1, Role::Assistant, "hi");
        a.usage = Some(Usage {
            prompt_tokens: 3,
            completion_tokens: 5,
        });
        db.with(|c| insert_message_in(c, &a)).unwrap();
        let got = get_message(&db, &a.id).unwrap().unwrap();
        assert_eq!(got.usage.unwrap().completion_tokens, 5);
    }

    #[test]
    fn deleting_a_thread_cascades_and_hands_back_image_files() {
        let db = Db::open_in_memory().unwrap();
        let t = thread(&db, Kind::Image);
        let a = message(&t.id, 1, Role::Assistant, "");
        let img = ImageRef {
            id: "i".into(),
            message_id: a.id.clone(),
            mime: "image/png".into(),
            width: None,
            height: None,
            bytes: 1,
            size: None,
            created_at: "2026-09-03T00:00:00Z".into(),
        };
        db.with(|c| {
            insert_message_in(c, &a)?;
            insert_image_in(c, &img, &t.id, "i.png")
        })
        .unwrap();
        let files = delete_thread(&db, &t.id).unwrap();
        assert_eq!(files, vec!["i.png".to_string()]);
        assert!(get_thread(&db, &t.id).is_err());
        let n: i64 = db
            .with(|c| c.query_row("SELECT COUNT(*) FROM playground_images", [], |r| r.get(0)))
            .unwrap();
        assert_eq!(n, 0, "级联删除要把图片行一起带走");
        assert!(delete_thread(&db, "nope").is_err());
    }

    #[test]
    fn assets_join_the_prompt_that_made_them_and_delete_one_at_a_time() {
        let db = Db::open_in_memory().unwrap();
        let t = thread(&db, Kind::Image);
        db.with(|c| {
            c.execute(
                "UPDATE playground_threads SET title = '猫' WHERE id = ?1",
                [&t.id],
            )
        })
        .unwrap();
        let u1 = message(&t.id, 1, Role::User, "画只猫");
        let mut a1 = message(&t.id, 2, Role::Assistant, "");
        a1.model = Some("gpt-image-1".into());
        let u2 = message(&t.id, 3, Role::User, "换成狗");
        let mut a2 = message(&t.id, 4, Role::Assistant, "");
        a2.model = Some("seedream-5".into());
        let img = |id: &str, m: &Message, at: &str| ImageRef {
            id: id.into(),
            message_id: m.id.clone(),
            mime: "image/png".into(),
            width: None,
            height: None,
            bytes: 10,
            size: None,
            created_at: at.into(),
        };
        db.with(|c| {
            for m in [&u1, &a1, &u2, &a2] {
                insert_message_in(c, m)?;
            }
            insert_image_in(c, &img("i1", &a1, "2026-09-03T00:00:02Z"), &t.id, "i1.png")?;
            insert_image_in(c, &img("i2", &a2, "2026-09-03T00:00:04Z"), &t.id, "i2.png")?;
            insert_image_in(c, &img("i3", &a2, "2026-09-03T00:00:04Z"), &t.id, "i3.png")
        })
        .unwrap();

        let assets = list_assets(&db).unwrap();
        assert_eq!(assets.len(), 3);
        assert_eq!(assets[0].image.id, "i3", "新的在前，同一批按 rowid 倒序");
        assert_eq!(assets[0].prompt.as_deref(), Some("换成狗"));
        assert_eq!(assets[0].model.as_deref(), Some("seedream-5"));
        assert_eq!(assets[0].thread_title, "猫");
        assert_eq!(assets[2].image.id, "i1");
        assert_eq!(assets[2].prompt.as_deref(), Some("画只猫"));

        assert_eq!(delete_image(&db, "i2").unwrap(), Some("i2.png".into()));
        assert_eq!(delete_image(&db, "i2").unwrap(), None);
        assert_eq!(list_assets(&db).unwrap().len(), 2);
        // 消息本身还在，只是少了一张图。
        assert_eq!(get_message(&db, &a2.id).unwrap().unwrap().images.len(), 1);
    }

    #[test]
    fn set_target_does_not_bump_updated_at_but_rename_does() {
        let db = Db::open_in_memory().unwrap();
        let t = thread(&db, Kind::Chat);
        set_target(&db, &t.id, Source::Cloud, "claude-sonnet-5", Some("tok")).unwrap();
        let got = get_thread(&db, &t.id).unwrap();
        assert_eq!(got.source, Source::Cloud);
        assert_eq!(got.token_id.as_deref(), Some("tok"));
        assert_eq!(got.updated_at, t.updated_at);
        rename_thread(&db, &t.id, "新名字", "2026-09-04T00:00:00Z").unwrap();
        let got = get_thread(&db, &t.id).unwrap();
        assert_eq!(got.title, "新名字");
        assert_eq!(got.updated_at, "2026-09-04T00:00:00Z");
    }
}
