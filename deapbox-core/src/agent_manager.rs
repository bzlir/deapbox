//! Agent 会话生命周期管理。

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::traits::{AgentDriver, AgentManager, AgentSession, PersistentStore};
use crate::types::*;

/// 默认 AgentManager 实现。
///
/// 会话表保存 `Arc<dyn AgentSession>`：表内保留一份，Router task 拿 clone。driver
/// 仍按 trait 产出 `Box<dyn AgentSession>`，在进入共享表时转换为 `Arc`。
pub struct AgentManagerImpl {
    store: Arc<dyn PersistentStore>,
    drivers: RwLock<HashMap<AgentKind, Arc<dyn AgentDriver>>>,
    agent_kinds: RwLock<HashMap<AgentId, AgentKind>>,
    sessions: Mutex<HashMap<ChatId, Arc<dyn AgentSession>>>,
}

impl AgentManagerImpl {
    pub fn new(store: Arc<dyn PersistentStore>) -> Self {
        Self {
            store,
            drivers: RwLock::new(HashMap::new()),
            agent_kinds: RwLock::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_registries(
        store: Arc<dyn PersistentStore>,
        agents: impl IntoIterator<Item = AgentConfig>,
        drivers: impl IntoIterator<Item = (AgentKind, Arc<dyn AgentDriver>)>,
    ) -> Self {
        let manager = Self::new(store);
        for (kind, driver) in drivers {
            manager.register_driver(kind, driver);
        }
        for agent in agents {
            manager.register_agent(agent.id, agent.kind);
        }
        manager
    }

    pub fn register_driver(&self, kind: AgentKind, driver: Arc<dyn AgentDriver>) {
        self.drivers
            .write()
            .expect("driver registry poisoned")
            .insert(kind, driver);
    }

    pub fn register_agent(&self, agent_id: AgentId, kind: AgentKind) {
        self.agent_kinds
            .write()
            .expect("agent registry poisoned")
            .insert(agent_id, kind);
    }

    pub async fn handle_command(
        &self,
        chat: &ChatId,
        command: BotCommand,
    ) -> Result<BotCommandResult, CoreError> {
        match command {
            BotCommand::New => {
                self.drop_session(chat).await;
                self.store.set_resume_key(chat, "").await?;
                Ok(BotCommandResult::NewSession {
                    chat_id: chat.clone(),
                })
            }
            BotCommand::SwitchAgent(agent_id) => {
                let kind = self.agent_kind(&agent_id)?;
                let current = self.current_binding(chat).await?;
                let binding = Binding {
                    agent_id,
                    workspace: current.workspace,
                };
                self.agent_kinds
                    .write()
                    .expect("agent registry poisoned")
                    .insert(binding.agent_id.clone(), kind);
                self.store
                    .set_session_binding(chat, &binding.agent_id, &binding.workspace)
                    .await?;
                self.drop_session(chat).await;
                Ok(BotCommandResult::SwitchedAgent {
                    chat_id: chat.clone(),
                    binding,
                })
            }
            BotCommand::SwitchWorkspace(workspace) => {
                let current = self.current_binding(chat).await?;
                let binding = Binding {
                    agent_id: current.agent_id,
                    workspace,
                };
                self.store
                    .set_session_binding(chat, &binding.agent_id, &binding.workspace)
                    .await?;
                self.drop_session(chat).await;
                Ok(BotCommandResult::SwitchedWorkspace {
                    chat_id: chat.clone(),
                    binding,
                })
            }
            BotCommand::Session => {
                let binding = self.store.get_session_binding(chat).await?;
                let resume_key = self.store.get_resume_key(chat).await?;
                let active = self.session_alive(chat).await;
                Ok(BotCommandResult::Session {
                    chat_id: chat.clone(),
                    binding,
                    resume_key: resume_key.filter(|key| !key.is_empty()),
                    active,
                })
            }
        }
    }

    pub async fn health_check(&self) -> Vec<(ChatId, HealthStatus)> {
        let mut sessions = self.sessions.lock().await;
        let mut statuses = Vec::with_capacity(sessions.len());
        sessions.retain(|chat, session| {
            let alive = session.alive();
            statuses.push((
                chat.clone(),
                if alive {
                    HealthStatus::Healthy
                } else {
                    HealthStatus::Dead
                },
            ));
            alive
        });
        statuses
    }

    pub async fn active_session_count(&self) -> usize {
        self.sessions.lock().await.len()
    }

    async fn drop_session(&self, chat: &ChatId) {
        self.sessions.lock().await.remove(chat);
    }

    async fn session_alive(&self, chat: &ChatId) -> bool {
        self.sessions
            .lock()
            .await
            .get(chat)
            .is_some_and(|session| session.alive())
    }

    async fn current_binding(&self, chat: &ChatId) -> Result<Binding, CoreError> {
        self.store
            .get_session_binding(chat)
            .await?
            .ok_or_else(|| CoreError::SessionNotFound(chat.0.clone()))
    }

    fn driver_for(&self, agent_id: &AgentId) -> Result<Arc<dyn AgentDriver>, CoreError> {
        let kind = self.agent_kind(agent_id)?;
        self.drivers
            .read()
            .expect("driver registry poisoned")
            .get(&kind)
            .cloned()
            .ok_or_else(|| CoreError::AgentNotFound(agent_id.0.clone()))
    }

    fn agent_kind(&self, agent_id: &AgentId) -> Result<AgentKind, CoreError> {
        self.agent_kinds
            .read()
            .expect("agent registry poisoned")
            .get(agent_id)
            .cloned()
            .ok_or_else(|| CoreError::AgentNotFound(agent_id.0.clone()))
    }
}

#[async_trait]
impl AgentManager for AgentManagerImpl {
    async fn get_or_start(
        &self,
        chat: &ChatId,
        binding: &Binding,
    ) -> Result<Arc<dyn AgentSession>, CoreError> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get(chat) {
            if session.alive() {
                return Ok(Arc::clone(session));
            }
        }
        sessions.remove(chat);

        let driver = self.driver_for(&binding.agent_id)?;
        let resume_key = self
            .store
            .get_resume_key(chat)
            .await?
            .filter(|key| !key.is_empty());
        let session: Arc<dyn AgentSession> = Arc::from(
            driver
                .start_session(resume_key.as_deref(), &binding.workspace)
                .await?,
        );
        sessions.insert(chat.clone(), Arc::clone(&session));
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Mutex as StdMutex,
        },
    };

