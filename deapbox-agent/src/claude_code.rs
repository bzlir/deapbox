//! Claude Code `stream-json` driver and long-lived session.

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex as StdMutex,
};

use async_trait::async_trait;
use deapbox_core::{
    traits::{AgentDriver, AgentEventReceiver, AgentSession},
    types::{AgentConfig, AgentEvent, CoreError, NormalizedEvent, WorkspacePath},
};
use nix::{
    sys::signal::{kill, Signal},
    unistd::Pid,
};
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout},
    sync::{broadcast, Mutex},
};

use crate::{
    adapter::{dispatch_ndjson_line, shared_agent_event, StreamJsonEvent},
    protocol::spawn_stdio,
};

const EVENT_CHANNEL_CAPACITY: usize = 256;

/// Driver factory for Claude Code sessions.
#[derive(Debug, Clone)]
pub struct ClaudeCodeDriver {
    config: AgentConfig,
}

impl ClaudeCodeDriver {
    pub fn new(config: AgentConfig) -> Self {
        Self { config }
    }

    fn session_config(&self, resume: Option<&str>) -> AgentConfig {
        let mut config = self.config.clone();
        config.args.extend([
            "--input-format".to_owned(),
            "stream-json".to_owned(),
            "--output-format".to_owned(),
            "stream-json".to_owned(),
            "--verbose".to_owned(),
        ]);
        if let Some(resume) = resume.filter(|key| !key.is_empty()) {
            config
                .args
                .extend(["--resume".to_owned(), resume.to_owned()]);
        }
        config
    }
}

#[async_trait]
impl AgentDriver for ClaudeCodeDriver {
    async fn start_session(
        &self,
        resume: Option<&str>,
        workspace: &WorkspacePath,
    ) -> Result<Box<dyn AgentSession>, CoreError> {
        let handles = spawn_stdio(&self.session_config(resume), workspace)?;
        Ok(Box::new(ClaudeCodeSession::new(
            handles.child,
            handles.stdin,
            handles.stdout,
        )))
    }
}

/// Long-lived Claude Code process session.
pub struct ClaudeCodeSession {
    child: Arc<Mutex<Child>>,
    stdin: Arc<Mutex<ChildStdin>>,
    tx: broadcast::Sender<AgentEvent>,
    resume_key: Arc<StdMutex<Option<String>>>,
    alive: Arc<AtomicBool>,
}

impl ClaudeCodeSession {
    fn new(child: Child, stdin: ChildStdin, stdout: BufReader<ChildStdout>) -> Self {
        let (tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        let session = Self {
            child: Arc::new(Mutex::new(child)),
            stdin: Arc::new(Mutex::new(stdin)),
            tx,
            resume_key: Arc::new(StdMutex::new(None)),
            alive: Arc::new(AtomicBool::new(true)),
        };
        session.spawn_read_loop(stdout);
        session
    }

    fn spawn_read_loop(&self, stdout: BufReader<ChildStdout>) {
        let child = Arc::clone(&self.child);
        let tx = self.tx.clone();
        let resume_key = Arc::clone(&self.resume_key);
        let alive = Arc::clone(&self.alive);

        tokio::spawn(async move {
            read_stdout(stdout, tx.clone(), Arc::clone(&resume_key)).await;

            let exit_code = {
                let mut child = child.lock().await;
                match child.wait().await {
                    Ok(status) => status.code(),
                    Err(err) => {
                        let _ = tx.send(AgentEvent::Failed(CoreError::AgentProcess(format!(
                            "wait failed: {}",
                            err
                        ))));
                        None
                    }
                }
            };

            alive.store(false, Ordering::Release);
            let _ = tx.send(AgentEvent::Exited(exit_code));
        });
    }

    #[cfg(test)]
    fn from_handles(child: Child, stdin: ChildStdin, stdout: BufReader<ChildStdout>) -> Self {
        Self::new(child, stdin, stdout)
    }
}

#[async_trait]
impl AgentSession for ClaudeCodeSession {
    async fn send(&self, text: &str) -> Result<(), CoreError> {
        let frame = json!({
            "type": "user",
            "message": {
                "role": "user",
                "content": [
                    {
                        "type": "text",
                        "text": text,
                    }
                ],
            },
        });
        let mut stdin = self.stdin.lock().await;
        stdin
            .write_all(frame.to_string().as_bytes())
            .await
            .map_err(io_error)?;
        stdin.write_all(b"\n").await.map_err(io_error)?;
        stdin.flush().await.map_err(io_error)
    }

    fn subscribe(&self) -> AgentEventReceiver {
        self.tx.subscribe()
    }

