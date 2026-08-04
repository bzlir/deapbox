//! Agent 进程生命周期管理

use std::sync::Arc;

use async_trait::async_trait;

use crate::traits::AgentManager;
use crate::types::*;

/// 默认 AgentManager 实现
pub struct AgentManagerImpl;

impl AgentManagerImpl {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl AgentManager for AgentManagerImpl {
    async fn get_or_spawn(
        &self,
        _session: &ChatSession,
    ) -> Result<Arc<dyn crate::traits::AgentProcess>, CoreError> {
        todo!()
    }

    async fn create_session(
        &self,
        _session: &ChatSession,
        _label: Option<&str>,
    ) -> Result<(AgentSession, Arc<dyn crate::traits::AgentProcess>), CoreError> {
        todo!()
    }

    async fn switch_session(
        &self,
        _session: &ChatSession,
        _key: &str,
    ) -> Result<Arc<dyn crate::traits::AgentProcess>, CoreError> {
        todo!()
    }

    async fn list_sessions(
        &self,
        _session: &ChatSession,
    ) -> Result<Vec<AgentSession>, CoreError> {
        todo!()
    }
}
