//! Setup 子命令错误类型。

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("invalid setup arguments: {0}")]
    InvalidArgs(String),
    #[error("feishu/lark HTTP error: {0}")]
    Http(String),
    #[error("feishu/lark OAuth rejected credentials: code={code} msg={msg}")]
    OAuth { code: i64, msg: String },
    #[error("config file write error: {0}")]
    WriteConfig(String),
    #[error("setup feature not implemented: {0}")]
    NotImplemented(&'static str),
}
