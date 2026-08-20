//! `Agent` trait — behavioral contract for coding-agent CLIs.

use async_trait::async_trait;

use crate::types::{AgentEventStream, Attachment, ChatId, CoreError};

/// A coding-agent CLI driven by deapbox. One impl per `AgentKind`.
///
/// Stage 2 streaming shape: `send` returns an `AgentEventStream`
/// (`mpsc::Receiver<AgentEvent>`) so real agents can stream events as they
/// arrive from the subprocess stdout. The agent impl spawns a task that pushes
/// events into the channel and closes it after emitting `TurnEnd`.
///
/// `chat_id` is threaded explicitly (per ADR-0003, avoiding review F4's
/// "trait missing per-chat identity" smell). `attachments` is shape-ready
/// for multimodal agents; echo ignores it.
#[async_trait]
pub trait Agent: Send + Sync {
    /// Process one Operator message within a Turn. Returns an
    /// `AgentEventStream` — a channel receiver that yields `AgentEvent`s as
    /// the agent produces them. The stream ends (returns `None`) after
    /// `TurnEnd` is emitted and the agent impl closes the channel.
    async fn send(
        &self,
        chat_id: &ChatId,
        text: &str,
        attachments: &[Attachment],
    ) -> Result<AgentEventStream, CoreError>;
}
