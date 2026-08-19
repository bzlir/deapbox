//! Setup subcommand argument parsing + auto-detect (BIND vs NEW).
//!
//! Inspired by `cc-connect/cmd/cc-connect/feishu.go:380-418` (`resolveFeishuSetupInputs`).
//!
//! Auto-detect rule: if `--app` / `--app-id` / `--app-secret` is present,
//! resolve to `SetupCommand::Bind`; otherwise resolve to `SetupCommand::New`.
//! Explicit `bind` or `new` subcommand overrides auto-detect.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindArgs {
    pub app_id: String,
    pub app_secret: String,
    pub config_path: PathBuf,
    pub platform_type: Option<PlatformType>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewArgs {
    pub config_path: PathBuf,
    pub platform_type: Option<PlatformType>,
    pub timeout_seconds: u64,
    pub qr_image_path: Option<PathBuf>,
    pub debug: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformType {
    Feishu,
    Lark,
}

impl PlatformType {
    pub fn base_url(&self) -> &'static str {
        match self {
            PlatformType::Feishu => "https://open.feishu.cn",
            PlatformType::Lark => "https://open.larksuite.com",
        }
    }

    pub fn accounts_base_url(&self) -> &'static str {
        match self {
            PlatformType::Feishu => "https://accounts.feishu.cn",
            PlatformType::Lark => "https://accounts.larksuite.com",
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            PlatformType::Feishu => "feishu",
            PlatformType::Lark => "lark",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupCommand {
    Bind(BindArgs),
    New(NewArgs),
}

/// Parse `deapbox setup` args. Auto-detects Bind vs New based on whether
/// credentials are provided.
///
/// Accepted forms:
/// - `setup` (no args) → New (auto-detect)
/// - `setup --app cli_xxx:sec_xxx` → Bind (auto-detect)
/// - `setup --app-id cli_xxx --app-secret sec_xxx` → Bind (auto-detect)
/// - `setup bind --app cli_xxx:sec_xxx` → Bind (explicit)
/// - `setup new` → New (explicit)
///
/// `--config <path>` sets config file path (default: `config.toml`).
/// `--platform-type feishu|lark` forces platform (default: auto from response).
/// `--timeout <sec>` sets QR onboarding timeout (default: 600, New only).
/// `--qr-image <path>` saves QR as PNG (New only).
/// `--debug` enables onboarding HTTP debug logs (New only).
pub fn parse_args(args: Vec<String>) -> Result<SetupCommand, ParseError> {
    let mut iter = args.into_iter().peekable();

    let explicit_mode = match iter.peek() {
        Some(s) if s == "bind" || s == "link" => {
            iter.next();
            Some(SetupMode::Bind)
        }
        Some(s) if s == "new" || s == "create" => {
            iter.next();
            Some(SetupMode::New)
        }
        _ => None,
    };

    let mut config_path = PathBuf::from("config.toml");
    let mut platform_type: Option<PlatformType> = None;
    let mut app_pair: Option<String> = None;
    let mut app_id: Option<String> = None;
    let mut app_secret: Option<String> = None;
    let mut timeout_seconds: u64 = 600;
    let mut qr_image_path: Option<PathBuf> = None;
    let mut debug = false;

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--config" => {
                config_path = PathBuf::from(take_value(&mut iter, "--config")?);
            }
            "--platform-type" => {
                let v = take_value(&mut iter, "--platform-type")?;
                platform_type = Some(parse_platform_type(&v)?);
            }
            "--app" => {
                app_pair = Some(take_value(&mut iter, "--app")?);
            }
            "--app-id" => {
                app_id = Some(take_value(&mut iter, "--app-id")?);
            }
            "--app-secret" => {
                app_secret = Some(take_value(&mut iter, "--app-secret")?);
            }
            "--timeout" => {
                let v = take_value(&mut iter, "--timeout")?;
                timeout_seconds = v
                    .parse()
                    .map_err(|_| ParseError::InvalidValue("--timeout".to_owned(), v))?;
            }
            "--qr-image" => {
                qr_image_path = Some(PathBuf::from(take_value(&mut iter, "--qr-image")?));
            }
            "--debug" => {
                debug = true;
            }
            "-h" | "--help" => return Err(ParseError::Help),
            other => return Err(ParseError::UnknownArg(other.to_owned())),
        }
    }

    if app_pair.is_some() && (app_id.is_some() || app_secret.is_some()) {
        return Err(ParseError::Conflict(
            "use either --app or --app-id/--app-secret, not both".to_owned(),
        ));
    }

    if let Some(raw) = app_pair {
        let (id, sec) = parse_app_pair(&raw)?;
        app_id = Some(id);
        app_secret = Some(sec);
    }

    if (app_id.is_some()) != (app_secret.is_some()) {
        return Err(ParseError::Conflict(
            "both --app-id and --app-secret are required".to_owned(),
        ));
    }

    let has_credentials = app_id.is_some() && app_secret.is_some();
    let mode = match (explicit_mode, has_credentials) {
        (Some(SetupMode::Bind), _) => SetupMode::Bind,
        (Some(SetupMode::New), _) => SetupMode::New,
        (None, true) => SetupMode::Bind,
        (None, false) => SetupMode::New,
    };

    match (mode, has_credentials) {
        (SetupMode::Bind, false) => Err(ParseError::Conflict(
            "bind mode requires credentials: use --app id:secret or --app-id/--app-secret"
                .to_owned(),
        )),
        (SetupMode::New, true) => Err(ParseError::Conflict(
            "new mode does not accept credentials; use `deapbox setup bind`".to_owned(),
        )),
        (SetupMode::Bind, true) => Ok(SetupCommand::Bind(BindArgs {
            app_id: app_id.unwrap(),
            app_secret: app_secret.unwrap(),
            config_path,
            platform_type,
        })),
        (SetupMode::New, false) => Ok(SetupCommand::New(NewArgs {
            config_path,
            platform_type,
            timeout_seconds,
            qr_image_path,
            debug,
        })),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SetupMode {
    Bind,
    New,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("missing value for {0}")]
    MissingValue(String),
    #[error("invalid value for {0}: {1}")]
    InvalidValue(String, String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("unknown argument: {0}")]
    UnknownArg(String),
    #[error("invalid platform-type {0}, want feishu or lark")]
    InvalidPlatformType(String),
    #[error("--app format must be app_id:app_secret")]
    InvalidAppPair,
    #[error("help requested")]
    Help,
}

