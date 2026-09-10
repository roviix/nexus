//! `nexus-connect` —— 一键接入：把客户端（Claude Code / Codex）的配置文件直接改到中转 API 上。
//!
//! 接入页原来只给「复制」：用户抄一段 JSON / TOML 回自己的配置文件里。桌面应用明明就在这台
//! 机器上、拿得到那些文件，让人手抄是把最容易错的一步留给了人。这个 crate 替他改：
//!
//! - **只动我们那几个键**，其余一字不改（`claude.rs` / `codex.rs`）；文件本来就坏的（不是合法
//!   JSON / TOML）**拒绝改**，让人自己看；
//! - **动之前先备份**到 `~/.roviix/backups/clients/<tool>/`，并记一份清单；
//! - **可撤销**：按清单把备份拷回去；文件是我们建的就删掉；清单丢了就退回「只删我们的键」。
//!
//! 它只认三个字串 —— 地址、钥匙、模型 —— 从哪个号源来、钥匙怎么取，是 Tauri 层的事；
//! 这里不依赖 gateway / shop。写进去的键与前端 `relay/snippets.ts` 里「复制」给出的一致。

mod claude;
mod codex;
mod fsx;
mod grok;
mod opencode;

use nexus_core::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tool {
    /// `~/.claude/settings.json`
    ClaudeCode,
    /// `~/.codex/config.toml` + `~/.codex/auth.json`
    Codex,
    /// `~/.config/opencode/opencode.json`
    OpenCode,
    /// `~/.grok/config.toml`
    Grok,
}

impl Tool {
    pub fn id(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "claude",
            Tool::Codex => "codex",
            Tool::OpenCode => "opencode",
            Tool::Grok => "grok",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "Claude Code",
            Tool::Codex => "Codex CLI",
            Tool::OpenCode => "OpenCode",
            Tool::Grok => "Grok CLI",
        }
    }

    /// 前端用的 id（`relay/snippets.ts` 的 `Tool`）。
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "claude" => Some(Tool::ClaudeCode),
            "codex" => Some(Tool::Codex),
            "opencode" => Some(Tool::OpenCode),
            "grok" => Some(Tool::Grok),
            _ => None,
        }
    }
}

/// 要接到哪：地址（带不带 `/v1` 都行，这里会归一）、钥匙、模型。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl Target {
    /// 不带 `/v1` 的根地址：Anthropic 协议填这个。
    pub fn root_url(&self) -> String {
        self.base_url
            .trim()
            .trim_end_matches('/')
            .trim_end_matches("/v1")
            .trim_end_matches('/')
            .to_string()
    }

    /// 带 `/v1`：OpenAI / Responses 协议填这个。
    pub fn v1_url(&self) -> String {
        format!("{}/v1", self.root_url())
    }
}

/// 文件都在哪。`home` 是用户目录；`backups` 是 `~/.roviix/backups/clients`。
#[derive(Debug, Clone)]
pub struct Layout {
    pub home: PathBuf,
    pub backups: PathBuf,
}

impl Layout {
    pub fn new(home: impl Into<PathBuf>, backups: impl Into<PathBuf>) -> Self {
        Self {
            home: home.into(),
            backups: backups.into(),
        }
    }

    pub fn files(&self, tool: Tool) -> Vec<PathBuf> {
        match tool {
            Tool::ClaudeCode => vec![self.home.join(".claude").join("settings.json")],
            Tool::Codex => vec![
                self.home.join(".codex").join("config.toml"),
                self.home.join(".codex").join("auth.json"),
            ],
            Tool::OpenCode => vec![self
                .home
                .join(".config")
                .join("opencode")
                .join("opencode.json")],
            Tool::Grok => vec![self.home.join(".grok").join("config.toml")],
        }
    }

    fn backup_dir(&self, tool: Tool) -> PathBuf {
        self.backups.join(tool.id())
    }

