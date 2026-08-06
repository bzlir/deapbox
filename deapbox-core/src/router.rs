//! 消息路由 — task-per-message，非阻塞。
//!
//! 一条用户消息 = 一个独立 tokio task：解析 binding → `get_or_start` →
//! `subscribe()`（在 send 之前，broadcast 只投递给已存在的接收端，先 send 会
//! 丢首发事件，见 working.md lesson #2）→ `send` → 循环收 `AgentEvent` 到
//! `OutputSink`：`TurnEnd` 写 resume_key、`Exited`/`Failed` 走 `on_error`
//! （不冒充完成）。主循环立即返回 `TurnHandle`，不等完成——一个长 turn 不饿死
//! 其他 chat。

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::broadcast;

use crate::traits::{
    AgentEventReceiver, AgentManager, OutputSink, PersistentStore, Router, TurnHandle,
};
use crate::types::*;

/// 默认路由实现。
///
/// 持 `Arc<dyn PersistentStore>`（读 binding / 写 resume_key）、
/// `Arc<dyn AgentManager>`（`get_or_start`）、`Arc<dyn OutputSink>`（事件下沉）。
/// 三者 `Send + Sync`，可在 spawn 的 task 里 clone。
pub struct RouterImpl {
    store: Arc<dyn PersistentStore>,
    agent_manager: Arc<dyn AgentManager>,
    sink: Arc<dyn OutputSink>,
}

impl RouterImpl {
    pub fn new(
        store: Arc<dyn PersistentStore>,
        agent_manager: Arc<dyn AgentManager>,
        sink: Arc<dyn OutputSink>,
    ) -> Self {
        Self {
            store,
            agent_manager,
            sink,
        }
    }
}

#[async_trait]
impl Router for RouterImpl {
    async fn route_user_message(&self, msg: UserMessage) -> Result<TurnHandle, CoreError> {
        // 1. 解析绑定。无绑定 → `SessionNotFound`（行为固定，测试覆盖）。
        let binding = self
            .store
            .get_session_binding(&msg.chat_id)
            .await?
            .ok_or_else(|| CoreError::SessionNotFound(msg.chat_id.0.clone()))?;

        // 2. 取/起会话。
        let session = self
            .agent_manager
            .get_or_start(&msg.chat_id, &binding)
            .await?;

        // 3. subscribe 必须在 send 之前——broadcast 只投递给已存在的接收端，
        //    先 send 再 subscribe 会丢首发事件。
        let rx = session.subscribe();

        // 4. send（非阻塞，`&self`）。
        session.send(&msg.text).await?;

        // 5. spawn task 收事件到 sink，主循环不等完成。
        let store = Arc::clone(&self.store);
        let sink = Arc::clone(&self.sink);
        let chat_id = msg.chat_id;
        let join = tokio::spawn(async move {
            run_turn_loop(rx, store, sink, chat_id).await;
        });

        Ok(TurnHandle { join })
    }
}

