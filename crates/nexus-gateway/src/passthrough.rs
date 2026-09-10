//! 模式⑤：透传。让 `cursor-agent -e http://127.0.0.1:PORT --agent-endpoint http://127.0.0.1:PORT`
//! 把**全部流量**（含 agentic 主循环 `agent.v1.AgentService/Run`）导进本机，网关只换身份头、
//! 原样转发 protobuf 帧，不解 body、不翻译方言——这是它和 `server.rs`（OpenAI/Anthropic
//! 方言翻译）的根本区别，也是探针把它列为独立模式的原因。
//!
//! ## 为什么要单开一个入站循环，不塞进 `server.rs`
//!
//! `server.rs` 的 axum 只点了 `http1` 特性：够用（OpenAI/Anthropic SDK 都走 HTTP/1.1），
//! 但 `agent.v1.AgentService/Run` 是双向流（BiDi）——探针实测：HTTP/1.1 转发一到这个方法就是
//! `Protocol error`，必须让客户端看到真正的 HTTP/2（h2c 即可，客户端连的是
//! `http://127.0.0.1`，不需要 TLS）。给 axum 加 http2 特性不能解决这个问题：BiDi 要求服务端在
//! 客户端还没写完请求体时就能读取到部分请求帧、且能在此期间开始写响应帧，这是连接级别的
//! 双向语义，和"翻译方言"那条路径（收满一个 JSON body 再决定发不发流）完全是两种编程模型，
//! 硬揽在一起只会把两边都写复杂。于是：翻译模式继续用 `server.rs` 的 axum（未变一行），
//! 透传模式在这里另起一个专用的接受循环，两者**分端口**（`GatewaySettings.port` /
//! `passthrough_port`），各自独立起停，坏一个不连累另一个（ARCHITECTURE §3.4 的一贯精神）。
//! `cursor-agent` 的 `-e` 和 `--agent-endpoint` 各自接受一个 URL，天然就填得下两个端口，
//! 权衡下来不值得为了省一个端口号去合并两套完全不同的 Service。
//!
//! 这个"专用接受循环"内部**必须**同时讲 HTTP/1.1 和 h2——一开始想当然地以为透传端口只要
//! 讲 h2 就够了（`/agent.v1.*` / `/aiserver.v1.*` 确实都是 Connect-over-h2），但拿真机
//! `cursor-agent` 二进制（bundle 是 Node.js，能直接 grep 源码）联调后发现：
//! `/auth/exchange_user_api_key`、`/auth/poll`（api-key 换 session、登录轮询）走的是
//! `undici` 的 `fetch`，也就是纯 HTTP/1.1，和 `-e`/`--agent-endpoint` 报的是**同一个**
//! base URL。纯 h2 的监听对着一个 HTTP/1.1 请求行只会在协议层直接报废这条连接
//! （客户端看到的是连接被重置，`cursor-agent` 把这一路错误包成
//! `Failed to reach the Cursor API`）。所以入站用
//! `hyper_util::server::conn::auto::Builder`（每条连接先嗅探 h2 前奏，退回 HTTP/1.1），
//! 出站的 TLS ALPN 也不强制只报 `h2`（同时报 `h1`/`h2`，服务端认哪个用哪个，和
//! `inference.rs` 现有的 reqwest 客户端行为一致）——这段教训写在这里，别再"以为够用"。
//!
//! ## 路由与身份
//!
//! 路径前缀决定转发到哪个上游主机（[`classify_path`]）；上游连接是**真 TLS**（rustls），
//! ALPN 里 h1/h2 都报，服务端认哪个用哪个（见下面"真机验证"一节的教训）。
//! 身份只换六个头（[`identity_overrides`]）：`authorization` / `x-cursor-checksum` /
//! `x-client-key` / `x-session-id` / `x-cursor-config-version` / `x-cursor-client-type`，
//! 其余客户端发来的头原样转发（去掉 hop-by-hop）——`cursor-agent` 自己拼的
//! `x-cursor-client-version` / os / arch 等头本来就是一致的一套，我们没有理由也没有必要碰它们。
//!
//! ## 会话级接力
//!
//! `agent.v1.AgentService/Run` 这类 BiDi 要求**同一条连接全程用同一个号**——中途换账号等于
//! 把一个正在进行的会话腰斩。做法是把 [`Lane::acquire`] 的结果钉在**入站 TCP 连接**上
//! （[`ConnState`]，`tokio::sync::OnceCell`，同一连接上无论 unary 还是 BiDi 都复用它），
//! 只有这条连接断开、进程整体退出重连时才可能换号——这比"只对 Run 方法特殊处理"更简单，
//! 也更符合真实情况：同一条连接（不管是 h1 还是 h2）上不会有别的账号语境需要区分。
//!
//! ## 真机验证（2026-09-02）
//!
//! 用 `examples/serve_passthrough.rs` 起了真的透传服务（Cursor 里正登着的号，真机码），
//! 分别对两条真上游发了真请求，两条都收到了上游的真实业务响应（不是连接失败、不是我们
//! 自己合成的）：
//! - `POST /auth/exchange_user_api_key`（HTTP/1.1）→ `api2.cursor.sh` 回
//!   `{"code":"error","message":"Invalid User API Key"}`——证明 h1 请求被 auto 接受循环
//!   正确识别、身份头替换、路由到 api2、TLS 握手全部走通；这条本身返回业务错误是因为测试
//!   用的是 session token 冒充"user API key"（两者不是一回事），不是透传的问题。
//! - `POST /agent.v1.AgentService/Run`（HTTP/2，`application/connect+proto`）→
//!   `agentn.global.api5.cursor.sh` 回一个标准 Connect 信封（5 字节前缀 + JSON），业务错误是
//!   `ERROR_GPT_4_VISION_PREVIEW_RATE_LIMIT`/"Update Required"（因为测试请求没带
//!   `x-cursor-client-version` 等头，curl 不是真的 `cursor-agent`）——证明 BiDi 路径的路由、
//!   身份头替换（走到了版本校验这一步，说明 checksum / session-id 都过了鉴权）、Connect 响应
//!   帧的原样转发全部成立。
//!
//! 没能跑通的是完整的 `cursor-agent -e ... --agent-endpoint ... --print "PONG"`：卡在
//! `cursor-agent` **自己的** CLI 登录态（`~/.cursor/agent-cli-state.json` 之外、系统层面的
//! 会话），和本网关要转发的 Cursor 账号登录态是两件事——CLI 第一次要用需要跑一次
//! `cursor-agent login`（打开浏览器走 OAuth），这一步需要人在浏览器里点一下，非交互环境里
//! 做不完。这不是透传实现的问题：上面两条真请求已经把"透传要做的全部事"（h1/h2 自动识别、
//! 路由分流、身份头替换、Connect 帧原样转发）在真上游上验证过了。
//!
//! 联调过程中还顺手读了 `cursor-agent` 的 bundle 源码（Node.js，直接 grep 得到，不是靠猜）：
//! `--agent-endpoint` 没有对应的环境变量（`--help` 里也不显示，是隐藏选项），默认值是
//! "使用 `-e`/`CURSOR_API_ENDPOINT` 的值"；服务端还能通过隐私配置下发
//! `network.useHttp1ForAgent` 来告诉客户端 agent 传输走 h1 还是 h2，默认是 h2
//! （`unknown_server_config_default_http2`）。
//!
//! ## 做不到的事：额度记账
//!
//! 因为不解 body，拿不到 usage、也几乎看不到 Connect 协议的业务错误（`connect.rs` 的文档
//! 说得很清楚：Connect 错误几乎总是包在 HTTP 200 里的流尾 JSON，不在 HTTP 状态码上）。这里
//! 只能做最粗的事：HTTP 状态码非 2xx 时按状态码给 [`Lane::report`] 一个近似分类，让冷却 /
//! 耗尽机制还能生效；但大多数业务错误（限流、鉴权失效）在透传模式下网关是看不见的，只能
//! 指望 `cursor-agent` 自己的重试逻辑，或用户在网关界面手动 `reset_lane` / `set_current`。
//!
//! ## 唯一的例外：IDE Agent 面板拦截（`/aiserver.v1.InferenceService/Stream`）
//!
//! 装了 [`PassthroughContext::with_intercept`] 之后，这一条路径**解 body**（`intercept` 模块）：
//! 请求是一个信封、收满再转不伤流式，可以在这里改写上下文；响应逐帧原样转给客户端，同时
//! 分一份给只读解码器挑 usage / 路由模型 / 错误，流结束记一行账本。别的路径一个字节都不看，
//! 上面那段「做不到」对它们照旧成立。

use crate::error::{UpstreamError, UpstreamKind};
use crate::grokbot::GrokBotStreamAuth;
use crate::headers::RequestNonce;
use crate::intercept::{
    self, Collected, CollectedError, InterceptHub, InterceptRecord, RequestInfo, UsageCollector,
    INFERENCE_STREAM_PATH,
};
use crate::lane::{Credential, Lane, Outcome};
use crate::ledger::{Ledger, RequestRecord, SOURCE_IDE_AGENT};
use crate::normalized::Usage;
use bytes::Bytes;
use http::header::{HeaderName, HeaderValue, CONTENT_LENGTH, HOST};
use http::uri::PathAndQuery;
use http::{HeaderMap, Method, Request, Response, StatusCode, Uri};
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::{Body, Frame, Incoming, SizeHint};
use hyper::service::service_fn;
use hyper_rustls::HttpsConnectorBuilder;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::client::legacy::{Client, Error as ClientError};
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use std::convert::Infallible;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::OnceCell;