    fn manifest_path(&self, tool: Tool) -> PathBuf {
        self.backups.join(format!("{}.json", tool.id()))
    }
}

/// 某个工具此刻的配置指向哪。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    /// 主配置文件（Claude 的 settings.json / Codex 的 config.toml）。
    pub path: String,
    pub exists: bool,
    /// 文件里配的中转地址；没配、或配的不是我们能认的形状时为 `None`。
    pub base_url: Option<String>,
    pub model: Option<String>,
    /// 有我们留下的接入清单 —— 也就是「可以撤销」。
    pub revertible: bool,
    /// 上次一键接入的时刻（清单里的）。
    pub applied_at: Option<String>,
}

/// 一次接入写了哪些文件。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Applied {
    pub files: Vec<AppliedFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AppliedFile {
    pub path: String,
    /// 之前没有这个文件，是我们建的。
    pub created: bool,
    /// 改之前拷的那份。
    pub backup: Option<String>,
}

/// 撤销的结果：哪些文件还原了、哪些删了。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Reverted {
    pub restored: Vec<String>,
    pub removed: Vec<String>,
    /// 没有清单、也没有备份，只删掉了我们的键。
    pub stripped: Vec<String>,
}

/// 落在备份目录里的接入清单：撤销靠它。**只在第一次接入时写**——连续接入两次，第二次的
/// 「改之前」已经是我们的东西了，拿它当备份撤销等于撤了个寂寞。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    at: String,
    files: Vec<AppliedFile>,
}

