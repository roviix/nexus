//! 额度接力。
//!
//! 本地网关不需要负载均衡：一个用户一台机器，任何时刻一个号就够。它需要的是把几个号**串成
//! 一个大额度**——一直用当前号，额度耗尽才接力下一个。于是：会话粘性天然成立（同一时段只有
//! 一个号在出流量，上游对话缓存不会因换号失效）、换号频率极低（一周几次而不是每请求）、
//! 风控面小、也不必解析 protobuf 做 sticky。
//!
//! 三条规则：
//! 1. **粘住当前号**。它还能用，就一直用它。第一次成功的号自动成为当前号。
//! 2. **耗尽才接力**。上游说额度 / 鉴权 / 权限不行（`blames_account` 且不是限流），或者用量快照
//!    显示到线，才换下一个并把它记成当前号。耗尽记录带 TTL，到点自动再试——额度会重置，
//!    永久标死会让号白白闲着。
//! 3. **限流只绕行**。某个模型被限流（或这个号出不了这个模型）时，本次请求借别的号，
//!    **当前号不变**——限流是（号 × 模型）级的暂时状态，为它换号会让其他模型的对话丢缓存。
//!
//! 这里只管「下一次该给谁」。**同一次请求里**的换号在 server 层（`server::relay`）：上游一回
//! 怪号的错误，server 先 `report` 让这里记下耗尽 / 冷却，再 `acquire` 一次就自然拿到下一个号，
//! 客户端看不到中间那次失败。所以 `report` 必须在 `acquire` 之前、而且同步生效。
//!
//! 同一个号从两个来源来（Cursor 正登着 + 也存在「我的账号」里）按邮箱去重，先到的来源赢：
//! Cursor 登录排第一，因为它钉的是真机码。
//!
//! 来源给出的号只是**能**用的；真进接力队的只有名单（[`Roster`]）里的那些。名单外的号在
//! 快照里单列成 `available`，界面上就是「添加」弹窗里的候选。

use super::roster::Roster;
use super::sources::{Candidate, CandidateKind, QuotaHint, Source};
use super::{BoxFuture, Credential, Lane, Outcome};
use crate::error::{UpstreamError, UpstreamKind};
use crate::identity::DeviceIdentity;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// 限流：这个号对这个模型歇多久。
pub const RATE_LIMIT_COOLDOWN: Duration = Duration::from_secs(5 * 60);
/// 出不了这个模型：多半是套餐差异，歇长一点。
pub const MODEL_UNSUPPORTED_COOLDOWN: Duration = Duration::from_secs(6 * 60 * 60);
/// 耗尽 / 鉴权失败：多久之后再试一次。
pub const EXHAUSTED_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone)]
struct Cooldown {
    until: Instant,
    kind: UpstreamKind,
}

#[derive(Debug, Default)]
struct State {
    /// 当前号（小写邮箱）。
    current: Option<String>,
    /// 号 → (到点时刻, 原因)。
    exhausted: HashMap<String, (Instant, String)>,
    /// (号, 模型) → 冷却。
    cooldowns: HashMap<(String, String), Cooldown>,
}

impl State {
    fn sweep(&mut self, now: Instant) {
        self.exhausted.retain(|_, (until, _)| *until > now);
        self.cooldowns.retain(|_, c| c.until > now);
    }
}

/// 一个候选此刻为什么不能用。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Block {
    Exhausted(String),
    QuotaLine,
    Cooled { kind: UpstreamKind, secs_left: u64 },
    Tried,
}

pub struct RelayLane {
    sources: Vec<Arc<dyn Source>>,
    roster: Arc<Roster>,
    state: Mutex<State>,
    rate_limit_cooldown: Duration,
    model_unsupported_cooldown: Duration,
    exhausted_ttl: Duration,
}

impl RelayLane {
    /// `sources` 的顺序就是接力顺序；`roster` 决定其中哪些号真的进队。
    pub fn new(sources: Vec<Arc<dyn Source>>, roster: Arc<Roster>) -> Self {
        Self {
            sources,
            roster,
            state: Mutex::new(State::default()),
            rate_limit_cooldown: RATE_LIMIT_COOLDOWN,
            model_unsupported_cooldown: MODEL_UNSUPPORTED_COOLDOWN,
            exhausted_ttl: EXHAUSTED_TTL,
        }
    }

    pub fn roster(&self) -> &Roster {
        &self.roster
    }

