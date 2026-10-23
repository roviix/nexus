//! 对真 api2 跑一组协议用例：系统提示、工具调用、思考、缓存与上下文、多模态。
//!
//! 单元测试只能证明「我们发出去的字节是我们以为的样子」，证明不了上游认不认。这个 example
//! 补的就是那一半：拿 Cursor 里**正登着的号**（或 `PROBE_ACCESS_TOKEN`）逐条打真上游，
//! 每条用例都有判据，结果打成 PASS / FAIL 表。Cursor 升级、换模型之后重跑一遍，就知道
//! 协议层还站不站得住。
//!
//! ```bash
//! NEXUS_GATEWAY_PROBE=1 cargo run -p nexus-gateway --example live_matrix -- status
//! NEXUS_GATEWAY_PROBE=1 cargo run -p nexus-gateway --example live_matrix -- catalog
//! NEXUS_GATEWAY_PROBE=1 cargo run -p nexus-gateway --example live_matrix -- raw system-role
//! # 同条件下只切 max/effort/fast，判断 0.65 本地 host 的默认 profile 是否影响裸 Stream 准入
//! NEXUS_GATEWAY_PROBE=1 LIVE_MODELS=grok-4.7 \
//!   cargo run -p nexus-gateway --example live_matrix -- raw profile-ab
//! # 换成 Grok Bot 的票据跑同一组对照（token 只经环境变量传入，不落盘、不打印）
//! NEXUS_GATEWAY_PROBE=1 PROBE_CLIENT_TYPE=sand PROBE_ACCESS_TOKEN=… PROBE_MACHINE_ID=… \
//!   LIVE_MODELS=grok-4.7 cargo run -p nexus-gateway --example live_matrix -- raw profile-ab
//! NEXUS_GATEWAY_PROBE=1 LIVE_MODELS=claude-sonnet-5,gpt-5.6-sol \
//!   cargo run -p nexus-gateway --example live_matrix -- matrix [用例名过滤…]
//! ```
//!
//! - `status`：账号与 token 有效期，不发请求。
//! - `catalog`：`AiService/AvailableModels`——这个号能用哪些模型、各自的参数定义（思考 / 档位 /
//!   fast）、变体与 legacy slug、上下文上限。模型映射表该照着它改，而不是凭记忆。
//! - `raw <实验>`：绕过请求构造，直接改 `InferenceStreamRequest` 试协议的某个形状，打印上游
//!   回的每一类帧。用来在动代码之前先拿到证据。`profile-ab` 是其中的单变量对照：其余全部固定，
//!   只切 `requested_model` 的 max/effort/fast，输出一张可比对的准入表。
//! - `matrix`：走网关真正的 `inference::stream`，逐模型逐用例判 PASS / FAIL。
//!
//! 和 `probe` 一样**不刷 refresh_token**：Cursor 会轮换它，从外面刷一次可能把 IDE 登出。
//! 每条用例都很短（几百 token），缓存用例两发各约 2k token。会花这个号的额度。

use base64::Engine as _;
use nexus_cursor::Cursor;
use nexus_gateway::connect::{self, StreamItem};
use nexus_gateway::headers::{ai_headers, HeaderList, RequestNonce};
use nexus_gateway::proto::inference_content_part::Part;
use nexus_gateway::proto::inference_core_message::Content;
use nexus_gateway::proto::inference_stream_response::Response;
use nexus_gateway::proto::*;
use nexus_gateway::{
    inference, ChatRequest, Completion, Delta, DeviceIdentity, ImageInput, Message, Role, Sampling,
    StreamConfig, ToolCall, ToolChoice, ToolDef, ToolResult, UpstreamError,
};
use prost::Message as _;
use std::collections::BTreeMap;
use std::time::Instant;

// ---------- 账号 ----------

struct Live {
    client: reqwest::Client,
    cfg: StreamConfig,
    token: String,
    identity: DeviceIdentity,
    email: String,
    exp: Option<i64>,
}