/// 冷却 / 耗尽账本按 (账号, 模型) 记；透传不知道模型是什么，借一个固定的伪模型名占位，
/// 不会撞上翻译模式真实模型名的键。
const PASSTHROUGH_MODEL: &str = "__passthrough__";

/// 统一的转发 body 类型：入站 `Incoming`、出站合成的错误响应（`Full`）都装进同一个箱子，
/// 这样处理函数才能有一个单一的返回类型。装箱本身不解码、不缓冲整条消息——帧到就转发，
/// 这正是"透传不解 body"的实现方式。
type ProxyBody = BoxBody<Bytes, Box<dyn std::error::Error + Send + Sync>>;

fn boxed_incoming(body: Incoming) -> ProxyBody {
    body.map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        .boxed()
}

fn text_body(s: impl Into<Bytes>) -> ProxyBody {
    Full::new(s.into())
        .map_err(|never: Infallible| match never {})
        .boxed()
}

/// 把入站请求体原样落盘的目录。空 = 关，这是默认。
///
/// 开着会把整个请求体先收进内存再转发，**流式就没了**——所以它只该在一次明确的取证里临时
/// 打开：录官方 Cursor 真实发出的 `InferenceStreamRequest`，拿去和我们发的逐字节比。
/// 落盘的是未解密的业务明文（提示词、工具定义都在里面），用完就删。
fn dump_dir() -> Option<std::path::PathBuf> {
    let raw = std::env::var("NEXUS_PASSTHROUGH_DUMP_DIR").ok()?;
    let dir = raw.trim();
    if dir.is_empty() {
        return None;
    }
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// 文件名带上时间戳与方法名，一次会话里几十发请求才分得清先后。
fn dump_request(dir: &std::path::Path, path: &str, body: &[u8]) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let method = path.rsplit('/').next().unwrap_or("unknown");
    let file = dir.join(format!("{stamp}-{method}.bin"));
    match std::fs::write(&file, body) {
        Ok(()) => tracing::info!(path, bytes = body.len(), file = %file.display(), "已录请求体"),
        Err(err) => tracing::warn!(%err, file = %file.display(), "请求体落盘失败"),
    }
}

// ---------- 路由：路径前缀 → 上游 ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upstream {
    /// `aiserver.v1.*` / `auth/*` / `v1/traces`：探针确认的 `-e` 覆盖范围。
    Api2,
    /// `agent.v1.*`：agentic 主循环，`-e` 管不到，只能靠 `--agent-endpoint`。
    AgentApi5,
}

/// 纯函数：请求路径 → 该转发去哪个上游。认不出的路径返回 `None`，调用方回 404——
/// 总比把陌生流量悄悄转去某个固定上游更安全。
pub fn classify_path(path: &str) -> Option<Upstream> {
    if path.starts_with("/agent.v1.") {
        Some(Upstream::AgentApi5)
    } else if path.starts_with("/aiserver.v1.")
        || path.starts_with("/auth/")
        || path == "/v1/traces"
    {
        Some(Upstream::Api2)
    } else {
        None
    }
}

/// 一个上游的落地地址。生产环境是真域名 + TLS；集成测试换成本地假上游 + 明文 h2，
/// 好在不启网络、不做 TLS 握手的情况下把整条转发路径测穿。
#[derive(Debug, Clone)]
pub struct Target {
    pub authority: String,
    pub tls: bool,
}

#[derive(Debug, Clone)]
pub struct Targets {
    pub api2: Target,
    pub agent: Target,
}

impl Default for Targets {
    fn default() -> Self {
        Self {
            api2: Target {
                authority: "api2.cursor.sh".into(),
                tls: true,
            },
            agent: Target {
                authority: "agentn.global.api5.cursor.sh".into(),
                tls: true,
            },
        }
    }
}

impl Targets {
    fn resolve(&self, u: Upstream) -> &Target {
        match u {
            Upstream::Api2 => &self.api2,
            Upstream::AgentApi5 => &self.agent,
        }
    }
}

// ---------- 头处理 ----------

/// hop-by-hop：只在这一条 TCP 连接上有意义的头，换了一条新连接（去上游）就该丢掉。
/// 照 RFC 9110 §7.6.1 抄，外加 `host`——host 单独按上游地址重写，不能和客户端发来的混着留。
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "trailers",
    "transfer-encoding",
    "upgrade",
    "host",
];

fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.contains(&name)
}

/// 只换身份，不换别的。六个键对应探针列出的清单；`checksum` 依赖时间戳，每次转发都要重算
/// （借用 `headers::RequestNonce` 而不是自己再写一遍取时间戳的代码）。
fn identity_overrides(credential: &Credential, client_type: &str) -> [(&'static str, String); 6] {
    let now_ms = RequestNonce::now().now_ms;
    [
        (
            "authorization",
            format!("Bearer {}", credential.access_token),
        ),
        ("x-cursor-checksum", credential.identity.checksum(now_ms)),
        ("x-client-key", credential.identity.client_key()),
        ("x-session-id", credential.identity.session_id()),
        (
            "x-cursor-config-version",
            credential.identity.config_version(),
        ),
        ("x-cursor-client-type", client_type.to_string()),
    ]
}

/// 走 Grok Bot 额度时额外钉的两个头。头容忍度矩阵（2026-09-09）证明不带也放行，带上只是
/// 和 Grok Bot 客户端 / Box relay 发出的形态一致，少一个变量。
const GROKBOT_CLIENT_VERSION: &str = nexus_grokbot::credential::DEFAULT_CLIENT_VERSION;
const GROKBOT_NAMESPACE: &str = nexus_grokbot::credential::DEFAULT_NAMESPACE;

/// 组去上游的请求头：客户端原样的头（去掉 hop-by-hop）+ 身份覆盖 + 重写后的 `host`。
#[cfg(test)]
fn forward_request_headers(
    orig: &HeaderMap,
    credential: &Credential,
    client_type: &str,
    upstream_authority: &str,
) -> HeaderMap {
    forward_request_headers_ext(orig, credential, client_type, upstream_authority, false)
}

/// `via_grokbot`：走 Grok Bot 额度时多钉两个形态头。
fn forward_request_headers_ext(
    orig: &HeaderMap,
    credential: &Credential,
    client_type: &str,
    upstream_authority: &str,
    via_grokbot: bool,
) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(orig.len() + 10);
    for (name, value) in orig.iter() {
        if is_hop_by_hop(name.as_str()) {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    for (name, value) in identity_overrides(credential, client_type) {
        let value = HeaderValue::from_str(&value).expect("identity 纯函数只产生合法 header 字节");
        out.insert(HeaderName::from_static(name), value);
    }
    if via_grokbot {
        out.insert(
            HeaderName::from_static("x-cursor-client-version"),
            HeaderValue::from_static(GROKBOT_CLIENT_VERSION),
        );
        out.insert(
            HeaderName::from_static("x-sand-box-namespace"),
            HeaderValue::from_static(GROKBOT_NAMESPACE),
        );
    }
    out.insert(
        HOST,
        HeaderValue::from_str(upstream_authority).expect("authority 是我们自己配的常量"),
    );
    out
}

/// 把客户端**原始**的身份头记一行（`authorization` 只留前缀）。
///
/// 这是给 remote SSH 那条链路排查用的：远程 agent-host 发的 `x-cursor-client-type` 到底是
/// `sand` 还是 `ide`，直接决定这一发算在 sand 额度还是账号自己的额度上，而这在补丁侧只能靠推。
/// 我们马上要把这些头覆盖掉，所以不在这里看就再也看不到了。
fn log_inbound_identity(path: &str, headers: &HeaderMap) {
    if !tracing::enabled!(tracing::Level::DEBUG) {
        return;
    }
    let peek = |name: &str| -> String {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|v| {
                if name == "authorization" {
                    format!("{}…", v.chars().take(16).collect::<String>())
                } else {
                    v.to_string()
                }
            })
            .unwrap_or_else(|| "(缺)".into())
    };
    tracing::debug!(
        path,
        client_type = %peek("x-cursor-client-type"),
        client_version = %peek("x-cursor-client-version"),
        checksum = %peek("x-cursor-checksum"),
        client_key = %peek("x-client-key"),
        session_id = %peek("x-session-id"),
        authorization = %peek("authorization"),
        "客户端原始身份头（覆盖前）"
    );
}

/// 响应方向只去 hop-by-hop，其余原样带回——业务错误在流尾 JSON 里，我们不解，客户端自己认。
fn strip_hop_by_hop_headers(orig: HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(orig.len());
    for (name, value) in orig.into_iter().filter_map(|(n, v)| n.map(|n| (n, v))) {
        if !is_hop_by_hop(name.as_str()) {
            out.append(name, value);
        }
    }
    out
}

fn build_upstream_uri(scheme: &str, authority: &str, orig: &Uri) -> Option<Uri> {
    let path_and_query = orig
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| PathAndQuery::from_static("/"));
    Uri::builder()
        .scheme(scheme)
        .authority(authority)
        .path_and_query(path_and_query)
        .build()
        .ok()
}

