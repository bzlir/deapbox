use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use deapbox_core::types::{AppConfig, ChatId, UserMessage};
use deapbox_lark::{LarkMessageApi, OpenLarkMessageApi};
use deapbox_store::config::{load_config, ConfigError};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

pub mod setup;

pub use setup::SetupError;

const OPERATOR_HELP: &str = "/chats | /use <chat_id> | /send <chat_id> <text> | /quit | /exit";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOptions {
    pub config_path: PathBuf,
    pub check_config: bool,
    pub dry_run: bool,
}

impl CliOptions {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, CliError> {
        let mut config_path = PathBuf::from("config.toml");
        let mut check_config = false;
        let mut dry_run = false;
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config" => {
                    let Some(path) = args.next() else {
                        return Err(CliError::InvalidArgs(
                            "--config requires a file path".to_string(),
                        ));
                    };
                    config_path = PathBuf::from(path);
                }
                "--check-config" => check_config = true,
                "--dry-run" => dry_run = true,
                "-h" | "--help" => return Err(CliError::Help(usage())),
                other => {
                    return Err(CliError::InvalidArgs(format!(
                        "unknown argument: {other}\n{}",
                        usage()
                    )));
                }
            }
        }

        Ok(Self {
            config_path,
            check_config,
            dry_run,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    Help(String),
    #[error("{0}")]
    InvalidArgs(String),
    #[error("configuration error: {0}")]
    Config(#[from] ConfigError),
    #[error("Lark API setup error: {0}")]
    LarkApi(#[from] deapbox_lark::LarkApiError),
    #[error("Lark inbound event source is unavailable: {0}")]
    InboundEventsUnavailable(&'static str),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("setup error: {0}")]
    Setup(#[from] SetupError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Run(CliOptions),
    Setup(setup::SetupCommand),
}

pub fn parse_command(args: impl IntoIterator<Item = String>) -> Result<Command, CliError> {
    let mut iter = args.into_iter().peekable();
    if let Some(first) = iter.peek() {
        if first == "setup" {
            iter.next();
            let rest: Vec<String> = iter.collect();
            return Ok(Command::Setup(setup::parse_args(rest)?));
        }
    }
    Ok(Command::Run(CliOptions::parse(iter)?))
}

#[derive(Debug, Default)]
pub struct ConsoleState {
    seen_chats: BTreeMap<ChatId, ChatSummary>,
    seen_msg_ids: HashSet<String>,
    current_chat: Option<ChatId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChatSummary {
    sender: String,
    last_text: String,
}

impl ConsoleState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_inbound(&mut self, message: &UserMessage) -> Option<String> {
        if !self.seen_msg_ids.insert(message.msg_id.clone()) {
            return None;
        }

        self.seen_chats.insert(
            message.chat_id.clone(),
            ChatSummary {
                sender: message.sender.0.clone(),
                last_text: message.text.clone(),
            },
        );
        Some(format!(
            "[chat_id={}] sender={} msg_id={} text={}",
            message.chat_id.0, message.sender.0, message.msg_id, message.text
        ))
    }

    pub fn list_chats(&self) -> Vec<String> {
        if self.seen_chats.is_empty() {
            return vec!["no chats seen yet".to_string()];
        }

        self.seen_chats
            .iter()
            .map(|(chat_id, summary)| {
                let marker = if self.current_chat.as_ref() == Some(chat_id) {
                    "*"
                } else {
                    " "
                };
                format!(
                    "{marker} chat_id={} last_sender={} last_text={}",
                    chat_id.0, summary.sender, summary.last_text
                )
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResult {
    pub output: Vec<String>,
    pub should_quit: bool,
}

impl CommandResult {
    fn line(line: impl Into<String>) -> Self {
        Self {
            output: vec![line.into()],
            should_quit: false,
        }
    }

    fn quit() -> Self {
        Self {
            output: vec!["bye".to_string()],
            should_quit: true,
        }
    }
}

pub async fn handle_operator_line<A: LarkMessageApi>(
    state: &mut ConsoleState,
    api: &A,
    input: &str,
) -> CommandResult {
    let input = input.trim_start().trim_end_matches(['\r', '\n']);
    if input.trim().is_empty() {
        return CommandResult::line("");
    }

    let command = input.trim_end();

    if command == "/quit" || command == "/exit" {
        return CommandResult::quit();
    }
    if command == "/help" {
        return CommandResult::line(OPERATOR_HELP);
    }
    if command == "/chats" {
        return CommandResult {
            output: state.list_chats(),
            should_quit: false,
        };
    }
    if let Some(rest) = input.strip_prefix("/use ") {
        let chat_id = match parse_chat_id(rest.trim()) {
            Ok(chat_id) => chat_id,
            Err(message) => return CommandResult::line(message),
        };
        let output = format!("current chat_id={}", chat_id.0);
        state.current_chat = Some(chat_id);
        return CommandResult::line(output);
    }
    if let Some(rest) = input.strip_prefix("/send ") {
        let Some((chat_id, text)) = split_first_arg(rest) else {
            return CommandResult::line("usage: /send <chat_id> <text>");
        };
        let chat_id = match parse_chat_id(chat_id) {
            Ok(chat_id) => chat_id,
            Err(message) => return CommandResult::line(message),
        };
        return send_text(api, chat_id, text).await;
    }
    if input.starts_with('/') {
        return CommandResult::line("unknown command; try /help");
    }

    let Some(chat_id) = state.current_chat.clone() else {
        return CommandResult::line(
            "no current chat; use /send <chat_id> <text> or /use <chat_id>",
        );
    };
    send_text(api, chat_id, input).await
}

async fn send_text<A: LarkMessageApi>(api: &A, chat_id: ChatId, text: &str) -> CommandResult {
    if text.trim().is_empty() {
        return CommandResult::line("message text cannot be empty");
    }

    match api.send_text(&chat_id, text).await {
        Ok(()) => CommandResult::line(format!("sent chat_id={}", chat_id.0)),
        Err(err) => CommandResult::line(format!("send failed chat_id={}: {err}", chat_id.0)),
    }
}

fn split_first_arg(input: &str) -> Option<(&str, &str)> {
    let input = input.trim_start();
    let split_at = input
        .char_indices()
        .find_map(|(index, ch)| ch.is_whitespace().then_some(index))?;
    let (first, rest) = input.split_at(split_at);
    Some((first, rest.trim_start()))
}

fn parse_chat_id(input: &str) -> Result<ChatId, &'static str> {
    if input.is_empty() {
        return Err("chat_id cannot be empty");
    }
    if input.chars().any(char::is_whitespace) {
        return Err("chat_id cannot contain whitespace");
    }
    Ok(ChatId(input.to_string()))
}

pub fn load_checked_config(path: &Path) -> Result<AppConfig, CliError> {
    Ok(load_config(path)?)
}

pub async fn run_from_args(args: impl IntoIterator<Item = String>) -> Result<(), CliError> {
    match parse_command(args)? {
        Command::Run(opts) => run_service(opts).await,
        Command::Setup(cmd) => setup::run(cmd).await,
    }
}

async fn run_service(options: CliOptions) -> Result<(), CliError> {
    let config = load_checked_config(&options.config_path)?;

    if options.check_config {
        println!("configuration ok: {}", options.config_path.display());
        return Ok(());
    }

    let _api = OpenLarkMessageApi::new(&config.lark)?;

    if options.dry_run {
        println!(
            "dry-run: config loaded; outbound Lark API is constructible; inbound event source is not started"
        );
        return Ok(());
    }

    Err(CliError::InboundEventsUnavailable(
        "the pinned openlark WebSocket handler does not expose event payload forwarding yet; run --dry-run for startup validation or wire a real inbound source before starting the service",
    ))
}

pub async fn run_console_loop<A, R, W>(
    mut state: ConsoleState,
    api: A,
    mut inbound_rx: mpsc::Receiver<UserMessage>,
    stdin: R,
    mut stdout: W,
) -> Result<(), CliError>
where
    A: LarkMessageApi,
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = stdin.lines();
    let mut inbound_open = true;

    loop {
        tokio::select! {
            biased;

            message = inbound_rx.recv(), if inbound_open => {
                match message {
                    Some(message) => {
                        if let Some(line) = state.record_inbound(&message) {
                            write_line(&mut stdout, &line).await?;
                        }
                    }
                    None => {
                        inbound_open = false;
                        write_line(&mut stdout, "inbound Lark channel closed").await?;
                    }
                }
            }
            line = lines.next_line() => {
                let Some(line) = line? else {
                    break;
                };
                let result = handle_operator_line(&mut state, &api, &line).await;
                for line in result.output {
                    if !line.is_empty() {
                        write_line(&mut stdout, &line).await?;
                    }
                }
                if result.should_quit {
                    break;
                }
            }
        }
    }

    Ok(())
}

async fn write_line<W: AsyncWrite + Unpin>(writer: &mut W, line: &str) -> Result<(), CliError> {
    writer.write_all(line.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

fn usage() -> String {
    "usage: deapbox [--config <path>] [--check-config] [--dry-run]\n       deapbox setup <command> [options]  (run `deapbox setup --help` for details)".to_string()
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;

    #[test]
    fn parse_command_routes_setup_subcommand() {
        let cmd = parse_command(["setup".into(), "--help".into()]).unwrap();
        match cmd {
            Command::Setup(setup::SetupCommand::Help) => {}
            other => panic!("expected Setup::Help, got {other:?}"),
        }
    }

    #[test]
    fn parse_command_routes_setup_bind() {
        let cmd =
            parse_command(["setup".into(), "bind".into(), "--app".into(), "x:y".into()]).unwrap();
        match cmd {
            Command::Setup(setup::SetupCommand::Bind(b)) => {
                assert_eq!(b.app_id, "x");
                assert_eq!(b.app_secret, "y");
            }
            other => panic!("expected Setup::Bind, got {other:?}"),
        }
    }

    #[test]
    fn parse_command_routes_setup_new() {
        let cmd = parse_command(["setup".into(), "new".into()]).unwrap();
        assert!(matches!(cmd, Command::Setup(setup::SetupCommand::New(_))));
    }

    #[test]
    fn parse_command_falls_through_to_run_when_no_setup_prefix() {
        let cmd = parse_command(["--check-config".into()]).unwrap();
        match cmd {
            Command::Run(opts) => assert!(opts.check_config),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn parse_command_run_with_config_path_unchanged() {
        let cmd = parse_command(["--config".into(), "/tmp/x.toml".into()]).unwrap();
        match cmd {
            Command::Run(opts) => assert_eq!(opts.config_path, PathBuf::from("/tmp/x.toml")),
            other => panic!("expected Run, got {other:?}"),
        }
    }

    #[test]
    fn parse_command_setup_with_no_args_returns_auto_new() {
        // IVA-10: `deapbox setup`（无参）→ auto-detect → NEW（无 --app）
        let cmd = parse_command(["setup".into()]).unwrap();
        match cmd {
            Command::Setup(setup::SetupCommand::Auto(a)) => {
                assert_eq!(a.kind, setup::AutoKind::New);
            }
            other => panic!("expected Setup::Auto(New), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_from_args_setup_help_prints_usage_and_succeeds() {
        let result = run_from_args(["setup".into(), "--help".into()]).await;
        assert!(result.is_ok());
    }

    #[test]
    fn parse_command_routes_setup_new_to_new_variant() {
        // C2 后 NEW 模式真实现，不再返 NotImplemented；这里只测路由不真调飞书 OAuth。
        let cmd = parse_command(["setup".into(), "new".into()]).unwrap();
        match cmd {
            Command::Setup(setup::SetupCommand::New(_)) => {}
            other => panic!("expected Setup::New, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use deapbox_core::types::UserId;

    use super::*;

    #[derive(Debug, Default, Clone)]
    struct FakeLarkApi {
        sent: Arc<Mutex<Vec<(ChatId, String)>>>,
    }

    #[async_trait]
    impl LarkMessageApi for FakeLarkApi {
        async fn send_text(
            &self,
            chat_id: &ChatId,
            text: &str,
        ) -> Result<(), deapbox_lark::LarkApiError> {
            self.sent
                .lock()
                .unwrap()
                .push((chat_id.clone(), text.to_string()));
            Ok(())
        }
    }

    #[tokio::test]
    async fn send_command_targets_only_the_explicit_chat() {
        let api = FakeLarkApi::default();
        let mut state = ConsoleState::new();

        let result = handle_operator_line(&mut state, &api, "/send oc_a hello A").await;

        assert_eq!(result.output, vec!["sent chat_id=oc_a"]);
        assert_eq!(
            *api.sent.lock().unwrap(),
            vec![(ChatId("oc_a".to_string()), "hello A".to_string())]
        );
    }

    #[test]
    fn interleaved_inbound_messages_print_distinct_chat_ids() {
        let mut state = ConsoleState::new();
        let first = state
            .record_inbound(&message("oc_a", "ou_1", "om_1", "hello A"))
            .unwrap();
        let second = state
            .record_inbound(&message("oc_b", "ou_2", "om_2", "hello B"))
            .unwrap();

        assert!(first.contains("chat_id=oc_a"));
        assert!(first.contains("sender=ou_1"));
        assert!(first.contains("text=hello A"));
        assert!(second.contains("chat_id=oc_b"));
        assert!(second.contains("sender=ou_2"));
        assert!(second.contains("text=hello B"));
    }

    #[test]
    fn duplicate_inbound_msg_id_is_skipped() {
        let mut state = ConsoleState::new();
        let first = message("oc_a", "ou_1", "om_same", "hello");
        let retry = message("oc_a", "ou_1", "om_same", "hello again");

        assert!(state.record_inbound(&first).is_some());
        assert_eq!(state.record_inbound(&retry), None);
    }

    #[tokio::test]
    async fn plain_text_without_default_chat_does_not_send() {
        let api = FakeLarkApi::default();
        let mut state = ConsoleState::new();

        let result = handle_operator_line(&mut state, &api, "do not leak").await;

        assert_eq!(
            result.output,
            vec!["no current chat; use /send <chat_id> <text> or /use <chat_id>"]
        );
        assert!(api.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn use_command_routes_plain_text_until_switched() {
        let api = FakeLarkApi::default();
        let mut state = ConsoleState::new();

        handle_operator_line(&mut state, &api, "/use oc_a").await;
        handle_operator_line(&mut state, &api, "hello A").await;
        handle_operator_line(&mut state, &api, "/use oc_b").await;
        handle_operator_line(&mut state, &api, "hello B").await;

        assert_eq!(
            *api.sent.lock().unwrap(),
            vec![
                (ChatId("oc_a".to_string()), "hello A".to_string()),
                (ChatId("oc_b".to_string()), "hello B".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn invalid_chat_id_is_rejected_before_use() {
        let api = FakeLarkApi::default();
        let mut state = ConsoleState::new();

        let result = handle_operator_line(&mut state, &api, "/use oc a").await;

        assert_eq!(result.output, vec!["chat_id cannot contain whitespace"]);
        assert!(api.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn send_command_rejects_empty_text_consistently() {
        let api = FakeLarkApi::default();
        let mut state = ConsoleState::new();

        let result = handle_operator_line(&mut state, &api, "/send oc_a ").await;

        assert_eq!(result.output, vec!["message text cannot be empty"]);
        assert!(api.sent.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn console_loop_handles_inbound_and_operator_commands() {
        let api = FakeLarkApi::default();
        let (tx, rx) = mpsc::channel(4);
        tx.send(message("oc_a", "ou_1", "om_1", "hello"))
            .await
            .unwrap();
        drop(tx);

        let stdin = tokio::io::BufReader::new("/use oc_a\nreply\n/quit\n".as_bytes());
        let mut output = Vec::new();
        run_console_loop(ConsoleState::new(), api.clone(), rx, stdin, &mut output)
            .await
            .unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("chat_id=oc_a"));
        assert!(output.contains("sender=ou_1"));
        assert!(output.contains("sent chat_id=oc_a"));
        assert_eq!(
            *api.sent.lock().unwrap(),
            vec![(ChatId("oc_a".to_string()), "reply".to_string())]
        );
    }

    #[test]
    fn check_config_validates_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let valid = dir.path().join("valid.toml");
        std::fs::write(
            &valid,
            r#"
[lark]
app_id = "cli_xxx"
app_secret = "secret_xxx"

[[agents]]
id = "codex-dev"
kind = "codex"
command = "codex"
"#,
        )
        .unwrap();
        let invalid = dir.path().join("invalid.toml");
        std::fs::write(&invalid, "not toml").unwrap();

        assert!(load_checked_config(&valid).is_ok());
        assert!(load_checked_config(&invalid).is_err());
    }

    #[tokio::test]
    async fn non_dry_run_fails_when_inbound_source_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("valid.toml");
        std::fs::write(
            &config,
            r#"
[lark]
app_id = "cli_xxx"
app_secret = "secret_xxx"

[[agents]]
id = "codex-dev"
kind = "codex"
command = "codex"
"#,
        )
        .unwrap();

        let err = run_from_args(vec!["--config".to_string(), config.display().to_string()])
            .await
            .unwrap_err();

        assert!(matches!(err, CliError::InboundEventsUnavailable(_)));
    }

    #[test]
    fn default_config_path_is_cwd_relative() {
        let options = CliOptions::parse(Vec::<String>::new()).unwrap();

        assert_eq!(options.config_path, PathBuf::from("config.toml"));
    }

    fn message(chat_id: &str, sender: &str, msg_id: &str, text: &str) -> UserMessage {
        UserMessage {
            chat_id: ChatId(chat_id.to_string()),
            sender: UserId(sender.to_string()),
            text: text.to_string(),
            msg_id: msg_id.to_string(),
        }
    }
}
