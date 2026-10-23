//! 按会话记住上游发过的 thoughtSignature。
//!
//! 客户端经常把签名丢掉，下一轮只带回思考正文。没有签名，Claude / Gemini 的工具轮会 400。
//! 这里按「会话 + 第几轮模型回复」存真实签名，请求里缺的时候再放回去。

use crate::protocol::{ContentMessage, ContentPart};
use base64::Engine;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Gemini 工具轮没有真签名时，官方校验器认这串哨兵，缺了就 400。
/// Claude 模型不认它，送上去是 `Invalid signature`。
pub const GEMINI_SIGNATURE_SENTINEL: &str = "skip_thought_signature_validator";

const MAX_SESSIONS: usize = 256;
const MAX_TURNS: usize = 64;
const TTL: Duration = Duration::from_secs(6 * 60 * 60);

struct TurnSig {
    signature: String,
    at: Instant,
}

#[derive(Default)]
pub struct SignatureLedger {
    sessions: Mutex<HashMap<String, Vec<TurnSig>>>,
}

impl SignatureLedger {
    pub fn remember(&self, session: &str, turn: usize, signature: &str) {
        let signature = signature.trim();
        if session.is_empty() || signature.is_empty() || turn >= MAX_TURNS {
            return;
        }
        let mut sessions = self.sessions.lock().expect("signature ledger");
        sweep(&mut sessions);
        let turns = sessions.entry(session.to_string()).or_default();
        if turns.len() <= turn {
            turns.resize_with(turn + 1, || TurnSig {
                signature: String::new(),
                at: Instant::now(),
            });
        }
        turns[turn] = TurnSig {
            signature: signature.to_string(),
            at: Instant::now(),
        };
        if sessions.len() > MAX_SESSIONS {
            let overflow = sessions.len() - MAX_SESSIONS;
            let mut stale: Vec<(String, Instant)> = sessions
                .iter()
                .filter_map(|(key, turns)| turns.last().map(|turn| (key.clone(), turn.at)))
                .collect();
            stale.sort_by_key(|(_, at)| *at);
            for (key, _) in stale.into_iter().take(overflow) {
                sessions.remove(&key);
            }
        }
    }

    pub fn recall(&self, session: &str, turn: usize) -> Option<String> {
        let sessions = self.sessions.lock().expect("signature ledger");
        sessions
            .get(session)
            .and_then(|turns| turns.get(turn))
            .and_then(|turn| {
                if turn.signature.is_empty() || turn.at.elapsed() > TTL {
                    None
                } else {
                    Some(turn.signature.clone())
                }
            })
    }

    /// 模型轮按出现顺序编号。客户端带了真签名就记下；没带就用这一轮上次的。
    /// 记完之后按目标模型把签名放到上游要求的那个 part 上。
    pub fn restore(&self, session: &str, model: &str, contents: &mut [ContentMessage]) {
        if session.is_empty() {
            seal_model_turns(model, contents);
            return;
        }
        let mut turn = 0usize;
        for message in contents.iter_mut() {
            if message.role.as_deref() != Some("model") {
                continue;
            }
            if let Some(existing) = current_signature(message) {
                self.remember(session, turn, &signature_for_client(model, &existing));
            } else if let Some(saved) = self.recall(session, turn) {
                if let Some(part) = message.parts.last_mut() {
                    part.thought_signature = Some(saved);
                }
            }
            turn += 1;
        }
        seal_model_turns(model, contents);
    }
}

pub fn is_claude_model(model: &str) -> bool {
    model.to_ascii_lowercase().contains("claude")
}

/// 发给 Google 之前：Claude 签名包一层 Base64。REST 会自己解一次，
/// 不解包的话上游看到的是原始字节，直接 `Invalid signature`。
pub fn prepare_upstream_signature(model: &str, signature: &str) -> String {
    if is_claude_model(model) {
        ensure_google_claude_thought_signature(signature)
    } else {
        signature.trim().to_string()
    }
}

