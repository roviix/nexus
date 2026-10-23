//! 对真 Amazon Q 跑一组 Kiro 协议用例：system、工具调用、思考、图片、上下文与用量。
//!
//! 单元测试只能证明「我们发出去的 JSON 是我们以为的样子」，证明不了上游认不认。这个
//! example 补的就是那一半：拿**本机 Kiro 里正登着的号**
//! （`~/.aws/sso/cache/kiro-auth-token.json`）或 `KIRO_ACCESS_TOKEN` 逐条打真上游，
//! 每条用例都有判据，结果打成 PASS / FAIL 表。Kiro 升级、换模型之后重跑一遍，就知道
//! 协议层还站不站得住。
//!
//! ```bash
//! NEXUS_GATEWAY_PROBE=1 cargo run -p nexus-gateway --example kiro_matrix -- status
//! NEXUS_GATEWAY_PROBE=1 cargo run -p nexus-gateway --example kiro_matrix -- usage
//! NEXUS_GATEWAY_PROBE=1 cargo run -p nexus-gateway --example kiro_matrix -- profiles
//! NEXUS_GATEWAY_PROBE=1 cargo run -p nexus-gateway --example kiro_matrix -- models [过滤]
//! NEXUS_GATEWAY_PROBE=1 KIRO_LIVE_MODELS=kiro-claude-sonnet-4.6 \
//!   cargo run -p nexus-gateway --example kiro_matrix -- matrix [用例过滤…]
//! ```
//!
//! - `status`：账号、登录方式、profileArn、机器码、token 有效期。社交登录的 access token
//!   读不出身份时，首次载入会调用一次 `getUsageLimits` 补 userId / 邮箱；不是严格离线命令。
//! - `usage`：`getUsageLimits`——套餐名、基础额度、试用、奖励、重置日。模型清单和额度
//!   判断该照着它改，而不是凭记忆。
//! - `profiles`：`ListAvailableProfiles`，看这个号有没有企业 profile。
//! - `models`：把 `KIRO_MODELS` 里每个**上游名**用一条最短请求打一遍，告诉你哪些名字
//!   上游真认。表里的前瞻条目对不对，只有这样才知道。
//! - `matrix`：走网关真正的 `KiroUpstream::stream`，逐模型逐用例判 PASS / FAIL。
//!
//! 和 Cursor 的 `live_matrix` 一样**默认不碰 refresh token**：Kiro 的社交刷新端点可能轮换它，
//! 从外面刷一次有把 IDE 登出的风险。所以 token 剩余不足 10 分钟就直接退出，让你先在 Kiro
//! 里随便用一下让它自己刷新。确实要让探针去刷，设 `KIRO_PROBE_ALLOW_REFRESH=1`。
//!
//! 每条用例都很短（几百 token），但**会花这个号的额度**。

use base64::Engine as _;
use nexus_core::{KiroAccountId, Secret};
use nexus_gateway::upstream::Upstream;
use nexus_gateway::{
    ChatRequest, Completion, Credential, Delta, DeviceIdentity, ImageInput, KiroUpstream, Message,
    Role, Sampling, ToolCall, ToolChoice, ToolDef, ToolResult, UpstreamError,
};
use nexus_kiro::oauth::{jwt_expiry, TokenClient, TokenSet};
use nexus_kiro::{protocol, KiroService, KIRO_MODELS};
use nexus_store::{Db, SqliteSecrets};
use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

/// token 剩这么久以内就不跑，免得网关自己去刷、把 IDE 的登录态换掉。
const MIN_TOKEN_LEFT_SECS: i64 = 10 * 60;

// ---------- 账号 ----------

struct Probe {
    svc: Arc<KiroService>,
    upstream: KiroUpstream,
    id: KiroAccountId,
    label: String,
    email: Option<String>,
    auth_method: Option<String>,
    profile_arn: Option<String>,
    token_type: Option<String>,
    machine_id: String,
    expires_at: Option<OffsetDateTime>,
}

fn home() -> String {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .expect("找不到 HOME / USERPROFILE")
}

fn token_file() -> String {
    std::env::var("KIRO_TOKEN_FILE")
        .unwrap_or_else(|_| format!("{}/.aws/sso/cache/kiro-auth-token.json", home()))
}

/// 读本机登录态里的到期时间。社交登录的 access token 不是 JWT，只能靠文件里的 `expiresAt`。
fn expiry_from_file(raw: &str) -> Option<OffsetDateTime> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let s = v
        .get("expiresAt")
        .or_else(|| v.get("expires_at"))
        .and_then(|x| x.as_str())?;
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

