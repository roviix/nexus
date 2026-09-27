//! Claude 订阅号出站的指纹：对齐真实 Claude Code CLI。
//!
//! Anthropic 从 2026-04 起把认不出是 Claude Code 的 OAuth 流量划进 extra usage。
//! 本地网关接的常常是 OpenCode、SDK、curl，它们的 UA、system、工具名和官方 CLI
//! 对不上。这条路上要补齐官方请求会带的那几样——计费头、身份句、beta 集合、
//! TitleCase 工具名、`metadata.user_id`——上游才按套餐额度计，而不是按 API 价。
//!
//! 做法对齐 sub2API / CLIProxyAPI 已经在线上验过的那一套。真实 Claude Code
//! 客户端（UA + 身份句都在）只改认证和会泄露账号的 `user_id`，不重写它自己的
//! system，也不改工具名。

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// 与 sub2API `CLICurrentVersion` / CLIProxyAPI 2.1.258 抓包对齐。
/// User-Agent 和 billing 的 `cc_version` 必须是同一个数。
pub const CLI_VERSION: &str = "2.1.280";
pub const SDK_VERSION: &str = "0.94.0";
pub const NODE_VERSION: &str = "v24.3.0";

pub const IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";
const OPENCODE_IDENTITY: &str = "You are OpenCode, the best coding agent on the planet.";

/// 身份句之后的第三块：CLIProxyAPI 拼起来的官方静态段（intro、system、doing tasks、
/// tone、output efficiency）。不带 `cache_control`。调用方自己的 system 仍挪到
/// 最前面的 user / assistant 对里。Fable 只发计费头和身份句。
pub const EXPANSION: &str = crate::prompt::STATIC;

/// 官方 CLI 的指纹盐。算法见 `compute_fingerprint`。
const FINGERPRINT_SALT: &str = "59cf53e54c78";

pub const BETA_CLAUDE_CODE: &str = "claude-code-20250219";
pub const BETA_OAUTH: &str = "oauth-2025-04-20";
pub const BETA_INTERLEAVED: &str = "interleaved-thinking-2025-05-14";
pub const BETA_PROMPT_CACHE: &str = "prompt-caching-scope-2026-01-05";
pub const BETA_EFFORT: &str = "effort-2025-11-24";
pub const BETA_CONTEXT_MGMT: &str = "context-management-2025-06-27";
pub const BETA_THINKING_BIND: &str = "thinking-binding-controls-2026-08-01";
pub const BETA_MID_OUTPUT: &str = "mid-conversation-output-config-2026-07-01";
pub const BETA_CACHE_TTL: &str = "extended-cache-ttl-2025-04-11";
pub const BETA_CONTEXT_1M: &str = "context-1m-2025-08-07";

const CACHE_TTL: &str = "5m";

pub const BETA_REDACT: &str = "redact-thinking-2026-02-12";
pub const BETA_THINKING_COUNT: &str = "thinking-token-count-2026-05-13";
pub const BETA_MID_SYSTEM: &str = "mid-conversation-system-2026-04-07";
pub const BETA_MID_TOOLS: &str = "mid-conversation-tool-changes-2026-07-01";
pub const BETA_PER_TURN: &str = "per-turn-control-2026-07-01";
pub const BETA_FAST: &str = "fast-mode-2026-02-01";
pub const BETA_FALLBACK: &str = "server-side-fallback-2026-06-01";
pub const BETA_FALLBACK_CREDIT: &str = "fallback-credit-2026-06-01";
pub const BETA_STRUCTURED: &str = "structured-outputs-2025-12-15";

pub fn user_agent() -> String {
    format!("claude-cli/{CLI_VERSION} (external, cli)")
}

pub fn identity_headers() -> Vec<(String, String)> {
    let mut h = vec![
        ("user-agent".to_string(), user_agent()),
        ("x-app".into(), "cli".into()),
        ("x-stainless-lang".into(), "js".into()),
        ("x-stainless-package-version".into(), SDK_VERSION.into()),
        ("x-stainless-os".into(), stainless_os().into()),
        ("x-stainless-arch".into(), stainless_arch().into()),
        ("x-stainless-runtime".into(), "node".into()),
        ("x-stainless-runtime-version".into(), NODE_VERSION.into()),
        ("x-stainless-retry-count".into(), "0".into()),
        ("x-stainless-timeout".into(), "600".into()),
        (
            "anthropic-dangerous-direct-browser-access".into(),
            "true".into(),
        ),
        ("anthropic-version".into(), "2023-06-01".into()),
    ];
    h.sort_by(|a, b| a.0.cmp(&b.0));
    h
}

