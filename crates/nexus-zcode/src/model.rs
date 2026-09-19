//! ZCode 账号领域模型。表里没有秘密。

use nexus_core::ZcodeAccountId;
use serde::{Deserialize, Serialize};

/// 服务商。同一套协议，两个域名、两套 biz API。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZcodeProvider {
    Zai,
    Bigmodel,
}

impl ZcodeProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            ZcodeProvider::Zai => "zai",
            ZcodeProvider::Bigmodel => "bigmodel",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "bigmodel" => ZcodeProvider::Bigmodel,
            _ => ZcodeProvider::Zai,
        }
    }

    pub fn display(self) -> &'static str {
        match self {
            ZcodeProvider::Zai => "Z.AI",
            ZcodeProvider::Bigmodel => "智谱",
        }
    }
}

/// 套餐档。决定上游地址、认证方式，以及要不要过验证码那道门。
///
/// 两档的请求体格式完全一样（都是 Anthropic Messages）——差别只在地址、认证头，
/// 以及 start-plan 那两道反滥用门禁（每请求验证码 + 强制系统提示词）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ZcodePlan {
    /// 编码套餐：付费订阅，凭证是永久 API key，直连服务商域名，没有门禁。
    CodingPlan,
    /// 体验套餐：免费额度（含活动领取的周末套餐），凭证是 OAuth JWT，
    /// 走 `zcode.z.ai` 的 zcode-plan 网关，每个请求都要验证码。
    StartPlan,
}

impl ZcodePlan {
    pub fn as_str(self) -> &'static str {
        match self {
            ZcodePlan::CodingPlan => "coding-plan",
            ZcodePlan::StartPlan => "start-plan",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw.trim() {
            "start-plan" => ZcodePlan::StartPlan,
            _ => ZcodePlan::CodingPlan,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ZcodeStatus {
    Active,
    NeedsLogin,
    Dead,
}

impl ZcodeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ZcodeStatus::Active => "active",
            ZcodeStatus::NeedsLogin => "needs_login",
            ZcodeStatus::Dead => "dead",
        }
    }

    pub fn parse(raw: &str) -> Self {
        match raw {
            "active" => ZcodeStatus::Active,
            "dead" => ZcodeStatus::Dead,
            _ => ZcodeStatus::NeedsLogin,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZcodeAccount {
    pub id: ZcodeAccountId,
    /// 稳定身份，导入时用来判重。coding 是 `{provider}:{family}:{uuid}`，
    /// start 是 `{provider}:start-plan:{user_id}`。
    pub account_ref: String,
    /// 给人看的名字，也是接力队里的键。
    ///
    /// 存成字段而不是让各处自己拼：它同时是网关接力队的键和界面上的标题，
    /// 前端再推导一遍就会有对不上的那天（`channel_forget` 拿着错的键什么也忘不掉）。
    pub label: String,
    pub provider: ZcodeProvider,
    pub plan: ZcodePlan,
    /// 官方客户端里的套餐族名，如 `zai-individual-coding-plan`。只用于展示。
    pub family: Option<String>,
    pub email: Option<String>,
    pub status: ZcodeStatus,
    pub enabled: bool,
    pub note: Option<String>,
    /// API key 的前几位，给人认哪张是哪张。不是秘密。
    pub key_hint: Option<String>,
    pub has_api_key: bool,
    pub has_jwt: bool,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// 拼一个号的显示名。同一个邮箱下的个人版 / 团队版 / 体验套餐是三条号，
/// 所以邮箱后面必须带上档位 —— 只有邮箱的话三条长得一模一样，接力队的键就撞了。
pub fn compute_label(
    email: Option<&str>,
    family: Option<&str>,
    plan: ZcodePlan,
    account_ref: &str,
) -> String {
    if let Some(email) = email.map(str::trim).filter(|e| !e.is_empty()) {
        let suffix = family
            .and_then(family_short)
            .unwrap_or_else(|| plan_short(plan));
        return format!("{email} · {suffix}");
    }
    if let Some(family) = family.map(str::trim).filter(|f| !f.is_empty()) {
        return family.to_string();
    }
    format!("zcode…{}", tail(account_ref, 6))
}

fn plan_short(plan: ZcodePlan) -> &'static str {
    match plan {
        ZcodePlan::CodingPlan => "编码套餐",
        ZcodePlan::StartPlan => "体验套餐",
    }
}

/// `zai-individual-coding-plan` → `个人版`。认不出就交给档位名。
fn family_short(family: &str) -> Option<&'static str> {
    let f = family.to_ascii_lowercase();
    if f.contains("individual") {
        Some("个人版")
    } else if f.contains("team") {
        Some("团队版")
    } else {
        None
    }
}

impl ZcodeAccount {
    pub fn label(&self) -> String {
        self.label.clone()
    }

    /// 这个号此刻拿不拿得出凭证。coding 看 api key，start 看 JWT。
    pub fn has_credential(&self) -> bool {
        match self.plan {
            ZcodePlan::CodingPlan => self.has_api_key,
            ZcodePlan::StartPlan => self.has_jwt,
        }
    }
}

fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        return s.to_string();
    }
    chars[chars.len() - n..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_round_trips_through_the_wire_name() {
        for p in [ZcodePlan::CodingPlan, ZcodePlan::StartPlan] {
            assert_eq!(ZcodePlan::parse(p.as_str()), p);
        }
        // 认不出的档位落回 coding-plan，而不是把用户送进要验证码的那条路。
        assert_eq!(ZcodePlan::parse("nonsense"), ZcodePlan::CodingPlan);
    }

    #[test]
    fn a_label_tells_individual_from_team_under_one_email() {
        // 两条号只有套餐族不同。标签必须分得开——它是接力队的键。
        let indiv = compute_label(
            Some("me@x.com"),
            Some("zai-individual-coding-plan"),
            ZcodePlan::CodingPlan,
            "r1",
        );
        let team = compute_label(
            Some("me@x.com"),
            Some("zai-team-coding-plan"),
            ZcodePlan::CodingPlan,
            "r2",
        );
        assert_eq!(indiv, "me@x.com · 个人版");
        assert_eq!(team, "me@x.com · 团队版");
        assert_ne!(indiv, team);
    }

    #[test]
    fn a_label_without_an_email_falls_back_to_family_then_to_the_ref() {
        assert_eq!(
            compute_label(
                None,
                Some("zai-team-coding-plan"),
                ZcodePlan::CodingPlan,
                "r"
            ),
            "zai-team-coding-plan"
        );
        assert_eq!(
            compute_label(
                None,
                None,
                ZcodePlan::StartPlan,
                "zai:start-plan:abcdef123456"
            ),
            "zcode…123456"
        );
        // 体验套餐没有 family，走档位名。
        assert_eq!(
            compute_label(Some("me@x.com"), None, ZcodePlan::StartPlan, "r"),
            "me@x.com · 体验套餐"
        );
    }

    #[test]
    fn credential_presence_follows_the_plan() {
        let mut a = ZcodeAccount {
            id: ZcodeAccountId::from_raw("x"),
            account_ref: "zai:coding:1".into(),
            label: "me@x.com · 个人版".into(),
            provider: ZcodeProvider::Zai,
            plan: ZcodePlan::CodingPlan,
            family: None,
            email: None,
            status: ZcodeStatus::Active,
            enabled: true,
            note: None,
            key_hint: None,
            has_api_key: true,
            has_jwt: false,
            last_checked_at: None,
            last_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!(a.has_credential());
        a.plan = ZcodePlan::StartPlan;
        assert!(!a.has_credential(), "体验套餐要的是 JWT，不是 api key");
    }
}