fn jwt_exp_secs(jwt: &str) -> Option<i64> {
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()?
        .get("exp")?
        .as_i64()
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

impl Live {
    fn load() -> Self {
        let (token, email, identity) = match std::env::var("PROBE_ACCESS_TOKEN") {
            Ok(t) if !t.trim().is_empty() => {
                let t = t.trim().to_string();
                let id = match std::env::var("PROBE_MACHINE_ID") {
                    Ok(machine) if machine.trim().len() == 64 => {
                        DeviceIdentity::pinned(&t, machine.trim().to_string())
                    }
                    Ok(_) => panic!("PROBE_MACHINE_ID 必须是 64 位 machineId"),
                    Err(_) => DeviceIdentity::derived(&t),
                };
                (t, "(PROBE_ACCESS_TOKEN)".to_string(), id)
            }
            _ => {
                let cursor = Cursor::detect().expect("找不到本机 Cursor");
                let auth = cursor.state.read_auth().expect("读不到 Cursor 登录态");
                let token = auth
                    .get("cursorAuth/accessToken")
                    .filter(|t| !t.is_empty())
                    .expect("Cursor 当前没登录（没有 accessToken）")
                    .to_string();
                let email = auth.email().unwrap_or_else(|| "(unknown)".into());
                let machine = cursor.machine.read();
                let identity = if machine.machine_id.len() == 64 {
                    DeviceIdentity::pinned(&token, machine.machine_id.clone())
                } else {
                    DeviceIdentity::derived(&token)
                };
                (token, email, identity)
            }
        };
        let mut cfg = StreamConfig::default();
        if let Ok(ct) = std::env::var("PROBE_CLIENT_TYPE") {
            cfg.client_type = ct;
        }
        let exp = jwt_exp_secs(&token);
        // `LIVE_HTTP1=1`：和官方 agent-host 的 backendTransport 一样走 HTTP/1.1。
        let client = if std::env::var("LIVE_HTTP1").is_ok() {
            reqwest::Client::builder()
                .http1_only()
                .no_proxy()
                .connect_timeout(std::time::Duration::from_secs(15))
                .build()
                .expect("reqwest")
        } else {
            inference::http_client()
        };
        Self {
            client,
            cfg,
            token,
            identity,
            email,
            exp,
        }
    }

    fn headers(&self) -> HeaderList {
        ai_headers(
            &self.token,
            &self.identity,
            &self.cfg.client_type,
            RequestNonce::now(),
        )
    }

    fn require_fresh(&self) {
        if let Some(exp) = self.exp {
            if exp <= now_secs() {
                eprintln!(
                    "accessToken 已过期（{} 秒前）。在 Cursor 里随便用一下让它自己刷新，再跑。",
                    now_secs() - exp
                );
                std::process::exit(3);
            }
        }
    }

    async fn chat(&self, req: &ChatRequest) -> (Vec<Delta>, Result<Completion, UpstreamError>) {
        let mut deltas = Vec::new();
        let mut sink = |d: Delta| deltas.push(d);
        let r = inference::stream(
            &self.client,
            &self.cfg,
            &self.token,
            &self.identity,
            req,
            &mut sink,
        )
        .await;
        (deltas, r)
    }
}

// ---------- 低层：直接打 InferenceStreamRequest ----------

/// 上游回来的东西，按类摘要。
#[derive(Debug, Default)]
struct Raw {
    frames: BTreeMap<&'static str, usize>,
    text: String,
    thinking: String,
    stream_signatures: Vec<String>,
    tool_parts: Vec<(String, String, String, bool)>,
    info_models: Vec<String>,
    /// (is_redacted, 正文长度, 有签名, model_name, 密文长度)
    info_reasoning: Vec<(bool, usize, bool, Option<String>, usize)>,
    info_errors: Vec<String>,
    info_extra: Vec<String>,
    extended_usage: Option<InferenceExtendedUsageInfo>,
    usage: Option<InferenceUsageInfo>,
    provider_metadata: Vec<serde_json::Value>,
    stream_errors: Vec<String>,
    end_error: Option<String>,
    failure: Option<String>,
    ttft_ms: Option<u128>,
    total_ms: u128,
}

impl Raw {
    fn ok(&self) -> bool {
        self.failure.is_none() && self.end_error.is_none() && self.stream_errors.is_empty()
    }

    fn print(&self, label: &str) {
        println!("── {label} ──");
        if let Some(f) = &self.failure {
            println!("  失败：{f}");
            return;
        }
        println!(
            "  帧：{:?}  首字 {:?}ms 总 {}ms",
            self.frames, self.ttft_ms, self.total_ms
        );
        if !self.thinking.is_empty() {
            println!(
                "  思考 {} 字符；流内签名 {} 个{}",
                self.thinking.chars().count(),
                self.stream_signatures.len(),
                self.stream_signatures
                    .first()
                    .map(|s| format!("（首个 {} 字节，前缀 {}…）", s.len(), &s[..s.len().min(12)]))
                    .unwrap_or_default()
            );
        }
        if !self.text.is_empty() {
            let t: String = self.text.chars().take(300).collect();
            println!("  正文：{t}");
        }
        for (id, name, args, complete) in &self.tool_parts {
            if *complete {
                println!("  工具调用：id={id:?} name={name} args={args}");
            }
        }
        if !self.info_models.is_empty() {
            println!("  ResponseInfo.model：{:?}", self.info_models);
        }
        for (red, len, sig, model, data) in &self.info_reasoning {
            println!(
                "  ResponseInfo 思考：redacted={red} 正文 {len} 字符 签名={sig} model_name={model:?} 密文 {data} 字节"
            );
        }
        for e in &self.info_extra {
            println!("  ResponseInfo：{e}");
        }
        for e in &self.info_errors {
            println!("  ResponseInfo.error_message：{e}");
        }
        if let Some(u) = &self.extended_usage {
            println!(
                "  extended_usage：in={} out={} cache_r={} cache_w={} max={}",
                u.input_tokens,
                u.output_tokens,
                u.cache_read_tokens,
                u.cache_write_tokens,
                u.max_tokens
            );
        }
        if let Some(u) = &self.usage {
            println!(
                "  usage：prompt={} completion={} total={:?}",
                u.prompt_tokens, u.completion_tokens, u.total_tokens
            );
        }
        for m in &self.provider_metadata {
            println!("  provider_metadata：{m}");
        }
        for e in &self.stream_errors {
            println!("  流内错误：{e}");
        }
        if let Some(e) = &self.end_error {
            println!("  流尾错误：{e}");
        }
    }
}

fn value_to_json(v: &prost_types::Value) -> serde_json::Value {
    use prost_types::value::Kind;
    match &v.kind {
        None | Some(Kind::NullValue(_)) => serde_json::Value::Null,
        Some(Kind::BoolValue(b)) => serde_json::Value::Bool(*b),
        Some(Kind::NumberValue(n)) => serde_json::json!(n),
        Some(Kind::StringValue(s)) => serde_json::Value::String(s.clone()),
        Some(Kind::ListValue(l)) => {
            serde_json::Value::Array(l.values.iter().map(value_to_json).collect())
        }
        Some(Kind::StructValue(s)) => struct_to_json(s),
    }
}

fn struct_to_json(s: &prost_types::Struct) -> serde_json::Value {
    serde_json::Value::Object(
        s.fields
            .iter()
            .map(|(k, v)| (k.clone(), value_to_json(v)))
            .collect(),
    )
}

impl Live {
    async fn raw(&self, req: &InferenceStreamRequest) -> Raw {
        let started = Instant::now();
        let mut out = Raw::default();
        let url = format!(
            "{}{}",
            self.cfg.base_url.trim_end_matches('/'),
            inference::STREAM_PATH
        );
        let mut stream = match connect::call_server_stream(
            &self.client,
            &url,
            &self.headers(),
            &req.encode_to_vec(),
        )
        .await
        {
            Ok(s) => s,
            Err(e) => {
                out.failure = Some(inference::failure_to_error(e).message);
                return out;
            }
        };
        loop {
            let item = match tokio::time::timeout(self.cfg.idle_timeout, stream.next()).await {
                Ok(Ok(i)) => i,
                Ok(Err(e)) => {
                    out.failure = Some(inference::failure_to_error(e).message);
                    break;
                }
                Err(_) => {
                    out.failure = Some("空闲超时".into());
                    break;
                }
            };
            match item {
                StreamItem::Eof => break,
                StreamItem::End(end) => {
                    if let Some(err) = end.error {
                        out.end_error = Some(inference::connect_error_to_upstream(err).message);
                    }
                    break;
                }
                StreamItem::Message(bytes) => {
                    let Ok(resp) = InferenceStreamResponse::decode(&bytes[..]) else {
                        out.failure = Some("响应解码失败".into());
                        break;
                    };
                    let Some(r) = resp.response else { continue };
                    let kind = match &r {
                        Response::TextPart(_) => "text",
                        Response::ToolCallPart(_) => "tool_call",
                        Response::Usage(_) => "usage",
                        Response::ResponseInfo(_) => "response_info",
                        Response::ExtendedUsage(_) => "extended_usage",
                        Response::ProviderMetadata(_) => "provider_metadata",
                        Response::InvocationId(_) => "invocation_id",
                        Response::Error(_) => "error",
                        Response::ThinkingPart(_) => "thinking",
                        Response::ImageDescriptions(_) => "image_descriptions",
                    };
                    *out.frames.entry(kind).or_default() += 1;
                    if out.ttft_ms.is_none() && matches!(kind, "text" | "thinking" | "tool_call") {
                        out.ttft_ms = Some(started.elapsed().as_millis());
                    }
                    match r {
                        Response::TextPart(p) => out.text.push_str(&p.text),
                        Response::ThinkingPart(p) => {
                            out.thinking.push_str(&p.text);
                            if let Some(s) = p.signature.filter(|s| !s.is_empty()) {
                                out.stream_signatures.push(s);
                            }
                        }
                        Response::ToolCallPart(p) => out.tool_parts.push((
                            p.tool_call_id,
                            p.tool_name,
                            p.args,
                            p.is_complete,
                        )),
                        Response::ResponseInfo(info) => {
                            if !info.model.is_empty() {
                                out.info_models.push(info.model.clone());
                            }
                            if let Some(e) = info.error_message.filter(|e| !e.is_empty()) {
                                out.info_errors.push(e);
                            }
                            if let Some(s) = info.supports_self_summary {
                                out.info_extra.push(format!("supports_self_summary={s}"));
                            }
                            for m in &info.messages {
                                for p in &m.reasoning_parts {
                                    out.info_reasoning.push((
                                        p.is_redacted,
                                        p.text.chars().count(),
                                        p.signature.as_deref().is_some_and(|s| !s.is_empty()),
                                        p.model_name.clone(),
                                        p.redacted_data.as_deref().map(str::len).unwrap_or(0),
                                    ));
                                }
                                if !m.tool_calls.is_empty() {
                                    out.info_extra.push(format!(
                                        "messages[] tool_calls ids={:?}",
                                        m.tool_calls
                                            .iter()
                                            .map(|t| t.tool_call_id.clone())
                                            .collect::<Vec<_>>()
                                    ));
                                }
                            }
                        }
                        Response::ExtendedUsage(u) => out.extended_usage = Some(u),
                        Response::Usage(u) => out.usage = Some(u),
                        Response::ProviderMetadata(m) => {
                            if let Some(s) = m.metadata {
                                out.provider_metadata.push(struct_to_json(&s));
                            }
                        }
                        Response::Error(e) => out.stream_errors.push(format!(
                            "type={} code={} input_limit={} output_limit={} {}",
                            e.error_type,
                            e.code,
                            e.is_input_token_limit_error,
                            e.is_output_token_limit_error,
                            e.message
                        )),
                        Response::InvocationId(_) | Response::ImageDescriptions(_) => {}
                    }
                }
            }
        }
        out.total_ms = started.elapsed().as_millis();
        out
    }
}

// ---------- 素材 ----------

/// 左右两色的 PNG（base64）。纯色图模型可能蒙对，两色加左右顺序就蒙不对了。
fn split_png(left: [u8; 3], right: [u8; 3]) -> String {
    let img =
        image::RgbImage::from_fn(96, 64, |x, _| image::Rgb(if x < 48 { left } else { right }));
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img)
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("PNG 编码");
    base64::engine::general_purpose::STANDARD.encode(buf.into_inner())
}

const BLUE: [u8; 3] = [0, 64, 255];
const YELLOW: [u8; 3] = [255, 220, 0];
const COLOR_QUESTION: &str = "Look at the image. Name the color of its left half and of its right half, as two lowercase English words separated by one space, and nothing else.";

fn says_blue_yellow(text: &str) -> bool {
    let t = text.to_lowercase();
    match (t.find("blue"), t.find("yellow")) {
        (Some(b), Some(y)) => b < y,
        _ => false,
    }
}

/// 最小的单页 PDF，正文一行字。交叉引用表按真实偏移写，严格的解析器也认。
fn tiny_pdf(text: &str) -> Vec<u8> {
    let content = format!("BT /F1 24 Tf 30 70 Td ({text}) Tj ET");
    let objs = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 420 160] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes());
    for off in offsets {
        out.extend(format!("{off:010} 00000 n \n").as_bytes());
    }
    out.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// 一段约 2k token、每次都一样的系统提示：缓存用例的前缀。
fn long_system() -> String {
    let mut s =
        String::from("You are a meticulous assistant. Follow the house rules below exactly.\n");
    for i in 0..120 {
        s.push_str(&format!(
            "Rule {i}: when a user mentions the number {i}, reply politely, keep answers short, and never invent facts about item {i}.\n"
        ));
    }
    s
}

fn text_msg(role: InferenceMessageRole, text: &str) -> InferenceCoreMessage {
    InferenceCoreMessage {
        role: role as i32,
        content: Some(Content::Text(text.to_string())),
        ..Default::default()
    }
}

fn parts_msg(role: InferenceMessageRole, parts: Vec<Part>) -> InferenceCoreMessage {
    InferenceCoreMessage {
        role: role as i32,
        content: Some(Content::Parts(InferenceContentParts {
            parts: parts
                .into_iter()
                .map(|p| InferenceContentPart { part: Some(p) })
                .collect(),
        })),
        ..Default::default()
    }
}

fn text_part(text: &str) -> Part {
    Part::Text(InferenceTextPart {
        text: text.to_string(),
        provider_options: None,
    })
}

fn image_part(data: String, mime: &str) -> Part {
    Part::Image(InferenceImagePart {
        data,
        mime_type: Some(mime.to_string()),
        provider_options: None,
    })
}

fn weather_tool() -> ToolDef {
    ToolDef {
        name: "get_weather".into(),
        description: "Get the current weather for a city.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": { "city": { "type": "string", "description": "City name" } },
            "required": ["city"]
        }),
        ..ToolDef::default()
    }
}

fn shot_tool() -> ToolDef {
    ToolDef {
        name: "take_screenshot".into(),
        description: "Capture the current screen and return it as an image.".into(),
        parameters: serde_json::json!({ "type": "object", "properties": {} }),
        ..ToolDef::default()
    }
}

/// 用网关自己的请求构造起一个底稿，再按实验改。
fn base_request(
    model: &str,
    messages: Vec<Message>,
    tools: Vec<ToolDef>,
) -> InferenceStreamRequest {
    let req = ChatRequest {
        model: model.to_string(),
        messages,
        tools,
        tool_choice: ToolChoice::Auto,
        sampling: Sampling {
            max_output_tokens: Some(400),
            ..Default::default()
        },
        ..Default::default()
    };
    inference::build_request_with(&req, &uuid_v4(), None)
}

fn uuid_v4() -> String {
    uuid::Uuid::new_v4().to_string()
}

// ---------- catalog：AiService/AvailableModels ----------

