//! TOML config parsing — `config.toml` → `AppConfig`.
//!
//! Stage 1: `[[agents]]` + `[[sessions]]` + `[lark]` sections.
//! `workspace` in `[[sessions]]` is optional (ADR-0007).
//! Validates: dangling `agent_id` references in `[[sessions]]` are rejected
//! (V2.2-equivalent — fail fast on config errors).

use std::path::Path;

use deapbox_core::types::{
    AgentConfig, AgentId, AgentKind, AppConfig, ChatId, LarkConfig, SessionConfig, WorkspacePath,
};
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    ReadFile {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config TOML: {0}")]
    ParseToml(#[from] toml::de::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

/// Load and validate `config.toml`.
pub fn load_config(path: &Path) -> Result<AppConfig, ConfigError> {
    let contents = std::fs::read_to_string(path).map_err(|source| ConfigError::ReadFile {
        path: path.display().to_string(),
        source,
    })?;
    let raw: RawConfig = toml::from_str(&contents)?;
    raw.into_domain()
}

// ============ Raw (deserialization shape) ============

#[derive(Debug, Deserialize)]
struct RawConfig {
    lark: RawLark,
    #[serde(default)]
    agents: Vec<RawAgent>,
    #[serde(default)]
    sessions: Vec<RawSession>,
}

#[derive(Debug, Deserialize)]
struct RawLark {
    app_id: String,
    app_secret: String,
}

#[derive(Debug, Deserialize)]
struct RawAgent {
    id: String,
    kind: String,
    command: String,
}

#[derive(Debug, Deserialize)]
struct RawSession {
    chat_id: String,
    agent_id: String,
    #[serde(default)]
    workspace: Option<String>,
}

impl RawConfig {
    fn into_domain(self) -> Result<AppConfig, ConfigError> {
        // Validate lark
        if self.lark.app_id.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "[lark].app_id must not be empty".to_owned(),
            ));
        }
        if self.lark.app_secret.trim().is_empty() {
            return Err(ConfigError::Invalid(
                "[lark].app_secret must not be empty".to_owned(),
            ));
        }

        // Parse agents
        let mut agents = Vec::with_capacity(self.agents.len());
        let mut agent_ids = std::collections::HashSet::new();
        for raw in self.agents {
            if raw.id.is_empty() {
                return Err(ConfigError::Invalid(
                    "[[agents]].id must not be empty".to_owned(),
                ));
            }
            if raw.command.is_empty() && raw.kind != "echo" {
                return Err(ConfigError::Invalid(format!(
                    "[[agents]] id={} kind={}: command must not be empty (except for kind=echo)",
                    raw.id, raw.kind
                )));
            }
            let kind = parse_kind(&raw.kind)?;
            if !agent_ids.insert(raw.id.clone()) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate agent id: {}",
                    raw.id
                )));
            }
            agents.push(AgentConfig {
                id: AgentId(raw.id),
                kind,
                command: raw.command,
            });
        }

        // Parse sessions
        let mut sessions = Vec::with_capacity(self.sessions.len());
        let mut chat_ids = std::collections::HashSet::new();
        for raw in self.sessions {
            if raw.chat_id.is_empty() {
                return Err(ConfigError::Invalid(
                    "[[sessions]].chat_id must not be empty".to_owned(),
                ));
            }
            if raw.agent_id.is_empty() {
                return Err(ConfigError::Invalid(
                    "[[sessions]].agent_id must not be empty".to_owned(),
                ));
            }
            if !chat_ids.insert(raw.chat_id.clone()) {
                return Err(ConfigError::Invalid(format!(
                    "duplicate session chat_id: {} (V2.2: fail fast, no silent overwrite)",
                    raw.chat_id
                )));
            }
            if !agent_ids.contains(&raw.agent_id) {
                return Err(ConfigError::Invalid(format!(
                    "[[sessions]] chat_id={} references unknown agent_id={}",
                    raw.chat_id, raw.agent_id
                )));
            }
            let workspace = raw
                .workspace
                .map(|p| WorkspacePath(std::path::PathBuf::from(p)));
            sessions.push(SessionConfig {
                chat_id: ChatId(raw.chat_id),
                agent_id: AgentId(raw.agent_id),
                workspace,
            });
        }

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

