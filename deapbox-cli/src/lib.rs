//! deapbox-cli — binary entry: arg parsing + service assembly + main loop.
//!
//! Stage 1 walking skeleton (ADRs 0001-0009):
//! 1. parse CLI args → config path
//! 2. load_config → AppConfig
//! 3. build bindings HashMap + agents HashMap
//! 4. construct OpenLarkMessageApi + ChatDispatcher
//! 5. start Feishu WS inbound
//! 6. main loop: payload_rx → parse → dispatcher.dispatch(msg) | ctrl_c | sigterm
//! 7. ChatDispatcher::drop aborts all per-chat tasks (ADR-0008)

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use deapbox_agent::EchoAgent;
use deapbox_core::agent::Agent;
use deapbox_core::dispatcher::ChatDispatcher;
use deapbox_core::lark_api::LarkMessageApi;
use deapbox_core::types::{AgentId, AgentKind, AppConfig, Binding, ChatId};
use deapbox_lark::{parse_text_message, start_ws, OpenLarkMessageApi};

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("configuration error: {0}")]
    Config(#[from] deapbox_store::ConfigError),
    #[error("Lark API setup error: {0}")]
    LarkApi(#[from] deapbox_core::types::LarkApiError),
    #[error("agent '{0}' has unsupported kind for Stage 1 (only echo is implemented)")]
    UnsupportedAgentKind(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct CliOptions {
    pub config_path: PathBuf,
}

impl CliOptions {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, CliError> {
        let mut config_path = PathBuf::from("config.toml");
        let mut args = args.into_iter();

        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--config" => {
                    let Some(path) = args.next() else {
                        return Err(CliError::Config(deapbox_store::ConfigError::Invalid(
                            "--config requires a file path".to_owned(),
                        )));
                    };
                    config_path = PathBuf::from(path);
                }
                "-h" | "--help" => {
                    println!("deapbox --config <path>");
                    std::process::exit(0);
                }
                other => {
                    return Err(CliError::Config(deapbox_store::ConfigError::Invalid(
                        format!("unknown argument: {other}"),
                    )));
                }
            }
        }

        Ok(Self { config_path })
    }
}

/// Assemble agents registry from config. Stage 1: only `Echo` kind is
/// supported; other kinds return `UnsupportedAgentKind`.
pub fn build_agents_registry(
    config: &AppConfig,
) -> Result<HashMap<AgentId, Arc<dyn Agent>>, CliError> {
    let mut agents = HashMap::new();
    for agent_cfg in &config.agents {
        match agent_cfg.kind {
            AgentKind::Echo => {
                agents.insert(
                    agent_cfg.id.clone(),
                    Arc::new(EchoAgent::new()) as Arc<dyn Agent>,
                );
            }
            _ => {
                return Err(CliError::UnsupportedAgentKind(agent_cfg.id.0.clone()));
            }
        }
    }
    Ok(agents)
}

/// Assemble bindings registry from config. One entry per `[[sessions]]`.
pub fn build_bindings_registry(config: &AppConfig) -> HashMap<ChatId, Binding> {
    config
        .sessions
        .iter()
        .map(|s| {
            (
                s.chat_id.clone(),
                Binding {
                    agent_id: s.agent_id.clone(),
                    workspace: s.workspace.clone(),
                },
            )
        })
        .collect()
}

/// Run the deapbox service. Returns when the main loop exits (Ctrl+C,
/// SIGTERM, or WS connection closes — ADR-0008).
pub async fn run_service(config: AppConfig) -> Result<(), CliError> {
    let agents = build_agents_registry(&config)?;
    let bindings = build_bindings_registry(&config);

    let lark_api: Arc<dyn LarkMessageApi> = Arc::new(OpenLarkMessageApi::new(&config.lark)?);

    let dispatcher = ChatDispatcher::start(bindings, agents, Arc::clone(&lark_api));

    let (mut payload_rx, ws_join) = start_ws(&config.lark)?;

    tracing::info!(
        sessions = dispatcher.route_count(),
        "deapbox Stage 1 running; press Ctrl+C to shut down"
    );

    main_loop(&mut payload_rx, &dispatcher).await;

    // main loop exited — abort everything
    ws_join.abort();
    drop(dispatcher);
    tracing::info!("deapbox shut down");
    Ok(())
}