    use tokio::sync::broadcast;

    use super::*;
    use crate::traits::{AgentEventReceiver, AgentSession};

    #[derive(Default)]
    struct FakeStore {
        bindings: StdMutex<HashMap<ChatId, Binding>>,
        resumes: StdMutex<HashMap<ChatId, String>>,
    }

    #[async_trait]
    impl PersistentStore for FakeStore {
        async fn get_session_binding(
            &self,
            chat_id: &ChatId,
        ) -> Result<Option<Binding>, CoreError> {
            Ok(self.bindings.lock().unwrap().get(chat_id).cloned())
        }

        async fn set_session_binding(
            &self,
            chat_id: &ChatId,
            agent_id: &AgentId,
            workspace: &WorkspacePath,
        ) -> Result<(), CoreError> {
            self.bindings.lock().unwrap().insert(
                chat_id.clone(),
                Binding {
                    agent_id: agent_id.clone(),
                    workspace: workspace.clone(),
                },
            );
            Ok(())
        }

        async fn get_resume_key(&self, chat_id: &ChatId) -> Result<Option<String>, CoreError> {
            Ok(self.resumes.lock().unwrap().get(chat_id).cloned())
        }

        async fn set_resume_key(&self, chat_id: &ChatId, key: &str) -> Result<(), CoreError> {
            self.resumes
                .lock()
                .unwrap()
                .insert(chat_id.clone(), key.to_owned());
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeDriver {
        starts: AtomicUsize,
        requests: StdMutex<Vec<(Option<String>, WorkspacePath)>>,
        alive_flags: StdMutex<Vec<Arc<AtomicBool>>>,
    }

    #[async_trait]
    impl AgentDriver for FakeDriver {
        async fn start_session(
            &self,
            resume: Option<&str>,
            workspace: &WorkspacePath,
        ) -> Result<Box<dyn AgentSession>, CoreError> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.requests
                .lock()
                .unwrap()
                .push((resume.map(str::to_owned), workspace.clone()));
            let alive = Arc::new(AtomicBool::new(true));
            self.alive_flags.lock().unwrap().push(Arc::clone(&alive));
            Ok(Box::new(FakeSession::new(alive)))
        }
    }

    struct FakeSession {
        alive: Arc<AtomicBool>,
        tx: broadcast::Sender<AgentEvent>,
    }

    impl FakeSession {
        fn new(alive: Arc<AtomicBool>) -> Self {
            let (tx, _) = broadcast::channel(8);
            Self { alive, tx }
        }
    }

