//! 供应商通道（`provider/`）：用户自己的 API Key，各带一个兼容端点和一张模型清单。
//!
//! 它和 Cursor、ChatGPT 那几条通道是同一种东西——通道 = 前缀 + 一队「号」+ 后端 + 门禁——
//! 只是这里的「号」是一家家供应商：
//!
//! - **门禁**：声明了这个模型的供应商才接；目录是几家清单的并集。
//! - **一队号**：同一个模型可以有好几家声明（官方直连 + 一家中转）。一直用上次成功的那家，
//!   它出错了才换下一家；钥匙坏了、余额没了整家停一会儿，限流只冷却「这家 × 这个模型」。
//!   和订阅号不同，换一家就是换一个上游，所以上游自己抖（5xx、连不上）也值得换。
//! - **后端**：方言一致原样转发，不一致翻译（见 [`wire`]）。
//!
//! 裸名：写进客户端的模型名常常不带前缀（上一版就是这么写的）。供应商清单里的名字是用户
//! 亲手声明的，所以裸名恰好对上某一家时走这里，其余照旧走默认通道。

pub mod wire;

use crate::channel::{Capability, Channel, ChannelGate, PROVIDER};
use crate::error::{UpstreamError, UpstreamKind};
use crate::identity::DeviceIdentity;
use crate::lane::{
    AvailableView, BoxFuture, CandidateState, CandidateView, Credential, Lane, LaneSnapshot,
    Outcome,
};
use crate::normalized::{ChatRequest, Completion};
use crate::upstream::{DeltaSink, Upstream};
use nexus_store::key_providers::{self, bare_model, KeyProvider};
use nexus_store::{Db, SecretStore};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const PREFIXES: &[&str] = &["provider/"];

/// 钥匙被拒（401 / 403）：整家先停这么久，别每个请求都去撞一次。
const AUTH_HOLD: Duration = Duration::from_secs(10 * 60);
/// 余额 / 额度没了。
const QUOTA_HOLD: Duration = Duration::from_secs(30 * 60);
/// 这家限流了这个模型。
const RATE_HOLD: Duration = Duration::from_secs(60);
/// 这家其实跑不了这个模型（清单写错了、上游下架了）。
const MODEL_HOLD: Duration = Duration::from_secs(30 * 60);
/// 上游自己在抖。短一点：多半是一阵子的事。
const UPSTREAM_HOLD: Duration = Duration::from_secs(30);

/// lane 拿到的是路由后的全名（`provider/deepseek-v4-pro`）或裸名；比清单要的是上游 id。
fn base_model(model: &str) -> &str {
    let m = model.trim();
    let stripped = PREFIXES.iter().find_map(|p| {
        (m.len() >= p.len() && m.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes()))
            .then(|| &m[p.len()..])
    });
    bare_model(stripped.unwrap_or(m))
}

fn enabled(db: &Db) -> Vec<KeyProvider> {
    match key_providers::list(db) {
        Ok(list) => list.into_iter().filter(|p| p.enabled).collect(),
        Err(err) => {
            tracing::warn!(%err, "读供应商列表失败");
            Vec::new()
        }
    }
}

pub struct ProviderGate {
    db: Arc<Db>,
}

impl ChannelGate for ProviderGate {
    fn ready(&self) -> bool {
        !enabled(&self.db).is_empty()
    }

    fn owns(&self, cap: Capability, base_model: &str) -> bool {
        cap == Capability::Chat && enabled(&self.db).iter().any(|p| p.serves(base_model))
    }

    fn models(&self, cap: Capability) -> Vec<String> {
        if cap != Capability::Chat {
            return Vec::new();
        }
        let mut out: Vec<String> = Vec::new();
        for p in enabled(&self.db) {
            for m in p.models {
                if !out.iter().any(|e| e.eq_ignore_ascii_case(&m)) {
                    out.push(m);
                }
            }
        }
        out
    }

    fn media_ready(&self) -> bool {
        false
    }

    fn claims_bare(&self, model: &str) -> bool {
        self.owns(Capability::Chat, model)
    }
}

struct Hold {
    until: Instant,
    reason: String,
}