#[derive(Clone, PartialEq, prost::Message)]
struct AvailableModelsRequest {
    #[prost(bool, tag = "2")]
    include_long_context_models: bool,
    #[prost(bool, optional, tag = "5")]
    use_model_parameters: Option<bool>,
    #[prost(bool, optional, tag = "6")]
    include_hidden_models: Option<bool>,
    #[prost(bool, optional, tag = "11")]
    use_react_model_picker: Option<bool>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct AvailableModelsResponse {
    #[prost(string, repeated, tag = "1")]
    model_names: Vec<String>,
    #[prost(message, repeated, tag = "2")]
    models: Vec<AvailableModel>,
    #[prost(bool, tag = "11")]
    use_model_parameters: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
struct AvailableModel {
    #[prost(string, tag = "1")]
    name: String,
    #[prost(bool, tag = "2")]
    default_on: bool,
    #[prost(bool, optional, tag = "5")]
    supports_agent: Option<bool>,
    #[prost(bool, optional, tag = "9")]
    supports_thinking: Option<bool>,
    #[prost(bool, optional, tag = "10")]
    supports_images: Option<bool>,
    #[prost(bool, optional, tag = "14")]
    supports_max_mode: Option<bool>,
    #[prost(int32, optional, tag = "15")]
    context_token_limit: Option<i32>,
    #[prost(int32, optional, tag = "16")]
    context_token_limit_for_max_mode: Option<i32>,
    #[prost(string, optional, tag = "17")]
    client_display_name: Option<String>,
    #[prost(string, optional, tag = "18")]
    server_model_name: Option<String>,
    #[prost(bool, optional, tag = "19")]
    supports_non_max_mode: Option<bool>,
    #[prost(message, repeated, tag = "29")]
    parameter_definitions: Vec<ParamDef>,
    #[prost(message, repeated, tag = "30")]
    variants: Vec<Variant>,
    #[prost(bool, optional, tag = "35")]
    is_hidden: Option<bool>,
    #[prost(string, repeated, tag = "36")]
    legacy_slugs: Vec<String>,
    #[prost(string, repeated, tag = "37")]
    id_aliases: Vec<String>,
    #[prost(string, optional, tag = "41")]
    vendor_name: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ParamDef {
    #[prost(string, tag = "1")]
    id: String,
    #[prost(string, tag = "2")]
    name: String,
    #[prost(message, optional, tag = "4")]
    parameter_type: Option<ParamType>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ParamType {
    #[prost(message, optional, tag = "1")]
    boolean_parameter: Option<ParamValues>,
    #[prost(message, optional, tag = "2")]
    enum_parameter: Option<ParamValues>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ParamValues {
    #[prost(message, repeated, tag = "1")]
    values: Vec<ParamValue>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct ParamValue {
    #[prost(string, tag = "1")]
    value: String,
    #[prost(string, optional, tag = "2")]
    display_name: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Variant {
    #[prost(message, repeated, tag = "1")]
    parameter_values: Vec<Kv>,
    #[prost(string, tag = "2")]
    display_name: String,
    #[prost(bool, tag = "3")]
    is_max_mode: bool,
    #[prost(bool, optional, tag = "4")]
    is_default_max_config: Option<bool>,
    #[prost(bool, optional, tag = "5")]
    is_default_non_max_config: Option<bool>,
    #[prost(string, optional, tag = "9")]
    variant_string_representation: Option<String>,
    #[prost(string, optional, tag = "11")]
    legacy_slug: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct Kv {
    #[prost(string, tag = "1")]
    id: String,
    #[prost(string, tag = "2")]
    value: String,
}

async fn catalog(live: &Live, filter: Option<&str>) {
    let req = AvailableModelsRequest {
        include_long_context_models: true,
        use_model_parameters: Some(true),
        include_hidden_models: Some(std::env::var("LIVE_HIDDEN").is_ok()),
        use_react_model_picker: Some(true),
    };
    let url = format!(
        "{}/aiserver.v1.AiService/AvailableModels",
        live.cfg.base_url.trim_end_matches('/')
    );
    let bytes = match connect::call_unary(&live.client, &url, &live.headers(), &req.encode_to_vec())
        .await
    {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "AvailableModels 失败：{}",
                inference::failure_to_error(e).message
            );
            std::process::exit(1);
        }
    };
    let resp = AvailableModelsResponse::decode(&bytes[..]).expect("AvailableModelsResponse 解码");
    println!(
        "模型 {} 个（model_names {} 个，use_model_parameters={}）",
        resp.models.len(),
        resp.model_names.len(),
        resp.use_model_parameters
    );
    for m in &resp.models {
        if filter.is_some_and(|f| !m.name.contains(f)) {
            continue;
        }
        println!(
            "\n● {}  「{}」 vendor={} server={:?} default_on={} hidden={:?}",
            m.name,
            m.client_display_name.as_deref().unwrap_or(""),
            m.vendor_name.as_deref().unwrap_or("?"),
            m.server_model_name,
            m.default_on,
            m.is_hidden
        );
        println!(
            "  agent={:?} thinking={:?} images={:?} max={:?} non_max={:?} ctx={:?} ctx_max={:?}",
            m.supports_agent,
            m.supports_thinking,
            m.supports_images,
            m.supports_max_mode,
            m.supports_non_max_mode,
            m.context_token_limit,
            m.context_token_limit_for_max_mode
        );
        if !m.id_aliases.is_empty() {
            println!("  别名：{:?}", m.id_aliases);
        }
        for p in &m.parameter_definitions {
            let (kind, values) = match &p.parameter_type {
                Some(ParamType {
                    boolean_parameter: Some(v),
                    ..
                }) => ("bool", &v.values),
                Some(ParamType {
                    enum_parameter: Some(v),
                    ..
                }) => ("enum", &v.values),
                _ => ("?", &Vec::new()),
            };
            println!(
                "  参数 {}（{}，{kind}）：{:?}",
                p.id,
                p.name,
                values.iter().map(|v| v.value.as_str()).collect::<Vec<_>>()
            );
        }
        for v in &m.variants {
            let kv: Vec<String> = v
                .parameter_values
                .iter()
                .map(|p| format!("{}={}", p.id, p.value))
                .collect();
            println!(
                "  变体 {:<34} max={} 默认(max/非max)={:?}/{:?} legacy={:?} repr={:?} 「{}」",
                kv.join(","),
                v.is_max_mode,
                v.is_default_max_config,
                v.is_default_non_max_config,
                v.legacy_slug,
                v.variant_string_representation,
                v.display_name
            );
        }
        if !m.legacy_slugs.is_empty() {
            println!("  legacy_slugs：{:?}", m.legacy_slugs);
        }
    }
}

// ---------- raw 实验 ----------

async fn raw_experiment(live: &Live, name: &str, model: &str) {
    match name {
        // system：折进首条 user（现状） vs 原生 role=SYSTEM（官方 3.21.13 本地 loop 的做法）。
        "system-fold" | "system-role" => {
            let sys = "Secret codeword: ZEBRA-17. Whenever asked for the codeword, reply with exactly the codeword and nothing else.";
            let mut req = base_request(
                model,
                vec![
                    Message::text(Role::System, sys),
                    Message::text(Role::User, "What is the codeword?"),
                ],
                vec![],
            );
            if name == "system-role" {
                req.messages = vec![
                    text_msg(InferenceMessageRole::System, sys),
                    text_msg(InferenceMessageRole::User, "What is the codeword?"),
                ];
            }
            let r = live.raw(&req).await;
            r.print(&format!("{name} · {model}"));
            println!(
                "  判据（正文含 ZEBRA-17）：{}",
                r.ok() && r.text.contains("ZEBRA-17")
            );
        }
        // 用户消息里的图：裸 base64（现状） / data URL（官方客户端拿到字节时的写法） / https 链接。
        "image-raw" | "image-dataurl" | "image-url" => {
            let png = split_png(BLUE, YELLOW);
            let data = match name {
                "image-raw" => png,
                "image-dataurl" => format!("data:image/png;base64,{png}"),
                _ => std::env::var("LIVE_IMAGE_URL").unwrap_or_else(|_| {
                    "https://upload.wikimedia.org/wikipedia/commons/thumb/4/47/PNG_transparency_demonstration_1.png/280px-PNG_transparency_demonstration_1.png".into()
                }),
            };
            let question = if name == "image-url" {
                "What objects are in this image? Answer in at most five words."
            } else {
                COLOR_QUESTION
            };
            let mut req = base_request(model, vec![Message::text(Role::User, "x")], vec![]);
            req.messages = vec![parts_msg(
                InferenceMessageRole::User,
                vec![text_part(question), image_part(data, "image/png")],
            )];
            let r = live.raw(&req).await;
            r.print(&format!("{name} · {model}"));
            if name != "image-url" {
                println!(
                    "  判据（左蓝右黄）：{}",
                    r.ok() && says_blue_yellow(&r.text)
                );
            }
        }
        // 工具结果里的图：experimental_content（官方形状），以及只给 result 不给图的对照。
        "tool-image" | "tool-image-none" => {
            let png = split_png(BLUE, YELLOW);
            let mut req = base_request(
                model,
                vec![Message::text(Role::User, "x")],
                vec![shot_tool()],
            );
            let call_id = "toolu_live_shot_1";
            let mut result = InferenceToolResultPart {
                tool_call_id: call_id.into(),
                tool_name: "take_screenshot".into(),
                result: Some(prost_types::Value {
                    kind: Some(prost_types::value::Kind::StringValue(
                        "Screenshot captured.".into(),
                    )),
                }),
                ..Default::default()
            };
            if name == "tool-image" {
                result.experimental_content = vec![
                    InferenceContentPart {
                        part: Some(text_part("Screenshot captured.")),
                    },
                    InferenceContentPart {
                        part: Some(image_part(png, "image/png")),
                    },
                ];
            }
            req.messages = vec![
                text_msg(
                    InferenceMessageRole::User,
                    &format!("Take a screenshot, then answer: {COLOR_QUESTION}"),
                ),
                InferenceCoreMessage {
                    role: InferenceMessageRole::Assistant as i32,
                    tool_calls: vec![InferenceToolCall {
                        tool_call_id: call_id.into(),
                        tool_name: "take_screenshot".into(),
                        args: Some(prost_types::Struct::default()),
                        raw_tool_call_args: Some("{}".into()),
                    }],
                    ..Default::default()
                },
                InferenceCoreMessage {
                    role: InferenceMessageRole::Tool as i32,
                    content: Some(Content::ToolContent(InferenceToolResultContent {
                        parts: vec![result],
                    })),
                    ..Default::default()
                },
            ];
            let r = live.raw(&req).await;
            r.print(&format!("{name} · {model}"));
            println!(
                "  判据（左蓝右黄）：{}",
                r.ok() && says_blue_yellow(&r.text)
            );
        }
        // PDF：InferenceFilePart（协议里有，官方客户端却换成 `[File: name]` 占位）。
        "pdf-file" | "pdf-tool" => {
            let pdf =
                base64::engine::general_purpose::STANDARD.encode(tiny_pdf("CODE PINEAPPLE42"));
            let file = Part::File(InferenceFilePart {
                data: pdf,
                media_type: "application/pdf".into(),
                filename: Some("code.pdf".into()),
                provider_options: None,
            });
            let ask = "The attached PDF contains a code. Reply with the code only.";
            let mut req = base_request(model, vec![Message::text(Role::User, "x")], vec![]);
            req.messages = if name == "pdf-file" {
                vec![parts_msg(
                    InferenceMessageRole::User,
                    vec![text_part(ask), file],
                )]
            } else {
                vec![
                    text_msg(InferenceMessageRole::User, ask),
                    InferenceCoreMessage {
                        role: InferenceMessageRole::Assistant as i32,
                        tool_calls: vec![InferenceToolCall {
                            tool_call_id: "toolu_live_read_1".into(),
                            tool_name: "read_file".into(),
                            args: Some(prost_types::Struct::default()),
                            raw_tool_call_args: Some(r#"{"path":"code.pdf"}"#.into()),
                        }],
                        ..Default::default()
                    },
                    InferenceCoreMessage {
                        role: InferenceMessageRole::Tool as i32,
                        content: Some(Content::ToolContent(InferenceToolResultContent {
                            parts: vec![InferenceToolResultPart {
                                tool_call_id: "toolu_live_read_1".into(),
                                tool_name: "read_file".into(),
                                result: Some(prost_types::Value {
                                    kind: Some(prost_types::value::Kind::StringValue(
                                        "PDF attached.".into(),
                                    )),
                                }),
                                experimental_content: vec![InferenceContentPart {
                                    part: Some(file),
                                }],
                                ..Default::default()
                            }],
                        })),
                        ..Default::default()
                    },
                ]
            };
            if name == "pdf-tool" {
                req.tools = base_request(model, vec![], vec![ToolDef {
                    name: "read_file".into(),
                    description: "Read a file".into(),
                    parameters: serde_json::json!({"type":"object","properties":{"path":{"type":"string"}}}),
                    ..ToolDef::default()
                }])
                .tools;
            }
            let r = live.raw(&req).await;
            r.print(&format!("{name} · {model}"));
            println!(
                "  判据（PINEAPPLE42）：{}",
                r.ok() && r.text.contains("PINEAPPLE42")
            );
        }
        // 思考：签名从哪来（流内 ThinkingPart.signature / ResponseInfo.reasoning_parts）。
        "thinking" => {
            let req = base_request(
                model,
                vec![Message::text(
                    Role::User,
                    "A bat and a ball cost $1.10 in total. The bat costs $1.00 more than the ball. How much is the ball? Think it through, then answer in one short sentence.",
                )],
                vec![],
            );
            live.raw(&req).await.print(&format!("thinking · {model}"));
        }
        // 思考回放：第一发要一个工具调用并收下思考与签名，第二发把它们按 reasoning_parts 带回去。
        // `replay-bogus` 故意带一个假签名，看上游怎么反应（决定我们过滤占位签名有多必要）。
        "replay" | "replay-bogus" | "replay-none" => {
            let tools = vec![weather_tool()];
            let ask = "What's the weather in Paris right now? Use the tool.";
            let first = base_request(model, vec![Message::text(Role::User, ask)], tools.clone());
            let r1 = live.raw(&first).await;
            r1.print(&format!("{name} 第一发 · {model}"));
            let Some((id, tool_name, args, _)) = r1.tool_parts.iter().rev().find(|t| t.3).cloned()
            else {
                println!("  第一发没出工具调用，停");
                return;
            };
            let sig = r1.stream_signatures.last().cloned();
            let reasoning = match name {
                "replay-none" => Vec::new(),
                "replay-bogus" => vec![InferenceReasoningPart {
                    text: r1.thinking.clone(),
                    signature: Some("EqQBCkYIBxgCKkBdefinitelynotarealsignature".into()),
                    ..Default::default()
                }],
                _ => match sig {
                    Some(s) => vec![InferenceReasoningPart {
                        text: r1.thinking.clone(),
                        signature: Some(s),
                        ..Default::default()
                    }],
                    None => {
                        println!("  第一发没有签名：回放无从谈起（这个模型不签）");
                        Vec::new()
                    }
                },
            };
            let mut second = first.clone();
            second.messages.push(InferenceCoreMessage {
                role: InferenceMessageRole::Assistant as i32,
                content: if r1.text.trim().is_empty() {
                    None
                } else {
                    Some(Content::Text(r1.text.clone()))
                },
                tool_calls: vec![InferenceToolCall {
                    tool_call_id: id.clone(),
                    tool_name: tool_name.clone(),
                    args: serde_json::from_str::<serde_json::Value>(&args)
                        .ok()
                        .map(|v| inference::struct_from_json(&v)),
                    raw_tool_call_args: Some(args.clone()),
                }],
                reasoning_parts: reasoning,
                ..Default::default()
            });
            second.messages.push(InferenceCoreMessage {
                role: InferenceMessageRole::Tool as i32,
                content: Some(Content::ToolContent(InferenceToolResultContent {
                    parts: vec![InferenceToolResultPart {
                        tool_call_id: id,
                        tool_name,
                        result: Some(prost_types::Value {
                            kind: Some(prost_types::value::Kind::StringValue(
                                "Paris: 17°C, light rain, wind 12 km/h (station LIVE-8841).".into(),
                            )),
                        }),
                        ..Default::default()
                    }],
                })),
                ..Default::default()
            });
            let r2 = live.raw(&second).await;
            r2.print(&format!("{name} 第二发 · {model}"));
            println!(
                "  判据（第二发成功且引用了工具结果 17°C）：{}",
                r2.ok() && r2.text.contains("17")
            );
        }
        // 缓存：同一前缀、同一 conversation_id 连发两次，看第二次的 cache_read。
        "cache" | "cache-newconv" => {
            let sys = long_system();
            let mut req = base_request(
                model,
                vec![
                    Message::text(Role::System, &sys),
                    Message::text(Role::User, "Mention rule 7 in one short sentence."),
                ],
                vec![],
            );
            let r1 = live.raw(&req).await;
            r1.print(&format!("{name} 第一发 · {model}"));
            if name == "cache-newconv" {
                req.conversation_id = Some(uuid_v4());
            }
            let r2 = live.raw(&req).await;
            r2.print(&format!("{name} 第二发 · {model}"));
        }
        // 参数：requested_model.parameters（catalog 里看到的 id / value），对照 legacy slug。
        "params" => {
            let spec = std::env::var("LIVE_PARAMS").unwrap_or_default();
            let mut req = base_request(
                model,
                vec![Message::text(Role::User, "Reply with the single word: ok")],
                vec![],
            );
            let params: Vec<InferenceModelParameterValue> = spec
                .split(',')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| InferenceModelParameterValue {
                    id: k.trim().into(),
                    value: v.trim().into(),
                })
                .collect();
            if let Some(rm) = req.requested_model.as_mut() {
                rm.parameters = params;
                rm.max_mode = std::env::var("LIVE_MAX").is_ok();
            }
            if std::env::var("LIVE_NO_MODEL_ID").is_ok() {
                req.model_id = None;
            }
            live.raw(&req)
                .await
                .print(&format!("params {spec:?} · {model}"));
        }
        // profile A/B：固定其余一切，只切 `requested_model` 的 max_mode 与 parameters。
        //
        // 0.65 本地 host 的默认模型选择可用 `grok-4.7` + max + effort=high + fast=true。
        // 当前成功 agent 全走云端 Temporal，无法证明该 profile 被实际使用；这里仅判断这组值
        // 是否会改变裸 Stream 准入。此前 pod 对照同时换了执行位置、系统提示、工具、会话 id
        // 与 headers，任何一项都能解释结果，所以证不出因果。
        // 这里把它收敛成单变量：同一 token / identity / headers / 消息 / sampling / conversation，
        // 只有 profile 不同。四档拆开是为了分辨「max 的作用」和「parameters 的作用」。
        "profile-ab" => {
            let conversation = uuid_v4();
            let base = inference::build_request_with(
                &ChatRequest {
                    model: model.to_string(),
                    messages: vec![Message::text(Role::User, "Reply with exactly: ok")],
                    sampling: Sampling {
                        max_output_tokens: Some(16),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                &conversation,
                None,
            );
            type ProfileVariant<'a> = (&'a str, bool, Vec<(&'a str, &'a str)>);
            let only = std::env::var("LIVE_AB_VARIANTS").ok();
            let mut order: Vec<ProfileVariant<'_>> = [
                ("bare", false, vec![]),
                ("max", true, vec![]),
                ("params", false, vec![("effort", "high"), ("fast", "true")]),
                ("full", true, vec![("effort", "high"), ("fast", "true")]),
            ]
            .into_iter()
            .filter(|(name, _, _)| {
                only.as_deref()
                    .is_none_or(|o| o.split(',').any(|x| x == *name))
            })
            .collect();
            // 顺序本身是个混杂项（限流、冷却都可能只罚第二发）。反向再跑一遍，结论一致才算稳。
            if std::env::var("LIVE_AB_REVERSE").is_ok() {
                order.reverse();
            }
            println!("── profile-ab · {model} ──");
            println!(
                "  固定：token / identity / client-type {} / 消息 / sampling / conversation={conversation}",
                live.cfg.client_type
            );
            println!(
                "  仅变：requested_model.max_mode 与 parameters · 顺序 {:?}（LIVE_AB_REVERSE=1 反向复测）",
                order.iter().map(|(n, _, _)| *n).collect::<Vec<_>>()
            );
            let mut verdicts: Vec<(&str, bool, Option<String>)> = Vec::new();
            for (name, max_mode, params) in &order {
                let mut req = base.clone();
                if let Some(rm) = req.requested_model.as_mut() {
                    rm.max_mode = *max_mode;
                    rm.parameters = params
                        .iter()
                        .map(|(id, value)| InferenceModelParameterValue {
                            id: (*id).to_string(),
                            value: (*value).to_string(),
                        })
                        .collect();
                }
                let r = live.raw(&req).await;
                r.print(&format!(
                    "profile-ab/{name} · {model} · max_mode={max_mode} parameters={params:?}"
                ));
                let reason = r
                    .failure
                    .clone()
                    .or_else(|| r.end_error.clone())
                    .or_else(|| r.stream_errors.first().cloned());
                verdicts.push((*name, r.ok(), reason));
            }
            println!("  ── profile 判据 ──");
            for (name, ok, reason) in &verdicts {
                println!(
                    "  {name:<7}{}{}",
                    if *ok { "通过" } else { "被拒" },
                    reason
                        .as_deref()
                        .map(|r| format!(" · {r}"))
                        .unwrap_or_default()
                );
            }
            let passed: Vec<&str> = verdicts
                .iter()
                .filter(|(_, ok, _)| *ok)
                .map(|(n, _, _)| *n)
                .collect();
            println!(
                "  结论：{}",
                if passed.len() == verdicts.len() {
                    "profile 不影响准入（每一档都通过）".to_string()
                } else if passed.is_empty() {
                    "profile 不是差异来源（每一档都被拒，准入卡在别处）".to_string()
                } else {
                    format!("profile 影响准入：只有 {passed:?} 通过")
                }
            );
        }
        "runinference" | "runinference-finish" => {
            println!("── {name} · {model} ──");
            run_inference_once(live, model, name.ends_with("finish")).await;
        }
        // 鉴权：同一发最小请求，换几种请求头形态，看 Stream 认哪一种。
        "auth" => {
            let req = base_request(
                model,
                vec![Message::text(Role::User, "Reply with: ok")],
                vec![],
            );
            let mac = Cursor::detect()
                .map(|c| c.machine.read().mac_machine_id)
                .unwrap_or_default();
            let only = std::env::var("LIVE_AUTH_VARIANTS").ok();
            let variants: Vec<&str> = [
                "base",
                "mac",
                "ide",
                "ide-mac",
                "stream",
                "cookie",
                "all",
                "derived",
                "derived-mac",
                "cli-new",
            ]
            .into_iter()
            .filter(|v| {
                only.as_deref()
                    .is_none_or(|o| o.split(',').any(|x| x == *v))
            })
            .collect();
            for v in variants {
                let identity = if v.starts_with("derived") {
                    DeviceIdentity::derived(&live.token)
                } else {
                    live.identity.clone()
                };
                let ide = matches!(v, "ide" | "ide-mac" | "all");
                let mut h = ai_headers(
                    &live.token,
                    &identity,
                    if ide { "ide" } else { &live.cfg.client_type },
                    RequestNonce::now(),
                );
                let set = |h: &mut HeaderList, k: &'static str, val: String| match h
                    .iter_mut()
                    .find(|(name, _)| *name == k)
                {
                    Some(slot) => slot.1 = val,
                    None => h.push((k, val)),
                };
                if ide {
                    set(&mut h, "x-cursor-client-version", "3.21.13".into());
                }
                if v == "cli-new" {
                    let ver = std::env::var("LIVE_CLI_VERSION")
                        .unwrap_or_else(|_| "cli-2026.09.28-64d2043".into());
                    set(&mut h, "x-cursor-client-version", ver);
                }
                let with_mac = matches!(
                    v,
                    "mac" | "ide-mac" | "stream" | "cookie" | "all" | "derived-mac"
                );
                if with_mac {
                    let mac_id = if v == "derived-mac" {
                        nexus_gateway::identity::sha256_hex(format!(
                            "{}macMachineId",
                            identity.account
                        ))
                    } else {
                        mac.clone()
                    };
                    let base = identity.checksum(now_secs() as u64 * 1000);
                    set(&mut h, "x-cursor-checksum", format!("{base}/{mac_id}"));
                }
                if matches!(v, "stream" | "all") {
                    set(&mut h, "x-cursor-streaming", "true".into());
                }
                if matches!(v, "cookie" | "all") {
                    let head: String = live.token.chars().take(15).collect();
                    set(&mut h, "cookie", format!("CursorCookie=Cookie-{head}"));
                }
                if v == "all" {
                    set(&mut h, "x-cursor-client-layout", "editor".into());
                    set(
                        &mut h,
                        "x-cursor-client-commit",
                        "e44a49c17e334d442e58bbde931d791200f014a0".into(),
                    );
                }
                let url = format!(
                    "{}{}",
                    live.cfg.base_url.trim_end_matches('/'),
                    inference::STREAM_PATH
                );
                let verdict =
                    match connect::call_server_stream(&live.client, &url, &h, &req.encode_to_vec())
                        .await
                    {
                        Err(e) => format!("连接失败：{}", inference::failure_to_error(e).message),
                        Ok(mut s) => {
                            let mut text = String::new();
                            let mut err = None;
                            loop {
                                match s.next().await {
                                    Ok(StreamItem::Message(b)) => {
                                        if let Ok(InferenceStreamResponse {
                                            response: Some(Response::TextPart(p)),
                                        }) = InferenceStreamResponse::decode(&b[..])
                                        {
                                            text.push_str(&p.text);
                                        }
                                    }
                                    Ok(StreamItem::End(end)) => {
                                        if std::env::var("LIVE_VERBOSE").is_ok() {
                                            eprintln!(
                                                "    流尾原文：{:?} metadata={}",
                                                end.error, end.metadata
                                            );
                                        }
                                        err = end.error.map(|e| {
                                            inference::connect_error_to_upstream(e).message
                                        });
                                        break;
                                    }
                                    Ok(StreamItem::Eof) => break,
                                    Err(e) => {
                                        err = Some(inference::failure_to_error(e).message);
                                        break;
                                    }
                                }
                            }
                            match err {
                                Some(e) => format!("✗ {e}"),
                                None => format!("✓ {text:?}"),
                            }
                        }
                    };
                println!("  auth {v:<12} · {model}: {verdict}");
            }
        }
        other => {
            eprintln!("没有这个实验：{other}");
            std::process::exit(2);
        }
    }
}

// ---------- RunInference（BiDi）：官方本地 loop 的推理会话 ----------

#[derive(Clone, PartialEq, prost::Message)]
struct RiClientMessage {
    #[prost(oneof = "ri_client::Message", tags = "1, 2, 3, 4")]
    message: Option<ri_client::Message>,
}

mod ri_client {
    // prost oneof 的内存布局由 wire message 直接决定；这里只有 live probe，按生成形状保留。
    #[allow(clippy::large_enum_variant)]
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Message {
        #[prost(message, tag = "1")]
        RunRequest(super::RiRunRequest),
        #[prost(message, tag = "2")]
        InvokeModel(super::RiInvokeModel),
        #[prost(message, tag = "3")]
        CancelInvocation(super::RiInvocationId),
        #[prost(message, tag = "4")]
        FinishRun(super::RiEmpty),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
struct RiEmpty {}

#[derive(Clone, PartialEq, prost::Message)]
struct RiInvocationId {
    #[prost(string, tag = "1")]
    invocation_id: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct RiRunRequest {
    #[prost(string, tag = "1")]
    conversation_id: String,
    #[prost(string, optional, tag = "2")]
    conversation_group_id: Option<String>,
    #[prost(message, optional, tag = "3")]
    requested_model: Option<InferenceRequestedModel>,
    #[prost(message, repeated, tag = "4")]
    routing_conversation: Vec<RiRoutingMessage>,
    #[prost(string, optional, tag = "5")]
    agent_mode: Option<String>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct RiRoutingMessage {
    /// RunInferenceRoutingRole：1 USER，2 ASSISTANT。
    #[prost(int32, tag = "1")]
    role: i32,
    #[prost(string, tag = "2")]
    text: String,
}

#[derive(Clone, PartialEq, prost::Message)]
struct RiInvokeModel {
    #[prost(string, tag = "1")]
    invocation_id: String,
    #[prost(message, optional, tag = "2")]
    request: Option<InferenceStreamRequest>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct RiServerMessage {
    #[prost(oneof = "ri_server::Message", tags = "1, 2, 3, 4")]
    message: Option<ri_server::Message>,
}

mod ri_server {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub enum Message {
        #[prost(message, tag = "1")]
        Heartbeat(super::RiEmpty),
        #[prost(message, tag = "2")]
        RunReady(super::RiRunReady),
        #[prost(message, tag = "3")]
        InvocationResponse(super::RiInvocationResponse),
        #[prost(message, tag = "4")]
        InvocationEnd(super::RiInvocationEnd),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
struct RiRunReady {
    #[prost(message, optional, tag = "1")]
    resolved_model: Option<InferenceRequestedModel>,
    #[prost(bool, tag = "2")]
    supports_self_summary: bool,
    #[prost(string, optional, tag = "3")]
    routed_model_display_name: Option<String>,
    #[prost(message, optional, tag = "4")]
    prompt_model_metadata: Option<RiPromptMeta>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct RiPromptMeta {
    #[prost(string, tag = "1")]
    vendor: String,
    #[prost(string, tag = "2")]
    prompt_version: String,
    #[prost(string, optional, tag = "30")]
    reasoning_effort: Option<String>,
    #[prost(int32, optional, tag = "32")]
    agent_token_limit: Option<i32>,
    #[prost(int32, optional, tag = "33")]
    estimated_cache_ttl_ms: Option<i32>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct RiInvocationResponse {
    #[prost(string, tag = "1")]
    invocation_id: String,
    #[prost(message, optional, tag = "2")]
    response: Option<InferenceStreamResponse>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct RiInvocationEnd {
    #[prost(string, tag = "1")]
    invocation_id: String,
    #[prost(message, optional, tag = "2")]
    error: Option<RiInvocationError>,
}

#[derive(Clone, PartialEq, prost::Message)]
struct RiInvocationError {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
}

fn envelope(msg: &RiClientMessage) -> Vec<u8> {
    connect::Envelope::message(msg.encode_to_vec()).encode()
}

/// 半双工：请求体一次写完（run_request + invoke_model [+ finish_run]），再读响应流。
async fn run_inference_once(live: &Live, model: &str, with_finish: bool) {
    let conversation_id = uuid_v4();
    let mut inner = base_request(
        model,
        vec![Message::text(Role::User, "Reply with exactly: pong")],
        vec![],
    );
    // 官方客户端在 invokeModel 里把会话级的字段剥掉，交给 runRequest。
    let requested = inner.requested_model.take();
    inner.model_id = None;
    inner.conversation_id = None;
    let run = RiClientMessage {
        message: Some(ri_client::Message::RunRequest(RiRunRequest {
            conversation_id: conversation_id.clone(),
            conversation_group_id: Some(conversation_id.clone()),
            requested_model: requested,
            routing_conversation: vec![RiRoutingMessage {
                role: 1,
                text: "Reply with exactly: pong".into(),
            }],
            agent_mode: std::env::var("LIVE_AGENT_MODE").ok(),
        })),
    };
    let invocation = uuid_v4();
    let invoke = RiClientMessage {
        message: Some(ri_client::Message::InvokeModel(RiInvokeModel {
            invocation_id: invocation.clone(),
            request: Some(inner),
        })),
    };
    let mut body = envelope(&run);
    body.extend(envelope(&invoke));
    if with_finish {
        body.extend(envelope(&RiClientMessage {
            message: Some(ri_client::Message::FinishRun(RiEmpty {})),
        }));
    }
    let url = format!(
        "{}/aiserver.v1.InferenceService/RunInference",
        live.cfg.base_url.trim_end_matches('/')
    );
    let mut req = live
        .client
        .post(&url)
        .header("content-type", connect::CONTENT_TYPE_STREAM)
        .body(body);
    for (k, v) in live.headers() {
        req = req.header(k, v);
    }
    let started = Instant::now();
    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            println!("  RunInference 连接失败：{e}");
            return;
        }
    };
    println!(
        "  HTTP {} · {:?}",
        resp.status(),
        resp.headers().get("content-type")
    );
    let mut resp = resp;
    let mut dec = connect::EnvelopeDecoder::default();
    let mut text = String::new();
    let mut kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
    loop {
        let chunk =
            match tokio::time::timeout(std::time::Duration::from_secs(90), resp.chunk()).await {
                Ok(Ok(Some(c))) => c,
                Ok(Ok(None)) => break,
                Ok(Err(e)) => {
                    println!("  读流失败：{e}");
                    break;
                }
                Err(_) => {
                    println!("  90 秒没动静，停");
                    break;
                }
            };
        dec.push(&chunk);
        while let Ok(Some(env)) = dec.next_envelope() {
            if env.is_end_stream() {
                let tail = String::from_utf8_lossy(&env.data);
                println!("  流尾：{}", tail.chars().take(600).collect::<String>());
                continue;
            }
            match RiServerMessage::decode(&env.data[..]) {
                Ok(RiServerMessage { message: Some(m) }) => match m {
                    ri_server::Message::Heartbeat(_) => *kinds.entry("heartbeat").or_default() += 1,
                    ri_server::Message::RunReady(r) => {
                        *kinds.entry("run_ready").or_default() += 1;
                        println!(
                            "  run_ready：resolved={:?} params={:?} self_summary={} display={:?} meta={:?}",
                            r.resolved_model.as_ref().map(|m| m.model_id.clone()),
                            r.resolved_model.as_ref().map(|m| m.parameters.iter().map(|p| format!("{}={}", p.id, p.value)).collect::<Vec<_>>()),
                            r.supports_self_summary,
                            r.routed_model_display_name,
                            r.prompt_model_metadata
                        );
                    }
                    ri_server::Message::InvocationResponse(r) => {
                        *kinds.entry("invocation_response").or_default() += 1;
                        if let Some(InferenceStreamResponse {
                            response: Some(Response::TextPart(p)),
                        }) = r.response
                        {
                            text.push_str(&p.text);
                        }
                    }
                    ri_server::Message::InvocationEnd(e) => {
                        *kinds.entry("invocation_end").or_default() += 1;
                        println!("  invocation_end：error={:?}", e.error);
                    }
                },
                Ok(_) => *kinds.entry("empty").or_default() += 1,
                Err(e) => println!("  解码失败：{e}"),
            }
        }
    }
    println!(
        "  帧 {kinds:?} 正文 {text:?} 用时 {}ms",
        started.elapsed().as_millis()
    );
}

// ---------- pool：号池里哪些号开着 InferenceService ----------

fn mask(email: &str) -> String {
    match email.split_once('@') {
        Some((user, domain)) => format!("{}***@{domain}", user.chars().take(3).collect::<String>()),
        None => "***".into(),
    }
}

/// 每个号各打一发最小的 `Stream` 与 `RunInference`。凭证走 desktop 的号池（nexus.db，必要时
/// 由 nexus-accounts 续期并存回），token 不落日志。
async fn pool(live: &Live, filter: Option<&str>) {
    use nexus_gateway::lane::Source;
    use nexus_store::{Db, SecretStore, SqliteSecrets};
    let home = std::env::var("HOME").expect("HOME");
    let db_path = std::path::PathBuf::from(home)
        .join("Library/Application Support/com.roviix.nexus/nexus.db");
    let db = std::sync::Arc::new(Db::open(db_path).expect("打不开 nexus.db"));
    let secrets: std::sync::Arc<dyn SecretStore> =
        std::sync::Arc::new(SqliteSecrets::new(db.clone()));
    let accounts = std::sync::Arc::new(nexus_accounts::AccountsService::new(db, secrets));
    let source = nexus_gateway::StoredAccountsSource::new(accounts, live.cfg.client_type.clone());
    let candidates = source.candidates();
    println!("号池候选 {} 个", candidates.len());
    for c in candidates {
        if filter.is_some_and(|f| !c.label.contains(f)) {
            continue;
        }
        let token = match source.resolve(&c).await {
            Ok(t) => t.access_token,
            Err(e) => {
                println!("  {:<28} 取不到会话：{}", mask(&c.label), e.message);
                continue;
            }
        };
        let probe = Live {
            client: live.client.clone(),
            cfg: live.cfg.clone(),
            identity: DeviceIdentity::derived(&token),
            token,
            email: c.label.clone(),
            exp: None,
        };
        let req = base_request(
            "default",
            vec![Message::text(Role::User, "Reply with: ok")],
            vec![],
        );
        let stream = probe.raw(&req).await;
        let stream_verdict = if stream.ok() {
            format!(
                "Stream ✓ {:?} routed={:?}",
                stream.text.trim(),
                stream.info_models.last()
            )
        } else {
            format!(
                "Stream ✗ {}",
                stream
                    .end_error
                    .or(stream.failure)
                    .or_else(|| stream.stream_errors.first().cloned())
                    .unwrap_or_default()
            )
        };
        println!("  {:<28} {stream_verdict}", mask(&c.label));
        if std::env::var("LIVE_POOL_RUNINFERENCE").is_ok() {
            run_inference_once(&probe, "default", false).await;
        }
    }
}

// ---------- matrix：走网关真正的 inference::stream ----------

type CaseFuture<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = Verdict> + 'a>>;
type CaseRunner = for<'a> fn(&'a Live, &'a str) -> CaseFuture<'a>;

struct Case {
    name: &'static str,
    run: CaseRunner,
}

struct Verdict {
    pass: bool,
    note: String,
}

impl Verdict {
    fn new(pass: bool, note: impl Into<String>) -> Self {
        Self {
            pass,
            note: note.into(),
        }
    }
}

fn chat_req(model: &str, messages: Vec<Message>, tools: Vec<ToolDef>) -> ChatRequest {
    ChatRequest {
        model: model.to_string(),
        messages,
        tools,
        tool_choice: ToolChoice::Auto,
        sampling: Sampling {
            max_output_tokens: Some(600),
            ..Default::default()
        },
        conversation_id: Some(uuid_v4()),
        ..Default::default()
    }
}

fn summarize(c: &Completion) -> String {
    format!(
        "finish={} routed={} in={} (未命中 {}) out={} cache_r={} cache_w={} 思考 {} 字符 签名={} 工具 {} ttft={:?}ms",
        c.finish_reason.as_str(),
        c.routed_model.as_deref().unwrap_or("?"),
        c.usage.input_tokens,
        c.usage.uncached_input(),
        c.usage.output_tokens,
        c.usage.cache_read_tokens,
        c.usage.cache_write_tokens,
        c.thinking.chars().count(),
        c.thinking_signature.is_some(),
        c.tool_calls.len(),
        c.ttft_ms
    )
}

fn case_text<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let req = chat_req(
            model,
            vec![Message::text(Role::User, "Reply with exactly: pong")],
            vec![],
        );
        match live.chat(&req).await.1 {
            Ok(c) => Verdict::new(
                c.text.to_lowercase().contains("pong") && c.usage_measured,
                summarize(&c),
            ),
            Err(e) => Verdict::new(false, format!("{}: {}", e.kind.as_str(), e.message)),
        }
    })
}

fn case_system<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let req = chat_req(
            model,
            vec![
                Message::text(Role::System, "Secret codeword: ZEBRA-17. When asked for the codeword, reply with exactly the codeword."),
                Message::text(Role::User, "What is the codeword?"),
            ],
            vec![],
        );
        match live.chat(&req).await.1 {
            Ok(c) => Verdict::new(c.text.contains("ZEBRA-17"), summarize(&c)),
            Err(e) => Verdict::new(false, format!("{}: {}", e.kind.as_str(), e.message)),
        }
    })
}

fn case_tool_single<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let req = chat_req(
            model,
            vec![Message::text(
                Role::User,
                "What's the weather in Paris? Use the tool.",
            )],
            vec![weather_tool()],
        );
        match live.chat(&req).await.1 {
            Ok(c) => {
                let ok = c.tool_calls.len() == 1
                    && c.tool_calls[0].name == "get_weather"
                    && serde_json::from_str::<serde_json::Value>(&c.tool_calls[0].arguments)
                        .ok()
                        .and_then(|v| {
                            v.get("city")
                                .and_then(|c| c.as_str())
                                .map(|s| s.to_lowercase().contains("paris"))
                        })
                        .unwrap_or(false);
                Verdict::new(
                    ok,
                    format!(
                        "{} args={:?}",
                        summarize(&c),
                        c.tool_calls
                            .iter()
                            .map(|t| &t.arguments)
                            .collect::<Vec<_>>()
                    ),
                )
            }
            Err(e) => Verdict::new(false, format!("{}: {}", e.kind.as_str(), e.message)),
        }
    })
}

fn case_tool_parallel<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let req = chat_req(
            model,
            vec![Message::text(
                Role::User,
                "Get the weather for Paris and for Tokyo. Call the tool once per city, both calls in this same turn.",
            )],
            vec![weather_tool()],
        );
        match live.chat(&req).await.1 {
            Ok(c) => {
                let mut ids: Vec<&str> = c.tool_calls.iter().map(|t| t.id.as_str()).collect();
                ids.sort_unstable();
                ids.dedup();
                let ok = c.tool_calls.len() >= 2 && ids.len() == c.tool_calls.len();
                Verdict::new(
                    ok,
                    format!(
                        "{} ids={:?}",
                        summarize(&c),
                        c.tool_calls.iter().map(|t| &t.id).collect::<Vec<_>>()
                    ),
                )
            }
            Err(e) => Verdict::new(false, format!("{}: {}", e.kind.as_str(), e.message)),
        }
    })
}

/// 两发的工具循环：第二发带着第一发的思考（含签名）、工具调用和结果，答案必须引用结果里才有的值。
fn case_tool_loop<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let mut req = chat_req(
            model,
            vec![Message::text(Role::User, "What's the weather in Paris right now? Use the tool, then tell me the temperature and the station id.")],
            vec![weather_tool()],
        );
        let first = match live.chat(&req).await.1 {
            Ok(c) => c,
            Err(e) => {
                return Verdict::new(false, format!("第一发 {}: {}", e.kind.as_str(), e.message))
            }
        };
        let Some(call) = first.tool_calls.first().cloned() else {
            return Verdict::new(false, format!("第一发没调工具：{}", summarize(&first)));
        };
        let mut assistant = Message::text(Role::Assistant, first.text.clone());
        assistant.tool_calls = first.tool_calls.clone();
        if !first.thinking.is_empty() {
            assistant.reasoning.push(nexus_gateway::Reasoning {
                text: first.thinking.clone(),
                signature: first.thinking_signature.clone(),
                redacted: None,
            });
        }
        if let Some(data) = &first.redacted_thinking {
            assistant.reasoning.push(nexus_gateway::Reasoning {
                redacted: Some(data.clone()),
                ..Default::default()
            });
        }
        let mut tool = Message::text(Role::Tool, "");
        tool.tool_results.push(ToolResult {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            text: "Paris: 17°C, light rain (station LIVE-8841).".into(),
            ..Default::default()
        });
        req.messages.push(assistant);
        req.messages.push(tool);
        match live.chat(&req).await.1 {
            Ok(c) => Verdict::new(
                c.text.contains("17") && c.text.contains("8841"),
                format!("第一发 {}｜第二发 {}", summarize(&first), summarize(&c)),
            ),
            Err(e) => Verdict::new(
                false,
                format!(
                    "第二发 {}: {}（第一发 {}）",
                    e.kind.as_str(),
                    e.message,
                    summarize(&first)
                ),
            ),
        }
    })
}

