//! 用 API Key 接入的供应商。
//!
//! 供应商是网关的一条通道（`provider/`），和 Cursor、ChatGPT 那几条并列：它背后不是一队
//! 订阅号，而是用户自己的几把钥匙，各自一个兼容端点、一张模型清单。业务行不放钥匙，
//! 明文只在 `secrets`（`keyprov/<id>/api_key`）；列表给界面的是尾号。
//!
//! 模型清单就是这家能跑的上游模型 id。客户端怎么叫它们（Claude Code 的四档、Codex 的一个
//! 模型）是接入那一层按客户端配的事，不再存在供应商身上——那样每条通道配模型的方式才一致。

use crate::db::Db;
use crate::keys::key_provider_secret;
use crate::secrets::SecretStore;
use nexus_core::{now_iso, AppError, Result, Secret};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// 一家供应商最多记这么多个模型。拉下来的目录常有几百个，全留着只会让下拉框没法用。
pub const MAX_MODELS: usize = 300;

/// 供应商讲哪种方言。决定网关用哪条路径、怎么翻译。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApiFormat {
    /// Anthropic Messages。
    Anthropic,
    /// OpenAI Chat Completions。
    OpenaiChat,
    /// OpenAI Responses。
    OpenaiResponses,
}

impl ApiFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            ApiFormat::Anthropic => "anthropic",
            ApiFormat::OpenaiChat => "openai_chat",
            ApiFormat::OpenaiResponses => "openai_responses",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "anthropic" => Ok(ApiFormat::Anthropic),
            "openai_chat" => Ok(ApiFormat::OpenaiChat),
            "openai_responses" => Ok(ApiFormat::OpenaiResponses),
            other => Err(AppError::internal(format!("未知的 API 格式：{other}"))),
        }
    }
}

/// Anthropic 格式的钥匙放哪个头。
///
/// 第三方中转认 `Authorization: Bearer`；官方 API 认 `x-api-key`。放错一个，对面就是 401。
/// OpenAI 两种格式一律 Bearer，这一项不看。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthField {
    AuthToken,
    ApiKey,
}

impl AuthField {
    pub fn as_str(self) -> &'static str {
        match self {
            AuthField::AuthToken => "auth_token",
            AuthField::ApiKey => "api_key",
        }
    }

    fn parse(raw: &str) -> Result<Self> {
        match raw {
            "auth_token" => Ok(AuthField::AuthToken),
            "api_key" => Ok(AuthField::ApiKey),
            other => Err(AppError::internal(format!("未知的认证字段：{other}"))),
        }
    }
}

/// 列表和详情。没有钥匙明文。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct KeyProvider {
    pub id: String,
    pub name: String,
    pub website: Option<String>,
    pub base_url: String,
    pub api_format: ApiFormat,
    pub auth_field: AuthField,
    /// 这家能跑的上游模型 id，按用户排的顺序。网关的 `provider/{id}` 走声明了它的供应商。
    pub models: Vec<String>,
    /// 停用的供应商不进网关，配置和钥匙都留着。
    pub enabled: bool,
    /// 末四位。短钥匙是空串，界面显示成「已保存」。
    pub key_tail: String,
    pub created_at: String,
    pub updated_at: String,
}

impl KeyProvider {
    /// 这家有没有声明这个模型。大小写不敏感，Claude Code 的 `[1m]` 标记不算名字的一部分。
    pub fn serves(&self, model: &str) -> bool {
        let want = bare_model(model);
        !want.is_empty() && self.models.iter().any(|m| m.eq_ignore_ascii_case(want))
    }
}

/// 新建或更新。`api_key` 在编辑时可以空着，表示不换钥匙；`enabled` 空着表示不动。
///
/// 不派生 `Debug`：这结构体带着明文，随手 `dbg!` 就会进日志。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyProviderInput {
    #[serde(default)]
    pub id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub website: Option<String>,
    pub base_url: String,
    pub api_format: ApiFormat,
    pub auth_field: AuthField,
    pub models: Vec<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub api_key: Option<String>,
}

