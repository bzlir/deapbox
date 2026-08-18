//! Inbound Lark event parsing — `im.message.receive_v1` → `UserMessage`.
//!
//! Stage 1: text messages only. Non-text (image/post/file) are rejected with
//! `UnsupportedMessageType` and logged + dropped by the caller (ADR-0005).
//! `attachments` is always `vec![]` in Stage 1 — the `Attachment::Image`
//! shape is reserved for Stage 2 (ADR-0003).

use deapbox_core::types::{ChatId, UserId, UserMessage};
use serde::Deserialize;

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
}

/// Parse an `im.message.receive_v1` event payload into a `UserMessage`.
///
/// `payload` is the raw JSON bytes forwarded by the openlark WS client.
/// Non-text messages and missing required fields return `LarkEventError`.
pub fn parse_text_message(payload: &[u8]) -> Result<UserMessage, LarkEventError> {
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

    Ok(UserMessage {
        chat_id: ChatId(required(
            event.event.message.chat_id,
            "event.message.chat_id",
        )?),
        sender: UserId(required(
            event.event.sender.sender_id.open_id,
            "event.sender.sender_id.open_id",
        )?),
        text: text.text,
        msg_id: event.event.message.message_id,
        attachments: vec![], // Stage 1: always empty; Stage 2 fills from image messages
    })
}

fn required(value: Option<String>, field: &'static str) -> Result<String, LarkEventError> {
    value
        .filter(|text| !text.is_empty())
        .ok_or(LarkEventError::MissingField(field))
}

#[derive(Debug, Deserialize)]
struct LarkEventEnvelope {
    header: LarkEventHeader,
    event: LarkMessageReceiveEvent,
}

#[derive(Debug, Deserialize)]
struct LarkEventHeader {
    event_type: String,
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

    // ============ V6.1: legal text message parses to UserMessage ============

    #[test]
    fn v6_1_legal_text_message_parses_to_user_message() {
        let payload = fixture("oc_a", "ou_sender", "om_1", "hello world", "111");
        let msg = parse_text_message(payload.to_string().as_bytes()).unwrap();

        assert_eq!(msg.chat_id, ChatId("oc_a".to_owned()));
        assert_eq!(msg.sender, UserId("ou_sender".to_owned()));
        assert_eq!(msg.text, "hello world");
        assert_eq!(msg.msg_id, "om_1");
        assert!(msg.attachments.is_empty());
    }

    // ============ V6.2: non-text message is rejected ============

    #[test]
    fn v6_2_non_text_message_rejected_with_unsupported_message_type() {
        let mut payload = fixture("oc_a", "ou_sender", "om_1", "ignored", "111");
        payload["event"]["message"]["message_type"] = json!("image");

        let err = parse_text_message(payload.to_string().as_bytes()).unwrap_err();
        assert!(matches!(err, LarkEventError::UnsupportedMessageType(_)));
    }

    // ============ V6.3: missing chat_id is rejected ============

    #[test]
    fn v6_3_missing_chat_id_rejected_with_missing_field() {
        let mut payload = fixture("oc_a", "ou_sender", "om_1", "hello", "111");
        payload["event"]["message"]["chat_id"] = Value::Null;

        let err = parse_text_message(payload.to_string().as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            LarkEventError::MissingField("event.message.chat_id")
        ));
    }

    // ============ V6.4: missing sender.open_id is rejected ============

    #[test]
    fn v6_4_missing_sender_open_id_rejected_with_missing_field() {
        let mut payload = fixture("oc_a", "ou_sender", "om_1", "hello", "111");
        payload["event"]["sender"]["sender_id"]["open_id"] = Value::Null;

        let err = parse_text_message(payload.to_string().as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            LarkEventError::MissingField("event.sender.sender_id.open_id")
        ));
    }

    // ============ V6.5: p2p and group both carry chat_id ============

    #[test]
    fn v6_5_p2p_chat_with_chat_id_parses_correctly() {
        // p2p chats also carry chat_id (oc_ prefix); ADR-0005 treats them
        // uniformly — no chat_type branching in Stage 1.
        let payload = fixture("oc_p2p_chat", "ou_user", "om_p2p", "private hello", "222");
        let msg = parse_text_message(payload.to_string().as_bytes()).unwrap();
        assert_eq!(msg.chat_id, ChatId("oc_p2p_chat".to_owned()));
        assert_eq!(msg.text, "private hello");
    }

    // ============ Extra: unsupported event type ============

    #[test]
    fn unsupported_event_type_rejected() {
        let mut payload = fixture("oc_a", "ou_sender", "om_1", "hello", "111");
        payload["header"]["event_type"] = json!("some.other.event");

        let err = parse_text_message(payload.to_string().as_bytes()).unwrap_err();
        assert!(matches!(err, LarkEventError::UnsupportedEventType(_)));
    }

    // ============ Extra: invalid JSON ============

    #[test]
    fn invalid_json_rejected_with_invalid_event_payload() {
        let err = parse_text_message(b"not json").unwrap_err();
        assert!(matches!(err, LarkEventError::InvalidEventPayload(_)));
    }

    // ============ Extra: empty chat_id string is treated as missing ============

    #[test]
    fn empty_chat_id_string_treated_as_missing() {
        let mut payload = fixture("oc_a", "ou_sender", "om_1", "hello", "111");
        payload["event"]["message"]["chat_id"] = json!("");

        let err = parse_text_message(payload.to_string().as_bytes()).unwrap_err();
        assert!(matches!(
            err,
            LarkEventError::MissingField("event.message.chat_id")
        ));
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