    #[async_trait]
    impl AgentSession for FakeSession {
        async fn send(&self, _text: &str) -> Result<(), CoreError> {
            Ok(())
        }

        fn subscribe(&self) -> AgentEventReceiver {
            self.tx.subscribe()
        }

        async fn interrupt(&self) -> Result<(), CoreError> {
            Ok(())
        }

        fn current_resume_key(&self) -> Option<String> {
            None
        }

        fn alive(&self) -> bool {
            self.alive.load(Ordering::SeqCst)
        }

        async fn close(self: Box<Self>) -> Result<(), CoreError> {
            self.alive.store(false, Ordering::SeqCst);
            Ok(())
        }
    }

    fn chat(id: &str) -> ChatId {
        ChatId(id.to_owned())
    }

    fn agent(id: &str) -> AgentId {
        AgentId(id.to_owned())
    }

    fn ws(path: &str) -> WorkspacePath {
        WorkspacePath(PathBuf::from(path))
    }

    fn binding(agent_id: &str, workspace: &str) -> Binding {
        Binding {
            agent_id: agent(agent_id),
            workspace: ws(workspace),
        }
    }

    fn manager(store: Arc<FakeStore>, driver: Arc<FakeDriver>, agent_id: &str) -> AgentManagerImpl {
        AgentManagerImpl::with_registries(
            store,
            [AgentConfig {
                id: agent(agent_id),
                kind: AgentKind::ClaudeCode,
                command: "fake".to_owned(),
                args: Vec::new(),
                env_vars: HashMap::new(),
            }],
            [(AgentKind::ClaudeCode, driver as Arc<dyn AgentDriver>)],
        )
    }

    #[tokio::test]
    async fn first_start_then_reuse_same_chat_session() {
        let store = Arc::new(FakeStore::default());
        let driver = Arc::new(FakeDriver::default());
        let manager = manager(Arc::clone(&store), Arc::clone(&driver), "claude");
        let chat = chat("chat-a");
        store.set_resume_key(&chat, "resume-a").await.unwrap();

        let first = manager
            .get_or_start(&chat, &binding("claude", "/work/a"))
            .await
            .unwrap();
        let second = manager
            .get_or_start(&chat, &binding("claude", "/work/a"))
            .await
            .unwrap();

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(driver.starts.load(Ordering::SeqCst), 1);
        assert_eq!(
            driver.requests.lock().unwrap().as_slice(),
            &[(Some("resume-a".to_owned()), ws("/work/a"))]
        );
    }