impl Probe {
    async fn load() -> Self {
        let db = Arc::new(Db::open_in_memory().expect("内存库"));
        let secrets = Arc::new(SqliteSecrets::new(db.clone()));
        let svc = Arc::new(KiroService::new(db, secrets));

        let upserted = match std::env::var("KIRO_ACCESS_TOKEN") {
            Ok(t) if !t.trim().is_empty() => {
                let access = t.trim().to_string();
                let expires = jwt_expiry(&access);
                guard_expiry(expires);
                let tokens = TokenSet {
                    expires_at: expires,
                    access_token: Secret::new(access),
                    // 探针不给 refresh：没有它，网关这条路就绝不会去刷。
                    refresh_token: None,
                    client_id: None,
                    client_secret: None,
                };
                let method = std::env::var("KIRO_AUTH_METHOD").ok();
                let arn = std::env::var("KIRO_PROFILE_ARN").ok();
                svc.add_tokens(tokens, Some("探针"), method.as_deref(), arn.as_deref())
                    .await
                    .expect("KIRO_ACCESS_TOKEN 入库")
            }
            _ => {
                let path = token_file();
                let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                    eprintln!("读不到 {path}：{e}");
                    eprintln!("先在 Kiro IDE 登录一次，或者设 KIRO_ACCESS_TOKEN。");
                    std::process::exit(3);
                });
                // 先判有效期再导：导入本身可能触发换 token。
                guard_expiry(expiry_from_file(&raw));
                svc.import_kiro_cli(std::path::Path::new(&path))
                    .await
                    .unwrap_or_else(|e| {
                        eprintln!("导入本机 Kiro 登录态失败：{}", e.message);
                        if let Some(hint) = &e.hint {
                            eprintln!("提示：{hint}");
                        }
                        std::process::exit(3);
                    })
            }
        };

        let account = upserted.account;
        let label = account.label();
        let outbound = svc.outbound_for(&label);
        Self {
            upstream: KiroUpstream::new(svc.clone()),
            svc,
            id: account.id,
            label,
            email: account.email,
            auth_method: account.auth_method,
            profile_arn: outbound.profile_arn,
            token_type: outbound.token_type,
            machine_id: outbound.machine_id,
            expires_at: account
                .access_expires_at
                .as_deref()
                .and_then(|s| OffsetDateTime::parse(s, &Rfc3339).ok()),
        }
    }

    async fn token(&self) -> String {
        self.svc
            .access_token(&self.id)
            .await
            .unwrap_or_else(|e| {
                eprintln!("取不到 access token：{}", e.message);
                std::process::exit(3);
            })
            .expose()
            .to_string()
    }

    async fn credential(&self) -> Credential {
        let token = self.token().await;
        Credential {
            label: self.label.clone(),
            identity: DeviceIdentity::derived(&token),
            access_token: token,
        }
    }

    async fn chat(&self, req: &ChatRequest) -> (Vec<Delta>, Result<Completion, UpstreamError>) {
        let cred = self.credential().await;
        let mut deltas = Vec::new();
        let mut sink = |d: Delta| deltas.push(d);
        let r = self.upstream.stream(&cred, req, &mut sink).await;
        (deltas, r)
    }
}

fn guard_expiry(expires: Option<OffsetDateTime>) {
    if std::env::var("KIRO_PROBE_ALLOW_REFRESH").is_ok() {
        return;
    }
    let Some(exp) = expires else {
        eprintln!("读不出 access token 的有效期，不敢往下跑（怕触发刷新把 Kiro 登出）。");
        eprintln!("确认可以刷新的话设 KIRO_PROBE_ALLOW_REFRESH=1。");
        std::process::exit(3);
    };
    let left = (exp - OffsetDateTime::now_utc()).whole_seconds();
    if left <= MIN_TOKEN_LEFT_SECS {
        eprintln!(
            "access token 只剩 {left} 秒（阈值 {MIN_TOKEN_LEFT_SECS}）。网关会去刷新，\
             而 Kiro 的社交刷新端点可能轮换 refresh token，有把 IDE 登出的风险。"
        );
        eprintln!("在 Kiro 里随便用一下让它自己刷新，再跑；或者设 KIRO_PROBE_ALLOW_REFRESH=1。");
        std::process::exit(3);
    }
}

fn mask(email: &str) -> String {
    match email.split_once('@') {
        Some((user, domain)) => {
            let head: String = user.chars().take(2).collect();
            format!("{head}***@{domain}")
        }
        None => "***".into(),
    }
}

fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

// ---------- status / usage / profiles ----------