/// 去掉 Claude Code 的上下文标记。`deepseek-v4-pro[1m]` 发给上游的是 `deepseek-v4-pro`。
pub fn bare_model(model: &str) -> &str {
    let m = model.trim();
    let lower = m.to_ascii_lowercase();
    if lower.ends_with("[1m]") {
        m[..m.len() - 4].trim_end()
    } else {
        m
    }
}

/// 一次请求打哪条路径。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Endpoint {
    /// 按供应商自己的方言发一轮对话。
    Chat,
    /// 模型目录。
    Models,
}

/// 地址里是不是已经带了版本段（`/v1`、`/v4`、`/v1beta`）。带了就是 OpenAI SDK 意义上的
/// base URL，直接接 `/chat/completions`；没带才补一个 `/v1`。
fn has_version_segment(base: &str) -> bool {
    let path = base.split_once("://").map(|(_, r)| r).unwrap_or(base);
    let path = path.split_once('/').map(|(_, p)| p).unwrap_or("");
    path.split('/').any(|seg| {
        let seg = seg.to_ascii_lowercase();
        let mut chars = seg.chars();
        chars.next() == Some('v')
            && chars.next().is_some_and(|c| c.is_ascii_digit())
            && seg[1..].chars().all(|c| c.is_ascii_alphanumeric())
    })
}

/// 这家供应商某一类请求的完整地址。
///
/// Anthropic 的根地址后面固定是 `/v1/messages`（`normalize_base_url` 已经去掉了多余的 `/v1`）；
/// OpenAI 两种格式的地址是 SDK 里的 base URL：带了版本段就原样接，没带补 `/v1`——智谱的
/// `/api/paas/v4`、Gemini 的 `/v1beta/openai` 这种不是 `/v1` 的都照写。
pub fn endpoint(base_url: &str, format: ApiFormat, kind: Endpoint) -> String {
    let root = base_url.trim().trim_end_matches('/');
    match format {
        ApiFormat::Anthropic => match kind {
            Endpoint::Chat => format!("{root}/v1/messages"),
            Endpoint::Models => format!("{root}/v1/models"),
        },
        ApiFormat::OpenaiChat | ApiFormat::OpenaiResponses => {
            let base = if has_version_segment(root) {
                root.to_string()
            } else {
                format!("{root}/v1")
            };
            match (format, kind) {
                (_, Endpoint::Models) => format!("{base}/models"),
                (ApiFormat::OpenaiResponses, Endpoint::Chat) => format!("{base}/responses"),
                _ => format!("{base}/chat/completions"),
            }
        }
    }
}

pub fn list(db: &Db) -> Result<Vec<KeyProvider>> {
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, name, website, base_url, api_format, auth_field, models_json, key_tail, created_at, updated_at
             FROM key_providers ORDER BY created_at, name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], row_from)?;
        rows.collect::<rusqlite::Result<Vec<_>>>()
    })?
    .into_iter()
    .map(|row| row.parse())
    .collect()
}

pub fn get(db: &Db, id: &str) -> Result<KeyProvider> {
    let id = id.trim();
    let raw = db.with(|c| {
        c.query_row(
            "SELECT id, name, website, base_url, api_format, auth_field, models_json, key_tail, created_at, updated_at
             FROM key_providers WHERE id = ?1",
            [id],
            row_from,
        )
        .map(Some)
        .or_else(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            other => Err(other),
        })
    })?;
    match raw {
        Some(row) => row.parse(),
        None => Err(missing(id)),
    }
}

/// 按名字找（大小写不敏感）。网关的号标签就是供应商名。
pub fn find_by_name(db: &Db, name: &str) -> Result<Option<KeyProvider>> {
    let name = name.trim();
    Ok(list(db)?
        .into_iter()
        .find(|p| p.name.eq_ignore_ascii_case(name)))
}

pub fn load_key(secrets: &dyn SecretStore, id: &str) -> Result<Secret> {
    secrets.require(&key_provider_secret(id.trim()))
}

