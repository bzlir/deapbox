//! deapbox-store — TOML config + (Stage 3) sled persistence.

pub mod config;

pub use config::{load_config, ConfigError};
