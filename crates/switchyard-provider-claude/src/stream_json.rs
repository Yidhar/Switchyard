//! Shared parsing helpers for Claude's `--output-format stream-json` wire
//! protocol. Used by both the persistent IO path ([`crate::live`]) and the
//! per-turn `claude -p` path ([`crate::turn`]).
//!
//! Claude emits two flavours of text delta on the wire depending on whether
//! `--include-partial-messages` is set:
//!
//! - **Wrapped** (flag on): `{"type":"stream_event","event":{"type":
//!   "content_block_delta","delta":{"type":"text_delta","text":"…"}}}`
//! - **Unwrapped** (flag off): `{"type":"content_block_delta","delta":{
//!   "type":"text_delta","text":"…"}}`
//!
//! [`extract_delta_text`] returns the inner `text` for either shape so
//! downstream emitters can treat both invocations identically.

/// Pull the text payload out of a stream-json delta, whether wrapped in a
/// `stream_event` envelope or arriving at top level. Returns `None` for
/// non-text deltas (e.g. `input_json_delta` for tool calls) or any other
/// event type.
pub(crate) fn extract_delta_text<'a>(
    json: &'a serde_json::Value,
    msg_type: &str,
) -> Option<&'a str> {
    match msg_type {
        "stream_event" => {
            let event = json.get("event")?;
            if event.get("type").and_then(|t| t.as_str()) != Some("content_block_delta") {
                return None;
            }
            text_delta_payload(event.get("delta")?)
        }
        "content_block_delta" => text_delta_payload(json.get("delta")?),
        _ => None,
    }
}

fn text_delta_payload(delta: &serde_json::Value) -> Option<&str> {
    if delta.get("type").and_then(|t| t.as_str()) == Some("text_delta") {
        delta.get("text").and_then(|t| t.as_str())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn wrapped_content_block_delta_text() {
        let payload = json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "index": 0,
                "delta": { "type": "text_delta", "text": "hello" }
            }
        });
        assert_eq!(extract_delta_text(&payload, "stream_event"), Some("hello"));
    }

    #[test]
    fn unwrapped_content_block_delta_text() {
        let payload = json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": "world" }
        });
        assert_eq!(
            extract_delta_text(&payload, "content_block_delta"),
            Some("world")
        );
    }

    #[test]
    fn non_delta_stream_events_ignored() {
        let payload = json!({
            "type": "stream_event",
            "event": { "type": "message_start" }
        });
        assert_eq!(extract_delta_text(&payload, "stream_event"), None);
    }

    #[test]
    fn non_text_deltas_ignored() {
        // input_json_delta arrives for streaming tool-call arguments; we
        // don't want to render those as user-visible text.
        let payload = json!({
            "type": "stream_event",
            "event": {
                "type": "content_block_delta",
                "delta": { "type": "input_json_delta", "partial_json": "{" }
            }
        });
        assert_eq!(extract_delta_text(&payload, "stream_event"), None);
    }
}
