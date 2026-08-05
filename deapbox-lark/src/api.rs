//! Outbound Lark API boundary.
//!
//! Backed by [`foxzool/openlark`](https://crates.io/crates/openlark) (`openlark`
//! crate, Rust import path `open_lark`). The SDK's `Client` exposes a
//! `communication` meta-chain whose `im` helper sends text via
//! `send_text(MessageRecipient, text)`. We thread `ChatId` explicitly into a
//! [`MessageRecipient::chat_id`] so two groups can never share a reply target.

use async_trait::async_trait;
use deapbox_core::types::{ChatId, LarkConfig};
use open_lark::communication::MessageRecipient;
use open_lark::Client;

#[derive(Debug, thiserror::Error)]
pub enum LarkApiError {
    #[error("invalid Lark client configuration: {0}")]
    ClientConfig(String),
    #[error("Lark API request failed: {0}")]
    Request(String),
}

/// Minimal outbound API required by the Stage 2 console MVP.
#[async_trait]
pub trait LarkMessageApi: Send + Sync {
    async fn send_text(&self, chat_id: &ChatId, text: &str) -> Result<(), LarkApiError>;
}

/// `openlark`-backed outbound adapter.
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
        let recipient = MessageRecipient::chat_id(&chat_id.0);

        self.client
            .communication
            .im
            .send_text(recipient, text)
            .await
            .map_err(|err| LarkApiError::Request(err.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use open_lark::communication::prelude::ReceiveIdType;

    #[test]
    fn chat_recipient_targets_only_the_given_chat() {
        let a = MessageRecipient::chat_id("oc_a");
        let b = MessageRecipient::chat_id("oc_b");

        // The recipient pins both the target id and the ChatId routing type,
        // so send_text(chat_id) can only address one group.
        assert_eq!(a.receive_id, "oc_a");
        assert_eq!(a.receive_id_type, ReceiveIdType::ChatId);
        assert_eq!(b.receive_id, "oc_b");
        assert_ne!(a.receive_id, b.receive_id);
    }

    #[test]
    fn new_maps_invalid_sdk_config_to_client_config_without_panicking() {
        let config = LarkConfig {
            app_id: String::new(),
            app_secret: String::new(),
        };

        let err = OpenLarkMessageApi::new(&config).unwrap_err();

        assert!(matches!(err, LarkApiError::ClientConfig(_)));
        assert!(err.to_string().contains("app_id"));
    }
}