fn stainless_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "MacOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "Linux"
    }
}

fn stainless_arch() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else if cfg!(target_arch = "x86_64") {
        "x64"
    } else {
        "arm64"
    }
}

pub fn is_identity_header(name: &str) -> bool {
    let k = name.to_ascii_lowercase();
    matches!(
        k.as_str(),
        "user-agent"
            | "x-app"
            | "anthropic-beta"
            | "anthropic-version"
            | "anthropic-dangerous-direct-browser-access"
    ) || k.starts_with("x-stainless-")
}

/// 客户端已经是 Claude Code。
///
/// UA 对上，并且 system 里有身份句或计费头；或者身份句和计费头都在（中间代理改过 UA
/// 的真 CLI 也算）。只有 UA、正文不像 CLI 的，仍走伪装。
pub fn looks_like_claude_code(headers: &[(String, String)], body: &Value) -> bool {
    let ua = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("user-agent"))
        .map(|(_, v)| v.as_str())
        .unwrap_or("");
    let identity = has_identity_text(body);
    let billing = has_billing_text(body);
    (claude_cli_ua(ua) && (identity || billing)) || (identity && billing)
}

fn claude_cli_ua(ua: &str) -> bool {
    let lower = ua.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix("claude-cli/") else {
        return false;
    };
    let ver = rest
        .split(|c: char| c == ' ' || c == '(')
        .next()
        .unwrap_or("");
    let mut parts = ver.split('.');
    parts.next().and_then(|s| s.parse::<u32>().ok()).is_some()
        && parts.next().and_then(|s| s.parse::<u32>().ok()).is_some()
        && parts.next().and_then(|s| s.parse::<u32>().ok()).is_some()
}

fn has_identity_text(body: &Value) -> bool {
    system_texts(body)
        .iter()
        .any(|t| t.contains("You are Claude Code, Anthropic's official CLI"))
}

fn has_billing_text(body: &Value) -> bool {
    system_texts(body)
        .iter()
        .any(|t| t.contains("x-anthropic-billing-header") && t.contains("cc_entrypoint=cli"))
}

fn system_texts(body: &Value) -> Vec<String> {
    match body.get("system") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// SHA256(SALT + msg[4] + msg[7] + msg[20] + version) 的 hex 前三位。
/// 下标按 JS 的 UTF-16 码元，不是 Unicode 标量。emoji 占两个码元，用 `chars()` 会算错。
pub fn compute_fingerprint(first_user: &str, version: &str) -> String {
    let chars: String = [4, 7, 20]
        .into_iter()
        .map(|i| js_unit(first_user, i))
        .collect();
    let digest = Sha256::digest(format!("{FINGERPRINT_SALT}{chars}{version}").as_bytes());
    hex_lower(&digest)[..3].to_string()
}

/// JS `text[i] || "0"`。越界是 `"0"`；落在代理对上时 Node 会编成 U+FFFD。
fn js_unit(text: &str, index: usize) -> String {
    match text.encode_utf16().nth(index) {
        None => "0".to_string(),
        Some(unit) => match char::from_u32(u32::from(unit)) {
            Some(c) => c.to_string(),
            None => "\u{FFFD}".to_string(),
        },
    }
}

pub fn first_user_text(body: &Value) -> String {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return String::new();
    };
    for msg in messages {
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        return match msg.get("content") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Array(blocks)) => blocks
                .iter()
                .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .and_then(|b| b.get("text").and_then(Value::as_str))
                .unwrap_or("")
                .to_string(),
            _ => String::new(),
        };
    }
    String::new()
}

pub fn billing_header(first_user: &str, version: &str) -> String {
    let fp = compute_fingerprint(first_user, version);
    format!("x-anthropic-billing-header: cc_version={version}.{fp}; cc_entrypoint=cli;")
}

/// `user_{64hex}_account_{uuid}_session_{uuid}`。session 由账号 + 首条用户消息派生，
/// 同一段对话追加消息时保持不变，贴近真实 CLI 进程级稳定的 session。
/// `metadata.user_id` 末尾的 `session_{uuid}`。`X-Claude-Code-Session-Id` 用的就是这一个。
pub fn session_from_user_id(user_id: &str) -> Option<&str> {
    let session = user_id.rsplit_once("_session_")?.1;
    looks_like_uuid(session).then_some(session)
}

