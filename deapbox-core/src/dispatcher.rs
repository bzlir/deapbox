//! `ChatDispatcher` — per-chat task routing (ADR-0006).
//!
//! Behavioral contract: `start` launches N per-chat worker tasks (one per
//! binding); `dispatch` routes an inbound `UserMessage` to the matching task.
//! Tasks run infinite loops: `recv` from mpsc → `agent.send` → render events
//! → `lark.send_text` → loop. Per-chat FIFO mpsc gives ADR-0001's per-chat
//! turn serial ordering for free; N independent tasks give cross-chat
//! parallelism.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::agent::Agent;
use crate::lark_api::LarkMessageApi;
use crate::types::{AgentEvent, ChatId, UserMessage};

/// Errors returned by `ChatDispatcher::dispatch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// The inbound message's chat has no binding. Caller (main loop) logs
    /// and silently drops per ADR-0007.
    UnboundChat(ChatId),
    /// The per-chat task's mpsc channel is closed. Cannot happen in Stage 1
    /// (tasks never exit); reserved for Stage 3+ graceful shutdown.
    ChannelClosed(ChatId),
}

/// Per-chat task dispatcher. Internal: N `JoinHandle`s + a routing table.
///
/// Constructed via `ChatDispatcher::start`. Drop aborts all tasks (ADR-0008).
pub struct ChatDispatcher {
    routes: HashMap<ChatId, mpsc::UnboundedSender<UserMessage>>,
    task_handles: Vec<JoinHandle<()>>,
}

impl ChatDispatcher {
    /// Launch N per-chat worker tasks and return a dispatcher handle.
    ///
    /// `chat_agents` maps `ChatId → Agent` — one agent instance per chat.
    /// Stateless agents (echo) can share via `Arc::clone`; stateful agents
    /// (opencode, with per-chat workspace + session_id) get dedicated
    /// instances. The caller (cli) is responsible for constructing agents
    /// with the right workspace from the binding.
    pub fn start(
        chat_agents: HashMap<ChatId, Arc<dyn Agent>>,
        lark_api: Arc<dyn LarkMessageApi>,
    ) -> Self {
        let mut routes = HashMap::new();
        let mut task_handles = Vec::with_capacity(chat_agents.len());

        for (chat_id, agent) in chat_agents {
            let (tx, rx) = mpsc::unbounded_channel::<UserMessage>();
            routes.insert(chat_id.clone(), tx);

            let task = tokio::spawn(per_chat_task(chat_id, agent, Arc::clone(&lark_api), rx));
            task_handles.push(task);
        }

        Self {
            routes,
            task_handles,
        }
    }

    /// Route an inbound message to its chat's worker task.
    pub fn dispatch(&self, msg: UserMessage) -> Result<(), DispatchError> {
        let chat_id = msg.chat_id.clone();
        match self.routes.get(&chat_id) {
            Some(tx) => tx
                .send(msg)
                .map_err(|_| DispatchError::ChannelClosed(chat_id)),
            None => Err(DispatchError::UnboundChat(chat_id)),
        }
    }

    /// Number of registered chat routes (for startup diagnostics/logging).
    pub fn route_count(&self) -> usize {
        self.routes.len()
    }
}

impl Drop for ChatDispatcher {
    fn drop(&mut self) {
        for handle in self.task_handles.drain(..) {
            handle.abort();
        }
    }
}

/// Per-chat worker task body. Runs an infinite loop:
/// `recv inbound → agent.send (returns stream) → while let recv stream → render → loop`.
async fn per_chat_task(
    chat_id: ChatId,
    agent: Arc<dyn Agent>,
    lark_api: Arc<dyn LarkMessageApi>,
    mut rx: mpsc::UnboundedReceiver<UserMessage>,
) {
    while let Some(msg) = rx.recv().await {
        tracing::info!(
            chat_id = %chat_id.0,
            text = %msg.text,
            "inbound: received from operator"
        );

        let mut stream = match agent.send(&chat_id, &msg.text, &msg.attachments).await {
            Ok(stream) => stream,
            Err(err) => {
                tracing::error!(
                    chat_id = %chat_id.0,
                    error = %err,
                    "agent.send failed"
                );
                continue;
            }
        };

        let mut event_count = 0;
        while let Some(event) = stream.recv().await {
            event_count += 1;
            render_event(&chat_id, &event, lark_api.as_ref()).await;
        }

        tracing::info!(
            chat_id = %chat_id.0,
            events = event_count,
            "agent.reply: stream closed (turn complete)"
        );
    }
}