    #[tokio::test]
    async fn dead_session_is_removed_and_rebuilt() {
        let store = Arc::new(FakeStore::default());
        let driver = Arc::new(FakeDriver::default());
        let manager = manager(Arc::clone(&store), Arc::clone(&driver), "claude");
        let chat = chat("chat-a");

        let first = manager
            .get_or_start(&chat, &binding("claude", "/work/a"))
            .await
            .unwrap();
        driver.alive_flags.lock().unwrap()[0].store(false, Ordering::SeqCst);
        let second = manager
            .get_or_start(&chat, &binding("claude", "/work/a"))
            .await
            .unwrap();

        assert!(!Arc::ptr_eq(&first, &second));
        assert_eq!(driver.starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn unknown_agent_id_returns_agent_not_found() {
        let store = Arc::new(FakeStore::default());
        let driver = Arc::new(FakeDriver::default());
        let manager = manager(store, driver, "claude");

        let result = manager
            .get_or_start(&chat("chat-a"), &binding("missing", "/work/a"))
            .await;

        assert!(matches!(result, Err(CoreError::AgentNotFound(id)) if id == "missing"));
    }

    #[tokio::test]
    async fn different_chats_do_not_share_sessions() {
        let store = Arc::new(FakeStore::default());
        let driver = Arc::new(FakeDriver::default());
        let manager = manager(store, Arc::clone(&driver), "claude");

        let a = manager
            .get_or_start(&chat("chat-a"), &binding("claude", "/work/a"))
            .await
            .unwrap();
        let b = manager
            .get_or_start(&chat("chat-b"), &binding("claude", "/work/a"))
            .await
            .unwrap();

        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(driver.starts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn new_command_clears_resume_and_drops_active_session() {
        let store = Arc::new(FakeStore::default());
        let driver = Arc::new(FakeDriver::default());
        let manager = manager(Arc::clone(&store), Arc::clone(&driver), "claude");
        let chat = chat("chat-a");
        store.set_resume_key(&chat, "resume-a").await.unwrap();
        manager
            .get_or_start(&chat, &binding("claude", "/work/a"))
            .await
            .unwrap();

        let result = manager
            .handle_command(&chat, BotCommand::New)
            .await
            .unwrap();
        manager
            .get_or_start(&chat, &binding("claude", "/work/a"))
            .await
            .unwrap();

        assert_eq!(
            result,
            BotCommandResult::NewSession {
                chat_id: chat.clone()
            }
        );
        assert_eq!(
            store.get_resume_key(&chat).await.unwrap(),
            Some(String::new())
        );
        assert_eq!(driver.starts.load(Ordering::SeqCst), 2);
        assert_eq!(driver.requests.lock().unwrap()[1].0, None);
    }

    #[tokio::test]
    async fn switch_agent_updates_binding_and_drops_active_session() {
        let store = Arc::new(FakeStore::default());
        let driver = Arc::new(FakeDriver::default());
        let manager = manager(Arc::clone(&store), Arc::clone(&driver), "claude");
        let chat = chat("chat-a");
        store
            .set_session_binding(&chat, &agent("claude"), &ws("/work/a"))
            .await
            .unwrap();
        manager.register_agent(agent("kimi"), AgentKind::ClaudeCode);
        manager
            .get_or_start(&chat, &binding("claude", "/work/a"))
            .await
            .unwrap();

        let result = manager
            .handle_command(&chat, BotCommand::SwitchAgent(agent("kimi")))
            .await
            .unwrap();

        assert_eq!(
            result,
            BotCommandResult::SwitchedAgent {
                chat_id: chat.clone(),
                binding: binding("kimi", "/work/a"),
            }
        );
        assert_eq!(
            store.get_session_binding(&chat).await.unwrap(),
            Some(binding("kimi", "/work/a"))
        );
        assert_eq!(manager.active_session_count().await, 0);
    }

    #[tokio::test]
    async fn switch_workspace_updates_binding_and_drops_active_session() {
        let store = Arc::new(FakeStore::default());
        let driver = Arc::new(FakeDriver::default());
        let manager = manager(Arc::clone(&store), Arc::clone(&driver), "claude");
        let chat = chat("chat-a");
        store
            .set_session_binding(&chat, &agent("claude"), &ws("/work/a"))
            .await
            .unwrap();
        manager
            .get_or_start(&chat, &binding("claude", "/work/a"))
            .await
            .unwrap();

        let result = manager
            .handle_command(&chat, BotCommand::SwitchWorkspace(ws("/work/b")))
            .await
            .unwrap();

        assert_eq!(
            result,
            BotCommandResult::SwitchedWorkspace {
                chat_id: chat.clone(),
                binding: binding("claude", "/work/b"),
            }
        );
        assert_eq!(
            store.get_session_binding(&chat).await.unwrap(),
            Some(binding("claude", "/work/b"))
        );
        assert_eq!(manager.active_session_count().await, 0);
    }

    #[tokio::test]
    async fn concurrent_get_or_start_for_same_chat_spawns_once() {
        let store = Arc::new(FakeStore::default());
        let driver = Arc::new(FakeDriver::default());
        let manager = Arc::new(manager(store, Arc::clone(&driver), "claude"));
        let chat = chat("chat-a");
        let binding = binding("claude", "/work/a");

        let mut tasks = Vec::new();
        for _ in 0..16 {
            let manager = Arc::clone(&manager);
            let chat = chat.clone();
            let binding = binding.clone();
            tasks.push(tokio::spawn(async move {
                manager.get_or_start(&chat, &binding).await.unwrap()
            }));
        }

        let mut sessions = Vec::new();
        for task in tasks {
            sessions.push(task.await.unwrap());
        }

        for session in &sessions[1..] {
            assert!(Arc::ptr_eq(&sessions[0], session));
        }
        assert_eq!(driver.starts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn health_check_reports_and_removes_dead_sessions() {
        let store = Arc::new(FakeStore::default());
        let driver = Arc::new(FakeDriver::default());
        let manager = manager(store, Arc::clone(&driver), "claude");
        let chat = chat("chat-a");
        manager
            .get_or_start(&chat, &binding("claude", "/work/a"))
            .await
            .unwrap();
        driver.alive_flags.lock().unwrap()[0].store(false, Ordering::SeqCst);

        let statuses = manager.health_check().await;

        assert_eq!(statuses, vec![(chat, HealthStatus::Dead)]);
        assert_eq!(manager.active_session_count().await, 0);
    }
}
