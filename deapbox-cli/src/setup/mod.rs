//! `deapbox setup` 子命令：飞书 bot 自助式初始化。
//!
//! 对标 `cc-connect/cmd/cc-connect/feishu.go:86-235`（`runFeishu` + `runFeishuSetup`）。
//! 两种模式：
//!   - `bind` — 已有 app_id/app_secret → 校验 + 写回 config.toml
//!   - `new`  — 扫码 onboarding → 自动创建飞书应用 → 写回 config.toml（IVA-13 实现）

pub mod args;
pub mod bind;
pub mod error;
pub mod new;
pub mod oauth;
pub mod qr;

pub use args::{AutoArgs, AutoKind, BindArgs, NewArgs, SetupCommand};
pub use bind::{run_bind, run_bind_with, CredentialValidator, HttpCredentialValidator};
pub use error::SetupError;
pub use new::run_new;
pub use oauth::{run_registration_flow, HttpOAuthClient, OAuthClient, RegistrationResult};
pub use qr::{AnsiQrRenderer, QrRenderer};

use crate::CliError;

pub fn parse_args(args: Vec<String>) -> Result<SetupCommand, CliError> {
    args::parse_args(args)
}

pub async fn run(cmd: SetupCommand) -> Result<(), CliError> {
    match cmd {
        SetupCommand::Bind(args) => Ok(bind::run_bind(args).await?),
        SetupCommand::New(args) => Ok(new::run_new(args).await?),
        SetupCommand::Auto(auto) => match auto.kind {
            args::AutoKind::Bind => {
                let bind_args = auto.bind.expect("auto.bind must be set when kind=Bind");
                Ok(bind::run_bind(bind_args).await?)
            }
            args::AutoKind::New => {
                let new_args = auto.new.expect("auto.new must be set when kind=New");
                Ok(new::run_new(new_args).await?)
            }
        },
        SetupCommand::Help => {
            println!("{}", args::usage());
            Ok(())
        }
    }
}
