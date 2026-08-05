//! 核心 trait：AgentDriver, AgentSession, Router, OutputSink, AgentManager, PersistentStore
//!
//! 旧 `AgentProcess` 已删——它把 `get_or_spawn → Arc<dyn AgentProcess>`（共享）、
//! `recv_output(&mut self)`（独占）、`shutdown(self: Box<Self>)`（所有权）三者揉进一个
//! trait，三方所有权/可变性矛盾见 working.md lesson #5。新设计用 channel 解耦：
//! `AgentSession` 全 `&self`，input/output 都从 `&self` 走。

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::types::*;

/// `AgentSession::subscribe()` 的返回端。
///
/// `broadcast` 以便 TES-81 测 `subscribe()` 多接收端不丢事件。
pub type AgentEventReceiver = broadcast::Receiver<AgentEvent>;

// ============ AgentDriver（工厂，per-kind） ============

/// Agent 驱动工厂——每个 AgentKind 一个实现（ClaudeCode / KimiCode ...）。
///
/// 只负责 `start_session` 产出 owned `AgentSession`；进程生命周期归 session own。
/// driver 内部按 `AgentKind` 注入原生 `--output-format stream-json` 等 flag。
#[async_trait]
pub trait AgentDriver: Send + Sync {
    /// 启动一个会话。`resume` 非空时尝试恢复既有 turn（`--resume <id>`）。
    async fn start_session(
        &self,
        resume: Option<&str>,
        workspace: &WorkspacePath,
    ) -> Result<Box<dyn AgentSession>, CoreError>;
}

// ============ AgentSession（运行，per chat） ============

/// 运行中的 agent 会话（每个 chat 一个）。
///
/// 所有方法 `&self`：`send` / `subscribe` / `interrupt` / `current_resume_key`
/// / `alive` 都不取所有权；`close(self: Box<Self>)` 是显式关闭（`kill_on_drop`
/// 兜底）。`&self` + channel 让 `Arc<dyn AgentSession>` 共享一致（不再自相矛盾）。
///
/// `Send + Sync`：`Arc<dyn AgentSession>: Send` 要求 `T: Send + Sync`（Arc 的 Send
/// impl 的硬约束），TES-82 的会话表 `Map<ChatId, Arc<dyn AgentSession>>` 在
/// `Send + Sync` 的 `AgentManagerImpl` 里依赖此 bound。trait object 不能少。
#[async_trait]
pub trait AgentSession: Send + Sync {
    /// 向 agent 发送用户消息（非阻塞，`&self`）。
    async fn send(&self, text: &str) -> Result<(), CoreError>;
    /// 订阅事件流。多次调用各得独立接收端（broadcast）。
    fn subscribe(&self) -> AgentEventReceiver;
    /// 中断当前 turn（SIGINT，`&self`）。
    async fn interrupt(&self) -> Result<(), CoreError>;
    /// 最近一次 `TurnEnd` 携带的 `resume_key`（供 Router 持久化）。
    fn current_resume_key(&self) -> Option<String>;
    /// 进程是否存活（dead-agent 安全网，非 turn 边界探测器）。
    fn alive(&self) -> bool;
    /// 显式关闭（`kill_on_drop` 兜底）。
    async fn close(self: Box<Self>) -> Result<(), CoreError>;
}

// ============ Router（task-per-message，非阻塞） ============

/// 一轮处理的句柄——spawn 出来的 tokio task，主循环不等完成。
pub struct TurnHandle {
    pub join: tokio::task::JoinHandle<()>,
}

/// 消息路由：解析绑定 → `get_or_start` → `send` + spawn task 收事件到 `OutputSink`。
/// 非阻塞，返回 turn 句柄。`OutputSink` 注入在实现层（`RouterImpl::new`）。
#[async_trait]
pub trait Router: Send + Sync {
    async fn route_user_message(&self, msg: UserMessage) -> Result<TurnHandle, CoreError>;
}

// ============ OutputSink（展示层下沉，飞书卡片实现见 TES-84） ============

#[async_trait]
pub trait OutputSink: Send + Sync {
    async fn consume(&self, event: NormalizedEvent) -> Result<(), CoreError>;
    async fn on_turn_end(&self, resume_key: Option<String>) -> Result<(), CoreError>;
    async fn on_error(&self, err: CoreError) -> Result<(), CoreError>;
}

// ============ AgentManager（会话表 + driver 注册） ============

/// Agent 会话生命周期管理。
///
/// **注意**：`get_or_start` 返回 `Arc<dyn AgentSession>` 而非 issue 原文 `Box`——
/// 会话表需同时**保留**长驻会话（claude 跨 turn 复用）与**分发**句柄给 Router task，
/// `Box` 无法两处同时存在（`Box` lend-out 模型会让下一条消息 spawn 新进程，
/// 破坏长驻 claude 的复用目标）。`Arc` 是 sound 形态；`start_session -> Box` 与
/// `close(self: Box<Self>)` 仍按原文保留（driver 产出 owned，close 消费 owned）。
#[async_trait]
pub trait AgentManager: Send + Sync {
    /// 按 chat 取得会话：命中表则复用，否则按 binding 取 driver → `start_session`
    /// → 入表。返回 `Arc` 以便 Router task 与表共享同一会话。
    async fn get_or_start(
        &self,
        chat: &ChatId,
        binding: &Binding,
    ) -> Result<Arc<dyn AgentSession>, CoreError>;
}

// ============ PersistentStore（binding 含 workspace + resume_key 独立 KV） ============

/// 持久化存储：`binding:{chat_id}`（冷，含 workspace）+ `resume:{chat_id}`（热）。
/// 分存的理由见 working.md lesson #4。
#[async_trait]
pub trait PersistentStore: Send + Sync {
    async fn get_session_binding(&self, chat_id: &ChatId) -> Result<Option<Binding>, CoreError>;
    async fn set_session_binding(
        &self,
        chat_id: &ChatId,
        agent_id: &AgentId,
        workspace: &WorkspacePath,
    ) -> Result<(), CoreError>;
    async fn get_resume_key(&self, chat_id: &ChatId) -> Result<Option<String>, CoreError>;
    async fn set_resume_key(&self, chat_id: &ChatId, key: &str) -> Result<(), CoreError>;
}

// ============ 编译期类型断言（downstream 依赖的 Send + Sync bound） ============

const _: fn() = || {
    fn assert_send_sync<T: ?Sized + Send + Sync>() {}
    assert_send_sync::<Arc<dyn AgentSession>>();
    assert_send_sync::<Arc<dyn AgentDriver>>();
    assert_send_sync::<Arc<dyn AgentManager>>();
    assert_send_sync::<Arc<dyn OutputSink>>();
    assert_send_sync::<Arc<dyn Router>>();
    assert_send_sync::<Arc<dyn PersistentStore>>();
};
