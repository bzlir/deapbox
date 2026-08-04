// deapbox-agent — Agent Protocol 抽象 + 适配

// 重新导出核心 trait 方便内部使用
pub use deapbox_core::traits::{AgentDriver, AgentProcess, ProtocolAdapter};
pub use deapbox_core::types::*;

pub mod adapter;
pub mod claude_code;
pub mod codex;
pub mod kimi_code;
pub mod opencode;
pub mod protocol;