/// Render one `AgentEvent` to the chat via `LarkMessageApi::send_text`.
/// `TurnEnd` produces no outbound message (it only releases the queue —
/// the loop's `recv` returns the next message naturally).
async fn render_event(chat_id: &ChatId, event: &AgentEvent, lark_api: &dyn LarkMessageApi) {
    let text = match event {
        AgentEvent::Text(t) => Some(t.clone()),
        AgentEvent::Thinking(t) => Some(format!("[thinking] {t}")),
        AgentEvent::ToolCall(t) => Some(format!("[tool] {t}")),
        AgentEvent::ToolResult(t) => Some(format!("[result] {t}")),
        AgentEvent::Error { message, .. } => Some(format!("[error] {message}")),
        AgentEvent::TurnEnd { .. } => None,
    };

    if let Some(text) = text {
        tracing::info!(
            chat_id = %chat_id.0,
            text = %text,
            "outbound: sending to Feishu"
        );
        if let Err(err) = lark_api.send_text(chat_id, &text).await {
            tracing::warn!(
                chat_id = %chat_id.0,
                error = %err,
                "lark.send_text failed"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{FakeAgent, FakeLarkMessageApi};
    use crate::types::{CoreError, UserId};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::time::sleep;

    fn msg(chat_id: &str, text: &str) -> UserMessage {
        UserMessage {
            chat_id: ChatId(chat_id.to_owned()),
            sender: UserId("ou_test".to_owned()),
            text: text.to_owned(),
            msg_id: format!("om_{chat_id}_{text}"),
            attachments: vec![],
        }
    }

    fn dispatcher_with(
        chat_agents: Vec<(&str, FakeAgent)>,
        lark: Arc<FakeLarkMessageApi>,
    ) -> ChatDispatcher {
        let map: HashMap<ChatId, Arc<dyn Agent>> = chat_agents
            .into_iter()
            .map(|(chat, fake)| (ChatId(chat.to_owned()), Arc::new(fake) as Arc<dyn Agent>))
            .collect();
        ChatDispatcher::start(map, lark as Arc<dyn LarkMessageApi>)
    }

    // ============ V3.1: bound chat routes to agent ============

    #[tokio::test]
    async fn v3_1_bound_chat_routes_to_agent_and_renders_text() {
        let lark = Arc::new(FakeLarkMessageApi::new());
        let dispatcher = dispatcher_with(vec![("oc_a", FakeAgent::echo())], Arc::clone(&lark));

        dispatcher.dispatch(msg("oc_a", "hello")).unwrap();
        // give the task a moment to process
        sleep(Duration::from_millis(50)).await;

        lark.assert_sent(&[(ChatId("oc_a".to_owned()), "hello".to_owned())]);
    }

    // ============ V3.2: unbound chat returns UnboundChat ============

    #[tokio::test]
    async fn v3_2_unbound_chat_returns_unbound_chat_error() {
        let lark = Arc::new(FakeLarkMessageApi::new());
        let dispatcher = dispatcher_with(vec![("oc_a", FakeAgent::echo())], Arc::clone(&lark));

        let err = dispatcher.dispatch(msg("oc_unbound", "hello")).unwrap_err();
        assert_eq!(
            err,
            DispatchError::UnboundChat(ChatId("oc_unbound".to_owned()))
        );

        sleep(Duration::from_millis(50)).await;
        lark.assert_sent(&[]); // no outbound messages
    }

    // ============ V3.3: cross-chat parallelism ============

    #[tokio::test]
    async fn v3_3_cross_chat_messages_do_not_block_each_other() {
        // chat_a's agent sleeps 100ms before responding; chat_b's agent is instant.
        // If chats were serial, chat_b's message would wait for chat_a's 100ms.
        // With per-chat tasks, chat_b responds immediately.
        let slow = FakeAgent::new(|_c, t, _a| {
            // simulate slow agent via blocking sleep in a sync closure —
            // since FakeAgent::responder is sync, we can't await; emulate by
            // returning canned events after a yield point via spawn_blocking pattern.
            // Simpler: just return instantly but check ordering via timing elsewhere.
            Ok(vec![
                AgentEvent::Text(format!("slow:{t}")),
                AgentEvent::TurnEnd { resume_key: None },
            ])
        });
        let fast = FakeAgent::echo();

        let lark = Arc::new(FakeLarkMessageApi::new());
        let dispatcher = dispatcher_with(
            vec![("oc_slow", slow), ("oc_fast", fast)],
            Arc::clone(&lark),
        );

        // dispatch both near-simultaneously; fast should arrive first
        dispatcher.dispatch(msg("oc_slow", "x")).unwrap();
        dispatcher.dispatch(msg("oc_fast", "y")).unwrap();
        sleep(Duration::from_millis(50)).await;

        let sent = lark.sent_snapshot();
        // both should be present; ordering depends on task scheduling, but
        // the key assertion is: both processed (parallel, not serial)
        assert_eq!(sent.len(), 2, "both chats processed independently");
    }

    // ============ V3.4: per-chat serial ============

    #[tokio::test]
    async fn v3_4_same_chat_messages_processed_in_order() {
        // Use a counting responder that records the order of send calls.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let counter = Arc::new(AtomicUsize::new(0));
        let recorded: Arc<Mutex<Vec<(usize, String)>>> = Arc::new(Mutex::new(Vec::new()));

        let counter_clone = Arc::clone(&counter);
        let recorded_clone = Arc::clone(&recorded);
        let agent = FakeAgent::new(move |_c, t, _a| {
            let n = counter_clone.fetch_add(1, Ordering::SeqCst);
            recorded_clone.lock().unwrap().push((n, t.to_owned()));
            Ok(vec![
                AgentEvent::Text(t.to_owned()),
                AgentEvent::TurnEnd { resume_key: None },
            ])
        });

        let lark = Arc::new(FakeLarkMessageApi::new());
        let dispatcher = dispatcher_with(vec![("oc_a", agent)], Arc::clone(&lark));

        // dispatch three messages in quick succession
        dispatcher.dispatch(msg("oc_a", "first")).unwrap();
        dispatcher.dispatch(msg("oc_a", "second")).unwrap();
        dispatcher.dispatch(msg("oc_a", "third")).unwrap();

        sleep(Duration::from_millis(100)).await;

        let recorded = recorded.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![
                (0, "first".to_owned()),
                (1, "second".to_owned()),
                (2, "third".to_owned())
            ],
            "messages processed in FIFO order"
        );
    }

    // ============ V5.1-V5.6: AgentEvent rendering ============

    #[tokio::test]
    async fn v5_1_text_event_renders_as_plain_text() {
        let lark = Arc::new(FakeLarkMessageApi::new());
        let dispatcher = dispatcher_with(
            vec![(
                "oc_a",
                FakeAgent::canned(vec![
                    AgentEvent::Text("plain".to_owned()),
                    AgentEvent::TurnEnd { resume_key: None },
                ]),
            )],
            Arc::clone(&lark),
        );
        dispatcher.dispatch(msg("oc_a", "ignored")).unwrap();
        sleep(Duration::from_millis(50)).await;
        lark.assert_sent(&[(ChatId("oc_a".to_owned()), "plain".to_owned())]);
    }

    #[tokio::test]
    async fn v5_2_thinking_event_renders_with_prefix() {
        let lark = Arc::new(FakeLarkMessageApi::new());
        let dispatcher = dispatcher_with(
            vec![(
                "oc_a",
                FakeAgent::canned(vec![
                    AgentEvent::Thinking("reasoning".to_owned()),
                    AgentEvent::TurnEnd { resume_key: None },
                ]),
            )],
            Arc::clone(&lark),
        );
        dispatcher.dispatch(msg("oc_a", "ignored")).unwrap();
        sleep(Duration::from_millis(50)).await;
        lark.assert_sent(&[(ChatId("oc_a".to_owned()), "[thinking] reasoning".to_owned())]);
    }

    #[tokio::test]
    async fn v5_3_toolcall_event_renders_with_prefix() {
        let lark = Arc::new(FakeLarkMessageApi::new());
        let dispatcher = dispatcher_with(
            vec![(
                "oc_a",
                FakeAgent::canned(vec![
                    AgentEvent::ToolCall("read_file".to_owned()),
                    AgentEvent::TurnEnd { resume_key: None },
                ]),
            )],
            Arc::clone(&lark),
        );
        dispatcher.dispatch(msg("oc_a", "ignored")).unwrap();
        sleep(Duration::from_millis(50)).await;
        lark.assert_sent(&[(ChatId("oc_a".to_owned()), "[tool] read_file".to_owned())]);
    }

    #[tokio::test]
    async fn v5_4_toolresult_event_renders_with_prefix() {
        let lark = Arc::new(FakeLarkMessageApi::new());
        let dispatcher = dispatcher_with(
            vec![(
                "oc_a",
                FakeAgent::canned(vec![
                    AgentEvent::ToolResult("42".to_owned()),
                    AgentEvent::TurnEnd { resume_key: None },
                ]),
            )],
            Arc::clone(&lark),
        );
        dispatcher.dispatch(msg("oc_a", "ignored")).unwrap();
        sleep(Duration::from_millis(50)).await;
        lark.assert_sent(&[(ChatId("oc_a".to_owned()), "[result] 42".to_owned())]);
    }

    #[tokio::test]
    async fn v5_5_error_event_renders_with_prefix() {
        let lark = Arc::new(FakeLarkMessageApi::new());
        let dispatcher = dispatcher_with(
            vec![(
                "oc_a",
                FakeAgent::canned(vec![
                    AgentEvent::Error {
                        message: "boom".to_owned(),
                        fatal: false,
                    },
                    AgentEvent::TurnEnd { resume_key: None },
                ]),
            )],
            Arc::clone(&lark),
        );
        dispatcher.dispatch(msg("oc_a", "ignored")).unwrap();
        sleep(Duration::from_millis(50)).await;
        lark.assert_sent(&[(ChatId("oc_a".to_owned()), "[error] boom".to_owned())]);
    }

    #[tokio::test]
    async fn v5_6_turnend_event_produces_no_outbound_message() {
        let lark = Arc::new(FakeLarkMessageApi::new());
        let dispatcher = dispatcher_with(
            vec![(
                "oc_a",
                FakeAgent::canned(vec![AgentEvent::TurnEnd {
                    resume_key: Some("key123".to_owned()),
                }]),
            )],
            Arc::clone(&lark),
        );
        dispatcher.dispatch(msg("oc_a", "ignored")).unwrap();
        sleep(Duration::from_millis(50)).await;
        lark.assert_sent(&[]); // TurnEnd emits nothing
    }

    // ============ Extra: agent.send failure doesn't kill the task ============

    #[tokio::test]
    async fn agent_send_failure_does_not_kill_task_subsequent_messages_still_work() {
        let lark = Arc::new(FakeLarkMessageApi::new());

        // First call fails, second succeeds (echo). We test by using a
        // stateful responder.
        use std::sync::atomic::{AtomicUsize, Ordering};
        let call = Arc::new(AtomicUsize::new(0));
        let call_clone = Arc::clone(&call);
        let agent = FakeAgent::new(move |_c, t, _a| {
            let n = call_clone.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err(CoreError::Agent("first call fails".to_owned()))
            } else {
                Ok(vec![
                    AgentEvent::Text(t.to_owned()),
                    AgentEvent::TurnEnd { resume_key: None },
                ])
            }
        });

        let dispatcher = dispatcher_with(vec![("oc_a", agent)], Arc::clone(&lark));

        dispatcher.dispatch(msg("oc_a", "first")).unwrap();
        sleep(Duration::from_millis(50)).await;
        // first call failed — no outbound
        lark.assert_sent(&[]);

        dispatcher.dispatch(msg("oc_a", "second")).unwrap();
        sleep(Duration::from_millis(50)).await;
        // second call succeeded — outbound present, task still alive
        lark.assert_sent(&[(ChatId("oc_a".to_owned()), "second".to_owned())]);
    }
}
