//! TOML 配置解析

use std::collections::HashSet;
use std::path::Path;

use deapbox_core::types::{
    AgentConfig, AgentId, AgentKind, AppConfig, ChatId, ChatSession, LarkConfig, WorkspacePath,
};
use serde::Deserialize;

// ============ TOML 序列化类型 ============

#[derive(Debug, Deserialize)]
struct RawConfig {
    lark: RawLarkConfig,
    #[serde(default)]
    agents: Vec<RawAgentConfig>,
    sessions: Option<Vec<RawSessionBinding>>,
}

#[derive(Debug, Deserialize)]
struct RawLarkConfig {
    app_id: String,
    app_secret: String,
}

#[derive(Debug, Deserialize)]
struct RawAgentConfig {
    id: String,
    kind: String,
    command: String,
    args: Option<Vec<String>>,
    env_vars: Option<std::collections::HashMap<String, String>>,
}

#[derive(Debug, Deserialize)]
struct RawSessionBinding {
    chat_id: String,
    agent_id: String,
    workspace: String,
}

// ============ 解析逻辑 ============

impl RawConfig {
    fn into_domain(self) -> Result<AppConfig, ConfigError> {
        let mut agent_ids = HashSet::new();
        let agents: Vec<AgentConfig> = self
            .agents
            .into_iter()
            .map(|a| {
                if !agent_ids.insert(a.id.clone()) {
                    return Err(ConfigError::DuplicateAgentId(a.id));
                }
                if a.command.trim().is_empty() {
                    return Err(ConfigError::EmptyCommand(a.id));
                }
                let kind = match a.kind.to_lowercase().as_str() {
                    "opencode" => AgentKind::Opencode,
                    "codex" => AgentKind::Codex,
                    "claude-code" | "claudecode" => AgentKind::ClaudeCode,
                    "kimi-code" | "kimicode" => AgentKind::KimiCode,
                    other => return Err(ConfigError::UnknownAgentKind(other.to_string())),
                };
                Ok(AgentConfig {
                    id: AgentId(a.id),
                    kind,
                    command: a.command,
                    args: a.args.unwrap_or_default(),
                    env_vars: a.env_vars.unwrap_or_default(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let known_agent_ids: HashSet<AgentId> = agents.iter().map(|a| a.id.clone()).collect();
        let sessions = self
            .sessions
            .unwrap_or_default()
            .into_iter()
            .map(|s| {
                let agent_id = AgentId(s.agent_id);
                if !known_agent_ids.contains(&agent_id) {
                    return Err(ConfigError::UnknownSessionAgent(agent_id.0));
                }
                if s.workspace.trim().is_empty() {
                    return Err(ConfigError::EmptyWorkspace(s.chat_id));
                }
                Ok(ChatSession {
                    chat_id: ChatId(s.chat_id),
                    agent_id,
                    workspace: WorkspacePath(s.workspace.into()),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(AppConfig {
            lark: LarkConfig {
                app_id: self.lark.app_id,
                app_secret: self.lark.app_secret,
            },
            agents,
            sessions,
        })
    }
}

// ============ 公开 API ============

/// 从 TOML 文件加载配置
pub fn load_config<P: AsRef<Path>>(path: P) -> Result<AppConfig, ConfigError> {
    let content =
        std::fs::read_to_string(path.as_ref()).map_err(|e| ConfigError::Io(e.to_string()))?;
    let raw: RawConfig = toml::from_str(&content).map_err(|e| ConfigError::Parse(e.to_string()))?;
    raw.into_domain()
}

// ============ 错误类型 ============

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(String),
    #[error("TOML parse error: {0}")]
    Parse(String),
    #[error("Unknown agent kind: {0}")]
    UnknownAgentKind(String),
    #[error("Duplicate agent id: {0}")]
    DuplicateAgentId(String),
    #[error("Empty command for agent: {0}")]
    EmptyCommand(String),
    #[error("Session references unknown agent: {0}")]
    UnknownSessionAgent(String),
    #[error("Empty workspace for chat: {0}")]
    EmptyWorkspace(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_minimal_config() {
        let toml = r#"
[lark]
app_id = "cli_xxx"
app_secret = "secret_xxx"

[[agents]]
id = "opencode-main"
kind = "opencode"
command = "opencode"
"#;

        let raw: RawConfig = toml::from_str(toml).unwrap();
        let config = raw.into_domain().unwrap();
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.agents[0].id.0, "opencode-main");
        assert_eq!(config.agents[0].kind, AgentKind::Opencode);
    }

    #[test]
    fn test_parse_lark_only_config_without_agents() {
        let toml = r#"
[lark]
app_id = "cli_xxx"
app_secret = "secret_xxx"
"#;

        let raw: RawConfig = toml::from_str(toml).unwrap();
        let config = raw.into_domain().unwrap();
        assert_eq!(config.agents.len(), 0);
        assert_eq!(config.sessions.len(), 0);
        assert_eq!(config.lark.app_id, "cli_xxx");
    }

    #[test]
    fn test_parse_full_config() {
        let toml = r#"
[lark]
app_id = "cli_xxx"
app_secret = "secret_xxx"

[[agents]]
id = "opencode-main"
kind = "opencode"
command = "opencode"
args = ["--model", "gpt-4o"]

[[agents]]
id = "codex-dev"
kind = "codex"
command = "codex"

[[sessions]]
chat_id = "oc_xxxxxxxx"
agent_id = "opencode-main"
workspace = "/tmp/workspace-1"
"#;

        let raw: RawConfig = toml::from_str(toml).unwrap();
        let config = raw.into_domain().unwrap();
        assert_eq!(config.agents.len(), 2);
        assert_eq!(config.sessions.len(), 1);
        assert_eq!(config.sessions[0].chat_id.0, "oc_xxxxxxxx");
    }

    #[test]
    fn test_unknown_agent_kind() {
        let toml = r#"
[lark]
app_id = "cli_xxx"
app_secret = "secret_xxx"

[[agents]]
id = "bad-agent"
kind = "unknown"
command = "whatever"
"#;
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let result = raw.into_domain();
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Unknown agent kind"));
    }

    #[test]
    fn test_duplicate_agent_id() {
        let toml = r#"
[lark]
app_id = "cli_xxx"
app_secret = "secret_xxx"

[[agents]]
id = "dup"
kind = "codex"
command = "codex"

[[agents]]
id = "dup"
kind = "opencode"
command = "opencode"
"#;
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let result = raw.into_domain();
        assert!(matches!(result, Err(ConfigError::DuplicateAgentId(id)) if id == "dup"));
    }

    #[test]
    fn test_empty_agent_command() {
        let toml = r#"
[lark]
app_id = "cli_xxx"
app_secret = "secret_xxx"

[[agents]]
id = "codex-dev"
kind = "codex"
command = " "
"#;
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let result = raw.into_domain();
        assert!(matches!(result, Err(ConfigError::EmptyCommand(id)) if id == "codex-dev"));
    }

    #[test]
    fn test_session_references_unknown_agent() {
        let toml = r#"
[lark]
app_id = "cli_xxx"
app_secret = "secret_xxx"

[[agents]]
id = "codex-dev"
kind = "codex"
command = "codex"

[[sessions]]
chat_id = "oc_xxxxxxxx"
agent_id = "missing"
workspace = "/tmp/workspace-1"
"#;
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let result = raw.into_domain();
        assert!(matches!(result, Err(ConfigError::UnknownSessionAgent(id)) if id == "missing"));
    }

    #[test]
    fn test_empty_session_workspace() {
        let toml = r#"
[lark]
app_id = "cli_xxx"
app_secret = "secret_xxx"

[[agents]]
id = "codex-dev"
kind = "codex"
command = "codex"

[[sessions]]
chat_id = "oc_xxxxxxxx"
agent_id = "codex-dev"
workspace = " "
"#;
        let raw: RawConfig = toml::from_str(toml).unwrap();
        let result = raw.into_domain();
        assert!(
            matches!(result, Err(ConfigError::EmptyWorkspace(chat_id)) if chat_id == "oc_xxxxxxxx")
        );
    }
}