pub fn save(db: &Db, secrets: &dyn SecretStore, input: KeyProviderInput) -> Result<KeyProvider> {
    let name = clean_name(&input.name)?;
    let website = clean_website(input.website.as_deref())?;
    let base_url = normalize_base_url(&input.base_url, input.api_format)?;
    let models = clean_models(input.models);
    if models.is_empty() {
        return Err(AppError::invalid("至少填一个模型。")
            .with_hint("点「获取模型列表」从这家拉，或者手动加上它的模型 id。"));
    }

    let now = now_iso();
    let existing = match input.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => Some(get(db, id)?),
        None => None,
    };
    if list(db)?.iter().any(|p| {
        p.name.eq_ignore_ascii_case(&name) && existing.as_ref().is_none_or(|e| e.id != p.id)
    }) {
        return Err(
            AppError::invalid(format!("已经有一家叫「{name}」的供应商了。"))
                .with_hint("网关和请求记录按名字认供应商，换个名字区分开。"),
        );
    }

    let api_key = input.api_key.unwrap_or_default();
    let api_key = api_key.trim();
    if existing.is_none() && api_key.is_empty() {
        return Err(AppError::invalid("先填 API Key。"));
    }
    if api_key.chars().count() > 512 || api_key.contains(char::is_whitespace) {
        return Err(AppError::invalid("API Key 长得不像一把钥匙。")
            .with_hint("复制的时候可能带上了空格或换行。"));
    }
    let enabled = input
        .enabled
        .or(existing.as_ref().map(|p| p.enabled))
        .unwrap_or(true);

    let id = existing
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let key_tail = if api_key.is_empty() {
        existing
            .as_ref()
            .map(|p| p.key_tail.clone())
            .unwrap_or_default()
    } else {
        key_tail(api_key)
    };
    let created_at = existing
        .as_ref()
        .map(|p| p.created_at.clone())
        .unwrap_or_else(|| now.clone());

    if !api_key.is_empty() {
        secrets.set(&key_provider_secret(&id), &Secret::new(api_key.to_string()))?;
    }

    let models_json = StoredModels::encode(&models, enabled)?;
    let wrote = db.with(|c| {
        if existing.is_some() {
            c.execute(
                "UPDATE key_providers
                 SET name = ?2, website = ?3, base_url = ?4, api_format = ?5, auth_field = ?6,
                     models_json = ?7, key_tail = ?8, updated_at = ?9
                 WHERE id = ?1",
                rusqlite::params![
                    id,
                    name,
                    website,
                    base_url,
                    input.api_format.as_str(),
                    input.auth_field.as_str(),
                    models_json,
                    key_tail,
                    now,
                ],
            )
        } else {
            c.execute(
                "INSERT INTO key_providers
                   (id, name, website, base_url, api_format, auth_field, models_json, key_tail, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                rusqlite::params![
                    id,
                    name,
                    website,
                    base_url,
                    input.api_format.as_str(),
                    input.auth_field.as_str(),
                    models_json,
                    key_tail,
                    created_at,
                    now,
                ],
            )
        }
    });
    if let Err(err) = wrote {
        if existing.is_none() {
            let _ = secrets.delete(&key_provider_secret(&id));
        }
        return Err(err);
    }
    get(db, &id)
}

/// 只改开关，不碰别的字段。
pub fn set_enabled(db: &Db, id: &str, enabled: bool) -> Result<KeyProvider> {
    let current = get(db, id)?;
    let models_json = StoredModels::encode(&current.models, enabled)?;
    db.with(|c| {
        c.execute(
            "UPDATE key_providers SET models_json = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![current.id, models_json, now_iso()],
        )
    })?;
    get(db, &current.id)
}

pub fn delete(db: &Db, secrets: &dyn SecretStore, id: &str) -> Result<()> {
    let id = id.trim();
    let gone = db.with(|c| c.execute("DELETE FROM key_providers WHERE id = ?1", [id]))?;
    if gone == 0 {
        return Err(missing(id));
    }
    secrets.delete(&key_provider_secret(id))?;
    Ok(())
}