fn status(p: &Probe) {
    let left = p
        .expires_at
        .map(|e| {
            let secs = (e - OffsetDateTime::now_utc()).whole_seconds();
            format!("{} 分钟", secs / 60)
        })
        .unwrap_or_else(|| "未知".into());
    println!("账号标签   {}", p.label);
    println!(
        "邮箱       {}",
        p.email
            .as_deref()
            .map(mask)
            .unwrap_or_else(|| "(空)".into())
    );
    println!("登录方式   {}", p.auth_method.as_deref().unwrap_or("(空)"));
    println!(
        "profileArn {}{}",
        p.profile_arn.as_deref().unwrap_or("(无)"),
        match p.profile_arn.as_deref() {
            Some(a) if protocol::is_placeholder_profile_arn(a) => "  ← Builder ID 占位符",
            Some(protocol::SOCIAL_PROFILE_ARN) => "  ← 社交登录共用",
            _ => "",
        }
    );
    println!("tokentype  {}", p.token_type.as_deref().unwrap_or("(不带)"));
    println!("机器码     {}…", head(&p.machine_id, 16));
    println!("token 剩余 {left}");
}

async fn usage(p: &Probe) {
    let client = TokenClient::new(nexus_gateway::http_client());
    let token = p.token().await;
    match client
        .usage_limits(&token, p.token_type.as_deref(), p.profile_arn.as_deref())
        .await
    {
        Ok(u) => {
            println!(
                "套餐       {}",
                u.subscription_title.as_deref().unwrap_or("(上游没给)")
            );
            println!(
                "邮箱       {}",
                u.email
                    .as_deref()
                    .map(mask)
                    .unwrap_or_else(|| "(空)".into())
            );
            println!(
                "userId     {}",
                u.user_id
                    .as_deref()
                    .unwrap_or("(空) ← 没有它，账号只能按共用 ARN 记")
            );
            println!("基础额度   {} / {}", u.base_used, u.base_limit);
            println!("合计       {} / {}  （{:.2}%）", u.used, u.limit, u.percent);
            if let (Some(tu), Some(tl)) = (u.trial_used, u.trial_limit) {
                println!("试用       {tu} / {tl}  到期 {:?}", u.trial_expires_at);
            }
            if let (Some(bu), Some(bl)) = (u.bonus_used, u.bonus_limit) {
                println!("奖励       {bu} / {bl}");
            }
            println!("重置       {}", u.reset_at.as_deref().unwrap_or("(没给)"));
            println!("超额       {:?}", u.overage_enabled);
        }
        Err(e) => {
            eprintln!("getUsageLimits 失败：{}", e.message);
            std::process::exit(1);
        }
    }
}

async fn profiles(p: &Probe) {
    let client = TokenClient::new(nexus_gateway::http_client());
    let token = p.token().await;
    match client.list_profiles(&token, p.token_type.as_deref()).await {
        Ok(Some(arn)) => println!("企业 profile：{arn}"),
        Ok(None) => println!("上游确认这个号没有企业 profile（纯 Builder ID / 社交登录）。"),
        Err(e) => {
            eprintln!("ListAvailableProfiles 失败：{}", e.message);
            std::process::exit(1);
        }
    }
}

// ---------- models：表里的名字上游认不认 ----------

async fn models(p: &Probe, filter: Option<&str>) {
    // 对外名多、上游名少（`-thinking` 只影响思考开关）。按上游名去重，省额度。
    let mut wanted: BTreeSet<(String, String)> = BTreeSet::new();
    for (ext, up) in KIRO_MODELS {
        if filter.is_some_and(|f| !ext.contains(f) && !up.contains(f)) {
            continue;
        }
        wanted.insert(((*up).to_string(), (*ext).to_string()));
    }
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    println!("逐个上游模型名打一条最短请求（{} 个）…\n", wanted.len());
    for (up, ext) in wanted {
        if !seen.insert(up.clone()) {
            continue;
        }
        let req = ChatRequest {
            // 用对外名进去，顺带验 upstream_model 的映射。
            model: ext.clone(),
            messages: vec![Message::text(Role::User, "Reply with exactly: OK")],
            sampling: Sampling {
                max_output_tokens: Some(16),
                ..Default::default()
            },
            ..Default::default()
        };
        let (_, r) = p.chat(&req).await;
        match r {
            Ok(c) => {
                println!(
                    "[ OK ] {up:<22} ← {ext:<34} 出字 {:?} routed={:?}",
                    head(c.text.trim(), 24),
                    c.routed_model
                );
                ok.push(up);
            }
            Err(e) => {
                println!(
                    "[FAIL] {up:<22} ← {ext:<34} {} {}",
                    e.status,
                    head(&e.message, 150)
                );
                bad.push(up);
            }
        }
    }
    println!("\n上游认的 {} 个，拒的 {} 个。", ok.len(), bad.len());
    if !bad.is_empty() {
        println!("拒掉的：{bad:?}");
        println!("这些该从 KIRO_MODELS 里摘掉，或者改成上游真实的名字。");
    }
}

