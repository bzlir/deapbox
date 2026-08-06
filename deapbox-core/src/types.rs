//! 核心数据结构

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ============ 路由标识（NewType） ============

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// Binding — `PersistentStore::get_session_binding` 的返回值。
///
/// `resume_key` 不在这里（它是热键，独立 KV；见 working.md lesson #4）。
/// `ChatSession` 是首启 seed + 配置态；`Binding` 是运行时从 sled 读出的冷绑定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub agent_id: AgentId,
    pub workspace: WorkspacePath,
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

/// 标准化输出事件 — per-kind session 把 agent 原生 stream-json 事件映射到此。
///
/// 注意：`TurnComplete` 已移除（turn 结束是 `AgentEvent::TurnEnd`，由 agent 自己说，
/// 见 working.md lesson #2）。
///
/// `PartialEq, Eq` 让 TES-84 Router 测试可断言输出顺序（对标 `Binding` /
/// `BotCommandResult` / `HealthStatus` 同款 derive-for-testability 惯用法）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NormalizedEvent {
    /// 最终回复文本
    Text(String),
    /// 思考过程（卡片层用灰字/折叠展示）
    Thinking(String),
    /// 工具调用（"正在读取 main.rs..."）
    ToolCall(String),
    /// 工具执行结果
    ToolResult(String),
    /// 错误（agent 自报的 error 事件）
    Error { message: String, fatal: bool },
}

/// Agent 事件流 — per-kind `AgentSession` 通过 `subscribe()` 输出。
///
/// `TurnEnd`/`Exited`/`Failed` 在流里，不再由 host 猜（非 idle-timeout / 非 EOF）。
/// `Clone` 以便 `tokio::sync::broadcast` 多接收端（TES-81 测试 subscribe 多端）。
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// adapter 清洗后的标准化事件
    Normalized(NormalizedEvent),
    /// 本轮结束；`resume_key` 来自 agent 的 `result` 事件（`None` 表示无 resume）。
    /// `subtype: compact/compaction` 的 `result` 不发 `TurnEnd`（TES-79 过滤）。
    TurnEnd { resume_key: Option<String> },
    /// 进程退出（kimi 正常退出 / claude 异常）；code 为退出码
    Exited(Option<i32>),
    /// 会话失败
    Failed(CoreError),
}

// ============ 命令 ============

/// 飞书消息中识别的 bot 命令。
#[derive(Debug, Clone)]
pub enum BotCommand {
    /// `/new`：清当前 chat 的 resume key，并丢弃活跃会话。
    New,
    /// `/switch agent <id>`：保留当前 workspace，切到另一个 agent。
    SwitchAgent(AgentId),
    /// `/switch workspace <path>`：保留当前 agent，切到另一个 workspace。
    SwitchWorkspace(WorkspacePath),
    /// `/session`：展示当前 binding、resume key 与活跃会话状态。
    Session,
}

/// Bot command handling result for operator-facing rendering.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BotCommandResult {
    NewSession {
        chat_id: ChatId,
    },
    SwitchedAgent {
        chat_id: ChatId,
        binding: Binding,
    },
    SwitchedWorkspace {
        chat_id: ChatId,
        binding: Binding,
    },
    Session {
        chat_id: ChatId,
        binding: Option<Binding>,
        resume_key: Option<String>,
        active: bool,
    },
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

/// `Clone` 是为了让 `AgentEvent::Failed(CoreError)` 走 `tokio::sync::broadcast`
/// 多接收端（`Io` 用 `String` 而非 `#[from] std::io::Error`，后者非 `Clone`）。
#[derive(Debug, Clone, thiserror::Error)]
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
    Io(String),
}
