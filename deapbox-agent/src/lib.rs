//! deapbox-agent — per-`AgentKind` impls of `deapbox_core::Agent`.

pub mod echo;
pub mod opencode;

pub use echo::EchoAgent;
pub use opencode::OpenCodeAgent;
