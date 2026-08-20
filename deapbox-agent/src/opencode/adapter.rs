//! opencode wire layer — NDJSON event parsing + mapping to `AgentEvent`.
//!
//! Independent from `adapter.rs` (claude stream-json kinds) per ADR-0010:
//! opencode's protocol shares only "NDJSON line + type field dispatch" with
//! claude stream-json; turn-end (`step_finish` vs `result`), resume_key
//! (`sessionID` vs `session_id`), and assistant-block structure all differ.
//!
//! Pure functions only — no IO, no spawn. Testable in isolation.

use deapbox_core::types::AgentEvent;
use serde_json::Value;

/// Guardrail against accidentally buffering arbitrary stdout as a protocol
/// frame. Real opencode events should stay far below this.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// One parsed opencode NDJSON event after `type` dispatch.
#[derive(Debug, Clone, PartialEq)]
pub enum OpenCodeEvent {
    /// `step_start` — a new reasoning/tool step begins. Not mapped to
    /// `AgentEvent` (host-internal turn state).
    StepStart { session_id: String },
    /// `text` — assistant text output.
    Text { text: String, session_id: String },
    /// `tool_use` — tool invocation (with optional result if state.status=completed).
    ToolUse {
        tool: String,
        status: String,
        output: Option<String>,
        session_id: String,
    },
    /// `step_finish` — step completed.
    /// `reason="stop"` on the final step = turn end; `reason="tool-calls"` =
    /// mid-turn step (agent will continue with another step).
    StepFinish { reason: String, session_id: String },
    /// Any other `type` value we don't recognize yet.
    Unknown {
        kind: String,
        session_id: Option<String>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("opencode line exceeds {limit} bytes: {actual} bytes")]
    LineTooLong { actual: usize, limit: usize },
    #[error("opencode line is not valid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("opencode event is not a JSON object")]
    NotObject,
    #[error("opencode event is missing a string `type` field")]
    MissingType,
}

