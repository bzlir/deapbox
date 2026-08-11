//! Setup 子命令参数解析。

//! 对标 `cc-connect/cmd/cc-connect/feishu.go:347-378` 的 `printFeishuUsage` +
//! `:380-418` 的 `resolveFeishuSetupInputs`（模式判定 + 参数解析）。

use std::path::PathBuf;

use crate::CliError;

use super::error::SetupError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupCommand {
    Bind(BindArgs),
    New(NewArgs),
    Auto(AutoArgs),
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindArgs {
    pub app_id: String,
    pub app_secret: String,
    pub config_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewArgs {
    pub config_path: PathBuf,
    pub timeout_seconds: u64,
    pub qr_image: Option<PathBuf>,
    pub debug: bool,
    pub no_qr: bool,
}

/// `deapbox setup`（无显式子命令）的 auto-detect 模式。
/// 有 `--app` / `--app-id` / `--app-secret` 任一 → 走 bind；否则走 new。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoArgs {
    pub kind: AutoKind,
    pub bind: Option<BindArgs>,
    pub new: Option<NewArgs>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoKind {
    Bind,
    New,
}

/// 解析 `deapbox setup ...` 之后的参数。
pub fn parse_args(args: Vec<String>) -> Result<SetupCommand, CliError> {
    let mut iter = args.into_iter();
    let Some(first) = iter.next() else {
        return Ok(SetupCommand::Auto(parse_auto(Vec::new())?));
    };

    let rest: Vec<String> = iter.collect();
    match first.as_str() {
        "bind" => Ok(SetupCommand::Bind(parse_bind_inner(rest)?)),
        "new" => Ok(SetupCommand::New(parse_new_inner(rest)?)),
        "-h" | "--help" => Ok(SetupCommand::Help),
        other => {
            // 第一个 arg 不是已知子命令 → 视为 auto-detect
            let mut all = vec![first.clone()];
            all.extend(rest);
            if other.starts_with('-') && other != "--help" && other != "-h" {
                Ok(SetupCommand::Auto(parse_auto(all)?))
            } else {
                Err(CliError::InvalidArgs(format!(
                    "unknown setup subcommand: {other}\n{}",
                    usage()
                )))
            }
        }
    }
}

fn has_bind_flag(args: &[String]) -> bool {
    args.iter()
        .any(|a| a == "--app" || a == "--app-id" || a == "--app-secret")
}

fn parse_auto(args: Vec<String>) -> Result<AutoArgs, SetupError> {
    if has_bind_flag(&args) {
        let bind = parse_bind_inner(args.clone())?;
        return Ok(AutoArgs {
            kind: AutoKind::Bind,
            bind: Some(bind),
            new: None,
        });
    }
    let new = parse_new_inner(args)?;
    Ok(AutoArgs {
        kind: AutoKind::New,
        bind: None,
        new: Some(new),
    })
}

fn parse_bind_inner(args: Vec<String>) -> Result<BindArgs, SetupError> {
    let mut app_id = String::new();
    let mut app_secret = String::new();
    let mut config_path = PathBuf::from("config.toml");
    let mut app_pair: Option<String> = None;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--app" => {
                let Some(v) = iter.next() else {
                    return Err(SetupError::InvalidArgs(
                        "--app requires a value (app_id:app_secret)".into(),
                    ));
                };
                app_pair = Some(v);
            }
            "--app-id" => {
                let Some(v) = iter.next() else {
                    return Err(SetupError::InvalidArgs("--app-id requires a value".into()));
                };
                app_id = v;
            }
            "--app-secret" => {
                let Some(v) = iter.next() else {
                    return Err(SetupError::InvalidArgs(
                        "--app-secret requires a value".into(),
                    ));
                };
                app_secret = v;
            }
            "--config" => {
                let Some(v) = iter.next() else {
                    return Err(SetupError::InvalidArgs("--config requires a path".into()));
                };
                config_path = PathBuf::from(v);
            }
            "-h" | "--help" => return Err(SetupError::InvalidArgs(usage())),
            other => {
                return Err(SetupError::InvalidArgs(format!(
                    "unknown bind option: {other}\n{}",
                    usage()
                )));
            }
        }
    }

    if let Some(pair) = app_pair {
        if !app_id.is_empty() || !app_secret.is_empty() {
            return Err(SetupError::InvalidArgs(
                "use either --app or --app-id/--app-secret, not both".into(),
            ));
        }
        let (id, secret) = parse_app_pair(&pair)?;
        app_id = id;
        app_secret = secret;
    }

    if app_id.trim().is_empty() || app_secret.trim().is_empty() {
        return Err(SetupError::InvalidArgs(
            "bind requires --app id:secret (or --app-id + --app-secret)".into(),
        ));
    }

    Ok(BindArgs {
        app_id,
        app_secret,
        config_path,
    })
}