// ---------- import：复现「库里已经有一行记在共用 ARN 上」再导入 ----------

/// 你的库现在就是这个样子：一行记在社交登录共用的 profileArn 上，邮箱、套餐、用量全空。
/// 这个子命令在一个干净的内存库里造出同样的状态，再走一次真实导入，看最后剩几行、怎么记的。
///
/// 旧行**故意配一把对不上的 refresh token**：账号管理器切号时会轮换它，所以切走再切回来，
/// 库里那把就不是文件里这把了。这正是会漏的那个口子——
/// - `adopt_legacy` 第一条（refresh 一样就认回）对不上；
/// - 第二条（共用 ARN 上那行已失效就复活）也不适用，因为它的 access token 还有效、还是 Active；
/// - 于是 `upsert` 另起一行，同一个号在号池里占两格，旧那格还带着过期凭证。
///
/// 期望：**一行**，记在 userId 上，带邮箱和套餐。
async fn import_scenario() {
    let path = token_file();
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("读不到 {path}：{e}");
        std::process::exit(3);
    });
    guard_expiry(expiry_from_file(&raw));
    let v: serde_json::Value = serde_json::from_str(&raw).expect("kiro-auth-token.json");
    let pick = |k: &str| {
        v.get(k)
            .and_then(|x| x.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let access = pick("accessToken").expect("accessToken");
    let refresh = pick("refreshToken");
    let shared = pick("profileArn").unwrap_or_else(|| protocol::SOCIAL_PROFILE_ARN.to_string());

    let db = Arc::new(Db::open_in_memory().expect("内存库"));
    let secrets = Arc::new(SqliteSecrets::new(db.clone()));
    let svc = KiroService::new(db, secrets);

    // 1. 造出旧行：身份就是共用 ARN，和旧版导入的结果一样。
    let legacy = nexus_kiro::Identity {
        subject: shared.clone(),
        email: None,
        auth_method: Some("social".into()),
        profile_arn: Some(shared.clone()),
    };
    let _ = &refresh;
    let legacy_tokens = TokenSet {
        // access token 是真的（还没过期），所以这一行是 Active、问得到自己的身份；
        // refresh 是对不上的那把，模拟账号管理器切号时轮换过。
        access_token: Secret::new(access),
        refresh_token: Some(Secret::new(
            "rt_rotated_away_by_the_account_manager".to_string(),
        )),
        client_id: None,
        client_secret: None,
        expires_at: expiry_from_file(&raw),
    };
    let up = svc
        .repo
        .upsert(&legacy, &legacy_tokens, Some("旧版按共用 ARN 记的行"))
        .expect("造旧行");
    svc.repo
        .store_tokens(&up.account.id, &legacy_tokens)
        .expect("写凭证");
    println!(
        "造好旧行：account_ref={} label={}",
        up.account.account_ref,
        up.account.label()
    );
    println!("导入前 {} 行\n", svc.list().expect("list").len());

    // 2. 走真实导入。
    match svc.import_text(&raw, Some("来自本机 Kiro")).await {
        Ok(up) => println!("导入成功：{}\n", up.account.label()),
        Err(e) => {
            println!("导入失败：{}", e.message);
            if let Some(h) = &e.hint {
                println!("提示：{h}");
            }
            println!();
        }
    }

    // 3. 看结果。
    let rows = svc.list().expect("list");
    println!("导入后 {} 行：", rows.len());
    for a in &rows {
        println!(
            "  label={} account_ref={} 邮箱={} 套餐={} 用量={} 状态={:?}",
            a.label(),
            head(&a.account_ref, 48),
            a.email
                .as_deref()
                .map(mask)
                .unwrap_or_else(|| "(空)".into()),
            a.plan_type.as_deref().unwrap_or("(空)"),
            if a.usage.is_some() { "有" } else { "无" },
            a.status
        );
    }
    println!();
    if rows.len() == 1 && nexus_kiro::oauth::names_one_user(&rows[0].account_ref) {
        println!("✓ 一行，记在真实身份上。同一个号不会在号池里占两格。");
    } else {
        println!("✗ 期望一行且记在 userId 上，实际 {} 行。", rows.len());
        std::process::exit(1);
    }
}

// ---------- raw：绕过流解析，看上游到底回了什么字节 ----------

async fn raw(p: &Probe, model: &str) {
    let req = chat_req(
        model,
        vec![Message::text(Role::User, "Reply with exactly: PONG")],
        vec![],
    );
    let upstream = nexus_kiro::upstream_model(&req.model);
    let (body, _names) = nexus_gateway::kiro::build_body(&req, &upstream, p.profile_arn.as_deref());
    let payload = serde_json::to_vec(&body).expect("序列化");
    println!("对外名 {} → 上游名 {upstream}", req.model);
    println!("请求体 {} 字节", payload.len());
    println!(
        "{}\n",
        head(&serde_json::to_string_pretty(&body).unwrap(), 1600)
    );

    let token = p.token().await;
    let invocation = uuid::Uuid::new_v4().to_string();
    let client = nexus_gateway::http_client();
    let mut rb = client
        .post(protocol::Q_ENDPOINT)
        .timeout(std::time::Duration::from_secs(120))
        .body(payload);
    for (k, v) in protocol::chat_headers(
        &token,
        &p.machine_id,
        p.profile_arn.as_deref(),
        &invocation,
        p.token_type.as_deref(),
    ) {
        if k == "authorization" {
            println!("头 {k}: Bearer <{}  字节>", token.len());
        } else {
            println!("头 {k}: {v}");
        }
        rb = rb.header(k, v);
    }
    println!();

    let res = match rb.send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("发不出去：{e}");
            std::process::exit(1);
        }
    };
    println!("状态 {}", res.status());
    for (k, v) in res.headers() {
        println!("响应头 {k}: {}", v.to_str().unwrap_or("<非 ascii>"));
    }
    let bytes = match res.bytes().await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            eprintln!("读不完响应体：{e}");
            std::process::exit(1);
        }
    };
    println!("\n响应体 {} 字节", bytes.len());
    if bytes.is_empty() {
        println!("空的。上游给了 200 却没有任何帧 —— 这就是「不出字」的根。");
        return;
    }
    let dump_len = bytes.len().min(512);
    print!("前 {dump_len} 字节 hex：");
    for (i, b) in bytes[..dump_len].iter().enumerate() {
        if i % 32 == 0 {
            print!("\n  ");
        }
        print!("{b:02x} ");
    }
    println!();
    println!(
        "\n按 ascii 看：{:?}",
        String::from_utf8_lossy(&bytes[..dump_len])
    );
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