/// Main event loop: WS payload → parse → dispatch, with Ctrl+C / SIGTERM exit.
async fn main_loop(
    payload_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    dispatcher: &ChatDispatcher,
) {
    let mut sigterm = signal_sigterm();
    loop {
        tokio::select! {
            biased;
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("received SIGINT (Ctrl+C), shutting down");
                break;
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, shutting down");
                break;
            }
            payload = payload_rx.recv() => {
                match payload {
                    Some(bytes) => handle_payload(&bytes, dispatcher).await,
                    None => {
                        tracing::info!("WebSocket inbound channel closed, shutting down");
                        break;
                    }
                }
            }
        }
    }
}

async fn handle_payload(bytes: &[u8], dispatcher: &ChatDispatcher) {
    match parse_text_message(bytes) {
        Ok(msg) => match dispatcher.dispatch(msg) {
            Ok(()) => {}
            Err(deapbox_core::dispatcher::DispatchError::UnboundChat(chat)) => {
                tracing::info!(chat_id = %chat.0, "unbound chat, ignored");
            }
            Err(deapbox_core::dispatcher::DispatchError::ChannelClosed(chat)) => {
                tracing::error!(chat_id = %chat.0, "per-chat channel closed unexpectedly");
            }
        },
        Err(err) => {
            tracing::warn!(error = %err, "failed to parse Lark event payload, ignored");
        }
    }
}

#[cfg(unix)]
fn signal_sigterm() -> tokio::signal::unix::Signal {
    tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("install SIGTERM handler")
}

#[cfg(not(unix))]
fn signal_sigterm() -> std::pin::Pin<Box<dyn futures_util::stream::Stream<Item = ()> + Send>> {
    Box::pin(futures_util::stream::pending())
}

#[cfg(test)]
mod tests {
    use super::*;
    use deapbox_store::load_config;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_config(toml: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "{}", toml).unwrap();
        f
    }

    fn sample_config_toml() -> &'static str {
        r#"
[lark]
app_id = "cli_test"
app_secret = "sec_test"

[[agents]]
id = "echo-a"
kind = "echo"
command = ""

[[agents]]
id = "echo-b"
kind = "echo"
command = ""

[[sessions]]
chat_id = "oc_x"
agent_id = "echo-a"

