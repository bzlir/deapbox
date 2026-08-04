//! 核心数据结构

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ============ 路由标识（NewType） ============

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ChatId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentSessionId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePath(pub PathBuf);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UserId(pub String);

// ============ Agent 类型 ============

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentKind {
    Opencode,
    Codex,
    ClaudeCode,
    KimiCode,
}

// ============ 核心 Entity ============

/// ChatSession — 一个飞书群绑定一个 (Agent + Workspace)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatSession {
    pub chat_id: ChatId,
    pub agent_id: AgentId,
    pub workspace: WorkspacePath,
}

/// AgentConfig — agent 定义
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub id: AgentId,
    pub kind: AgentKind,
    pub command: String,
    pub args: Vec<String>,
    pub env_vars: std::collections::HashMap<String, String>,
}

/// AgentSession — agent 内部的一个工作会话（通过 resume 恢复）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSession {
    pub id: AgentSessionId,
    pub agent_id: AgentId,
    pub agent_session_key: String,
    pub workspace: WorkspacePath,
    pub created_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}

// ============ 消息 / 事件 ============

/// 用户消息
#[derive(Debug, Clone)]
pub struct UserMessage {
    pub chat_id: ChatId,
    pub sender: UserId,
    pub text: String,
    pub msg_id: String,
}

/// 标准化输出事件 — 各 agent 原始输出经 adapter 清洗后的统一格式
#[derive(Debug, Clone)]
pub enum NormalizedEvent {
    /// 最终回复文本
    Text(String),
    /// 思考过程（卡片层用灰字/折叠展示）
    Thinking(String),
    /// 工具调用（"正在读取 main.rs..."）
    ToolCall(String),
    /// 工具执行结果
    ToolResult(String),
    /// 当前 turn 结束
    TurnComplete,
    /// 错误
    Error { message: String, fatal: bool },
}

/// Agent 输出事件（adapter 层之外统一从此取得）
#[derive(Debug, Clone)]
pub enum AgentOutputEvent {
    Normalized(NormalizedEvent),
    /// agent 返回 session key
    SessionCreated(AgentSessionId, String),
}

// ============ 命令 ============

/// 飞书消息中识别的 bot 命令
#[derive(Debug, Clone)]
pub enum BotCommand {
    NewSession(Option<String>),
    ListSessions,
    SwitchSession(AgentSessionId),
}

// ============ 健康状态 ============

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    Healthy,
    Unhealthy(String),
    Dead,
}

// ============ 应用配置 ============

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LarkConfig {
    pub app_id: String,
    pub app_secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub lark: LarkConfig,
    pub agents: Vec<AgentConfig>,
    pub sessions: Vec<ChatSession>,
}

// ============ 错误类型 ============

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("Agent not found: {0}")]
    AgentNotFound(String),
    #[error("Session not found: {0}")]
    SessionNotFound(String),
    #[error("Agent process error: {0}")]
    AgentProcess(String),
    #[error("Store error: {0}")]
    Store(String),
    #[error("Lark error: {0}")]
    Lark(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
