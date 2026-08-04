use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use deapbox_core::types::{AppConfig, ChatId, UserMessage};
use deapbox_lark::{LarkEventBridge, LarkMessageApi, OpenLarkMessageApi};
use deapbox_store::config::{load_config, ConfigError};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOptions {
    pub config_path: PathBuf,
    pub check_config: bool,
    pub dry_run: bool,
}

impl CliOptions {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, CliError> {
        let mut config_path = PathBuf::from("deapbox-cli/src/config.toml");
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
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Default)]
pub struct ConsoleState {
    seen_chats: BTreeMap<ChatId, ChatSummary>,
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

    pub fn record_inbound(&mut self, message: &UserMessage) -> String {
        self.seen_chats.insert(
            message.chat_id.clone(),
            ChatSummary {
                sender: message.sender.0.clone(),
                last_text: message.text.clone(),
            },
        );
        format!(
            "[chat_id={}] sender={} msg_id={} text={}",
            message.chat_id.0, message.sender.0, message.msg_id, message.text
        )
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
    let input = input.trim();
    if input.is_empty() {
        return CommandResult::line("");
    }

    if input == "/quit" || input == "/exit" {
        return CommandResult::quit();
    }
    if input == "/help" {
        return CommandResult::line("/chats | /use <chat_id> | /send <chat_id> <text> | /quit");
    }
    if input == "/chats" {
        return CommandResult {
            output: state.list_chats(),
            should_quit: false,
        };
    }
    if let Some(rest) = input.strip_prefix("/use ") {
        let chat_id = rest.trim();
        if chat_id.is_empty() {
            return CommandResult::line("usage: /use <chat_id>");
        }
        state.current_chat = Some(ChatId(chat_id.to_string()));
        return CommandResult::line(format!("current chat_id={chat_id}"));
    }
    if let Some(rest) = input.strip_prefix("/send ") {
        let Some((chat_id, text)) = rest.trim().split_once(char::is_whitespace) else {
            return CommandResult::line("usage: /send <chat_id> <text>");
        };
        return send_text(api, ChatId(chat_id.to_string()), text.trim()).await;
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

pub fn load_checked_config(path: &Path) -> Result<AppConfig, CliError> {
    Ok(load_config(path)?)
}

pub async fn run_from_args(args: impl IntoIterator<Item = String>) -> Result<(), CliError> {
    let options = CliOptions::parse(args)?;
    let config = load_checked_config(&options.config_path)?;

    if options.check_config {
        println!("configuration ok: {}", options.config_path.display());
        return Ok(());
    }

    let (inbound_tx, inbound_rx) = mpsc::channel(128);
    let _event_bridge = LarkEventBridge::new(inbound_tx);
    let api = OpenLarkMessageApi::new(&config.lark)?;

    if options.dry_run {
        println!("dry-run: config loaded; Lark event bridge is ready");
        return Ok(());
    }

    println!("deapbox console ready. Type /help for commands.");
    run_console_loop(
        ConsoleState::new(),
        api,
        inbound_rx,
        tokio::io::BufReader::new(tokio::io::stdin()),
        tokio::io::stdout(),
    )
    .await
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
            message = inbound_rx.recv(), if inbound_open => {
                match message {
                    Some(message) => {
                        write_line(&mut stdout, &state.record_inbound(&message)).await?;
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
    "usage: deapbox [--config <path>] [--check-config] [--dry-run]".to_string()
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
        let first = state.record_inbound(&message("oc_a", "ou_1", "om_1", "hello A"));
        let second = state.record_inbound(&message("oc_b", "ou_2", "om_2", "hello B"));

        assert!(first.contains("chat_id=oc_a"));
        assert!(first.contains("sender=ou_1"));
        assert!(first.contains("text=hello A"));
        assert!(second.contains("chat_id=oc_b"));
        assert!(second.contains("sender=ou_2"));
        assert!(second.contains("text=hello B"));
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

    fn message(chat_id: &str, sender: &str, msg_id: &str, text: &str) -> UserMessage {
        UserMessage {
            chat_id: ChatId(chat_id.to_string()),
            sender: UserId(sender.to_string()),
            text: text.to_string(),
            msg_id: msg_id.to_string(),
        }
    }
}