    #[cfg(test)]
    fn with_timings(
        mut self,
        rate_limit: Duration,
        model_unsupported: Duration,
        exhausted: Duration,
    ) -> Self {
        self.rate_limit_cooldown = rate_limit;
        self.model_unsupported_cooldown = model_unsupported;
        self.exhausted_ttl = exhausted;
        self
    }

    /// 清掉全部耗尽 / 冷却记录（用户手动，或知道额度刚重置）。当前号保留。
    pub fn reset(&self) {
        let mut st = self.state.lock().expect("lane state");
        st.exhausted.clear();
        st.cooldowns.clear();
    }

    /// 手动指定当前号（界面上点「用这个」）。不存在的号也记下——下次它出现就用它。
    pub fn set_current(&self, label: &str) {
        self.state.lock().expect("lane state").current = Some(label.trim().to_lowercase());
    }

    /// 号被移出名单：它不再是当前号，关于它的耗尽 / 冷却记录也一起忘掉——
    /// 下次再加回来，从干净状态开始。
    pub fn forget(&self, label: &str) {
        let k = label.trim().to_lowercase();
        let mut st = self.state.lock().expect("lane state");
        if st.current.as_deref() == Some(k.as_str()) {
            st.current = None;
        }
        st.exhausted.remove(&k);
        st.cooldowns.retain(|(l, _), _| *l != k);
    }

    /// 合并各来源的候选：按邮箱去重，先到的赢，只把后来者的额度线索合并进去。
    /// **不看名单**——名单外的号也在，`snapshot` 要把它们列成「可添加」。
    fn all_candidates(&self) -> Vec<(Arc<dyn Source>, Candidate)> {
        let mut out: Vec<(Arc<dyn Source>, Candidate)> = Vec::new();
        for src in &self.sources {
            for mut c in src.candidates() {
                c.label = c.label.trim().to_lowercase();
                if c.label.is_empty() {
                    continue;
                }
                if let Some((_, existing)) = out.iter_mut().find(|(_, e)| e.label == c.label) {
                    if existing.quota == QuotaHint::default() {
                        existing.quota = c.quota;
                    }
                    continue;
                }
                out.push((src.clone(), c));
            }
        }
        out
    }

    /// 真进接力队的：名单里的那些。
    fn merged_candidates(&self) -> Vec<(Arc<dyn Source>, Candidate)> {
        self.all_candidates()
            .into_iter()
            .filter(|(_, c)| self.roster.contains(&c.label))
            .collect()
    }

    fn block_for(
        st: &State,
        c: &Candidate,
        model: &str,
        tried: &HashSet<String>,
        now: Instant,
    ) -> Option<Block> {
        if tried.contains(&c.label) {
            return Some(Block::Tried);
        }
        if let Some((_, reason)) = st.exhausted.get(&c.label) {
            return Some(Block::Exhausted(reason.clone()));
        }
        if c.quota.exhausted {
            return Some(Block::QuotaLine);
        }
        if let Some(cd) = st.cooldowns.get(&(c.label.clone(), model.to_string())) {
            return Some(Block::Cooled {
                kind: cd.kind,
                secs_left: cd.until.saturating_duration_since(now).as_secs(),
            });
        }
        None
    }

    /// 选号。规则见模块注释。
    fn pick(
        &self,
        cands: &[(Arc<dyn Source>, Candidate)],
        model: &str,
        tried: &HashSet<String>,
    ) -> Result<(Arc<dyn Source>, Candidate), UpstreamError> {
        let mut st = self.state.lock().expect("lane state");
        let now = Instant::now();
        st.sweep(now);

        let blocks: Vec<Option<Block>> = cands
            .iter()
            .map(|(_, c)| Self::block_for(&st, c, model, tried, now))
            .collect();
        let eligible: Vec<usize> = (0..cands.len()).filter(|i| blocks[*i].is_none()).collect();
        if eligible.is_empty() {
            return Err(no_account_error(cands, &blocks, model, &self.roster));
        }

        if let Some(cur) = st.current.clone() {
            if let Some(i) = eligible.iter().find(|i| cands[**i].1.label == cur) {
                return Ok(cands[*i].clone());
            }
            // 当前号只是对这个模型冷却中：绕行，不换当前号。
            let cur_block = cands
                .iter()
                .position(|(_, c)| c.label == cur)
                .and_then(|i| blocks[i].clone());
            if let Some(Block::Cooled { kind, secs_left }) = cur_block {
                let alt = cands[eligible[0]].clone();
                tracing::info!(
                    current = %cur, detour = %alt.1.label, model, kind = kind.as_str(), secs_left,
                    "当前号对此模型冷却中，本次绕行"
                );
                return Ok(alt);
            }
            // 当前号耗尽 / 不在了 → 往下接力。
        }

        let next = cands[eligible[0]].clone();
        tracing::info!(from = ?st.current, to = %next.1.label, source = next.0.name(), "接力");
        st.current = Some(next.1.label.clone());
        Ok(next)
    }