// ---------- 会话级账号钉住 ----------

/// 钉在一条入站连接上的账号。同一条连接不管转发多少个请求（unary 或 BiDi），都用它——
/// 连接结束（这个 `ConnState` 被 drop）才谈得上下一条连接换号。
struct ConnState {
    credential: OnceCell<Credential>,
}

impl ConnState {
    fn new() -> Self {
        Self {
            credential: OnceCell::new(),
        }
    }

    async fn credential(&self, lane: &dyn Lane) -> Result<Credential, UpstreamError> {
        self.credential
            .get_or_try_init(|| lane.acquire(PASSTHROUGH_MODEL))
            .await
            .cloned()
    }
}

// ---------- 出站客户端 ----------

/// 两个客户端：TLS 给生产上游（api2 / api5，ALPN 里 h1/h2 都报，服务端认哪个用哪个——
/// `/auth/*` 走 h1，`agent.v1.*`/`aiserver.v1.*` 的 Connect 调用走 h2，参见模块文档的教训）；
/// 明文给集成测试的假上游（本地 h2c，没有 TLS 可协商，只能靠 `http2_only(true)` 强制客户端
/// 直接讲 HTTP/2 prior-knowledge）。生产路径只会用到前者。
struct Clients {
    tls: Client<hyper_rustls::HttpsConnector<HttpConnector>, ProxyBody>,
    plain: Client<HttpConnector, ProxyBody>,
}

impl Clients {
    fn new() -> Self {
        // 系统证书库优先（企业代理的自签 CA 只在那儿），读不出来再退回内置锚点。
        // 退回而不是报错：证书库不可读是环境问题，不该让整个透传口起不来。
        let builder = HttpsConnectorBuilder::new();
        let https = match builder.with_native_roots() {
            Ok(b) => b,
            Err(err) => {
                tracing::warn!(%err, "读不到系统根证书，回退到内置锚点");
                HttpsConnectorBuilder::new().with_webpki_roots()
            }
        }
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();
        let tls = Client::builder(TokioExecutor::new()).build(https);
        let plain = Client::builder(TokioExecutor::new())
            .http2_only(true)
            .build(HttpConnector::new());
        Self { tls, plain }
    }

    async fn send(
        &self,
        tls: bool,
        req: Request<ProxyBody>,
    ) -> Result<Response<Incoming>, ClientError> {
        if tls {
            self.tls.request(req).await
        } else {
            self.plain.request(req).await
        }
    }
}

/// 一次透传服务要用到的全部依赖：号从哪来、报哪个通道标签、上游落在哪。
pub struct PassthroughContext {
    lane: Arc<dyn Lane>,
    client_type: String,
    targets: Targets,
    clients: Clients,
    /// 装上就对 `InferenceService/Stream` 解 body（改写 + 记用量）；`None` = 纯盲转发。
    intercept: Option<Arc<InterceptHub>>,
    /// 拦截记的账本行往哪写；`None` 只留内存里的最近记录。
    ledger: Option<Arc<Ledger>>,
    /// Grok Bot 额度开关（只对 `InferenceService/Stream`）。`None` = 这个部署没有 Grok Bot 桥。
    grokbot: Option<Arc<GrokBotStreamAuth>>,
}

impl PassthroughContext {
    pub fn new(lane: Arc<dyn Lane>, client_type: String) -> Self {
        Self::with_targets(lane, client_type, Targets::default())
    }

    pub fn with_targets(lane: Arc<dyn Lane>, client_type: String, targets: Targets) -> Self {
        Self {
            lane,
            client_type,
            targets,
            clients: Clients::new(),
            intercept: None,
            ledger: None,
            grokbot: None,
        }
    }

    /// 打开 IDE Agent 面板拦截。
    pub fn with_intercept(mut self, hub: Arc<InterceptHub>, ledger: Option<Arc<Ledger>>) -> Self {
        self.intercept = Some(hub);
        self.ledger = ledger;
        self
    }

    /// 挂上 Grok Bot 额度开关（开关本身是热的，运行中可切）。
    pub fn with_grokbot(mut self, grokbot: Arc<GrokBotStreamAuth>) -> Self {
        self.grokbot = Some(grokbot);
        self
    }
}

/// 这一发用谁的身份出去。
struct Identity {
    credential: Credential,
    client_type: String,
    /// 走 Grok Bot 额度：不回报 Lane（它不是接力队里的号），头上多钉两项。
    via_grokbot: bool,
}

async fn resolve_identity(
    path: &str,
    conn: &ConnState,
    ctx: &PassthroughContext,
) -> Result<Identity, UpstreamError> {
    if path == INFERENCE_STREAM_PATH {
        if let Some(g) = ctx.grokbot.as_ref().filter(|g| g.enabled()) {
            let credential = g.credential().await?;
            return Ok(Identity {
                credential,
                client_type: "sand".into(),
                via_grokbot: true,
            });
        }
    }
    Ok(Identity {
        credential: conn.credential(ctx.lane.as_ref()).await?,
        client_type: ctx.client_type.clone(),
        via_grokbot: false,
    })
}

fn status_or_bad_gateway(status: u16) -> StatusCode {
    StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY)
}

fn text_response(status: StatusCode, msg: &str) -> Response<ProxyBody> {
    let mut resp = Response::new(text_body(msg.to_string()));
    *resp.status_mut() = status;
    resp.headers_mut().insert(
        http::header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    resp
}

/// HTTP 状态码非 2xx 时给 Lane 一个粗分类，让冷却 / 耗尽还能生效——模块文档里说过，这是
/// 透传模式在"不解 body"前提下唯一能做的记账精度。
fn report_from_status(lane: &dyn Lane, credential: &Credential, status: StatusCode) {
    if status.is_success() {
        lane.report(
            credential,
            PASSTHROUGH_MODEL,
            Outcome::Ok(&Usage::default()),
        );
        return;
    }
    let kind = match status.as_u16() {
        401 => UpstreamKind::Auth,
        402 => UpstreamKind::Quota,
        403 => UpstreamKind::Forbidden,
        429 => UpstreamKind::RateLimit,
        s if s >= 500 => UpstreamKind::Upstream,
        // 4xx 里剩下的（400 等）多半是请求本身的问题，不该怪号。
        _ => return,
    };
    let err = UpstreamError::new(kind, status.as_u16(), format!("透传上游返回 HTTP {status}"));
    lane.report(credential, PASSTHROUGH_MODEL, Outcome::Err(&err));
}

async fn handle(
    req: Request<Incoming>,
    conn: Arc<ConnState>,
    ctx: Arc<PassthroughContext>,
) -> Result<Response<ProxyBody>, Infallible> {
    let path = req.uri().path().to_string();
    tracing::debug!(method = %req.method(), path, "透传收到请求");
    let Some(upstream) = classify_path(&path) else {
        return Ok(text_response(
            StatusCode::NOT_FOUND,
            "nexus-gateway 透传：没有这条路径的路由（只认 agent.v1.* / aiserver.v1.* / auth/* / v1/traces）",
        ));
    };

    let Identity {
        credential,
        client_type,
        via_grokbot,
    } = match resolve_identity(&path, &conn, &ctx).await {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(path, kind = err.kind.as_str(), %err.message, "透传取身份失败");
            return Ok(text_response(
                status_or_bad_gateway(err.status),
                &err.message,
            ));
        }
    };

    let target = ctx.targets.resolve(upstream).clone();
    let scheme = if target.tls { "https" } else { "http" };
    let Some(uri) = build_upstream_uri(scheme, &target.authority, req.uri()) else {
        return Ok(text_response(StatusCode::BAD_GATEWAY, "拼不出上游 URI"));
    };

    let (parts, body) = req.into_parts();
    if parts.method == Method::CONNECT {
        // CONNECT 不是 Connect 协议要用的方法，也不该出现在这条透传路径上；拒绝而不是
        // 转发一个我们没想清楚语义的方法。
        return Ok(text_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "不支持 CONNECT",
        ));
    }
    log_inbound_identity(&path, &parts.headers);
    let headers = forward_request_headers_ext(
        &parts.headers,
        &credential,
        &client_type,
        &target.authority,
        via_grokbot,
    );

    if path == INFERENCE_STREAM_PATH {
        if let Some(hub) = ctx.intercept.clone() {
            return Ok(handle_inference(InferenceCall {
                method: parts.method,
                uri,
                headers,
                body,
                path,
                credential,
                via_grokbot,
                target,
                ctx,
                hub,
            })
            .await);
        }
    }

    // 默认按流转发（一个字节都不缓冲）；只有显式开了取证开关、且这一发是推理请求时才收进
    // 内存落盘——那种时候流式已经不重要，能拿到字节才重要。
    let out_body = match dump_dir() {
        Some(dir) if path.contains("Inference") => match body.collect().await {
            Ok(collected) => {
                let bytes = collected.to_bytes();
                dump_request(&dir, &path, &bytes);
                text_body(bytes)
            }
            Err(err) => {
                tracing::warn!(%err, path, "读入站请求体失败");
                return Ok(text_response(StatusCode::BAD_GATEWAY, "读请求体失败"));
            }
        },
        _ => boxed_incoming(body),
    };

    let mut out_req = Request::new(out_body);
    *out_req.method_mut() = parts.method;
    *out_req.uri_mut() = uri;
    *out_req.headers_mut() = headers;

    match ctx.clients.send(target.tls, out_req).await {
        Ok(resp) => {
            if !via_grokbot {
                report_from_status(ctx.lane.as_ref(), &credential, resp.status());
            }
            let (rparts, rbody) = resp.into_parts();
            let mut out = Response::new(boxed_incoming(rbody));
            *out.status_mut() = rparts.status;
            *out.headers_mut() = strip_hop_by_hop_headers(rparts.headers);
            Ok(out)
        }
        Err(err) => {
            tracing::warn!(%err, path, upstream = ?upstream, "透传转发失败");
            Ok(text_response(
                StatusCode::BAD_GATEWAY,
                &format!("转发到上游失败：{err}"),
            ))
        }
    }
}

