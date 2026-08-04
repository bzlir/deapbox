//! Feishu/Lark adapter boundary.

pub mod api;
pub mod card;
pub mod event;
pub mod types;

pub use api::{LarkApiError, LarkMessageApi, OpenLarkMessageApi};
pub use event::{LarkEventBridge, LarkEventError};
pub use types::{InboundTextMessage, OutboundTextMessage};
