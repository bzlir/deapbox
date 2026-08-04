//! 消息路由

use std::sync::Arc;

use async_trait::async_trait;

use crate::traits::{AgentManager, PersistentStore, Router};
use crate::types::*;

/// 默认路由实现
pub struct RouterImpl {
    store: Arc<dyn PersistentStore>,
    agent_manager: Arc<dyn AgentManager>,
}

impl RouterImpl {
    pub fn new(
        store: Arc<dyn PersistentStore>,
        agent_manager: Arc<dyn AgentManager>,
    ) -> Self {
        Self {
            store,
            agent_manager,
        }
    }
}

#[async_trait]
impl Router for RouterImpl {
    async fn route_user_message(&self, msg: UserMessage) -> Result<(), CoreError> {
        // 1. 查找 ChatSession（从 store 或配置中获取 AgentId + Workspace）
        // 2. 通过 AgentManager.get_or_spawn() 获取进程
        // 3. 调用 process.send_input()
        // 4. 循环 process.recv_output() 处理 NormalizedEvent
        // 5. 每个事件转发到 LarkCard 流式刷新
        todo!()
    }
}
