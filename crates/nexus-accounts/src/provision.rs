//! 入库后的自动配置：把「铸 key → 换 session → 开按需 → 开数据保留 → 刷用量」编成一条流水线。
//!
//! 批量添加进来的号（邮箱 + 一把 access token）离「能派出去」还差几个动作，过去全靠人一个个
//! 点。这些动作的底层实现早就各自存在（[`crate::token::mint_user_api_key`]、[`crate::convert`]、
//! [`crate::usage::set_on_demand`]、[`crate::usage::set_data_retention_consent`]、
//! [`crate::usage::fetch`]），这个模块只管**次序、前置条件、失败隔离**三件事。
//!
//! 三条规矩：
//!
//! 1. **次序不能换。** 铸 `crsr_` Key 排第一——它是这个号的**保命绳**：只要在手上这把 token
//!    （哪怕是会死的 web token）还活着时铸出来，之后哪一步把 token 弄掉了，查用量 / 进网关都还有
//!    退路。换 session 排第二：它一成功这个号就有了 refresh，后面几步从此都能重做，但它要拿网站
//!    会话去官方登录端点走一遭、有失手的可能，所以放在铸 key 之后。刷用量排最后，卡片上看到的
//!    才是配置完成后的终态，而不是配置前的旧数。
//! 2. **一步失败不影响后面几步。** 「开按需」对 Apple 内购号和团队成员号会**正当地**失败
//!    （上游只让管理员改），为这个把整条流水线停掉，等于让一个号的策略问题拖累其余几步。
//! 3. **跳过要说出原因。** 已经有 key 的不重复铸、已经是 session 的不用转换——报告里写明
//!    「为什么没做」，否则用户对着一片空白只能怀疑压根没跑。
//!
//! 「开数据保留」是 Fable 5 的隐私开关（`set-user-no-zdr-model-consent`，2026-09-19 抓包接上），
//! 排在「刷用量」之前：它改的是上游状态，得让最后那次刷新在它之后跑。

use crate::model::Account;
use serde::{Deserialize, Serialize};

/// 这一批要做哪几件事。
///
/// 每一项都可以单独关掉，因为这几件事的成本和风险不一样：铸 key 会在对方 Dashboard 上留一条
/// 记录，换 session 要拿用户的网站会话去官方登录端点走一遭，而刷用量只是读。让用户能只勾其中
/// 一项，比给一个全或无的开关有用。
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionPlan {
    /// 铸一把长期 `crsr_` User API Key。
    pub mint_api_key: bool,
    /// web token → 桌面 session + refresh。
    pub convert_session: bool,
    /// 开按需计费。
    pub on_demand: bool,
    /// 按需的每月上限（美分）。`None` = 不封顶，也就是仪表盘上的 "No Limit"。
    pub on_demand_limit_cents: Option<f64>,
    /// 打开 Fable 5 的数据保留策略同意。
    pub data_retention: bool,
    /// 最后刷一遍用量。
    pub refresh_usage: bool,
}

impl Default for ProvisionPlan {
    /// 默认全做、按需不封顶——这正是日常批量入库想要的那一套。
    fn default() -> Self {
        Self {
            mint_api_key: true,
            convert_session: true,
            on_demand: true,
            on_demand_limit_cents: None,
            data_retention: true,
            refresh_usage: true,
        }
    }
}

impl ProvisionPlan {
    /// 计划里勾了的步骤，**按执行次序**。
    pub fn steps(&self) -> Vec<ProvisionStep> {
        ProvisionStep::ORDER
            .iter()
            .copied()
            .filter(|s| self.wants(*s))
            .collect()
    }

    pub fn wants(&self, step: ProvisionStep) -> bool {
        match step {
            ProvisionStep::MintApiKey => self.mint_api_key,
            ProvisionStep::ConvertSession => self.convert_session,
            ProvisionStep::OnDemand => self.on_demand,
            ProvisionStep::DataRetention => self.data_retention,
            ProvisionStep::RefreshUsage => self.refresh_usage,
        }
    }

    /// 一件事都没勾。调用方据此当场拒绝，而不是跑一圈回来报告一堆「没勾」。
    pub fn is_empty(&self) -> bool {
        self.steps().is_empty()
    }
}

/// 流水线上的一步。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProvisionStep {
    MintApiKey,
    ConvertSession,
    OnDemand,
    DataRetention,
    RefreshUsage,
}