// ---------- IDE Agent 面板拦截：InferenceService/Stream ----------

struct InferenceCall {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Incoming,
    path: String,
    credential: Credential,
    via_grokbot: bool,
    target: Target,
    ctx: Arc<PassthroughContext>,
    hub: Arc<InterceptHub>,
}

/// 收满请求（就一个信封）→ 按规则改写 → 转发；响应逐帧转回并分一份给解码器，流结束记一行。
///
/// 任何一步解不开都退回「原字节转发」，拦截失败不该让 Agent 面板停摆。
async fn handle_inference(call: InferenceCall) -> Response<ProxyBody> {
    let InferenceCall {
        method,
        uri,
        mut headers,
        body,
        path,
        credential,
        via_grokbot,
        target,
        ctx,
        hub,
    } = call;
    let started = Instant::now();
    let bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) => {
            tracing::warn!(%err, path, "IDE 拦截：读入站请求体失败");
            return text_response(StatusCode::BAD_GATEWAY, "读请求体失败");
        }
    };
    if let Some(dir) = dump_dir() {
        dump_request(&dir, &path, &bytes);
    }

    let rule = hub.rule();
    let (out_bytes, info, rewritten) = match intercept::rewrite_request(&bytes, &rule) {
        Ok(r) => (Bytes::from(r.body), r.info, r.rewritten),
        Err(err) => {
            tracing::warn!(%err, path, "IDE 拦截：请求解不开，按原样转发");
            (bytes, RequestInfo::default(), false)
        }
    };
    tracing::debug!(
        conversation = info.conversation_id.as_deref().unwrap_or("-"),
        model = info.model_id.as_deref().unwrap_or("-"),
        messages = info.message_count,
        rewritten,
        bytes = out_bytes.len(),
        "IDE 拦截：转发推理请求"
    );
    // 改写过长度就变了；就算没改，也用我们手里这份的长度盖掉客户端报的那个。
    headers.insert(CONTENT_LENGTH, HeaderValue::from(out_bytes.len()));

    let recorder = Recorder {
        hub,
        ledger: ctx.ledger.clone(),
        account: credential.label.clone(),
        model: info
            .model_id
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        info,
        rewritten,
        started,
    };

    let mut out_req = Request::new(text_body(out_bytes));
    *out_req.method_mut() = method;
    *out_req.uri_mut() = uri;
    *out_req.headers_mut() = headers;

    match ctx.clients.send(target.tls, out_req).await {
        Ok(resp) => {
            let status = resp.status();
            if !via_grokbot {
                report_from_status(ctx.lane.as_ref(), &credential, status);
            }
            let (rparts, rbody) = resp.into_parts();
            if !status.is_success() {
                // 非 2xx 不是 Connect 流，没有帧可解；按状态码记一行失败，body 原样透传。
                let kind = match status.as_u16() {
                    401 => UpstreamKind::Auth,
                    402 => UpstreamKind::Quota,
                    403 => UpstreamKind::Forbidden,
                    429 => UpstreamKind::RateLimit,
                    s if s >= 500 => UpstreamKind::Upstream,
                    _ => UpstreamKind::BadRequest,
                };
                recorder.fail(status.as_u16(), kind, format!("上游 HTTP {status}"));
                let mut out = Response::new(boxed_incoming(rbody));
                *out.status_mut() = rparts.status;
                *out.headers_mut() = strip_hop_by_hop_headers(rparts.headers);
                return out;
            }
            let mut out = Response::new(TeeBody::new(rbody, recorder).boxed());
            *out.status_mut() = rparts.status;
            *out.headers_mut() = strip_hop_by_hop_headers(rparts.headers);
            out
        }
        Err(err) => {
            tracing::warn!(%err, path, "IDE 拦截：转发到上游失败");
            recorder.fail(
                502,
                UpstreamKind::Upstream,
                format!("转发到上游失败：{err}"),
            );
            text_response(StatusCode::BAD_GATEWAY, &format!("转发到上游失败：{err}"))
        }
    }
}

/// 一次拦截结束时把结果写成两份：账本一行（数字），最近记录一条（给界面）。
struct Recorder {
    hub: Arc<InterceptHub>,
    ledger: Option<Arc<Ledger>>,
    account: String,
    model: String,
    info: RequestInfo,
    rewritten: bool,
    started: Instant,
}

impl Recorder {
    fn fail(self, status: u16, kind: UpstreamKind, message: String) {
        self.finish(Collected {
            error: Some(CollectedError {
                status,
                kind,
                message,
            }),
            ..Collected::default()
        });
    }

    fn finish(self, c: Collected) {
        let duration_ms = self.started.elapsed().as_millis() as u64;
        let ttft_ms = c
            .first_output_at
            .map(|t| t.saturating_duration_since(self.started).as_millis() as u64);
        // 没见到 END_STREAM 也没见到错误 = 客户端中途停了（Agent 面板点 Stop 很常见），
        // 不算失败；usage 多半也没到，`measured=false` 会说明这一点。
        let (ok, status, kind, error) = match &c.error {
            Some(e) => (false, e.status, Some(e.kind), Some(e.message.clone())),
            None => (true, 200, None, None),
        };
        if let Some(ledger) = &self.ledger {
            ledger.record(RequestRecord {
                channel: crate::channel::CURSOR,
                account: &self.account,
                model: &self.model,
                routed: c.routed.as_deref(),
                dialect: SOURCE_IDE_AGENT,
                ok,
                status,
                kind: kind.map(UpstreamKind::as_str),
                usage: c.usage,
                usage_measured: c.measured,
                ttft_ms,
                duration_ms,
            });
        }
        let at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        self.hub.record(InterceptRecord {
            at: nexus_core::clock::iso_from_millis(at_ms).unwrap_or_default(),
            account: self.account,
            conversation_id: self.info.conversation_id,
            model: self.model,
            routed: c.routed,
            ok,
            status,
            kind: kind.map(|k| k.as_str().to_string()),
            error,
            input_tokens: c.usage.input_tokens,
            output_tokens: c.usage.output_tokens,
            cache_read_tokens: c.usage.cache_read_tokens,
            cache_write_tokens: c.usage.cache_write_tokens,
            measured: c.measured,
            rewritten: self.rewritten,
            message_count: self.info.message_count,
            ttft_ms,
            duration_ms,
        });
    }
}

/// 把上游响应帧原样交给客户端，同时喂一份给解码器。流走到头、出错、或客户端提前断开
/// （`Drop`）都会结账——三条路只结一次。
struct TeeBody {
    inner: Incoming,
    collector: Option<UsageCollector>,
    recorder: Option<Recorder>,
}

impl TeeBody {
    fn new(inner: Incoming, recorder: Recorder) -> Self {
        Self {
            inner,
            collector: Some(UsageCollector::default()),
            recorder: Some(recorder),
        }
    }

    fn finalize(&mut self) {
        if let (Some(collector), Some(recorder)) = (self.collector.take(), self.recorder.take()) {
            recorder.finish(collector.finish());
        }
    }
}