fn parse_kind(s: &str) -> Result<AgentKind, ConfigError> {
    match s {
        "echo" => Ok(AgentKind::Echo),
        "claude-code" => Ok(AgentKind::ClaudeCode),
        "kimi-code" => Ok(AgentKind::KimiCode),
        "opencode" => Ok(AgentKind::Opencode),
        "codex" => Ok(AgentKind::Codex),
        other => Err(ConfigError::Invalid(format!(
            "unknown agent kind: {} (valid: echo, claude-code, kimi-code, opencode, codex)",
            other
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_config(toml: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "{}", toml).unwrap();
        f
    }

    fn load(toml: &str) -> Result<AppConfig, ConfigError> {
        let f = write_config(toml);
        load_config(f.path())
    }

    fn ok_config() -> &'static str {
        r#"
[lark]
app_id = "cli_xxx"
app_secret = "sec_xxx"

[[agents]]
id = "echo-a"
kind = "echo"
command = ""

[[agents]]
id = "echo-b"
kind = "echo"
command = ""

[[sessions]]
chat_id = "oc_x"
agent_id = "echo-a"

[[sessions]]
chat_id = "oc_y"
agent_id = "echo-b"
workspace = "/tmp/project-b"
"#
    }

    // ============ V1.1: legal config with multiple agents + sessions ============

    #[test]
    fn v1_1_legal_config_parses_correctly() {
        let cfg = load(ok_config()).unwrap();
        assert_eq!(cfg.lark.app_id, "cli_xxx");
        assert_eq!(cfg.lark.app_secret, "sec_xxx");
        assert_eq!(cfg.agents.len(), 2);
        assert_eq!(cfg.agents[0].id, AgentId("echo-a".to_owned()));
        assert_eq!(cfg.agents[0].kind, AgentKind::Echo);
        assert_eq!(cfg.agents[1].id, AgentId("echo-b".to_owned()));
        assert_eq!(cfg.sessions.len(), 2);
        assert_eq!(cfg.sessions[0].chat_id, ChatId("oc_x".to_owned()));
        assert_eq!(cfg.sessions[0].agent_id, AgentId("echo-a".to_owned()));
        assert_eq!(cfg.sessions[0].workspace, None);
        assert_eq!(
            cfg.sessions[1].workspace,
            Some(WorkspacePath("/tmp/project-b".into()))
        );
    }

    // ============ V1.2: missing [lark] section ============

    #[test]
    fn v1_2_missing_lark_section_rejected() {
        let err = load(
            r#"
[[agents]]
id = "echo-a"
kind = "echo"
command = ""
"#,
        )
        .unwrap_err();
        // toml returns missing field error
        assert!(matches!(err, ConfigError::ParseToml(_)));
    }

    // ============ V1.3: missing agent id/kind/command ============

    #[test]
    fn v1_3_missing_agent_id_rejected() {
        let err = load(
            r#"
[lark]
app_id = "x"
app_secret = "y"

[[agents]]
kind = "echo"
command = ""
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::ParseToml(_)));
    }

    #[test]
    fn v1_3_non_echo_missing_command_rejected() {
        let err = load(
            r#"
[lark]
app_id = "x"
app_secret = "y"

[[agents]]
id = "claude"
kind = "claude-code"
command = ""
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)));
    }

    // ============ V1.4: missing session chat_id/agent_id ============

    #[test]
    fn v1_4_missing_session_chat_id_rejected() {
        let err = load(
            r#"
[lark]
app_id = "x"
app_secret = "y"

[[agents]]
id = "echo-a"
kind = "echo"
command = ""

[[sessions]]
agent_id = "echo-a"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::ParseToml(_)));
    }

    // ============ V1.5: session references unknown agent_id ============

    #[test]
    fn v1_5_dangling_agent_reference_rejected() {
        let err = load(
            r#"
[lark]
app_id = "x"
app_secret = "y"

[[agents]]
id = "echo-a"
kind = "echo"
command = ""

[[sessions]]
chat_id = "oc_x"
agent_id = "nonexistent"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(s) if s.contains("unknown agent_id")));
    }

    // ============ V1.6: workspace optional ============

    #[test]
    fn v1_6_workspace_optional_both_present_and_absent_parse() {
        let cfg = load(ok_config()).unwrap();
        assert_eq!(cfg.sessions[0].workspace, None);
        assert_eq!(
            cfg.sessions[1].workspace,
            Some(WorkspacePath("/tmp/project-b".into()))
        );
    }

    // ============ V1.7: unknown kind ============

    #[test]
    fn v1_7_unknown_kind_rejected() {
        let err = load(
            r#"
[lark]
app_id = "x"
app_secret = "y"

[[agents]]
id = "gemini"
kind = "gemini-code"
command = "gemini"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(s) if s.contains("unknown agent kind")));
    }

    // ============ V2.2: duplicate chat_id in sessions ============

    #[test]
    fn v2_2_duplicate_chat_id_rejected() {
        let err = load(
            r#"
[lark]
app_id = "x"
app_secret = "y"

[[agents]]
id = "echo-a"
kind = "echo"
command = ""

[[sessions]]
chat_id = "oc_dup"
agent_id = "echo-a"

[[sessions]]
chat_id = "oc_dup"
agent_id = "echo-a"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(s) if s.contains("duplicate session chat_id")));
    }

    // ============ Extra: empty agents list is OK (Stage 1 can still parse) ============

    #[test]
    fn empty_agents_list_parses() {
        let cfg = load(
            r#"
[lark]
app_id = "x"
app_secret = "y"
"#,
        )
        .unwrap();
        assert!(cfg.agents.is_empty());
        assert!(cfg.sessions.is_empty());
    }

    // ============ Extra: duplicate agent id rejected ============

    #[test]
    fn duplicate_agent_id_rejected() {
        let err = load(
            r#"
[lark]
app_id = "x"
app_secret = "y"

[[agents]]
id = "echo-a"
kind = "echo"
command = ""

[[agents]]
id = "echo-a"
kind = "echo"
command = ""
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(s) if s.contains("duplicate agent id")));
    }

    // ============ Extra: empty app_id rejected ============

    #[test]
    fn empty_app_id_rejected() {
        let err = load(
            r#"
[lark]
app_id = ""
app_secret = "y"
"#,
        )
        .unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(s) if s.contains("app_id")));
    }

    // ============ Extra: missing file ============

    #[test]
    fn missing_file_returns_read_error() {
        let err = load_config(Path::new("/nonexistent/path/config.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::ReadFile { .. }));
    }
}
