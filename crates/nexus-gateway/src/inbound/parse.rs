//! 三种方言 → [`ChatRequest`]。
//!
//! 方言在「一条消息里能放什么」上差异很大，但落到我们关心的只有五样：角色、文本、图片、
//! 工具调用、工具结果。解析器的职责就是从各自的结构里把这五样挖出来。

use super::Dialect;
use crate::normalized::{
    ChatRequest, ImageInput, Message, Role, Sampling, ToolCall, ToolChoice, ToolDef, ToolResult,
};
use serde_json::{json, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// 没有任何可发的消息。
    NoMessages,
    /// 客户端给了图片链接。Cursor 这条路把图塞进 protobuf，只认字节；静默丢掉的话客户端
    /// 以为模型看了图，而它答的是一段凭空的话——所以诚实拒绝。
    ImageUrlUnsupported(String),
    /// 客户端指望我们替它记着上一轮（`previous_response_id` / `item_reference`），
    /// 而我们是无状态转发，补不了。静默忽略是最坏的选择：模型每轮都只看到最新的
    /// 输入、看不到自己上一轮说过什么，于是把同一段开场白一遍遍重说——看起来像
    /// 模型犯病，实际是我们把它的记忆吃掉了。宁可明确报错，把「怎么改」写在错误里。
    MissingHistory(String),
}

