// deapbox-agent — Agent Protocol 抽象 + 适配

// 重新导出核心 trait 方便内部使用
// （旧 AgentProcess / ProtocolAdapter 已随 TES-86 删除：per-kind session 自己
//  own 进程 + 读循环 + broadcast channel，不再走共享 mutable 进程抽象。）
pub use deapbox_core::traits::AgentDriver;
pub use deapbox_core::types::*;

pub mod adapter;
pub mod claude_code;
pub mod codex;
pub mod kimi_code;
pub mod opencode;
pub mod protocol;