fn read_manifest(layout: &Layout, tool: Tool) -> Option<Manifest> {
    let text = std::fs::read_to_string(layout.manifest_path(tool)).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_manifest(layout: &Layout, tool: Tool, m: &Manifest) -> Result<()> {
    let text = serde_json::to_string_pretty(m)
        .map_err(|e| AppError::internal(format!("接入清单序列化失败：{e}")))?;
    fsx::write_atomic(&layout.manifest_path(tool), &text)
}

fn display(p: &Path) -> String {
    p.display().to_string()
}

pub fn inspect(layout: &Layout, tool: Tool) -> Result<Inspection> {
    let files = layout.files(tool);
    let main = &files[0];
    let text = fsx::read_opt(main)?;
    let (base_url, model) = match tool {
        Tool::ClaudeCode => claude::inspect(text.as_deref()),
        Tool::Codex => codex::inspect_config(text.as_deref()),
        Tool::OpenCode => opencode::inspect(text.as_deref()),
        Tool::Grok => grok::inspect(text.as_deref()),
    };
    let manifest = read_manifest(layout, tool);
    Ok(Inspection {
        path: display(main),
        exists: text.is_some(),
        base_url,
        model,
        revertible: manifest.is_some(),
        applied_at: manifest.map(|m| m.at),
    })
}

/// 接入：备份 → 合并 → 原子写。任一文件失败就停在那儿（已写的文件留着——它们各自完整，
/// 且备份都在清单里，撤销能收回来）。
pub fn apply(layout: &Layout, tool: Tool, target: &Target) -> Result<Applied> {
    let mut written = Vec::new();
    // 已经有清单 = 之前接过：备份沿用第一次的，不再拿「我们自己写的」当备份。
    let first_time = read_manifest(layout, tool).is_none();
    let mut manifest_files = Vec::new();

    for path in layout.files(tool) {
        let existing = fsx::read_opt(&path)?;
        let merged = match (tool, path.file_name().and_then(|s| s.to_str())) {
            (Tool::ClaudeCode, _) => claude::merge(existing.as_deref(), target)?,
            (Tool::Codex, Some("auth.json")) => codex::merge_auth(existing.as_deref(), target)?,
            (Tool::Codex, _) => codex::merge_config(existing.as_deref(), target)?,
            (Tool::OpenCode, _) => opencode::merge(existing.as_deref(), target)?,
            (Tool::Grok, _) => grok::merge(existing.as_deref(), target)?,
        };
        let backup = if first_time {
            fsx::backup(&path, &layout.backup_dir(tool))?
        } else {
            None
        };
        fsx::write_atomic(&path, &merged)?;
        let file = AppliedFile {
            path: display(&path),
            created: existing.is_none(),
            backup: backup.as_deref().map(display),
        };
        manifest_files.push(file.clone());
        written.push(file);
    }

    if first_time {
        write_manifest(
            layout,
            tool,
            &Manifest {
                at: nexus_core::now_iso(),
                files: manifest_files,
            },
        )?;
    }
    Ok(Applied { files: written })
}

/// 撤销接入。有清单：按清单还原 / 删除；没清单：把我们的键从文件里剔掉。
pub fn revert(layout: &Layout, tool: Tool) -> Result<Reverted> {
    let mut out = Reverted {
        restored: vec![],
        removed: vec![],
        stripped: vec![],
    };
    match read_manifest(layout, tool) {
        Some(m) => {
            for f in &m.files {
                let path = PathBuf::from(&f.path);
                match &f.backup {
                    Some(b) if Path::new(b).is_file() => {
                        fsx::restore(Path::new(b), &path)?;
                        out.restored.push(f.path.clone());
                    }
                    _ if f.created => {
                        fsx::remove_if_exists(&path)?;
                        out.removed.push(f.path.clone());
                    }
                    _ => {
                        // 有过原文件、备份却不在了：退回剔键。
                        strip_file(tool, &path, &mut out)?;
                    }
                }
            }
            fsx::remove_if_exists(&layout.manifest_path(tool))?;
        }
        None => {
            for path in layout.files(tool) {
                strip_file(tool, &path, &mut out)?;
            }
        }
    }
    Ok(out)
}

fn strip_file(tool: Tool, path: &Path, out: &mut Reverted) -> Result<()> {
    let Some(existing) = fsx::read_opt(path)? else {
        return Ok(());
    };
    let stripped = match (tool, path.file_name().and_then(|s| s.to_str())) {
        (Tool::ClaudeCode, _) => claude::strip(&existing)?,
        (Tool::Codex, Some("auth.json")) => codex::strip_auth(&existing)?,
        (Tool::Codex, _) => codex::strip_config(&existing)?,
        (Tool::OpenCode, _) => opencode::strip(&existing)?,
        (Tool::Grok, _) => grok::strip(&existing)?,
    };
    match stripped {
        Some(text) => {
            fsx::write_atomic(path, &text)?;
            out.stripped.push(display(path));
        }
        None => {
            fsx::remove_if_exists(path)?;
            out.removed.push(display(path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layout() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let l = Layout::new(dir.path().join("home"), dir.path().join("backups"));
        (dir, l)
    }

    fn target(url: &str) -> Target {
        Target {
            base_url: url.into(),
            api_key: "nx-secret".into(),
            model: "claude-sonnet-5".into(),
        }
    }

    #[test]
    fn target_urls_are_normalised() {
        let t = target("http://127.0.0.1:8787/v1/");
        assert_eq!(t.root_url(), "http://127.0.0.1:8787");
        assert_eq!(t.v1_url(), "http://127.0.0.1:8787/v1");
        assert_eq!(
            target("https://relay.example.com").v1_url(),
            "https://relay.example.com/v1"
        );
    }

    #[test]
    fn apply_on_a_fresh_machine_creates_files_and_revert_removes_them() {
        let (_d, l) = layout();
        let before = inspect(&l, Tool::ClaudeCode).unwrap();
        assert!(!before.exists);
        assert!(!before.revertible);

        let applied = apply(&l, Tool::ClaudeCode, &target("http://127.0.0.1:8787")).unwrap();
        assert_eq!(applied.files.len(), 1);
        assert!(applied.files[0].created);
        assert!(applied.files[0].backup.is_none());

        let after = inspect(&l, Tool::ClaudeCode).unwrap();
        assert!(after.exists);
        assert_eq!(after.base_url.as_deref(), Some("http://127.0.0.1:8787"));
        assert_eq!(after.model.as_deref(), Some("claude-sonnet-5"));
        assert!(after.revertible);
        assert!(after.applied_at.is_some());

        let r = revert(&l, Tool::ClaudeCode).unwrap();
        assert_eq!(r.removed.len(), 1);
        assert!(!l.home.join(".claude/settings.json").exists());
        assert!(!inspect(&l, Tool::ClaudeCode).unwrap().revertible);
    }

    #[test]
    fn apply_backs_up_an_existing_file_and_revert_restores_it_byte_for_byte() {
        let (_d, l) = layout();
        // 和 lib 里一样分段 join：Windows 上 `join(".codex/config.toml")` 会保留正斜杠，
        // 与 revert 回报的 `\` 路径字面不等。
        let path = l.home.join(".codex").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "# mine\nmodel_provider = \"openai\"\nmodel = \"gpt-4.1\"\n";
        std::fs::write(&path, original).unwrap();

        let applied = apply(&l, Tool::Codex, &target("http://127.0.0.1:8787")).unwrap();
        assert_eq!(applied.files.len(), 2);
        let cfg = &applied.files[0];
        assert!(!cfg.created);
        let backup = cfg.backup.as_ref().expect("原文件要有备份");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), original);
        assert!(applied.files[1].created, "auth.json 之前没有");

        let now = std::fs::read_to_string(&path).unwrap();
        assert!(now.starts_with("# mine"));
        assert!(now.contains("[model_providers.nexus]"));
        let insp = inspect(&l, Tool::Codex).unwrap();
        assert_eq!(insp.base_url.as_deref(), Some("http://127.0.0.1:8787/v1"));

        // 再接一次（换云端）：不再备份，清单仍是第一次的。
        let again = apply(&l, Tool::Codex, &target("https://relay.example.com")).unwrap();
        assert!(again.files.iter().all(|f| f.backup.is_none()));
        assert_eq!(
            inspect(&l, Tool::Codex).unwrap().base_url.as_deref(),
            Some("https://relay.example.com/v1")
        );

        let r = revert(&l, Tool::Codex).unwrap();
        assert_eq!(r.restored, vec![path.display().to_string()]);
        assert_eq!(r.removed.len(), 1, "我们建的 auth.json 该被删掉");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!l.home.join(".codex/auth.json").exists());
    }

    #[test]
    fn revert_without_a_manifest_only_strips_our_keys() {
        let (_d, l) = layout();
        let path = l.home.join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 像是用户照「复制」手抄进去的：没有我们的清单。
        std::fs::write(
            &path,
            r#"{"theme":"dark","env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:8787","ANTHROPIC_MODEL":"m","KEEP":"1"}}"#,
        )
        .unwrap();
        let r = revert(&l, Tool::ClaudeCode).unwrap();
        assert_eq!(r.stripped.len(), 1);
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["theme"], "dark");
        assert_eq!(v["env"]["KEEP"], "1");
        assert!(v["env"].get("ANTHROPIC_BASE_URL").is_none());
    }

    #[test]
    fn a_broken_config_is_left_untouched() {
        let (_d, l) = layout();
        let path = l.home.join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ definitely not json").unwrap();
        let err = apply(&l, Tool::ClaudeCode, &target("http://x")).unwrap_err();
        assert!(err.message.contains("不是合法的 JSON"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ definitely not json"
        );
        assert!(!l.backups.join("claude.json").exists(), "没写就不该留清单");
    }

    #[cfg(unix)]
    #[test]
    fn written_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, l) = layout();
        apply(&l, Tool::Codex, &target("http://127.0.0.1:8787")).unwrap();
        for f in l.files(Tool::Codex) {
            let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", f.display());
        }
    }
}