fn case_tool_required<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let mut req = chat_req(
            model,
            vec![Message::text(Role::User, "Hello! How are you today?")],
            vec![weather_tool()],
        );
        req.tool_choice = ToolChoice::Tool("get_weather".into());
        match live.chat(&req).await.1 {
            Ok(c) => Verdict::new(
                c.tool_calls.iter().any(|t| t.name == "get_weather"),
                summarize(&c),
            ),
            Err(e) => Verdict::new(false, format!("{}: {}", e.kind.as_str(), e.message)),
        }
    })
}

fn case_tool_none<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let mut req = chat_req(
            model,
            vec![Message::text(
                Role::User,
                "What's the weather in Paris? If you cannot check, just say so in one sentence.",
            )],
            vec![weather_tool()],
        );
        req.tool_choice = ToolChoice::None;
        match live.chat(&req).await.1 {
            Ok(c) => Verdict::new(c.tool_calls.is_empty() && !c.text.is_empty(), summarize(&c)),
            Err(e) => Verdict::new(false, format!("{}: {}", e.kind.as_str(), e.message)),
        }
    })
}

fn case_image_user<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let mut user = Message::text(Role::User, COLOR_QUESTION);
        user.images.push(ImageInput {
            data: split_png(BLUE, YELLOW),
            mime_type: "image/png".into(),
        });
        let req = chat_req(model, vec![user], vec![]);
        match live.chat(&req).await.1 {
            Ok(c) => Verdict::new(
                says_blue_yellow(&c.text),
                format!("{} text={:?}", summarize(&c), c.text),
            ),
            Err(e) => Verdict::new(false, format!("{}: {}", e.kind.as_str(), e.message)),
        }
    })
}

