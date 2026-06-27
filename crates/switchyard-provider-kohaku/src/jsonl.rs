//! Parse the KohakuTerrarium headless JSONL stream.
//!
//! The `switchyard-headless` fork emits one JSON object per line on stdout:
//! `turn_start`, `text`, `activity` (with `activity_type` + nested
//! `metadata`), `turn_end` (`status`/`text`/`error`/`usage`), and a top-level
//! `error`. This module classifies each line into a [`KohakuEvent`]; the
//! adapter (`turn.rs`) then maps only the user-facing signal onto events:
//! assistant `text` deltas (sentinel-gated), genuine tool/subagent `activity`
//! (normalized to collapsed tool cards), and the turn outcome/error. Pure
//! runtime telemetry and `turn_start` are dropped so the chat shows only the
//! model's message; the raw protocol is preserved solely in the archived
//! stdout artifact, not as live `ItemUpdated` events.

use serde_json::Value;

#[derive(Debug, PartialEq)]
pub enum KohakuEvent {
    /// An assistant text delta (`{"type":"text","content":...}`).
    Text(String),
    /// The turn boundary (`{"type":"turn_end",...}`).
    TurnEnd {
        status: String,
        text: String,
        error: Option<String>,
    },
    /// Runtime activity (`{"type":"activity","activity_type":...,"metadata":...}`).
    /// `value` is the whole activity object so the adapter can read
    /// `activity_type` / `detail` / `metadata` when deciding what (if anything)
    /// to surface.
    Activity { activity_type: String, value: Value },
    /// The turn-start marker (`{"type":"turn_start",...}`).
    TurnStart,
    /// A top-level fatal error (`{"type":"error","content":...}`).
    Error(String),
    /// Anything else — carries no user-facing signal.
    Other,
}

pub fn classify(json: &Value) -> KohakuEvent {
    match json.get("type").and_then(Value::as_str) {
        Some("text") => KohakuEvent::Text(
            json.get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        ),
        Some("turn_end") => KohakuEvent::TurnEnd {
            status: json
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            text: json
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            error: json
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string),
        },
        Some("activity") => KohakuEvent::Activity {
            activity_type: json
                .get("activity_type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            value: json.clone(),
        },
        Some("turn_start") => KohakuEvent::TurnStart,
        Some("error") => KohakuEvent::Error(
            json.get("content")
                .and_then(Value::as_str)
                .or_else(|| json.get("error").and_then(Value::as_str))
                .or_else(|| json.get("message").and_then(Value::as_str))
                .unwrap_or("")
                .to_string(),
        ),
        _ => KohakuEvent::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_text_delta() {
        assert_eq!(
            classify(&json!({"type": "text", "content": "Hello"})),
            KohakuEvent::Text("Hello".to_string())
        );
    }

    #[test]
    fn classifies_turn_end_ok() {
        assert_eq!(
            classify(&json!({
                "type": "turn_end",
                "status": "ok",
                "text": "done",
                "error": null,
                "usage": {"total_tokens": 5}
            })),
            KohakuEvent::TurnEnd {
                status: "ok".to_string(),
                text: "done".to_string(),
                error: None,
            }
        );
    }

    #[test]
    fn classifies_turn_end_error() {
        assert_eq!(
            classify(&json!({
                "type": "turn_end",
                "status": "error",
                "text": "",
                "error": "boom"
            })),
            KohakuEvent::TurnEnd {
                status: "error".to_string(),
                text: String::new(),
                error: Some("boom".to_string()),
            }
        );
    }

    #[test]
    fn classifies_activity_with_type() {
        assert_eq!(
            classify(&json!({"type": "activity", "activity_type": "tool_start", "detail": "read"})),
            KohakuEvent::Activity {
                activity_type: "tool_start".to_string(),
                value: json!({"type": "activity", "activity_type": "tool_start", "detail": "read"}),
            }
        );
    }

    #[test]
    fn classifies_turn_start() {
        assert_eq!(
            classify(&json!({"type": "turn_start", "agent": "x"})),
            KohakuEvent::TurnStart
        );
    }

    #[test]
    fn classifies_top_level_error() {
        assert_eq!(
            classify(&json!({"type": "error", "content": "fatal boom"})),
            KohakuEvent::Error("fatal boom".to_string())
        );
    }

    #[test]
    fn unknown_type_is_other() {
        assert_eq!(classify(&json!({"type": "mystery"})), KohakuEvent::Other);
    }
}
