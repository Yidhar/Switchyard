//! Claude headless turn execution via `claude -p --output-format stream-json`.

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_provider_api::*;
use switchyard_provider_subprocess::{
    StreamingOutputLine, SubprocessConfig, build_subprocess_invocation_plan, build_turn_result,
    compose_prompt, emit_completion_event, handle_subprocess_error, run_subprocess_streaming,
};

#[allow(clippy::too_many_arguments)]
pub async fn run_claude_turn(
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
        .send(ProviderEvent::turn_started(turn_id, "claude"))
        .await
        .ok();

    // Claude requires --verbose for stream-json output format
    let mut args: Vec<String> = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
    ];
    args.extend_from_slice(extra_args);
    args.push(compose_prompt(input));
    let plan = build_subprocess_invocation_plan(original_command, command, &args);

    let config = SubprocessConfig {
        command: &plan.command,
        args: &plan.args,
        stdin_data: None,
        timeout_secs,
        cwd,
        pty_registry_key: Some(turn_id),
        prefer_pty: false,
        env,
    };

    let (line_tx, mut line_rx) = mpsc::channel::<StreamingOutputLine>(256);

    event_tx
        .send(ProviderEvent::execution_telemetry(
            turn_id,
            "claude",
            &plan.execution,
        ))
        .await
        .ok();

    let event_tx_clone = event_tx.clone();
    let consumer = tokio::spawn(async move {
        let mut assistant_message = String::new();
        while let Some(output_line) = line_rx.recv().await {
            let line = output_line.text;
            let protocol_line = line.trim_end_matches(['\r', '\n']);
            event_tx_clone
                .send(ProviderEvent::terminal_output(
                    turn_id,
                    "claude",
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
                let msg_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match msg_type {
                    "content_block_delta" => {
                        if let Some(delta) = json.get("delta")
                            && delta.get("type").and_then(|t| t.as_str()) == Some("text_delta")
                            && let Some(text) = delta.get("text").and_then(|t| t.as_str())
                        {
                            assistant_message.push_str(text);
                        }
                    }
                    // {"type":"result","result":"hello switchyard",...}
                    "result" => {
                        if let Some(text) = json.get("result").and_then(|r| r.as_str())
                            && assistant_message.is_empty()
                        {
                            assistant_message = text.to_string();
                        }
                    }
                    // {"type":"assistant","message":{"content":[{"type":"text","text":"..."}]}}
                    "assistant" => {
                        if let Some(content) = json
                            .get("message")
                            .and_then(|m| m.get("content"))
                            .and_then(|c| c.as_array())
                        {
                            for block in content {
                                if block.get("type").and_then(|t| t.as_str()) == Some("text")
                                    && let Some(text) = block.get("text").and_then(|t| t.as_str())
                                    && assistant_message.is_empty()
                                {
                                    assistant_message = text.to_string();
                                }
                            }
                        }
                    }
                    _ => {}
                }
                event_tx_clone
                    .send(ProviderEvent::new(
                        turn_id,
                        EventType::ItemUpdated,
                        "claude",
                        json,
                    ))
                    .await
                    .ok();
            } else if !protocol_line.trim().is_empty() {
                event_tx_clone
                    .send(ProviderEvent::text_message(
                        turn_id,
                        "claude",
                        protocol_line,
                    ))
                    .await
                    .ok();
            }
        }
        assistant_message
    });

    let result = run_subprocess_streaming(&config, &line_tx, cancel).await;
    drop(line_tx);
    let assistant_message = consumer.await.unwrap_or_default();

    let output = match result {
        Ok(o) => o,
        Err(e) => return Err(handle_subprocess_error(e, turn_id, "claude", event_tx).await),
    };

    emit_completion_event(&output, turn_id, "claude", event_tx).await;

    let response_text = if assistant_message.is_empty() {
        output.stdout.trim().to_string()
    } else {
        assistant_message
    };

    Ok(build_turn_result(response_text, &output, "claude"))
}