fn case_image_tool<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let call = ToolCall {
            id: "toolu_live_shot_1".into(),
            name: "take_screenshot".into(),
            arguments: "{}".into(),
        };
        let mut assistant = Message::text(Role::Assistant, "");
        assistant.tool_calls.push(call.clone());
        let mut tool = Message::text(Role::Tool, "");
        tool.tool_results.push(ToolResult {
            tool_call_id: call.id.clone(),
            tool_name: call.name.clone(),
            text: "Screenshot captured.".into(),
            images: vec![ImageInput {
                data: split_png(BLUE, YELLOW),
                mime_type: "image/png".into(),
            }],
            ..Default::default()
        });
        let req = chat_req(
            model,
            vec![
                Message::text(
                    Role::User,
                    format!("Take a screenshot, then answer: {COLOR_QUESTION}"),
                ),
                assistant,
                tool,
            ],
            vec![shot_tool()],
        );
        match live.chat(&req).await.1 {
            Ok(c) => Verdict::new(
                says_blue_yellow(&c.text),
                format!("{} text={:?}", summarize(&c), c.text),
            ),
            Err(e) => Verdict::new(false, format!("{}: {}", e.kind.as_str(), e.message)),
        }
    })
}

fn case_stop<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let mut req = chat_req(
            model,
            vec![Message::text(
                Role::User,
                "Count from 1 to 10, comma separated, nothing else.",
            )],
            vec![],
        );
        req.sampling.stop_sequences = vec!["5".into()];
        match live.chat(&req).await.1 {
            Ok(c) => Verdict::new(
                !c.text.contains('5') && !c.text.contains("6") && c.text.contains('4'),
                format!("{} text={:?}", summarize(&c), c.text),
            ),
            Err(e) => Verdict::new(false, format!("{}: {}", e.kind.as_str(), e.message)),
        }
    })
}