fn missing(id: &str) -> AppError {
    AppError::invalid(format!("没有这个供应商（{id}）。")).with_hint("列表可能过期了，刷新后再试。")
}

fn clean_name(raw: &str) -> Result<String> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(AppError::invalid("先给供应商起个名字。"));
    }
    if name.chars().count() > 80 {
        return Err(AppError::invalid("名字太长了。"));
    }
    if name.contains('/') {
        return Err(AppError::invalid("名字里不要带斜杠。")
            .with_hint("模型名写成 provider/模型，名字里有斜杠会被当成路径。"));
    }
    Ok(name.to_string())
}

fn clean_website(raw: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let url = check_url(raw.trim_end_matches('/'))?;
    Ok(Some(url))
}

/// 存下来的地址。
///
/// 去掉尾部斜杠，以及用户整段贴进来的端点路径（`/chat/completions` `/responses` `/messages`
/// `/models`）。Anthropic 再去掉结尾的 `/v1`：它的根地址后面固定拼 `/v1/messages`，
/// 带着 `/v1` 会拼成 `/v1/v1/messages`。OpenAI 两种格式的版本段留着——那是 base URL 的一部分，
/// 智谱是 `/api/paas/v4`，去掉就错了。路径前缀（`/anthropic`）一律留着。
pub fn normalize_base_url(raw: &str, format: ApiFormat) -> Result<String> {
    let mut s = raw.trim().trim_end_matches('/').to_string();
    for tail in [
        "/chat/completions",
        "/completions",
        "/responses",
        "/messages",
        "/models",
    ] {
        if s.to_ascii_lowercase().ends_with(tail) {
            s.truncate(s.len() - tail.len());
            s = s.trim_end_matches('/').to_string();
            break;
        }
    }
    if format == ApiFormat::Anthropic && s.to_ascii_lowercase().ends_with("/v1") {
        s.truncate(s.len() - 3);
        s = s.trim_end_matches('/').to_string();
    }
    check_url(&s)
}

fn check_url(s: &str) -> Result<String> {
    if s.chars().count() > 500 {
        return Err(AppError::invalid("请求地址太长了。"));
    }
    if s.contains(char::is_whitespace) || s.contains('?') || s.contains('#') {
        return Err(AppError::invalid("请求地址不要带空格、查询参数或片段。")
            .with_hint("钥匙填在 API Key 里，不要嵌进地址。"));
    }
    let Some((scheme, rest)) = s.split_once("://") else {
        return Err(AppError::invalid("请求地址要以 http:// 或 https:// 开头。"));
    };
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(AppError::invalid("请求地址只接受 http 或 https。"));
    }
    if rest.is_empty() {
        return Err(AppError::invalid("请求地址缺主机名。"));
    }
    let (hostport, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, String::new()),
    };
    if hostport.is_empty() || hostport.contains('@') {
        return Err(AppError::invalid("地址里不要带账号密码。").with_hint("钥匙填在 API Key 里。"));
    }
    Ok(format!(
        "{scheme}://{}{path}",
        hostport.to_ascii_lowercase()
    ))
}

/// 去空白、去 `[1m]`、去重（大小写不敏感），保留用户排的顺序。
fn clean_models(models: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in models {
        let id: String = bare_model(&m).chars().take(200).collect();
        if id.is_empty() || id.contains(char::is_whitespace) {
            continue;
        }
        if out.iter().any(|e| e.eq_ignore_ascii_case(&id)) {
            continue;
        }
        out.push(id);
        if out.len() >= MAX_MODELS {
            break;
        }
    }
    out
}

/// 末四位。不到 8 个字符的钥匙不留尾号——尾号本身就会是整把钥匙。
fn key_tail(key: &str) -> String {
    let n = key.chars().count();
    if n < 8 {
        return String::new();
    }
    key.chars().skip(n - 4).collect()
}

/// 上一版按 Claude Code 四档存的模型。只为读老行、给老版本留一份能读的镜像。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LegacyRole {
    #[serde(default)]
    display: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    context1m: bool,
}