fn parse_new_inner(args: Vec<String>) -> Result<NewArgs, SetupError> {
    let mut config_path = PathBuf::from("config.toml");
    let mut timeout_seconds: u64 = 600;
    let mut qr_image: Option<PathBuf> = None;
    let mut debug = false;
    let mut no_qr = false;

    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--config" => {
                let Some(v) = iter.next() else {
                    return Err(SetupError::InvalidArgs("--config requires a path".into()));
                };
                config_path = PathBuf::from(v);
            }
            "--timeout" => {
                let Some(v) = iter.next() else {
                    return Err(SetupError::InvalidArgs(
                        "--timeout requires a value (seconds)".into(),
                    ));
                };
                timeout_seconds = v.parse().map_err(|_| {
                    SetupError::InvalidArgs(format!(
                        "invalid --timeout value: {v} (must be a number)"
                    ))
                })?;
            }
            "--qr-image" => {
                let Some(v) = iter.next() else {
                    return Err(SetupError::InvalidArgs("--qr-image requires a path".into()));
                };
                qr_image = Some(PathBuf::from(v));
            }
            "--debug" => debug = true,
            "--no-qr" => no_qr = true,
            "-h" | "--help" => return Err(SetupError::InvalidArgs(usage())),
            other => {
                return Err(SetupError::InvalidArgs(format!(
                    "unknown new option: {other}\n{}",
                    usage()
                )));
            }
        }
    }

    Ok(NewArgs {
        config_path,
        timeout_seconds,
        qr_image,
        debug,
        no_qr,
    })
}

/// 解析 `app_id:app_secret` 形式的复合参数。
/// 对标 `cc-connect/cmd/cc-connect/feishu.go:420-431`（`parseAppPair`）。
pub fn parse_app_pair(raw: &str) -> Result<(String, String), SetupError> {
    let idx = raw
        .find(':')
        .ok_or_else(|| SetupError::InvalidArgs("--app format must be app_id:app_secret".into()))?;
    if idx == 0 || idx >= raw.len() - 1 {
        return Err(SetupError::InvalidArgs(
            "--app format must be app_id:app_secret".into(),
        ));
    }
    let id = raw[..idx].trim().to_string();
    let secret = raw[idx + 1..].trim().to_string();
    if id.is_empty() || secret.is_empty() {
        return Err(SetupError::InvalidArgs(
            "--app format must be app_id:app_secret".into(),
        ));
    }
    Ok((id, secret))
}