fn take_value<I>(iter: &mut std::iter::Peekable<I>, flag: &str) -> Result<String, ParseError>
where
    I: Iterator<Item = String>,
{
    iter.next()
        .ok_or_else(|| ParseError::MissingValue(flag.to_owned()))
}

fn parse_platform_type(s: &str) -> Result<PlatformType, ParseError> {
    match s.to_lowercase().as_str() {
        "feishu" => Ok(PlatformType::Feishu),
        "lark" => Ok(PlatformType::Lark),
        other => Err(ParseError::InvalidPlatformType(other.to_owned())),
    }
}

fn parse_app_pair(raw: &str) -> Result<(String, String), ParseError> {
    let idx = raw.find(':');
    let idx = idx.ok_or(ParseError::InvalidAppPair)?;
    if idx == 0 || idx >= raw.len() - 1 {
        return Err(ParseError::InvalidAppPair);
    }
    let app_id = raw[..idx].trim().to_owned();
    let app_secret = raw[idx + 1..].trim().to_owned();
    if app_id.is_empty() || app_secret.is_empty() {
        return Err(ParseError::InvalidAppPair);
    }
    Ok((app_id, app_secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    // ============ auto-detect: no credentials → New ============

    #[test]
    fn auto_detect_no_args_resolves_to_new() {
        let cmd = parse_args(args(&[])).unwrap();
        match cmd {
            SetupCommand::New(NewArgs {
                timeout_seconds, ..
            }) => {
                assert_eq!(timeout_seconds, 600);
            }
            other => panic!("expected New, got {:?}", other),
        }
    }

    #[test]
    fn auto_detect_with_app_pair_resolves_to_bind() {
        let cmd = parse_args(args(&["--app", "cli_xxx:sec_yyy"])).unwrap();
        match cmd {
            SetupCommand::Bind(BindArgs {
                app_id, app_secret, ..
            }) => {
                assert_eq!(app_id, "cli_xxx");
                assert_eq!(app_secret, "sec_yyy");
            }
            other => panic!("expected Bind, got {:?}", other),
        }
    }

    #[test]
    fn auto_detect_with_app_id_and_secret_resolves_to_bind() {
        let cmd = parse_args(args(&["--app-id", "cli_a", "--app-secret", "sec_b"])).unwrap();
        match cmd {
            SetupCommand::Bind(BindArgs {
                app_id, app_secret, ..
            }) => {
                assert_eq!(app_id, "cli_a");
                assert_eq!(app_secret, "sec_b");
            }
            other => panic!("expected Bind, got {:?}", other),
        }
    }

    // ============ explicit mode ============

    #[test]
    fn explicit_bind_subcommand_with_credentials() {
        let cmd = parse_args(args(&["bind", "--app", "cli:sec"])).unwrap();
        assert!(matches!(cmd, SetupCommand::Bind(_)));
    }

    #[test]
    fn explicit_new_subcommand_without_credentials() {
        let cmd = parse_args(args(&["new"])).unwrap();
        assert!(matches!(cmd, SetupCommand::New(_)));
    }

    #[test]
    fn explicit_bind_without_credentials_rejected() {
        let err = parse_args(args(&["bind"])).unwrap_err();
        assert!(matches!(err, ParseError::Conflict(_)));
    }

    #[test]
    fn explicit_new_with_credentials_rejected() {
        let err = parse_args(args(&["new", "--app", "cli:sec"])).unwrap_err();
        assert!(matches!(err, ParseError::Conflict(_)));
    }

    #[test]
    fn link_is_alias_for_bind() {
        let cmd = parse_args(args(&["link", "--app", "cli:sec"])).unwrap();
        assert!(matches!(cmd, SetupCommand::Bind(_)));
    }

    #[test]
    fn create_is_alias_for_new() {
        let cmd = parse_args(args(&["create"])).unwrap();
        assert!(matches!(cmd, SetupCommand::New(_)));
    }

    // ============ --app pair parsing ============

    #[test]
    fn app_pair_missing_colon_rejected() {
        let err = parse_args(args(&["--app", "no_colon"])).unwrap_err();
        assert_eq!(err, ParseError::InvalidAppPair);
    }

    #[test]
    fn app_pair_empty_app_id_rejected() {
        let err = parse_args(args(&["--app", ":sec"])).unwrap_err();
        assert_eq!(err, ParseError::InvalidAppPair);
    }

    #[test]
    fn app_pair_empty_app_secret_rejected() {
        let err = parse_args(args(&["--app", "cli:"])).unwrap_err();
        assert_eq!(err, ParseError::InvalidAppPair);
    }

    #[test]
    fn app_pair_with_whitespace_trimmed() {
        let cmd = parse_args(args(&["--app", "  cli_x  :  sec_y  "])).unwrap();
        match cmd {
            SetupCommand::Bind(BindArgs {
                app_id, app_secret, ..
            }) => {
                assert_eq!(app_id, "cli_x");
                assert_eq!(app_secret, "sec_y");
            }
            other => panic!("expected Bind, got {:?}", other),
        }
    }

    // ============ conflict detection ============

    #[test]
    fn app_and_app_id_secret_together_rejected() {
        let err = parse_args(args(&["--app", "cli:sec", "--app-id", "x"])).unwrap_err();
        assert!(matches!(err, ParseError::Conflict(_)));
    }

    #[test]
    fn only_app_id_without_secret_rejected() {
        let err = parse_args(args(&["--app-id", "cli"])).unwrap_err();
        assert!(matches!(err, ParseError::Conflict(_)));
    }

    #[test]
    fn only_app_secret_without_id_rejected() {
        let err = parse_args(args(&["--app-secret", "sec"])).unwrap_err();
        assert!(matches!(err, ParseError::Conflict(_)));
    }

    // ============ flag parsing ============

    #[test]
    fn config_path_default_is_config_toml() {
        let cmd = parse_args(args(&[])).unwrap();
        match cmd {
            SetupCommand::New(NewArgs { config_path, .. }) => {
                assert_eq!(config_path, PathBuf::from("config.toml"));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn config_path_custom() {
        let cmd = parse_args(args(&["--config", "/tmp/x.toml"])).unwrap();
        match cmd {
            SetupCommand::New(NewArgs { config_path, .. }) => {
                assert_eq!(config_path, PathBuf::from("/tmp/x.toml"));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn platform_type_feishu_parsed() {
        let cmd = parse_args(args(&["--platform-type", "feishu"])).unwrap();
        match cmd {
            SetupCommand::New(NewArgs {
                platform_type: Some(PlatformType::Feishu),
                ..
            }) => {}
            _ => panic!("wrong cmd shape"),
        }
    }

    #[test]
    fn platform_type_lark_case_insensitive() {
        let cmd = parse_args(args(&["--platform-type", "LARK"])).unwrap();
        match cmd {
            SetupCommand::New(NewArgs {
                platform_type: Some(PlatformType::Lark),
                ..
            }) => {}
            _ => panic!("wrong cmd shape"),
        }
    }

    #[test]
    fn platform_type_invalid_rejected() {
        let err = parse_args(args(&["--platform-type", "discord"])).unwrap_err();
        assert!(matches!(err, ParseError::InvalidPlatformType(_)));
    }

    #[test]
    fn timeout_seconds_default_600() {
        let cmd = parse_args(args(&[])).unwrap();
        match cmd {
            SetupCommand::New(NewArgs {
                timeout_seconds: 600,
                ..
            }) => {}
            _ => unreachable!(),
        }
    }

    #[test]
    fn timeout_seconds_custom() {
        let cmd = parse_args(args(&["--timeout", "30"])).unwrap();
        match cmd {
            SetupCommand::New(NewArgs {
                timeout_seconds: 30,
                ..
            }) => {}
            _ => unreachable!(),
        }
    }

    #[test]
    fn timeout_non_numeric_rejected() {
        let err = parse_args(args(&["--timeout", "abc"])).unwrap_err();
        assert!(matches!(err, ParseError::InvalidValue(_, _)));
    }

    #[test]
    fn qr_image_path_parsed() {
        let cmd = parse_args(args(&["--qr-image", "/tmp/qr.png"])).unwrap();
        match cmd {
            SetupCommand::New(NewArgs {
                qr_image_path: Some(p),
                ..
            }) => {
                assert_eq!(p, PathBuf::from("/tmp/qr.png"));
            }
            _ => unreachable!(),
        }
    }

    #[test]
    fn debug_flag_parsed() {
        let cmd = parse_args(args(&["--debug"])).unwrap();
        match cmd {
            SetupCommand::New(NewArgs { debug: true, .. }) => {}
            _ => unreachable!(),
        }
    }

    #[test]
    fn missing_value_for_config_rejected() {
        let err = parse_args(args(&["--config"])).unwrap_err();
        assert_eq!(err, ParseError::MissingValue("--config".to_owned()));
    }

    #[test]
    fn unknown_arg_rejected() {
        let err = parse_args(args(&["--bogus"])).unwrap_err();
        assert!(matches!(err, ParseError::UnknownArg(_)));
    }

    #[test]
    fn help_returns_help_error() {
        let err = parse_args(args(&["--help"])).unwrap_err();
        assert_eq!(err, ParseError::Help);
    }

    // ============ PlatformType helpers ============

    #[test]
    fn platform_type_base_urls() {
        assert_eq!(PlatformType::Feishu.base_url(), "https://open.feishu.cn");
        assert_eq!(PlatformType::Lark.base_url(), "https://open.larksuite.com");
        assert_eq!(
            PlatformType::Feishu.accounts_base_url(),
            "https://accounts.feishu.cn"
        );
        assert_eq!(
            PlatformType::Lark.accounts_base_url(),
            "https://accounts.larksuite.com"
        );
        assert_eq!(PlatformType::Feishu.as_str(), "feishu");
        assert_eq!(PlatformType::Lark.as_str(), "lark");
    }
}
