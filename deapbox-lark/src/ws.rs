//! Feishu WebSocket inbound — `LarkWsClient` startup + raw payload channel.
//!
//! ADR-0005: openlark 0.20 `websocket` feature provides
//! `EventDispatcherHandler::builder().payload_sender(tx).build()` + `LarkWsClient::open`.
//! Raw event bytes are forwarded to `mpsc::UnboundedReceiver<Vec<u8>>`; the
//! caller (main loop) parses them via `event::parse_text_message`.

use std::sync::Arc;

use deapbox_core::types::{LarkApiError, LarkConfig};
use open_lark::ws_client::{EventDispatcherHandler, LarkWsClient, WsClientError};
use open_lark::{Config, CoreConfig};
use tokio::sync::mpsc;

/// Start the Feishu WebSocket long-connection. Returns the inbound payload
/// channel receiver. The WS task runs in the background; if the connection
/// closes, the channel receiver will return `None` on next `recv()`.
///
/// Stage 1: no reconnection (ADR-0005). On connection close, the main loop
/// should exit and let the operator restart deapbox.
pub fn start_ws(
    config: &LarkConfig,
) -> Result<
    (
        mpsc::UnboundedReceiver<Vec<u8>>,
        tokio::task::JoinHandle<()>,
    ),
    LarkApiError,
> {
    let ws_config = Config::builder()
        .app_id(config.app_id.clone())
        .app_secret(config.app_secret.clone())
        .build();
    let ws_config = Arc::new(ws_config);

    let (payload_tx, payload_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let event_handler = EventDispatcherHandler::builder()
        .payload_sender(payload_tx)
        .build();

    let ws_config_clone = Arc::clone(&ws_config);
    let join = tokio::spawn(async move {
        tracing::info!("starting Feishu WebSocket long-connection");
        match LarkWsClient::open(ws_config_clone, event_handler).await {
            Ok(()) => tracing::info!("WebSocket session ended normally"),
            Err(WsClientError::ConnectionClosed { ref reason }) => {
                tracing::info!(?reason, "WebSocket connection closed");
            }
            Err(ref err) => {
                tracing::error!(error = %err, "WebSocket session error");
            }
        }
    });

    Ok((payload_rx, join))
}

/// Build a `CoreConfig` from `LarkConfig` — used when the caller needs to
/// construct an `openlark::Client` for outbound API alongside the WS inbound.
pub fn core_config_from(config: &LarkConfig) -> CoreConfig {
    CoreConfig::builder()
        .app_id(config.app_id.clone())
        .app_secret(config.app_secret.clone())
        .build()
}