fn png_image() -> ImageInput {
    ImageInput {
        data: split_png(BLUE, YELLOW),
        mime_type: "image/png".into(),
    }
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

/// 嵌套 + 组合子的 schema：`kiro_schema` 要把它收成上游收得下的形状。
fn filter_tool() -> ToolDef {
    ToolDef {
        name: "search_files".into(),
        description: "Search files with a nested filter object.".into(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" },
                "filter": {
                    "type": "object",
                    "properties": {
                        "ext": { "type": "string", "enum": ["rs", "ts", "md"] },
                        "size": {
                            "type": "object",
                            "properties": { "min": { "type": "integer" }, "max": { "type": "integer" } }
                        }
                    },
                    "required": ["ext"]
                }
            },
            "required": ["query", "filter"]
        }),
        ..ToolDef::default()
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
        conversation_id: Some(uuid::Uuid::new_v4().to_string()),
        ..Default::default()
    }
}

fn summarize(c: &Completion) -> String {
    format!(
        "finish={:?} in={} out={} cache_r={} cache_w={} measured={} 思考 {} 字符 签名={} 工具 {} ttft={:?}ms",
        c.finish_reason,
        c.usage.input_tokens,
        c.usage.output_tokens,
        c.usage.cache_read_tokens,
        c.usage.cache_write_tokens,
        c.usage_measured,
        c.thinking.chars().count(),
        c.thinking_signature.is_some(),
        c.tool_calls.len(),
        c.ttft_ms
    )
}

// ---------- 用例 ----------

#[derive(PartialEq)]
enum Outcome {
    Pass,
    Fail,
    /// 上游确认不支持这件事。不是我们坏了，别让它一直红着——但要留在表上。
    Limit,
}

impl Outcome {
    fn tag(&self) -> &'static str {
        match self {
            Outcome::Pass => "PASS",
            Outcome::Fail => "FAIL",
            Outcome::Limit => "LIMIT",
        }
    }
}

struct Verdict {
    outcome: Outcome,
    note: String,
}

impl Verdict {
    fn new(pass: bool, note: impl Into<String>) -> Self {
        Self {
            outcome: if pass { Outcome::Pass } else { Outcome::Fail },
            note: note.into(),
        }
    }

    fn limit(note: impl Into<String>) -> Self {
        Self {
            outcome: Outcome::Limit,
            note: note.into(),
        }
    }
}

type CaseFut<'a> = Pin<Box<dyn Future<Output = Verdict> + 'a>>;

struct Case {
    name: &'static str,
    run: for<'a> fn(&'a Probe, &'a str) -> CaseFut<'a>,
}

