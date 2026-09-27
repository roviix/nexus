//! 会让接入失效的环境变量。
//!
//! 配置文件写得再对，shell 里一行 `export` 就能把它架空：`CLAUDE_CONFIG_DIR` 让 Claude Code 去读
//! 另一份 settings.json，`CODEX_HOME` 让 Codex 读另一个目录，终端里残留的 `ANTHROPIC_API_KEY`
//! 和配置里的口令撞在一起……这些是「明明接入了却不生效」时最难查的原因，所以接入页把它们摆出来。
//!
//! 看两处：这个进程继承到的环境（从终端启动 Nexus 时就是终端的环境），和常见 shell 启动文件里的
//! `export`（从 Dock / 开始菜单启动时进程里看不到它们，但用户在终端里跑客户端时会生效）。
//! 只报、不改：删用户的 shell 配置不是一个接入工具该做的事。

use crate::Tool;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvConflict {
    pub name: String,
    /// 值（像钥匙的只留开头几位）。
    pub value: String,
    /// `环境变量` 或 `~/.zshrc:12`。
    pub source: String,
    /// 它会怎么影响这个客户端。
    pub effect: String,
}

enum Rule {
    Exact(&'static str, &'static str),
    Prefix(&'static str, &'static str),
}

impl Rule {
    fn matches(&self, name: &str) -> Option<&'static str> {
        match self {
            Rule::Exact(n, effect) => (name == *n).then_some(*effect),
            Rule::Prefix(p, effect) => name.starts_with(p).then_some(*effect),
        }
    }
}

fn rules(tool: Tool) -> &'static [Rule] {
    match tool {
        Tool::ClaudeCode => &[
            Rule::Exact(
                "CLAUDE_CONFIG_DIR",
                "Claude Code 会去读这个目录里的 settings.json，接入写的那份不生效。",
            ),
            Rule::Exact(
                "CLAUDE_CODE_USE_BEDROCK",
                "Claude Code 会改走 AWS Bedrock，不经过接入配的地址。",
            ),
            Rule::Exact(
                "CLAUDE_CODE_USE_VERTEX",
                "Claude Code 会改走 Google Vertex，不经过接入配的地址。",
            ),
            Rule::Exact(
                "ANTHROPIC_API_KEY",
                "和配置里的口令同时在时，Claude Code 可能拿它认证，或者弹窗问用哪一把。",
            ),
            Rule::Exact(
                "ANTHROPIC_MODEL",
                "Claude Code 的默认模型会变成它；网关仍按档名认，但名字里没有档位时走主模型。",
            ),
            Rule::Prefix(
                "ANTHROPIC_",
                "终端里也设了这个变量，和配置里的值不一样时容易查不清到底用了哪个。",
            ),
        ],
        Tool::Codex => &[Rule::Exact(
            "CODEX_HOME",
            "Codex 会去读这个目录里的 config.toml，接入写的那份不生效。",
        )],
        Tool::OpenCode => &[
            Rule::Exact(
                "OPENCODE_CONFIG",
                "OpenCode 会先读这个文件，接入写的 opencode.json 可能被它盖掉。",
            ),
            Rule::Exact(
                "OPENCODE_CONFIG_DIR",
                "OpenCode 会去读这个目录里的配置，接入写的那份可能不生效。",
            ),
        ],
        Tool::Grok => &[Rule::Exact(
            "GROK_HOME",
            "Grok CLI 会去读这个目录里的 config.toml，接入写的那份不生效。",
        )],
    }
}

/// 像钥匙的值只留开头几位。
fn mask(name: &str, value: &str) -> String {
    let v = value.trim();
    let secret = ["KEY", "TOKEN", "SECRET", "PASSWORD"]
        .iter()
        .any(|w| name.contains(w));
    if secret {
        let head: String = v.chars().take(6).collect();
        return if v.chars().count() > 6 {
            format!("{head}…")
        } else {
            "••••".into()
        };
    }
    if v.chars().count() > 80 {
        return v.chars().take(80).collect::<String>() + "…";
    }
    v.to_string()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit())
}

