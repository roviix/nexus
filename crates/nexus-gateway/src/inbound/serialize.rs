//! 统一表示 → 客户端要的方言。
//!
//! 三种方言的流式协议差异很大：OpenAI Chat 是一串 delta chunk，Anthropic 是 content_block
//! 的开合，Responses 是带序号的 output item 开合（比 Anthropic 还严格：每个 item 必须先
//! `added` 再 `done`，孤立的 delta 会被客户端直接丢弃）。共同点只有「文本增量、思考增量、
//! 工具调用、结束」四件事。这里每个动作返回一组 [`SseFrame`]，不碰 socket，server 层负责
//! 写出去。

use super::{random_id, Dialect, SseFrame};
use crate::normalized::{Completion, FinishReason, ToolCall, ToolDef, Usage};
use serde_json::{json, Value};
use std::collections::HashMap;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn openai_usage(u: &Usage) -> Value {
    json!({
        "prompt_tokens": u.input_tokens,
        "completion_tokens": u.output_tokens,
        "total_tokens": u.input_tokens + u.output_tokens,
        "prompt_tokens_details": { "cached_tokens": u.cache_read_tokens },
        "completion_tokens_details": { "reasoning_tokens": u.reasoning_tokens },
    })
}

fn anthropic_usage(u: &Usage) -> Value {
    json!({
        "input_tokens": u.input_tokens,
        "output_tokens": u.output_tokens,
        "cache_read_input_tokens": u.cache_read_tokens,
        "cache_creation_input_tokens": u.cache_write_tokens,
    })
}

/// `output_tokens_details` 这类对象一旦出现在响应里就必须带上 `reasoning_tokens`——
/// 不发这个对象是合法的，发了却缺字段不合法。`Usage::reasoning_tokens` 是 `u32`
/// 不是 `Option`，天然没有「缺省键」这个状态，但仍然把它单独摘出来写清楚：这是
/// Codex 的 serde 会直接判死连接、反复重连的一个字段，不能靠隐含的类型系统巧合过关。
fn responses_usage(u: &Usage) -> Value {
    json!({
        "input_tokens": u.input_tokens,
        "output_tokens": u.output_tokens,
        "total_tokens": u.input_tokens + u.output_tokens,
        "input_tokens_details": { "cached_tokens": u.cache_read_tokens },
        "output_tokens_details": { "reasoning_tokens": u.reasoning_tokens },
    })
}

fn anthropic_stop_reason(reason: FinishReason, tool_calls: &[ToolCall]) -> &'static str {
    if !tool_calls.is_empty() {
        "tool_use"
    } else if reason == FinishReason::Length {
        "max_tokens"
    } else {
        "end_turn"
    }
}

fn openai_finish_reason(reason: FinishReason, tool_calls: &[ToolCall]) -> &'static str {
    if !tool_calls.is_empty() {
        "tool_calls"
    } else {
        match reason {
            FinishReason::Length => "length",
            FinishReason::ContentFilter => "content_filter",
            // 绝不写 "error"——不是合法值，SDK 会在这里解析失败。
            _ => "stop",
        }
    }
}

fn resp_text_part(text: &str) -> Value {
    json!({ "type": "output_text", "text": text, "annotations": [] })
}

fn tool_input_value(arguments: &str) -> Value {
    match serde_json::from_str::<Value>(if arguments.is_empty() {
        "{}"
    } else {
        arguments
    }) {
        Ok(v) => v,
        // 上游给的参数不是合法 JSON。原样塞进去而不是丢掉——客户端至少能看到模型想传什么。
        Err(_) => json!({ "_raw": arguments }),
    }
}

/// 语法工具的原文。声明时给上游套的是 `{ input }` 壳（见 parse 的 `grammar_tool_schema`），
/// 模型照着壳回一段 JSON，这里拆掉还原成客户端要的裸文本；不是这个形状就原样给——
/// 客户端至少能看到模型想传什么。
fn grammar_input(arguments: &str) -> String {
    if let Ok(Value::Object(mut m)) = serde_json::from_str::<Value>(arguments) {
        if let Some(Value::String(s)) = m.remove("input") {
            return s;
        }
    }
    arguments.to_string()
}

/// 上游报的工具名去掉命名空间前缀后的尾段。上游（或模型）可能把命名空间拼进名字：
/// `functions.exec`、`functions__exec`、`mcp__server__tool`、`ns::tool`。
fn tool_name_tail(name: &str) -> &str {
    let mut tail = name;
    for sep in [".", "__", "/", ":"] {
        if let Some((_, rest)) = tail.rsplit_once(sep) {
            tail = rest;
        }
    }
    tail
}

/// 客户端声明工具时带的、回程必须还原的元数据。
#[derive(Clone, Default)]
struct DeclaredTool {
    grammar: bool,
    namespace: Option<String>,
}

/// 一次响应的序列化状态。
pub struct Serializer {
    dialect: Dialect,
    model: String,
    chat_id: String,
    message_id: String,
    opened: bool,
    block_index: u32,
    text_block_open: bool,
    thinking_block_open: bool,
    sent_text: String,
    /// 客户端声明过的工具（名字 → 语法 / 命名空间元数据）。不在这里的调用不能回给它——
    /// 它既执行不了，也没法带回结果。
    client_tools: HashMap<String, DeclaredTool>,
    // --- 以下只有 Responses 用：这个方言按 output item 的 added/done 重建内容，
    // 光发 delta 不够，客户端连该往哪个 item 里塞都不知道。
    response_id: String,
    /// 非流式且没有思考流出的兜底 id（流式路径每段思考各起一个 id，见 `resp_open_reasoning`）。
    reasoning_id: String,
    resp_seq: u32,
    resp_index: u32,
    resp_text_open: bool,
    resp_text_index: u32,
    resp_text_id: String,
    resp_text_buf: String,
    /// 是否已经发过 message item。第一段沿用 `message_id`，之后另起 id。
    resp_text_emitted: bool,
    resp_reasoning_open: bool,
    resp_reasoning_index: u32,
    resp_reasoning_id: String,
    /// 当前这段思考的正文（不是整轮累计——上游可以「想一段、说一句、再想一段」，
    /// 用累计值的话第二段会把第一段重复一遍）。
    resp_reasoning_text: String,
    /// 已经流出去的 item，按 output_index 顺序。`response.completed` 必须直接复用它，
    /// 不能照着最终文本再拼一遍——两边各拼各的，id、条数、顺序迟早对不上。
    resp_items: Vec<Value>,
}

impl Serializer {
    pub fn new(dialect: Dialect, model: &str, client_tools: &[ToolDef]) -> Self {
        Self {
            dialect,
            model: model.to_string(),
            chat_id: random_id("chatcmpl"),
            message_id: random_id("msg"),
            opened: false,
            block_index: 0,
            text_block_open: false,
            thinking_block_open: false,
            sent_text: String::new(),
            client_tools: client_tools
                .iter()
                .map(|t| {
                    (
                        t.name.clone(),
                        DeclaredTool {
                            grammar: t.grammar,
                            namespace: t.namespace.clone(),
                        },
                    )
                })
                .collect(),
            response_id: random_id("resp"),
            reasoning_id: random_id("rs"),
            resp_seq: 0,
            resp_index: 0,
            resp_text_open: false,
            resp_text_index: 0,
            resp_text_id: String::new(),
            resp_text_buf: String::new(),
            resp_text_emitted: false,
            resp_reasoning_open: false,
            resp_reasoning_index: 0,
            resp_reasoning_id: String::new(),
            resp_reasoning_text: String::new(),
            resp_items: Vec::new(),
        }
    }

    fn next_seq(&mut self) -> u32 {
        let s = self.resp_seq;
        self.resp_seq += 1;
        s
    }