fn failed(e: &UpstreamError) -> Verdict {
    Verdict::new(
        false,
        format!("{:?} {} {}", e.kind, e.status, head(&e.message, 200)),
    )
}

fn case_text<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let req = chat_req(
            model,
            vec![Message::text(Role::User, "Reply with exactly: PONG")],
            vec![],
        );
        match p.chat(&req).await.1 {
            Err(e) => failed(&e),
            Ok(c) => Verdict::new(
                c.text.contains("PONG"),
                format!("{} | 正文 {:?}", summarize(&c), head(c.text.trim(), 60)),
            ),
        }
    })
}

/// system 不折进用户首句，而是单独成一轮 user + assistant。上游到底读没读到它。
fn case_system<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let req = chat_req(
            model,
            vec![
                Message::text(
                    Role::System,
                    "Secret codeword: ZEBRA-17. Whenever asked for the codeword, reply with exactly the codeword and nothing else.",
                ),
                Message::text(Role::User, "What is the codeword?"),
            ],
            vec![],
        );
        match p.chat(&req).await.1 {
            Err(e) => failed(&e),
            Ok(c) => Verdict::new(
                c.text.contains("ZEBRA-17"),
                format!("{} | 正文 {:?}", summarize(&c), head(c.text.trim(), 60)),
            ),
        }
    })
}

fn case_tool_single<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let req = chat_req(
            model,
            vec![Message::text(
                Role::User,
                "What's the weather in Paris? Use the tool.",
            )],
            vec![weather_tool()],
        );
        match p.chat(&req).await.1 {
            Err(e) => failed(&e),
            Ok(c) => {
                let hit = c
                    .tool_calls
                    .iter()
                    .find(|t| t.name == "get_weather")
                    .filter(|t| t.arguments.to_lowercase().contains("paris"));
                Verdict::new(
                    hit.is_some(),
                    format!(
                        "{} | 调用 {:?}",
                        summarize(&c),
                        c.tool_calls
                            .iter()
                            .map(|t| format!("{}{}", t.name, t.arguments))
                            .collect::<Vec<_>>()
                    ),
                )
            }
        }
    })
}

fn case_tool_parallel<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let req = chat_req(
            model,
            vec![Message::text(
                Role::User,
                "Get the weather for both Paris and Tokyo. Call the tool once per city, both in this turn.",
            )],
            vec![weather_tool()],
        );
        match p.chat(&req).await.1 {
            Err(e) => failed(&e),
            Ok(c) => {
                let n = c
                    .tool_calls
                    .iter()
                    .filter(|t| t.name == "get_weather")
                    .count();
                let ids: BTreeSet<&str> = c.tool_calls.iter().map(|t| t.id.as_str()).collect();
                Verdict::new(
                    n >= 2 && ids.len() == c.tool_calls.len(),
                    format!("{} | {n} 次调用，id 去重后 {} 个", summarize(&c), ids.len()),
                )
            }
        }
    })
}

/// 工具结果能不能接回去：assistant 的调用 + tool 的结果 → 上游要用上那个值。
fn case_tool_loop<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let mut assistant = Message::text(Role::Assistant, "");
        assistant.tool_calls = vec![ToolCall {
            id: "tooluse_probe_1".into(),
            name: "get_weather".into(),
            arguments: r#"{"city":"Paris"}"#.into(),
        }];
        let mut tool = Message::text(Role::Tool, "");
        tool.tool_results = vec![ToolResult {
            tool_call_id: "tooluse_probe_1".into(),
            tool_name: "get_weather".into(),
            text: "-7C and snowing".into(),
            is_error: false,
            images: Vec::new(),
        }];
        let req = chat_req(
            model,
            vec![
                Message::text(Role::User, "What's the weather in Paris? Use the tool."),
                assistant,
                tool,
                Message::text(
                    Role::User,
                    "Now state the temperature you got, including the unit.",
                ),
            ],
            vec![weather_tool()],
        );
        match p.chat(&req).await.1 {
            Err(e) => failed(&e),
            Ok(c) => {
                let t = c.text.to_lowercase();
                Verdict::new(
                    t.contains("-7") && t.contains('c'),
                    format!("{} | 正文 {:?}", summarize(&c), head(c.text.trim(), 80)),
                )
            }
        }
    })
}

fn case_tool_required<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let mut req = chat_req(
            model,
            vec![Message::text(Role::User, "Tell me about the weather.")],
            vec![weather_tool()],
        );
        req.tool_choice = ToolChoice::Required;
        match p.chat(&req).await.1 {
            Err(e) => failed(&e),
            Ok(c) => Verdict::new(
                !c.tool_calls.is_empty(),
                format!("{} | 正文 {:?}", summarize(&c), head(c.text.trim(), 50)),
            ),
        }
    })
}