/// `models_json` 这一列的形状。
///
/// 现在认 `models` + `enabled`。老行只有 `sonnet` / `opus` / `fable` / `haiku` 四档，读的时候
/// 把四档里的 id 收成清单；写的时候仍把第一个模型镜像进 `sonnet`——同一个库还可能被没升级的
/// 版本打开，它们只认四档，Sonnet 空着会把这家当成坏行。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoredModels {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    models: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    enabled: Option<bool>,
    #[serde(default)]
    sonnet: LegacyRole,
    #[serde(default)]
    opus: LegacyRole,
    #[serde(default)]
    fable: LegacyRole,
    #[serde(default)]
    haiku: LegacyRole,
}

impl StoredModels {
    fn encode(models: &[String], enabled: bool) -> Result<String> {
        let stored = StoredModels {
            models: Some(models.to_vec()),
            enabled: Some(enabled),
            sonnet: LegacyRole {
                id: models.first().cloned().unwrap_or_default(),
                ..LegacyRole::default()
            },
            ..StoredModels::default()
        };
        serde_json::to_string(&stored)
            .map_err(|e| AppError::internal(format!("模型清单序列化失败：{e}")))
    }

    fn decode(raw: &str) -> std::result::Result<(Vec<String>, bool), serde_json::Error> {
        let stored: StoredModels = serde_json::from_str(raw)?;
        let enabled = stored.enabled.unwrap_or(true);
        let models = match stored.models {
            Some(list) => clean_models(list),
            None => clean_models(
                [stored.sonnet, stored.opus, stored.fable, stored.haiku]
                    .into_iter()
                    .map(|r| r.id)
                    .collect(),
            ),
        };
        Ok((models, enabled))
    }
}

struct RawRow {
    id: String,
    name: String,
    website: Option<String>,
    base_url: String,
    api_format: String,
    auth_field: String,
    models_json: String,
    key_tail: String,
    created_at: String,
    updated_at: String,
}

