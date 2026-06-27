//! Parse the KohakuTerrarium headless JSONL stream.
//!
//! The `switchyard-headless` fork emits one JSON object per line on stdout:
//! `turn_start`, `text`, `activity` (with `activity_type` + nested
//! `metadata`), `turn_end` (`status`/`text`/`error`/`usage`), and `error`.
//! Every line is also passed through verbatim as an `ItemUpdated` event for
//! the diagnostics drawer; this module only classifies the lines that affect
//! the assistant text and the turn outcome.

use serde_json::Value;

#[derive(Debug, PartialEq, Eq)]
pub enum KohakuEvent {
    /// An assistant text delta (`{"type":"text","content":...}`).
    Text(String),
    /// The turn boundary (`{"type":"turn_end",...}`).
    TurnEnd {
        status: String,
        text: String,
        error: Option<String>,
    },
    /// activity / turn_start / error / anything else — passed through as-is.
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
            error: json.get("error").and_then(Value::as_str).map(str::to_string),
        },
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
    fn activity_and_turn_start_are_other() {
        assert_eq!(
            classify(&json!({"type": "activity", "activity_type": "tool_start"})),
            KohakuEvent::Other
        );
        assert_eq!(
            classify(&json!({"type": "turn_start", "agent": "x"})),
            KohakuEvent::Other
        );
    }
}