impl Body for TeeBody {
    type Data = Bytes;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = &mut *self;
        match Pin::new(&mut this.inner).poll_frame(cx) {
            Poll::Ready(Some(Ok(frame))) => {
                if let (Some(data), Some(collector)) = (frame.data_ref(), this.collector.as_mut()) {
                    collector.push(data);
                }
                Poll::Ready(Some(Ok(frame)))
            }
            Poll::Ready(Some(Err(err))) => {
                this.finalize();
                Poll::Ready(Some(Err(Box::new(err))))
            }
            Poll::Ready(None) => {
                this.finalize();
                Poll::Ready(None)
            }
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for TeeBody {
    fn drop(&mut self) {
        self.finalize();
    }
}

// ---------- 入站接受循环（h1 + h2 自动嗅探） ----------

async fn serve_one(stream: TcpStream, ctx: Arc<PassthroughContext>) -> std::io::Result<()> {
    let _ = stream.set_nodelay(true);
    let io = TokioIo::new(stream);
    let conn = Arc::new(ConnState::new());
    let svc = service_fn(move |req: Request<Incoming>| {
        let conn = conn.clone();
        let ctx = ctx.clone();
        async move { handle(req, conn, ctx).await }
    });
    // auto：每条连接先嗅探是不是 h2 前奏，不是就退回 HTTP/1.1——模块文档里说过，
    // `/auth/*` 是纯 HTTP/1.1 的 `fetch`，`agent.v1.*` 的 BiDi 又必须是 h2，
    // 同一个端口两种都要接。
    auto::Builder::new(TokioExecutor::new())
        .serve_connection(io, svc)
        .await
        .map_err(|e| std::io::Error::other(e.to_string()))
}

/// 透传的入站主循环：一条连接一个任务、一份钉住的账号（[`ConnState`]），放进
/// [`tokio::task::JoinSet`] 而不是散养 `tokio::spawn`，是为了让
/// "关掉网关"能真正带走在途连接：`JoinSet` 被 drop（无论是 `shutdown` 触发的正常返回，
/// 还是外层把这个 `serve` 的 `JoinHandle` 直接 `abort()`）会自动 abort 它管着的每一条连接，
/// 不会有分离出去的连接任务在后台悄悄留着。
pub async fn serve(
    listener: TcpListener,
    ctx: Arc<PassthroughContext>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    let mut conns = tokio::task::JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                tracing::info!("透传服务收到关停信号，停止接受新连接");
                return Ok(());
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let ctx = ctx.clone();
                conns.spawn(async move {
                    if let Err(err) = serve_one(stream, ctx).await {
                        tracing::debug!(%peer, %err, "透传连接结束");
                    }
                });
            }
            Some(joined) = conns.join_next(), if !conns.is_empty() => {
                if let Err(err) = joined {
                    if !err.is_cancelled() {
                        tracing::debug!(%err, "透传连接任务异常退出");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceIdentity;
    use crate::lane::{BoxFuture, StaticLane};
    use http::header::CONTENT_TYPE;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tokio::io::{AsyncRead, AsyncWriteExt};

    // ---------- 路由分流表 ----------

    #[test]
    fn classify_path_routes_agent_to_api5_and_everything_else_documented_to_api2() {
        assert_eq!(
            classify_path("/agent.v1.AgentService/Run"),
            Some(Upstream::AgentApi5)
        );
        assert_eq!(
            classify_path("/aiserver.v1.InferenceService/Stream"),
            Some(Upstream::Api2)
        );
        assert_eq!(classify_path("/auth/refresh"), Some(Upstream::Api2));
        assert_eq!(classify_path("/v1/traces"), Some(Upstream::Api2));
    }

    #[test]
    fn classify_path_rejects_unknown_prefixes() {
        assert_eq!(classify_path("/"), None);
        assert_eq!(
            classify_path("/v1/chat/completions"),
            None,
            "那是翻译模式的路径，不该被透传接住"
        );
        assert_eq!(
            classify_path("/v1/traces/extra"),
            None,
            "只认精确的 /v1/traces"
        );
        assert_eq!(classify_path("/agent.v2.Something/Run"), None);
    }

    #[test]
    fn targets_default_to_the_real_cursor_hosts() {
        let t = Targets::default();
        assert_eq!(t.resolve(Upstream::Api2).authority, "api2.cursor.sh");
        assert!(t.resolve(Upstream::Api2).tls);
        assert_eq!(
            t.resolve(Upstream::AgentApi5).authority,
            "agentn.global.api5.cursor.sh"
        );
        assert!(t.resolve(Upstream::AgentApi5).tls);
    }

    // ---------- 身份头替换 ----------

    fn cred(label: &str, token: &str) -> Credential {
        Credential {
            label: label.into(),
            access_token: token.into(),
            identity: DeviceIdentity::derived(token),
        }
    }

    #[test]
    fn forward_headers_swap_only_the_six_identity_keys_and_rewrite_host() {
        let mut orig = HeaderMap::new();
        orig.insert(
            HeaderName::from_static("authorization"),
            HeaderValue::from_static("Bearer client-side-stale-token"),
        );
        orig.insert(
            HeaderName::from_static("x-cursor-client-version"),
            HeaderValue::from_static("cli-2026.08.11-e8db854"),
        );
        orig.insert(HOST, HeaderValue::from_static("127.0.0.1:8788"));
        orig.insert(
            HeaderName::from_static("connection"),
            HeaderValue::from_static("keep-alive"),
        );

        let credential = cred("a@x.com", "real-upstream-token");
        let out =
            forward_request_headers(&orig, &credential, "cli", "agentn.global.api5.cursor.sh");

        assert_eq!(
            out.get("authorization").unwrap(),
            "Bearer real-upstream-token"
        );
        assert_eq!(out.get("x-cursor-client-type").unwrap(), "cli");
        assert!(out.contains_key("x-cursor-checksum"));
        assert!(out.contains_key("x-client-key"));
        assert!(out.contains_key("x-session-id"));
        assert!(out.contains_key("x-cursor-config-version"));
        // 客户端自己发的、不在替换清单里的头原样保留。
        assert_eq!(
            out.get("x-cursor-client-version").unwrap(),
            "cli-2026.08.11-e8db854"
        );
        // host 按上游重写，不是客户端发来的那个。
        assert_eq!(out.get(HOST).unwrap(), "agentn.global.api5.cursor.sh");
        // hop-by-hop 被去掉。
        assert!(!out.contains_key("connection"));
    }

    #[test]
    fn forward_headers_checksum_and_session_track_the_chosen_account_not_the_client() {
        let orig = HeaderMap::new();
        let a = forward_request_headers(&orig, &cred("a@x.com", "tok-a"), "cli", "api2.cursor.sh");
        let b = forward_request_headers(&orig, &cred("b@x.com", "tok-b"), "cli", "api2.cursor.sh");
        assert_ne!(a.get("x-client-key"), b.get("x-client-key"));
        assert_ne!(a.get("x-session-id"), b.get("x-session-id"));
    }

    #[test]
    fn response_headers_only_strip_hop_by_hop_and_keep_everything_else() {
        let mut orig = HeaderMap::new();
        orig.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("application/connect+proto"),
        );
        orig.insert(
            HeaderName::from_static("connection"),
            HeaderValue::from_static("close"),
        );
        let out = strip_hop_by_hop_headers(orig);
        assert_eq!(out.get(CONTENT_TYPE).unwrap(), "application/connect+proto");
        assert!(!out.contains_key("connection"));
    }

    #[test]
    fn build_upstream_uri_keeps_the_original_path_and_query() {
        let orig: Uri = "http://127.0.0.1:8788/agent.v1.AgentService/Run?x=1"
            .parse()
            .unwrap();
        let uri = build_upstream_uri("https", "agentn.global.api5.cursor.sh", &orig).unwrap();
        assert_eq!(uri.scheme_str(), Some("https"));
        assert_eq!(
            uri.authority().unwrap().as_str(),
            "agentn.global.api5.cursor.sh"
        );
        assert_eq!(
            uri.path_and_query().unwrap().as_str(),
            "/agent.v1.AgentService/Run?x=1"
        );
    }

    // ---------- 会话级钉住 ----------

    struct CountingSource {
        calls: AtomicUsize,
        labels: Mutex<Vec<&'static str>>,
    }

    impl Lane for CountingSource {
        fn acquire<'a>(
            &'a self,
            _model: &'a str,
        ) -> BoxFuture<'a, Result<Credential, UpstreamError>> {
            Box::pin(async move {
                let n = self.calls.fetch_add(1, Ordering::SeqCst);
                let label = if n == 0 {
                    "first@x.com"
                } else {
                    "second@x.com"
                };
                self.labels.lock().unwrap().push(label);
                Ok(cred(label, &format!("tok-{n}")))
            })
        }
        fn report(&self, _c: &Credential, _model: &str, _o: Outcome<'_>) {}
    }

    #[tokio::test]
    async fn conn_state_pins_the_first_credential_for_every_later_call_on_the_same_connection() {
        let lane = CountingSource {
            calls: AtomicUsize::new(0),
            labels: Mutex::new(vec![]),
        };
        let conn = ConnState::new();
        let a = conn.credential(&lane).await.unwrap();
        let b = conn.credential(&lane).await.unwrap();
        let c = conn.credential(&lane).await.unwrap();
        assert_eq!(a.label, "first@x.com");
        assert_eq!(b.label, "first@x.com", "同一条连接不该在中途换号");
        assert_eq!(c.label, "first@x.com");
        assert_eq!(lane.calls.load(Ordering::SeqCst), 1, "只在第一次真正取号");

        // 一条新连接（新的 ConnState）才谈得上换号。
        let conn2 = ConnState::new();
        let d = conn2.credential(&lane).await.unwrap();
        assert_eq!(d.label, "second@x.com");
    }

    #[test]
    fn report_from_status_maps_the_coarse_categories_it_can_see() {
        let lane = StaticLane::new(cred("a@x.com", "t"));
        // 只验证不 panic、状态码分类不出岔子；StaticLane 不记录调用，这里主要测 report_from_status
        // 本身对每个分支都能构造出合法的 UpstreamError（kind/status 对得上）。
        for status in [200u16, 401, 402, 403, 429, 500, 502, 400, 404] {
            report_from_status(&lane, &cred("a@x.com", "t"), status_or_bad_gateway(status));
        }
    }

    // ---------- 集成：起一个假上游 + 一个透传实例，端到端过一遍 ----------

    /// 极简的假上游：h1 + h2c 都接（`auto::Builder`），不做真的 Connect 协议理解，只看
    /// 请求头 + 把 body 原样回声。用于验证「身份头被换、原头被保留、body 帧原样转发」——
    /// 不需要理解 protobuf。
    async fn spawn_fake_upstream() -> (String, Arc<Mutex<Option<http::HeaderMap>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen_headers: Arc<Mutex<Option<http::HeaderMap>>> = Arc::new(Mutex::new(None));
        let seen_clone = seen_headers.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let seen = seen_clone.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let seen = seen.clone();
                    let svc = service_fn(move |req: Request<Incoming>| {
                        let seen = seen.clone();
                        async move {
                            *seen.lock().unwrap() = Some(req.headers().clone());
                            // 统一逐帧回声：读一段就立刻写一段，不等对端写完——这既能验证 unary
                            // （body 原样转发）也能验证 BiDi（两端能在同一次交换里互相看到
                            // "还没写完对方就先收到了"），不用为两种测试各写一套假上游逻辑。
                            let mut body = req.into_body();
                            let (mut tx, rx) = tokio::io::duplex(64 * 1024);
                            tokio::spawn(async move {
                                while let Some(frame) = body.frame().await {
                                    let Ok(frame) = frame else { break };
                                    if let Some(data) = frame.data_ref() {
                                        if tx.write_all(data).await.is_err() {
                                            break;
                                        }
                                    }
                                }
                            });
                            let stream = tokio_util_read_to_body_stream(rx);
                            Ok::<_, Infallible>(Response::new(stream))
                        }
                    });
                    // 假上游也走 auto：既服务 BiDi 测试用的 h2 客户端，也服务新加的 h1
                    // （模拟 `/auth/*`）测试用的 HTTP/1.1 客户端，不用为两种协议各写一个假上游。
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });
        (format!("127.0.0.1:{}", addr.port()), seen_headers)
    }

