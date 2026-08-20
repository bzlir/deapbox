//! `opencode` — per-kind `Agent` impl for the opencode CLI.
//!
//! Two modules (ADR-0010):
//! - `adapter.rs` — pure-function wire layer (NDJSON parse + event mapping)
//! - `agent.rs` — `OpenCodeAgent: Agent` impl (spawn subprocess + stream)
//!
//! opencode is process-per-turn: each `send` spawns a fresh `opencode run`
//! process and streams its NDJSON stdout into `AgentEvent`s. The session ID
//! from `step_finish` is kept for the next turn's `--session` resume.

pub mod adapter;
pub mod agent;

pub use agent::OpenCodeAgent;
