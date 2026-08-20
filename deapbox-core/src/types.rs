//! Core domain types.
//!
//! Pure data structures — no behavior, no traits. The trait contracts
//! (`Agent`, `LarkMessageApi`, `ChatDispatcher`) live in sibling modules.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::mpsc;

// ============ Identifiers (NewType) ============

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ChatId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePath(pub PathBuf);

// ============ Agent classification ============

/// The variety of coding-agent CLI behind an Agent. See `CONTEXT.md`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AgentKind {
    Echo,
    ClaudeCode,
    KimiCode,
    Opencode,
    Codex,
}

// ============ Binding ============

/// The (Agent, Workspace) pair a Chat is bound to. Cold state.
///
/// `workspace` is `Option` in Stage 1 (echo agent ignores it); Stage 2 makes
/// it required when real agents that need a working directory land.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub agent_id: AgentId,
    pub workspace: Option<WorkspacePath>,
}

// ============ Inbound message ============

/// An inbound Feishu message from the Operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessage {
    pub chat_id: ChatId,
    pub sender: UserId,
    pub text: String,
    pub msg_id: String,
    pub attachments: Vec<Attachment>,
}

/// An artifact attached to an inbound Feishu message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    /// A Feishu image. `image_key` is lazily downloaded by the agent impl
    /// via `LarkMessageApi::download_image` (Stage 2).
    Image { image_key: String },
}

// ============ Agent output stream ============

/// A stream of `AgentEvent`s returned by `Agent::send` (Stage 2 streaming shape).
///
/// Stage 1 returned `Vec<AgentEvent>` (batch); Stage 2 returns `mpsc::Receiver`
/// so real agents (opencode, claude-code) can stream events as they arrive
/// from the subprocess stdout, without buffering the entire turn first.
///
/// Stream end = sender drop (channel returns `None` on next `recv`). The agent
/// impl is responsible for spawning a task that pushes events and closing the
/// channel when the turn finishes (after emitting `TurnEnd`).
pub type AgentEventStream = mpsc::Receiver<AgentEvent>;

// ============ Agent output ============

/// A structured piece of an Agent's reply within a Turn. See `CONTEXT.md`.
///
/// Stage 1 echo only emits `Text` + `TurnEnd{None}`. Other variants are
/// shape-ready for Stage 2 real agents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// Final reply text.
    Text(String),
    /// Reasoning trace.
    Thinking(String),
    /// Tool invocation summary.
    ToolCall(String),
    /// Tool execution result.
    ToolResult(String),
    /// Agent-reported failure.
    Error { message: String, fatal: bool },
    /// Turn boundary. `resume_key` is `None` for echo; Stage 2 real agents
    /// populate it from the agent's turn-end signal (per ADR-0002).
    TurnEnd { resume_key: Option<String> },
}

// ============ Config shapes ============

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LarkConfig {
    pub app_id: String,
    pub app_secret: String,
}

/// Agent definition in `config.toml` `[[agents]]` section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentConfig {
    pub id: AgentId,
    pub kind: AgentKind,
    pub command: String,
}

/// Session binding in `config.toml` `[[sessions]]` section.
///
/// `workspace` is optional per ADR-0007 (Stage 1 echo doesn't use it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionConfig {
    pub chat_id: ChatId,
    pub agent_id: AgentId,
    #[serde(default)]
    pub workspace: Option<WorkspacePath>,
}

/// Top-level config shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppConfig {
    pub lark: LarkConfig,
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
    #[serde(default)]
    pub sessions: Vec<SessionConfig>,
}

// ============ Errors ============

#[derive(Debug, Clone, thiserror::Error)]
pub enum CoreError {
    #[error("Agent not found: {0}")]
    AgentNotFound(String),
    #[error("Session not found: {0}")]
    SessionNotFound(String),
    #[error("Agent error: {0}")]
    Agent(String),
    #[error("Lark API error: {0}")]
    LarkApi(String),
    #[error("IO error: {0}")]
    Io(String),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum LarkApiError {
    #[error("invalid Lark client configuration: {0}")]
    ClientConfig(String),
    #[error("Lark API request failed: {0}")]
    Request(String),
}
