//! `Agent` trait — behavioral contract for coding-agent CLIs.

use async_trait::async_trait;

use crate::types::{AgentEvent, Attachment, ChatId, CoreError};

/// A coding-agent CLI driven by deapbox. One impl per `AgentKind`.
///
/// Stage 1: only `EchoAgent`. Stage 2: per-kind impls (claude-code, kimi-code,
/// opencode, codex) replace the batch `Vec<AgentEvent>` return with a
/// streaming `Receiver<AgentEvent>` (per ADR-0003).
#[async_trait]
pub trait Agent: Send + Sync {
    /// Process one Operator message within a Turn. Returns the Agent's
    /// structured reply as a batch of `AgentEvent`s.
    ///
    /// `chat_id` is threaded explicitly (per ADR-0003, avoiding review F4's
    /// "trait missing per-chat identity" smell). `attachments` is shape-ready
    /// for Stage 2 multimodal agents; Stage 1 echo ignores it.
    async fn send(
        &self,
        chat_id: &ChatId,
        text: &str,
        attachments: &[Attachment],
    ) -> Result<Vec<AgentEvent>, CoreError>;
}