fn case_max_tokens<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let mut req = chat_req(
            model,
            vec![Message::text(
                Role::User,
                "Write a 400-word essay about the sea.",
            )],
            vec![],
        );
        req.sampling.max_output_tokens = Some(60);
        match live.chat(&req).await.1 {
            Ok(c) => Verdict::new(
                c.finish_reason == nexus_gateway::FinishReason::Length,
                summarize(&c),
            ),
            Err(e) => Verdict::new(false, format!("{}: {}", e.kind.as_str(), e.message)),
        }
    })
}

fn case_cache<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let req = chat_req(
            model,
            vec![
                Message::text(Role::System, long_system()),
                Message::text(Role::User, "Mention rule 7 in one short sentence."),
            ],
            vec![],
        );
        let first = live.chat(&req).await.1;
        let second = live.chat(&req).await.1;
        match (first, second) {
            (Ok(a), Ok(b)) => Verdict::new(
                b.usage.cache_read_tokens > 0,
                format!("第一发 {}｜第二发 {}", summarize(&a), summarize(&b)),
            ),
            (Err(e), _) | (_, Err(e)) => {
                Verdict::new(false, format!("{}: {}", e.kind.as_str(), e.message))
            }
        }
    })
}

fn case_thinking<'a>(live: &'a Live, model: &'a str) -> CaseFuture<'a> {
    Box::pin(async move {
        let req = chat_req(
            model,
            vec![Message::text(Role::User, "A bat and a ball cost $1.10 in total. The bat costs $1.00 more than the ball. How much is the ball? Answer in one short sentence.")],
            vec![],
        );
        let (deltas, r) = live.chat(&req).await;
        match r {
            Ok(c) => {
                let sig_before_text = {
                    let s = deltas
                        .iter()
                        .position(|d| matches!(d, Delta::ThinkingSignature(_)));
                    let t = deltas.iter().position(|d| matches!(d, Delta::Text(_)));
                    match (s, t) {
                        (Some(s), Some(t)) => {
                            format!("签名在正文{}", if s < t { "之前" } else { "之后" })
                        }
                        (Some(_), None) => "只有签名没有正文".into(),
                        (None, _) => "无签名".into(),
                    }
                };
                Verdict::new(
                    c.text.contains("0.05")
                        || c.text.contains("5 cents")
                        || c.text.contains("five cents"),
                    format!("{} {sig_before_text}", summarize(&c)),
                )
            }
            Err(e) => Verdict::new(false, format!("{}: {}", e.kind.as_str(), e.message)),
        }
    })
}

