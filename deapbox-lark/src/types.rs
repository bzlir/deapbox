//! Lark-facing message types.

use deapbox_core::types::{ChatId, UserId, UserMessage};
use serde::{Deserialize, Serialize};

/// Text message received from a Feishu group chat.
///
/// `chat_id` is mandatory on purpose: routing must not depend on process-wide
/// "current chat" state, otherwise two groups can be mixed under load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboundTextMessage {
    pub chat_id: ChatId,
    pub message_id: String,
    pub sender: UserId,
    pub text: String,
    pub timestamp_ms: i64,
}

impl InboundTextMessage {
    pub fn into_user_message(self) -> UserMessage {
        UserMessage {
            chat_id: self.chat_id,
            sender: self.sender,
            text: self.text,
            msg_id: self.message_id,
        }
    }
}

/// Minimal text reply addressed to one explicit Feishu group chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboundTextMessage {
    pub chat_id: ChatId,
    pub text: String,
}

impl OutboundTextMessage {
    pub fn new(chat_id: ChatId, text: impl Into<String>) -> Self {
        Self {
            chat_id,
            text: text.into(),
        }
    }
}
