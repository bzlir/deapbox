//! Shared stream-json NDJSON helpers.
//!
//! Per-kind sessions own semantic mapping into `NormalizedEvent`; this module
//! provides the common wire boundary: one JSON object per stdout line, dispatch
//! by `type`, and deterministic `result` handling.

use deapbox_core::types::AgentEvent;
use serde_json::Value;

/// Guardrail against accidentally buffering arbitrary stdout as a protocol
/// frame. Real stream-json events should stay far below this.
pub const MAX_NDJSON_LINE_BYTES: usize = 1024 * 1024;

/// One parsed stream-json event after shared `type` dispatch.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamJsonEvent {
    Assistant(Value),
    User(Value),
    Result {
        raw: Value,
        resume_key: Option<String>,
    },
    System(Value),
    ControlRequest(Value),
    Unknown {
        kind: String,
        raw: Value,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum StreamJsonError {
    #[error("stream-json line exceeds {limit} bytes: {actual} bytes")]
    LineTooLong { actual: usize, limit: usize },
    #[error("stream-json line is not valid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("stream-json event is not a JSON object")]
    NotObject,
    #[error("stream-json event is missing a string `type` field")]
    MissingType,
}

/// Parse one NDJSON line into a JSON object.
///
/// Empty or whitespace-only lines are ignored. This function deliberately does
/// not clean ANSI, spinners, timestamps, or other free-form stdout; stream-json
/// mode is the protocol contract.
pub fn parse_ndjson_line(line: &str) -> Result<Option<Value>, StreamJsonError> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(None);
    }

    let len = line.len();
    if len > MAX_NDJSON_LINE_BYTES {
        return Err(StreamJsonError::LineTooLong {
            actual: len,
            limit: MAX_NDJSON_LINE_BYTES,
        });
    }

    let raw: Value = serde_json::from_str(line)?;
    if !raw.is_object() {
        return Err(StreamJsonError::NotObject);
    }

    Ok(Some(raw))
}

/// Parse and dispatch one NDJSON stream-json line.
///
/// `result` events with `subtype: compact|compaction` are filtered because they
/// are mid-turn context compaction notifications, not turn completion.
pub fn dispatch_ndjson_line(line: &str) -> Result<Option<StreamJsonEvent>, StreamJsonError> {
    let Some(raw) = parse_ndjson_line(line)? else {
        return Ok(None);
    };

    dispatch_stream_json_value(raw)
}

/// Dispatch an already parsed stream-json object by its `type` field.
pub fn dispatch_stream_json_value(raw: Value) -> Result<Option<StreamJsonEvent>, StreamJsonError> {
    let kind = raw
        .get("type")
        .and_then(Value::as_str)
        .ok_or(StreamJsonError::MissingType)?;

    let event = match kind {
        "assistant" => Some(StreamJsonEvent::Assistant(raw)),
        "user" => Some(StreamJsonEvent::User(raw)),
        "result" => {
            if is_compaction_result(&raw) {
                None
            } else {
                Some(StreamJsonEvent::Result {
                    resume_key: extract_resume_key(&raw),
                    raw,
                })
            }
        }
        "system" => Some(StreamJsonEvent::System(raw)),
        "control_request" => Some(StreamJsonEvent::ControlRequest(raw)),
        unknown => {
            log::debug!("unknown stream-json event type: {}", unknown);
            Some(StreamJsonEvent::Unknown {
                kind: unknown.to_owned(),
                raw,
            })
        }
    };

    Ok(event)
}

/// Convert shared protocol events that have host-level meaning.
///
/// Per-kind sessions should handle all `NormalizedEvent` mapping themselves.
/// Only a non-compaction `result` has a shared meaning: the turn ended.
pub fn shared_agent_event(event: StreamJsonEvent) -> Option<AgentEvent> {
    match event {
        StreamJsonEvent::Result { resume_key, .. } => Some(AgentEvent::TurnEnd { resume_key }),
        StreamJsonEvent::Assistant(_)
        | StreamJsonEvent::User(_)
        | StreamJsonEvent::System(_)
        | StreamJsonEvent::ControlRequest(_)
        | StreamJsonEvent::Unknown { .. } => None,
    }
}