#[derive(Default)]
struct LaneState {
    /// 每个模型粘住的那家（小写模型名 → 供应商名）。一直用它，上游的缓存才接得住。
    sticky: HashMap<String, String>,
    /// （小写供应商名, 小写模型名）→ 冷却。
    cooled: HashMap<(String, String), Hold>,
    /// 小写供应商名 → 整家停用到什么时候。
    down: HashMap<String, Hold>,
    /// 最近一次成功的那家，状态快照里标「正在用」。
    last: Option<String>,
}

impl LaneState {
    fn prune(&mut self, now: Instant) {
        self.cooled.retain(|_, h| h.until > now);
        self.down.retain(|_, h| h.until > now);
    }
}

pub struct ProviderLane {
    db: Arc<Db>,
    secrets: Arc<dyn SecretStore>,
    state: Mutex<LaneState>,
}

impl ProviderLane {
    pub fn new(db: Arc<Db>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            db,
            secrets,
            state: Mutex::new(LaneState::default()),
        }
    }

    fn credential(&self, p: &KeyProvider) -> Result<Credential, UpstreamError> {
        let secret = key_providers::load_key(self.secrets.as_ref(), &p.id).map_err(|_| {
            UpstreamError::new(
                UpstreamKind::Auth,
                401,
                format!("供应商「{}」的钥匙读不出来，重新填一次。", p.name),
            )
        })?;
        Ok(Credential {
            label: p.name.clone(),
            access_token: secret.expose().to_string(),
            identity: DeviceIdentity::derived(&p.id),
        })
    }

    /// 清掉冷却与停用记录（钥匙刚换过、刚充了值）。
    pub fn reset(&self) {
        let mut st = self.state.lock().expect("provider lane");
        st.cooled.clear();
        st.down.clear();
    }

    /// 用户点了「用这家」：它声明的每个模型都先找它，冷却记录一并清掉。
    pub fn prefer(&self, name: &str) {
        let Some(p) = key_providers::find_by_name(&self.db, name).ok().flatten() else {
            return;
        };
        let key = p.name.to_ascii_lowercase();
        let mut st = self.state.lock().expect("provider lane");
        for m in &p.models {
            st.sticky.insert(m.to_ascii_lowercase(), p.name.clone());
        }
        st.down.remove(&key);
        st.cooled.retain(|(prov, _), _| *prov != key);
        st.last = Some(p.name.clone());
    }

    /// 一家被删 / 停用：关于它的记录一起忘。
    pub fn forget(&self, name: &str) {
        let key = name.to_ascii_lowercase();
        let mut st = self.state.lock().expect("provider lane");
        st.down.remove(&key);
        st.cooled.retain(|(p, _), _| *p != key);
        st.sticky.retain(|_, v| !v.eq_ignore_ascii_case(name));
        if st
            .last
            .as_deref()
            .is_some_and(|l| l.eq_ignore_ascii_case(name))
        {
            st.last = None;
        }
    }

    pub fn snapshot(&self) -> LaneSnapshot {
        let all = key_providers::list(&self.db).unwrap_or_default();
        let now = Instant::now();
        let mut st = self.state.lock().expect("provider lane");
        st.prune(now);
        let mut candidates = Vec::new();
        let mut available = Vec::new();
        for p in all {
            if !p.enabled {
                available.push(AvailableView {
                    label: p.name.clone(),
                    source: "provider",
                    pinned: false,
                    percent_used: None,
                });
                continue;
            }
            let key = p.name.to_ascii_lowercase();
            let state = if let Some(h) = st.down.get(&key) {
                CandidateState::Exhausted {
                    reason: h.reason.clone(),
                    retry_in_secs: h.until.saturating_duration_since(now).as_secs(),
                }
            } else {
                let cooled: Vec<(&String, &Hold)> = st
                    .cooled
                    .iter()
                    .filter(|((prov, _), _)| *prov == key)
                    .map(|((_, m), h)| (m, h))
                    .collect();
                if !cooled.is_empty() {
                    let secs_left = cooled
                        .iter()
                        .map(|(_, h)| h.until.saturating_duration_since(now).as_secs())
                        .max()
                        .unwrap_or(0);
                    let mut models: Vec<String> =
                        cooled.iter().map(|(m, _)| (*m).clone()).collect();
                    models.sort();
                    CandidateState::Cooled { models, secs_left }
                } else if st
                    .last
                    .as_deref()
                    .is_some_and(|l| l.eq_ignore_ascii_case(&p.name))
                {
                    CandidateState::Current
                } else {
                    CandidateState::Ready
                }
            };
            candidates.push(CandidateView {
                label: p.name.clone(),
                source: "provider",
                pinned: false,
                stored_id: Some(p.id.clone()),
                percent_used: None,
                state,
            });
        }
        LaneSnapshot {
            current: st.last.clone(),
            candidates,
            missing: Vec::new(),
            available,
        }
    }
}

