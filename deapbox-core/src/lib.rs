//! deapbox-core — domain types, behavioral traits, and the per-chat dispatcher.
//!
//! Pure domain layer: no Lark SDK, no agent subprocess, no filesystem.
//! All externals are traits (`Agent`, `LarkMessageApi`) impl'd in sibling crates.

pub mod agent;
pub mod dispatcher;
pub mod lark_api;
pub mod types;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
