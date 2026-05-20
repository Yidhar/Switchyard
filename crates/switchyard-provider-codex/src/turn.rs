//! Codex headless turn execution via `codex exec --json`.

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_provider_api::*;
use switchyard_provider_subprocess::{
    StreamingOutputLine, SubprocessConfig, build_subprocess_invocation_plan, build_turn_result,
    compose_prompt, emit_completion_event, handle_subprocess_error, run_subprocess_streaming_until,
};

#[allow(clippy::too_many_arguments)]
pub async fn run_codex_turn(
    turn_id: Uuid,
    original_command: &str,
    command: &str,
    extra_args: &[String],
    input: &TurnInput,
    timeout_secs: u64,
    env: Option<&std::collections::HashMap<String, String>>,
    cwd: Option<&std::path::Path>,
    event_tx: &mpsc::Sender<ProviderEvent>,
    cancel: CancellationToken,
) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
    event_tx
        .send(ProviderEvent::turn_started(turn_id, "codex"))
        .await
        .ok();

    let mut args: Vec<String> = vec!["exec".to_string(), "--json".to_string()];
    args.extend_from_slice(extra_args);
    args.push("-".to_string());
    let plan = build_subprocess_invocation_plan(original_command, command, &args);

    let prompt = compose_prompt(input);
    let config = SubprocessConfig {
        command: &plan.command,
        args: &plan.args,
        stdin_data: Some(&prompt),
        timeout_secs,
        cwd,
        pty_registry_key: Some(turn_id),
        prefer_pty: false,
        env,
    };

    let (line_tx, mut line_rx) = mpsc::channel::<StreamingOutputLine>(256);
    let protocol_done = CancellationToken::new();

    event_tx
        .send(ProviderEvent::execution_telemetry(
            turn_id,
            "codex",
            &plan.execution,
        ))
        .await
        .ok();

    let event_tx_clone = event_tx.clone();
    let protocol_done_consumer = protocol_done.clone();
    let consumer = tokio::spawn(async move {
        let mut assistant_message = String::new();
        let mut has_protocol_json = false;
        while let Some(output_line) = line_rx.recv().await {
            let line = output_line.text;
            let protocol_line = line.trim_end_matches(['\r', '\n']);
            event_tx_clone
                .send(ProviderEvent::terminal_output(
                    turn_id,
                    "codex",
                    &line,
                    Some("merged"),
                    Some(output_line.transport.as_str()),
                ))
                .await
                .ok();
            if protocol_line.is_empty() {
                continue;
            }
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(protocol_line) {
                has_protocol_json = true;
                let msg_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
                if msg_type == "turn.completed" {
                    protocol_done_consumer.cancel();
                    event_tx_clone
                        .send(ProviderEvent::turn_completed(turn_id, "codex"))
                        .await
                        .ok();
                    continue;
                }
                // {"type":"item.delta","delta":{"type":"agent_message_delta","text":"..."}}
                if msg_type == "item.delta"
                    && let Some(delta) = json.get("delta")
                    && delta.get("type").and_then(|t| t.as_str()) == Some("agent_message_delta")
                    && let Some(text) = delta.get("text").and_then(|t| t.as_str())
                {
                    assistant_message.push_str(text);
                }
                // {"type":"item.completed","item":{"type":"agent_message","text":"..."}}
                if msg_type == "item.completed"
                    && let Some(item) = json.get("item")
                    && item.get("type").and_then(|t| t.as_str()) == Some("agent_message")
                    && let Some(text) = item.get("text").and_then(|t| t.as_str())
                    && assistant_message.is_empty()
                {
                    assistant_message.push_str(text);
                }
                event_tx_clone
                    .send(ProviderEvent::new(
                        turn_id,
                        EventType::ItemUpdated,
                        "codex",
                        json,
                    ))
                    .await
                    .ok();
            } else if !line.trim().is_empty() {
                event_tx_clone
                    .send(ProviderEvent::text_message(turn_id, "codex", protocol_line))
                    .await
                    .ok();
            }
        }
        (assistant_message, has_protocol_json)
    });

    let result = run_subprocess_streaming_until(
        &config,
        &line_tx,
        cancel,
        Some(protocol_done.clone()),
        tokio::time::Duration::from_millis(250),
    )
    .await;
    drop(line_tx);
    let (assistant_message, has_protocol_json) = consumer.await.unwrap_or_default();

    let output = match result {
        Ok(o) => o,
        Err(e) => return Err(handle_subprocess_error(e, turn_id, "codex", event_tx).await),
    };

    // Fallback for older/non-conforming CLIs that don't emit protocol
    // turn.completed. When protocol completion was observed, the consumer
    // already emitted ProviderEvent::turn_completed at the correct boundary.
    if !protocol_done.is_cancelled() {
        emit_completion_event(&output, turn_id, "codex", event_tx).await;
    }

    let response_text = if assistant_message.is_empty() && !has_protocol_json {
        output.stdout.trim().to_string()
    } else {
        assistant_message
    };

    Ok(build_turn_result(response_text, &output, "codex"))
}
