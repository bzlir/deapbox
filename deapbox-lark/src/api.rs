//! Outbound Lark API boundary.

use async_trait::async_trait;
use deapbox_core::types::{ChatId, LarkConfig};
use open_lark::{
    openlark_client::Client,
    openlark_communication::im::im::v1::message::{
        create::{CreateMessageBody, CreateMessageRequest},
        models::ReceiveIdType,
    },
};
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum LarkApiError {
    #[error("invalid Lark client configuration: {0}")]
    ClientConfig(String),
    #[error("failed to encode text message content: {0}")]
    Encode(#[from] serde_json::Error),
    #[error("Lark API request failed: {0}")]
    Request(String),
}

/// Minimal outbound API required by the Stage 2 console MVP.
#[async_trait]
pub trait LarkMessageApi: Send + Sync {
    async fn send_text(&self, chat_id: &ChatId, text: &str) -> Result<(), LarkApiError>;
}

#[derive(Debug, Clone)]
pub struct OpenLarkMessageApi {
    client: Client,
}

impl OpenLarkMessageApi {
    pub fn new(config: &LarkConfig) -> Result<Self, LarkApiError> {
        let client = Client::builder()
            .app_id(config.app_id.clone())
            .app_secret(config.app_secret.clone())
            .build()
            .map_err(|err| LarkApiError::ClientConfig(err.to_string()))?;

        Ok(Self { client })
    }
}

#[async_trait]
impl LarkMessageApi for OpenLarkMessageApi {
    async fn send_text(&self, chat_id: &ChatId, text: &str) -> Result<(), LarkApiError> {
        let body = build_text_message_body(chat_id, text)?;

        CreateMessageRequest::new(self.client.core_config().clone())
            .receive_id_type(ReceiveIdType::ChatId)
            .execute(body)
            .await
            .map_err(|err| LarkApiError::Request(err.to_string()))?;

        Ok(())
    }
}

fn build_text_message_body(
    chat_id: &ChatId,
    text: &str,
) -> Result<CreateMessageBody, serde_json::Error> {
    Ok(CreateMessageBody {
        receive_id: chat_id.0.clone(),
        msg_type: "text".to_string(),
        content: serde_json::to_string(&json!({ "text": text }))?,
        uuid: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_body_uses_chat_id_receive_target() {
        let body = build_text_message_body(&ChatId("oc_a".to_string()), "hello").unwrap();

        assert_eq!(body.receive_id, "oc_a");
        assert_eq!(body.msg_type, "text");
        assert_eq!(body.content, r#"{"text":"hello"}"#);
        assert_eq!(body.uuid, None);
    }
}
