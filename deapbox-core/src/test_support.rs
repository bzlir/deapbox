//! Shared test fakes for `deapbox-core` unit tests + downstream integration tests.
//!
//! Directly applies the architecture-review F5 lesson: in the old code,
//! `FakeStore` was duplicated across `router.rs::tests` and
//! `agent_manager.rs::tests`. Shared fakes here avoid that duplication.

#![cfg(any(test, feature = "test-support"))]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::agent::Agent;
use crate::lark_api::LarkMessageApi;
use crate::types::{AgentEvent, Attachment, ChatId, CoreError, LarkApiError};

/// Responder closure type for `FakeAgent`.
type Responder =
    Arc<dyn Fn(&ChatId, &str, &[Attachment]) -> Result<Vec<AgentEvent>, CoreError> + Send + Sync>;

// ============ FakeAgent ============

/// A scripted `Agent` impl for tests.
///
/// `responder` is called for each `send`; the returned `Vec<AgentEvent>` is
/// what `dispatch` will render. Default behavior: echo the input text +
/// `TurnEnd{None}` (matches `EchoAgent`).
pub struct FakeAgent {
    pub responder: Responder,
}

impl FakeAgent {
    pub fn new<F>(responder: F) -> Self
    where
        F: Fn(&ChatId, &str, &[Attachment]) -> Result<Vec<AgentEvent>, CoreError>
            + Send
            + Sync
            + 'static,
    {
        Self {
            responder: Arc::new(responder),
        }
    }

    /// Echo the input text verbatim + `TurnEnd{None}` (matches `EchoAgent`).
    pub fn echo() -> Self {
        Self::new(|_chat, text, _| {
            Ok(vec![
                AgentEvent::Text(text.to_owned()),
                AgentEvent::TurnEnd { resume_key: None },
            ])
        })
    }

    /// Always return the provided canned events, ignoring input.
    pub fn canned(events: Vec<AgentEvent>) -> Self {
        let events = Arc::new(events);
        Self::new(move |_, _, _| Ok(events.to_vec()))
    }

    /// Always fail with the provided error.
    pub fn failing(err: CoreError) -> Self {
        Self::new(move |_, _, _| Err(err.clone()))
    }
}

#[async_trait]
impl Agent for FakeAgent {
    async fn send(
        &self,
        chat_id: &ChatId,
        text: &str,
        attachments: &[Attachment],
    ) -> Result<Vec<AgentEvent>, CoreError> {
        (self.responder)(chat_id, text, attachments)
    }
}

// ============ FakeLarkMessageApi ============

/// Records every `send_text` call for assertion. Thread-safe via `Mutex`.
#[derive(Default, Debug)]
pub struct FakeLarkMessageApi {
    pub sent: Mutex<Vec<(ChatId, String)>>,
}

impl FakeLarkMessageApi {
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot all recorded sends.
    pub fn sent_snapshot(&self) -> Vec<(ChatId, String)> {
        self.sent.lock().expect("sent mutex poisoned").clone()
    }

    /// Assert the recorded sends match the expected (chat_id, text) pairs.
    pub fn assert_sent(&self, expected: &[(ChatId, String)]) {
        let actual = self.sent_snapshot();
        assert_eq!(actual, expected.to_vec(), "sent messages mismatch");
    }
}

#[async_trait]
impl LarkMessageApi for FakeLarkMessageApi {
    async fn send_text(&self, chat_id: &ChatId, text: &str) -> Result<(), LarkApiError> {
        self.sent
            .lock()
            .expect("sent mutex poisoned")
            .push((chat_id.clone(), text.to_owned()));
        Ok(())
    }
}
