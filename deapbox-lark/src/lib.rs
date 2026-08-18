//! deapbox-lark — Feishu WS inbound + OpenAPI outbound adapter.

pub mod api;
pub mod event;
pub mod ws;

pub use api::OpenLarkMessageApi;
pub use event::{parse_text_message, LarkEventError};
pub use ws::start_ws;