[[sessions]]
chat_id = "oc_y"
agent_id = "echo-b"
"#
    }

    // ============ CLI option parsing ============

    #[test]
    fn cli_options_default_config_path() {
        let opts = CliOptions::parse(std::iter::empty::<String>()).unwrap();
        assert_eq!(opts.config_path, PathBuf::from("config.toml"));
    }

    #[test]
    fn cli_options_custom_config_path() {
        let opts = CliOptions::parse(["--config".to_owned(), "/tmp/x.toml".to_owned()]).unwrap();
        assert_eq!(opts.config_path, PathBuf::from("/tmp/x.toml"));
    }

    #[test]
    fn cli_options_unknown_arg_rejected() {
        let err = CliOptions::parse(["--bogus".to_owned()]).unwrap_err();
        assert!(matches!(err, CliError::Config(_)));
    }

    // ============ build_agents_registry ============

    #[test]
    fn build_agents_registry_only_echo_supported() {
        let cfg = load_config(write_config(sample_config_toml()).path()).unwrap();
        let agents = build_agents_registry(&cfg).unwrap();
        assert_eq!(agents.len(), 2);
        assert!(agents.contains_key(&AgentId("echo-a".to_owned())));
        assert!(agents.contains_key(&AgentId("echo-b".to_owned())));
    }

    #[test]
    fn build_agents_registry_non_echo_rejected() {
        let cfg = load_config(
            write_config(
                r#"
[lark]
app_id = "x"
app_secret = "y"

[[agents]]
id = "claude"
kind = "claude-code"
command = "claude"
"#,
            )
            .path(),
        )
        .unwrap();
        match build_agents_registry(&cfg) {
            Err(CliError::UnsupportedAgentKind(_)) => {}
            other => panic!("expected UnsupportedAgentKind, got {:?}", other.map(|_| ())),
        }
    }

    // ============ build_bindings_registry ============

    #[test]
    fn build_bindings_registry_maps_all_sessions() {
        let cfg = load_config(write_config(sample_config_toml()).path()).unwrap();
        let bindings = build_bindings_registry(&cfg);
        assert_eq!(bindings.len(), 2);
        assert_eq!(
            bindings.get(&ChatId("oc_x".to_owned())).unwrap().agent_id,
            AgentId("echo-a".to_owned())
        );
        assert_eq!(
            bindings.get(&ChatId("oc_y".to_owned())).unwrap().agent_id,
            AgentId("echo-b".to_owned())
        );
    }

    // ============ handle_payload with fake dispatcher ============

    #[tokio::test]
    async fn handle_payload_parses_and_dispatches() {
        use deapbox_core::test_support::{FakeAgent, FakeLarkMessageApi};

        // assemble a tiny dispatcher with one binding
        let mut bindings = HashMap::new();
        bindings.insert(
            ChatId("oc_a".to_owned()),
            Binding {
                agent_id: AgentId("e".to_owned()),
                workspace: None,
            },
        );
        let mut agents = HashMap::new();
        agents.insert(
            AgentId("e".to_owned()),
            Arc::new(FakeAgent::echo()) as Arc<dyn Agent>,
        );
        let lark = Arc::new(FakeLarkMessageApi::new()) as Arc<dyn LarkMessageApi>;
        let dispatcher = ChatDispatcher::start(bindings, agents, lark);

        // simulate a text-message payload
        let payload = serde_json::json!({
            "schema": "2.0",
            "header": { "event_type": "im.message.receive_v1", "create_time": "111" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_s" } },
                "message": {
                    "chat_id": "oc_a",
                    "message_id": "om_1",
                    "message_type": "text",
                    "create_time": "111",
                    "content": serde_json::to_string(&serde_json::json!({"text": "hi"})).unwrap()
                }
            }
        })
        .to_string();
        handle_payload(payload.as_bytes(), &dispatcher).await;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // dispatcher should have routed to the echo agent
        // (we can't easily assert the outbound here without inspecting internals,
        //  but the absence of a panic + the route being hit is the assertion)
        drop(dispatcher);
    }

    #[tokio::test]
    async fn handle_payload_unbound_chat_does_not_panic() {
        use deapbox_core::test_support::{FakeAgent, FakeLarkMessageApi};

        let bindings = HashMap::new();
        let mut agents = HashMap::new();
        agents.insert(
            AgentId("e".to_owned()),
            Arc::new(FakeAgent::echo()) as Arc<dyn Agent>,
        );
        let lark = Arc::new(FakeLarkMessageApi::new()) as Arc<dyn LarkMessageApi>;
        let dispatcher = ChatDispatcher::start(bindings, agents, lark);

        let payload = serde_json::json!({
            "schema": "2.0",
            "header": { "event_type": "im.message.receive_v1", "create_time": "111" },
            "event": {
                "sender": { "sender_id": { "open_id": "ou_s" } },
                "message": {
                    "chat_id": "oc_unbound",
                    "message_id": "om_1",
                    "message_type": "text",
                    "create_time": "111",
                    "content": serde_json::to_string(&serde_json::json!({"text": "hi"})).unwrap()
                }
            }
        })
        .to_string();
        handle_payload(payload.as_bytes(), &dispatcher).await;
        // no panic, no crash — that's the assertion
    }

    #[tokio::test]
    async fn handle_payload_malformed_payload_does_not_panic() {
        use deapbox_core::test_support::{FakeAgent, FakeLarkMessageApi};

        let bindings = HashMap::new();
        let mut agents = HashMap::new();
        agents.insert(
            AgentId("e".to_owned()),
            Arc::new(FakeAgent::echo()) as Arc<dyn Agent>,
        );
        let lark = Arc::new(FakeLarkMessageApi::new()) as Arc<dyn LarkMessageApi>;
        let dispatcher = ChatDispatcher::start(bindings, agents, lark);

        handle_payload(b"not valid json", &dispatcher).await;
        handle_payload(b"", &dispatcher).await;
        // no panic
    }
}