pub fn usage() -> String {
    "\
usage: deapbox setup <command> [options]

commands:
  bind    Validate existing app_id/app_secret and write to config.toml
  new     QR onboarding — scan to auto-create a Feishu/Lark app
  (no subcommand)  Auto-detect: --app → bind; otherwise new

bind options:
  --app <id:secret>          Existing credentials (recommended)
  --app-id <id>              Existing app_id (must pair with --app-secret)
  --app-secret <secret>      Existing app_secret (must pair with --app-id)
  --config <path>            Target config file (default: config.toml)

new options:
  --config <path>            Target config file (default: config.toml)
  --timeout <seconds>        QR onboarding timeout (default: 600)
  --qr-image <path>          Save QR code as PNG (default: terminal only)
  --no-qr                    Skip terminal QR rendering; print URL only (for headless/CI)
  --debug                    Print onboarding debug logs

examples:
  deapbox setup                       # auto-detect (no --app → new)
  deapbox setup --app cli_xxx:sec_xxx # auto-detect → bind
  deapbox setup bind --app cli_xxx:sec_xxx
  deapbox setup new
"
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_args_returns_auto_new() {
        // IVA-10 spec: `deapbox setup`（无参）→ auto-detect → 无 --app → 走 NEW 模式
        let cmd = parse_args(args(&[])).unwrap();
        match cmd {
            SetupCommand::Auto(a) => {
                assert_eq!(a.kind, AutoKind::New);
                assert!(a.bind.is_none());
                assert!(a.new.is_some());
            }
            other => panic!("expected Auto(New), got {other:?}"),
        }
    }

    #[test]
    fn auto_with_app_flag_routes_to_bind() {
        let cmd = parse_args(args(&["--app", "x:y"])).unwrap();
        match cmd {
            SetupCommand::Auto(a) => {
                assert_eq!(a.kind, AutoKind::Bind);
                assert!(a.bind.is_some());
                let bind = a.bind.unwrap();
                assert_eq!(bind.app_id, "x");
                assert_eq!(bind.app_secret, "y");
            }
            other => panic!("expected Auto(Bind), got {other:?}"),
        }
    }

    #[test]
    fn auto_with_app_id_secret_routes_to_bind() {
        let cmd = parse_args(args(&[
            "--app-id",
            "a",
            "--app-secret",
            "b",
            "--config",
            "/tmp/x.toml",
        ]))
        .unwrap();
        match cmd {
            SetupCommand::Auto(a) => {
                assert_eq!(a.kind, AutoKind::Bind);
                let bind = a.bind.unwrap();
                assert_eq!(bind.app_id, "a");
                assert_eq!(bind.app_secret, "b");
                assert_eq!(bind.config_path, PathBuf::from("/tmp/x.toml"));
            }
            other => panic!("expected Auto(Bind), got {other:?}"),
        }
    }

    #[test]
    fn auto_with_new_flags_routes_to_new() {
        let cmd = parse_args(args(&["--config", "/tmp/c.toml", "--timeout", "30"])).unwrap();
        match cmd {
            SetupCommand::Auto(a) => {
                assert_eq!(a.kind, AutoKind::New);
                let new = a.new.unwrap();
                assert_eq!(new.config_path, PathBuf::from("/tmp/c.toml"));
                assert_eq!(new.timeout_seconds, 30);
            }
            other => panic!("expected Auto(New), got {other:?}"),
        }
    }

    #[test]
    fn help_flag_returns_help() {
        assert_eq!(parse_args(args(&["--help"])).unwrap(), SetupCommand::Help);
        assert_eq!(parse_args(args(&["-h"])).unwrap(), SetupCommand::Help);
    }

    #[test]
    fn unknown_subcommand_rejected() {
        let err = parse_args(args(&["frob"])).unwrap_err();
        assert!(err.to_string().contains("unknown setup subcommand: frob"));
    }

    #[test]
    fn bind_with_app_pair_parses() {
        let cmd = parse_args(args(&["bind", "--app", "cli_xxx:sec_yyy"])).unwrap();
        match cmd {
            SetupCommand::Bind(b) => {
                assert_eq!(b.app_id, "cli_xxx");
                assert_eq!(b.app_secret, "sec_yyy");
                assert_eq!(b.config_path, PathBuf::from("config.toml"));
            }
            other => panic!("expected Bind, got {other:?}"),
        }
    }

    #[test]
    fn bind_with_app_id_and_secret_parses() {
        let cmd = parse_args(args(&[
            "bind",
            "--app-id",
            "cli_a",
            "--app-secret",
            "sec_b",
        ]))
        .unwrap();
        match cmd {
            SetupCommand::Bind(b) => {
                assert_eq!(b.app_id, "cli_a");
                assert_eq!(b.app_secret, "sec_b");
            }
            other => panic!("expected Bind, got {other:?}"),
        }
    }

    #[test]
    fn bind_rejects_app_pair_and_split_form_together() {
        let err = parse_args(args(&["bind", "--app", "x:y", "--app-id", "z"])).unwrap_err();
        assert!(err.to_string().contains("not both"));
    }

    #[test]
    fn bind_requires_credentials() {
        let err = parse_args(args(&["bind"])).unwrap_err();
        assert!(err.to_string().contains("bind requires"));
    }

    #[test]
    fn bind_rejects_malformed_app_pair() {
        let cases = ["no_colon", ":missing_id", "missing_secret:", ":"];
        for raw in cases {
            let err = parse_app_pair(raw).unwrap_err();
            assert!(err.to_string().contains("app_id:app_secret"));
        }
    }

    #[test]
    fn bind_custom_config_path() {
        let cmd = parse_args(args(&["bind", "--app", "a:b", "--config", "/tmp/x.toml"])).unwrap();
        match cmd {
            SetupCommand::Bind(b) => assert_eq!(b.config_path, PathBuf::from("/tmp/x.toml")),
            other => panic!("expected Bind, got {other:?}"),
        }
    }

    #[test]
    fn new_defaults() {
        let cmd = parse_args(args(&["new"])).unwrap();
        match cmd {
            SetupCommand::New(n) => {
                assert_eq!(n.config_path, PathBuf::from("config.toml"));
                assert_eq!(n.timeout_seconds, 600);
                assert_eq!(n.qr_image, None);
                assert!(!n.debug);
                assert!(!n.no_qr);
            }
            other => panic!("expected New, got {other:?}"),
        }
    }

    #[test]
    fn new_custom_all_options() {
        let cmd = parse_args(args(&[
            "new",
            "--config",
            "/tmp/c.toml",
            "--timeout",
            "120",
            "--qr-image",
            "/tmp/qr.png",
            "--debug",
            "--no-qr",
        ]))
        .unwrap();
        match cmd {
            SetupCommand::New(n) => {
                assert_eq!(n.config_path, PathBuf::from("/tmp/c.toml"));
                assert_eq!(n.timeout_seconds, 120);
                assert_eq!(n.qr_image, Some(PathBuf::from("/tmp/qr.png")));
                assert!(n.debug);
                assert!(n.no_qr);
            }
            other => panic!("expected New, got {other:?}"),
        }
    }

    #[test]
    fn new_rejects_non_numeric_timeout() {
        let err = parse_args(args(&["new", "--timeout", "abc"])).unwrap_err();
        assert!(err.to_string().contains("invalid --timeout value"));
    }

    #[test]
    fn bind_unknown_option_rejected() {
        let err = parse_args(args(&["bind", "--app", "a:b", "--frob"])).unwrap_err();
        assert!(err.to_string().contains("unknown bind option: --frob"));
    }

    #[test]
    fn bind_missing_value_for_app() {
        let err = parse_args(args(&["bind", "--app"])).unwrap_err();
        assert!(err.to_string().contains("--app requires a value"));
    }
}