impl RawRow {
    fn parse(self) -> Result<KeyProvider> {
        let (models, enabled) = StoredModels::decode(&self.models_json).map_err(|e| {
            AppError::internal(format!("供应商 {} 的模型清单读不出来：{e}", self.id))
        })?;
        Ok(KeyProvider {
            id: self.id,
            name: self.name,
            website: self.website,
            base_url: self.base_url,
            api_format: ApiFormat::parse(&self.api_format)?,
            auth_field: AuthField::parse(&self.auth_field)?,
            models,
            enabled,
            key_tail: self.key_tail,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn row_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        id: row.get(0)?,
        name: row.get(1)?,
        website: row.get(2)?,
        base_url: row.get(3)?,
        api_format: row.get(4)?,
        auth_field: row.get(5)?,
        models_json: row.get(6)?,
        key_tail: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::SqliteSecrets;
    use std::sync::Arc;

    fn env() -> (Arc<Db>, SqliteSecrets) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let secrets = SqliteSecrets::new(db.clone());
        (db, secrets)
    }

    fn input(key: &str) -> KeyProviderInput {
        KeyProviderInput {
            id: None,
            name: "DeepSeek".into(),
            website: Some("https://api-docs.deepseek.com/".into()),
            base_url: "https://API.DeepSeek.com/anthropic/v1/".into(),
            api_format: ApiFormat::Anthropic,
            auth_field: AuthField::AuthToken,
            models: vec![
                " deepseek-v4-pro[1m] ".into(),
                "deepseek-v4-flash".into(),
                "DeepSeek-V4-Pro".into(),
                "".into(),
            ],
            enabled: None,
            api_key: Some(key.into()),
        }
    }

    #[test]
    fn the_row_keeps_a_tail_and_the_secret_stays_out_of_the_table() {
        let (db, secrets) = env();
        let saved = save(&db, &secrets, input("sk-live-abcdef1234")).unwrap();
        assert_eq!(saved.name, "DeepSeek");
        assert_eq!(saved.base_url, "https://api.deepseek.com/anthropic");
        assert_eq!(
            saved.website.as_deref(),
            Some("https://api-docs.deepseek.com")
        );
        assert_eq!(saved.key_tail, "1234");
        assert_eq!(saved.models, vec!["deepseek-v4-pro", "deepseek-v4-flash"]);
        assert!(saved.enabled);
        assert!(saved.serves("deepseek-v4-pro[1m]"));
        assert!(saved.serves("DEEPSEEK-V4-FLASH"));
        assert!(!saved.serves("gpt-5.4"));

        let dumped: String = db
            .with(|c| {
                c.query_row(
                    "SELECT name || base_url || models_json || key_tail || COALESCE(website, '') FROM key_providers",
                    [],
                    |r| r.get(0),
                )
            })
            .unwrap();
        assert!(!dumped.contains("sk-live-abcdef1234"));
        assert_eq!(
            load_key(&secrets, &saved.id).unwrap().expose(),
            "sk-live-abcdef1234"
        );
    }

    #[test]
    fn an_empty_key_on_update_keeps_the_old_secret_and_the_switch() {
        let (db, secrets) = env();
        let saved = save(&db, &secrets, input("sk-live-abcdef1234")).unwrap();
        set_enabled(&db, &saved.id, false).unwrap();
        let mut next = input("");
        next.id = Some(saved.id.clone());
        next.api_key = Some("   ".into());
        next.name = "DeepSeek 2".into();
        let updated = save(&db, &secrets, next).unwrap();
        assert_eq!(updated.name, "DeepSeek 2");
        assert_eq!(updated.key_tail, "1234");
        assert!(!updated.enabled, "没传开关就不动它");
        assert_eq!(
            load_key(&secrets, &saved.id).unwrap().expose(),
            "sk-live-abcdef1234"
        );
    }

    #[test]
    fn names_are_unique_case_insensitively() {
        let (db, secrets) = env();
        save(&db, &secrets, input("sk-live-abcdef1234")).unwrap();
        let mut dup = input("sk-live-zzzzzz9999");
        dup.name = "deepseek".into();
        let err = save(&db, &secrets, dup).unwrap_err();
        assert!(err.message.contains("已经有"));
        let mut slash = input("sk-live-zzzzzz9999");
        slash.name = "a/b".into();
        assert!(save(&db, &secrets, slash).is_err());
    }

    #[test]
    fn delete_removes_the_secret_too() {
        let (db, secrets) = env();
        let saved = save(&db, &secrets, input("sk-live-abcdef1234")).unwrap();
        delete(&db, &secrets, &saved.id).unwrap();
        assert!(list(&db).unwrap().is_empty());
        assert!(load_key(&secrets, &saved.id).is_err());
    }

    #[test]
    fn a_short_key_leaves_no_tail() {
        let (db, secrets) = env();
        let saved = save(&db, &secrets, input("short-k")).unwrap();
        assert!(saved.key_tail.is_empty());
    }

    #[test]
    fn a_provider_needs_at_least_one_model() {
        let (db, secrets) = env();
        let mut bad = input("sk-live-abcdef1234");
        bad.models = vec!["  ".into()];
        let err = save(&db, &secrets, bad).unwrap_err();
        assert!(err.message.contains("模型"));
    }

    #[test]
    fn legacy_four_role_rows_still_read_as_a_model_list() {
        let (db, _secrets) = env();
        db.with(|c| {
            c.execute(
                "INSERT INTO key_providers
                   (id, name, website, base_url, api_format, auth_field, models_json, key_tail, created_at, updated_at)
                 VALUES ('p1', 'Wasu', NULL, 'https://token.wasu.cn', 'openai_chat', 'auth_token',
                   '{\"sonnet\":{\"display\":\"V4 Pro\",\"id\":\"deepseek-v4-pro\",\"context1m\":true},\"opus\":{\"id\":\"\"},\"fable\":{\"id\":\"\"},\"haiku\":{\"id\":\"deepseek-v4-flash\"}}',
                   'abcd', '2026-09-25T00:00:00Z', '2026-09-25T00:00:00Z')",
                [],
            )
        })
        .unwrap();
        let p = get(&db, "p1").unwrap();
        assert_eq!(p.models, vec!["deepseek-v4-pro", "deepseek-v4-flash"]);
        assert!(p.enabled);
    }

    #[test]
    fn new_rows_keep_a_sonnet_mirror_for_older_builds() {
        let (db, secrets) = env();
        save(&db, &secrets, input("sk-live-abcdef1234")).unwrap();
        let raw: String = db
            .with(|c| c.query_row("SELECT models_json FROM key_providers", [], |r| r.get(0)))
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(v["sonnet"]["id"], "deepseek-v4-pro");
        assert_eq!(v["models"][1], "deepseek-v4-flash");
    }

    #[test]
    fn userinfo_and_query_strings_are_refused() {
        let err = normalize_base_url("https://user:pw@example.com/v1", ApiFormat::OpenaiChat)
            .unwrap_err();
        assert!(err.message.contains("账号密码"));
        let err = normalize_base_url("https://example.com/v1?key=secret", ApiFormat::OpenaiChat)
            .unwrap_err();
        assert!(err.message.contains("查询参数"));
    }

    #[test]
    fn base_urls_keep_openai_version_paths_and_drop_pasted_endpoints() {
        let n = |raw: &str, f: ApiFormat| normalize_base_url(raw, f).unwrap();
        assert_eq!(
            n(
                "https://api.deepseek.com/anthropic/v1/messages",
                ApiFormat::Anthropic
            ),
            "https://api.deepseek.com/anthropic"
        );
        assert_eq!(
            n("https://token.wasu.cn/v1/", ApiFormat::OpenaiChat),
            "https://token.wasu.cn/v1"
        );
        assert_eq!(
            n(
                "https://open.bigmodel.cn/api/paas/v4/chat/completions",
                ApiFormat::OpenaiChat
            ),
            "https://open.bigmodel.cn/api/paas/v4"
        );
        assert_eq!(
            n(
                "https://api.openai.com/v1/responses",
                ApiFormat::OpenaiResponses
            ),
            "https://api.openai.com/v1"
        );
    }

    #[test]
    fn endpoints_follow_each_format() {
        use ApiFormat::*;
        assert_eq!(
            endpoint(
                "https://api.deepseek.com/anthropic",
                Anthropic,
                Endpoint::Chat
            ),
            "https://api.deepseek.com/anthropic/v1/messages"
        );
        // 老版本把 `/v1` 去掉了才存：没有版本段就补回来。
        assert_eq!(
            endpoint("https://token.wasu.cn", OpenaiChat, Endpoint::Chat),
            "https://token.wasu.cn/v1/chat/completions"
        );
        assert_eq!(
            endpoint("https://token.wasu.cn/v1", OpenaiChat, Endpoint::Models),
            "https://token.wasu.cn/v1/models"
        );
        assert_eq!(
            endpoint(
                "https://open.bigmodel.cn/api/paas/v4",
                OpenaiChat,
                Endpoint::Chat
            ),
            "https://open.bigmodel.cn/api/paas/v4/chat/completions"
        );
        assert_eq!(
            endpoint(
                "https://generativelanguage.googleapis.com/v1beta/openai",
                OpenaiChat,
                Endpoint::Chat
            ),
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
        );
        assert_eq!(
            endpoint("https://api.openai.com/v1", OpenaiResponses, Endpoint::Chat),
            "https://api.openai.com/v1/responses"
        );
        // 主机名里的 v 开头片段不算版本段。
        assert_eq!(
            endpoint("https://vip.example.com", OpenaiChat, Endpoint::Chat),
            "https://vip.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn the_1m_marker_is_not_part_of_the_name() {
        assert_eq!(bare_model("claude-opus-5[1m]"), "claude-opus-5");
        assert_eq!(bare_model("claude-opus-5[1M]"), "claude-opus-5");
        assert_eq!(bare_model(" gpt-5.4 "), "gpt-5.4");
    }
}
