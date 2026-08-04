//! Inbound Lark event parsing and forwarding.

use deapbox_core::types::{ChatId, UserId, UserMessage};
use serde::Deserialize;
use tokio::sync::mpsc;

use crate::types::InboundTextMessage;

#[derive(Debug, thiserror::Error)]
pub enum LarkEventError {
    #[error("unsupported Lark event type: {0}")]
    UnsupportedEventType(String),
    #[error("unsupported Lark message type: {0}")]
    UnsupportedMessageType(String),
    #[error("missing required Lark field: {0}")]
    MissingField(&'static str),
    #[error("invalid Lark event payload: {0}")]
    InvalidEventPayload(serde_json::Error),
    #[error("invalid Lark text message content: {0}")]
    InvalidTextContent(serde_json::Error),
    #[error("failed to forward Lark event to console: {0}")]
    Forward(String),
}

#[derive(Debug)]
pub struct LarkEventBridge {
    tx: mpsc::Sender<UserMessage>,
}

impl LarkEventBridge {
    pub fn new(tx: mpsc::Sender<UserMessage>) -> Self {
        Self { tx }
    }

    pub async fn handle_event_payload(&self, payload: &[u8]) -> Result<(), LarkEventError> {
        let message = parse_text_message(payload)?;
        self.tx
            .send(message.into_user_message())
            .await
            .map_err(|err| LarkEventError::Forward(err.to_string()))
    }
}

pub fn parse_text_message(payload: &[u8]) -> Result<InboundTextMessage, LarkEventError> {
    let event: LarkEventEnvelope =
        serde_json::from_slice(payload).map_err(LarkEventError::InvalidEventPayload)?;

    if event.header.event_type != "im.message.receive_v1" {
        return Err(LarkEventError::UnsupportedEventType(
            event.header.event_type,
        ));
    }

    if event.event.message.message_type != "text" {
        return Err(LarkEventError::UnsupportedMessageType(
            event.event.message.message_type,
        ));
    }

    let text: TextContent = serde_json::from_str(&event.event.message.content)
        .map_err(LarkEventError::InvalidTextContent)?;

    Ok(InboundTextMessage {
        chat_id: ChatId(required(
            event.event.message.chat_id,
            "event.message.chat_id",
        )?),
        message_id: event.event.message.message_id,
        sender: UserId(required(
            event.event.sender.sender_id.open_id,
            "event.sender.sender_id.open_id",
        )?),
        text: text.text,
        timestamp_ms: parse_timestamp_ms(
            &event.event.message.create_time,
            event.header.create_time,
        ),
    })
}

fn required(value: Option<String>, field: &'static str) -> Result<String, LarkEventError> {
    value
        .filter(|text| !text.is_empty())
        .ok_or(LarkEventError::MissingField(field))
}

fn parse_timestamp_ms(message_time: &str, header_time: Option<String>) -> i64 {
    message_time
        .parse()
        .ok()
        .or_else(|| header_time.and_then(|time| time.parse().ok()))
        .unwrap_or_default()
}

#[derive(Debug, Deserialize)]
struct LarkEventEnvelope {
    header: LarkEventHeader,
    event: LarkMessageReceiveEvent,
}

#[derive(Debug, Deserialize)]
struct LarkEventHeader {
    event_type: String,
    #[serde(default)]
    create_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LarkMessageReceiveEvent {
    sender: LarkSender,
    message: LarkMessage,
}

#[derive(Debug, Deserialize)]
struct LarkSender {
    sender_id: LarkSenderId,
}

#[derive(Debug, Deserialize)]
struct LarkSenderId {
    open_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LarkMessage {
    message_id: String,
    #[serde(default)]
    chat_id: Option<String>,
    #[serde(default)]
    create_time: String,
    message_type: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct TextContent {
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use serde_json::Value;

    #[test]
    fn parses_text_messages_with_distinct_chat_ids() {
        let a = fixture("oc_a", "ou_same_user", "om_a", "hello A", "111");
        let b = fixture("oc_b", "ou_same_user", "om_b", "hello B", "222");

        let first = parse_text_message(a.to_string().as_bytes()).unwrap();
        let second = parse_text_message(b.to_string().as_bytes()).unwrap();

        assert_eq!(first.chat_id, ChatId("oc_a".to_string()));
        assert_eq!(first.sender, UserId("ou_same_user".to_string()));
        assert_eq!(first.text, "hello A");
        assert_eq!(first.timestamp_ms, 111);
        assert_eq!(second.chat_id, ChatId("oc_b".to_string()));
        assert_eq!(second.sender, UserId("ou_same_user".to_string()));
        assert_eq!(second.text, "hello B");
        assert_eq!(second.timestamp_ms, 222);
    }

    #[test]
    fn rejects_missing_chat_id() {
        let mut value = fixture("oc_a", "ou_sender", "om_a", "hello", "111");
        value["event"]["message"]["chat_id"] = Value::Null;

        let err = parse_text_message(value.to_string().as_bytes()).unwrap_err();

        assert!(matches!(
            err,
            LarkEventError::MissingField("event.message.chat_id")
        ));
    }

    #[test]
    fn rejects_non_text_message() {
        let mut value = fixture("oc_a", "ou_sender", "om_a", "ignored", "111");
        value["event"]["message"]["message_type"] = json!("image");

        let err = parse_text_message(value.to_string().as_bytes()).unwrap_err();

        assert!(matches!(err, LarkEventError::UnsupportedMessageType(_)));
    }

    #[tokio::test]
    async fn bridge_forwards_user_message_without_losing_chat_id() {
        let (tx, mut rx) = mpsc::channel(1);
        let bridge = LarkEventBridge::new(tx);
        let payload = fixture("oc_target", "ou_sender", "om_1", "reply here", "333");

        bridge
            .handle_event_payload(payload.to_string().as_bytes())
            .await
            .unwrap();
        let message = rx.recv().await.unwrap();

        assert_eq!(message.chat_id, ChatId("oc_target".to_string()));
        assert_eq!(message.sender, UserId("ou_sender".to_string()));
        assert_eq!(message.msg_id, "om_1");
        assert_eq!(message.text, "reply here");
    }

    fn fixture(
        chat_id: &str,
        sender: &str,
        message_id: &str,
        text: &str,
        create_time: &str,
    ) -> Value {
        json!({
            "schema": "2.0",
            "header": {
                "event_type": "im.message.receive_v1",
                "create_time": create_time
            },
            "event": {
                "sender": {
                    "sender_id": {
                        "open_id": sender
                    }
                },
                "message": {
                    "chat_id": chat_id,
                    "message_id": message_id,
                    "message_type": "text",
                    "create_time": create_time,
                    "content": serde_json::to_string(&json!({ "text": text })).unwrap()
                }
            }
        })
    }
}
