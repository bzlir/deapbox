//! 核心 trait：AgentDriver, AgentProcess, ProtocolAdapter, Router, AgentManager

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;

use crate::types::*;

/// Agent 协议 — 所有 coding agent 统一抽象
#[async_trait]
pub trait AgentDriver: Send + Sync {
    /// 启动一个 agent 进程，返回 AgentProcess 句柄
    async fn spawn(
        &self,
        config: &AgentConfig,
        workspace: &Path,
    ) -> Result<Box<dyn AgentProcess>, CoreError>;
}

/// 运行中的 agent 进程（每个 ChatSession 一个独立进程）
#[async_trait]
pub trait AgentProcess: Send {
    /// 向 agent 发送用户消息
    async fn send_input(&self, text: &str) -> Result<(), CoreError>;
    /// 从 agent 读取输出（已通过 ProtocolAdapter 清洗为 NormalizedEvent）
    async fn recv_output(&mut self) -> Result<AgentOutputEvent, CoreError>;
    /// 中断当前 turn
    async fn interrupt(&self) -> Result<(), CoreError>;
    /// 健康检查
    async fn health_check(&self) -> Result<HealthStatus, CoreError>;
    /// 关闭进程
    async fn shutdown(self: Box<Self>) -> Result<(), CoreError>;
}

/// Protocol Adapter — 将 agent 原始 stdout 清洗为 NormalizedEvent
pub trait ProtocolAdapter: Send + Sync {
    /// 对原始输出行做清洗，返回 0..N 个 NormalizedEvent
    fn process_line(&mut self, line: &str) -> Vec<NormalizedEvent>;
    /// agent 进程退出时清空缓冲区
    fn flush(&mut self) -> Vec<NormalizedEvent>;
}

/// 消息路由
#[async_trait]
pub trait Router: Send + Sync {
    async fn route_user_message(&self, msg: UserMessage) -> Result<(), CoreError>;
}

/// Agent 进程生命周期管理
#[async_trait]
pub trait AgentManager: Send + Sync {
    async fn get_or_spawn(
        &self,
        session: &ChatSession,
    ) -> Result<Arc<dyn AgentProcess>, CoreError>;
    async fn create_session(
        &self,
        session: &ChatSession,
        label: Option<&str>,
    ) -> Result<(AgentSession, Arc<dyn AgentProcess>), CoreError>;
    async fn switch_session(
        &self,
        session: &ChatSession,
        key: &str,
    ) -> Result<Arc<dyn AgentProcess>, CoreError>;
    async fn list_sessions(
        &self,
        session: &ChatSession,
    ) -> Result<Vec<AgentSession>, CoreError>;
}

/// 持久化存储
#[async_trait]
pub trait PersistentStore: Send + Sync {
    async fn get_session_binding(
        &self,
        chat_id: &ChatId,
    ) -> Result<Option<AgentId>, CoreError>;
    async fn set_session_binding(
        &self,
        chat_id: &ChatId,
        agent_id: &AgentId,
    ) -> Result<(), CoreError>;
    async fn get_resume_key(
        &self,
        chat_id: &ChatId,
    ) -> Result<Option<String>, CoreError>;
    async fn set_resume_key(
        &self,
        chat_id: &ChatId,
        key: &str,
    ) -> Result<(), CoreError>;
}