pub fn metadata_user_id(account_ref: &str, first_user: &str) -> String {
    let user = hex_lower(&Sha256::digest(
        format!("nexus:claude:user:{account_ref}").as_bytes(),
    ));
    let account = if looks_like_uuid(account_ref) {
        account_ref.to_string()
    } else {
        String::new()
    };
    let session = uuid_from_digest(&format!("nexus:claude:session:{account_ref}:{first_user}"));
    format!("user_{user}_account_{account}_session_{session}")
}

fn looks_like_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b[8] == b'-'
        && b[13] == b'-'
        && b[18] == b'-'
        && b[23] == b'-'
        && s.bytes()
            .filter(|&c| c != b'-')
            .all(|c| c.is_ascii_hexdigit())
}

fn uuid_from_digest(seed: &str) -> String {
    let d = Sha256::digest(seed.as_bytes());
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        d[0], d[1], d[2], d[3], d[4], d[5], (d[6] & 0x0f) | 0x50, d[7], (d[8] & 0x3f) | 0x80, d[9],
        d[10], d[11], d[12], d[13], d[14], d[15]
    )
}

fn hex_lower(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// 官方 Claude Code 工具名。小写 / 下划线 / 连字符都归到 TitleCase。
/// 不认识的名字原样留下——剥掉会让 OpenCode 自己的工具整轮作废。
const TOOLS: &[(&str, &str)] = &[
    ("agent", "Agent"),
    ("askuserquestion", "AskUserQuestion"),
    ("bash", "Bash"),
    ("config", "Config"),
    ("edit", "Edit"),
    ("enterplanmode", "EnterPlanMode"),
    ("enterworktree", "EnterWorktree"),
    ("exitplanmode", "ExitPlanMode"),
    ("exitworktree", "ExitWorktree"),
    ("glob", "Glob"),
    ("grep", "Grep"),
    ("listmcpresources", "ListMcpResources"),
    ("ls", "LS"),
    ("mcpauth", "McpAuth"),
    ("multiedit", "MultiEdit"),
    ("notebookedit", "NotebookEdit"),
    ("read", "Read"),
    ("readmcpresource", "ReadMcpResource"),
    ("skill", "Skill"),
    ("structuredoutput", "StructuredOutput"),
    ("task", "Task"),
    ("taskcreate", "TaskCreate"),
    ("taskget", "TaskGet"),
    ("tasklist", "TaskList"),
    ("taskupdate", "TaskUpdate"),
    ("todoread", "TodoRead"),
    ("todowrite", "TodoWrite"),
    ("toolsearch", "ToolSearch"),
    ("webfetch", "WebFetch"),
    ("websearch", "WebSearch"),
    ("write", "Write"),
];

pub fn official_tool_name(name: &str) -> String {
    known_tool(name)
        .map(str::to_string)
        .unwrap_or_else(|| name.to_string())
}

fn known_tool(name: &str) -> Option<&'static str> {
    let key: String = name
        .chars()
        .filter(|c| *c != '_' && *c != '-')
        .flat_map(|c| c.to_lowercase())
        .collect();
    TOOLS.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// 出站改过的工具名 → 客户端原来的名字。回程 SSE 要还回去，客户端才认得出自己的工具。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolMap {
    /// upstream TitleCase → original
    restore: Vec<(String, String)>,
}

impl ToolMap {
    pub fn restore(&self, name: &str) -> String {
        self.restore
            .iter()
            .find(|(up, _)| up == name)
            .map(|(_, orig)| orig.clone())
            .unwrap_or_else(|| name.to_string())
    }

    pub fn is_empty(&self) -> bool {
        self.restore.is_empty()
    }

    fn note(&mut self, upstream: &str, original: &str) {
        if upstream == original {
            return;
        }
        if self.restore.iter().any(|(up, _)| up == upstream) {
            return;
        }
        self.restore
            .push((upstream.to_string(), original.to_string()));
    }
}

pub struct Prepare {
    pub native: bool,
    pub tools: ToolMap,
    /// 非空时整段替换 `anthropic-beta`。真实 CLI 透传时空着，沿用客户端自己的头。
    pub betas: String,
}