    /// 把一个 `AsyncRead` 端包成一个 `ProxyBody`：按到达的字节切帧往外发，不是攒完再发——
    /// 这样假上游的回声测试才能真的测出"流式"而不是"整体收发"。
    fn tokio_util_read_to_body_stream(mut reader: tokio::io::DuplexStream) -> ProxyBody {
        use futures_util::stream::poll_fn;
        use http_body_util::StreamBody;
        let s = poll_fn(move |cx| {
            let mut buf = [0u8; 4096];
            let mut read_buf = tokio::io::ReadBuf::new(&mut buf);
            match std::pin::Pin::new(&mut reader).poll_read(cx, &mut read_buf) {
                std::task::Poll::Ready(Ok(())) => {
                    let n = read_buf.filled().len();
                    if n == 0 {
                        std::task::Poll::Ready(None)
                    } else {
                        let bytes = Bytes::copy_from_slice(read_buf.filled());
                        std::task::Poll::Ready(Some(Ok::<
                            _,
                            Box<dyn std::error::Error + Send + Sync>,
                        >(
                            hyper::body::Frame::data(bytes)
                        )))
                    }
                }
                std::task::Poll::Ready(Err(e)) => std::task::Poll::Ready(Some(Err(
                    Box::new(e) as Box<dyn std::error::Error + Send + Sync>
                ))),
                std::task::Poll::Pending => std::task::Poll::Pending,
            }
        });
        StreamBody::new(s).boxed()
    }