/// 回给 Claude Code 之前拆掉那层包装。客户端下一轮原样带回，我们再包回去。
pub fn signature_for_client(model: &str, signature: &str) -> String {
    if is_claude_model(model) {
        ensure_raw_claude_thought_signature(signature)
    } else {
        signature.trim().to_string()
    }
}

/// 每一轮模型回复只留一处签名，并且落在上游会检查的那个 part 上。
pub fn seal_model_turns(model: &str, contents: &mut [ContentMessage]) {
    for message in contents {
        if message.role.as_deref() == Some("model") {
            seal_model_turn(model, &mut message.parts);
        }
    }
}

fn seal_model_turn(model: &str, parts: &mut [ContentPart]) {
    let claude = is_claude_model(model);
    let existing = turn_signature(parts).map(|sig| prepare_upstream_signature(model, &sig));
    for part in parts.iter_mut() {
        part.thought_signature = None;
    }
    if claude {
        if let (Some(sig), Some(index)) = (existing, first_non_thought(parts)) {
            parts[index].thought_signature = Some(sig);
        }
        return;
    }
    let calls: Vec<usize> = parts
        .iter()
        .enumerate()
        .filter(|(_, part)| part.function_call.is_some())
        .map(|(index, _)| index)
        .collect();
    if let Some(sig) = existing {
        let anchor = calls.first().copied().or_else(|| first_non_thought(parts));
        if let Some(index) = anchor {
            parts[index].thought_signature = Some(sig);
        }
    }
    for index in calls {
        if parts[index]
            .thought_signature
            .as_deref()
            .is_none_or(|sig| sig.trim().is_empty())
        {
            parts[index].thought_signature = Some(GEMINI_SIGNATURE_SENTINEL.to_string());
        }
    }
}

fn turn_signature(parts: &[ContentPart]) -> Option<String> {
    parts
        .iter()
        .find(|part| part.function_call.is_some())
        .and_then(real_signature)
        .or_else(|| parts.iter().find_map(real_signature))
}

fn real_signature(part: &ContentPart) -> Option<String> {
    let signature = part.thought_signature.as_deref()?.trim();
    if signature.is_empty() || signature == GEMINI_SIGNATURE_SENTINEL {
        None
    } else {
        Some(signature.to_string())
    }
}

fn first_non_thought(parts: &[ContentPart]) -> Option<usize> {
    parts.iter().position(|part| part.thought != Some(true))
}

fn ensure_google_claude_thought_signature(signature: &str) -> String {
    let signature = signature.trim();
    if signature.is_empty() || signature == GEMINI_SIGNATURE_SENTINEL {
        return signature.to_string();
    }
    if let Some(decoded) = b64(signature) {
        if is_wrapped_claude_payload(&decoded) {
            return signature.to_string();
        }
        if is_raw_claude_payload(&decoded) {
            return base64::engine::general_purpose::STANDARD.encode(signature.as_bytes());
        }
    }
    signature.to_string()
}

fn ensure_raw_claude_thought_signature(signature: &str) -> String {
    let signature = signature.trim();
    if signature.is_empty() || signature == GEMINI_SIGNATURE_SENTINEL {
        return signature.to_string();
    }
    if let Some(decoded) = b64(signature) {
        if is_wrapped_claude_payload(&decoded) {
            if let Ok(raw) = String::from_utf8(decoded) {
                return raw;
            }
        }
    }
    signature.to_string()
}

fn b64(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(text).ok()
}

fn is_raw_claude_payload(bytes: &[u8]) -> bool {
    bytes
        .windows(6)
        .any(|window| window.eq_ignore_ascii_case(b"claude"))
        || bytes.starts_with(&[0x08, 0x04, 0x12])
}

fn is_wrapped_claude_payload(decoded: &[u8]) -> bool {
    if decoded.starts_with(b"CAQS") {
        return true;
    }
    if let Ok(inner) = base64::engine::general_purpose::STANDARD.decode(decoded) {
        if is_raw_claude_payload(&inner) {
            return true;
        }
    }
    false
}

fn current_signature(message: &ContentMessage) -> Option<String> {
    turn_signature(&message.parts)
}