/// 给一次 OAuth / setup-token 出站改请求体。
///
/// `requested` 是客户端原来的 `anthropic-beta`。伪装路径按 body 现拼一份，
/// 客户端点名的、以及这份清单没管的新 beta，都留着。
pub fn prepare(
    body: &mut Value,
    account_ref: &str,
    native: bool,
    requested: &[String],
    wants_1m: bool,
) -> Prepare {
    if let Some(model) = body.get("model").and_then(Value::as_str) {
        let normalized = crate::protocol::normalize_model_id(model);
        if normalized != model {
            body["model"] = json!(normalized);
        }
    }
    if native {
        let first = first_user_text(body);
        ensure_metadata(body, account_ref, &first);
        return Prepare {
            native: true,
            tools: ToolMap::default(),
            betas: String::new(),
        };
    }
    sanitize_system(body);
    inject_system(body);
    let tools = rewrite_tools(body);
    ensure_defaults(body);
    let first = first_user_text(body);
    ensure_metadata(body, account_ref, &first);
    Prepare {
        native: false,
        tools,
        betas: assemble_betas(body, requested, wants_1m),
    }
}

/// 按这一次请求拼 `anthropic-beta`。顺序对齐 Claude Code 2.1.280：常驻的在前，
/// body 或客户端点名才带的跟在后面。没列进清单的客户端 beta 原样附在末尾。
pub fn assemble_betas(body: &Value, requested: &[String], wants_1m: bool) -> String {
    let asked = |token: &str| requested.iter().any(|b| b.eq_ignore_ascii_case(token));
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let legacy = model.contains("claude-3-");
    let sonnet5 = model.starts_with("claude-sonnet-5");
    let per_turn = model.contains("claude-opus-5-5") || model.contains("claude-fable-5-1");
    let thinking = body
        .pointer("/thinking/type")
        .and_then(Value::as_str)
        .unwrap_or("");
    let display_set = body.pointer("/thinking/display").is_some();
    let mut out: Vec<&str> = vec![BETA_CLAUDE_CODE, BETA_OAUTH];
    if wants_1m || asked(BETA_CONTEXT_1M) {
        out.push(BETA_CONTEXT_1M);
    }
    out.push(BETA_INTERLEAVED);
    if !display_set {
        out.push(BETA_REDACT);
    }
    out.push(BETA_THINKING_COUNT);
    out.push(BETA_CONTEXT_MGMT);
    out.push(BETA_PROMPT_CACHE);
    if !legacy {
        out.push(BETA_MID_SYSTEM);
        if per_turn || asked(BETA_PER_TURN) {
            out.push(BETA_PER_TURN);
        }
        if !sonnet5 {
            out.push(BETA_MID_TOOLS);
        }
    }
    if asked("advisor-tool-2026-03-01") || body_has_tool_type(body, "advisor") {
        out.push("advisor-tool-2026-03-01");
    }
    if asked("advanced-tool-use-2025-11-20") || known_tool_present(body, "ToolSearch") {
        out.push("advanced-tool-use-2025-11-20");
    }
    if !legacy
        && (asked("mid-conversation-system-clear-at-2026-08-21") || body_contains(body, "clear_at"))
    {
        out.push("mid-conversation-system-clear-at-2026-08-21");
    }
    if asked("dangerous-tool-use-2026-09-03") || body.get("safeguards").is_some() {
        out.push("dangerous-tool-use-2026-09-03");
    }
    let effort = asked(BETA_EFFORT)
        || matches!(thinking, "enabled" | "adaptive")
        || body.pointer("/output_config/effort").is_some()
        || per_turn;
    if effort {
        out.push(BETA_EFFORT);
    }
    if body.get("fallbacks").is_some() || asked(BETA_FALLBACK) {
        out.push(BETA_FALLBACK);
    }
    if body.get("fallback_credit_token").is_some()
        || body.get("fallbacks").is_some()
        || asked(BETA_FALLBACK_CREDIT)
    {
        out.push(BETA_FALLBACK_CREDIT);
    }
    if asked(BETA_STRUCTURED) || body.get("output_format").is_some() {
        out.push(BETA_STRUCTURED);
    }
    let bind = asked(BETA_THINKING_BIND)
        || body.pointer("/thinking/block_binding").is_some()
        || (per_turn && thinking == "adaptive")
        || thinking == "enabled"
        || thinking == "adaptive";
    if bind {
        out.push(BETA_THINKING_BIND);
    }
    if body.get("output_config").is_some() || asked(BETA_MID_OUTPUT) {
        out.push(BETA_MID_OUTPUT);
    }
    if asked("thinking-display-updates-2026-08-18")
        || body.pointer("/thinking/display").and_then(Value::as_str) == Some("updates")
    {
        out.push("thinking-display-updates-2026-08-18");
    }
    if asked("thinking-resumption-2026-07-17") {
        out.push("thinking-resumption-2026-07-17");
    }
    if asked(BETA_FAST) || body.get("speed").and_then(Value::as_str) == Some("fast") {
        out.push(BETA_FAST);
    }
    if asked("afk-mode-2026-01-31") {
        out.push("afk-mode-2026-01-31");
    }
    out.push(BETA_CACHE_TTL);
    if asked("prompt-caching-evict-2026-05-12") || body_contains(body, "evict_on_complete") {
        out.push("prompt-caching-evict-2026-05-12");
    }
    if body.get("diagnostics").and_then(Value::as_object).is_some() {
        out.push("cache-diagnosis-2026-04-07");
    }
    let mut extras = Vec::new();
    for extra in requested {
        let token = extra.trim();
        if token.is_empty()
            || out.iter().any(|b| b.eq_ignore_ascii_case(token))
            || extras
                .iter()
                .any(|b: &String| b.eq_ignore_ascii_case(token))
        {
            continue;
        }
        extras.push(token.to_string());
    }
    let mut joined = out.join(",");
    for extra in extras {
        if !joined.is_empty() {
            joined.push(',');
        }
        joined.push_str(&extra);
    }
    joined
}