    async fn interrupt(&self) -> Result<(), CoreError> {
        let pid = {
            let child = self.child.lock().await;
            child
                .id()
                .ok_or_else(|| CoreError::AgentProcess("process has no pid".into()))?
        };
        kill(Pid::from_raw(pid as i32), Signal::SIGINT)
            .map_err(|err| CoreError::AgentProcess(format!("SIGINT failed: {}", err)))
    }

    fn current_resume_key(&self) -> Option<String> {
        self.resume_key.lock().expect("resume key poisoned").clone()
    }

    fn alive(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }

    async fn close(self: Box<Self>) -> Result<(), CoreError> {
        let mut child = self.child.lock().await;
        if self.alive() {
            child.start_kill().map_err(io_error)?;
            self.alive.store(false, Ordering::Release);
        }
        Ok(())
    }
}

async fn read_stdout(
    mut stdout: BufReader<ChildStdout>,
    tx: broadcast::Sender<AgentEvent>,
    resume_key: Arc<StdMutex<Option<String>>>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match stdout.read_line(&mut line).await {
            Ok(0) => break,
            Ok(_) => match dispatch_ndjson_line(&line) {
                Ok(Some(event)) => emit_stream_event(event, &tx, &resume_key).await,
                Ok(None) => {}
                Err(err) => {
                    let _ = tx.send(AgentEvent::Failed(CoreError::AgentProcess(format!(
                        "stream-json parse failed: {}",
                        err
                    ))));
                }
            },
            Err(err) => {
                let _ = tx.send(AgentEvent::Failed(io_error(err)));
                break;
            }
        }
    }
}

async fn emit_stream_event(
    event: StreamJsonEvent,
    tx: &broadcast::Sender<AgentEvent>,
    resume_key: &Arc<StdMutex<Option<String>>>,
) {
    if let StreamJsonEvent::Assistant(raw) = &event {
        for normalized in assistant_events(raw) {
            let _ = tx.send(AgentEvent::Normalized(normalized));
        }
    }

    if let Some(AgentEvent::TurnEnd { resume_key: key }) = shared_agent_event(event) {
        *resume_key.lock().expect("resume key poisoned") = key.clone();
        let _ = tx.send(AgentEvent::TurnEnd { resume_key: key });
    }
}

fn assistant_events(raw: &Value) -> Vec<NormalizedEvent> {
    let mut events = Vec::new();
    collect_assistant_events(raw.get("message").unwrap_or(raw), &mut events);
    events
}