    struct OneShotLane {
        credential: Credential,
        oks: AtomicUsize,
        errs: AtomicUsize,
    }
    impl Lane for OneShotLane {
        fn acquire<'a>(
            &'a self,
            _model: &'a str,
        ) -> BoxFuture<'a, Result<Credential, UpstreamError>> {
            Box::pin(async move { Ok(self.credential.clone()) })
        }
        fn report(&self, _c: &Credential, _model: &str, o: Outcome<'_>) {
            match o {
                Outcome::Ok(_) => self.oks.fetch_add(1, Ordering::SeqCst),
                Outcome::Err(_) => self.errs.fetch_add(1, Ordering::SeqCst),
            };
        }
    }

    async fn spawn_passthrough(targets: Targets, lane: Arc<dyn Lane>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let ctx = Arc::new(PassthroughContext::with_targets(
            lane,
            "cli".into(),
            targets,
        ));
        tokio::spawn(serve(listener, ctx, std::future::pending()));
        format!("127.0.0.1:{}", addr.port())
    }

    /// 一个手写的极简 h2c 客户端：只用来在测试里验证"请求头被换了""body 被转发了"，
    /// 不引入又一个高层客户端依赖。
    async fn h2c_get_echo(gateway_addr: &str, path: &str, body: &[u8]) -> (StatusCode, Vec<u8>) {
        let stream = TcpStream::connect(gateway_addr).await.unwrap();
        let io = TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http2::handshake(TokioExecutor::new(), io)
            .await
            .unwrap();
        tokio::spawn(conn);
        let uri: Uri = format!("http://{gateway_addr}{path}").parse().unwrap();
        let req = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("authorization", "Bearer stale-client-side-token")
            .header("x-cursor-client-version", "cli-2026.08.11-e8db854")
            .body(text_body(Bytes::copy_from_slice(body)))
            .unwrap();
        let resp = sender.send_request(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }

    #[tokio::test]
    async fn unary_request_headers_are_replaced_and_body_is_forwarded_unmodified() {
        let (upstream_addr, seen) = spawn_fake_upstream().await;
        let credential = cred("relay@x.com", "real-token-for-upstream");
        let lane: Arc<dyn Lane> = Arc::new(OneShotLane {
            credential: credential.clone(),
            oks: AtomicUsize::new(0),
            errs: AtomicUsize::new(0),
        });
        let targets = Targets {
            api2: Target {
                authority: upstream_addr.clone(),
                tls: false,
            },
            agent: Target {
                authority: upstream_addr.clone(),
                tls: false,
            },
        };
        let gw = spawn_passthrough(targets, lane).await;

        let (status, body) = h2c_get_echo(
            &gw,
            "/aiserver.v1.InferenceService/Stream",
            b"raw-protobuf-bytes",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"raw-protobuf-bytes", "body 原样转发，网关没有解它");

        let headers = seen.lock().unwrap().clone().expect("假上游该收到一个请求");
        assert_eq!(
            headers.get("authorization").unwrap(),
            "Bearer real-token-for-upstream",
            "客户端带的旧 token 必须被换掉"
        );
        assert_eq!(
            headers.get("x-cursor-client-version").unwrap(),
            "cli-2026.08.11-e8db854",
            "不在替换清单里的头原样转发"
        );
        assert!(headers.contains_key("x-cursor-checksum"));
        assert_eq!(headers.get(HOST).unwrap(), upstream_addr.as_str());
    }

    /// 真机联调撞出来的坑：`/auth/exchange_user_api_key` / `/auth/poll` 是纯 HTTP/1.1 的
    /// `fetch`，和 BiDi 共用同一个透传端口。这里用 hyper 的 h1 客户端直连（不是 h2），
    /// 验证 auto 接受循环真的会退回 HTTP/1.1，而不是要求所有连接先讲 h2 前奏。
    #[tokio::test]
    async fn http1_requests_to_auth_paths_are_accepted_and_forwarded_like_h2_ones() {
        let (upstream_addr, seen) = spawn_fake_upstream().await;
        let credential = cred("relay@x.com", "real-token-for-upstream");
        let lane: Arc<dyn Lane> = Arc::new(OneShotLane {
            credential,
            oks: AtomicUsize::new(0),
            errs: AtomicUsize::new(0),
        });
        let targets = Targets {
            api2: Target {
                authority: upstream_addr.clone(),
                tls: false,
            },
            agent: Target {
                authority: upstream_addr.clone(),
                tls: false,
            },
        };
        let gw = spawn_passthrough(targets, lane).await;

        let stream = TcpStream::connect(&gw).await.unwrap();
        let io = TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
        tokio::spawn(conn);
        let uri: Uri = format!("http://{gw}/auth/exchange_user_api_key")
            .parse()
            .unwrap();
        let req = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("authorization", "Bearer stale-client-side-token")
            .body(text_body(Bytes::from_static(b"{}")))
            .unwrap();
        let resp = sender.send_request(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::OK,
            "auto 接受循环该识别出这是 HTTP/1.1 而不是拒绝连接"
        );
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..], b"{}");

        let headers = seen.lock().unwrap().clone().expect("假上游该收到一个请求");
        assert_eq!(
            headers.get("authorization").unwrap(),
            "Bearer real-token-for-upstream",
            "/auth/* 也要换身份头，不是只有 agent.v1.* / aiserver.v1.* 才换"
        );
    }

    /// 假的 InferenceService：把请求体记下来，回一段固定的 Connect 流。用来测拦截那条路径。
    async fn spawn_inference_upstream(canned: Vec<u8>) -> (String, Arc<Mutex<Option<Vec<u8>>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));
        let seen_clone = seen.clone();
        let canned = Arc::new(canned);
        tokio::spawn(async move {
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(v) => v,
                    Err(_) => return,
                };
                let seen = seen_clone.clone();
                let canned = canned.clone();
                tokio::spawn(async move {
                    let io = TokioIo::new(stream);
                    let svc = service_fn(move |req: Request<Incoming>| {
                        let seen = seen.clone();
                        let canned = canned.clone();
                        async move {
                            let body = req.into_body().collect().await.unwrap().to_bytes();
                            *seen.lock().unwrap() = Some(body.to_vec());
                            let mut resp =
                                Response::new(text_body(Bytes::copy_from_slice(&canned)));
                            resp.headers_mut().insert(
                                CONTENT_TYPE,
                                HeaderValue::from_static("application/connect+proto"),
                            );
                            Ok::<_, Infallible>(resp)
                        }
                    });
                    let _ = auto::Builder::new(TokioExecutor::new())
                        .serve_connection(io, svc)
                        .await;
                });
            }
        });
        (format!("127.0.0.1:{}", addr.port()), seen)
    }

    fn inference_request_envelope(last_user: &str) -> Vec<u8> {
        use crate::proto::{
            inference_core_message::Content, InferenceCoreMessage, InferenceRequestedModel,
            InferenceStreamRequest,
        };
        use prost::Message;
        let req = InferenceStreamRequest {
            messages: vec![
                InferenceCoreMessage {
                    role: 1,
                    content: Some(Content::Text("earlier".into())),
                    ..Default::default()
                },
                InferenceCoreMessage {
                    role: 2,
                    content: Some(Content::Text("ok".into())),
                    ..Default::default()
                },
                InferenceCoreMessage {
                    role: 1,
                    content: Some(Content::Text(last_user.into())),
                    ..Default::default()
                },
            ],
            requested_model: Some(InferenceRequestedModel {
                model_id: "claude-opus-5".into(),
                ..Default::default()
            }),
            conversation_id: Some("conv-e2e".into()),
            ..Default::default()
        };
        crate::connect::Envelope::message(req.encode_to_vec()).encode()
    }

    fn canned_stream() -> Vec<u8> {
        use crate::connect::Envelope;
        use crate::proto::{
            inference_stream_response::Response as R, InferenceExtendedUsageInfo,
            InferenceResponseInfo, InferenceStreamResponse, InferenceTextStreamPart,
        };
        use prost::Message;
        let frame = |r: R| {
            Envelope::message(InferenceStreamResponse { response: Some(r) }.encode_to_vec())
                .encode()
        };
        let mut out = Vec::new();
        out.extend(frame(R::ResponseInfo(InferenceResponseInfo {
            model: "claude-opus-5-thinking-high".into(),
            ..Default::default()
        })));
        out.extend(frame(R::TextPart(InferenceTextStreamPart {
            text: "[nexus-mark] heard".into(),
            is_final: true,
        })));
        out.extend(frame(R::ExtendedUsage(InferenceExtendedUsageInfo {
            input_tokens: 1234,
            output_tokens: 56,
            cache_read_tokens: 1000,
            cache_write_tokens: 7,
            max_tokens: 200_000,
        })));
        out.extend(
            Envelope {
                flags: Envelope::FLAG_END_STREAM,
                data: b"{}".to_vec(),
            }
            .encode(),
        );
        out
    }

    async fn h1_post(gateway_addr: &str, path: &str, body: Vec<u8>) -> (StatusCode, Vec<u8>) {
        let stream = TcpStream::connect(gateway_addr).await.unwrap();
        let io = TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http1::handshake(io).await.unwrap();
        tokio::spawn(conn);
        let uri: Uri = format!("http://{gateway_addr}{path}").parse().unwrap();
        let req = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("authorization", "Bearer stale")
            .header(CONTENT_TYPE, "application/connect+proto")
            .header(CONTENT_LENGTH, body.len())
            .body(text_body(Bytes::from(body)))
            .unwrap();
        let resp = sender.send_request(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, bytes.to_vec())
    }

    /// 拦截那条路端到端：上游看到的是**改写后**的请求（哨兵在最后一条 user 末尾、别的字段不动、
    /// content-length 对得上），客户端拿到的响应字节与上游发的**逐字节一致**，账本 / 最近记录里
    /// 有这一行（用量来自帧、路由模型来自 response_info、rewritten=true、conversation_id 对得上）。
    #[tokio::test]
    async fn inference_stream_is_rewritten_forwarded_verbatim_and_recorded() {
        use crate::intercept::{MarkerPosition, RewriteRule};
        use crate::proto::{inference_core_message::Content, InferenceStreamRequest};
        use prost::Message;

        let canned = canned_stream();
        let (upstream_addr, seen) = spawn_inference_upstream(canned.clone()).await;
        let lane: Arc<dyn Lane> = Arc::new(OneShotLane {
            credential: cred("relay@x.com", "tok"),
            oks: AtomicUsize::new(0),
            errs: AtomicUsize::new(0),
        });
        let hub = Arc::new(InterceptHub::new(RewriteRule {
            enabled: true,
            position: MarkerPosition::Tail,
            marker: "[nexus-mark]".into(),
        }));
        let ledger = Arc::new(Ledger::new(Arc::new(
            nexus_store::Db::open_in_memory().unwrap(),
        )));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gw = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let targets = Targets {
            api2: Target {
                authority: upstream_addr.clone(),
                tls: false,
            },
            agent: Target {
                authority: upstream_addr,
                tls: false,
            },
        };
        let ctx = Arc::new(
            PassthroughContext::with_targets(lane, "sand".into(), targets)
                .with_intercept(hub.clone(), Some(ledger.clone())),
        );
        tokio::spawn(serve(listener, ctx, std::future::pending()));

        let (status, body) = h1_post(
            &gw,
            INFERENCE_STREAM_PATH,
            inference_request_envelope("say it back"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, canned, "响应帧必须原样到客户端");

        // 上游收到的是改写后的一个信封。
        let upstream_body = seen.lock().unwrap().clone().expect("上游该收到请求");
        assert_eq!(upstream_body[0], 0, "未压缩信封");
        let len = u32::from_be_bytes([
            upstream_body[1],
            upstream_body[2],
            upstream_body[3],
            upstream_body[4],
        ]) as usize;
        assert_eq!(
            5 + len,
            upstream_body.len(),
            "content-length 与信封长度一致"
        );
        let req = InferenceStreamRequest::decode(&upstream_body[5..]).unwrap();
        assert_eq!(req.messages.len(), 3);
        assert_eq!(
            req.messages[0].content,
            Some(Content::Text("earlier".into()))
        );
        assert_eq!(
            req.messages[2].content,
            Some(Content::Text("say it back[nexus-mark]".into()))
        );
        assert_eq!(req.conversation_id.as_deref(), Some("conv-e2e"));

        // 记录：TeeBody 在流读完时结账；给 Drop / 落库一点时间。
        let snap = {
            let mut snap = hub.snapshot();
            for _ in 0..50 {
                if !snap.recent.is_empty() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                snap = hub.snapshot();
            }
            snap
        };
        assert_eq!(snap.calls, 1);
        assert_eq!(snap.rewritten, 1);
        assert_eq!(snap.errors, 0);
        let rec = &snap.recent[0];
        assert!(rec.ok && rec.rewritten && rec.measured);
        assert_eq!(rec.conversation_id.as_deref(), Some("conv-e2e"));
        assert_eq!(rec.model, "claude-opus-5");
        assert_eq!(rec.routed.as_deref(), Some("claude-opus-5-thinking-high"));
        assert_eq!(
            (
                rec.input_tokens,
                rec.output_tokens,
                rec.cache_read_tokens,
                rec.cache_write_tokens
            ),
            (1234, 56, 1000, 7)
        );
        assert_eq!(rec.message_count, 3);
        assert_eq!(rec.account, "relay@x.com");

        let ide = ledger.summary_source(1, 0, SOURCE_IDE_AGENT).unwrap();
        assert_eq!(ide.window.calls, 1);
        assert_eq!(ide.window.input_tokens, 1234);
        assert_eq!(
            ide.recent[0].routed.as_deref(),
            Some("claude-opus-5-thinking-high")
        );
        assert_eq!(
            ledger.summary(1, 0).unwrap().window.calls,
            0,
            "方言口的账不受影响"
        );
    }

    /// 规则关着：字节原样过、照样记账（记用量不依赖改写开关）。
    #[tokio::test]
    async fn inference_stream_with_rewrite_off_is_forwarded_untouched_but_still_recorded() {
        let (upstream_addr, seen) = spawn_inference_upstream(canned_stream()).await;
        let lane: Arc<dyn Lane> = Arc::new(OneShotLane {
            credential: cred("relay@x.com", "tok"),
            oks: AtomicUsize::new(0),
            errs: AtomicUsize::new(0),
        });
        let hub = Arc::new(InterceptHub::new(crate::intercept::RewriteRule::default()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gw = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let targets = Targets {
            api2: Target {
                authority: upstream_addr.clone(),
                tls: false,
            },
            agent: Target {
                authority: upstream_addr,
                tls: false,
            },
        };
        let ctx = Arc::new(
            PassthroughContext::with_targets(lane, "sand".into(), targets)
                .with_intercept(hub.clone(), None),
        );
        tokio::spawn(serve(listener, ctx, std::future::pending()));

        let sent = inference_request_envelope("plain");
        let (status, _) = h1_post(&gw, INFERENCE_STREAM_PATH, sent.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            seen.lock().unwrap().clone().unwrap(),
            sent,
            "没开改写就一个字节都不动"
        );

        let mut snap = hub.snapshot();
        for _ in 0..50 {
            if !snap.recent.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            snap = hub.snapshot();
        }
        assert_eq!(snap.calls, 1);
        assert_eq!(snap.rewritten, 0);
        assert!(!snap.recent[0].rewritten);
        assert_eq!(snap.recent[0].input_tokens, 1234);
    }

    /// Grok Bot 额度开着：只有 `InferenceService/Stream` 换成 grokBotToken + sand 身份，别的路径
    /// 照旧走 Lane 里的号；开关是热的，关掉立刻回到 Lane。
    #[tokio::test]
    async fn grokbot_switch_only_rewrites_the_inference_stream_path() {
        use crate::grokbot::GrokBotStreamAuth;
        use nexus_grokbot::{GrokBotService, StreamCredential};

        let (upstream_addr, seen) = spawn_fake_upstream().await;
        let lane: Arc<dyn Lane> = Arc::new(OneShotLane {
            credential: cred("lane@x.com", "lane-token"),
            oks: AtomicUsize::new(0),
            errs: AtomicUsize::new(0),
        });
        let dir = tempfile::tempdir().unwrap();
        StreamCredential {
            grok_bot_token: "g.r.ok".into(),
            machine_id: "grokmachine".into(),
            renewal_credential: Some("sbi_test".into()),
            expires_at_ms: Some(u64::MAX / 2),
            client_version: "0.44.0".into(),
            namespace: "prod".into(),
            account_email: Some("bot@x.com".into()),
            account_slot: None,
            source: None,
            minted_at_ms: None,
            renewed_at_ms: None,
        }
        .save(dir.path())
        .unwrap();
        let grokbot = Arc::new(GrokBotStreamAuth::new(
            Arc::new(GrokBotService::offline(dir.path())),
            true,
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gw = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let targets = Targets {
            api2: Target {
                authority: upstream_addr.clone(),
                tls: false,
            },
            agent: Target {
                authority: upstream_addr,
                tls: false,
            },
        };
        let ctx = Arc::new(
            PassthroughContext::with_targets(lane, "cli".into(), targets)
                .with_grokbot(grokbot.clone()),
        );
        tokio::spawn(serve(listener, ctx, std::future::pending()));

        let (status, _) = h1_post(&gw, INFERENCE_STREAM_PATH, b"x".to_vec()).await;
        assert_eq!(status, StatusCode::OK);
        let h = seen.lock().unwrap().clone().unwrap();
        assert_eq!(h.get("authorization").unwrap(), "Bearer g.r.ok");
        assert_eq!(h.get("x-cursor-client-type").unwrap(), "sand");
        assert_eq!(h.get("x-cursor-client-version").unwrap(), "0.44.0");
        assert_eq!(h.get("x-sand-box-namespace").unwrap(), "prod");
        assert!(h
            .get("x-cursor-checksum")
            .unwrap()
            .to_str()
            .unwrap()
            .ends_with("grokmachine"));

        // 别的 aiserver 路径：Lane 的号。
        let (status, _) =
            h1_post(&gw, "/aiserver.v1.AiService/AvailableModels", b"x".to_vec()).await;
        assert_eq!(status, StatusCode::OK);
        let h = seen.lock().unwrap().clone().unwrap();
        assert_eq!(h.get("authorization").unwrap(), "Bearer lane-token");
        assert_eq!(h.get("x-cursor-client-type").unwrap(), "cli");

        // 关掉开关：Stream 也回到 Lane。
        grokbot.set_enabled(false);
        let (status, _) = h1_post(&gw, INFERENCE_STREAM_PATH, b"x".to_vec()).await;
        assert_eq!(status, StatusCode::OK);
        let h = seen.lock().unwrap().clone().unwrap();
        assert_eq!(h.get("authorization").unwrap(), "Bearer lane-token");
    }

    /// 开着却没有凭证：Stream 回 502 + 能看懂的原因，不碰 Lane、不 panic。
    #[tokio::test]
    async fn grokbot_without_credential_fails_the_stream_with_a_readable_error() {
        use crate::grokbot::GrokBotStreamAuth;
        use nexus_grokbot::GrokBotService;

        let (upstream_addr, seen) = spawn_fake_upstream().await;
        let lane: Arc<dyn Lane> = Arc::new(OneShotLane {
            credential: cred("lane@x.com", "lane-token"),
            oks: AtomicUsize::new(0),
            errs: AtomicUsize::new(0),
        });
        let dir = tempfile::tempdir().unwrap();
        let grokbot = Arc::new(GrokBotStreamAuth::new(
            Arc::new(GrokBotService::offline(dir.path())),
            true,
        ));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gw = format!("127.0.0.1:{}", listener.local_addr().unwrap().port());
        let targets = Targets {
            api2: Target {
                authority: upstream_addr.clone(),
                tls: false,
            },
            agent: Target {
                authority: upstream_addr,
                tls: false,
            },
        };
        let ctx = Arc::new(
            PassthroughContext::with_targets(lane, "cli".into(), targets).with_grokbot(grokbot),
        );
        tokio::spawn(serve(listener, ctx, std::future::pending()));

        let (status, body) = h1_post(&gw, INFERENCE_STREAM_PATH, b"x".to_vec()).await;
        assert_eq!(status, StatusCode::BAD_GATEWAY);
        assert!(String::from_utf8_lossy(&body).contains("Grok Bot"));
        assert!(seen.lock().unwrap().is_none(), "没凭证就不该打到上游");
    }

    #[tokio::test]
    async fn unknown_path_is_rejected_before_touching_any_account() {
        let (upstream_addr, _seen) = spawn_fake_upstream().await;
        let lane: Arc<dyn Lane> = Arc::new(OneShotLane {
            credential: cred("a@x.com", "t"),
            oks: AtomicUsize::new(0),
            errs: AtomicUsize::new(0),
        });
        let targets = Targets {
            api2: Target {
                authority: upstream_addr.clone(),
                tls: false,
            },
            agent: Target {
                authority: upstream_addr,
                tls: false,
            },
        };
        let gw = spawn_passthrough(targets, lane).await;
        let (status, _) = h2c_get_echo(&gw, "/v1/chat/completions", b"").await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "那是翻译模式的路径，透传认不出"
        );
    }

    #[tokio::test]
    async fn bidi_frames_flow_both_ways_before_either_side_finishes_writing() {
        let (upstream_addr, _seen) = spawn_fake_upstream().await;
        let credential = cred("relay@x.com", "tok");
        let lane: Arc<dyn Lane> = Arc::new(OneShotLane {
            credential,
            oks: AtomicUsize::new(0),
            errs: AtomicUsize::new(0),
        });
        let targets = Targets {
            api2: Target {
                authority: upstream_addr.clone(),
                tls: false,
            },
            agent: Target {
                authority: upstream_addr,
                tls: false,
            },
        };
        let gw = spawn_passthrough(targets, lane).await;

        let stream = TcpStream::connect(&gw).await.unwrap();
        let io = TokioIo::new(stream);
        let (mut sender, conn) = hyper::client::conn::http2::handshake(TokioExecutor::new(), io)
            .await
            .unwrap();
        tokio::spawn(conn);

        let (body_tx, body_rx) = tokio::sync::mpsc::channel::<Bytes>(8);
        let req_body = {
            use futures_util::stream::poll_fn;
            use http_body_util::StreamBody;
            let mut rx = body_rx;
            StreamBody::new(poll_fn(move |cx| {
                rx.poll_recv(cx).map(|opt| {
                    opt.map(|b| {
                        Ok::<_, Box<dyn std::error::Error + Send + Sync>>(hyper::body::Frame::data(
                            b,
                        ))
                    })
                })
            }))
            .boxed()
        };
        let uri: Uri = format!("http://{gw}/agent.v1.AgentService/Run")
            .parse()
            .unwrap();
        let req = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header("authorization", "Bearer stale")
            .body(req_body)
            .unwrap();
        let resp = sender.send_request(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let mut resp_body = resp.into_body();

        for i in 0..3u8 {
            body_tx.send(Bytes::from(vec![i; 3])).await.unwrap();
            // 逐帧收：证明响应帧在整条请求体写完之前就已经能读到。
            loop {
                let frame = resp_body.frame().await.unwrap().unwrap();
                if let Some(data) = frame.data_ref() {
                    assert_eq!(data.as_ref(), &[i, i, i]);
                    break;
                }
            }
        }
        drop(body_tx);
    }
}