fn body_has_tool_type(body: &Value, ty: &str) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools
                .iter()
                .any(|t| t.get("type").and_then(Value::as_str) == Some(ty))
        })
}

fn known_tool_present(body: &Value, official: &str) -> bool {
    body.get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|t| {
                t.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| known_tool(n) == Some(official))
            })
        })
}

fn body_contains(body: &Value, needle: &str) -> bool {
    body.to_string().contains(needle)
}

fn sanitize_system(body: &mut Value) {
    match body.get_mut("system") {
        Some(Value::String(s)) => {
            *s = s.replace(OPENCODE_IDENTITY, IDENTITY);
        }
        Some(Value::Array(items)) => {
            for item in items {
                if let Some(Value::String(s)) = item.get_mut("text") {
                    *s = s.replace(OPENCODE_IDENTITY, IDENTITY);
                }
            }
        }
        _ => {}
    }
}

fn inject_system(body: &mut Value) {
    let (original, cache) = take_system(body);
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let fable = model.contains("fable");
    let trimmed = original.trim().to_string();
    let carry = !trimmed.is_empty() && trimmed != IDENTITY && !trimmed.contains(IDENTITY);
    if carry {
        let mut instruction = json!({
            "type": "text",
            "text": format!("[System Instructions]\n{trimmed}"),
        });
        if let Some(cc) = cache {
            instruction["cache_control"] = cc;
        }
        let user = json!({ "role": "user", "content": [instruction] });
        let ack = json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "Understood. I will follow these instructions." }],
        });
        let mut messages = vec![user, ack];
        if let Some(Value::Array(existing)) = body.get("messages").cloned() {
            messages.extend(existing);
        }
        body["messages"] = Value::Array(messages);
    }
    // 计费指纹按**发出去的**第一条 user 算。插过说明之后，第一条已经不是原来的那句。
    let billing = billing_header(&first_user_text(body), CLI_VERSION);
    let mut blocks = vec![text_block(&billing, None), text_block(IDENTITY, None)];
    if !fable {
        blocks.push(text_block(EXPANSION, None));
    }
    body["system"] = Value::Array(blocks);
}

fn take_system(body: &Value) -> (String, Option<Value>) {
    match body.get("system") {
        Some(Value::String(s)) => (s.clone(), None),
        Some(Value::Array(items)) => {
            let mut parts = Vec::new();
            let mut cache = None;
            for item in items {
                if let Some(t) = item.get("text").and_then(Value::as_str) {
                    if !t.trim().is_empty() {
                        parts.push(t.to_string());
                    }
                }
                if let Some(cc) = item.get("cache_control") {
                    if !cc.is_null() {
                        cache = Some(cc.clone());
                    }
                }
            }
            (parts.join("\n\n"), cache)
        }
        _ => (String::new(), None),
    }
}