    /// 已经发过第一个语义事件。这之后再出错就只能在流里说，HTTP 状态改不了了。
    pub fn opened(&self) -> bool {
        self.opened
    }

    fn chunk(&self, delta: Value, finish_reason: Option<&str>, usage: Option<Value>) -> Value {
        let mut v = json!({
            "id": self.chat_id,
            "object": "chat.completion.chunk",
            "created": now_secs(),
            "model": self.model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish_reason }],
        });
        if let Some(u) = usage {
            v["usage"] = u;
        }
        v
    }

    /// 把上游报的工具名对齐到客户端声明的写法：先精确匹配，再去掉命名空间前缀，
    /// 最后忽略大小写。客户端按自己声明的名字找 handler，差一个字符就是 unsupported call。
    fn resolve_tool_name(&self, name: &str) -> Option<String> {
        if self.client_tools.contains_key(name) {
            return Some(name.to_string());
        }
        let tail = tool_name_tail(name);
        if !tail.is_empty() && self.client_tools.contains_key(tail) {
            return Some(tail.to_string());
        }
        let lower = if tail.is_empty() { name } else { tail }.to_lowercase();
        self.client_tools
            .keys()
            .find(|declared| declared.to_lowercase() == lower)
            .cloned()
    }

    /// 只回客户端认得的工具调用，名字对齐到它声明的写法。
    fn visible(&self, calls: &[ToolCall]) -> Vec<ToolCall> {
        calls
            .iter()
            .filter_map(|c| match self.resolve_tool_name(&c.name) {
                Some(name) => Some(ToolCall { name, ..c.clone() }),
                None => {
                    tracing::warn!(tool = %c.name, "丢弃客户端没声明过的工具调用");
                    None
                }
            })
            .collect()
    }

    /// Responses 的工具调用 item。语法工具是 `custom_tool_call`（正文在 `input`），其余是
    /// `function_call`（正文在 `arguments`）；声明时带命名空间的，调用也得带着回去。
    fn resp_tool_item(&self, call: &ToolCall, item_id: &str, call_id: &str, status: &str) -> Value {
        let decl = self
            .client_tools
            .get(&call.name)
            .cloned()
            .unwrap_or_default();
        let mut item = if decl.grammar {
            json!({ "id": item_id, "type": "custom_tool_call", "call_id": call_id,
                    "name": call.name, "input": grammar_input(&call.arguments), "status": status })
        } else {
            json!({ "id": item_id, "type": "function_call", "call_id": call_id,
                    "name": call.name,
                    "arguments": if call.arguments.is_empty() { "{}" } else { &call.arguments },
                    "status": status })
        };
        if let Some(ns) = decl.namespace {
            item["namespace"] = Value::String(ns);
        }
        item
    }

    /// 第一帧。OpenAI 官方首帧只带 role，SDK 靠它初始化 message 对象；Anthropic 是
    /// message_start + 一个 ping（有客户端拿 ping 当「连接活着」的判据）。
    pub fn open(&mut self) -> Vec<SseFrame> {
        if self.opened {
            return Vec::new();
        }
        self.opened = true;
        match self.dialect {
            Dialect::AnthropicMessages => vec![
                SseFrame::json(
                    Some("message_start"),
                    &json!({
                        "type": "message_start",
                        "message": {
                            "id": self.message_id, "type": "message", "role": "assistant",
                            "model": self.model, "content": [], "stop_reason": null,
                            "usage": anthropic_usage(&Usage::default()),
                        }
                    }),
                ),
                SseFrame::json(Some("ping"), &json!({ "type": "ping" })),
            ],
            Dialect::OpenAiChat => vec![SseFrame::json(
                None,
                &self.chunk(json!({ "role": "assistant", "content": "" }), None, None),
            )],
            Dialect::OpenAiResponses => {
                let created_seq = self.next_seq();
                let progress_seq = self.next_seq();
                vec![
                    SseFrame::json(
                        Some("response.created"),
                        &json!({ "type": "response.created", "sequence_number": created_seq,
                                 "response": self.resp_stub() }),
                    ),
                    SseFrame::json(
                        Some("response.in_progress"),
                        &json!({ "type": "response.in_progress", "sequence_number": progress_seq,
                                 "response": self.resp_stub() }),
                    ),
                ]
            }
        }
    }

    fn resp_stub(&self) -> Value {
        json!({ "id": self.response_id, "object": "response", "model": self.model,
                "status": "in_progress", "output": [] })
    }

    fn close_thinking(&mut self, out: &mut Vec<SseFrame>) {
        if self.thinking_block_open {
            out.push(SseFrame::json(
                Some("content_block_stop"),
                &json!({ "type": "content_block_stop", "index": self.block_index }),
            ));
            self.thinking_block_open = false;
            self.block_index += 1;
        }
    }

    fn close_text(&mut self, out: &mut Vec<SseFrame>) {
        if self.text_block_open {
            out.push(SseFrame::json(
                Some("content_block_stop"),
                &json!({ "type": "content_block_stop", "index": self.block_index }),
            ));
            self.text_block_open = false;
            self.block_index += 1;
        }
    }

    /// Responses 的思考 item。同一时刻只能有一个 item 开着——不先收掉正文的话，
    /// 后续的正文 delta 指向的 item 已经不是当前活跃的那个，客户端会把它整段丢弃。
    fn resp_open_reasoning(&mut self, out: &mut Vec<SseFrame>) {
        if self.resp_reasoning_open {
            return;
        }
        self.resp_close_text(out);
        self.resp_reasoning_open = true;
        self.resp_reasoning_index = self.resp_index;
        self.resp_index += 1;
        // 每段思考一个新 id，复用同一个 id 会让一次响应里出现两个同 id 的 item。
        self.resp_reasoning_id = random_id("rs");
        self.resp_reasoning_text.clear();
        let seq1 = self.next_seq();
        out.push(SseFrame::json(
            Some("response.output_item.added"),
            &json!({ "type": "response.output_item.added", "sequence_number": seq1,
                     "output_index": self.resp_reasoning_index,
                     "item": { "id": self.resp_reasoning_id, "type": "reasoning", "summary": [] } }),
        ));
        let seq2 = self.next_seq();
        out.push(SseFrame::json(
            Some("response.reasoning_summary_part.added"),
            &json!({ "type": "response.reasoning_summary_part.added", "sequence_number": seq2,
                     "item_id": self.resp_reasoning_id, "output_index": self.resp_reasoning_index,
                     "summary_index": 0, "part": { "type": "summary_text", "text": "" } }),
        ));
    }

    fn resp_close_reasoning(&mut self, out: &mut Vec<SseFrame>) {
        if !self.resp_reasoning_open {
            return;
        }
        self.resp_reasoning_open = false;
        let text = std::mem::take(&mut self.resp_reasoning_text);
        let id = self.resp_reasoning_id.clone();
        let index = self.resp_reasoning_index;
        let item = json!({ "id": id, "type": "reasoning",
                            "summary": [{ "type": "summary_text", "text": text }] });
        let seq1 = self.next_seq();
        out.push(SseFrame::json(
            Some("response.reasoning_summary_text.done"),
            &json!({ "type": "response.reasoning_summary_text.done", "sequence_number": seq1,
                     "item_id": id, "output_index": index, "summary_index": 0, "text": text }),
        ));
        let seq2 = self.next_seq();
        out.push(SseFrame::json(
            Some("response.reasoning_summary_part.done"),
            &json!({ "type": "response.reasoning_summary_part.done", "sequence_number": seq2,
                     "item_id": id, "output_index": index, "summary_index": 0,
                     "part": { "type": "summary_text", "text": text } }),
        ));
        let seq3 = self.next_seq();
        out.push(SseFrame::json(
            Some("response.output_item.done"),
            &json!({ "type": "response.output_item.done", "sequence_number": seq3,
                     "output_index": index, "item": item }),
        ));
        self.resp_items.push(item);
    }

    /// Responses 的正文 item。客户端按 `output_item.added` → delta → `output_item.done`
    /// 重建内容，只发 delta 的话它连该往哪个 item 里塞都不知道；codex-cli 实测直接报
    /// `OutputTextDelta without active item` 并把整段正文丢掉。
    fn resp_open_text(&mut self, out: &mut Vec<SseFrame>) {
        if self.resp_text_open {
            return;
        }
        // 思考和正文是两个并列的 output item，正文开始前先把思考收掉，否则两者会抢
        // 同一个 output_index。
        self.resp_close_reasoning(out);
        self.resp_text_open = true;
        self.resp_text_index = self.resp_index;
        self.resp_index += 1;
        // 一轮里可能有多段正文（思考插在中间），各是一个 item，id 不能共用。第一段
        // 沿用 message_id，让最常见的「只有一段正文」情形和非流式那条路 id 一致。
        self.resp_text_id = if self.resp_text_emitted {
            random_id("msg")
        } else {
            self.message_id.clone()
        };
        self.resp_text_emitted = true;
        self.resp_text_buf.clear();
        let seq1 = self.next_seq();
        out.push(SseFrame::json(
            Some("response.output_item.added"),
            &json!({ "type": "response.output_item.added", "sequence_number": seq1,
                     "output_index": self.resp_text_index,
                     "item": { "id": self.resp_text_id, "type": "message", "status": "in_progress",
                               "role": "assistant", "content": [] } }),
        ));
        let seq2 = self.next_seq();
        out.push(SseFrame::json(
            Some("response.content_part.added"),
            &json!({ "type": "response.content_part.added", "sequence_number": seq2,
                     "item_id": self.resp_text_id, "output_index": self.resp_text_index,
                     "content_index": 0, "part": resp_text_part("") }),
        ));
    }

    fn resp_close_text(&mut self, out: &mut Vec<SseFrame>) {
        if !self.resp_text_open {
            return;
        }
        self.resp_text_open = false;
        let text = std::mem::take(&mut self.resp_text_buf);
        let seq1 = self.next_seq();
        out.push(SseFrame::json(
            Some("response.output_text.done"),
            &json!({ "type": "response.output_text.done", "sequence_number": seq1,
                     "item_id": self.resp_text_id, "output_index": self.resp_text_index,
                     "content_index": 0, "text": text }),
        ));
        let seq2 = self.next_seq();
        out.push(SseFrame::json(
            Some("response.content_part.done"),
            &json!({ "type": "response.content_part.done", "sequence_number": seq2,
                     "item_id": self.resp_text_id, "output_index": self.resp_text_index,
                     "content_index": 0, "part": resp_text_part(&text) }),
        ));
        let item = json!({ "id": self.resp_text_id, "type": "message", "status": "completed",
                            "role": "assistant", "content": [resp_text_part(&text)] });
        let seq3 = self.next_seq();
        out.push(SseFrame::json(
            Some("response.output_item.done"),
            &json!({ "type": "response.output_item.done", "sequence_number": seq3,
                     "output_index": self.resp_text_index, "item": item }),
        ));
        self.resp_items.push(item);
    }

    /// 工具调用也是 output item，只塞进 `response.completed` 的话客户端一个都收不到。
    ///
    /// 语法工具（Codex 的 `exec` / `apply_patch`）必须按 `custom_tool_call` 回：增量事件叫
    /// `custom_tool_call_input.*`，正文字段是 `input` 而不是 `arguments`。按 `function_call`
    /// 回的话 Codex 找得到工具却对不上载荷类型，整次调用按致命错误吞掉、不记结果，
    /// 模型下一轮只看到一个「aborted」，于是原样再调一次——无限循环。
    fn resp_emit_tool_call(&mut self, out: &mut Vec<SseFrame>, call: &ToolCall) {
        let index = self.resp_index;
        self.resp_index += 1;
        let grammar = self
            .client_tools
            .get(&call.name)
            .map(|d| d.grammar)
            .unwrap_or(false);
        let item_id = random_id(if grammar { "ctc" } else { "fc" });
        let call_id = if call.id.is_empty() {
            random_id("call")
        } else {
            call.id.clone()
        };
        let item = self.resp_tool_item(call, &item_id, &call_id, "completed");
        let (payload_key, delta_event, done_event) = if grammar {
            (
                "input",
                "response.custom_tool_call_input.delta",
                "response.custom_tool_call_input.done",
            )
        } else {
            (
                "arguments",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
            )
        };
        let payload = item[payload_key].clone();

        let mut added = item.clone();
        added["status"] = json!("in_progress");
        added[payload_key] = json!("");
        let seq1 = self.next_seq();
        out.push(SseFrame::json(
            Some("response.output_item.added"),
            &json!({ "type": "response.output_item.added", "sequence_number": seq1,
                     "output_index": index, "item": added }),
        ));
        let seq2 = self.next_seq();
        out.push(SseFrame::json(
            Some(delta_event),
            &json!({ "type": delta_event, "sequence_number": seq2,
                     "item_id": item_id, "output_index": index, "delta": payload }),
        ));
        let seq3 = self.next_seq();
        out.push(SseFrame::json(
            Some(done_event),
            &json!({ "type": done_event, "sequence_number": seq3,
                     "item_id": item_id, "output_index": index, payload_key: payload }),
        ));
        let seq4 = self.next_seq();
        out.push(SseFrame::json(
            Some("response.output_item.done"),
            &json!({ "type": "response.output_item.done", "sequence_number": seq4,
                     "output_index": index, "item": item }),
        ));
        self.resp_items.push(item);
    }

    /// 流式已经发出去的 item 直接复用；非流式（或流式里啥都没发生过）现拼一份。
    /// 两边各拼各的话，id、条数、顺序迟早对不上，而客户端会拿这两份东西对账。
    fn completed_response(&self, c: &Completion) -> Value {
        let usage = responses_usage(&c.usage);
        if !self.resp_items.is_empty() {
            return json!({
                "id": self.response_id, "object": "response", "created_at": now_secs(),
                "model": self.model, "status": "completed", "output": self.resp_items,
                "usage": usage,
            });
        }
        let visible = self.visible(&c.tool_calls);
        let mut output: Vec<Value> = Vec::new();
        if !c.thinking.is_empty() {
            output.push(json!({ "type": "reasoning", "id": self.reasoning_id,
                                 "summary": [{ "type": "summary_text", "text": c.thinking }] }));
        }
        if !c.text.is_empty() {
            output.push(
                json!({ "type": "message", "id": self.message_id, "role": "assistant",
                                 "status": "completed", "content": [resp_text_part(&c.text)] }),
            );
        }
        for call in &visible {
            let grammar = self
                .client_tools
                .get(&call.name)
                .map(|d| d.grammar)
                .unwrap_or(false);
            let item_id = random_id(if grammar { "ctc" } else { "fc" });
            let call_id = if call.id.is_empty() {
                random_id("call")
            } else {
                call.id.clone()
            };
            output.push(self.resp_tool_item(call, &item_id, &call_id, "completed"));
        }
        json!({
            "id": self.response_id, "object": "response", "created_at": now_secs(),
            "model": self.model, "status": "completed", "output": output, "usage": usage,
        })
    }

    pub fn text(&mut self, delta: &str) -> Vec<SseFrame> {
        if delta.is_empty() {
            return Vec::new();
        }
        let mut out = self.open();
        self.sent_text.push_str(delta);
        match self.dialect {
            Dialect::AnthropicMessages => {
                self.close_thinking(&mut out);
                if !self.text_block_open {
                    out.push(SseFrame::json(
                        Some("content_block_start"),
                        &json!({ "type": "content_block_start", "index": self.block_index,
                                 "content_block": { "type": "text", "text": "" } }),
                    ));
                    self.text_block_open = true;
                }
                out.push(SseFrame::json(
                    Some("content_block_delta"),
                    &json!({ "type": "content_block_delta", "index": self.block_index,
                             "delta": { "type": "text_delta", "text": delta } }),
                ));
            }
            Dialect::OpenAiChat => out.push(SseFrame::json(
                None,
                &self.chunk(json!({ "content": delta }), None, None),
            )),
            Dialect::OpenAiResponses => {
                self.resp_open_text(&mut out);
                self.resp_text_buf.push_str(delta);
                let seq = self.next_seq();
                out.push(SseFrame::json(
                    Some("response.output_text.delta"),
                    &json!({ "type": "response.output_text.delta", "sequence_number": seq,
                             "item_id": self.resp_text_id, "output_index": self.resp_text_index,
                             "content_index": 0, "delta": delta }),
                ));
            }
        }
        out
    }

    /// 思考增量。上游「想一段、说一句、再想一段」时 Anthropic 要另开新块而不是丢弃；
    /// OpenAI Chat 没有官方字段，`reasoning_content` 是社区事实标准。
    pub fn thinking(&mut self, delta: &str) -> Vec<SseFrame> {
        if delta.is_empty() {
            return Vec::new();
        }
        let mut out = self.open();
        match self.dialect {
            Dialect::AnthropicMessages => {
                self.close_text(&mut out);
                if !self.thinking_block_open {
                    out.push(SseFrame::json(
                        Some("content_block_start"),
                        &json!({ "type": "content_block_start", "index": self.block_index,
                                 "content_block": { "type": "thinking", "thinking": "" } }),
                    ));
                    self.thinking_block_open = true;
                }
                out.push(SseFrame::json(
                    Some("content_block_delta"),
                    &json!({ "type": "content_block_delta", "index": self.block_index,
                             "delta": { "type": "thinking_delta", "thinking": delta } }),
                ));
            }
            Dialect::OpenAiChat => out.push(SseFrame::json(
                None,
                &self.chunk(json!({ "reasoning_content": delta }), None, None),
            )),
            Dialect::OpenAiResponses => {
                self.resp_open_reasoning(&mut out);
                self.resp_reasoning_text.push_str(delta);
                let seq = self.next_seq();
                out.push(SseFrame::json(
                    Some("response.reasoning_summary_text.delta"),
                    &json!({ "type": "response.reasoning_summary_text.delta", "sequence_number": seq,
                             "item_id": self.resp_reasoning_id, "output_index": self.resp_reasoning_index,
                             "summary_index": 0, "delta": delta }),
                ));
            }
        }
        out
    }

    /// 结束流。`completion.text` 是完整正文，这里只补发还没流出去的差额。
    pub fn finish(&mut self, c: &Completion) -> Vec<SseFrame> {
        let mut out = self.open();
        let missing = c
            .text
            .strip_prefix(self.sent_text.as_str())
            .unwrap_or(&c.text)
            .to_string();
        if !missing.is_empty() {
            out.extend(self.text(&missing));
        }
        let visible_owned = self.visible(&c.tool_calls);

        match self.dialect {
            Dialect::AnthropicMessages => {
                self.close_thinking(&mut out);
                self.close_text(&mut out);
                for call in &visible_owned {
                    let id = if call.id.is_empty() {
                        random_id("toolu")
                    } else {
                        call.id.clone()
                    };
                    out.push(SseFrame::json(
                        Some("content_block_start"),
                        &json!({ "type": "content_block_start", "index": self.block_index,
                                 "content_block": { "type": "tool_use", "id": id, "name": call.name, "input": {} } }),
                    ));
                    out.push(SseFrame::json(
                        Some("content_block_delta"),
                        &json!({ "type": "content_block_delta", "index": self.block_index,
                                 "delta": { "type": "input_json_delta",
                                            "partial_json": if call.arguments.is_empty() { "{}" } else { &call.arguments } } }),
                    ));
                    out.push(SseFrame::json(
                        Some("content_block_stop"),
                        &json!({ "type": "content_block_stop", "index": self.block_index }),
                    ));
                    self.block_index += 1;
                }
                out.push(SseFrame::json(
                    Some("message_delta"),
                    &json!({ "type": "message_delta",
                             "delta": { "stop_reason": anthropic_stop_reason(c.finish_reason, &visible_owned), "stop_sequence": null },
                             "usage": anthropic_usage(&c.usage) }),
                ));
                out.push(SseFrame::json(
                    Some("message_stop"),
                    &json!({ "type": "message_stop" }),
                ));
            }
            Dialect::OpenAiChat => {
                if !visible_owned.is_empty() {
                    let calls: Vec<Value> = visible_owned
                        .iter()
                        .enumerate()
                        .map(|(i, call)| {
                            json!({
                                "index": i,
                                "id": if call.id.is_empty() { random_id("call") } else { call.id.clone() },
                                "type": "function",
                                "function": { "name": call.name,
                                              "arguments": if call.arguments.is_empty() { "{}" } else { &call.arguments } },
                            })
                        })
                        .collect();
                    out.push(SseFrame::json(
                        None,
                        &self.chunk(json!({ "tool_calls": calls }), None, None),
                    ));
                }
                // 收尾帧的 model 写上游**实际路由到**的那个（`auto` 时才和请求不同）。
                // OpenAI 自己也是这么做的——返回的是真正用的模型名；前面的帧发出时还不知道，
                // 仍是请求名。客户端不校验帧间一致，广场的「试一下」靠它显示实际路由。
                let mut last = self.chunk(
                    json!({}),
                    Some(openai_finish_reason(c.finish_reason, &visible_owned)),
                    Some(openai_usage(&c.usage)),
                );
                if let Some(routed) = &c.routed_model {
                    last["model"] = json!(routed);
                }
                out.push(SseFrame::json(None, &last));
                out.push(SseFrame::data("[DONE]"));
            }
            Dialect::OpenAiResponses => {
                self.resp_close_reasoning(&mut out);
                self.resp_close_text(&mut out);
                for call in &visible_owned {
                    self.resp_emit_tool_call(&mut out, call);
                }
                let seq = self.next_seq();
                let response = self.completed_response(c);
                out.push(SseFrame::json(
                    Some("response.completed"),
                    &json!({ "type": "response.completed", "sequence_number": seq,
                             "response": response }),
                ));
            }
        }
        out
    }

    /// 流已经开了才发现出错：HTTP 状态改不了，只能在流里说。
    ///
    /// 光发 `error` 事件不够——不是每个客户端都认它。只发 error 的话，等着终结事件的客户端
    /// （Cursor 就是一个）看到的是一截戛然而止的流，报「stream ended before message_stop」，
    /// 我们写的原因一个字都到不了用户眼前。所以把原因当正文吐一遍（每个客户端都会渲染的
    /// 地方），再补齐终结事件。
    pub fn error(&mut self, message: &str, status: u16) -> Vec<SseFrame> {
        let mut out = self.open();
        let payload = json!({ "type": "error", "error": { "type": "upstream_error", "message": message, "code": status } });
        let note = format!("\n\n[网关错误] {message}");
        match self.dialect {
            Dialect::AnthropicMessages => {
                self.close_thinking(&mut out);
                if !self.text_block_open {
                    out.push(SseFrame::json(
                        Some("content_block_start"),
                        &json!({ "type": "content_block_start", "index": self.block_index,
                                 "content_block": { "type": "text", "text": "" } }),
                    ));
                    self.text_block_open = true;
                }
                out.push(SseFrame::json(
                    Some("content_block_delta"),
                    &json!({ "type": "content_block_delta", "index": self.block_index,
                             "delta": { "type": "text_delta", "text": note } }),
                ));
                self.close_text(&mut out);
                out.push(SseFrame::json(Some("error"), &payload));
                out.push(SseFrame::json(
                    Some("message_delta"),
                    &json!({ "type": "message_delta",
                             "delta": { "stop_reason": "end_turn", "stop_sequence": null },
                             "usage": anthropic_usage(&Usage::default()) }),
                ));
                out.push(SseFrame::json(
                    Some("message_stop"),
                    &json!({ "type": "message_stop" }),
                ));
            }
            Dialect::OpenAiChat => {
                out.extend(self.text(&note));
                out.push(SseFrame::json(
                    None,
                    &self.chunk(
                        json!({}),
                        Some("stop"),
                        Some(openai_usage(&Usage::default())),
                    ),
                ));
                out.push(SseFrame::json(None, &payload));
                out.push(SseFrame::data("[DONE]"));
            }
            Dialect::OpenAiResponses => {
                // 把已经开着的 item 收干净再报错。留着不关，客户端会等一个永远不来的
                // output_item.done，错误原因反而显示不出来。这里不走「把原因当正文吐
                // 一遍」那套——Responses 的错误落在 response.failed.error 上，客户端
                // （codex-cli）就是读这个字段的。
                self.resp_close_reasoning(&mut out);
                self.resp_close_text(&mut out);
                let seq = self.next_seq();
                out.push(SseFrame::json(
                    Some("response.failed"),
                    &json!({ "type": "response.failed", "sequence_number": seq,
                             "response": { "id": self.response_id, "status": "failed",
                                           "error": payload["error"].clone() } }),
                ));
            }
        }
        out
    }

    /// 非流式的完整响应体。
    pub fn final_json(&self, c: &Completion) -> Value {
        let visible_owned = self.visible(&c.tool_calls);
        match self.dialect {
            Dialect::AnthropicMessages => {
                let mut content = Vec::new();
                if !c.thinking.is_empty() {
                    content.push(
                        json!({ "type": "thinking", "thinking": c.thinking, "signature": "" }),
                    );
                }
                if !c.text.is_empty() {
                    content.push(json!({ "type": "text", "text": c.text }));
                }
                for call in &visible_owned {
                    content.push(json!({
                        "type": "tool_use",
                        "id": if call.id.is_empty() { random_id("toolu") } else { call.id.clone() },
                        "name": call.name,
                        "input": tool_input_value(&call.arguments),
                    }));
                }
                json!({
                    "id": self.message_id, "type": "message", "role": "assistant", "model": self.model,
                    "content": content,
                    "stop_reason": anthropic_stop_reason(c.finish_reason, &visible_owned),
                    "stop_sequence": null,
                    "usage": anthropic_usage(&c.usage),
                })
            }
            Dialect::OpenAiChat => {
                let mut message = json!({
                    "role": "assistant",
                    "content": if c.text.is_empty() { Value::Null } else { Value::String(c.text.clone()) },
                });
                if !c.thinking.is_empty() {
                    message["reasoning_content"] = Value::String(c.thinking.clone());
                }
                if !visible_owned.is_empty() {
                    message["tool_calls"] = Value::Array(
                        visible_owned
                            .iter()
                            .map(|call| {
                                json!({
                                    "id": if call.id.is_empty() { random_id("call") } else { call.id.clone() },
                                    "type": "function",
                                    "function": { "name": call.name,
                                                  "arguments": if call.arguments.is_empty() { "{}" } else { &call.arguments } },
                                })
                            })
                            .collect(),
                    );
                }
                json!({
                    "id": self.chat_id, "object": "chat.completion", "created": now_secs(),
                    // 同流式收尾帧：报实际路由到的模型。
                    "model": c.routed_model.as_deref().unwrap_or(&self.model),
                    "choices": [{ "index": 0, "message": message,
                                  "finish_reason": openai_finish_reason(c.finish_reason, &visible_owned) }],
                    "usage": openai_usage(&c.usage),
                })
            }
            Dialect::OpenAiResponses => self.completed_response(c),
        }
    }
}

