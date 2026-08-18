//! Outbound Lark API — `OpenLarkMessageApi` impl of `deapbox_core::LarkMessageApi`.
//!
//! Backed by `foxzool/openlark` (`openlark` crate, Rust import path `open_lark`).
//! Stage 1: `send_text` only. Uniformly uses `MessageRecipient::chat_id` for
//! both group and p2p chats (ADR-0005).

use async_trait::async_trait;
use deapbox_core::lark_api::LarkMessageApi;
use deapbox_core::types::{ChatId, LarkApiError, LarkConfig};
use open_lark::communication::MessageRecipient;
use open_lark::Client;

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
