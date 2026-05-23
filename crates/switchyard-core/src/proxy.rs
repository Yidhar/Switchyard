use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use switchyard_provider_api::{
    ArtifactBundle, ContextBundle, ExecutionPolicy, LiveInstanceRegistry, PersistentProvider,
    ProbeResult, Provider, ProviderError, ProviderEvent, TurnInput, TurnResult,
};

/// Adapts a [`Provider`] so that `start_turn` first looks for an idle live
/// instance in the registry, scoped to `session_id`. If one is found, the turn
/// is dispatched over it (preserving conversation context across turns); if
/// not, falls back to the wrapped provider's per-turn subprocess execution.
pub struct PersistentProviderProxy {
    provider_name: String,
    /// The Switchyard session this proxy serves. Used to scope pool lookups
    /// so two sessions can't accidentally share a live instance.
    session_id: Uuid,
    inner: Box<dyn Provider>,
    registry: Option<Arc<dyn LiveInstanceRegistry>>,
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
}

impl PersistentProviderProxy {
    pub fn new(
        provider_name: impl Into<String>,
        session_id: Uuid,
        inner: Box<dyn Provider>,
        registry: Option<Arc<dyn LiveInstanceRegistry>>,
    ) -> Self {
        Self {
            provider_name: provider_name.into(),
            session_id,
            inner,
            registry,
            results: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl Provider for PersistentProviderProxy {
    async fn probe(&self) -> Result<ProbeResult, ProviderError> {
        self.inner.probe().await
    }

    async fn start_turn(
        &self,
        turn_id: Uuid,
        input: TurnInput,
        policy: ExecutionPolicy,
        context: ContextBundle,
        event_tx: mpsc::Sender<ProviderEvent>,
        cancel: CancellationToken,
    ) -> Result<(), ProviderError> {
        if let Some(ref reg) = self.registry
            && let Some((instance_id, inst_lock)) =
                reg.checkout_any_idle(&self.provider_name, self.session_id)
        {
            let mut inst = inst_lock.lock().await;
            if let Err(e) = inst.update_context(context).await {
                reg.release(instance_id);
                return Err(ProviderError::ExecutionFailed(format!(
                    "Failed to sync context to persistent instance: {e}"
                )));
            }

            // Route through the full-turn, policy-aware variant so optional
            // attachments reach multimodal live providers and server-initiated
            // tool/file approval requests (Codex daemon) get gated by
            // `policy.write_access` and `policy.allowed_paths`. Live instances
            // without daemon-side approvals or image support fall through to
            // the trait default, which now sends only literal user text rather
            // than leaking local attachment paths into the prompt.
            let mut event_rx = match inst.send_turn_with_policy(&input, &policy).await {
                Ok(rx) => rx,
                Err(e) => {
                    reg.release(instance_id);
                    return Err(ProviderError::ExecutionFailed(format!(
                        "Failed to execute on persistent instance: {e}"
                    )));
                }
            };
            drop(inst); // Unlock early so other turns / health checks aren't blocked.

            let mut response_text = String::new();
            let mut failed = false;
            let provider_name = self.provider_name.clone();

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        failed = true;
                        break;
                    }
                    pe_opt = event_rx.recv() => {
                        if let Some(mut pe) = pe_opt {
                            pe.provider = provider_name.clone();
                            pe.turn_id = turn_id;
                            if pe.event_type == switchyard_provider_api::EventType::TurnFailed {
                                failed = true;
                            }
                            accumulate_response_text_from_event(&mut response_text, &pe.payload);
                            if event_tx.send(pe).await.is_err() {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
            }

            reg.release(instance_id);

            let mut metadata = HashMap::new();
            metadata.insert(
                "raw_stdout".to_string(),
                serde_json::Value::String(response_text.clone()),
            );

            let turn_result = TurnResult {
                response_text,
                exit_code: if failed { Some(1) } else { Some(0) },
                stderr: None,
                metadata,
            };
            let artifact_bundle = ArtifactBundle { artifacts: vec![] };

            self.results
                .lock()
                .await
                .insert(turn_id, (turn_result, artifact_bundle));
            Ok(())
        } else {
            self.inner
                .start_turn(turn_id, input, policy, context, event_tx, cancel)
                .await
        }
    }

    async fn finalize_turn(
        &self,
        turn_id: Uuid,
    ) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
        if let Some(res) = self.results.lock().await.remove(&turn_id) {
            Ok(res)
        } else {
            self.inner.finalize_turn(turn_id).await
        }
    }

    fn as_persistent(&self) -> Option<&dyn PersistentProvider> {
        self.inner.as_persistent()
    }
}

fn normalize_activity_kind(value: &str) -> String {
    value
        .trim()
        .replace(
            |c: char| c == '.' || c == '/' || c == '-' || c.is_whitespace(),
            "_",
        )
        .to_ascii_lowercase()
}

fn normalize_runtime_kind(value: &str) -> String {
    value
        .trim()
        .replace(
            |c: char| c == '/' || c == '_' || c == '-' || c.is_whitespace(),
            ".",
        )
        .to_ascii_lowercase()
}

fn item_type_from_loose_value(value: Option<&serde_json::Value>) -> Option<String> {
    let raw = value?.as_str()?.trim();
    if raw.is_empty() || raw.contains('.') || raw.contains('/') {
        return None;
    }
    let normalized = normalize_activity_kind(raw);
    (!normalized.is_empty()).then_some(normalized)
}

fn payload_item_type(payload: &serde_json::Value) -> Option<String> {
    [
        payload.get("item_type"),
        payload.get("item").and_then(|item| item.get("type")),
        payload
            .get("params")
            .and_then(|params| params.get("item_type")),
        payload
            .get("params")
            .and_then(|params| params.get("item"))
            .and_then(|item| item.get("type")),
    ]
    .into_iter()
    .flatten()
    .filter_map(|value| value.as_str())
    .map(normalize_activity_kind)
    .find(|item_type| !item_type.is_empty())
    .or_else(|| item_type_from_loose_value(payload.get("type")))
    .or_else(|| {
        item_type_from_loose_value(payload.get("params").and_then(|params| params.get("type")))
    })
}

fn is_non_assistant_activity_item(item_type: &str) -> bool {
    matches!(
        item_type,
        "tool_use"
            | "tool_call"
            | "function_call"
            | "custom_tool_call"
            | "mcp_tool_call"
            | "local_shell_call"
            | "tool_result"
            | "tool_response"
            | "function_call_output"
            | "custom_tool_call_output"
            | "mcp_tool_call_output"
            | "local_shell_call_output"
            | "command_execution"
            | "file_change"
            | "diff_ready"
            | "todo_list"
            | "approval_request"
            | "approval_decision"
            | "server_request"
            | "terminal_output"
            | "terminal_output_delta"
            | "tool_output_delta"
            | "command_output_delta"
            | "shell_output_delta"
            | "stdout_delta"
            | "stderr_delta"
            | "file_change_delta"
            | "diff_delta"
            | "patch_delta"
            | "execution_telemetry"
            | "reasoning"
    )
}

fn non_empty_payload_str(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

fn content_text(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = non_empty_payload_str(value) {
        return Some(text);
    }
    if let Some(blocks) = value.as_array() {
        let joined = blocks
            .iter()
            .filter_map(|block| {
                non_empty_payload_str(block)
                    .or_else(|| block.get("text").and_then(non_empty_payload_str))
                    .or_else(|| block.get("content").and_then(content_text))
            })
            .collect::<String>();
        return (!joined.is_empty()).then_some(joined);
    }
    if value.is_object() {
        return value
            .get("text")
            .and_then(non_empty_payload_str)
            .or_else(|| value.get("content").and_then(content_text));
    }
    None
}

fn runtime_protocol_kind(payload: &serde_json::Value) -> String {
    [
        payload.get("method"),
        payload
            .get("params")
            .and_then(|params| params.get("method")),
        payload.get("type"),
        payload.get("params").and_then(|params| params.get("type")),
    ]
    .into_iter()
    .flatten()
    .filter_map(|value| value.as_str())
    .map(normalize_runtime_kind)
    .find(|kind| !kind.is_empty())
    .unwrap_or_default()
}

fn protocol_has_text_hint(payload: &serde_json::Value) -> bool {
    let normalized = runtime_protocol_kind(payload);
    normalized.contains("agentmessage")
        || normalized.contains("agent.message")
        || normalized.contains("assistant")
        || normalized.contains("message.delta")
        || normalized.contains("content.delta")
        || normalized.contains("text.delta")
        || normalized.contains("output.text")
}

fn allows_plain_text_field(
    item_type: Option<&str>,
    text_protocol_hint: bool,
    has_protocol_kind: bool,
) -> bool {
    text_protocol_hint
        || !has_protocol_kind
        || matches!(item_type, Some("agent_message" | "assistant" | "message"))
}

fn is_textish_delta_kind(kind: &str) -> bool {
    let normalized = normalize_runtime_kind(kind);
    normalized.contains("agent.message")
        || normalized.contains("assistant")
        || normalized.contains("message.delta")
        || normalized.contains("content.block.delta")
        || normalized.contains("text.delta")
        || normalized == "text"
        || normalized == "output.text"
        || normalized.contains("output.text.delta")
        || normalized == "agent.message.delta"
}

fn delta_text(value: &serde_json::Value, inherited_text_hint: bool) -> Option<String> {
    if let Some(text) = non_empty_payload_str(value) {
        return inherited_text_hint.then_some(text);
    }

    let kind = value.get("type").and_then(|value| value.as_str());
    let kind_is_textish = kind.map(is_textish_delta_kind).unwrap_or(false);
    if kind.is_some() && !kind_is_textish {
        return None;
    }
    if !inherited_text_hint && !kind_is_textish {
        return None;
    }

    let nested_text_hint = inherited_text_hint || kind_is_textish;
    value
        .get("text")
        .and_then(non_empty_payload_str)
        .or_else(|| value.get("content").and_then(content_text))
        .or_else(|| {
            value
                .get("delta")
                .and_then(|delta| delta_text(delta, nested_text_hint))
        })
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(content_text)
        })
}

fn accumulate_response_text_from_event(response_text: &mut String, payload: &serde_json::Value) {
    accumulate_response_text_from_event_with_hint(response_text, payload, false);
}

fn accumulate_response_text_from_event_with_hint(
    response_text: &mut String,
    payload: &serde_json::Value,
    inherited_text_hint: bool,
) {
    let item_type = payload_item_type(payload);

    if let Some(item_type) = item_type.as_deref()
        && is_non_assistant_activity_item(item_type)
    {
        return;
    }

    let protocol_kind = runtime_protocol_kind(payload);
    let text_protocol_hint = inherited_text_hint || protocol_has_text_hint(payload);
    let plain_text_allowed = allows_plain_text_field(
        item_type.as_deref(),
        text_protocol_hint,
        !protocol_kind.is_empty(),
    );

    // Plain Switchyard text_message events are deltas emitted by live
    // instances (Codex item/agentMessage/delta, Claude content_block_delta, or
    // plain subprocess lines). Append them.
    if let Some(text) = payload.get("text").and_then(|value| value.as_str()) {
        if plain_text_allowed {
            response_text.push_str(text);
        }
        return;
    }

    // Raw protocol delta payloads should also append. This matters for live
    // adapters that forward native provider JSON instead of normalizing to
    // text_message first.
    if let Some(delta) = payload.get("delta") {
        if let Some(text) = delta_text(delta, text_protocol_hint) {
            response_text.push_str(&text);
            return;
        }
    }

    // Be tolerant of raw JSON-RPC notifications (`{ method, params: ... }`).
    // Most adapters flatten `params` before emitting ProviderEvent, but live
    // app-server providers may occasionally pass through an envelope. Recurse
    // into `params` so final `provider_response` does not remain empty.
    if let Some(params) = payload.get("params") {
        let before = response_text.clone();
        accumulate_response_text_from_event_with_hint(response_text, params, text_protocol_hint);
        if *response_text != before {
            return;
        }
    }

    // Gemini stream-json uses {type:"message", role:"assistant",
    // content:"...", delta:true} for increments and delta:false/absent for a
    // consolidated replacement.
    if plain_text_allowed && let Some(text) = payload.get("content").and_then(content_text) {
        if payload
            .get("delta")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            response_text.push_str(&text);
        } else {
            *response_text = text.to_string();
        }
        return;
    }

    // Codex app-server completion payloads carry the final assistant body as
    // item.text. Replace any previously streamed deltas with the canonical
    // final body instead of leaving persistent turns empty.
    if let Some(text) = payload
        .get("item")
        .and_then(|item| item.get("text"))
        .and_then(|value| value.as_str())
    {
        *response_text = text.to_string();
        return;
    }
    if let Some(text) = payload
        .get("item")
        .and_then(|item| item.get("content"))
        .and_then(content_text)
    {
        *response_text = text;
        return;
    }
    if let Some(text) = payload
        .get("item")
        .and_then(|item| item.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(content_text)
    {
        *response_text = text;
        return;
    }

    // Claude's result can be a final consolidated body. Replace instead of
    // append so deltas + final result do not duplicate.
    if let Some(text) = payload.get("result").and_then(|value| value.as_str()) {
        *response_text = text.to_string();
        return;
    }

    // Claude consolidated assistant message content. Persistent Claude usually
    // drops these after streaming deltas, but keep the proxy robust for
    // providers that pass the shape through.
    if let Some(joined) = payload
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(content_text)
    {
        *response_text = joined;
    }
}

#[cfg(test)]
mod tests {
    use super::accumulate_response_text_from_event;
    use serde_json::json;

    #[test]
    fn accumulates_plain_text_deltas() {
        let mut text = String::new();

        accumulate_response_text_from_event(&mut text, &json!({ "text": "hel" }));
        accumulate_response_text_from_event(&mut text, &json!({ "text": "lo" }));

        assert_eq!(text, "hello");
    }

    #[test]
    fn codex_item_completed_replaces_streamed_delta_body() {
        let mut text = String::new();

        accumulate_response_text_from_event(&mut text, &json!({ "text": "partial" }));
        accumulate_response_text_from_event(
            &mut text,
            &json!({
                "type": "item.completed",
                "item": { "type": "agent_message", "text": "final body" }
            }),
        );

        assert_eq!(text, "final body");
    }

    #[test]
    fn json_rpc_params_item_text_replaces_streamed_delta_body() {
        let mut text = String::new();

        accumulate_response_text_from_event(&mut text, &json!({ "text": "partial" }));
        accumulate_response_text_from_event(
            &mut text,
            &json!({
                "method": "item/completed",
                "params": {
                    "type": "item.completed",
                    "item": { "type": "agent_message", "text": "final from params" }
                }
            }),
        );

        assert_eq!(text, "final from params");
    }

    #[test]
    fn gemini_delta_appends_and_full_content_replaces() {
        let mut text = String::new();

        accumulate_response_text_from_event(
            &mut text,
            &json!({ "type": "message", "role": "assistant", "content": "he", "delta": true }),
        );
        accumulate_response_text_from_event(
            &mut text,
            &json!({ "type": "message", "role": "assistant", "content": "hello", "delta": false }),
        );

        assert_eq!(text, "hello");
    }

    #[test]
    fn claude_result_replaces_instead_of_duplicating_streamed_deltas() {
        let mut text = String::new();

        accumulate_response_text_from_event(&mut text, &json!({ "text": "hello" }));
        accumulate_response_text_from_event(
            &mut text,
            &json!({ "type": "result", "result": "hello" }),
        );

        assert_eq!(text, "hello");
    }

    #[test]
    fn claude_message_content_replaces_when_no_deltas_seen() {
        let mut text = String::new();

        accumulate_response_text_from_event(
            &mut text,
            &json!({
                "type": "assistant",
                "message": {
                    "content": [
                        { "type": "text", "text": "hello " },
                        { "type": "tool_use", "name": "ignored" },
                        { "type": "text", "text": "world" }
                    ]
                }
            }),
        );

        assert_eq!(text, "hello world");
    }

    #[test]
    fn codex_delta_string_and_nested_content_blocks_accumulate() {
        let mut text = String::new();

        accumulate_response_text_from_event(
            &mut text,
            &json!({
                "method": "item/agentMessage/delta",
                "params": { "delta": "hel" }
            }),
        );
        accumulate_response_text_from_event(
            &mut text,
            &json!({
                "type": "item.delta",
                "delta": { "type": "agent_message_delta", "text": "lo" }
            }),
        );

        assert_eq!(text, "hello");

        accumulate_response_text_from_event(
            &mut text,
            &json!({
                "type": "item.completed",
                "item": {
                    "type": "agent_message",
                    "content": [
                        { "type": "text", "text": "final " },
                        { "type": "text", "text": "body" }
                    ]
                }
            }),
        );

        assert_eq!(text, "final body");
    }

    #[test]
    fn tool_result_content_does_not_replace_assistant_response() {
        let mut text = String::from("assistant body");

        accumulate_response_text_from_event(
            &mut text,
            &json!({
                "type": "item.completed",
                "item": {
                    "type": "tool_result",
                    "content": "stdout that should stay in the tool card"
                }
            }),
        );

        assert_eq!(text, "assistant body");
    }

    #[test]
    fn non_text_item_delta_does_not_append_to_response() {
        let mut text = String::from("assistant body");

        accumulate_response_text_from_event(
            &mut text,
            &json!({
                "type": "item.delta",
                "delta": {
                    "type": "terminal_output_delta",
                    "text": "PWD:\nE:\\Switchyard\nROOT:\n..."
                }
            }),
        );
        accumulate_response_text_from_event(
            &mut text,
            &json!({
                "type": "item.delta",
                "delta": "large tool stdout should not become assistant text"
            }),
        );
        accumulate_response_text_from_event(
            &mut text,
            &json!({
                "type": "terminal_output_delta",
                "text": "CODEX (CORE)\n\nPWD:\nE:\\Switchyard\nROOT:\n..."
            }),
        );

        assert_eq!(text, "assistant body");
    }

    #[test]
    fn file_edit_payload_does_not_replace_response() {
        let mut text = String::from("assistant body");

        accumulate_response_text_from_event(
            &mut text,
            &json!({
                "type": "item.updated",
                "item": {
                    "type": "file_change",
                    "path": "src/main.rs",
                    "diff": "--- a/src/main.rs\n+++ b/src/main.rs\n@@\n-old\n+new"
                }
            }),
        );

        assert_eq!(text, "assistant body");
    }
}