impl Lane for ProviderLane {
    fn acquire<'a>(&'a self, model: &'a str) -> BoxFuture<'a, Result<Credential, UpstreamError>> {
        Box::pin(async move {
            let want = base_model(model).to_string();
            let providers = enabled(&self.db);
            if providers.is_empty() {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    503,
                    "供应商通道里还没有启用的供应商。去「账号 → 供应商」添加一家。",
                ));
            }
            let serving: Vec<&KeyProvider> = providers.iter().filter(|p| p.serves(&want)).collect();
            if serving.is_empty() {
                return Err(UpstreamError::new(
                    UpstreamKind::ModelUnsupported,
                    404,
                    format!(
                        "没有哪家供应商的模型清单里有 {want}。在「账号 → 供应商」里把它加进某一家。"
                    ),
                ));
            }
            let pick = {
                let now = Instant::now();
                let mut st = self.state.lock().expect("provider lane");
                st.prune(now);
                let m = want.to_ascii_lowercase();
                let sticky = st.sticky.get(&m).cloned();
                let mut order: Vec<&KeyProvider> = Vec::with_capacity(serving.len());
                if let Some(s) = &sticky {
                    order.extend(
                        serving
                            .iter()
                            .copied()
                            .filter(|p| p.name.eq_ignore_ascii_case(s)),
                    );
                }
                order.extend(serving.iter().copied().filter(|p| {
                    sticky
                        .as_deref()
                        .is_none_or(|s| !p.name.eq_ignore_ascii_case(s))
                }));
                let free = order.iter().copied().find(|p| {
                    let key = p.name.to_ascii_lowercase();
                    !st.down.contains_key(&key) && !st.cooled.contains_key(&(key, m.clone()))
                });
                match free {
                    Some(p) => Ok((*p).clone()),
                    None => {
                        let why = order
                            .iter()
                            .filter_map(|p| {
                                let key = p.name.to_ascii_lowercase();
                                st.down
                                    .get(&key)
                                    .or_else(|| st.cooled.get(&(key, m.clone())))
                                    .map(|h| {
                                        format!(
                                            "{}：{}（{} 秒后再试）",
                                            p.name,
                                            h.reason,
                                            h.until.saturating_duration_since(now).as_secs()
                                        )
                                    })
                            })
                            .collect::<Vec<_>>()
                            .join("；");
                        Err(UpstreamError::new(
                            UpstreamKind::RateLimit,
                            429,
                            format!("能跑 {want} 的供应商都在歇着——{why}"),
                        ))
                    }
                }
            }?;
            self.credential(&pick)
        })
    }

    fn acquire_label<'a>(
        &'a self,
        label: &'a str,
    ) -> BoxFuture<'a, Result<Credential, UpstreamError>> {
        Box::pin(async move {
            let found = key_providers::find_by_name(&self.db, label)
                .ok()
                .flatten()
                .ok_or_else(|| {
                    UpstreamError::new(
                        UpstreamKind::Upstream,
                        404,
                        format!("没有叫「{label}」的供应商"),
                    )
                })?;
            self.credential(&found)
        })
    }

    fn report(&self, credential: &Credential, model: &str, outcome: Outcome<'_>) {
        let key = credential.label.to_ascii_lowercase();
        let m = base_model(model).to_ascii_lowercase();
        let now = Instant::now();
        let mut st = self.state.lock().expect("provider lane");
        match outcome {
            Outcome::Ok(_) => {
                st.sticky.insert(m.clone(), credential.label.clone());
                st.cooled.remove(&(key.clone(), m));
                st.down.remove(&key);
                st.last = Some(credential.label.clone());
            }
            Outcome::Err(e) => {
                let hold = |d: Duration| Hold {
                    until: now + d,
                    reason: e.message.chars().take(160).collect(),
                };
                let quota_until = e
                    .reset_at_ms
                    .map(|ms| {
                        let left = ms - crate::media::now_ms();
                        Duration::from_millis(left.max(0) as u64)
                            .min(Duration::from_secs(24 * 3600))
                    })
                    .unwrap_or(QUOTA_HOLD);
                match e.kind {
                    UpstreamKind::Auth | UpstreamKind::Forbidden => {
                        st.down.insert(key.clone(), hold(AUTH_HOLD));
                    }
                    UpstreamKind::Quota => {
                        st.down.insert(key.clone(), hold(quota_until));
                    }
                    UpstreamKind::RateLimit | UpstreamKind::Provider => {
                        st.cooled.insert((key.clone(), m.clone()), hold(RATE_HOLD));
                    }
                    UpstreamKind::ModelUnsupported => {
                        st.cooled.insert((key.clone(), m.clone()), hold(MODEL_HOLD));
                    }
                    UpstreamKind::Upstream | UpstreamKind::Timeout => {
                        st.cooled
                            .insert((key.clone(), m.clone()), hold(UPSTREAM_HOLD));
                    }
                    UpstreamKind::BadRequest | UpstreamKind::Canceled => return,
                }
                if st
                    .sticky
                    .get(&m)
                    .is_some_and(|s| s.eq_ignore_ascii_case(&key))
                {
                    st.sticky.remove(&m);
                }
            }
        }
    }

    fn switch_helps_on_upstream_error(&self) -> bool {
        true
    }
}

