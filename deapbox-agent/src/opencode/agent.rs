//! `OpenCodeAgent: Agent` — spawn `opencode run` + stream NDJSON stdout.
//!
//! Process-per-turn model: each `send` spawns a fresh `opencode run --format
//! json --auto [--session <prev>] "<text>"` process. Stdout NDJSON lines are
//! parsed by `adapter::parse_event_line` + mapped via
//! `adapter::event_to_agent_events`, then pushed to an mpsc channel. The
//! channel closes when the process exits (dispatcher's `while let recv`
//! loop ends naturally).
//!
//! Cross-turn resume: the session ID from `step_finish{reason=stop}` is
//! stored in `self.session_id` (Arc<Mutex>) and passed as `--session` on the
//! next `send`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use deapbox_core::agent::Agent;
use deapbox_core::types::{AgentEvent, AgentEventStream, Attachment, ChatId, CoreError};

use super::adapter::{self, OpenCodeEvent};

/// `opencode run`-backed `Agent` impl. Process-per-turn, cross-turn resume
/// via `--session <ses_xxx>`.
///
/// One `OpenCodeAgent` instance per chat binding (each chat has its own
/// session ID chain). Cloning is cheap (Arc-shared inner state), but
/// typically the dispatcher holds one `Arc<OpenCodeAgent>` per chat.
pub struct OpenCodeAgent {
    /// Path to the `opencode` executable (e.g. "opencode" or "/usr/local/bin/opencode").
    command: String,
    /// Working directory for the opencode process (the Workspace).
    workspace: PathBuf,
    /// Optional model override (e.g. "anthropic/claude-sonnet-4").
    model: Option<String>,
    /// Cross-turn session ID for `--session` resume. `None` on first turn.
    session_id: Arc<Mutex<Option<String>>>,
}