const CASES: &[Case] = &[
    Case {
        name: "text",
        run: case_text,
    },
    Case {
        name: "system",
        run: case_system,
    },
    Case {
        name: "tool-single",
        run: case_tool_single,
    },
    Case {
        name: "tool-parallel",
        run: case_tool_parallel,
    },
    Case {
        name: "tool-loop",
        run: case_tool_loop,
    },
    Case {
        name: "tool-required",
        run: case_tool_required,
    },
    Case {
        name: "tool-none",
        run: case_tool_none,
    },
    Case {
        name: "image-user",
        run: case_image_user,
    },
    Case {
        name: "image-tool",
        run: case_image_tool,
    },
    Case {
        name: "stop",
        run: case_stop,
    },
    Case {
        name: "max-tokens",
        run: case_max_tokens,
    },
    Case {
        name: "cache",
        run: case_cache,
    },
    Case {
        name: "thinking",
        run: case_thinking,
    },
];

/// 用账号库里的长期 `crsr_` 换一张短期 `api_key_token`，只在内存里试一次低层推理口。
///
/// 这不是正式号源：探针不写 Access 槽、不写 CRSR 凭证文件，也不改 refresh token。默认最多
/// 试 4 个带 Key 的活跃账号；`LIVE_API_KEY_LIMIT` 可收窄，命令后的过滤串按邮箱匹配。
async fn api_key_probe(base: &Live, filter: Option<&str>) {
    use nexus_accounts::Status;
    use nexus_store::keys::AccountSecret;
    use nexus_store::{Db, SecretStore, SqliteSecrets};

    let home = std::env::var("HOME").expect("HOME");
    let db_path = std::path::PathBuf::from(home)
        .join("Library/Application Support/com.roviix.nexus/nexus.db");
    let db = std::sync::Arc::new(Db::open(db_path).expect("打不开 nexus.db"));
    let secrets: std::sync::Arc<dyn SecretStore> =
        std::sync::Arc::new(SqliteSecrets::new(db.clone()));
    let accounts = nexus_accounts::AccountsService::new(db, secrets);
    let limit = std::env::var("LIVE_API_KEY_LIMIT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(4);
    let selected: Vec<_> = accounts
        .repo
        .list()
        .expect("读账号列表")
        .into_iter()
        .filter(|a| {
            a.status == Status::Active
                && a.has_api_key
                && filter.is_none_or(|f| a.email.contains(f))
        })
        .take(limit)
        .collect();
    if selected.is_empty() {
        eprintln!("没有匹配的、带 crsr_ Key 的活跃账号");
        std::process::exit(3);
    }

    let model = std::env::var("LIVE_MODELS")
        .ok()
        .and_then(|s| s.split(',').next().map(str::trim).map(str::to_string))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".into());
    println!(
        "API Key 准入探针：{} 个账号 · model={model} · token 不落盘",
        selected.len()
    );

    for account in selected {
        let label = mask(&account.email);
        let key = match accounts
            .repo
            .require_secret(&account.id, AccountSecret::ApiKey)
        {
            Ok(key) => key,
            Err(e) => {
                println!("\n── {label} ──\n  读不到 crsr_：{}", e.message);
                continue;
            }
        };
        let ticket = match nexus_accounts::token::exchange_api_key(&base.client, key.expose()).await
        {
            Ok(ticket) => ticket,
            Err(e) => {
                println!("\n── {label} ──\n  兑换失败：{}", e.message);
                continue;
            }
        };
        let token_type = nexus_accounts::token::jwt_type(&ticket);
        let subject = nexus_accounts::token::extract_user_id(&ticket);
        let ttl = jwt_exp_secs(&ticket).map(|exp| exp - now_secs());
        let subject_ok = match (&account.workos_user_id, &subject) {
            (Some(expected), Some(actual)) => expected == actual,
            (None, Some(_)) => true,
            _ => false,
        };
        println!(
            "\n── {label} · plan={} ──\n  兑换票：type={} ttl={}s subject={}",
            account.membership.as_deref().unwrap_or("?"),
            token_type.as_deref().unwrap_or("?"),
            ttl.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
            if subject_ok { "ok" } else { "mismatch" }
        );
        if token_type.as_deref() != Some("api_key_token")
            || ttl.is_none_or(|n| n <= 120)
            || !subject_ok
        {
            println!("  票据 claims 不满足安全前提，不发推理请求");
            continue;
        }

        // 当前恰好登录的账号沿用真机码；号池账号沿用按稳定 subject 派生的机器码。
        let identity = if account.email.eq_ignore_ascii_case(&base.email) {
            DeviceIdentity::pinned(&ticket, base.identity.machine_id.clone())
        } else {
            DeviceIdentity::derived(&ticket)
        };
        let probe = Live {
            client: base.client.clone(),
            cfg: base.cfg.clone(),
            token: ticket,
            identity,
            email: account.email.clone(),
            exp: ttl.map(|n| now_secs() + n),
        };
        let mut req = base_request(
            &model,
            vec![Message::text(Role::User, "Reply with exactly: ok")],
            vec![],
        );
        req.model_config = Some(InferenceModelConfig {
            max_tokens: Some(8),
            ..Default::default()
        });
        match tokio::time::timeout(std::time::Duration::from_secs(60), probe.raw(&req)).await {
            Ok(stream) => {
                stream.print("api_key_token → Stream");
                println!(
                    "  Stream 准入：{}",
                    if stream.ok() && stream.info_errors.is_empty() && !stream.text.is_empty() {
                        "PASS"
                    } else {
                        "FAIL"
                    }
                );
            }
            Err(_) => println!("  Stream 准入：FAIL（总时长超过 60 秒）"),
        }
        println!("  接着试同一张票的 RunInference：");
        if tokio::time::timeout(
            std::time::Duration::from_secs(45),
            run_inference_once(&probe, &model, false),
        )
        .await
        .is_err()
        {
            println!("  RunInference：FAIL（总时长超过 45 秒）");
        }
    }
}

async fn matrix(live: &Live, filters: &[String]) {
    let models: Vec<String> = std::env::var("LIVE_MODELS")
        .unwrap_or_else(|_| "claude-sonnet-5,gpt-5.6-sol,gemini-3.7-flash,grok-4.6".into())
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let mut table: Vec<(String, &'static str, Verdict)> = Vec::new();
    for model in &models {
        for case in CASES {
            if !filters.is_empty() && !filters.iter().any(|f| case.name.contains(f.as_str())) {
                continue;
            }
            let started = Instant::now();
            let v = (case.run)(live, model).await;
            println!(
                "[{}] {model:<22} {:<14} {:>6}ms  {}",
                if v.pass { "PASS" } else { "FAIL" },
                case.name,
                started.elapsed().as_millis(),
                v.note
            );
            table.push((model.clone(), case.name, v));
        }
    }
    let failed: Vec<_> = table.iter().filter(|(_, _, v)| !v.pass).collect();
    println!(
        "\n合计 {} 条，PASS {}，FAIL {}",
        table.len(),
        table.len() - failed.len(),
        failed.len()
    );
    for (m, c, _) in &failed {
        println!("  FAIL {m} · {c}");
    }
    if !failed.is_empty() {
        std::process::exit(1);
    }
}

#[tokio::main]
async fn main() {
    if std::env::var("NEXUS_GATEWAY_PROBE").ok().as_deref() != Some("1") {
        eprintln!("这会用 Cursor 里正登着的号对 api2 发真请求、花它的额度。确认的话设 NEXUS_GATEWAY_PROBE=1 再跑。");
        std::process::exit(2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let live = Live::load();
    let exp = live
        .exp
        .map(|e| format!("{} 分钟", (e - now_secs()) / 60))
        .unwrap_or_else(|| "未知".into());
    let shown_email = if live.email.contains('@') {
        mask(&live.email)
    } else {
        live.email.clone()
    };
    eprintln!(
        "账号 {} · account {}… · 电脑 {}… · client-type {} · token 剩余 {exp}",
        shown_email,
        &live.identity.account[..12.min(live.identity.account.len())],
        &live.identity.machine_id[..12.min(live.identity.machine_id.len())],
        live.cfg.client_type
    );
    match args.first().map(String::as_str) {
        Some("status") | None => {}
        Some("catalog") => {
            live.require_fresh();
            catalog(&live, args.get(1).map(String::as_str)).await;
        }
        Some("raw") => {
            live.require_fresh();
            let name = args.get(1).map(String::as_str).unwrap_or("system-role");
            let models = std::env::var("LIVE_MODELS").unwrap_or_else(|_| "claude-sonnet-5".into());
            for model in models.split(',').map(str::trim).filter(|m| !m.is_empty()) {
                raw_experiment(&live, name, model).await;
            }
        }
        Some("matrix") => {
            live.require_fresh();
            matrix(&live, &args[1..]).await;
        }
        Some("pool") => pool(&live, args.get(1).map(String::as_str)).await,
        Some("api-key") => api_key_probe(&live, args.get(1).map(String::as_str)).await,
        Some(other) => {
            eprintln!(
                "不认识的命令 {other}：status | catalog [过滤] | raw <实验> | matrix [用例过滤…] | pool [过滤] | api-key [过滤]"
            );
            std::process::exit(2);
        }
    }
}