impl ParseError {
    pub fn status(&self) -> u16 {
        400
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::NoMessages => write!(f, "messages required"),
            ParseError::ImageUrlUnsupported(u) => write!(
                f,
                "只接受内联图片（base64 data URL），不支持图片链接：{}",
                u.chars().take(80).collect::<String>()
            ),
            ParseError::MissingHistory(m) => write!(f, "{m}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRequest {
    /// `request.tools` 就是客户端自己声明过的工具（含语法 / 命名空间元数据）。回程只能给它
    /// 这些——它不认识的调用既执行不了也带不回结果。
    pub request: ChatRequest,
    pub stream: bool,
}

/// 把内容块（字符串 / 数组 / 对象）压成纯文本。各协议的 content 形态都能进来。
pub fn content_to_text(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => {
                    if let Some(Value::String(t)) = o.get("text") {
                        Some(t.clone())
                    } else if o.contains_key("content") {
                        // tool_result 的 content 可能又是一层数组，递归穿透。
                        Some(content_to_text(o.get("content")))
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(o)) => {
            if let Some(Value::String(t)) = o.get("text") {
                t.clone()
            } else if o.contains_key("content") {
                content_to_text(o.get("content"))
            } else {
                String::new()
            }
        }
        Some(_) => String::new(),
    }
}

/// 收窄成四种角色。`developer`（Responses / 新 OpenAI SDK 的系统提示）算 system；认不出的当 user。
fn normalize_role(raw: Option<&Value>) -> Role {
    match raw
        .and_then(Value::as_str)
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("system") | Some("developer") => Role::System,
        Some("assistant") => Role::Assistant,
        Some("tool") => Role::Tool,
        _ => Role::User,
    }
}

fn str_of(v: Option<&Value>) -> String {
    v.and_then(Value::as_str).unwrap_or("").to_string()
}

/// 一个图片引用能长成什么样：data URL 收下，http(s) 链接是错误（见 [`ParseError`]），其余忽略。
fn image_from(url: &str, media_type: Option<&str>) -> Result<Option<ImageInput>, ParseError> {
    let url = url.trim();
    if url.is_empty() {
        return Ok(None);
    }
    if let Some(rest) = url.strip_prefix("data:") {
        if let Some((meta, data)) = rest.split_once(",") {
            if meta.ends_with(";base64") {
                let mime = meta.trim_end_matches(";base64");
                return Ok(Some(ImageInput {
                    data: data.to_string(),
                    mime_type: if mime.is_empty() {
                        media_type.unwrap_or("image/png").to_string()
                    } else {
                        mime.to_string()
                    },
                }));
            }
        }
        return Ok(None);
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return Err(ParseError::ImageUrlUnsupported(url.to_string()));
    }
    Ok(None)
}

/// `image_url` 既可能是 `{ url }` 也可能是裸字符串。
fn url_of(field: Option<&Value>) -> Option<&str> {
    match field {
        Some(Value::String(s)) => Some(s),
        Some(Value::Object(o)) => o.get("url").and_then(Value::as_str),
        _ => None,
    }
}

fn extract_images(content: Option<&Value>) -> Result<Vec<ImageInput>, ParseError> {
    let Some(Value::Array(parts)) = content else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for p in parts {
        let Value::Object(part) = p else { continue };
        match part.get("type").and_then(Value::as_str) {
            // OpenAI Chat：{ type: "image_url", image_url: { url } }；Responses：input_image
            Some("image_url") | Some("input_image") => {
                if let Some(u) = url_of(part.get("image_url")) {
                    if let Some(img) = image_from(u, None)? {
                        out.push(img);
                    }
                }
            }
            // Anthropic：{ type: "image", source: { type: "base64" | "url", media_type, data | url } }
            Some("image") => {
                let src = part.get("source").and_then(Value::as_object);
                let Some(src) = src else { continue };
                let media = src.get("media_type").and_then(Value::as_str);
                if src.get("type").and_then(Value::as_str) == Some("base64") {
                    let data = str_of(src.get("data"));
                    if !data.is_empty() {
                        out.push(ImageInput {
                            data,
                            mime_type: media.unwrap_or("image/png").to_string(),
                        });
                    }
                } else if let Some(u) = src.get("url").and_then(Value::as_str) {
                    if let Some(img) = image_from(u, media)? {
                        out.push(img);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

/// 摘掉 tool_use / tool_result 块再取纯文本——否则 tool_result 那种整个会话里最大的一块
/// 会被当成正文重复一遍，input token 直接翻倍。
fn strip_tool_parts(content: Option<&Value>) -> Option<Value> {
    match content {
        Some(Value::Array(parts)) => Some(Value::Array(
            parts
                .iter()
                .filter(|p| {
                    !matches!(
                        p.get("type").and_then(Value::as_str),
                        Some("tool_use") | Some("tool_result")
                    )
                })
                .cloned()
                .collect(),
        )),
        other => other.cloned(),
    }
}

fn arguments_of(fn_obj: Option<&Value>, call: &Value) -> String {
    let args = fn_obj
        .and_then(|f| f.get("arguments"))
        .or_else(|| call.get("arguments"));
    match args {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Null) | None => "{}".to_string(),
        Some(v) => v.to_string(),
    }
}

/// 一条方言消息 → 一到两条统一消息。
///
/// 拆成两条的情形：Anthropic 把 tool_result 放在 **user** 消息里（和后续的用户正文同一条）。
/// 上游要的是 role=TOOL 承载结果、role=USER 承载正文，所以这里先拆开；否则结果会跟着一条
/// 「没正文的 user」被整条丢掉——gateway 那份 JS 正是这么丢的。
fn parse_message(raw: &Value) -> Result<Vec<Message>, ParseError> {
    let m = raw.as_object().cloned().unwrap_or_default();
    let role = normalize_role(m.get("role"));
    let content = m.get("content").or_else(|| m.get("text"));
    let parts: Vec<Value> = content
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut tool_calls = Vec::new();
    if let Some(Value::Array(calls)) = m.get("tool_calls") {
        for c in calls {
            let f = c.get("function");
            tool_calls.push(ToolCall {
                id: str_of(c.get("id")),
                name: {
                    let n = str_of(f.and_then(|f| f.get("name")));
                    if n.is_empty() {
                        str_of(c.get("name"))
                    } else {
                        n
                    }
                },
                arguments: arguments_of(f, c),
            });
        }
    }
    for p in &parts {
        if p.get("type").and_then(Value::as_str) == Some("tool_use") {
            tool_calls.push(ToolCall {
                id: str_of(p.get("id")),
                name: str_of(p.get("name")),
                arguments: p
                    .get("input")
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "{}".into()),
            });
        }
    }

    let mut tool_results = Vec::new();
    if role == Role::Tool {
        tool_results.push(ToolResult {
            tool_call_id: str_of(m.get("tool_call_id")),
            tool_name: str_of(m.get("name")),
            text: content_to_text(content),
            is_error: false,
        });
    }
    for p in &parts {
        if p.get("type").and_then(Value::as_str) == Some("tool_result") {
            tool_results.push(ToolResult {
                tool_call_id: str_of(p.get("tool_use_id")),
                tool_name: String::new(),
                text: content_to_text(p.get("content")),
                is_error: p.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            });
        }
    }

    let text = if role == Role::Tool {
        String::new()
    } else {
        content_to_text(strip_tool_parts(content).as_ref())
    };
    let images = extract_images(content)?;

    let mut out = Vec::new();
    if role == Role::Tool {
        out.push(Message {
            role: Role::Tool,
            text: String::new(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_results,
        });
        return Ok(out);
    }
    if !tool_results.is_empty() {
        out.push(Message {
            role: Role::Tool,
            text: String::new(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_results,
        });
    }
    let has_body = !text.trim().is_empty() || !images.is_empty() || !tool_calls.is_empty();
    // 只承载 tool_result 的 user 消息拆完就没剩下什么了；system 即便为空也留着（无害，
    // 上游映射会过滤），其余空消息不发。
    if has_body || role == Role::System {
        out.push(Message {
            role,
            text,
            images,
            tool_calls: if role == Role::Assistant {
                tool_calls
            } else {
                Vec::new()
            },
            tool_results: Vec::new(),
        });
    }
    Ok(out)
}

fn parse_chat_like_messages(body: &Value) -> Result<Vec<Message>, ParseError> {
    let mut out = Vec::new();
    // Anthropic 把 system 放顶层，OpenAI 放 messages 里。两种都收。
    let sys = content_to_text(body.get("system"));
    if !sys.is_empty() {
        out.push(Message::text(Role::System, sys));
    }
    if let Some(Value::Array(list)) = body.get("messages") {
        for m in list {
            out.extend(parse_message(m)?);
        }
    }
    Ok(out)
}

fn parse_messages(dialect: Dialect, body: &Value) -> Result<Vec<Message>, ParseError> {
    match dialect {
        Dialect::OpenAiResponses => parse_responses_input(body),
        Dialect::OpenAiChat | Dialect::AnthropicMessages => parse_chat_like_messages(body),
    }
}

/// Responses 的 `input` 是一个异构数组：字符串、消息、函数调用、函数结果、reasoning
/// 块混在一起。工具流量在这里是独立的 input item 而不是消息内容——落到
/// `content_to_text` 会变成空串，于是每一次工具调用和每一个工具结果都被静默丢掉。
fn parse_responses_input(body: &Value) -> Result<Vec<Message>, ParseError> {
    let mut out = Vec::new();
    let instr = content_to_text(body.get("instructions"));
    if !instr.is_empty() {
        out.push(Message::text(Role::System, instr));
    }

    match body.get("input") {
        Some(Value::String(s)) => {
            if !s.trim().is_empty() {
                out.push(Message::text(Role::User, s.clone()));
            }
            return Ok(out);
        }
        Some(Value::Array(items)) => {
            for it in items {
                out.extend(parse_responses_item(it)?);
            }
            return Ok(out);
        }
        _ => {}
    }

    // 有些客户端对 /responses 也发 messages，按 chat 格式收下。这条分支直接返回自己
    // 拼出来的结果，不接着用上面已经塞了 instr 的 out——避免 system 消息重复一遍。
    if let Some(Value::Array(_)) = body.get("messages") {
        let synthetic = json!({
            "system": body.get("instructions").cloned().unwrap_or(Value::Null),
            "messages": body.get("messages").cloned().unwrap_or(Value::Null),
        });
        return parse_chat_like_messages(&synthetic);
    }
    Ok(out)
}

/// 一个 Responses input item → 零到一条统一消息。
fn parse_responses_item(it: &Value) -> Result<Vec<Message>, ParseError> {
    if let Value::String(s) = it {
        if s.is_empty() {
            return Ok(Vec::new());
        }
        return Ok(vec![Message::text(Role::User, s.clone())]);
    }
    let Some(o) = it.as_object() else {
        return Ok(Vec::new());
    };
    let t = o.get("type").and_then(Value::as_str).unwrap_or("");

    // reasoning 是上游自己的思考记录，回传给它没有意义；additional_tools 是工具声明，
    // 由 parse_tools 处理；item_reference 指向只存在服务端的历史条目，我们是无状态
    // 转发拿不到——parse_request 会在更早的地方就直接拒收带 item_reference 的请求，
    // 这里跳过是留给「万一没被拒收」的兜底。
    if matches!(t, "additional_tools" | "reasoning" | "item_reference") {
        return Ok(Vec::new());
    }

    let call_id = || -> String {
        let c = str_of(o.get("call_id"));
        if c.is_empty() {
            str_of(o.get("id"))
        } else {
            c
        }
    };

    if t == "function_call" || t == "custom_tool_call" {
        let arguments = if t == "custom_tool_call" {
            // 语法工具没有 JSON 参数，只有一段原文；用声明时套的壳（见 parse_tools）
            // 反着包一层，序列化到上游的形状就和普通函数调用一致了。
            let input = o
                .get("input")
                .cloned()
                .unwrap_or_else(|| Value::String(String::new()));
            json!({ "input": input }).to_string()
        } else {
            arguments_of(None, it)
        };
        return Ok(vec![Message {
            role: Role::Assistant,
            text: String::new(),
            images: Vec::new(),
            tool_calls: vec![ToolCall {
                id: call_id(),
                name: str_of(o.get("name")),
                arguments,
            }],
            tool_results: Vec::new(),
        }]);
    }

    if t == "function_call_output" || t == "custom_tool_call_output" {
        let text = content_to_text(o.get("output").or_else(|| o.get("result")));
        return Ok(vec![Message {
            role: Role::Tool,
            text: String::new(),
            images: Vec::new(),
            tool_calls: Vec::new(),
            tool_results: vec![ToolResult {
                tool_call_id: call_id(),
                tool_name: String::new(),
                text,
                is_error: false,
            }],
        }]);
    }

    // 图片有时不裹在 message 里，直接就是一个 input item。
    if matches!(t, "input_image" | "image_url" | "image") {
        let imgs = extract_images(Some(&Value::Array(vec![it.clone()])))?;
        return Ok(match imgs.into_iter().next() {
            Some(img) => vec![Message {
                role: Role::User,
                text: String::new(),
                images: vec![img],
                tool_calls: Vec::new(),
                tool_results: Vec::new(),
            }],
            None => Vec::new(),
        });
    }

    // 剩下走通用 message 形态：`{ type: "message", role, content }` 或裸的
    // `{ role, content }`（Responses 的 input item 常常没有 type 字段）。
    let content = o.get("content").or_else(|| o.get("text"));
    let text = content_to_text(content);
    let images = extract_images(content)?;
    if !text.is_empty() || !images.is_empty() {
        return Ok(vec![Message {
            role: normalize_role(o.get("role")),
            text,
            images,
            tool_calls: Vec::new(),
            tool_results: Vec::new(),
        }]);
    }

    // 剩下的是我们不认识的 item：托管工具调用（web_search_call、computer_call…）之类。
    // 静默吞掉会让模型以为那件事没发生过，但总比塞一条空消息把上游整轮请求送 400 强，
    // 至少打日志留个痕迹。
    if !t.is_empty() {
        tracing::warn!(item_type = %t, "Responses input 里有不认识的 item，已跳过");
    }
    Ok(Vec::new())
}

/// 客户端指望我们替它记着上一轮，而我们记不住。
///
/// Responses 的服务端状态（`store: true` + `previous_response_id`）意味着客户端只发
/// 这一轮的新内容，历史由服务端补齐；`item_reference` 更直接，指向一条只存在于
/// 服务端的历史条目。我们是无状态转发，两样都补不了。
///
/// 判据是历史在不在，不是字段在不在：有些客户端两样都发（带上 id 同时也带全历史），
/// 那种情况下这个字段无害，忽略就好。
fn missing_history_message(dialect: Dialect, body: &Value) -> Option<String> {
    if dialect != Dialect::OpenAiResponses {
        return None;
    }
    let tail =
        "本服务是无状态转发，不保存会话状态。请改为 store: false 并在 input 里带上完整对话历史\
（Codex 默认就是这样）——否则模型每轮都看不到自己上一轮的输出，会把同一段话反复重说。";
    let input = body.get("input").and_then(Value::as_array);

    if let Some(items) = input {
        if items
            .iter()
            .any(|it| it.get("type").and_then(Value::as_str) == Some("item_reference"))
        {
            return Some(format!(
                "input 里用了 item_reference 引用服务端保存的历史条目。{tail}"
            ));
        }
    }

    let prev = body
        .get("previous_response_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if prev.is_empty() {
        return None;
    }

    let has_history = input
        .map(|items| {
            items.iter().any(|it| {
                let o = it.as_object();
                let t = o.and_then(|o| o.get("type")).and_then(Value::as_str);
                if matches!(
                    t,
                    Some("function_call") | Some("custom_tool_call") | Some("reasoning")
                ) {
                    return true;
                }
                o.and_then(|o| o.get("role")).and_then(Value::as_str) == Some("assistant")
            })
        })
        .unwrap_or(false);
    if has_history {
        return None;
    }

    Some(format!(
        "previous_response_id 需要服务端保存会话状态。{tail}"
    ))
}

/// 语法工具声明给上游时套的壳。
///
/// `type: "custom"` 的工具带的是一份语法定义（Codex 的 exec 用 lark），不是 JSON Schema，
/// 而上游一律只认 Schema。用一个自由文本字段把它放过去，回程由序列化层拆壳还原成
/// `custom_tool_call`（`ToolDef::grammar`）。怎么写由工具自己的 description 约定——那段
/// 文字本来就是给模型看的。
fn grammar_tool_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "input": {
                "type": "string",
                "description": "Raw tool input, written exactly in the syntax this tool documents.",
            }
        },
        "required": ["input"],
    })
}

/// Responses 的默认命名空间。Codex 的 `ToolName::is_default_namespace` 也这么判；
/// 落在这里的工具回程不带 namespace 字段，和 OpenAI 官方行为一致。
const DEFAULT_TOOL_NAMESPACE: &str = "functions";

fn parse_tools(dialect: Dialect, body: &Value) -> Vec<ToolDef> {
    let mut out: Vec<ToolDef> = Vec::new();
    // 工具名 → 声明它的命名空间（只为重名时把两边都写进日志）。
    let mut seen: HashMap<String, String> = HashMap::new();

    fn walk(
        list: Option<&Value>,
        ns: &str,
        out: &mut Vec<ToolDef>,
        seen: &mut HashMap<String, String>,
    ) {
        let Some(Value::Array(items)) = list else {
            return;
        };
        for t in items {
            let Value::Object(t) = t else { continue };
            // 命名空间壳：{ type: "namespace", name, tools: [...] }。真东西在下一层，
            // 但壳的名字要带下去——回程的调用得原样带回这个命名空间。
            if t.get("tools").map(Value::is_array).unwrap_or(false) {
                let inner = t
                    .get("name")
                    .or_else(|| t.get("namespace"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(ns);
                walk(t.get("tools"), inner, out, seen);
                continue;
            }
            let (name, description, parameters, grammar) =
                match (t.get("type").and_then(Value::as_str), t.get("function")) {
                    // OpenAI Chat：{ type: "function", function: { name, description, parameters } }
                    (Some("function"), Some(f)) => (
                        str_of(f.get("name")),
                        str_of(f.get("description")),
                        f.get("parameters").cloned(),
                        false,
                    ),
                    // Responses 的语法工具：参数形状由 grammar_tool_schema 顶上。
                    (Some("custom"), _) => (
                        str_of(t.get("name")),
                        str_of(t.get("description")),
                        Some(grammar_tool_schema()),
                        true,
                    ),
                    // Responses 扁平形态 / Anthropic：{ name, description, parameters | input_schema }
                    _ => (
                        str_of(t.get("name")),
                        str_of(t.get("description")),
                        t.get("input_schema")
                            .or_else(|| t.get("parameters"))
                            .cloned(),
                        false,
                    ),
                };
            let name = name.trim().to_string();
            if name.is_empty() {
                continue;
            }
            // 同名只留第一个：原样发上去上游直接 400「duplicate function name」，整轮废掉。
            // 但不能悄悄留——被吞的那个模型永远调不到，表现成「时好时坏」。
            if let Some(prev) = seen.insert(name.clone(), ns.to_string()) {
                tracing::warn!(
                    tool = %name,
                    first = %if prev.is_empty() { "顶层" } else { prev.as_str() },
                    dropped = %if ns.is_empty() { "顶层" } else { ns },
                    "工具重名，已丢弃后一个"
                );
                continue;
            }
            out.push(ToolDef {
                name,
                description,
                parameters: match parameters {
                    Some(v @ Value::Object(_)) => v,
                    _ => Value::Object(Default::default()),
                },
                grammar,
                namespace: if ns.is_empty() || ns == DEFAULT_TOOL_NAMESPACE {
                    None
                } else {
                    Some(ns.to_string())
                },
            });
        }
    }

    walk(body.get("tools"), "", &mut out, &mut seen);

    // Responses 把额外工具塞在 input 里，别漏。Codex 只在这里声明工具，顶层 tools 是空的。
    if dialect == Dialect::OpenAiResponses {
        if let Some(Value::Array(items)) = body.get("input") {
            for it in items {
                if it.get("type").and_then(Value::as_str) == Some("additional_tools") {
                    walk(it.get("tools"), "", &mut out, &mut seen);
                }
            }
        }
    }
    out
}

/// Anthropic 是 `{ type: any | tool | auto | none, name }`，OpenAI 是字符串或
/// `{ type: function, function: { name } }`。指名的要校验确实声明过——指一个上游不认识的
/// 函数会换来 400，而这轮本来只要退化成 auto 就能跑完。
fn parse_tool_choice(body: &Value, tools: &[ToolDef]) -> ToolChoice {
    let named = |name: Option<&Value>| -> ToolChoice {
        let n = str_of(name).trim().to_string();
        if !n.is_empty() && tools.iter().any(|t| t.name == n) {
            return ToolChoice::Tool(n);
        }
        if !n.is_empty() {
            tracing::warn!(tool = %n, "tool_choice 指定了没声明的工具，退化成 auto");
        }
        ToolChoice::Auto
    };
    match body.get("tool_choice") {
        None | Some(Value::Null) => ToolChoice::Auto,
        Some(Value::String(s)) => match s.as_str() {
            "none" => ToolChoice::None,
            "required" | "any" => ToolChoice::Required,
            "auto" => ToolChoice::Auto,
            // 有客户端把工具名直接当字符串给。
            other => named(Some(&Value::String(other.to_string()))),
        },
        Some(Value::Object(o)) => match o.get("type").and_then(Value::as_str) {
            Some("none") => ToolChoice::None,
            Some("any") | Some("required") => ToolChoice::Required,
            Some("auto") => ToolChoice::Auto,
            Some("tool") | Some("function") | Some("custom") => named(
                o.get("name")
                    .or_else(|| o.get("function").and_then(|f| f.get("name"))),
            ),
            _ => ToolChoice::Auto,
        },
        _ => ToolChoice::Auto,
    }
}

fn parse_sampling(dialect: Dialect, body: &Value) -> Sampling {
    let raw_stop = match dialect {
        Dialect::AnthropicMessages => body.get("stop_sequences"),
        Dialect::OpenAiChat | Dialect::OpenAiResponses => body.get("stop"),
    };
    let stop_sequences = match raw_stop {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    };
    let max_output_tokens = ["max_output_tokens", "max_tokens", "max_completion_tokens"]
        .iter()
        .find_map(|k| body.get(*k).and_then(Value::as_f64))
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| n as u32);
    let f32_of = |k: &str| body.get(k).and_then(Value::as_f64).map(|n| n as f32);
    Sampling {
        max_output_tokens,
        temperature: f32_of("temperature"),
        top_p: f32_of("top_p"),
        stop_sequences,
    }
}

/// 上游对话缓存的键。优先客户端自己给的；都没有就对前两条有内容的 user 消息求哈希——
/// 同一会话内这两条不变（第一条是开场，第二条在首轮就定型）。只哈希第一条不够：
/// Claude Code 每个会话首条 user 是同一段固定的 caveat，全部会话会算出同一个键。
/// **必须带上模型**：缓存按（账号 × 模型 × 对话）存，换了模型本来就命不中。
fn conversation_key(
    body: &Value,
    explicit: Option<&str>,
    messages: &[Message],
    model: &str,
) -> String {
    let given = explicit
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            ["conversation_id", "previous_response_id", "user"]
                .iter()
                .find_map(|k| body.get(*k).and_then(Value::as_str))
                .map(str::to_string)
                .filter(|s| !s.is_empty())
        });
    if let Some(g) = given {
        return format!("{model}:{g}");
    }
    let seed = messages
        .iter()
        .filter(|m| m.role == Role::User && !m.text.trim().is_empty())
        .take(2)
        .map(|m| m.text.as_str())
        .collect::<Vec<_>>()
        .join("\n\u{0}\n");
    let seed: String = seed.chars().take(4096).collect();
    let h = crate::identity::sha256_hex(seed.as_bytes());
    format!("{model}:{}", &h[..16])
}

/// 解析一次请求。`explicit_conversation` 来自 `x-conversation-id` 头。
pub fn parse_request(
    dialect: Dialect,
    body: &Value,
    explicit_conversation: Option<&str>,
) -> Result<ParsedRequest, ParseError> {
    if let Some(m) = missing_history_message(dialect, body) {
        return Err(ParseError::MissingHistory(m));
    }
    let messages = parse_messages(dialect, body)?;
    if !messages
        .iter()
        .any(|m| m.role != Role::System || !m.text.trim().is_empty())
    {
        return Err(ParseError::NoMessages);
    }
    let tools = parse_tools(dialect, body);
    let tool_choice = parse_tool_choice(body, &tools);
    let sampling = parse_sampling(dialect, body);
    let model = str_of(body.get("model")).trim().to_string();
    let conversation_id = conversation_key(body, explicit_conversation, &messages, &model);
    Ok(ParsedRequest {
        request: ChatRequest {
            model,
            messages,
            tools,
            tool_choice,
            sampling,
            conversation_id: Some(conversation_id),
            // 由 server 决定要不要附原始体（只有 ChatGPT 通道的 Responses 请求要）与客户端头。
            raw_responses: None,
            client_headers: Default::default(),
        },
        stream: body.get("stream").and_then(Value::as_bool).unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn openai(body: Value) -> ParsedRequest {
        parse_request(Dialect::OpenAiChat, &body, None).unwrap()
    }
    fn anthropic(body: Value) -> ParsedRequest {
        parse_request(Dialect::AnthropicMessages, &body, None).unwrap()
    }
    fn responses(body: Value) -> ParsedRequest {
        parse_request(Dialect::OpenAiResponses, &body, None).unwrap()
    }

    #[test]
    fn content_to_text_flattens_every_shape() {
        assert_eq!(content_to_text(Some(&json!("hi"))), "hi");
        assert_eq!(
            content_to_text(Some(
                &json!([{"type":"text","text":"a"},{"type":"text","text":"b"}])
            )),
            "a\nb"
        );
        assert_eq!(
            content_to_text(Some(
                &json!([{"type":"tool_result","content":[{"type":"text","text":"inner"}]}])
            )),
            "inner"
        );
        assert_eq!(content_to_text(Some(&json!({"text":"obj"}))), "obj");
        assert_eq!(content_to_text(None), "");
        assert_eq!(content_to_text(Some(&json!(42))), "");
    }

    #[test]
    fn openai_chat_basics() {
        let p = openai(json!({
            "model": " gpt-5.6-sol ",
            "messages": [
                {"role":"system","content":"be brief"},
                {"role":"developer","content":"more rules"},
                {"role":"user","content":[{"type":"text","text":"hi"}]},
            ],
            "stream": true,
            "max_completion_tokens": 100,
            "temperature": 0.3,
            "stop": "END"
        }));
        assert!(p.stream);
        assert_eq!(p.request.model, "gpt-5.6-sol");
        assert_eq!(p.request.messages.len(), 3);
        assert_eq!(
            p.request.messages[1].role,
            Role::System,
            "developer 算 system"
        );
        assert_eq!(p.request.messages[2].text, "hi");
        assert_eq!(p.request.sampling.max_output_tokens, Some(100));
        assert_eq!(p.request.sampling.temperature, Some(0.3));
        assert_eq!(p.request.sampling.stop_sequences, vec!["END".to_string()]);
    }

    #[test]
    fn openai_tool_calls_and_tool_results() {
        let p = openai(json!({
            "messages": [
                {"role":"user","content":"weather?"},
                {"role":"assistant","content":null,"tool_calls":[
                    {"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Paris\"}"}}
                ]},
                {"role":"tool","tool_call_id":"call_1","name":"get_weather","content":"sunny"},
            ],
            "tools": [{"type":"function","function":{"name":"get_weather","description":"d","parameters":{"type":"object"}}}],
            "tool_choice": {"type":"function","function":{"name":"get_weather"}}
        }));
        let m = &p.request.messages;
        assert_eq!(m[1].role, Role::Assistant);
        assert_eq!(m[1].tool_calls[0].arguments, r#"{"city":"Paris"}"#);
        assert_eq!(m[2].role, Role::Tool);
        assert_eq!(m[2].tool_results[0].tool_call_id, "call_1");
        assert_eq!(m[2].tool_results[0].text, "sunny");
        assert_eq!(p.request.tools.len(), 1);
        assert_eq!(
            p.request.tool_choice,
            ToolChoice::Tool("get_weather".into())
        );
        assert_eq!(p.request.tools[0].name, "get_weather");
        assert!(!p.request.tools[0].grammar);
        assert_eq!(p.request.tools[0].namespace, None);
    }

    #[test]
    fn anthropic_tool_result_inside_a_user_message_is_split_out_as_a_tool_message() {
        // Claude Code 的形状：tool_result 和下一句用户正文放在同一条 user 里。
        let p = anthropic(json!({
            "system": "sys",
            "messages": [
                {"role":"user","content":"read the file"},
                {"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"read","input":{"path":"a.txt"}}]},
                {"role":"user","content":[
                    {"type":"tool_result","tool_use_id":"toolu_1","content":[{"type":"text","text":"file body"}],"is_error":false},
                    {"type":"text","text":"now summarize"}
                ]}
            ],
            "tools":[{"name":"read","description":"","input_schema":{"type":"object"}}],
            "max_tokens": 64
        }));
        let m = &p.request.messages;
        assert_eq!(m[0].role, Role::System);
        assert_eq!(m[2].role, Role::Assistant);
        assert_eq!(m[2].tool_calls[0].arguments, r#"{"path":"a.txt"}"#);
        assert_eq!(m[3].role, Role::Tool, "tool_result 拆成独立 tool 消息");
        assert_eq!(m[3].tool_results[0].tool_call_id, "toolu_1");
        assert_eq!(m[3].tool_results[0].text, "file body");
        assert_eq!(m[4].role, Role::User);
        assert_eq!(m[4].text, "now summarize", "正文不带 tool_result 的内容");
        assert_eq!(p.request.sampling.max_output_tokens, Some(64));
    }

    #[test]
    fn a_user_message_that_is_only_tool_results_yields_only_a_tool_message() {
        let p = anthropic(json!({
            "messages": [
                {"role":"user","content":"x"},
                {"role":"assistant","content":[{"type":"tool_use","id":"t","name":"f","input":{}}]},
                {"role":"user","content":[{"type":"tool_result","tool_use_id":"t","content":"ok"}]}
            ]
        }));
        let m = &p.request.messages;
        assert_eq!(m.len(), 3);
        assert_eq!(m[2].role, Role::Tool);
    }

    #[test]
    fn images_are_taken_from_all_three_shapes_and_urls_are_rejected() {
        let p = openai(json!({"messages":[{"role":"user","content":[
            {"type":"text","text":"see"},
            {"type":"image_url","image_url":{"url":"data:image/jpeg;base64,AAAA"}},
            {"type":"image_url","image_url":"data:image/png;base64,BBBB"},
            {"type":"image","source":{"type":"base64","media_type":"image/webp","data":"CCCC"}}
        ]}]}));
        let imgs = &p.request.messages[0].images;
        assert_eq!(imgs.len(), 3);
        assert_eq!(
            (imgs[0].mime_type.as_str(), imgs[0].data.as_str()),
            ("image/jpeg", "AAAA")
        );
        assert_eq!(imgs[1].mime_type, "image/png");
        assert_eq!(imgs[2].mime_type, "image/webp");

        let err = parse_request(
            Dialect::OpenAiChat,
            &json!({"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"https://x/y.png"}}]}]}),
            None,
        )
        .unwrap_err();
        assert!(matches!(err, ParseError::ImageUrlUnsupported(_)));
    }

    #[test]
    fn tools_dedupe_by_name_and_walk_namespaces() {
        let p = openai(json!({
            "messages":[{"role":"user","content":"x"}],
            "tools":[
                {"type":"function","function":{"name":"a","parameters":{"type":"object"}}},
                {"name":"ns","tools":[{"type":"function","name":"b","parameters":{"type":"object"}}]},
                {"type":"function","function":{"name":"a","parameters":{}}},
                {"type":"function","function":{"name":"   "}}
            ]
        }));
        let names: Vec<_> = p.request.tools.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
        assert!(p.request.tools[1].parameters.is_object());
    }

    #[test]
    fn tool_choice_variants() {
        let base = json!({"messages":[{"role":"user","content":"x"}],"tools":[{"name":"f","input_schema":{}}]});
        let with = |tc: Value| {
            let mut b = base.clone();
            b["tool_choice"] = tc;
            anthropic(b).request.tool_choice
        };
        assert_eq!(with(json!("none")), ToolChoice::None);
        assert_eq!(with(json!("required")), ToolChoice::Required);
        assert_eq!(with(json!({"type":"any"})), ToolChoice::Required);
        assert_eq!(
            with(json!({"type":"tool","name":"f"})),
            ToolChoice::Tool("f".into())
        );
        assert_eq!(
            with(json!({"type":"tool","name":"zzz"})),
            ToolChoice::Auto,
            "没声明的退化成 auto"
        );
        assert_eq!(
            with(json!("f")),
            ToolChoice::Tool("f".into()),
            "裸工具名也认"
        );
        assert_eq!(with(json!({"type":"allowed_tools"})), ToolChoice::Auto);
    }

    #[test]
    fn anthropic_uses_stop_sequences_and_openai_uses_stop() {
        let a = anthropic(
            json!({"messages":[{"role":"user","content":"x"}],"stop_sequences":["A","B"],"stop":"ignored"}),
        );
        assert_eq!(
            a.request.sampling.stop_sequences,
            vec!["A".to_string(), "B".to_string()]
        );
        let o = openai(
            json!({"messages":[{"role":"user","content":"x"}],"stop":["C"],"stop_sequences":["ignored"]}),
        );
        assert_eq!(o.request.sampling.stop_sequences, vec!["C".to_string()]);
    }

    #[test]
    fn empty_requests_are_rejected() {
        assert_eq!(
            parse_request(Dialect::OpenAiChat, &json!({"messages":[]}), None).unwrap_err(),
            ParseError::NoMessages
        );
        assert_eq!(
            parse_request(
                Dialect::OpenAiChat,
                &json!({"messages":[{"role":"system","content":""}]}),
                None
            )
            .unwrap_err(),
            ParseError::NoMessages
        );
        assert!(parse_request(
            Dialect::OpenAiChat,
            &json!({"messages":[{"role":"system","content":"only sys"}]}),
            None
        )
        .is_ok());
    }

    #[test]
    fn conversation_key_is_stable_across_turns_and_scoped_by_model() {
        let turn1 = json!({"model":"m1","messages":[
            {"role":"user","content":"caveat text"},{"role":"user","content":"real question"}]});
        let turn2 = json!({"model":"m1","messages":[
            {"role":"user","content":"caveat text"},{"role":"user","content":"real question"},
            {"role":"assistant","content":"answer"},{"role":"user","content":"follow up"}]});
        let k1 = openai(turn1.clone()).request.conversation_id.unwrap();
        let k2 = openai(turn2).request.conversation_id.unwrap();
        assert_eq!(k1, k2, "同一会话跨轮键不变");
        let mut other_model = turn1.clone();
        other_model["model"] = json!("m2");
        assert_ne!(k1, openai(other_model).request.conversation_id.unwrap());
        let explicit = parse_request(Dialect::OpenAiChat, &turn1, Some("conv-9")).unwrap();
        assert_eq!(
            explicit.request.conversation_id.as_deref(),
            Some("m1:conv-9")
        );
    }

    #[test]
    fn responses_string_input_and_instructions() {
        let p = responses(json!({
            "model": "gpt-5-codex",
            "instructions": "be terse",
            "input": "hello there",
        }));
        assert_eq!(p.request.messages.len(), 2);
        assert_eq!(p.request.messages[0].role, Role::System);
        assert_eq!(p.request.messages[0].text, "be terse");
        assert_eq!(p.request.messages[1].role, Role::User);
        assert_eq!(p.request.messages[1].text, "hello there");
    }

    #[test]
    fn responses_input_array_covers_every_item_shape() {
        let p = responses(json!({
            "instructions": "sys prompt",
            "input": [
                { "role": "user", "content": [{ "type": "input_text", "text": "read this file" }] },
                { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "thinking…" }] },
                { "type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{\"path\":\"a.txt\"}" },
                { "type": "function_call_output", "call_id": "call_1", "output": "file body" },
                { "type": "custom_tool_call", "call_id": "call_2", "name": "exec", "input": "ls -la" },
                { "type": "custom_tool_call_output", "call_id": "call_2", "output": "total 0" },
                { "type": "input_image", "image_url": "data:image/png;base64,AAAA" },
                { "type": "additional_tools", "tools": [{ "type": "function", "name": "web_search", "parameters": { "type": "object" } }] },
                { "type": "web_search_call", "id": "ws_1" },
            ],
        }));
        let m = &p.request.messages;
        // system(instructions) + user文本 + function_call + function_call_output
        // + custom_tool_call + custom_tool_call_output + input_image；reasoning /
        // additional_tools / 不认识的 web_search_call 都不落地成消息。
        assert_eq!(m.len(), 7, "{m:?}");
        assert_eq!(m[0].role, Role::System);
        assert_eq!(m[0].text, "sys prompt");
        assert_eq!(m[1].role, Role::User);
        assert_eq!(m[1].text, "read this file");
        assert_eq!(m[2].role, Role::Assistant);
        assert_eq!(m[2].tool_calls[0].id, "call_1");
        assert_eq!(m[2].tool_calls[0].name, "read");
        assert_eq!(m[2].tool_calls[0].arguments, r#"{"path":"a.txt"}"#);
        assert_eq!(m[3].role, Role::Tool);
        assert_eq!(m[3].tool_results[0].tool_call_id, "call_1");
        assert_eq!(m[3].tool_results[0].text, "file body");
        assert_eq!(
            m[4].role,
            Role::Assistant,
            "语法工具调用也落成 assistant 消息"
        );
        assert_eq!(m[4].tool_calls[0].id, "call_2");
        assert_eq!(
            m[4].tool_calls[0].arguments, r#"{"input":"ls -la"}"#,
            "语法工具用 {{input}} 壳包住原文"
        );
        assert_eq!(m[5].role, Role::Tool);
        assert_eq!(m[5].tool_results[0].text, "total 0");
        assert_eq!(m[6].role, Role::User);
        assert_eq!(m[6].images[0].mime_type, "image/png");

        // additional_tools 里的工具声明要被 parse_tools 摊平进 request.tools。
        assert!(p.request.tools.iter().any(|t| t.name == "web_search"));
    }

    #[test]
    fn responses_records_grammar_and_namespace_on_declared_tools() {
        // Codex Desktop 的真实形状：exec 是 type:"custom" 的语法工具，和普通函数一起
        // 套在 additional_tools 的 namespace 壳里；collaboration 组的工具带非默认命名空间。
        let p = responses(json!({
            "input": [
                { "role": "user", "content": [{ "type": "input_text", "text": "hi" }] },
                { "type": "additional_tools", "tools": [
                    { "type": "namespace", "name": "functions", "tools": [
                        { "type": "custom", "name": "exec", "description": "run js",
                          "format": { "type": "grammar", "syntax": "lark", "definition": "start: /.*/" } },
                        { "type": "function", "name": "read_file", "parameters": { "type": "object" } },
                    ]},
                    { "type": "namespace", "name": "collaboration", "tools": [
                        { "type": "function", "name": "spawn_agent", "parameters": { "type": "object" } },
                    ]},
                ]},
            ],
            "tools": [
                { "type": "function", "name": "top_level", "parameters": { "type": "object" } },
            ],
        }));
        let by_name = |n: &str| p.request.tools.iter().find(|t| t.name == n).unwrap();

        let exec = by_name("exec");
        assert!(exec.grammar, "type:custom 记成语法工具");
        assert_eq!(exec.namespace, None, "默认命名空间 functions 不记");
        assert_eq!(
            exec.parameters["properties"]["input"]["type"], "string",
            "语法工具给上游套 {{input}} 壳"
        );

        let read = by_name("read_file");
        assert!(!read.grammar);
        assert_eq!(read.namespace, None);

        let spawn = by_name("spawn_agent");
        assert!(!spawn.grammar);
        assert_eq!(
            spawn.namespace.as_deref(),
            Some("collaboration"),
            "非默认命名空间要带回给客户端"
        );

        let top = by_name("top_level");
        assert_eq!(top.namespace, None, "顶层 tools 没有命名空间");
    }

    #[test]
    fn responses_falls_back_to_chat_style_messages_when_input_is_absent() {
        let p = responses(json!({
            "instructions": "sys",
            "messages": [{ "role": "user", "content": "hi" }],
        }));
        assert_eq!(p.request.messages.len(), 2);
        assert_eq!(p.request.messages[0].role, Role::System);
        assert_eq!(p.request.messages[0].text, "sys");
        assert_eq!(p.request.messages[1].text, "hi");
    }

    #[test]
    fn responses_grammar_tools_get_the_input_string_schema() {
        let p = responses(json!({
            "input": "x",
            "tools": [{ "type": "custom", "name": "apply_patch", "description": "d" }],
        }));
        assert_eq!(p.request.tools[0].name, "apply_patch");
        assert_eq!(
            p.request.tools[0].parameters,
            json!({
                "type": "object",
                "properties": {
                    "input": {
                        "type": "string",
                        "description": "Raw tool input, written exactly in the syntax this tool documents.",
                    }
                },
                "required": ["input"],
            })
        );
    }

    #[test]
    fn previous_response_id_without_history_is_rejected_with_actionable_message() {
        let err = parse_request(
            Dialect::OpenAiResponses,
            &json!({ "previous_response_id": "resp_123", "input": "just a new message" }),
            None,
        )
        .unwrap_err();
        let ParseError::MissingHistory(msg) = err else {
            panic!("expected MissingHistory, got {err:?}");
        };
        assert!(msg.contains("previous_response_id"));
        assert!(msg.contains("store: false"), "错误要告诉用户怎么改");
    }

    #[test]
    fn previous_response_id_with_a_full_history_is_accepted() {
        // 带了 id，但 input 里也带着完整历史（有一条 function_call）——判据是历史
        // 在不在，不是字段在不在，这种情况不该被拒。
        let ok = parse_request(
            Dialect::OpenAiResponses,
            &json!({
                "previous_response_id": "resp_123",
                "input": [
                    { "role": "user", "content": "do it" },
                    { "type": "function_call", "call_id": "c1", "name": "f", "arguments": "{}" },
                    { "type": "function_call_output", "call_id": "c1", "output": "done" },
                ],
            }),
            None,
        );
        assert!(ok.is_ok());

        // 换成一条 assistant 消息（不带工具调用）也算历史。
        let ok2 = parse_request(
            Dialect::OpenAiResponses,
            &json!({
                "previous_response_id": "resp_123",
                "input": [
                    { "role": "user", "content": "do it" },
                    { "role": "assistant", "content": "did it" },
                ],
            }),
            None,
        );
        assert!(ok2.is_ok());
    }

    #[test]
    fn item_reference_is_always_rejected() {
        let err = parse_request(
            Dialect::OpenAiResponses,
            &json!({ "input": [{ "type": "item_reference", "id": "ref_1" }] }),
            None,
        )
        .unwrap_err();
        let ParseError::MissingHistory(msg) = err else {
            panic!("expected MissingHistory, got {err:?}");
        };
        assert!(msg.contains("item_reference"));
    }

    #[test]
    fn responses_tool_result_without_call_id_falls_back_to_id() {
        let p = responses(json!({
            "input": [
                { "role": "user", "content": "x" },
                { "type": "function_call", "id": "fc_1", "name": "f", "arguments": "{}" },
                { "type": "function_call_output", "id": "fc_1", "output": "ok" },
            ],
        }));
        let m = &p.request.messages;
        assert_eq!(m[1].tool_calls[0].id, "fc_1");
        assert_eq!(m[2].tool_results[0].tool_call_id, "fc_1");
    }
}
