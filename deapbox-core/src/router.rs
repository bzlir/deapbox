//! 消息路由

use std::sync::Arc;

use async_trait::async_trait;

use crate::traits::{AgentManager, OutputSink, PersistentStore, Router, TurnHandle};
use crate::types::*;

/// 默认路由实现
///
/// 字段在阶段1-功能6（TES-84 核心消息 Router MVP）启用 `route_user_message` 时使用，
/// 当前为可编译基线占位，故显式允许 `dead_code`。
#[allow(dead_code)]
pub struct RouterImpl {
    _store: Arc<dyn PersistentStore>,
    _agent_manager: Arc<dyn AgentManager>,
    _sink: Arc<dyn OutputSink>,
}

impl RouterImpl {
    pub fn new(
        store: Arc<dyn PersistentStore>,
        agent_manager: Arc<dyn AgentManager>,
        sink: Arc<dyn OutputSink>,
    ) -> Self {
        Self {
            _store: store,
            _agent_manager: agent_manager,
            _sink: sink,
        }
    }
}

#[async_trait]
impl Router for RouterImpl {
    async fn route_user_message(&self, _msg: UserMessage) -> Result<TurnHandle, CoreError> {
        // 1. store.get_session_binding(chat) → Binding { agent_id, workspace }
        // 2. agent_manager.get_or_start(chat, binding) → Arc<dyn AgentSession>
        // 3. session.send(text)（非阻塞，&self）
        // 4. spawn tokio task: session.subscribe() → 循环收 AgentEvent:
        //      Normalized(e) → sink.consume(e)
        //      TurnEnd{resume_key} → store.set_resume_key + sink.on_turn_end → task 结束
        //      Exited / Failed → sink.on_error（不冒充完成）
        // 5. 返回 TurnHandle，不等完成（TES-84 实现）
        todo!()
    }
}