    pub fn snapshot(&self) -> LaneSnapshot {
        let all = self.all_candidates();
        let (cands, spare): (Vec<_>, Vec<_>) = all
            .into_iter()
            .partition(|(_, c)| self.roster.contains(&c.label));
        // 名单里有、但此刻没有任何来源给出凭证的：退出了登录、丢了 refresh_token、被从号池删了。
        // 它们仍然要出现在界面上——用户明明加过，列表却短了一行，那比一个「不可用」标签吓人。
        let missing: Vec<String> = self
            .roster
            .list()
            .into_iter()
            .filter(|m| !cands.iter().any(|(_, c)| c.label == *m))
            .collect();
        let available = spare
            .iter()
            .map(|(src, c)| AvailableView {
                label: c.label.clone(),
                source: src.name(),
                pinned: c.pinned_machine_id.is_some(),
                percent_used: c.quota.percent_used,
            })
            .collect();

        let mut st = self.state.lock().expect("lane state");
        let now = Instant::now();
        st.sweep(now);
        let current = st.current.clone();
        let candidates = cands
            .iter()
            .map(|(src, c)| {
                let cooled: Vec<(String, u64)> = st
                    .cooldowns
                    .iter()
                    .filter(|((label, _), _)| *label == c.label)
                    .map(|((_, model), cd)| {
                        (
                            model.clone(),
                            cd.until.saturating_duration_since(now).as_secs(),
                        )
                    })
                    .collect();
                let state = if let Some((until, reason)) = st.exhausted.get(&c.label) {
                    CandidateState::Exhausted {
                        reason: reason.clone(),
                        retry_in_secs: until.saturating_duration_since(now).as_secs(),
                    }
                } else if c.quota.exhausted {
                    CandidateState::QuotaLine
                } else if !cooled.is_empty() {
                    CandidateState::Cooled {
                        models: cooled.iter().map(|(m, _)| m.clone()).collect(),
                        secs_left: cooled.iter().map(|(_, s)| *s).max().unwrap_or(0),
                    }
                } else if current.as_deref() == Some(c.label.as_str()) {
                    CandidateState::Current
                } else {
                    CandidateState::Ready
                };
                CandidateView {
                    label: c.label.clone(),
                    source: src.name(),
                    pinned: c.pinned_machine_id.is_some(),
                    stored_id: match &c.kind {
                        CandidateKind::Stored(id) => Some(id.as_str().to_string()),
                        CandidateKind::Subscription { id, .. } => Some(id.clone()),
                        CandidateKind::CursorLogin => None,
                    },
                    percent_used: c.quota.percent_used,
                    state,
                }
            })
            .collect();
        LaneSnapshot {
            current,
            candidates,
            missing,
            available,
        }
    }
}

fn no_account_error(
    cands: &[(Arc<dyn Source>, Candidate)],
    blocks: &[Option<Block>],
    model: &str,
    roster: &Roster,
) -> UpstreamError {
    if cands.is_empty() {
        // 两种「一个号都没有」要分开说：名单是空的，和名单里的号此刻都拿不到凭证，
        // 用户要做的事完全不同。
        if roster.is_open() {
            return UpstreamError::new(
                UpstreamKind::Upstream,
                503,
                "这条通道没有可用的账号：在「账号」对应的平台页签里授权登录一个，或把已有的号打开",
            );
        }
        if roster.is_empty() {
            return UpstreamError::new(
                UpstreamKind::Upstream,
                503,
                "网关号池是空的：在「本地网关」里把要接力的号加进来",
            );
        }
        return UpstreamError::new(
            UpstreamKind::Upstream,
            503,
            format!(
                "网关号池里的 {} 个号此刻都拿不到凭证：Cursor 已不是它们在登录，「账号」里的也没有可用的 refresh_token",
                roster.list().len()
            ),
        );
    }
    let reasons: Vec<String> = cands
        .iter()
        .zip(blocks)
        .map(|((_, c), b)| {
            let why = match b {
                Some(Block::Exhausted(r)) => format!("已耗尽（{r}）"),
                Some(Block::QuotaLine) => "用量快照显示额度到线".to_string(),
                Some(Block::Cooled { kind, secs_left }) => {
                    format!("对 {model} {}，{secs_left} 秒后再试", kind.as_str())
                }
                Some(Block::Tried) => "本次已试过".to_string(),
                None => "可用".to_string(),
            };
            format!("{} {why}", c.label)
        })
        .collect();
    UpstreamError::new(
        UpstreamKind::Upstream,
        503,
        format!(
            "没有可用的账号（共 {} 个）：{}",
            cands.len(),
            reasons.join("；")
        ),
    )
}