fn case_tool_none<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let mut req = chat_req(
            model,
            vec![Message::text(
                Role::User,
                "What's the weather in Paris? Answer in one short sentence.",
            )],
            vec![weather_tool()],
        );
        req.tool_choice = ToolChoice::None;
        match p.chat(&req).await.1 {
            Err(e) => failed(&e),
            Ok(c) => Verdict::new(
                c.tool_calls.is_empty() && !c.text.trim().is_empty(),
                format!("{} | 正文 {:?}", summarize(&c), head(c.text.trim(), 60)),
            ),
        }
    })
}

/// 嵌套 schema 过了 `kiro_schema` 的规整，上游还收不收、模型还填不填得对。
fn case_tool_schema<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let req = chat_req(
            model,
            vec![Message::text(
                Role::User,
                "Search for \"parser\" in Rust files only. Use the tool.",
            )],
            vec![filter_tool()],
        );
        match p.chat(&req).await.1 {
            Err(e) => failed(&e),
            Ok(c) => {
                let good = c.tool_calls.iter().any(|t| {
                    t.name == "search_files"
                        && serde_json::from_str::<serde_json::Value>(&t.arguments)
                            .ok()
                            .and_then(|v| {
                                v.pointer("/filter/ext")
                                    .and_then(|x| x.as_str())
                                    .map(|s| s == "rs")
                            })
                            .unwrap_or(false)
                });
                Verdict::new(
                    good,
                    format!(
                        "{} | 调用 {:?}",
                        summarize(&c),
                        c.tool_calls
                            .iter()
                            .map(|t| t.arguments.clone())
                            .collect::<Vec<_>>()
                    ),
                )
            }
        }
    })
}

fn case_image_user<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let mut user = Message::text(Role::User, COLOR_QUESTION);
        user.images = vec![png_image()];
        let req = chat_req(model, vec![user], vec![]);
        match p.chat(&req).await.1 {
            Err(e) => failed(&e),
            Ok(c) => Verdict::new(
                says_blue_yellow(&c.text),
                format!("{} | 正文 {:?}", summarize(&c), head(c.text.trim(), 60)),
            ),
        }
    })
}

/// 工具结果里的图。Kiro 只让 user 轮带图，所以它要被推到下一条 user。
fn case_image_tool<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let mut assistant = Message::text(Role::Assistant, "");
        assistant.tool_calls = vec![ToolCall {
            id: "tooluse_probe_shot".into(),
            name: "take_screenshot".into(),
            arguments: "{}".into(),
        }];
        let mut tool = Message::text(Role::Tool, "");
        tool.tool_results = vec![ToolResult {
            tool_call_id: "tooluse_probe_shot".into(),
            tool_name: "take_screenshot".into(),
            text: String::new(),
            is_error: false,
            images: vec![png_image()],
        }];
        tool.images = vec![png_image()];
        let req = chat_req(
            model,
            vec![
                Message::text(Role::User, "Take a screenshot, then describe it."),
                assistant,
                tool,
                Message::text(Role::User, COLOR_QUESTION),
            ],
            vec![shot_tool()],
        );
        match p.chat(&req).await.1 {
            Err(e) => failed(&e),
            Ok(c) => Verdict::new(
                says_blue_yellow(&c.text),
                format!("{} | 正文 {:?}", summarize(&c), head(c.text.trim(), 60)),
            ),
        }
    })
}

/// 思考块要出字，而且要带上游真签名——没签名 Claude Code 不收。
fn case_thinking<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let thinking_model = if model.contains("thinking") {
            model.to_string()
        } else {
            format!("{model}-thinking")
        };
        let req = chat_req(
            &thinking_model,
            vec![Message::text(
                Role::User,
                "A rope ladder hangs over a ship's side with 10 rungs, 30cm apart, 3 rungs above water. The tide rises 90cm. How many rungs are above water now? Think it through, then answer with a number.",
            )],
            vec![],
        );
        let (deltas, r) = p.chat(&req).await;
        match r {
            Err(e) => failed(&e),
            Ok(c) => {
                let streamed_thinking = deltas.iter().any(|d| matches!(d, Delta::Thinking(_)));
                let streamed_sig = deltas
                    .iter()
                    .any(|d| matches!(d, Delta::ThinkingSignature(_)));
                Verdict::new(
                    !c.thinking.trim().is_empty(),
                    format!(
                        "{} | 流内思考={streamed_thinking} 流内签名={streamed_sig} 正文 {:?}",
                        summarize(&c),
                        head(c.text.trim(), 40)
                    ),
                )
            }
        }
    })
}

