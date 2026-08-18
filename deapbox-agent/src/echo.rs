//! `EchoAgent` — Stage 1 test stub `Agent` impl.
//!
//! Behavior: echo the input text verbatim + `TurnEnd{None}`. ADR-0003.
//! Used to validate the "Feishu → router → agent → Feishu" walking skeleton
//! without spawning real agent subprocesses.

use async_trait::async_trait;

use deapbox_core::agent::Agent;
use deapbox_core::types::{AgentEvent, Attachment, ChatId, CoreError};

/// Stateless echo agent. Multiple chats can share one `Arc<EchoAgent>`.
#[derive(Debug, Default)]
pub struct EchoAgent;

impl EchoAgent {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Agent for EchoAgent {
    async fn send(
        &self,
        _chat_id: &ChatId,
        text: &str,
        _attachments: &[Attachment],
    ) -> Result<Vec<AgentEvent>, CoreError> {
        Ok(vec![
            AgentEvent::Text(text.to_owned()),
            AgentEvent::TurnEnd { resume_key: None },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deapbox_core::types::{Attachment, ChatId};

    fn chat() -> ChatId {
        ChatId("oc_test".to_owned())
    }

    // ============ V4.1: original text echoed verbatim ============

    #[tokio::test]
    async fn v4_1_echo_returns_text_verbatim_and_turn_end() {
        let agent = EchoAgent::new();
        let events = agent.send(&chat(), "hello", &[]).await.unwrap();
        assert_eq!(
            events,
            vec![
                AgentEvent::Text("hello".to_owned()),
                AgentEvent::TurnEnd { resume_key: None },
            ]
        );
    }

    // ============ V4.2: non-ASCII text echoed verbatim ============

    #[tokio::test]
    async fn v4_2_non_ascii_text_echoed_verbatim() {
        let agent = EchoAgent::new();
        let events = agent.send(&chat(), "中文测试 🎉", &[]).await.unwrap();
        assert_eq!(
            events,
            vec![
                AgentEvent::Text("中文测试 🎉".to_owned()),
                AgentEvent::TurnEnd { resume_key: None },
            ]
        );
    }

    // ============ V4.3: empty text echoed as empty string ============

    #[tokio::test]
    async fn v4_3_empty_text_echoed_as_empty_string() {
        let agent = EchoAgent::new();
        let events = agent.send(&chat(), "", &[]).await.unwrap();
        assert_eq!(
            events,
            vec![
                AgentEvent::Text("".to_owned()),
                AgentEvent::TurnEnd { resume_key: None },
            ]
        );
    }

    // ============ V4.4: attachments are ignored ============

    #[tokio::test]
    async fn v4_4_attachments_are_ignored_by_echo() {
        let agent = EchoAgent::new();
        let attachments = vec![Attachment::Image {
            image_key: "img_test_key".to_owned(),
        }];
        let events = agent.send(&chat(), "hello", &attachments).await.unwrap();
        // Same as V4.1 — attachments don't change echo behavior
        assert_eq!(
            events,
            vec![
                AgentEvent::Text("hello".to_owned()),
                AgentEvent::TurnEnd { resume_key: None },
            ]
        );
    }

    // ============ multiple chats sharing one EchoAgent ============

    #[tokio::test]
    async fn multiple_chats_sharing_one_echo_agent_all_get_correct_echo() {
        let agent = EchoAgent::new();
        let events_a = agent
            .send(&ChatId("oc_a".to_owned()), "from A", &[])
            .await
            .unwrap();
        let events_b = agent
            .send(&ChatId("oc_b".to_owned()), "from B", &[])
            .await
            .unwrap();
        assert_eq!(events_a[0], AgentEvent::Text("from A".to_owned()));
        assert_eq!(events_b[0], AgentEvent::Text("from B".to_owned()));
    }
}