impl Lane for RelayLane {
    fn acquire<'a>(&'a self, model: &'a str) -> BoxFuture<'a, Result<Credential, UpstreamError>> {
        Box::pin(async move {
            let cands = self.merged_candidates();
            let mut tried: HashSet<String> = HashSet::new();
            loop {
                let (source, cand) = self.pick(&cands, model, &tried)?;
                match source.resolve(&cand).await {
                    Ok(token) => {
                        let identity = match &cand.pinned_machine_id {
                            Some(m) => DeviceIdentity::pinned(&token.access_token, m.clone()),
                            None => DeviceIdentity::derived(&token.access_token),
                        };
                        return Ok(Credential {
                            label: cand.label,
                            access_token: token.access_token,
                            identity,
                        });
                    }
                    Err(err) => {
                        // 拿不到 token 的号这轮别再碰；是账号自身的问题就记成耗尽，下次也别碰。
                        tracing::warn!(account = %cand.label, kind = err.kind.as_str(), "取凭证失败，换下一个");
                        tried.insert(cand.label.clone());
                        if err.kind.blames_account() {
                            self.state.lock().expect("lane state").exhausted.insert(
                                cand.label.clone(),
                                (Instant::now() + self.exhausted_ttl, err.message.clone()),
                            );
                        }
                        if tried.len() >= cands.len() {
                            return Err(err);
                        }
                    }
                }
            }
        })
    }

    fn acquire_label<'a>(
        &'a self,
        label: &'a str,
    ) -> BoxFuture<'a, Result<Credential, UpstreamError>> {
        Box::pin(async move {
            let key = label.trim().to_lowercase();
            // 不看名单、不看耗尽：这是「查我之前提交的任务」，号即便已经到线也该能查。
            let Some((source, cand)) = self
                .all_candidates()
                .into_iter()
                .find(|(_, c)| c.label == key)
            else {
                return Err(UpstreamError::new(
                    UpstreamKind::Upstream,
                    404,
                    format!("账号 {label} 已不在这条通道里"),
                ));
            };
            let token = source.resolve(&cand).await?;
            let identity = match &cand.pinned_machine_id {
                Some(m) => DeviceIdentity::pinned(&token.access_token, m.clone()),
                None => DeviceIdentity::derived(&token.access_token),
            };
            Ok(Credential {
                label: cand.label,
                access_token: token.access_token,
                identity,
            })
        })
    }

    fn report(&self, credential: &Credential, model: &str, outcome: Outcome<'_>) {
        let key = credential.label.trim().to_lowercase();
        let mut st = self.state.lock().expect("lane state");
        match outcome {
            Outcome::Ok(_) => {
                // 第一次成功就钉住；成功了也说明这对（号, 模型）不再冷却。
                st.current.get_or_insert(key.clone());
                st.cooldowns.remove(&(key, model.to_string()));
            }
            Outcome::Err(e) => match e.kind {
                UpstreamKind::Quota | UpstreamKind::Auth | UpstreamKind::Forbidden => {
                    tracing::warn!(account = %key, kind = e.kind.as_str(), "号耗尽 / 失效，下次接力");
                    st.exhausted.insert(
                        key,
                        (Instant::now() + self.exhausted_ttl, e.message.clone()),
                    );
                }
                UpstreamKind::RateLimit => {
                    // 上游说了几点恢复就听它的（ChatGPT 的窗口限额会给）；没说按固定时长。
                    // 上限一天：一个离谱的重置时刻不该把号钉死。
                    let until = match e.reset_at_ms {
                        Some(at) => {
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as i64)
                                .unwrap_or(0);
                            let wait =
                                Duration::from_millis((at - now_ms).clamp(0, 86_400_000) as u64);
                            Instant::now() + wait.max(Duration::from_secs(30))
                        }
                        None => Instant::now() + self.rate_limit_cooldown,
                    };
                    st.cooldowns.insert(
                        (key, model.to_string()),
                        Cooldown {
                            until,
                            kind: e.kind,
                        },
                    );
                }
                UpstreamKind::ModelUnsupported => {
                    st.cooldowns.insert(
                        (key, model.to_string()),
                        Cooldown {
                            until: Instant::now() + self.model_unsupported_cooldown,
                            kind: e.kind,
                        },
                    );
                }
                // 供应商抖动 / 请求本身的问题 / 超时 / 取消：不怪号。
                _ => {}
            },
        }
    }
}