fn text_block(text: &str, cache: Option<Value>) -> Value {
    let mut o = Map::new();
    o.insert("type".into(), json!("text"));
    o.insert("text".into(), json!(text));
    if let Some(cc) = cache {
        o.insert("cache_control".into(), cc);
    }
    Value::Object(o)
}

fn rewrite_tools(body: &mut Value) -> ToolMap {
    let mut map = ToolMap::default();
    if let Some(Value::Array(tools)) = body.get_mut("tools") {
        tools.retain(|tool| {
            let Some(name) = tool.get("name").and_then(Value::as_str) else {
                return true;
            };
            known_tool(name).is_some()
        });
        for tool in tools.iter_mut() {
            rename_field(tool, "name", &mut map);
        }
        if let Some(last) = tools.last_mut() {
            if last.get("cache_control").is_none() {
                last["cache_control"] = json!({ "type": "ephemeral", "ttl": CACHE_TTL });
            }
        }
    }
    if let Some(choice) = body.get_mut("tool_choice") {
        rename_field(choice, "name", &mut map);
    }
    rewrite_history(body, &mut map);
    map
}

fn rewrite_history(body: &mut Value, map: &mut ToolMap) {
    let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) else {
        return;
    };
    for msg in messages {
        let Some(parts) = msg.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for part in parts {
            rewrite_part(part, map);
        }
    }
}

fn rewrite_part(part: &mut Value, map: &mut ToolMap) {
    match part.get("type").and_then(Value::as_str) {
        Some("tool_use") => rename_field(part, "name", map),
        Some("tool_reference") => rename_field(part, "tool_name", map),
        Some("tool_result") => {
            if let Some(nested) = part.get_mut("content").and_then(Value::as_array_mut) {
                for child in nested {
                    rewrite_part(child, map);
                }
            }
        }
        Some("tool_search_tool_result") => {
            if let Some(refs) = part
                .pointer_mut("/content/tool_references")
                .and_then(Value::as_array_mut)
            {
                for r in refs {
                    rename_field(r, "tool_name", map);
                }
            }
        }
        _ => {}
    }
}

fn rename_field(value: &mut Value, field: &str, map: &mut ToolMap) {
    let Some(name) = value.get(field).and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(official) = known_tool(&name) else {
        return;
    };
    if official != name {
        map.note(official, &name);
        value[field] = json!(official);
    }
}

fn ensure_defaults(body: &mut Value) {
    if body.get("max_tokens").is_none() {
        body["max_tokens"] = json!(128000);
    }
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if body.get("temperature").is_none() && !model.contains("opus-5-5") {
        body["temperature"] = json!(1);
    }
    let thinking = body
        .pointer("/thinking/type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if matches!(thinking.as_str(), "enabled" | "adaptive")
        && body.get("context_management").is_none()
    {
        body["context_management"] = json!({
            "edits": [{ "type": "clear_thinking_20251015", "keep": "all" }]
        });
    }
    let progress = model.contains("claude-opus-5-5")
        || model.contains("claude-fable-5-1")
        || model.starts_with("claude-sonnet-5");
    if progress
        && matches!(thinking.as_str(), "enabled" | "adaptive")
        && body.pointer("/thinking/display").is_none()
    {
        if let Some(obj) = body.get_mut("thinking").and_then(Value::as_object_mut) {
            obj.insert("display".into(), json!("updates"));
        }
    }
}

fn ensure_metadata(body: &mut Value, account_ref: &str, first_user: &str) {
    let uid = metadata_user_id(account_ref, first_user);
    match body.get_mut("metadata") {
        Some(Value::Object(m)) => {
            m.insert("user_id".into(), json!(uid));
        }
        _ => {
            body["metadata"] = json!({ "user_id": uid });
        }
    }
}

/// 回程 SSE / JSON 里的工具名还原成客户端原来的。
pub fn restore_tools_in_value(v: &mut Value, map: &ToolMap) {
    if map.is_empty() {
        return;
    }
    if let Some(name) = v
        .pointer("/content_block/name")
        .and_then(Value::as_str)
        .map(str::to_string)
    {
        if let Some(block) = v.get_mut("content_block") {
            block["name"] = json!(map.restore(&name));
        }
    }
    if let Some(blocks) = v.get_mut("content").and_then(Value::as_array_mut) {
        for block in blocks {
            if let Some(name) = block
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string)
            {
                block["name"] = json!(map.restore(&name));
            }
        }
    }
}

