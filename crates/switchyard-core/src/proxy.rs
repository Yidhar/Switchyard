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
            // the trait default, which appends local file references to text.
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

fn accumulate_response_text_from_event(response_text: &mut String, payload: &serde_json::Value) {
    // Plain Switchyard text_message events are deltas emitted by live
    // instances (Codex item/agentMessage/delta, Claude content_block_delta, or
    // plain subprocess lines). Append them.
    if let Some(text) = payload.get("text").and_then(|value| value.as_str()) {
        response_text.push_str(text);
        return;
    }

    // Raw protocol delta payloads should also append. This matters for live
    // adapters that forward native provider JSON instead of normalizing to
    // text_message first.
    if let Some(delta) = payload.get("delta") {
        if let Some(text) = delta.get("text").and_then(|value| value.as_str()) {
            response_text.push_str(text);
            return;
        }
        if let Some(text) = delta
            .get("delta")
            .and_then(|inner| inner.get("text"))
            .and_then(|value| value.as_str())
        {
            response_text.push_str(text);
            return;
        }
    }

    // Be tolerant of raw JSON-RPC notifications (`{ method, params: ... }`).
    // Most adapters flatten `params` before emitting ProviderEvent, but live
    // app-server providers may occasionally pass through an envelope. Recurse
    // into `params` so final `provider_response` does not remain empty.
    if let Some(params) = payload.get("params") {
        let before = response_text.clone();
        accumulate_response_text_from_event(response_text, params);
        if *response_text != before {
            return;
        }
    }

    // Gemini stream-json uses {type:"message", role:"assistant",
    // content:"...", delta:true} for increments and delta:false/absent for a
    // consolidated replacement.
    if let Some(text) = payload.get("content").and_then(|value| value.as_str()) {
        if payload
            .get("delta")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            response_text.push_str(text);
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

    // Claude's result can be a final consolidated body. Replace instead of
    // append so deltas + final result do not duplicate.
    if let Some(text) = payload.get("result").and_then(|value| value.as_str()) {
        *response_text = text.to_string();
        return;
    }

    // Claude consolidated assistant message content. Persistent Claude usually
    // drops these after streaming deltas, but keep the proxy robust for
    // providers that pass the shape through.
    if let Some(blocks) = payload
        .get("message")
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_array())
    {
        let joined = blocks
            .iter()
            .filter(|block| block.get("type").and_then(|value| value.as_str()) == Some("text"))
            .filter_map(|block| block.get("text").and_then(|value| value.as_str()))
            .collect::<String>();
        if !joined.is_empty() {
            *response_text = joined;
        }
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
}