/// turn 事件循环——spawn 的 task 体。
///
/// - `Normalized(e)` → `sink.consume(e)`（best-effort；sink 失败不中断——仍要抓
///   `TurnEnd` 写 resume_key，否则重启后续不上）。
/// - `TurnEnd{resume_key}` → 仅当 `Some(非空)` 写 `set_resume_key`（`None`/空串
///   不覆盖既有 key，保留 resume 链）+ `sink.on_turn_end` → 结束。
/// - `Exited(code)` / `Failed(err)` → `sink.on_error`（**不冒充完成**，不写
///   resume_key）→ 结束。
/// - `Lagged(_)` → 续收（best-effort，丢失事件由 sink 层兜底，不冒充完成）。
/// - `Closed` → 通道无更多事件，视同 `Exited` → `on_error` → 结束。
async fn run_turn_loop(
    mut rx: AgentEventReceiver,
    store: Arc<dyn PersistentStore>,
    sink: Arc<dyn OutputSink>,
    chat_id: ChatId,
) {
    loop {
        match rx.recv().await {
            Ok(AgentEvent::Normalized(event)) => {
                let _ = sink.consume(event).await;
            }
            Ok(AgentEvent::TurnEnd { resume_key }) => {
                if let Some(key) = resume_key.as_deref().filter(|key| !key.is_empty()) {
                    let _ = store.set_resume_key(&chat_id, key).await;
                }
                let _ = sink.on_turn_end(resume_key).await;
                break;
            }
            Ok(AgentEvent::Exited(code)) => {
                let err = CoreError::AgentProcess(format!("agent exited (code={code:?})"));
                let _ = sink.on_error(err).await;
                break;
            }
            Ok(AgentEvent::Failed(err)) => {
                let _ = sink.on_error(err).await;
                break;
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                // 丢若干事件；续收，不冒充完成。
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                let err =
                    CoreError::AgentProcess("agent event stream closed without TurnEnd".to_owned());
                let _ = sink.on_error(err).await;
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, VecDeque},
        path::PathBuf,
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc, Mutex as StdMutex,
        },
    };

    use async_trait::async_trait;
    use tokio::sync::broadcast;

    use super::*;
    use crate::traits::{AgentEventReceiver, AgentSession};

    // ============ fakes ============

    #[derive(Default)]
    struct FakeStore {
        bindings: StdMutex<HashMap<ChatId, Binding>>,
        resumes: StdMutex<HashMap<ChatId, String>>,
        set_resume_calls: StdMutex<Vec<(ChatId, String)>>,
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
            self.set_resume_calls
                .lock()
                .unwrap()
                .push((chat_id.clone(), key.to_owned()));
            self.resumes
                .lock()
                .unwrap()
                .insert(chat_id.clone(), key.to_owned());
            Ok(())
        }
    }

    struct FakeSession {
        alive: Arc<AtomicBool>,
        tx: broadcast::Sender<AgentEvent>,
        script: StdMutex<VecDeque<AgentEvent>>,
        sent_texts: StdMutex<Vec<String>>,
    }

    impl FakeSession {
        fn new(script: Vec<AgentEvent>) -> Arc<Self> {
            let (tx, _rx) = broadcast::channel(64);
            Arc::new(Self {
                alive: Arc::new(AtomicBool::new(true)),
                tx,
                script: StdMutex::new(script.into()),
                sent_texts: StdMutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl AgentSession for FakeSession {
        async fn send(&self, text: &str) -> Result<(), CoreError> {
            self.sent_texts.lock().unwrap().push(text.to_owned());
            let drained: Vec<AgentEvent> = self.script.lock().unwrap().drain(..).collect();
            for event in drained {
                let _ = self.tx.send(event);
            }
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

    struct FakeManager {
        sessions: StdMutex<HashMap<ChatId, Arc<dyn AgentSession>>>,
        get_calls: AtomicUsize,
    }

    impl FakeManager {
        fn with_session(chat: ChatId, session: Arc<dyn AgentSession>) -> Self {
            let mut map = HashMap::new();
            map.insert(chat, session);
            Self {
                sessions: StdMutex::new(map),
                get_calls: AtomicUsize::new(0),
            }
        }

        fn with_sessions(map: HashMap<ChatId, Arc<dyn AgentSession>>) -> Self {
            Self {
                sessions: StdMutex::new(map),
                get_calls: AtomicUsize::new(0),
            }
        }

        fn extract_call_count(&self) -> usize {
            self.get_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl AgentManager for FakeManager {
        async fn get_or_start(
            &self,
            chat: &ChatId,
            _binding: &Binding,
        ) -> Result<Arc<dyn AgentSession>, CoreError> {
            self.get_calls.fetch_add(1, Ordering::SeqCst);
            self.sessions
                .lock()
                .unwrap()
                .get(chat)
                .cloned()
                .map(|session| session as Arc<dyn AgentSession>)
                .ok_or_else(|| CoreError::AgentNotFound(chat.0.clone()))
        }
    }

    #[derive(Default)]
    struct FakeSink {
        consumed: StdMutex<Vec<NormalizedEvent>>,
        turn_ends: StdMutex<Vec<Option<String>>>,
        errors: StdMutex<Vec<CoreError>>,
    }

    #[async_trait]
    impl OutputSink for FakeSink {
        async fn consume(&self, event: NormalizedEvent) -> Result<(), CoreError> {
            self.consumed.lock().unwrap().push(event);
            Ok(())
        }

        async fn on_turn_end(&self, resume_key: Option<String>) -> Result<(), CoreError> {
            self.turn_ends.lock().unwrap().push(resume_key);
            Ok(())
        }

        async fn on_error(&self, err: CoreError) -> Result<(), CoreError> {
            self.errors.lock().unwrap().push(err);
            Ok(())
        }
    }

    // ============ helpers ============

    fn chat(id: &str) -> ChatId {
        ChatId(id.to_owned())
    }

    fn agent(id: &str) -> AgentId {
        AgentId(id.to_owned())
    }

    fn ws(path: &str) -> WorkspacePath {
        WorkspacePath(PathBuf::from(path))
    }

    fn user_msg(chat_id: &ChatId, text: &str) -> UserMessage {
        UserMessage {
            chat_id: chat_id.clone(),
            sender: UserId("ou_test".to_owned()),
            text: text.to_owned(),
            msg_id: "om_test".to_owned(),
        }
    }

    fn router(store: Arc<FakeStore>, manager: Arc<FakeManager>, sink: Arc<FakeSink>) -> RouterImpl {
        RouterImpl::new(
            Arc::clone(&store) as Arc<dyn PersistentStore>,
            Arc::clone(&manager) as Arc<dyn AgentManager>,
            Arc::clone(&sink) as Arc<dyn OutputSink>,
        )
    }

    // ============ tests ============

    #[tokio::test]
    async fn routes_message_and_persists_resume_key_on_turn_end() {
        let store = Arc::new(FakeStore::default());
        let chat = chat("chat-a");
        store
            .set_session_binding(&chat, &agent("claude"), &ws("/work/a"))
            .await
            .unwrap();

        let session = FakeSession::new(vec![
            AgentEvent::Normalized(NormalizedEvent::Thinking("analyzing".to_owned())),
            AgentEvent::Normalized(NormalizedEvent::Text("hello back".to_owned())),
            AgentEvent::TurnEnd {
                resume_key: Some("resume-xyz".to_owned()),
            },
        ]);
        let manager = Arc::new(FakeManager::with_session(chat.clone(), session.clone()));
        let sink = Arc::new(FakeSink::default());
        let router = router(store.clone(), manager.clone(), sink.clone());

        let handle = router
            .route_user_message(user_msg(&chat, "hi"))
            .await
            .unwrap();
        handle.join.await.unwrap();

        // 输出顺序：sink 收到事件顺序与 fake session 输出一致
        assert_eq!(
            sink.consumed.lock().unwrap().clone(),
            vec![
                NormalizedEvent::Thinking("analyzing".to_owned()),
                NormalizedEvent::Text("hello back".to_owned()),
            ]
        );
        // TurnEnd delivered to sink
        assert_eq!(
            sink.turn_ends.lock().unwrap().clone(),
            vec![Some("resume-xyz".to_owned())]
        );
        // resume key persisted with correct arg
        assert_eq!(
            store.resumes.lock().unwrap().get(&chat).cloned(),
            Some("resume-xyz".to_owned())
        );
        assert_eq!(
            store.set_resume_calls.lock().unwrap().clone(),
            vec![(chat.clone(), "resume-xyz".to_owned())]
        );
        // no errors delivered
        assert!(sink.errors.lock().unwrap().is_empty());
        // message text forwarded to session
        assert_eq!(
            session.sent_texts.lock().unwrap().clone(),
            vec!["hi".to_owned()]
        );
        // manager called exactly once
        assert_eq!(manager.extract_call_count(), 1);
    }

    #[tokio::test]
    async fn exited_does_not_pretend_complete_and_writes_no_resume_key() {
        let store = Arc::new(FakeStore::default());
        let chat = chat("chat-a");
        store
            .set_session_binding(&chat, &agent("claude"), &ws("/work/a"))
            .await
            .unwrap();
        // 一个既有 resume key，证明 Exited 不覆盖它
        store.set_resume_key(&chat, "prior-key").await.unwrap();
        store.set_resume_calls.lock().unwrap().clear();

        let session = FakeSession::new(vec![
            AgentEvent::Normalized(NormalizedEvent::Text("partial".to_owned())),
            AgentEvent::Exited(Some(0)),
        ]);
        let manager = Arc::new(FakeManager::with_session(chat.clone(), session));
        let sink = Arc::new(FakeSink::default());
        let router = router(store.clone(), manager, sink.clone());

        let handle = router
            .route_user_message(user_msg(&chat, "go"))
            .await
            .unwrap();
        handle.join.await.unwrap();

        // partial event delivered
        assert_eq!(
            sink.consumed.lock().unwrap().clone(),
            vec![NormalizedEvent::Text("partial".to_owned())]
        );
        // on_error called, NOT on_turn_end
        assert_eq!(sink.turn_ends.lock().unwrap().len(), 0);
        assert_eq!(sink.errors.lock().unwrap().len(), 1);
        assert!(matches!(
            &sink.errors.lock().unwrap()[0],
            CoreError::AgentProcess(msg) if msg.contains("agent exited")
        ));
        // resume key NOT overwritten (no new set_resume_key call)
        assert!(store.set_resume_calls.lock().unwrap().is_empty());
        assert_eq!(
            store.resumes.lock().unwrap().get(&chat).cloned(),
            Some("prior-key".to_owned())
        );
    }

    #[tokio::test]
    async fn failed_delivers_error_and_writes_no_resume_key() {
        let store = Arc::new(FakeStore::default());
        let chat = chat("chat-a");
        store
            .set_session_binding(&chat, &agent("claude"), &ws("/work/a"))
            .await
            .unwrap();

        let session = FakeSession::new(vec![AgentEvent::Failed(CoreError::AgentProcess(
            "kimi exploded".to_owned(),
        ))]);
        let manager = Arc::new(FakeManager::with_session(chat.clone(), session));
        let sink = Arc::new(FakeSink::default());
        let router = router(store.clone(), manager, sink.clone());

        let handle = router
            .route_user_message(user_msg(&chat, "go"))
            .await
            .unwrap();
        handle.join.await.unwrap();

        // no Normalized events, no TurnEnd
        assert!(sink.consumed.lock().unwrap().is_empty());
        assert!(sink.turn_ends.lock().unwrap().is_empty());
        // error forwarded verbatim
        assert_eq!(sink.errors.lock().unwrap().len(), 1);
        assert!(matches!(
            &sink.errors.lock().unwrap()[0],
            CoreError::AgentProcess(msg) if msg == "kimi exploded"
        ));
        // resume key NOT written
        assert!(store.set_resume_calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unbound_chat_returns_session_not_found_without_manager_call() {
        let store = Arc::new(FakeStore::default());
        // chat-b has no binding set
        let manager = Arc::new(FakeManager::with_sessions(HashMap::new()));
        let sink = Arc::new(FakeSink::default());
        let router = router(store.clone(), manager.clone(), sink.clone());

        let chat_b = chat("chat-b");
        let result = router.route_user_message(user_msg(&chat_b, "hi")).await;

        assert!(matches!(result, Err(CoreError::SessionNotFound(id)) if id == "chat-b"));
        // never reached the manager
        assert_eq!(manager.extract_call_count(), 0);
        // never delivered anything to sink
        assert!(sink.consumed.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn two_chats_run_concurrently_without_blocking() {
        let store = Arc::new(FakeStore::default());
        let chat_a = chat("chat-a");
        let chat_b = chat("chat-b");
        store
            .set_session_binding(&chat_a, &agent("claude"), &ws("/work/a"))
            .await
            .unwrap();
        store
            .set_session_binding(&chat_b, &agent("kimi"), &ws("/work/b"))
            .await
            .unwrap();

        let session_a = FakeSession::new(vec![
            AgentEvent::Normalized(NormalizedEvent::Text("reply A".to_owned())),
            AgentEvent::TurnEnd {
                resume_key: Some("resume-a".to_owned()),
            },
        ]);
        let session_b = FakeSession::new(vec![
            AgentEvent::Normalized(NormalizedEvent::Text("reply B".to_owned())),
            AgentEvent::TurnEnd {
                resume_key: Some("resume-b".to_owned()),
            },
        ]);
        let mut sessions: HashMap<ChatId, Arc<dyn AgentSession>> = HashMap::new();
        sessions.insert(chat_a.clone(), session_a);
        sessions.insert(chat_b.clone(), session_b);
        let manager = Arc::new(FakeManager::with_sessions(sessions));
        let sink = Arc::new(FakeSink::default());
        let router = router(store.clone(), manager.clone(), sink.clone());

        // 两条消息都立即返回 TurnHandle（非阻塞）——若 Router 阻塞，第二条
        // route 不会在第一个 turn 完成前返回。
        let handle_a = router
            .route_user_message(user_msg(&chat_a, "hi A"))
            .await
            .unwrap();
        let handle_b = router
            .route_user_message(user_msg(&chat_b, "hi B"))
            .await
            .unwrap();

        // 两个 turn 都能完成
        handle_a.join.await.unwrap();
        handle_b.join.await.unwrap();

        // 各 chat 的 resume key 各自持久化、互不串
        assert_eq!(
            store.resumes.lock().unwrap().get(&chat_a).cloned(),
            Some("resume-a".to_owned())
        );
        assert_eq!(
            store.resumes.lock().unwrap().get(&chat_b).cloned(),
            Some("resume-b".to_owned())
        );
        // sink 收到两个 turn 的 turn-end
        let turn_ends = sink.turn_ends.lock().unwrap().clone();
        assert!(turn_ends.contains(&Some("resume-a".to_owned())));
        assert!(turn_ends.contains(&Some("resume-b".to_owned())));
        assert_eq!(turn_ends.len(), 2);
        // 两条 Normalized 都送达（顺序不互串——A、B 各一条 Text）
        let consumed = sink.consumed.lock().unwrap().clone();
        assert_eq!(consumed.len(), 2);
        assert!(consumed.contains(&NormalizedEvent::Text("reply A".to_owned())));
        assert!(consumed.contains(&NormalizedEvent::Text("reply B".to_owned())));
        // manager 各调一次
        assert_eq!(manager.extract_call_count(), 2);
    }

    #[tokio::test]
    async fn turn_end_with_none_resume_key_leaves_existing_key_untouched() {
        // agent 报 None（无 resume key）时不应覆盖既有 key，否则重启续不上。
        let store = Arc::new(FakeStore::default());
        let chat = chat("chat-a");
        store
            .set_session_binding(&chat, &agent("claude"), &ws("/work/a"))
            .await
            .unwrap();
        store.set_resume_key(&chat, "prior-key").await.unwrap();
        store.set_resume_calls.lock().unwrap().clear();

        let session = FakeSession::new(vec![
            AgentEvent::Normalized(NormalizedEvent::Text("done".to_owned())),
            AgentEvent::TurnEnd { resume_key: None },
        ]);
        let manager = Arc::new(FakeManager::with_session(chat.clone(), session));
        let sink = Arc::new(FakeSink::default());
        let router = router(store.clone(), manager, sink.clone());

        let handle = router
            .route_user_message(user_msg(&chat, "hi"))
            .await
            .unwrap();
        handle.join.await.unwrap();

        // TurnEnd delivered (with None)
        assert_eq!(sink.turn_ends.lock().unwrap().clone(), vec![None]);
        // no set_resume_key call — prior key preserved
        assert!(store.set_resume_calls.lock().unwrap().is_empty());
        assert_eq!(
            store.resumes.lock().unwrap().get(&chat).cloned(),
            Some("prior-key".to_owned())
        );
    }

    #[tokio::test]
    async fn closed_event_stream_is_treated_as_error_not_completion() {
        // 会话 channel 关闭而未发 TurnEnd：视同 Exited，不冒充完成。
        let store = Arc::new(FakeStore::default());
        let chat = chat("chat-a");
        store
            .set_session_binding(&chat, &agent("claude"), &ws("/work/a"))
            .await
            .unwrap();

        // 发完脚本后 drop sender，让 receiver 漏完 buffered 事件后收到 `Closed`。
        let session = ClosingFakeSession::new(vec![AgentEvent::Normalized(NormalizedEvent::Text(
            "then silence".to_owned(),
        ))]);
        let manager = Arc::new(FakeManager::with_session(chat.clone(), session));
        let sink = Arc::new(FakeSink::default());
        let router = router(store.clone(), manager, sink.clone());

        let handle = router
            .route_user_message(user_msg(&chat, "hi"))
            .await
            .unwrap();
        handle.join.await.unwrap();

        assert_eq!(
            sink.consumed.lock().unwrap().clone(),
            vec![NormalizedEvent::Text("then silence".to_owned())]
        );
        assert!(sink.turn_ends.lock().unwrap().is_empty());
        assert_eq!(sink.errors.lock().unwrap().len(), 1);
        assert!(matches!(
            &sink.errors.lock().unwrap()[0],
            CoreError::AgentProcess(msg) if msg.contains("closed without TurnEnd")
        ));
    }

    /// 发完脚本后 drop sender 让 `subscribe()` 端收到 `Closed` 的 fake。
    ///
    /// sender 放 `Option` 里，`send` 末尾 `take()` 掉——此时唯一 sender 消失，
    /// receiver 漏完 buffered 事件后返回 `RecvError::Closed`。
    struct ClosingFakeSession {
        tx: StdMutex<Option<broadcast::Sender<AgentEvent>>>,
        script: StdMutex<VecDeque<AgentEvent>>,
    }

    impl ClosingFakeSession {
        fn new(script: Vec<AgentEvent>) -> Arc<Self> {
            let (tx, _rx) = broadcast::channel(64);
            Arc::new(Self {
                tx: StdMutex::new(Some(tx)),
                script: StdMutex::new(script.into()),
            })
        }
    }

    #[async_trait]
    impl AgentSession for ClosingFakeSession {
        async fn send(&self, text: &str) -> Result<(), CoreError> {
            assert_eq!(text, "hi");
            let drained: Vec<AgentEvent> = self.script.lock().unwrap().drain(..).collect();
            let guard = self.tx.lock().unwrap();
            if let Some(sender) = guard.as_ref() {
                for event in &drained {
                    let _ = sender.send(event.clone());
                }
            }
            drop(guard);
            // drop the only sender → receiver gets Closed after draining buffer
            self.tx.lock().unwrap().take();
            Ok(())
        }

        fn subscribe(&self) -> AgentEventReceiver {
            self.tx
                .lock()
                .unwrap()
                .as_ref()
                .expect("sender alive at subscribe")
                .subscribe()
        }

        async fn interrupt(&self) -> Result<(), CoreError> {
            Ok(())
        }

        fn current_resume_key(&self) -> Option<String> {
            None
        }

        fn alive(&self) -> bool {
            true
        }

        async fn close(self: Box<Self>) -> Result<(), CoreError> {
            Ok(())
        }
    }
}