/// 流还没开时的错误响应体。
pub fn protocol_error(dialect: Dialect, status: u16, message: &str) -> Value {
    match dialect {
        Dialect::AnthropicMessages => {
            let t = match status {
                401 => "authentication_error",
                403 => "permission_error",
                404 => "not_found_error",
                413 => "request_too_large",
                429 => "rate_limit_error",
                s if s >= 500 => "api_error",
                _ => "invalid_request_error",
            };
            json!({ "type": "error", "error": { "type": t, "message": message } })
        }
        // Responses 用的也是 OpenAI 那套错误分类——Codex 走的是官方 SDK，认的是这一形状。
        Dialect::OpenAiChat | Dialect::OpenAiResponses => {
            let t = match status {
                402 => "insufficient_quota",
                429 => "rate_limit_error",
                s if s >= 500 => "server_error",
                _ => "invalid_request_error",
            };
            json!({ "error": { "message": message, "type": t, "code": status } })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion(text: &str, tools: Vec<ToolCall>, finish: FinishReason) -> Completion {
        Completion {
            text: text.into(),
            thinking: String::new(),
            tool_calls: tools,
            finish_reason: finish,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                cache_read_tokens: 3,
                cache_write_tokens: 0,
                reasoning_tokens: 0,
            },
            usage_measured: true,
            routed_model: None,
            ttft_ms: None,
            turn_ms: 0,
            raw_response: None,
        }
    }

    fn events(frames: &[SseFrame]) -> Vec<String> {
        frames
            .iter()
            .map(|f| f.event.clone().unwrap_or_else(|| "data".into()))
            .collect()
    }

    /// 客户端声明的普通函数工具。
    fn tools(names: &[&str]) -> Vec<ToolDef> {
        names
            .iter()
            .map(|n| ToolDef {
                name: (*n).into(),
                ..ToolDef::default()
            })
            .collect()
    }

    fn grammar_tool(name: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            grammar: true,
            ..ToolDef::default()
        }
    }

    fn namespaced_tool(name: &str, ns: &str) -> ToolDef {
        ToolDef {
            name: name.into(),
            namespace: Some(ns.into()),
            ..ToolDef::default()
        }
    }

    fn parse(f: &SseFrame) -> Value {
        serde_json::from_str(&f.data).unwrap()
    }

    #[test]
    fn anthropic_stream_follows_the_official_event_sequence() {
        let mut s = Serializer::new(Dialect::AnthropicMessages, "m", &[]);
        let mut all = s.thinking("hmm");
        all.extend(s.text("Hel"));
        all.extend(s.text("lo"));
        all.extend(s.finish(&completion("Hello", vec![], FinishReason::Stop)));
        assert_eq!(
            events(&all),
            vec![
                "message_start",
                "ping",
                "content_block_start",
                "content_block_delta", // thinking 块
                "content_block_stop",
                "content_block_start",
                "content_block_delta", // 切到 text 块
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        let start = parse(&all[2]);
        assert_eq!(start["content_block"]["type"], "thinking");
        assert_eq!(start["index"], 0);
        let text_start = parse(&all[5]);
        assert_eq!(text_start["content_block"]["type"], "text");
        assert_eq!(text_start["index"], 1, "块序号递增");
        let delta = parse(&all[9]);
        assert_eq!(delta["delta"]["stop_reason"], "end_turn");
        assert_eq!(delta["usage"]["cache_read_input_tokens"], 3);
    }

    #[test]
    fn anthropic_tool_use_blocks_and_stop_reason() {
        let mut s = Serializer::new(Dialect::AnthropicMessages, "m", &tools(&["read"]));
        let call = ToolCall {
            id: "toolu_1".into(),
            name: "read".into(),
            arguments: r#"{"p":1}"#.into(),
        };
        let stranger = ToolCall {
            id: "x".into(),
            name: "not_declared".into(),
            arguments: "{}".into(),
        };
        let all = s.finish(&completion(
            "",
            vec![call, stranger],
            FinishReason::ToolCalls,
        ));
        let ev = events(&all);
        assert_eq!(
            ev,
            vec![
                "message_start",
                "ping",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop"
            ],
            "没声明的工具调用被丢弃，只剩一个 tool_use 块"
        );
        let start = parse(&all[2]);
        assert_eq!(start["content_block"]["type"], "tool_use");
        assert_eq!(start["content_block"]["id"], "toolu_1");
        assert_eq!(parse(&all[3])["delta"]["partial_json"], r#"{"p":1}"#);
        assert_eq!(parse(&all[5])["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn anthropic_length_maps_to_max_tokens() {
        let mut s = Serializer::new(Dialect::AnthropicMessages, "m", &[]);
        let all = s.finish(&completion("partial", vec![], FinishReason::Length));
        let delta = all
            .iter()
            .find(|f| f.event.as_deref() == Some("message_delta"))
            .unwrap();
        assert_eq!(parse(delta)["delta"]["stop_reason"], "max_tokens");
    }

    #[test]
    fn openai_stream_has_role_first_frame_then_deltas_then_usage_and_done() {
        let mut s = Serializer::new(Dialect::OpenAiChat, "m", &[]);
        let mut all = s.text("Hi");
        all.extend(s.thinking("think"));
        all.extend(s.finish(&completion("Hi", vec![], FinishReason::Stop)));
        assert!(
            all.iter().all(|f| f.event.is_none()),
            "OpenAI 只有 data: 行"
        );
        assert_eq!(parse(&all[0])["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(parse(&all[1])["choices"][0]["delta"]["content"], "Hi");
        assert_eq!(
            parse(&all[2])["choices"][0]["delta"]["reasoning_content"],
            "think"
        );
        let last_chunk = parse(&all[3]);
        assert_eq!(last_chunk["choices"][0]["finish_reason"], "stop");
        assert_eq!(last_chunk["usage"]["total_tokens"], 15);
        assert_eq!(all[4].data, "[DONE]");
    }

    #[test]
    fn openai_tool_calls_come_in_one_chunk_with_indices() {
        let mut s = Serializer::new(Dialect::OpenAiChat, "m", &tools(&["a", "b"]));
        let calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "a".into(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "".into(),
                name: "b".into(),
                arguments: "".into(),
            },
        ];
        let all = s.finish(&completion("", calls, FinishReason::ToolCalls));
        let tc = parse(&all[1])["choices"][0]["delta"]["tool_calls"].clone();
        assert_eq!(tc.as_array().unwrap().len(), 2);
        assert_eq!(tc[0]["index"], 0);
        assert_eq!(tc[0]["id"], "c1");
        assert_eq!(tc[1]["index"], 1);
        assert!(
            tc[1]["id"].as_str().unwrap().starts_with("call_"),
            "没 id 的补一个"
        );
        assert_eq!(tc[1]["function"]["arguments"], "{}", "空参数补 {{}}");
        assert_eq!(parse(&all[2])["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn finish_only_sends_the_text_not_yet_streamed() {
        let mut s = Serializer::new(Dialect::OpenAiChat, "m", &[]);
        let _ = s.text("Hello");
        let all = s.finish(&completion("Hello world", vec![], FinishReason::Stop));
        assert_eq!(parse(&all[0])["choices"][0]["delta"]["content"], " world");
    }

    #[test]
    fn error_mid_stream_is_rendered_as_text_and_terminated_properly() {
        let mut s = Serializer::new(Dialect::AnthropicMessages, "m", &[]);
        let _ = s.text("partial");
        let all = s.error("upstream down", 502);
        assert_eq!(
            events(&all),
            vec![
                "content_block_delta",
                "content_block_stop",
                "error",
                "message_delta",
                "message_stop"
            ]
        );
        assert!(parse(&all[0])["delta"]["text"]
            .as_str()
            .unwrap()
            .contains("[网关错误] upstream down"));
        assert_eq!(parse(&all[2])["error"]["code"], 502);
        assert_eq!(parse(&all[3])["delta"]["stop_reason"], "end_turn");

        let mut o = Serializer::new(Dialect::OpenAiChat, "m", &[]);
        let all = o.error("boom", 500);
        assert_eq!(
            parse(&all[0])["choices"][0]["delta"]["role"],
            "assistant",
            "还没开流就先开"
        );
        assert!(parse(&all[1])["choices"][0]["delta"]["content"]
            .as_str()
            .unwrap()
            .contains("boom"));
        assert_eq!(
            parse(&all[2])["choices"][0]["finish_reason"],
            "stop",
            "绝不写 error"
        );
        assert_eq!(parse(&all[3])["type"], "error");
        assert_eq!(all[4].data, "[DONE]");
    }

    #[test]
    fn openai_reports_the_routed_model_on_the_closing_frame_and_non_stream_body() {
        let routed = Completion {
            routed_model: Some("composer-2.5-fast".into()),
            ..completion("ok", vec![], FinishReason::Stop)
        };
        let mut s = Serializer::new(Dialect::OpenAiChat, "auto", &[]);
        let mut all = s.text("ok");
        all.extend(s.finish(&routed));
        assert_eq!(parse(&all[0])["model"], "auto", "首帧时还不知道实际路由");
        let closing = &all[all.len() - 2];
        assert_eq!(parse(closing)["model"], "composer-2.5-fast");
        assert_eq!(parse(closing)["choices"][0]["finish_reason"], "stop");
        assert_eq!(all[all.len() - 1].data, "[DONE]");

        let o = Serializer::new(Dialect::OpenAiChat, "auto", &[]);
        assert_eq!(o.final_json(&routed)["model"], "composer-2.5-fast");
        // 上游没报时还是请求名，不能留空。
        assert_eq!(
            o.final_json(&completion("ok", vec![], FinishReason::Stop))["model"],
            "auto"
        );
    }

    #[test]
    fn error_before_any_output_still_opens_the_anthropic_stream_first() {
        let mut s = Serializer::new(Dialect::AnthropicMessages, "m", &[]);
        let all = s.error("nope", 429);
        assert_eq!(events(&all)[..2], ["message_start", "ping"]);
        assert!(events(&all).contains(&"message_stop".to_string()));
    }

    #[test]
    fn non_stream_bodies_have_the_right_shape() {
        let a = Serializer::new(Dialect::AnthropicMessages, "m", &tools(&["f"]));
        let c = Completion {
            thinking: "t".into(),
            ..completion(
                "txt",
                vec![ToolCall {
                    id: "i".into(),
                    name: "f".into(),
                    arguments: "not json".into(),
                }],
                FinishReason::ToolCalls,
            )
        };
        let body = a.final_json(&c);
        assert_eq!(body["type"], "message");
        assert_eq!(body["content"][0]["type"], "thinking");
        assert_eq!(body["content"][1]["text"], "txt");
        assert_eq!(
            body["content"][2]["input"]["_raw"], "not json",
            "坏 JSON 原样保留"
        );
        assert_eq!(body["stop_reason"], "tool_use");

        let o = Serializer::new(Dialect::OpenAiChat, "m", &tools(&["f"]));
        let body = o.final_json(&c);
        assert_eq!(body["object"], "chat.completion");
        assert_eq!(body["choices"][0]["message"]["reasoning_content"], "t");
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "not json"
        );
        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
        let empty = o.final_json(&completion("", vec![], FinishReason::Stop));
        assert!(
            empty["choices"][0]["message"]["content"].is_null(),
            "空正文是 null 不是空串"
        );
    }

    #[test]
    fn protocol_errors_use_each_dialects_error_taxonomy() {
        assert_eq!(
            protocol_error(Dialect::AnthropicMessages, 401, "x")["error"]["type"],
            "authentication_error"
        );
        assert_eq!(
            protocol_error(Dialect::AnthropicMessages, 429, "x")["error"]["type"],
            "rate_limit_error"
        );
        assert_eq!(
            protocol_error(Dialect::AnthropicMessages, 502, "x")["error"]["type"],
            "api_error"
        );
        assert_eq!(
            protocol_error(Dialect::OpenAiChat, 402, "x")["error"]["type"],
            "insufficient_quota"
        );
        assert_eq!(
            protocol_error(Dialect::OpenAiChat, 400, "x")["error"]["code"],
            400
        );
        assert_eq!(
            protocol_error(Dialect::OpenAiResponses, 429, "x")["error"]["type"],
            "rate_limit_error",
            "Responses 用 OpenAI 那套错误分类"
        );
    }

    #[test]
    fn responses_open_emits_created_then_in_progress_with_increasing_sequence_numbers() {
        let mut s = Serializer::new(Dialect::OpenAiResponses, "m", &[]);
        let all = s.open();
        assert_eq!(
            events(&all),
            vec!["response.created", "response.in_progress"]
        );
        assert_eq!(parse(&all[0])["sequence_number"], 0);
        assert_eq!(parse(&all[1])["sequence_number"], 1);
        assert_eq!(parse(&all[0])["response"]["status"], "in_progress");
        assert_eq!(parse(&all[0])["response"]["output"], json!([]));
        // 重复调用 open 是空操作——已经开过的流不能再开一遍。
        assert!(s.open().is_empty());
    }

    #[test]
    fn responses_text_stream_follows_the_added_delta_done_lifecycle() {
        let mut s = Serializer::new(Dialect::OpenAiResponses, "m", &[]);
        let mut all = s.text("Hel");
        all.extend(s.text("lo"));
        all.extend(s.finish(&completion("Hello", vec![], FinishReason::Stop)));
        assert_eq!(
            events(&all),
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        let added = parse(&all[2]);
        assert_eq!(added["item"]["type"], "message");
        assert_eq!(added["item"]["status"], "in_progress");
        assert_eq!(added["output_index"], 0);
        assert_eq!(parse(&all[4])["delta"], "Hel");
        assert_eq!(parse(&all[5])["delta"], "lo");
        let done = parse(&all[8]);
        assert_eq!(done["item"]["status"], "completed");
        assert_eq!(
            done["item"]["content"][0]["text"], "Hello",
            "output_item.done 里的正文是累计值，不是最后一段 delta"
        );
        let completed = parse(&all[9]);
        assert_eq!(completed["response"]["status"], "completed");
        assert_eq!(
            completed["response"]["output"][0], done["item"],
            "response.completed 必须直接复用已经发出去的 item，不能另拼"
        );
        // sequence_number 严格单调递增，一个不落。
        let seqs: Vec<i64> = all
            .iter()
            .map(|f| parse(f)["sequence_number"].as_i64().unwrap())
            .collect();
        let sorted = {
            let mut v = seqs.clone();
            v.sort_unstable();
            v
        };
        assert_eq!(seqs, sorted);
        assert_eq!(seqs.first(), Some(&0));
    }

    #[test]
    fn responses_thinking_then_text_closes_the_reasoning_item_first() {
        let mut s = Serializer::new(Dialect::OpenAiResponses, "m", &[]);
        let mut all = s.thinking("hmm");
        all.extend(s.text("Hi"));
        let events = events(&all);
        assert_eq!(
            events,
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added", // reasoning
                "response.reasoning_summary_part.added",
                "response.reasoning_summary_text.delta",
                "response.reasoning_summary_text.done", // 正文开始前先收掉思考
                "response.reasoning_summary_part.done",
                "response.output_item.done",
                "response.output_item.added", // message
                "response.content_part.added",
                "response.output_text.delta",
            ]
        );
        let reasoning_added = parse(&all[2]);
        assert_eq!(reasoning_added["item"]["type"], "reasoning");
        assert_eq!(reasoning_added["output_index"], 0);
        let text_added = parse(&all[8]);
        assert_eq!(text_added["output_index"], 1, "output_index 跨 item 递增");
    }

    #[test]
    fn responses_function_call_events_and_completed_reuse_the_streamed_item() {
        let mut s = Serializer::new(Dialect::OpenAiResponses, "m", &tools(&["read"]));
        let call = ToolCall {
            id: "call_1".into(),
            name: "read".into(),
            arguments: r#"{"path":"a.txt"}"#.into(),
        };
        let stranger = ToolCall {
            id: "x".into(),
            name: "not_declared".into(),
            arguments: "{}".into(),
        };
        let all = s.finish(&completion(
            "",
            vec![call, stranger],
            FinishReason::ToolCalls,
        ));
        assert_eq!(
            events(&all),
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.function_call_arguments.delta",
                "response.function_call_arguments.done",
                "response.output_item.done",
                "response.completed",
            ],
            "没声明的工具调用被丢弃，只剩一个 function_call"
        );
        let added = parse(&all[2]);
        assert_eq!(added["item"]["type"], "function_call");
        assert_eq!(added["item"]["call_id"], "call_1");
        assert_eq!(added["item"]["arguments"], "", "added 时参数还没到，是空串");
        assert_eq!(
            parse(&all[3])["delta"],
            r#"{"path":"a.txt"}"#,
            "delta 一次性给全量参数——上游本来就是攒完整了才交出去的"
        );
        assert_eq!(parse(&all[4])["arguments"], r#"{"path":"a.txt"}"#);
        let done = parse(&all[5]);
        assert_eq!(done["item"]["status"], "completed");
        let completed = parse(&all[6]);
        assert_eq!(
            completed["response"]["output"],
            json!([done["item"].clone()])
        );
        assert_eq!(
            completed["response"]["usage"]["output_tokens_details"]["reasoning_tokens"],
            0
        );
    }

    #[test]
    fn responses_error_mid_stream_closes_the_open_item_and_sends_response_failed() {
        let mut s = Serializer::new(Dialect::OpenAiResponses, "m", &[]);
        let _ = s.text("partial");
        let all = s.error("upstream down", 502);
        assert_eq!(
            events(&all),
            vec![
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.failed",
            ],
            "开着的 item 先收掉，再报错"
        );
        let failed = parse(&all[3]);
        assert_eq!(failed["response"]["status"], "failed");
        assert_eq!(failed["response"]["error"]["message"], "upstream down");
        assert_eq!(failed["response"]["error"]["code"], 502);
    }

    #[test]
    fn responses_non_stream_final_json_builds_output_from_scratch_when_nothing_was_streamed() {
        let s = Serializer::new(Dialect::OpenAiResponses, "m", &tools(&["f"]));
        let c = Completion {
            thinking: "because".into(),
            ..completion(
                "the answer",
                vec![ToolCall {
                    id: "".into(),
                    name: "f".into(),
                    arguments: "".into(),
                }],
                FinishReason::ToolCalls,
            )
        };
        let body = s.final_json(&c);
        assert_eq!(body["object"], "response");
        assert_eq!(body["status"], "completed");
        assert_eq!(body["output"][0]["type"], "reasoning");
        assert_eq!(body["output"][0]["summary"][0]["text"], "because");
        assert_eq!(body["output"][1]["type"], "message");
        assert_eq!(body["output"][1]["content"][0]["text"], "the answer");
        assert_eq!(body["output"][2]["type"], "function_call");
        assert_eq!(body["output"][2]["name"], "f");
        assert_eq!(body["output"][2]["arguments"], "{}", "空参数补 {{}}");
        assert!(
            body["output"][2]["call_id"]
                .as_str()
                .unwrap()
                .starts_with("call_"),
            "没 id 的补一个"
        );
        // usage 的 details 对象必须带数字字段，不能整个缺省——Codex 的 serde 见到
        // details 对象出现但缺 reasoning_tokens 会直接判死连接。
        assert_eq!(body["usage"]["input_tokens_details"]["cached_tokens"], 3);
        assert_eq!(
            body["usage"]["output_tokens_details"]["reasoning_tokens"],
            0
        );
        assert_eq!(body["usage"]["total_tokens"], 15);
    }

    /// Codex Desktop 的 `exec` 就是这条路：客户端按 `type:"custom"` 声明，模型按 parse 套的
    /// `{ input }` 壳回参数，回程必须还原成 `custom_tool_call` + `custom_tool_call_input.*`。
    /// 按 `function_call` 回的话，Codex 的 handler 对不上载荷类型，整次调用不记结果，模型
    /// 下一轮只看到「aborted」——这就是 2026-09-04 本地网关上 exec 无限重试的根因。
    #[test]
    fn responses_grammar_tool_round_trips_as_custom_tool_call() {
        let mut s = Serializer::new(
            Dialect::OpenAiResponses,
            "m",
            &[grammar_tool("exec"), tools(&["read"]).remove(0)],
        );
        let js = "await tools.exec_command({ cmd: \"ls\" });";
        let call = ToolCall {
            id: "call_1".into(),
            name: "exec".into(),
            arguments: json!({ "input": js }).to_string(),
        };
        let all = s.finish(&completion("", vec![call], FinishReason::ToolCalls));
        assert_eq!(
            events(&all),
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.custom_tool_call_input.delta",
                "response.custom_tool_call_input.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        let added = parse(&all[2]);
        assert_eq!(added["item"]["type"], "custom_tool_call");
        assert_eq!(added["item"]["status"], "in_progress");
        assert_eq!(added["item"]["input"], "", "added 时正文还没到，是空串");
        assert!(
            added["item"]["id"].as_str().unwrap().starts_with("ctc_"),
            "custom_tool_call 的 item id 前缀是 ctc"
        );
        assert!(
            added["item"].get("arguments").is_none(),
            "语法工具没有 arguments 字段"
        );
        assert_eq!(parse(&all[3])["delta"], js, "拆掉 {{input}} 壳还原原文");
        assert_eq!(parse(&all[4])["input"], js);
        let done = parse(&all[5]);
        assert_eq!(done["item"]["type"], "custom_tool_call");
        assert_eq!(done["item"]["call_id"], "call_1");
        assert_eq!(done["item"]["name"], "exec");
        assert_eq!(done["item"]["input"], js);
        assert_eq!(done["item"]["status"], "completed");
        assert!(
            done["item"].get("namespace").is_none(),
            "默认命名空间不带 namespace 字段"
        );
        let completed = parse(&all[6]);
        assert_eq!(
            completed["response"]["output"],
            json!([done["item"].clone()]),
            "response.completed 复用流出去的 custom_tool_call"
        );
    }

    #[test]
    fn responses_grammar_input_that_is_not_the_input_shell_is_passed_through_raw() {
        let s = Serializer::new(Dialect::OpenAiResponses, "m", &[grammar_tool("exec")]);
        // 模型没照壳回，直接给了原文；或者给了别的 JSON 形状——都原样交出去。
        let raw = ToolCall {
            id: "c".into(),
            name: "exec".into(),
            arguments: "console.log(1)".into(),
        };
        let body = s.final_json(&completion("", vec![raw], FinishReason::ToolCalls));
        assert_eq!(body["output"][0]["type"], "custom_tool_call");
        assert_eq!(body["output"][0]["input"], "console.log(1)");

        let other = ToolCall {
            id: "c".into(),
            name: "exec".into(),
            arguments: r#"{"cmd":"ls"}"#.into(),
        };
        let body = s.final_json(&completion("", vec![other], FinishReason::ToolCalls));
        assert_eq!(body["output"][0]["input"], r#"{"cmd":"ls"}"#);

        let empty = ToolCall {
            id: "c".into(),
            name: "exec".into(),
            arguments: "".into(),
        };
        let body = s.final_json(&completion("", vec![empty], FinishReason::ToolCalls));
        assert_eq!(
            body["output"][0]["input"], "",
            "空参数就是空原文，不补 {{}}"
        );
    }

    #[test]
    fn responses_function_call_carries_the_declared_namespace_back() {
        // Codex 的 collaboration 组：spawn_agent 声明在非默认命名空间里。回程不带
        // namespace 的话 Codex 按 {functions, spawn_agent} 找 handler → unsupported call。
        let mut s = Serializer::new(
            Dialect::OpenAiResponses,
            "m",
            &[
                namespaced_tool("spawn_agent", "collaboration"),
                tools(&["read"]).remove(0),
            ],
        );
        let calls = vec![
            ToolCall {
                id: "c1".into(),
                name: "spawn_agent".into(),
                arguments: r#"{"task":"x"}"#.into(),
            },
            ToolCall {
                id: "c2".into(),
                name: "read".into(),
                arguments: "{}".into(),
            },
        ];
        let all = s.finish(&completion("", calls, FinishReason::ToolCalls));
        let items: Vec<Value> = all
            .iter()
            .filter(|f| f.event.as_deref() == Some("response.output_item.done"))
            .map(|f| parse(f)["item"].clone())
            .collect();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["type"], "function_call");
        assert_eq!(items[0]["name"], "spawn_agent");
        assert_eq!(items[0]["namespace"], "collaboration");
        assert_eq!(items[0]["arguments"], r#"{"task":"x"}"#);
        assert_eq!(items[1]["name"], "read");
        assert!(
            items[1].get("namespace").is_none(),
            "没声明命名空间的工具不带 namespace"
        );
        // added 帧也带 namespace——Codex 从 output_item.added 就开始建 item。
        let added = parse(&all[2]);
        assert_eq!(added["item"]["namespace"], "collaboration");
        assert_eq!(added["item"]["arguments"], "");
    }

    #[test]
    fn tool_names_are_aligned_to_the_clients_declaration() {
        let s = Serializer::new(
            Dialect::OpenAiChat,
            "m",
            &[grammar_tool("exec"), tools(&["read_file"]).remove(0)],
        );
        // 上游可能把命名空间拼进名字，或者改了大小写；客户端只认自己声明的写法。
        for upstream in [
            "exec",
            "functions.exec",
            "functions__exec",
            "functions/exec",
            "functions::exec",
            "Exec",
            "functions.EXEC",
        ] {
            assert_eq!(
                s.resolve_tool_name(upstream).as_deref(),
                Some("exec"),
                "{upstream}"
            );
        }
        assert_eq!(
            s.resolve_tool_name("mcp__fs__read_file").as_deref(),
            Some("read_file")
        );
        assert_eq!(s.resolve_tool_name("write_file"), None);
        assert_eq!(s.resolve_tool_name(""), None);

        // 对齐后的名字才是最终发给客户端的。
        let call = ToolCall {
            id: "c".into(),
            name: "functions.read_file".into(),
            arguments: "{}".into(),
        };
        let body = s.final_json(&completion("", vec![call], FinishReason::ToolCalls));
        assert_eq!(
            body["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "read_file"
        );

        // 对齐之后再按声明查语法标记——`functions.exec` 也得走 custom_tool_call。
        let r = Serializer::new(Dialect::OpenAiResponses, "m", &[grammar_tool("exec")]);
        let call = ToolCall {
            id: "c".into(),
            name: "functions.exec".into(),
            arguments: r#"{"input":"1+1"}"#.into(),
        };
        let body = r.final_json(&completion("", vec![call], FinishReason::ToolCalls));
        assert_eq!(body["output"][0]["type"], "custom_tool_call");
        assert_eq!(body["output"][0]["name"], "exec");
        assert_eq!(body["output"][0]["input"], "1+1");
    }
}
