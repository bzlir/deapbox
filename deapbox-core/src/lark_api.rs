//! `LarkMessageApi` trait — outbound Feishu message boundary.
//!
//! Lives in `deapbox-core` (not `deapbox-lark`) to avoid a workspace cycle:
//! `ChatDispatcher` depends on `Arc<dyn LarkMessageApi>` and must not
//! reverse-depend on the lark crate. Same pattern as `Agent` trait in core,
//! impl in `deapbox-lark`.

use async_trait::async_trait;

use crate::types::{ChatId, LarkApiError};

/// Minimal outbound Feishu API required by Stage 1.
///
/// Stage 2 adds `download_image(image_key) -> bytes` for inbound image
/// attachments, and a card-streaming surface (replacing `send_text` for
/// incremental updates).
#[async_trait]
pub trait LarkMessageApi: Send + Sync {
    /// Send a plain text message to a chat. Stage 1 uses this for every
    /// `AgentEvent` rendering (one event = one text message).
    async fn send_text(&self, chat_id: &ChatId, text: &str) -> Result<(), LarkApiError>;
}