fn sweep(sessions: &mut HashMap<String, Vec<TurnSig>>) {
    sessions.retain(|_, turns| {
        turns
            .iter()
            .any(|turn| turn.at.elapsed() <= TTL && !turn.signature.is_empty())
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ContentPart;

    #[test]
    fn missing_signature_is_restored_on_the_same_turn() {
        let ledger = SignatureLedger::default();
        ledger.remember("sess", 0, "EqUrealSignature");
        ledger.remember("sess", 1, "EqUnext");

        let mut contents = vec![
            ContentMessage::user(vec![ContentPart::text("hi")]),
            ContentMessage::model(vec![ContentPart::thought("hmm"), ContentPart::text("ok")]),
            ContentMessage::model(vec![ContentPart {
                text: None,
                thought: None,
                thought_signature: None,
                function_call: Some(crate::protocol::FunctionCall {
                    id: Some("call_1".into()),
                    name: "exec".into(),
                    args: serde_json::json!({}),
                }),
                function_response: None,
                inline_data: None,
            }]),
        ];
        ledger.restore("sess", "gemini-pro-agent", &mut contents);
        assert!(contents[1].parts[0].thought_signature.is_none());
        assert_eq!(
            contents[1].parts[1].thought_signature.as_deref(),
            Some("EqUrealSignature")
        );
        assert_eq!(
            contents[2].parts[0].thought_signature.as_deref(),
            Some("EqUnext")
        );
    }

    #[test]
    fn claude_signature_is_wrapped_once_for_google_and_unwrapped_for_the_client() {
        let raw = base64::engine::general_purpose::STANDARD
            .encode(b"claude-sig-payload-0123456789abcdef");
        let wrapped = prepare_upstream_signature("claude-opus-4-6-thinking", &raw);
        assert_ne!(wrapped, raw);
        assert_eq!(
            prepare_upstream_signature("claude-opus-4-6-thinking", &wrapped),
            wrapped
        );
        assert_eq!(
            signature_for_client("claude-opus-4-6-thinking", &wrapped),
            raw
        );
        assert_eq!(prepare_upstream_signature("gemini-pro-agent", &raw), raw);
    }

    #[test]
    fn gemini_tool_call_without_a_signature_gets_the_sentinel() {
        let mut contents = vec![ContentMessage::model(vec![
            ContentPart {
                text: Some("look".into()),
                thought: None,
                thought_signature: None,
                function_call: None,
                function_response: None,
                inline_data: None,
            },
            ContentPart {
                text: None,
                thought: None,
                thought_signature: None,
                function_call: Some(crate::protocol::FunctionCall {
                    id: Some("call_1".into()),
                    name: "exec".into(),
                    args: serde_json::json!({}),
                }),
                function_response: None,
                inline_data: None,
            },
            ContentPart {
                text: None,
                thought: None,
                thought_signature: None,
                function_call: Some(crate::protocol::FunctionCall {
                    id: Some("call_2".into()),
                    name: "read".into(),
                    args: serde_json::json!({}),
                }),
                function_response: None,
                inline_data: None,
            },
        ])];
        seal_model_turns("gemini-pro-agent", &mut contents);
        assert!(contents[0].parts[0].thought_signature.is_none());
        assert_eq!(
            contents[0].parts[1].thought_signature.as_deref(),
            Some(GEMINI_SIGNATURE_SENTINEL)
        );
        assert_eq!(
            contents[0].parts[2].thought_signature.as_deref(),
            Some(GEMINI_SIGNATURE_SENTINEL)
        );

        contents[0].parts[0].thought_signature = Some("EqUon-the-text-by-mistake".into());
        contents[0].parts[1].thought_signature = None;
        contents[0].parts[2].thought_signature = None;
        seal_model_turns("gemini-pro-agent", &mut contents);
        assert!(contents[0].parts[0].thought_signature.is_none());
        assert_eq!(
            contents[0].parts[1].thought_signature.as_deref(),
            Some("EqUon-the-text-by-mistake")
        );
        assert_eq!(
            contents[0].parts[2].thought_signature.as_deref(),
            Some(GEMINI_SIGNATURE_SENTINEL)
        );
    }
}