/// 给界面看的全貌。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaneSnapshot {
    pub current: Option<String>,
    /// 名单里、此刻有来源给出凭证的号，按接力顺序。
    pub candidates: Vec<CandidateView>,
    /// 名单里、但此刻没有任何来源给出凭证的号（小写邮箱）。
    pub missing: Vec<String>,
    /// 有来源、但没进名单的号——「添加」弹窗列的就是它们。
    pub available: Vec<AvailableView>,
}

/// 名单外的一个号：够界面认出它是谁、从哪来。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableView {
    pub label: String,
    pub source: &'static str,
    pub pinned: bool,
    pub percent_used: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CandidateView {
    pub label: String,
    pub source: &'static str,
    /// 用的是真机码（Cursor 正登着的号）。
    pub pinned: bool,
    /// 在「我的账号」里的 id；Cursor 登录号没有。
    pub stored_id: Option<String>,
    pub percent_used: Option<f64>,
    pub state: CandidateState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CandidateState {
    Ready,
    Current,
    Exhausted { reason: String, retry_in_secs: u64 },
    QuotaLine,
    Cooled { models: Vec<String>, secs_left: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lane::sources::ResolvedToken;
    use crate::normalized::Usage;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 假来源：固定候选；resolve 按标签给 token，或者按脚本失败。
    struct FakeSource {
        name: &'static str,
        candidates: Mutex<Vec<Candidate>>,
        fail: Mutex<HashMap<String, UpstreamError>>,
        resolves: AtomicUsize,
    }

    impl FakeSource {
        fn new(name: &'static str, labels: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                name,
                candidates: Mutex::new(
                    labels
                        .iter()
                        .map(|l| Candidate {
                            label: l.to_string(),
                            kind: CandidateKind::CursorLogin,
                            pinned_machine_id: None,
                            quota: QuotaHint::default(),
                        })
                        .collect(),
                ),
                fail: Mutex::new(HashMap::new()),
                resolves: AtomicUsize::new(0),
            })
        }
    }

    impl Source for FakeSource {
        fn name(&self) -> &'static str {
            self.name
        }
        fn candidates(&self) -> Vec<Candidate> {
            self.candidates.lock().unwrap().clone()
        }
        fn resolve<'a>(
            &'a self,
            c: &'a Candidate,
        ) -> BoxFuture<'a, Result<ResolvedToken, UpstreamError>> {
            Box::pin(async move {
                self.resolves.fetch_add(1, Ordering::SeqCst);
                if let Some(e) = self.fail.lock().unwrap().get(&c.label) {
                    return Err(e.clone());
                }
                Ok(ResolvedToken {
                    access_token: format!("tok-{}", c.label),
                    expires_at: None,
                })
            })
        }
    }

    fn err(kind: UpstreamKind, status: u16) -> UpstreamError {
        UpstreamError::new(kind, status, format!("{} happened", kind.as_str()))
    }

    fn usage() -> Usage {
        Usage::default()
    }

    /// 把各来源此刻的号全部放进名单——这些测试考的是接力规则，不是名单。
    fn lane_of(sources: Vec<Arc<dyn Source>>) -> RelayLane {
        let labels: Vec<String> = sources
            .iter()
            .flat_map(|s| s.candidates())
            .map(|c| c.label)
            .collect();
        RelayLane::new(sources, Arc::new(Roster::in_memory(&labels)))
    }

    #[tokio::test]
    async fn only_enrolled_accounts_are_picked_or_listed_and_the_rest_are_available() {
        let src = FakeSource::new("fake", &["a@x.com", "b@x.com", "c@x.com"]);
        let roster = Arc::new(Roster::in_memory(&["B@x.com", "gone@x.com"]));
        let lane = RelayLane::new(vec![src], roster.clone());

        assert_eq!(
            lane.acquire("m").await.unwrap().label,
            "b@x.com",
            "只在名单里挑"
        );
        let snap = lane.snapshot();
        assert_eq!(snap.candidates.len(), 1);
        assert_eq!(snap.candidates[0].label, "b@x.com");
        assert_eq!(
            snap.missing,
            vec!["gone@x.com".to_string()],
            "名单里有、来源里没有的号要说出来"
        );
        let avail: Vec<&str> = snap.available.iter().map(|a| a.label.as_str()).collect();
        assert_eq!(avail, vec!["a@x.com", "c@x.com"], "名单外的号列成可添加");

        // 加进来立刻生效，不用重建 lane。
        roster.add("a@x.com").unwrap();
        assert_eq!(lane.snapshot().candidates.len(), 2);
        // 移出当前号：接力到还在名单里的那个，状态也忘掉。
        lane.report(
            &lane.acquire("m").await.unwrap(),
            "m",
            Outcome::Err(&err(UpstreamKind::RateLimit, 429)),
        );
        roster.remove("b@x.com").unwrap();
        lane.forget("b@x.com");
        assert_eq!(lane.snapshot().current, None, "移出的号不再是当前号");
        assert_eq!(lane.acquire("m").await.unwrap().label, "a@x.com");
        roster.add("b@x.com").unwrap();
        assert!(
            !matches!(
                lane.snapshot()
                    .candidates
                    .iter()
                    .find(|c| c.label == "b@x.com")
                    .unwrap()
                    .state,
                CandidateState::Cooled { .. }
            ),
            "加回来是干净状态"
        );
    }

    #[tokio::test]
    async fn an_empty_roster_says_so_instead_of_blaming_login_state() {
        let src = FakeSource::new("fake", &["a@x.com"]);
        let lane = RelayLane::new(vec![src], Arc::new(Roster::in_memory::<&str>(&[])));
        let e = lane.acquire("m").await.unwrap_err();
        assert_eq!(e.status, 503);
        assert!(e.message.contains("号池是空的"), "{}", e.message);
        assert_eq!(lane.snapshot().available.len(), 1);
    }

    #[tokio::test]
    async fn sticks_to_the_first_account_that_works() {
        let src = FakeSource::new("fake", &["A@x.com", "b@x.com"]);
        let lane = lane_of(vec![src]);
        let c1 = lane.acquire("m").await.unwrap();
        assert_eq!(c1.label, "a@x.com", "标签统一成小写");
        assert_eq!(c1.access_token, "tok-a@x.com");
        lane.report(&c1, "m", Outcome::Ok(&usage()));
        let c2 = lane.acquire("m").await.unwrap();
        let c3 = lane.acquire("other-model").await.unwrap();
        assert_eq!(c2.label, "a@x.com");
        assert_eq!(c3.label, "a@x.com", "换模型也粘住同一个号");
        assert_eq!(lane.snapshot().current.as_deref(), Some("a@x.com"));
    }

    #[tokio::test]
    async fn quota_exhaustion_relays_to_the_next_account_and_makes_it_current() {
        let src = FakeSource::new("fake", &["a@x.com", "b@x.com", "c@x.com"]);
        let lane = lane_of(vec![src]);
        let a = lane.acquire("m").await.unwrap();
        lane.report(&a, "m", Outcome::Err(&err(UpstreamKind::Quota, 402)));
        let b = lane.acquire("m").await.unwrap();
        assert_eq!(b.label, "b@x.com");
        assert_eq!(
            lane.snapshot().current.as_deref(),
            Some("b@x.com"),
            "接力后当前号变了"
        );
        // a 耗尽了，任何模型都不会再回到它。
        assert_eq!(lane.acquire("z").await.unwrap().label, "b@x.com");
        let snap = lane.snapshot();
        assert!(matches!(
            snap.candidates[0].state,
            CandidateState::Exhausted { .. }
        ));
        assert_eq!(snap.candidates[1].state, CandidateState::Current);
        assert_eq!(snap.candidates[2].state, CandidateState::Ready);
    }

    #[tokio::test]
    async fn auth_and_forbidden_also_count_as_exhausted() {
        for kind in [UpstreamKind::Auth, UpstreamKind::Forbidden] {
            let src = FakeSource::new("fake", &["a@x.com", "b@x.com"]);
            let lane = lane_of(vec![src]);
            let a = lane.acquire("m").await.unwrap();
            lane.report(&a, "m", Outcome::Err(&err(kind, 401)));
            assert_eq!(lane.acquire("m").await.unwrap().label, "b@x.com");
        }
    }

    #[tokio::test]
    async fn rate_limit_detours_for_that_model_only_and_keeps_the_current_account() {
        let src = FakeSource::new("fake", &["a@x.com", "b@x.com"]);
        let lane = lane_of(vec![src]);
        let a = lane.acquire("claude").await.unwrap();
        lane.report(&a, "claude", Outcome::Ok(&usage()));
        lane.report(
            &a,
            "claude",
            Outcome::Err(&err(UpstreamKind::RateLimit, 429)),
        );
        assert_eq!(
            lane.acquire("claude").await.unwrap().label,
            "b@x.com",
            "这个模型绕行"
        );
        assert_eq!(
            lane.acquire("auto").await.unwrap().label,
            "a@x.com",
            "别的模型照旧用当前号"
        );
        assert_eq!(
            lane.snapshot().current.as_deref(),
            Some("a@x.com"),
            "当前号没变"
        );
        let snap = lane.snapshot();
        assert!(matches!(
            &snap.candidates[0].state,
            CandidateState::Cooled { models, .. } if models == &vec!["claude".to_string()]
        ));
    }

    #[tokio::test]
    async fn a_success_clears_that_models_cooldown() {
        let src = FakeSource::new("fake", &["a@x.com", "b@x.com"]);
        let lane = lane_of(vec![src]);
        let a = lane.acquire("m").await.unwrap();
        lane.report(&a, "m", Outcome::Err(&err(UpstreamKind::RateLimit, 429)));
        assert_eq!(lane.acquire("m").await.unwrap().label, "b@x.com");
        // 外部又用 a 对 m 成功了（比如手动 set_current 后）：冷却解除。
        lane.report(&a, "m", Outcome::Ok(&usage()));
        assert_eq!(lane.acquire("m").await.unwrap().label, "a@x.com");
    }

    #[tokio::test]
    async fn cooldowns_and_exhaustion_expire_on_their_own() {
        let src = FakeSource::new("fake", &["a@x.com", "b@x.com"]);
        let lane = lane_of(vec![src]).with_timings(
            Duration::from_millis(20),
            Duration::from_millis(20),
            Duration::from_millis(20),
        );
        let a = lane.acquire("m").await.unwrap();
        lane.report(&a, "m", Outcome::Err(&err(UpstreamKind::Quota, 402)));
        assert_eq!(lane.acquire("m").await.unwrap().label, "b@x.com");
        tokio::time::sleep(Duration::from_millis(30)).await;
        // a 的耗尽到点了，但当前号已经是 b 且 b 还好，继续 b——接力不回头。
        assert_eq!(lane.acquire("m").await.unwrap().label, "b@x.com");
        // 直到 b 也不行了，a 作为「又可用了」的候选被重新接上。
        let b = lane.acquire("m").await.unwrap();
        lane.report(&b, "m", Outcome::Err(&err(UpstreamKind::Quota, 402)));
        assert_eq!(lane.acquire("m").await.unwrap().label, "a@x.com");
    }

    #[tokio::test]
    async fn provider_and_bad_request_errors_do_not_blame_the_account() {
        let src = FakeSource::new("fake", &["a@x.com", "b@x.com"]);
        let lane = lane_of(vec![src]);
        let a = lane.acquire("m").await.unwrap();
        for kind in [
            UpstreamKind::Provider,
            UpstreamKind::BadRequest,
            UpstreamKind::Timeout,
            UpstreamKind::Upstream,
            UpstreamKind::Canceled,
        ] {
            lane.report(&a, "m", Outcome::Err(&err(kind, 500)));
        }
        assert_eq!(lane.acquire("m").await.unwrap().label, "a@x.com");
    }

    #[tokio::test]
    async fn resolve_failure_falls_through_to_the_next_candidate_and_marks_account_faults() {
        let src = FakeSource::new("fake", &["a@x.com", "b@x.com"]);
        src.fail
            .lock()
            .unwrap()
            .insert("a@x.com".into(), err(UpstreamKind::Auth, 401));
        let lane = lane_of(vec![src.clone()]);
        let c = lane.acquire("m").await.unwrap();
        assert_eq!(c.label, "b@x.com");
        assert_eq!(src.resolves.load(Ordering::SeqCst), 2);
        let snap = lane.snapshot();
        assert!(
            matches!(snap.candidates[0].state, CandidateState::Exhausted { .. }),
            "刷不出 token 的号记成耗尽"
        );
        // 下一次直接跳过 a，不再白试。
        lane.acquire("m").await.unwrap();
        assert_eq!(src.resolves.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn when_every_candidate_fails_the_last_error_comes_back() {
        let src = FakeSource::new("fake", &["a@x.com"]);
        src.fail
            .lock()
            .unwrap()
            .insert("a@x.com".into(), err(UpstreamKind::Upstream, 502));
        let lane = lane_of(vec![src]);
        let e = lane.acquire("m").await.unwrap_err();
        assert_eq!(e.kind, UpstreamKind::Upstream);
        assert_eq!(e.status, 502);
    }

    #[tokio::test]
    async fn enrolled_but_absent_accounts_are_a_clear_503() {
        // 名单里有号，但来源一个都给不出来（Cursor 登出了、refresh 丢了）。
        let lane = RelayLane::new(
            vec![FakeSource::new("fake", &[])],
            Arc::new(Roster::in_memory(&["a@x.com"])),
        );
        let e = lane.acquire("m").await.unwrap_err();
        assert_eq!(e.status, 503);
        assert!(
            e.message.contains("1 个号此刻都拿不到凭证"),
            "{}",
            e.message
        );
        assert_eq!(lane.snapshot().missing, vec!["a@x.com".to_string()]);
    }

    #[tokio::test]
    async fn all_blocked_explains_each_account() {
        let src = FakeSource::new("fake", &["a@x.com", "b@x.com"]);
        let lane = lane_of(vec![src]);
        let a = lane.acquire("m").await.unwrap();
        lane.report(&a, "m", Outcome::Err(&err(UpstreamKind::Quota, 402)));
        let b = lane.acquire("m").await.unwrap();
        lane.report(&b, "m", Outcome::Err(&err(UpstreamKind::RateLimit, 429)));
        let e = lane.acquire("m").await.unwrap_err();
        assert_eq!(e.status, 503);
        assert!(e.message.contains("a@x.com 已耗尽"), "{}", e.message);
        assert!(
            e.message.contains("b@x.com 对 m rate_limit"),
            "{}",
            e.message
        );
        // 换个模型 b 就能用。
        assert_eq!(lane.acquire("other").await.unwrap().label, "b@x.com");
    }

    #[tokio::test]
    async fn quota_hint_from_a_snapshot_skips_the_account_proactively() {
        let src = FakeSource::new("fake", &["a@x.com", "b@x.com"]);
        src.candidates.lock().unwrap()[0].quota = QuotaHint {
            percent_used: Some(100.0),
            exhausted: true,
        };
        let lane = lane_of(vec![src]);
        assert_eq!(lane.acquire("m").await.unwrap().label, "b@x.com");
        assert_eq!(
            lane.snapshot().candidates[0].state,
            CandidateState::QuotaLine
        );
    }

    #[tokio::test]
    async fn the_same_account_from_two_sources_is_deduped_with_the_first_source_winning() {
        let login = FakeSource::new("cursor_login", &["Me@x.com"]);
        login.candidates.lock().unwrap()[0].pinned_machine_id = Some("m".repeat(64));
        let stored = FakeSource::new("stored", &["me@x.com", "other@x.com"]);
        stored.candidates.lock().unwrap()[0].quota = QuotaHint {
            percent_used: Some(40.0),
            exhausted: false,
        };
        let lane = lane_of(vec![login.clone(), stored.clone()]);
        let snap = lane.snapshot();
        assert_eq!(snap.candidates.len(), 2, "同一个号只出现一次");
        assert_eq!(snap.candidates[0].source, "cursor_login");
        assert!(
            snap.candidates[0].pinned,
            "以 Cursor 登录那份为准（真机码）"
        );
        assert_eq!(
            snap.candidates[0].percent_used,
            Some(40.0),
            "但用量线索从托管那份合并过来"
        );
        let c = lane.acquire("m").await.unwrap();
        assert_eq!(c.access_token, "tok-me@x.com");
        assert!(c.identity.machine_id == "m".repeat(64), "凭证用的是真机码");
        assert_eq!(login.resolves.load(Ordering::SeqCst), 1);
        assert_eq!(stored.resolves.load(Ordering::SeqCst), 0, "没去碰托管那份");
    }

    #[tokio::test]
    async fn reset_and_set_current_are_honoured() {
        let src = FakeSource::new("fake", &["a@x.com", "b@x.com"]);
        let lane = lane_of(vec![src]);
        let a = lane.acquire("m").await.unwrap();
        lane.report(&a, "m", Outcome::Err(&err(UpstreamKind::Quota, 402)));
        assert_eq!(lane.acquire("m").await.unwrap().label, "b@x.com");
        lane.reset();
        lane.set_current("A@x.com");
        assert_eq!(lane.acquire("m").await.unwrap().label, "a@x.com");
    }

    #[test]
    fn snapshot_serialises_for_the_ui() {
        let v = serde_json::to_value(CandidateState::Cooled {
            models: vec!["m".into()],
            secs_left: 3,
        })
        .unwrap();
        assert_eq!(v["kind"], "cooled");
        assert_eq!(v["secsLeft"], 3, "字段按前端习惯 camelCase");
        assert_eq!(
            serde_json::to_value(CandidateState::Current).unwrap()["kind"],
            "current"
        );
    }
}