/// 上下文占用：上游只给百分比，我们乘 `context_window` 反推。反推值得对得上量级。
fn case_context<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let filler = "The quick brown fox jumps over the lazy dog. ".repeat(400);
        let req = chat_req(
            model,
            vec![
                Message::text(Role::System, &filler),
                Message::text(Role::User, "Reply with exactly: OK"),
            ],
            vec![],
        );
        match p.chat(&req).await.1 {
            Err(e) => failed(&e),
            Ok(c) => {
                let window = protocol::context_window(model);
                // 填充约 3.6k token。反推值要在同一个量级，否则 context_window 配错了。
                let sane = c.usage.input_tokens > 500 && c.usage.input_tokens < window / 2;
                Verdict::new(
                    c.usage_measured && sane,
                    format!("{} | window={window}", summarize(&c)),
                )
            }
        }
    })
}

fn case_max_tokens<'a>(p: &'a Probe, model: &'a str) -> CaseFut<'a> {
    Box::pin(async move {
        let mut req = chat_req(
            model,
            vec![Message::text(
                Role::User,
                "Count from 1 to 400, separated by spaces. Do not stop early.",
            )],
            vec![],
        );
        let budget = 24;
        req.sampling.max_output_tokens = Some(budget);
        match p.chat(&req).await.1 {
            Err(e) => failed(&e),
            Ok(c) => {
                if matches!(c.finish_reason, nexus_gateway::FinishReason::Length) {
                    return Verdict::new(
                        true,
                        format!("{} | 正文 {:?}", summarize(&c), head(c.text.trim(), 50)),
                    );
                }
                if c.usage.output_tokens > budget {
                    return Verdict::limit(format!(
                        "上游收下 inferenceConfig.maxTokens={budget} 但不生效：出了 {} 个 token，\
                         停止原因 {:?}。别拿它当截断手段。",
                        c.usage.output_tokens, c.finish_reason
                    ));
                }
                Verdict::new(
                    false,
                    format!("{} | 正文 {:?}", summarize(&c), head(c.text.trim(), 50)),
                )
            }
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
        name: "tool-schema",
        run: case_tool_schema,
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
        name: "thinking",
        run: case_thinking,
    },
    Case {
        name: "context",
        run: case_context,
    },
    Case {
        name: "max-tokens",
        run: case_max_tokens,
    },
];

async fn matrix(p: &Probe, filters: &[String]) {
    let models =
        std::env::var("KIRO_LIVE_MODELS").unwrap_or_else(|_| "kiro-claude-sonnet-4.6".into());
    let models: Vec<&str> = models
        .split(',')
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .collect();
    let mut failures = Vec::new();
    let mut limits = Vec::new();
    for model in &models {
        println!("\n═══ {model} ═══");
        for case in CASES {
            if !filters.is_empty() && !filters.iter().any(|f| case.name.contains(f.as_str())) {
                continue;
            }
            let started = std::time::Instant::now();
            let v = (case.run)(p, model).await;
            println!(
                "[{}] {:<14} {:>6}ms  {}",
                v.outcome.tag(),
                case.name,
                started.elapsed().as_millis(),
                v.note
            );
            match v.outcome {
                Outcome::Fail => failures.push(format!("{model}/{}", case.name)),
                Outcome::Limit => limits.push(format!("{model}/{}", case.name)),
                Outcome::Pass => {}
            }
        }
    }
    println!();
    if !limits.is_empty() {
        println!("{} 条是上游不支持（LIMIT）：{limits:?}", limits.len());
    }
    if failures.is_empty() {
        println!("没有 FAIL。");
    } else {
        println!("{} 条没过：{failures:?}", failures.len());
        std::process::exit(1);
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if std::env::var("NEXUS_GATEWAY_PROBE").ok().as_deref() != Some("1") {
        eprintln!(
            "这会用本机 Kiro 里正登着的号对 Amazon Q 发真请求、花它的额度。\
             确认的话设 NEXUS_GATEWAY_PROBE=1 再跑。"
        );
        std::process::exit(2);
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    let p = Probe::load().await;
    match args.first().map(String::as_str) {
        Some("status") | None => status(&p),
        Some("usage") => usage(&p).await,
        Some("profiles") => profiles(&p).await,
        Some("models") => models(&p, args.get(1).map(String::as_str)).await,
        Some("import") => import_scenario().await,
        Some("raw") => {
            let model = args
                .get(1)
                .cloned()
                .unwrap_or_else(|| "kiro-claude-sonnet-4.6".into());
            raw(&p, &model).await;
        }
        Some("matrix") => matrix(&p, &args[1..]).await,
        Some(other) => {
            eprintln!(
                "不认识的命令 {other}：status | usage | profiles | models [过滤] | import | raw [模型] | matrix [用例过滤…]"
            );
            std::process::exit(2);
        }
    }
}