fn collect_assistant_events(value: &Value, events: &mut Vec<NormalizedEvent>) {
    match value {
        Value::Array(items) => {
            for item in items {
                collect_assistant_events(item, events);
            }
        }
        Value::Object(object) => {
            match object.get("type").and_then(Value::as_str) {
                Some("text") => push_string_field(
                    object,
                    &["text"],
                    |text| NormalizedEvent::Text(text.to_owned()),
                    events,
                ),
                Some("thinking") => push_string_field(
                    object,
                    &["thinking", "text"],
                    |text| NormalizedEvent::Thinking(text.to_owned()),
                    events,
                ),
                Some("tool_use") => {
                    let name = object.get("name").and_then(Value::as_str).unwrap_or("tool");
                    let input = object
                        .get("input")
                        .map(Value::to_string)
                        .unwrap_or_default();
                    events.push(NormalizedEvent::ToolCall(format_tool_call(name, &input)));
                }
                _ => {}
            }

            if let Some(content) = object.get("content") {
                collect_assistant_events(content, events);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn push_string_field(
    object: &serde_json::Map<String, Value>,
    fields: &[&str],
    build: impl Fn(&str) -> NormalizedEvent,
    events: &mut Vec<NormalizedEvent>,
) {
    if let Some(text) = fields
        .iter()
        .find_map(|field| object.get(*field).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
    {
        events.push(build(text));
    }
}

fn format_tool_call(name: &str, input: &str) -> String {
    if input.is_empty() {
        name.to_owned()
    } else {
        format!("{} {}", name, input)
    }
}

fn io_error(err: std::io::Error) -> CoreError {
    CoreError::Io(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::spawn_stdio;
    use deapbox_core::types::{AgentId, AgentKind};
    use std::{collections::HashMap, time::Duration};
    use tokio::time::timeout;

    fn shell_config(script: &str) -> AgentConfig {
        AgentConfig {
            id: AgentId("fake-claude".into()),
            kind: AgentKind::ClaudeCode,
            command: "sh".into(),
            args: vec!["-c".into(), script.into(), "fake-claude".into()],
            env_vars: HashMap::new(),
        }
    }

    fn temp_workspace() -> WorkspacePath {
        WorkspacePath(std::env::temp_dir())
    }

    async fn spawn_session(script: &str) -> ClaudeCodeSession {
        let handles = spawn_stdio(&shell_config(script), &temp_workspace()).expect("spawn");
        ClaudeCodeSession::from_handles(handles.child, handles.stdin, handles.stdout)
    }

    async fn recv_event(rx: &mut AgentEventReceiver) -> AgentEvent {
        timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("event timeout")
            .expect("event")
    }

    #[tokio::test]
    async fn driver_injects_stream_json_flags_and_resume_key() {
        let driver = ClaudeCodeDriver::new(shell_config(
            r#"ok=0
printf '%s\n' "$@" | grep -q -- '--input-format' && printf '%s\n' "$@" | grep -q -- '--output-format' && printf '%s\n' "$@" | grep -q -- '--resume' && ok=1
while IFS= read -r _; do
if [ "$ok" = 1 ]; then
printf '%s\n' '{"type":"result","session_id":"from-driver"}'
fi
done"#,
        ));

        let session = driver
            .start_session(Some("resume-0"), &temp_workspace())
            .await
            .expect("session");
        let mut rx = session.subscribe();
        session.send("start").await.expect("send");
        assert!(matches!(
            recv_event(&mut rx).await,
            AgentEvent::TurnEnd {
                resume_key: Some(key)
            } if key == "from-driver"
        ));
        assert_eq!(session.current_resume_key(), Some("from-driver".into()));
        session.close().await.expect("close");
    }

    #[tokio::test]
    async fn send_writes_stream_json_and_read_loop_emits_events() {
        let session = spawn_session(
            r#"while IFS= read -r line; do
case "$line" in *'"hello"'*)
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"plan"},{"type":"text","text":"hi"},{"type":"tool_use","name":"Read","input":{"file":"main.rs"}}]}}'
printf '%s\n' '{"type":"result","session_id":"resume-1"}'
;;
esac
done"#,
        )
        .await;
        let mut rx = session.subscribe();

        session.send("hello").await.expect("send");

        assert!(matches!(
            recv_event(&mut rx).await,
            AgentEvent::Normalized(NormalizedEvent::Thinking(text)) if text == "plan"
        ));
        assert!(matches!(
            recv_event(&mut rx).await,
            AgentEvent::Normalized(NormalizedEvent::Text(text)) if text == "hi"
        ));
        assert!(matches!(
            recv_event(&mut rx).await,
            AgentEvent::Normalized(NormalizedEvent::ToolCall(text)) if text.contains("Read") && text.contains("main.rs")
        ));
        assert!(matches!(
            recv_event(&mut rx).await,
            AgentEvent::TurnEnd {
                resume_key: Some(key)
            } if key == "resume-1"
        ));
        assert_eq!(session.current_resume_key(), Some("resume-1".into()));
        Box::new(session).close().await.expect("close");
    }

    #[tokio::test]
    async fn compaction_result_does_not_end_turn() {
        let session = spawn_session(
            r#"printf '%s\n' '{"type":"result","subtype":"compact","session_id":"skip"}'
while IFS= read -r _; do :; done"#,
        )
        .await;
        let mut rx = session.subscribe();

        assert!(timeout(Duration::from_millis(200), rx.recv())
            .await
            .is_err());
        assert_eq!(session.current_resume_key(), None);
        Box::new(session).close().await.expect("close");
    }

    #[tokio::test]
    async fn multiple_subscribers_receive_turn_events() {
        let session = spawn_session(
            r#"while IFS= read -r _; do
printf '%s\n' '{"type":"result","session_id":"shared"}'
done"#,
        )
        .await;
        let mut rx1 = session.subscribe();
        let mut rx2 = session.subscribe();

        session.send("go").await.expect("send");

        assert!(matches!(
            recv_event(&mut rx1).await,
            AgentEvent::TurnEnd {
                resume_key: Some(key)
            } if key == "shared"
        ));
        assert!(matches!(
            recv_event(&mut rx2).await,
            AgentEvent::TurnEnd {
                resume_key: Some(key)
            } if key == "shared"
        ));
        Box::new(session).close().await.expect("close");
    }

    #[tokio::test]
    async fn interrupt_sends_sigint_and_exit_is_observable() {
        let session = spawn_session(r#"trap 'exit 130' INT; while true; do sleep 1; done"#).await;
        let mut rx = session.subscribe();

        session.interrupt().await.expect("interrupt");

        assert!(matches!(
            recv_event(&mut rx).await,
            AgentEvent::Exited(Some(130)) | AgentEvent::Exited(None)
        ));
        assert!(!session.alive());
    }
}
