//! Agent 会话生命周期管理

use std::sync::Arc;

use async_trait::async_trait;

use crate::traits::{AgentManager, AgentSession};
use crate::types::*;

/// 默认 AgentManager 实现
///
/// 字段（per-kind driver 注册表 + `Map<ChatId, Arc<dyn AgentSession>>` 会话表 + store）
/// 在阶段1-功能5（TES-82）填充；当前为可编译基线占位。
/// `Arc<dyn AgentSession>` 而非 `Box` 的理由见 `traits.rs` 注释。
pub struct AgentManagerImpl;

impl AgentManagerImpl {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AgentManagerImpl {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AgentManager for AgentManagerImpl {
    async fn get_or_start(
        &self,
        _chat: &ChatId,
        _binding: &Binding,
    ) -> Result<Arc<dyn AgentSession>, CoreError> {
        // TES-82: 查表命中 → 复用 Arc；否则 store.get_resume_key → driver.start_session
        // → Arc::from(boxed) → 入表 → 返回 clone。Dead（alive()==false）清理重建。
        todo!()
    }
}