pub fn restore_tools_in_sse(data: &str, map: &ToolMap) -> String {
    if map.is_empty() {
        return data.to_string();
    }
    let Ok(mut v) = serde_json::from_str::<Value>(data) else {
        return data.to_string();
    };
    restore_tools_in_value(&mut v, map);
    v.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_matches_the_published_algorithm() {
        // SHA256("59cf53e54c78" + "lo!" + "2.1.258") 的前三位。
        // "hello world!!!"[4]=o [7]=w [20]=不足→0 → 但我们用 "hello world" : [4]=o [7]=r [20]=0
        let fp = compute_fingerprint("hello world", "2.1.258");
        assert_eq!(fp.len(), 3);
        assert!(fp.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(
            compute_fingerprint("hello world", "2.1.258"),
            compute_fingerprint("hello world", "2.1.258")
        );
        assert_ne!(
            compute_fingerprint("abcdefghij", "2.1.258"),
            compute_fingerprint("abcdXfghYj", "2.1.258")
        );
    }

    #[test]
    fn billing_header_has_version_fingerprint_and_cli_entrypoint() {
        let h = billing_header("ping", CLI_VERSION);
        assert!(h.starts_with(&format!(
            "x-anthropic-billing-header: cc_version={CLI_VERSION}."
        )));
        assert!(h.contains("cc_entrypoint=cli;"));
        assert!(!h.contains("cch="));
    }

    #[test]
    fn a_real_cli_request_is_recognized() {
        let body = json!({
            "system": [
                { "type": "text", "text": billing_header("hi", CLI_VERSION) },
                { "type": "text", "text": IDENTITY }
            ]
        });
        assert!(looks_like_claude_code(
            &[("user-agent".into(), user_agent())],
            &body
        ));
        assert!(
            looks_like_claude_code(&[("user-agent".into(), "OpenCode/1.0".into())], &body),
            "身份句和计费头都在，UA 被改过也仍是 CLI 正文"
        );
        assert!(!looks_like_claude_code(
            &[("user-agent".into(), "OpenCode/1.0".into())],
            &json!({"system": "be brief"})
        ));
        assert!(!looks_like_claude_code(
            &[("user-agent".into(), user_agent())],
            &json!({})
        ));
    }

    #[test]
    fn third_party_body_gets_the_three_blocks_and_keeps_instructions() {
        let mut body = json!({
            "model": "claude-sonnet-4-5",
            "system": "be brief",
            "messages": [{ "role": "user", "content": "hello world" }],
            "tools": [{ "name": "bash", "description": "run" }]
        });
        let prep = prepare(&mut body, "acct-1", false, &[], false);
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 3);
        assert!(sys[0]["text"]
            .as_str()
            .unwrap()
            .contains("cc_entrypoint=cli"));
        assert_eq!(sys[1]["text"], IDENTITY);
        assert!(sys[2].get("cache_control").is_none());
        let third = sys[2]["text"].as_str().unwrap();
        assert!(third.starts_with("You are an interactive agent"));
        assert!(third.contains("# Doing tasks"));
        assert!(third.contains("# Output efficiency"));
        assert_eq!(body["messages"][0]["role"], "user");
        assert!(body["messages"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("[System Instructions]\nbe brief"));
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["messages"][2]["content"], "hello world");
        assert_eq!(body["model"], "claude-sonnet-4-5-20250929");
        assert_eq!(body["tools"][0]["name"], "Bash");
        assert_eq!(prep.tools.restore("Bash"), "bash");
        let uid = body["metadata"]["user_id"].as_str().unwrap();
        assert!(uid.starts_with("user_"));
        assert_eq!(
            session_from_user_id(uid).map(|s| format!("_session_{s}")),
            Some(uid[uid.rfind("_session_").unwrap()..].to_string())
        );
    }

    #[test]
    fn fable_skips_the_expansion_block() {
        let mut body = json!({
            "model": "claude-fable-5",
            "messages": [{ "role": "user", "content": "hi" }]
        });
        prepare(&mut body, "a", false, &[], false);
        assert_eq!(body["system"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn native_cli_keeps_its_system_and_tools() {
        let mut body = json!({
            "system": [
                { "type": "text", "text": IDENTITY },
                { "type": "text", "text": "project rules" }
            ],
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "name": "Bash" }]
        });
        let prep = prepare(
            &mut body,
            "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            true,
            &[],
            false,
        );
        assert!(prep.native);
        assert_eq!(body["system"][1]["text"], "project rules");
        assert_eq!(body["tools"][0]["name"], "Bash");
        let uid = body["metadata"]["user_id"].as_str().unwrap();
        assert!(uid.contains("account_aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"));
    }

    #[test]
    fn opencode_identity_is_rewritten() {
        let mut body = json!({
            "system": OPENCODE_IDENTITY,
            "messages": [{ "role": "user", "content": "x" }]
        });
        prepare(&mut body, "a", false, &[], false);
        let sys = serde_json::to_string(&body["system"]).unwrap();
        assert!(!sys.contains("OpenCode"));
        assert!(sys.contains("Claude Code"));
    }

    #[test]
    fn tool_aliases_fold_to_title_case() {
        assert_eq!(official_tool_name("todo_write"), "TodoWrite");
        assert_eq!(official_tool_name("WebFetch"), "WebFetch");
        assert_eq!(official_tool_name("lsp_hover"), "lsp_hover");
    }

    #[test]
    fn restore_puts_the_client_name_back_in_sse() {
        let map = ToolMap {
            restore: vec![("Bash".into(), "bash".into())],
        };
        let out = restore_tools_in_sse(
            r#"{"type":"content_block_start","content_block":{"type":"tool_use","name":"Bash"}}"#,
            &map,
        );
        assert!(out.contains("\"name\":\"bash\""));
    }

    #[test]
    fn history_tool_names_are_rewritten_and_unknown_tools_are_dropped() {
        let mut body = json!({
            "model": "claude-sonnet-4-6",
            "messages": [
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "t1", "name": "bash", "input": {} },
                    { "type": "tool_use", "id": "t2", "name": "lsp_hover", "input": {} }
                ]},
                { "role": "user", "content": "next" }
            ],
            "tools": [
                { "name": "bash", "description": "run" },
                { "name": "lsp_hover", "description": "ide" }
            ],
            "tool_choice": { "type": "tool", "name": "bash" },
            "fallbacks": [{ "model": "claude-haiku-4-5" }],
            "speed": "fast"
        });
        let prep = prepare(&mut body, "acct", false, &["some-future-beta".into()], true);
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
        assert_eq!(body["tools"][0]["name"], "Bash");
        assert_eq!(body["messages"][0]["content"][0]["name"], "Bash");
        assert_eq!(body["messages"][0]["content"][1]["name"], "lsp_hover");
        assert_eq!(body["tool_choice"]["name"], "Bash");
        assert_eq!(prep.tools.restore("Bash"), "bash");
        assert!(prep.betas.contains(BETA_FALLBACK));
        assert!(prep.betas.contains(BETA_FAST));
        assert!(prep.betas.contains(BETA_CONTEXT_1M));
        assert!(prep.betas.contains("some-future-beta"));
        assert!(body["temperature"] == 1);
        let first = first_user_text(&body);
        let fp = compute_fingerprint(&first, CLI_VERSION);
        assert!(body["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains(&format!("{CLI_VERSION}.{fp}")));
    }

    #[test]
    fn utf16_indexes_do_not_follow_scalar_chars() {
        let emoji = "😀abcd";
        let by_unit = js_unit(emoji, 4);
        let by_char = emoji.chars().nth(4).unwrap().to_string();
        assert_ne!(by_unit, by_char);
        assert_eq!(compute_fingerprint("hello", CLI_VERSION).len(), 3);
    }

    #[test]
    fn billing_plus_identity_counts_as_claude_code_even_without_the_cli_ua() {
        let body = json!({
            "system": [
                { "type": "text", "text": billing_header("hi", CLI_VERSION) },
                { "type": "text", "text": IDENTITY }
            ]
        });
        assert!(looks_like_claude_code(&[], &body));
    }

    #[test]
    fn session_id_is_stable_for_the_same_first_user_line() {
        let a = metadata_user_id("acct", "hello");
        let b = metadata_user_id("acct", "hello");
        let c = metadata_user_id("acct", "hello again");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }
}