/// 一行 shell 里的变量赋值：`export A=b`、`A=b`、`setenv A b`、fish 的 `set -gx A b`。
fn parse_assignment(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let unquote = |v: &str| -> String {
        let v = v.trim();
        let v = v.split(" #").next().unwrap_or(v).trim();
        v.trim_matches('"').trim_matches('\'').to_string()
    };
    if let Some(rest) = line.strip_prefix("set ") {
        // fish：set -gx NAME value / set --export NAME value
        let mut parts = rest.split_whitespace().peekable();
        let mut exported = false;
        while let Some(p) = parts.peek() {
            if p.starts_with('-') {
                exported |= p.contains('x') || *p == "--export";
                parts.next();
            } else {
                break;
            }
        }
        let name = parts.next()?;
        if !exported || !valid_name(name) {
            return None;
        }
        let value = parts.collect::<Vec<_>>().join(" ");
        return Some((name.to_string(), unquote(&value)));
    }
    if let Some(rest) = line.strip_prefix("setenv ") {
        let (name, value) = rest.trim().split_once(char::is_whitespace)?;
        return valid_name(name).then(|| (name.to_string(), unquote(value)));
    }
    let rest = line.strip_prefix("export ").unwrap_or(line);
    let (name, value) = rest.split_once('=')?;
    let name = name.trim();
    valid_name(name).then(|| (name.to_string(), unquote(value)))
}

const SHELL_FILES: &[&str] = &[
    ".zshenv",
    ".zprofile",
    ".zshrc",
    ".bash_profile",
    ".bashrc",
    ".profile",
    ".config/fish/config.fish",
];

/// 这个客户端会被哪些环境变量影响。`home` 是用户目录（shell 启动文件在它下面）。
pub fn env_conflicts(tool: Tool, home: &Path) -> Vec<EnvConflict> {
    scan(tool, home, std::env::vars())
}

fn scan(tool: Tool, home: &Path, vars: impl Iterator<Item = (String, String)>) -> Vec<EnvConflict> {
    let rules = rules(tool);
    let effect_of = |name: &str| rules.iter().find_map(|r| r.matches(name));
    let mut out: Vec<EnvConflict> = Vec::new();
    for (name, value) in vars {
        if let Some(effect) = effect_of(&name) {
            out.push(EnvConflict {
                value: mask(&name, &value),
                name,
                source: "环境变量".into(),
                effect: effect.into(),
            });
        }
    }
    for rel in SHELL_FILES {
        let path = home.join(rel);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let Some((name, value)) = parse_assignment(line) else {
                continue;
            };
            if let Some(effect) = effect_of(&name) {
                out.push(EnvConflict {
                    value: mask(&name, &value),
                    name,
                    source: format!("~/{rel}:{}", i + 1),
                    effect: effect.into(),
                });
            }
        }
    }
    out.truncate(20);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_assignments_in_several_dialects() {
        assert_eq!(
            parse_assignment("export CODEX_HOME=\"$HOME/.codex-work\""),
            Some(("CODEX_HOME".into(), "$HOME/.codex-work".into()))
        );
        assert_eq!(
            parse_assignment("ANTHROPIC_API_KEY='sk-ant-123' # work key"),
            Some(("ANTHROPIC_API_KEY".into(), "sk-ant-123".into()))
        );
        assert_eq!(
            parse_assignment("set -gx CLAUDE_CONFIG_DIR ~/.claude-alt"),
            Some(("CLAUDE_CONFIG_DIR".into(), "~/.claude-alt".into()))
        );
        assert_eq!(
            parse_assignment("setenv GROK_HOME /tmp/g"),
            Some(("GROK_HOME".into(), "/tmp/g".into()))
        );
        assert_eq!(parse_assignment("# export CODEX_HOME=x"), None);
        assert_eq!(parse_assignment("set -g NOT_EXPORTED x"), None);
        assert_eq!(parse_assignment("alias ll='ls -l'"), None);
    }

    #[test]
    fn process_env_and_shell_files_are_both_reported_and_keys_masked() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join(".zshrc"),
            "export PATH=$PATH:/x\nexport ANTHROPIC_API_KEY=sk-ant-abcdefgh\nexport MY_ANTHROPIC_THING=1\n",
        )
        .unwrap();
        let vars = vec![
            ("CLAUDE_CONFIG_DIR".to_string(), "/tmp/alt".to_string()),
            ("HOME".to_string(), "/Users/x".to_string()),
        ];
        let found = scan(Tool::ClaudeCode, home.path(), vars.into_iter());
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].name, "CLAUDE_CONFIG_DIR");
        assert_eq!(found[0].source, "环境变量");
        assert!(found[0].effect.contains("settings.json"));
        assert_eq!(found[1].name, "ANTHROPIC_API_KEY");
        assert_eq!(found[1].source, "~/.zshrc:2");
        assert_eq!(found[1].value, "sk-ant…");
        // Codex 不关心 ANTHROPIC_*。
        assert!(scan(Tool::Codex, home.path(), std::iter::empty()).is_empty());
    }
}