/// Parse one NDJSON line into an `OpenCodeEvent`.
///
/// Empty or whitespace-only lines return `Ok(None)` (ignored). Non-JSON,
/// non-object, or missing `type` return `Err`. Unknown `type` values return
/// `Ok(Some(OpenCodeEvent::Unknown { .. }))` — the caller decides whether to
/// log or drop.
pub fn parse_event_line(line: &str) -> Result<Option<OpenCodeEvent>, WireError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }

    let len = line.len();
    if len > MAX_LINE_BYTES {
        return Err(WireError::LineTooLong {
            actual: len,
            limit: MAX_LINE_BYTES,
        });
    }

    let raw: Value = serde_json::from_str(line)?;
    if !raw.is_object() {
        return Err(WireError::NotObject);
    }

    let kind = raw
        .get("type")
        .and_then(Value::as_str)
        .ok_or(WireError::MissingType)?;

    let session_id = raw
        .get("sessionID")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();

    let event = match kind {
        "step_start" => OpenCodeEvent::StepStart { session_id },
        "text" => {
            let text = extract_part_text(&raw);
            OpenCodeEvent::Text { text, session_id }
        }
        "tool_use" => {
            let (tool, status, output) = extract_tool_fields(&raw);
            OpenCodeEvent::ToolUse {
                tool,
                status,
                output,
                session_id,
            }
        }
        "step_finish" => {
            let reason = raw
                .get("part")
                .and_then(|p| p.get("reason"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            OpenCodeEvent::StepFinish { reason, session_id }
        }
        other => OpenCodeEvent::Unknown {
            kind: other.to_owned(),
            session_id: if session_id.is_empty() {
                None
            } else {
                Some(session_id)
            },
        },
    };

    Ok(Some(event))
}

/// Map an `OpenCodeEvent` to an `AgentEvent` (or `None` if not mapped).
///
/// Mapping rules (per ADR-0010 + ADR-0002 TurnEnd abstraction):
/// - `StepStart` → `None` (host-internal state, not rendered to operator)
/// - `Text` → `AgentEvent::Text(text)`
/// - `ToolUse` → `AgentEvent::ToolCall(tool)` if status != "completed";
///   if status == "completed" and output present, also emit
///   `AgentEvent::ToolResult(output)` after the ToolCall
/// - `StepFinish` with `reason="stop"` → `AgentEvent::TurnEnd { resume_key: Some(session_id) }`
/// - `StepFinish` with other reason (e.g. "tool-calls") → `None` (mid-turn step)
/// - `Unknown` → `None`
///
/// Returns `Vec<AgentEvent>` because `ToolUse` can produce two events
/// (ToolCall + ToolResult).
pub fn event_to_agent_events(event: &OpenCodeEvent) -> Vec<AgentEvent> {
    match event {
        OpenCodeEvent::StepStart { .. } => Vec::new(),
        OpenCodeEvent::Text { text, .. } => vec![AgentEvent::Text(text.clone())],
        OpenCodeEvent::ToolUse {
            tool,
            status,
            output,
            ..
        } => {
            let mut events = vec![AgentEvent::ToolCall(tool.clone())];
            if status == "completed" {
                if let Some(out) = output {
                    if !out.is_empty() {
                        events.push(AgentEvent::ToolResult(out.clone()));
                    }
                }
            }
            events
        }
        OpenCodeEvent::StepFinish { reason, session_id } => {
            if reason == "stop" {
                let resume_key = if session_id.is_empty() {
                    None
                } else {
                    Some(session_id.clone())
                };
                vec![AgentEvent::TurnEnd { resume_key }]
            } else {
                Vec::new()
            }
        }
        OpenCodeEvent::Unknown { .. } => Vec::new(),
    }
}

// ============ Internal extraction helpers ============

/// Extract `part.text` from a `text` event.
fn extract_part_text(raw: &Value) -> String {
    raw.get("part")
        .and_then(|p| p.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// Extract `(tool, status, output)` from a `tool_use` event.
///
/// `output` is `Some` only if `part.state.status == "completed"` and
/// `part.state.output` is a non-empty string.
fn extract_tool_fields(raw: &Value) -> (String, String, Option<String>) {
    let part = raw.get("part").unwrap_or(&Value::Null);
    let tool = part
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    let state = part.get("state").unwrap_or(&Value::Null);
    let status = state
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let output = if status == "completed" {
        state
            .get("output")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_owned())
    } else {
        None
    };
    (tool, status, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(line: &str) -> OpenCodeEvent {
        parse_event_line(line).expect("parse").expect("event")
    }

    // ============ parse_event_line: happy path ============

    #[test]
    fn parses_step_start() {
        let ev = parse(
            r#"{"type":"step_start","timestamp":1787243791277,"sessionID":"ses_abc","part":{"type":"step-start","messageID":"msg_1","sessionID":"ses_abc"}}"#,
        );
        assert_eq!(
            ev,
            OpenCodeEvent::StepStart {
                session_id: "ses_abc".to_owned()
            }
        );
    }

    #[test]
    fn parses_text_event() {
        let ev = parse(
            r#"{"type":"text","timestamp":1787243791568,"sessionID":"ses_abc","part":{"type":"text","text":"hello world","time":{"start":1,"end":2}}}"#,
        );
        assert_eq!(
            ev,
            OpenCodeEvent::Text {
                text: "hello world".to_owned(),
                session_id: "ses_abc".to_owned()
            }
        );
    }

    #[test]
    fn parses_tool_use_in_progress() {
        let ev = parse(
            r#"{"type":"tool_use","sessionID":"ses_abc","part":{"type":"tool","tool":"read","callID":"call_1","state":{"status":"running","input":{"filePath":"/tmp/x"}}}}"#,
        );
        assert_eq!(
            ev,
            OpenCodeEvent::ToolUse {
                tool: "read".to_owned(),
                status: "running".to_owned(),
                output: None,
                session_id: "ses_abc".to_owned()
            }
        );
    }

    #[test]
    fn parses_tool_use_completed_with_output() {
        let ev = parse(
            r#"{"type":"tool_use","sessionID":"ses_abc","part":{"type":"tool","tool":"bash","callID":"call_2","state":{"status":"completed","output":"file1\nfile2\n","input":{"command":"ls"},"metadata":{"exit":0}}}}"#,
        );
        assert_eq!(
            ev,
            OpenCodeEvent::ToolUse {
                tool: "bash".to_owned(),
                status: "completed".to_owned(),
                output: Some("file1\nfile2\n".to_owned()),
                session_id: "ses_abc".to_owned()
            }
        );
    }

    #[test]
    fn parses_tool_use_error_status() {
        let ev = parse(
            r#"{"type":"tool_use","sessionID":"ses_abc","part":{"type":"tool","tool":"read","callID":"call_3","state":{"status":"error","error":"File not found","input":{"filePath":"/tmp/missing"}}}}"#,
        );
        assert_eq!(
            ev,
            OpenCodeEvent::ToolUse {
                tool: "read".to_owned(),
                status: "error".to_owned(),
                output: None,
                session_id: "ses_abc".to_owned()
            }
        );
    }

    #[test]
    fn parses_step_finish_stop_reason() {
        let ev = parse(
            r#"{"type":"step_finish","timestamp":1787243791568,"sessionID":"ses_abc","part":{"type":"step-finish","reason":"stop","tokens":{"total":15718}}}"#,
        );
        assert_eq!(
            ev,
            OpenCodeEvent::StepFinish {
                reason: "stop".to_owned(),
                session_id: "ses_abc".to_owned()
            }
        );
    }

    #[test]
    fn parses_step_finish_tool_calls_reason() {
        let ev = parse(
            r#"{"type":"step_finish","sessionID":"ses_abc","part":{"type":"step-finish","reason":"tool-calls"}}"#,
        );
        assert_eq!(
            ev,
            OpenCodeEvent::StepFinish {
                reason: "tool-calls".to_owned(),
                session_id: "ses_abc".to_owned()
            }
        );
    }

    // ============ parse_event_line: unknown type ============

    #[test]
    fn parses_unknown_type() {
        let ev = parse(r#"{"type":"some_future_event","sessionID":"ses_abc","payload":42}"#);
        assert_eq!(
            ev,
            OpenCodeEvent::Unknown {
                kind: "some_future_event".to_owned(),
                session_id: Some("ses_abc".to_owned())
            }
        );
    }

    #[test]
    fn parses_unknown_type_without_session_id() {
        let ev = parse(r#"{"type":"system_event","payload":"boot"}"#);
        assert_eq!(
            ev,
            OpenCodeEvent::Unknown {
                kind: "system_event".to_owned(),
                session_id: None
            }
        );
    }

    // ============ parse_event_line: edge cases ============

    #[test]
    fn ignores_empty_lines() {
        assert!(parse_event_line("").unwrap().is_none());
        assert!(parse_event_line("   \n  ").unwrap().is_none());
    }

    #[test]
    fn rejects_non_json() {
        let err = parse_event_line("plain stdout").unwrap_err();
        assert!(matches!(err, WireError::InvalidJson(_)));
    }

    #[test]
    fn rejects_non_object_json() {
        let err = parse_event_line(r#""just a string""#).unwrap_err();
        assert!(matches!(err, WireError::NotObject));
    }

    #[test]
    fn rejects_missing_type_field() {
        let err = parse_event_line(r#"{"sessionID":"ses_x","payload":"no type"}"#).unwrap_err();
        assert!(matches!(err, WireError::MissingType));
    }

    #[test]
    fn rejects_oversized_line() {
        let big = format!(
            r#"{{"type":"text","part":{{"text":"{}"}}}}"#,
            "x".repeat(MAX_LINE_BYTES)
        );
        let err = parse_event_line(&big).unwrap_err();
        assert!(matches!(err, WireError::LineTooLong { .. }));
    }

    #[test]
    fn handles_missing_session_id_gracefully() {
        // some event types might not carry sessionID; shouldn't crash
        let ev = parse(r#"{"type":"step_start","part":{"type":"step-start"}}"#);
        assert_eq!(
            ev,
            OpenCodeEvent::StepStart {
                session_id: "".to_owned()
            }
        );
    }

    // ============ event_to_agent_events: mapping ============

    #[test]
    fn step_start_maps_to_nothing() {
        let ev = OpenCodeEvent::StepStart {
            session_id: "ses_x".to_owned(),
        };
        assert!(event_to_agent_events(&ev).is_empty());
    }

    #[test]
    fn text_maps_to_agent_text() {
        let ev = OpenCodeEvent::Text {
            text: "hello".to_owned(),
            session_id: "ses_x".to_owned(),
        };
        assert_eq!(
            event_to_agent_events(&ev),
            vec![AgentEvent::Text("hello".to_owned())]
        );
    }

    #[test]
    fn tool_use_running_maps_to_only_tool_call() {
        let ev = OpenCodeEvent::ToolUse {
            tool: "read".to_owned(),
            status: "running".to_owned(),
            output: None,
            session_id: "ses_x".to_owned(),
        };
        assert_eq!(
            event_to_agent_events(&ev),
            vec![AgentEvent::ToolCall("read".to_owned())]
        );
    }

    #[test]
    fn tool_use_completed_with_output_maps_to_tool_call_then_tool_result() {
        let ev = OpenCodeEvent::ToolUse {
            tool: "bash".to_owned(),
            status: "completed".to_owned(),
            output: Some("file1\nfile2".to_owned()),
            session_id: "ses_x".to_owned(),
        };
        assert_eq!(
            event_to_agent_events(&ev),
            vec![
                AgentEvent::ToolCall("bash".to_owned()),
                AgentEvent::ToolResult("file1\nfile2".to_owned())
            ]
        );
    }

    #[test]
    fn tool_use_completed_empty_output_maps_to_only_tool_call() {
        let ev = OpenCodeEvent::ToolUse {
            tool: "bash".to_owned(),
            status: "completed".to_owned(),
            output: Some("".to_owned()),
            session_id: "ses_x".to_owned(),
        };
        assert_eq!(
            event_to_agent_events(&ev),
            vec![AgentEvent::ToolCall("bash".to_owned())]
        );
    }

    #[test]
    fn tool_use_error_status_maps_to_only_tool_call() {
        let ev = OpenCodeEvent::ToolUse {
            tool: "read".to_owned(),
            status: "error".to_owned(),
            output: None,
            session_id: "ses_x".to_owned(),
        };
        assert_eq!(
            event_to_agent_events(&ev),
            vec![AgentEvent::ToolCall("read".to_owned())]
        );
    }

    #[test]
    fn step_finish_stop_maps_to_turn_end_with_resume_key() {
        let ev = OpenCodeEvent::StepFinish {
            reason: "stop".to_owned(),
            session_id: "ses_abc".to_owned(),
        };
        assert_eq!(
            event_to_agent_events(&ev),
            vec![AgentEvent::TurnEnd {
                resume_key: Some("ses_abc".to_owned())
            }]
        );
    }

    #[test]
    fn step_finish_stop_empty_session_id_maps_to_turn_end_none() {
        let ev = OpenCodeEvent::StepFinish {
            reason: "stop".to_owned(),
            session_id: "".to_owned(),
        };
        assert_eq!(
            event_to_agent_events(&ev),
            vec![AgentEvent::TurnEnd { resume_key: None }]
        );
    }

    #[test]
    fn step_finish_tool_calls_maps_to_nothing() {
        let ev = OpenCodeEvent::StepFinish {
            reason: "tool-calls".to_owned(),
            session_id: "ses_x".to_owned(),
        };
        assert!(event_to_agent_events(&ev).is_empty());
    }

    #[test]
    fn unknown_event_maps_to_nothing() {
        let ev = OpenCodeEvent::Unknown {
            kind: "future".to_owned(),
            session_id: Some("ses_x".to_owned()),
        };
        assert!(event_to_agent_events(&ev).is_empty());
    }

    // ============ real opencode output regression ============

    #[test]
    fn regression_real_opencode_text_event() {
        // captured from `opencode run --format json` actual output
        let line = r#"{"type":"text","timestamp":1787243791568,"sessionID":"ses_fdff80dbbffe52UfyBSD4L353P","part":{"id":"prt_0200803df0017rZp3le3Igg3A5","messageID":"msg_02007f4830014b2dQctnn2RQXQ","sessionID":"ses_fdff80dbbffe52UfyBSD4L353P","type":"text","text":"Hi there, how are you?","time":{"start":1787243791327,"end":1787243791541}}}"#;
        let ev = parse(line);
        assert_eq!(
            ev,
            OpenCodeEvent::Text {
                text: "Hi there, how are you?".to_owned(),
                session_id: "ses_fdff80dbbffe52UfyBSD4L353P".to_owned()
            }
        );
        assert_eq!(
            event_to_agent_events(&ev),
            vec![AgentEvent::Text("Hi there, how are you?".to_owned())]
        );
    }

    #[test]
    fn regression_real_opencode_step_finish_stop() {
        let line = r#"{"type":"step_finish","timestamp":1787243791568,"sessionID":"ses_fdff80dbbffe52UfyBSD4L353P","part":{"id":"prt_0200804ba00126t1A0jU5CB3oa","reason":"stop","messageID":"msg_02007f4830014b2dQctnn2RQXQ","sessionID":"ses_fdff80dbbffe52UfyBSD4L353P","type":"step-finish","tokens":{"total":15718,"input":15709,"output":0,"reasoning":9,"cache":{"write":0,"read":0}},"cost":0}}"#;
        let ev = parse(line);
        assert_eq!(
            ev,
            OpenCodeEvent::StepFinish {
                reason: "stop".to_owned(),
                session_id: "ses_fdff80dbbffe52UfyBSD4L353P".to_owned()
            }
        );
        assert_eq!(
            event_to_agent_events(&ev),
            vec![AgentEvent::TurnEnd {
                resume_key: Some("ses_fdff80dbbffe52UfyBSD4L353P".to_owned())
            }]
        );
    }

    // ============ helper: line from json value ============

    #[allow(dead_code)]
    fn line_from(value: serde_json::Value) -> String {
        value.to_string()
    }

    #[test]
    fn _compile_check_line_from() {
        let _ = line_from(json!({"type": "text"}));
    }
}