impl ProvisionStep {
    /// 执行次序。**改这个数组就是改流水线的语义**，理由见模块头第 1 条。
    pub const ORDER: [ProvisionStep; 5] = [
        ProvisionStep::MintApiKey,
        ProvisionStep::ConvertSession,
        ProvisionStep::OnDemand,
        ProvisionStep::DataRetention,
        ProvisionStep::RefreshUsage,
    ];

    /// 给人看的名字。报告、活动日志、界面共用一套说法。
    pub fn label(self) -> &'static str {
        match self {
            ProvisionStep::MintApiKey => "铸 crsr_ Key",
            ProvisionStep::ConvertSession => "换桌面 session",
            ProvisionStep::OnDemand => "开按需计费",
            ProvisionStep::DataRetention => "开数据保留策略",
            ProvisionStep::RefreshUsage => "刷用量",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum StepState {
    Done,
    /// 前置条件不满足。**不是错误**——「已经有 key 了」和「铸失败了」要能分开看。
    Skipped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepReport {
    pub step: ProvisionStep,
    pub state: StepState,
    /// 跳过的原因，或上游的错误。`Done` 时缺席。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl StepReport {
    pub fn done(step: ProvisionStep) -> Self {
        Self {
            step,
            state: StepState::Done,
            message: None,
        }
    }

    pub fn skipped(step: ProvisionStep, reason: impl Into<String>) -> Self {
        Self {
            step,
            state: StepState::Skipped,
            message: Some(reason.into()),
        }
    }

    pub fn failed(step: ProvisionStep, message: impl Into<String>) -> Self {
        Self {
            step,
            state: StepState::Failed,
            message: Some(message.into()),
        }
    }
}

/// 一个号跑完一条流水线的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionReport {
    pub id: String,
    pub email: String,
    pub steps: Vec<StepReport>,
}

impl ProvisionReport {
    pub fn failed_count(&self) -> usize {
        self.count(StepState::Failed)
    }

    pub fn done_count(&self) -> usize {
        self.count(StepState::Done)
    }

    fn count(&self, state: StepState) -> usize {
        self.steps.iter().filter(|s| s.state == state).count()
    }

    /// 这个号算配好了：勾了的步骤里一个都没失败。
    ///
    /// 全部被跳过也算成功——「这个号本来就已经配好了」和「刚给它配好了」对用户是同一个
    /// 结论，没必要在结果栏里分成两栏。
    pub fn ok(&self) -> bool {
        self.failed_count() == 0
    }

    /// 给活动日志用的一行摘要。
    pub fn summary(&self) -> String {
        let failed: Vec<&str> = self
            .steps
            .iter()
            .filter(|s| s.state == StepState::Failed)
            .map(|s| s.step.label())
            .collect();
        match failed.is_empty() {
            true => format!("已自动配置（{} 步完成）", self.done_count()),
            false => format!("自动配置部分失败：{}", failed.join("、")),
        }
    }
}

/// 这一步对这个号该不该跑。`Err(原因)` = 跳过，原因会原样进报告。
///
/// 每一步执行**之前**都拿刚读出来的 `account` 问一次，不能开跑前一次算完：转换成功后号就
/// 有了 refresh，铸 key 成功后 `has_api_key` 变真，后面几步的前置条件跟着一起变。
pub fn decide(step: ProvisionStep, account: &Account) -> std::result::Result<(), String> {
    if account.status == crate::model::Status::Dead {
        return Err("号已失效".into());
    }

    match step {
        ProvisionStep::MintApiKey => {
            if account.has_api_key {
                return Err("已经有一把 crsr_ Key".into());
            }
            if !(account.has_refresh || account.has_live_access()) {
                return Err("此刻拿不出会话，铸不了".into());
            }
            Ok(())
        }

        // 只对「活着的 web-only」做转换。已经有 refresh 的不需要；已经是桌面 session 的
        // 不需要；type 读不出来的老行保守不动（`access_is_session` 的同一条理由：宁可少做
        // 一次，也不要拿一把不确定的 token 去换）。
        ProvisionStep::ConvertSession => {
            if account.has_refresh {
                return Err("已经有 refresh_token".into());
            }
            if !account.has_live_access() {
                return Err("没有还活着的 token，换不出来".into());
            }
            if account.access_is_session() {
                return Err("已经是桌面 session".into());
            }
            if !account.web_session_only() {
                return Err("这把 token 的类型读不出来，不动它".into());
            }
            Ok(())
        }

        // 按需是写操作，得有能打 dashboard 的会话（`crsr_` 兑出来的 api_key_token 不行）。
        // 已经是目标状态的不重复写：这条流水线会被反复跑在同一批号上。
        ProvisionStep::OnDemand => {
            if !(account.has_refresh || account.has_live_access()) {
                return Err("此刻拿不出会话，改不了".into());
            }
            if already_unlimited(account) {
                return Err("已经开着且不封顶".into());
            }
            Ok(())
        }

        // 数据保留也是 dashboard cookie 面的写操作，同样要一把会话。它幂等，不查旧状态、
        // 每次都写（见 [`crate::usage::set_data_retention_consent`]），所以没有「已配好」这一档。
        ProvisionStep::DataRetention => {
            if !(account.has_refresh || account.has_live_access()) {
                return Err("此刻拿不出会话，改不了".into());
            }
            Ok(())
        }

        ProvisionStep::RefreshUsage => match account.can_query_usage() {
            true => Ok(()),
            false => Err("没有可用于查用量的凭证".into()),
        },
    }
}

/// 按需已经开着、而且不封顶。
///
/// `on_demand_limit_cents` 是双层 `Option`：外层 `None` = 这次快照没读到（当作不知道，
/// 该写一次），`Some(None)` = 上游明确说不封顶。把两者混同会让一批从没查过用量的新号
/// 全被跳过。
fn already_unlimited(account: &Account) -> bool {
    let Some(usage) = account.usage.as_ref() else {
        return false;
    };
    usage.on_demand_enabled == Some(true) && usage.on_demand_limit_cents == Some(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Account, Availability, Status};
    use crate::usage::AccountUsage;

    /// 一个刚导入的 web-only 号：邮箱 + 一把还活着的 `type=web` token，别的都没有。
    fn web_only() -> Account {
        Account {
            id: nexus_core::AccountId::from_raw("a1"),
            email: "a@example.com".into(),
            source: crate::model::Source::Local,
            status: Status::Active,
            note: None,
            tags: Vec::new(),
            membership: None,
            signup_type: None,
            workos_user_id: Some("user_1".into()),
            usage: None,
            billing: None,
            last_checked_at: None,
            last_error: None,
            code_channel: "auto".into(),
            code_channel_resolved: None,
            last_code_at: None,
            has_refresh: false,
            has_access: true,
            access_expires_at: Some("2099-01-01T00:00:00Z".into()),
            access_token_type: Some("web".into()),
            has_password: false,
            has_email_password: false,
            has_recovery_email: false,
            has_api_key: false,
            created_at: "2026-09-19T00:00:00Z".into(),
            updated_at: "2026-09-19T00:00:00Z".into(),
            seq: 1,
            archived_at: None,
            availability: Availability::Session,
        }
    }

    #[test]
    fn a_freshly_imported_web_only_account_runs_every_step() {
        let a = web_only();
        for step in ProvisionStep::ORDER {
            assert_eq!(decide(step, &a), Ok(()), "{step:?} 该跑");
        }
    }

    #[test]
    fn conversion_is_skipped_once_the_account_has_a_desktop_session() {
        let mut a = web_only();
        a.access_token_type = Some("session".into());
        assert_eq!(
            decide(ProvisionStep::ConvertSession, &a),
            Err("已经是桌面 session".into())
        );

        // 有 refresh 的号更不用转换——它随时能换一把新鲜 session。
        let mut a = web_only();
        a.has_refresh = true;
        assert_eq!(
            decide(ProvisionStep::ConvertSession, &a),
            Err("已经有 refresh_token".into())
        );
    }

    #[test]
    fn an_expired_token_cannot_be_converted_but_a_crsr_key_can_still_read_usage() {
        let mut a = web_only();
        a.access_expires_at = Some("2020-01-01T00:00:00Z".into());
        a.has_api_key = true;

        assert!(decide(ProvisionStep::ConvertSession, &a).is_err());
        // 铸 key、改按需都要一把活会话，过期了就都做不了。
        assert!(decide(ProvisionStep::MintApiKey, &a).is_err());
        assert!(decide(ProvisionStep::OnDemand, &a).is_err());
        // 但 crsr_ 还能拉逐条花费，所以最后那次刷新仍然有意义。
        assert_eq!(decide(ProvisionStep::RefreshUsage, &a), Ok(()));
    }

    #[test]
    fn minting_is_skipped_when_a_key_is_already_there() {
        let mut a = web_only();
        a.has_api_key = true;
        assert_eq!(
            decide(ProvisionStep::MintApiKey, &a),
            Err("已经有一把 crsr_ Key".into())
        );
    }

    #[test]
    fn on_demand_is_written_unless_upstream_already_says_unlimited() {
        let mut a = web_only();

        // 从没查过用量：不知道 ≠ 已经不封顶，该写一次。
        assert_eq!(decide(ProvisionStep::OnDemand, &a), Ok(()));

        // 开着但有上限：仍要写，目标是不封顶。
        a.usage = Some(AccountUsage {
            on_demand_enabled: Some(true),
            on_demand_limit_cents: Some(Some(5_000.0)),
            ..Default::default()
        });
        assert_eq!(decide(ProvisionStep::OnDemand, &a), Ok(()));

        // 开着且不封顶：这才是目标状态，跳过。
        a.usage = Some(AccountUsage {
            on_demand_enabled: Some(true),
            on_demand_limit_cents: Some(None),
            ..Default::default()
        });
        assert_eq!(
            decide(ProvisionStep::OnDemand, &a),
            Err("已经开着且不封顶".into())
        );
    }

    #[test]
    fn a_dead_account_is_skipped_at_every_step() {
        let mut a = web_only();
        a.status = Status::Dead;
        for step in ProvisionStep::ORDER {
            assert_eq!(decide(step, &a), Err("号已失效".into()), "{step:?}");
        }
    }

    #[test]
    fn the_plan_keeps_the_pipeline_order_no_matter_how_it_was_built() {
        let plan = ProvisionPlan {
            mint_api_key: true,
            convert_session: false,
            on_demand: true,
            on_demand_limit_cents: None,
            data_retention: true,
            refresh_usage: true,
        };
        assert_eq!(
            plan.steps(),
            vec![
                ProvisionStep::MintApiKey,
                ProvisionStep::OnDemand,
                ProvisionStep::DataRetention,
                ProvisionStep::RefreshUsage
            ]
        );
        assert!(!plan.is_empty());

        // 铸 key 排在换 session 之前——保命绳先系上。
        assert_eq!(ProvisionStep::ORDER[0], ProvisionStep::MintApiKey);
        assert_eq!(ProvisionStep::ORDER[1], ProvisionStep::ConvertSession);

        assert_eq!(ProvisionPlan::default().steps().len(), 5);
        assert!(ProvisionPlan {
            mint_api_key: false,
            convert_session: false,
            on_demand: false,
            on_demand_limit_cents: None,
            data_retention: false,
            refresh_usage: false,
        }
        .is_empty());
    }

    #[test]
    fn data_retention_only_needs_a_session_and_never_claims_already_done() {
        let a = web_only();
        // web-only 也能写：它是 cookie 面的写操作，web token 的 cookie 一样认。
        assert_eq!(decide(ProvisionStep::DataRetention, &a), Ok(()));

        // 没有会话就跳过。
        let mut dead_token = web_only();
        dead_token.access_expires_at = Some("2020-01-01T00:00:00Z".into());
        assert!(decide(ProvisionStep::DataRetention, &dead_token).is_err());
    }

    #[test]
    fn a_report_is_ok_when_nothing_failed_even_if_everything_was_skipped() {
        let all_skipped = ProvisionReport {
            id: "a1".into(),
            email: "a@example.com".into(),
            steps: ProvisionStep::ORDER
                .iter()
                .map(|s| StepReport::skipped(*s, "已经配好了"))
                .collect(),
        };
        assert!(all_skipped.ok());
        assert_eq!(all_skipped.done_count(), 0);

        let one_failed = ProvisionReport {
            id: "a1".into(),
            email: "a@example.com".into(),
            steps: vec![
                StepReport::done(ProvisionStep::ConvertSession),
                StepReport::failed(ProvisionStep::OnDemand, "只有管理员能改"),
            ],
        };
        assert!(!one_failed.ok());
        assert_eq!(one_failed.failed_count(), 1);
        assert!(one_failed.summary().contains("开按需计费"));
    }

    #[test]
    fn the_report_serializes_in_the_shape_the_ui_reads() {
        let report = ProvisionReport {
            id: "a1".into(),
            email: "a@example.com".into(),
            steps: vec![
                StepReport::done(ProvisionStep::ConvertSession),
                StepReport::skipped(ProvisionStep::MintApiKey, "已经有一把 crsr_ Key"),
            ],
        };
        let v = serde_json::to_value(&report).unwrap();
        assert_eq!(v["steps"][0]["step"], "convertSession");
        assert_eq!(v["steps"][0]["state"], "done");
        assert!(v["steps"][0].get("message").is_none(), "成功不带消息");
        assert_eq!(v["steps"][1]["state"], "skipped");
        assert_eq!(v["steps"][1]["message"], "已经有一把 crsr_ Key");
    }
}