pub struct ProviderUpstream {
    db: Arc<Db>,
}

impl Upstream for ProviderUpstream {
    fn stream<'a>(
        &'a self,
        credential: &'a Credential,
        request: &'a ChatRequest,
        on_delta: DeltaSink<'a>,
    ) -> BoxFuture<'a, Result<Completion, UpstreamError>> {
        Box::pin(async move {
            let p = key_providers::find_by_name(&self.db, &credential.label)
                .ok()
                .flatten()
                .ok_or_else(|| {
                    UpstreamError::new(
                        UpstreamKind::Upstream,
                        404,
                        format!("供应商「{}」刚被删掉了", credential.label),
                    )
                })?;
            let target = wire::Target {
                name: p.name,
                base_url: p.base_url,
                api_key: credential.access_token.clone(),
                format: p.api_format,
                auth: p.auth_field,
                upstream_model: base_model(&request.model).to_string(),
                extra_headers: Vec::new(),
                claude_oauth: None,
            };
            wire::execute(&target, request, on_delta).await
        })
    }
}

/// 供应商通道。lane 单独交回去：服务层要拿它出状态快照、清冷却。
pub fn provider_channel(lane: Arc<ProviderLane>, db: Arc<Db>) -> Channel {
    Channel {
        id: PROVIDER,
        label: "供应商",
        vendor: "provider",
        prefixes: PREFIXES,
        lane,
        upstream: Arc::new(ProviderUpstream { db: db.clone() }),
        gate: Arc::new(ProviderGate { db }),
        passthrough: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::normalized::Usage;
    use nexus_store::key_providers::{ApiFormat, AuthField, KeyProviderInput};
    use nexus_store::SqliteSecrets;

    fn setup(providers: &[(&str, &[&str])]) -> (Arc<Db>, ProviderLane) {
        let db = Arc::new(Db::open_in_memory().unwrap());
        let secrets: Arc<dyn SecretStore> = Arc::new(SqliteSecrets::new(db.clone()));
        for (name, models) in providers {
            key_providers::save(
                &db,
                secrets.as_ref(),
                KeyProviderInput {
                    id: None,
                    name: (*name).into(),
                    website: None,
                    base_url: "https://x.example/v1".into(),
                    api_format: ApiFormat::OpenaiChat,
                    auth_field: AuthField::AuthToken,
                    models: models.iter().map(|m| m.to_string()).collect(),
                    enabled: None,
                    api_key: Some(format!("sk-{name}-0000")),
                },
            )
            .unwrap();
            // created_at 精度到秒：同一秒里建的几家按名字排，测试里名字就按想要的顺序起。
        }
        let lane = ProviderLane::new(db.clone(), secrets);
        (db, lane)
    }

    fn err(kind: UpstreamKind) -> UpstreamError {
        UpstreamError::new(kind, 500, "boom")
    }

    #[tokio::test]
    async fn picks_a_provider_that_declares_the_model() {
        let (_db, lane) = setup(&[("a-one", &["m1"]), ("b-two", &["m2"])]);
        assert_eq!(lane.acquire("m2").await.unwrap().label, "b-two");
        assert_eq!(lane.acquire("provider/M2").await.unwrap().label, "b-two");
        let e = lane.acquire("nope").await.unwrap_err();
        assert_eq!(e.kind, UpstreamKind::ModelUnsupported);
        assert_eq!(
            lane.acquire("m1[1m]").await.unwrap().access_token,
            "sk-a-one-0000"
        );
    }

    #[tokio::test]
    async fn fails_over_and_sticks_to_what_worked() {
        let (_db, lane) = setup(&[("a-one", &["m"]), ("b-two", &["m"])]);
        let first = lane.acquire("m").await.unwrap();
        assert_eq!(first.label, "a-one");
        lane.report(&first, "m", Outcome::Err(&err(UpstreamKind::Quota)));
        let second = lane.acquire("m").await.unwrap();
        assert_eq!(second.label, "b-two");
        lane.report(&second, "m", Outcome::Ok(&Usage::default()));
        // a-one 停着、b-two 粘住。
        assert_eq!(lane.acquire("m").await.unwrap().label, "b-two");
        let snap = lane.snapshot();
        assert_eq!(snap.current.as_deref(), Some("b-two"));
        assert!(matches!(
            snap.candidates[0].state,
            CandidateState::Exhausted { .. }
        ));
        assert!(matches!(snap.candidates[1].state, CandidateState::Current));
        lane.reset();
        assert!(matches!(
            lane.snapshot().candidates[0].state,
            CandidateState::Ready
        ));
    }

    #[tokio::test]
    async fn a_rate_limit_only_cools_that_model() {
        let (_db, lane) = setup(&[("a-one", &["m", "n"])]);
        let c = lane.acquire("m").await.unwrap();
        lane.report(&c, "m", Outcome::Err(&err(UpstreamKind::RateLimit)));
        let e = lane.acquire("m").await.unwrap_err();
        assert_eq!(e.status, 429);
        assert!(e.message.contains("a-one"));
        assert_eq!(lane.acquire("n").await.unwrap().label, "a-one");
        // 请求本身的问题不罚供应商。
        let c = lane.acquire("n").await.unwrap();
        lane.report(&c, "n", Outcome::Err(&err(UpstreamKind::BadRequest)));
        assert!(lane.acquire("n").await.is_ok());
    }

    #[tokio::test]
    async fn disabled_providers_are_skipped_and_listed_as_available() {
        let (db, lane) = setup(&[("a-one", &["m"]), ("b-two", &["m"])]);
        let a = key_providers::find_by_name(&db, "a-one").unwrap().unwrap();
        key_providers::set_enabled(&db, &a.id, false).unwrap();
        assert_eq!(lane.acquire("m").await.unwrap().label, "b-two");
        let snap = lane.snapshot();
        assert_eq!(snap.candidates.len(), 1);
        assert_eq!(snap.available[0].label, "a-one");
        let gate = ProviderGate { db: db.clone() };
        assert!(gate.claims_bare("M"));
        assert!(!gate.claims_bare("other"));
        assert_eq!(gate.models(Capability::Chat), vec!["m".to_string()]);
    }
}