fn is_compaction_result(raw: &Value) -> bool {
    raw.get("type").and_then(Value::as_str) == Some("result")
        && matches!(
            raw.get("subtype").and_then(Value::as_str),
            Some("compact" | "compaction")
        )
}

fn extract_resume_key(raw: &Value) -> Option<String> {
    ["session_id", "sessionId", "resume_key", "resumeKey"]
        .into_iter()
        .find_map(|field| raw.get(field).and_then(Value::as_str))
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispatch(line: &str) -> Option<StreamJsonEvent> {
        dispatch_ndjson_line(line).expect("dispatch")
    }

    #[test]
    fn parses_ndjson_line() {
        let raw = parse_ndjson_line(r#"{"type":"assistant","message":"hello"}"#)
            .expect("parse")
            .expect("event");

        assert_eq!(raw["type"], "assistant");
        assert_eq!(raw["message"], "hello");
    }

    #[test]
    fn ignores_empty_lines() {
        assert!(parse_ndjson_line("  \n").expect("parse").is_none());
    }

    #[test]
    fn rejects_non_json_lines() {
        assert!(matches!(
            parse_ndjson_line("plain stdout"),
            Err(StreamJsonError::InvalidJson(_))
        ));
    }

    #[test]
    fn rejects_non_object_json() {
        assert!(matches!(
            parse_ndjson_line(r#""text""#),
            Err(StreamJsonError::NotObject)
        ));
    }

    #[test]
    fn rejects_oversized_lines() {
        let line = format!(
            r#"{{"type":"assistant","message":"{}"}}"#,
            "x".repeat(MAX_NDJSON_LINE_BYTES)
        );

        assert!(matches!(
            parse_ndjson_line(&line),
            Err(StreamJsonError::LineTooLong { .. })
        ));
    }

    #[test]
    fn dispatches_known_types() {
        assert!(matches!(
            dispatch(r#"{"type":"assistant","message":"hello"}"#),
            Some(StreamJsonEvent::Assistant(_))
        ));
        assert!(matches!(
            dispatch(r#"{"type":"user","message":"hello"}"#),
            Some(StreamJsonEvent::User(_))
        ));
        assert!(matches!(
            dispatch(r#"{"type":"system","message":"init"}"#),
            Some(StreamJsonEvent::System(_))
        ));
        assert!(matches!(
            dispatch(r#"{"type":"control_request","request":"permission"}"#),
            Some(StreamJsonEvent::ControlRequest(_))
        ));
    }

    #[test]
    fn result_dispatches_to_turn_end_with_resume_key() {
        let event = dispatch(r#"{"type":"result","session_id":"abc123"}"#).expect("event");
        assert!(matches!(
            event,
            StreamJsonEvent::Result {
                resume_key: Some(ref key),
                ..
            } if key == "abc123"
        ));

        assert!(matches!(
            shared_agent_event(event),
            Some(AgentEvent::TurnEnd {
                resume_key: Some(key),
            }) if key == "abc123"
        ));
    }

    #[test]
    fn compaction_results_do_not_emit_turn_end() {
        assert!(dispatch(r#"{"type":"result","subtype":"compact"}"#).is_none());
        assert!(dispatch(r#"{"type":"result","subtype":"compaction"}"#).is_none());
    }

    #[test]
    fn unknown_type_is_observable() {
        assert!(matches!(
            dispatch(r#"{"type":"mystery","payload":1}"#),
            Some(StreamJsonEvent::Unknown { ref kind, .. }) if kind == "mystery"
        ));
    }

    #[test]
    fn missing_type_is_an_error() {
        assert!(matches!(
            dispatch_ndjson_line(r#"{"message":"hello"}"#),
            Err(StreamJsonError::MissingType)
        ));
    }
}
