//! 一键接入：把 Claude Code / Codex 的配置文件直接改到中转 API 上。
//!
//! 这一层只做「解出地址和钥匙」——本地网关的地址与口令——然后交给 `nexus-connect`
//! 去改文件。**钥匙从头到尾不经过前端**：接入页上点「一键接入」，前端只交工具名、模型，
//! 拿回的是写了哪些文件。这比「复制再粘贴」少一次明文在剪贴板里走过场。

use crate::state::AppState;
use nexus_connect::{Applied, Inspection, Layout, Reverted, Target, Tool};
use nexus_core::{AppError, Result};
use nexus_store::activity;
use serde::Serialize;
use tauri::State;

fn layout(state: &AppState) -> Layout {
    Layout::new(state.home_dir.clone(), state.client_backups_dir.clone())
}

fn tool_of(id: &str) -> Result<Tool> {
    Tool::parse(id).ok_or_else(|| {
        AppError::invalid(format!("「{id}」没有可以直接写的配置文件。"))
            .with_hint("只有 Claude Code、Codex、OpenCode、Grok CLI 有配置文件；其余工具照配置抄进设置面板即可。")
    })
}

/// 配置现在指向哪里。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PointsTo {
    /// 本机回环地址：本地网关。
    Local,
    /// 配了别的地址（官方直连、别家中转）。
    Other,
    /// 没配 / 文件不存在 / 用的不是我们认得的 provider。
    None,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientState {
    #[serde(flatten)]
    pub inspection: Inspection,
    pub points_to: PointsTo,
}

fn classify(base_url: Option<&str>) -> PointsTo {
    let Some(url) = base_url.map(str::trim).filter(|s| !s.is_empty()) else {
        return PointsTo::None;
    };
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("http://127.0.0.1")
        || lower.starts_with("http://localhost")
        || lower.starts_with("http://[::1]")
    {
        return PointsTo::Local;
    }
    PointsTo::Other
}

/// 某个工具的配置此刻是什么状态。不碰文件内容以外的任何东西，不弹窗、不写。
#[tauri::command(async)]
pub fn connect_inspect(state: State<'_, AppState>, tool: String) -> Result<ClientState> {
    let tool = tool_of(&tool)?;
    let inspection = nexus_connect::inspect(&layout(&state), tool)?;
    let points_to = classify(inspection.base_url.as_deref());
    Ok(ClientState {
        inspection,
        points_to,
    })
}

/// 解出要写进配置的三样：地址、钥匙、模型。
///
/// **不要求网关正在运行**——接入是「把客户端指过来」，网关开不开是另一件事；地址
/// 用运行中的那个，没开就用设置里的端口。口令没有就顺手生成一把（`api_key()` 会存起来，
/// 之后网关起来用的也是它）。
fn target_for(state: &AppState, model: &str) -> Result<Target> {
    let model = model.trim();
    if model.is_empty() {
        return Err(AppError::invalid("先选一个模型。"));
    }
    let status = state.gateway.status()?;
    let base_url = status
        .running
        .map(|r| r.base_url)
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", status.settings.port));
    Ok(Target {
        base_url,
        api_key: state.gateway.api_key()?,
        model: model.to_string(),
    })
}

/// 把这个工具接到本地网关上：备份 → 合并 → 写。返回写了哪些文件、哪些有备份。
#[tauri::command(async)]
pub fn connect_apply(state: State<'_, AppState>, tool: String, model: String) -> Result<Applied> {
    let tool = tool_of(&tool)?;
    let target = target_for(&state, &model)?;
    let applied = nexus_connect::apply(&layout(&state), tool, &target)?;
    activity::info(
        &state.db,
        "connect",
        None,
        format!(
            "已把 {} 接到本地网关（{} · {}）",
            tool.label(),
            model.trim(),
            applied
                .files
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>()
                .join("、")
        ),
    );
    Ok(applied)
}

/// 撤销接入：按清单把备份拷回去；文件是我们建的就删；没清单就只剔掉我们的键。
#[tauri::command(async)]
pub fn connect_revert(state: State<'_, AppState>, tool: String) -> Result<Reverted> {
    let tool = tool_of(&tool)?;
    let reverted = nexus_connect::revert(&layout(&state), tool)?;
    activity::info(
        &state.db,
        "connect",
        None,
        format!(
            "已撤销 {} 的接入（还原 {} · 删除 {} · 剔键 {}）",
            tool.label(),
            reverted.restored.len(),
            reverted.removed.len(),
            reverted.stripped.len()
        ),
    );
    Ok(reverted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_tells_local_and_other_apart() {
        assert_eq!(classify(Some("http://127.0.0.1:8787")), PointsTo::Local);
        assert_eq!(classify(Some("http://localhost:9000/v1")), PointsTo::Local);
        assert_eq!(classify(Some("https://api.anthropic.com")), PointsTo::Other);
        assert_eq!(classify(None), PointsTo::None);
        assert_eq!(classify(Some("  ")), PointsTo::None);
    }

    #[test]
    fn unknown_tools_are_refused_with_a_hint() {
        let err = tool_of("cline").unwrap_err();
        assert!(err.hint.is_some());
        assert_eq!(tool_of("claude").unwrap(), Tool::ClaudeCode);
        assert_eq!(tool_of("codex").unwrap(), Tool::Codex);
    }
}