impl OpenCodeAgent {
    pub fn new(command: impl Into<String>, workspace: impl Into<PathBuf>) -> Self {
        Self {
            command: command.into(),
            workspace: workspace.into(),
            model: None,
            session_id: Arc::new(Mutex::new(None)),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    /// Current session ID (for diagnostics / testing).
    pub fn current_session_id(&self) -> Option<String> {
        self.session_id
            .lock()
            .expect("session_id mutex poisoned")
            .clone()
    }
}

#[async_trait]
impl Agent for OpenCodeAgent {
    async fn send(
        &self,
        _chat_id: &ChatId,
        text: &str,
        _attachments: &[Attachment],
    ) -> Result<AgentEventStream, CoreError> {
        let (tx, rx) = mpsc::channel::<AgentEvent>(64);

        // Build args: run --format json --auto [--session <prev>] [--model <m>] "<text>"
        let mut args = vec![
            "run".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
            "--auto".to_owned(),
        ];

        let prev_session = self
            .session_id
            .lock()
            .expect("session_id mutex poisoned")
            .clone();
        if let Some(sid) = &prev_session {
            args.push("--session".to_owned());
            args.push(sid.clone());
        }

        if let Some(model) = &self.model {
            args.push("--model".to_owned());
            args.push(model.clone());
        }

        args.push(text.to_owned());

        // Spawn the opencode process
        let mut child = Command::new(&self.command)
            .args(&args)
            .current_dir(&self.workspace)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| CoreError::Agent(format!("failed to spawn opencode: {e}")))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| CoreError::Agent("opencode stdout not captured".to_owned()))?;
        let stderr = child.stderr.take();

        // Spawn the stdout reader task that pushes AgentEvents
        let session_id_clone = Arc::clone(&self.session_id);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();

            loop {
                line.clear();
                match reader.read_line(&mut line).await {
                    Ok(0) => break, // EOF — process exited
                    Ok(_) => {
                        let parsed = match adapter::parse_event_line(&line) {
                            Ok(Some(event)) => event,
                            Ok(None) => continue, // empty line
                            Err(err) => {
                                tracing::warn!(
                                    line = %line.trim(),
                                    error = %err,
                                    "opencode: failed to parse NDJSON line, skipped"
                                );
                                continue;
                            }
                        };

                        // Capture session ID from step_finish{reason=stop}
                        if let OpenCodeEvent::StepFinish { reason, session_id } = &parsed {
                            if reason == "stop" && !session_id.is_empty() {
                                *session_id_clone.lock().expect("session_id mutex poisoned") =
                                    Some(session_id.clone());
                            }
                        }

                        // Map to AgentEvent(s) and push
                        for agent_event in adapter::event_to_agent_events(&parsed) {
                            if tx.send(agent_event).await.is_err() {
                                // dispatcher dropped the receiver — stop reading
                                break;
                            }
                        }
                    }
                    Err(err) => {
                        tracing::warn!(
                            error = %err,
                            "opencode: failed to read stdout line"
                        );
                        break;
                    }
                }
            }

            // Wait for process to exit (drain stderr if present to avoid blocking)
            if let Some(mut stderr) = stderr {
                let mut err_buf = String::new();
                let _ = stderr.read_to_string(&mut err_buf).await;
                if !err_buf.trim().is_empty() {
                    tracing::debug!(stderr = %err_buf.trim(), "opencode stderr");
                }
            }

            let _ = child.wait().await;
            // tx drops here → channel closes → dispatcher's while let recv ends
        });

        Ok(rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use deapbox_core::agent::Agent;
    use deapbox_core::types::{AgentEvent, ChatId};
    use std::time::Duration;
    use tokio::time::timeout;

    async fn collect_stream(mut stream: AgentEventStream) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Some(event) = stream.recv().await {
            events.push(event);
        }
        events
    }

    /// Build a fake opencode script that emits canned NDJSON lines.
    fn fake_opencode_script(nljson_lines: &[&str]) -> String {
        let mut script = String::from("#!/bin/sh\n");
        for line in nljson_lines {
            // echo each line to stdout
            script.push_str(&format!("echo '{}'\n", line.replace('\'', "'\\''")));
        }
        script
    }

    /// Create a temp dir with an executable `fake-opencode` shell script.
    fn make_fake_opencode(nljson_lines: &[&str]) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().unwrap();
        let script_path = dir.path().join("fake-opencode");
        let script = fake_opencode_script(nljson_lines);
        std::fs::write(&script_path, &script).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&script_path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&script_path, perms).unwrap();
        }
        let path = script_path.to_string_lossy().to_string();
        (dir, path)
    }

    fn chat() -> ChatId {
        ChatId("oc_test".to_owned())
    }

    // ============ happy path: text + step_finish(stop) ============

    #[tokio::test]
    async fn send_streams_text_and_turn_end() {
        let lines = vec![
            r#"{"type":"step_start","sessionID":"ses_abc","part":{"type":"step-start"}}"#,
            r#"{"type":"text","sessionID":"ses_abc","part":{"type":"text","text":"hello world"}}"#,
            r#"{"type":"step_finish","sessionID":"ses_abc","part":{"type":"step-finish","reason":"stop"}}"#,
        ];
        let (_dir, fake_bin) = make_fake_opencode(&lines);

        let agent = OpenCodeAgent::new(&fake_bin, "/tmp");
        let stream = agent.send(&chat(), "anything", &[]).await.unwrap();
        let events = collect_stream(stream).await;

        assert_eq!(
            events,
            vec![
                AgentEvent::Text("hello world".to_owned()),
                AgentEvent::TurnEnd {
                    resume_key: Some("ses_abc".to_owned())
                },
            ]
        );
    }

    // ============ session ID captured for next turn ============

    #[tokio::test]
    async fn session_id_captured_after_turn_end() {
        let lines = vec![
            r#"{"type":"step_start","sessionID":"ses_first","part":{"type":"step-start"}}"#,
            r#"{"type":"text","sessionID":"ses_first","part":{"type":"text","text":"reply 1"}}"#,
            r#"{"type":"step_finish","sessionID":"ses_first","part":{"type":"step-finish","reason":"stop"}}"#,
        ];
        let (_dir, fake_bin) = make_fake_opencode(&lines);

        let agent = OpenCodeAgent::new(&fake_bin, "/tmp");
        assert!(agent.current_session_id().is_none());

        let stream = agent.send(&chat(), "first turn", &[]).await.unwrap();
        let _ = collect_stream(stream).await;

        assert_eq!(agent.current_session_id(), Some("ses_first".to_owned()));
    }

    // ============ mid-turn step_finish(tool-calls) doesn't end turn ============

    #[tokio::test]
    async fn mid_turn_step_finish_does_not_emit_turn_end() {
        let lines = vec![
            r#"{"type":"step_start","sessionID":"ses_x","part":{"type":"step-start"}}"#,
            r#"{"type":"tool_use","sessionID":"ses_x","part":{"type":"tool","tool":"read","state":{"status":"completed","output":"file content"}}}"#,
            r#"{"type":"step_finish","sessionID":"ses_x","part":{"type":"step-finish","reason":"tool-calls"}}"#,
            r#"{"type":"step_start","sessionID":"ses_x","part":{"type":"step-start"}}"#,
            r#"{"type":"text","sessionID":"ses_x","part":{"type":"text","text":"final answer"}}"#,
            r#"{"type":"step_finish","sessionID":"ses_x","part":{"type":"step-finish","reason":"stop"}}"#,
        ];
        let (_dir, fake_bin) = make_fake_opencode(&lines);

        let agent = OpenCodeAgent::new(&fake_bin, "/tmp");
        let stream = agent.send(&chat(), "do something", &[]).await.unwrap();
        let events = collect_stream(stream).await;

        // mid-turn step_finish(reason=tool-calls) → no TurnEnd
        // final step_finish(reason=stop) → TurnEnd
        assert_eq!(
            events,
            vec![
                AgentEvent::ToolCall("read".to_owned()),
                AgentEvent::ToolResult("file content".to_owned()),
                AgentEvent::Text("final answer".to_owned()),
                AgentEvent::TurnEnd {
                    resume_key: Some("ses_x".to_owned())
                },
            ]
        );
    }

    // ============ spawn failure returns error ============

    #[tokio::test]
    async fn spawn_failure_returns_core_error() {
        let agent = OpenCodeAgent::new("/nonexistent/path/opencode", "/tmp");
        let result = agent.send(&chat(), "hello", &[]).await;
        assert!(result.is_err());
        match result.unwrap_err() {
            CoreError::Agent(msg) => assert!(msg.contains("failed to spawn opencode")),
            other => panic!("expected CoreError::Agent, got {:?}", other),
        }
    }

    // ============ empty stdout (process exits immediately) ============

    #[tokio::test]
    async fn empty_stdout_produces_empty_stream() {
        let (_dir, fake_bin) = make_fake_opencode(&[]);
        let agent = OpenCodeAgent::new(&fake_bin, "/tmp");
        let stream = agent.send(&chat(), "hello", &[]).await.unwrap();
        let events = collect_stream(stream).await;
        assert!(events.is_empty());
    }

    // ============ stream closes when process exits ============

    #[tokio::test]
    async fn stream_closes_when_process_exits() {
        let lines = vec![
            r#"{"type":"text","sessionID":"ses_x","part":{"type":"text","text":"hi"}}"#,
            r#"{"type":"step_finish","sessionID":"ses_x","part":{"type":"step-finish","reason":"stop"}}"#,
        ];
        let (_dir, fake_bin) = make_fake_opencode(&lines);

        let agent = OpenCodeAgent::new(&fake_bin, "/tmp");
        let mut stream = agent.send(&chat(), "hello", &[]).await.unwrap();

        // collect with timeout — should complete well under 5s
        let events = timeout(Duration::from_secs(5), async {
            let mut events = Vec::new();
            while let Some(event) = stream.recv().await {
                events.push(event);
            }
            events
        })
        .await
        .expect("stream did not close within 5s");

        assert_eq!(events.len(), 2);
    }

    // ============ unknown event types are skipped, not fatal ============

    #[tokio::test]
    async fn unknown_event_types_skipped() {
        let lines = vec![
            r#"{"type":"step_start","sessionID":"ses_x","part":{}}"#,
            r#"{"type":"future_unknown_event","sessionID":"ses_x","payload":42}"#,
            r#"{"type":"text","sessionID":"ses_x","part":{"type":"text","text":"after unknown"}}"#,
            r#"{"type":"step_finish","sessionID":"ses_x","part":{"type":"step-finish","reason":"stop"}}"#,
        ];
        let (_dir, fake_bin) = make_fake_opencode(&lines);

        let agent = OpenCodeAgent::new(&fake_bin, "/tmp");
        let stream = agent.send(&chat(), "hello", &[]).await.unwrap();
        let events = collect_stream(stream).await;

        // unknown event skipped — only text + turn_end remain
        assert_eq!(
            events,
            vec![
                AgentEvent::Text("after unknown".to_owned()),
                AgentEvent::TurnEnd {
                    resume_key: Some("ses_x".to_owned())
                },
            ]
        );
    }

    // ============ malformed lines are skipped, not fatal ============

    #[tokio::test]
    async fn malformed_lines_skipped() {
        let lines = vec![
            r#"{"type":"text","sessionID":"ses_x","part":{"type":"text","text":"good"}}"#,
            "not valid json at all",
            r#"{"type":"step_finish","sessionID":"ses_x","part":{"type":"step-finish","reason":"stop"}}"#,
        ];
        let (_dir, fake_bin) = make_fake_opencode(&lines);

        let agent = OpenCodeAgent::new(&fake_bin, "/tmp");
        let stream = agent.send(&chat(), "hello", &[]).await.unwrap();
        let events = collect_stream(stream).await;

        // malformed line skipped — good + turn_end remain
        assert_eq!(
            events,
            vec![
                AgentEvent::Text("good".to_owned()),
                AgentEvent::TurnEnd {
                    resume_key: Some("ses_x".to_owned())
                },
            ]
        );
    }
}
