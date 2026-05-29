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
    policy: &ExecutionPolicy,
    cwd: Option<&std::path::Path>,
    event_tx: &mpsc::Sender<ProviderEvent>,
    cancel: CancellationToken,
) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
    event_tx
        .send(ProviderEvent::turn_started(turn_id, "codex"))
        .await
        .ok();

    let mut args: Vec<String> = vec!["exec".to_string(), "--json".to_string()];
    args.extend(codex_policy_args(policy));
    for attachment in &input.attachments {
        args.push("--image".to_string());
        args.push(attachment.path.display().to_string());
    }
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
        let mut plain_text_fallback = String::new();
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
                if let Some(text) = extract_protocol_assistant_text(msg_type, &json) {
                    // Codex commonly emits streaming deltas followed by a
                    // completed agent_message item containing the same full
                    // text.  Keep the completed item as a fallback only when
                    // no deltas were seen so we do not duplicate responses.
                    if msg_type == "item.completed" {
                        if assistant_message.is_empty() {
                            assistant_message.push_str(&text);
                        }
                    } else {
                        assistant_message.push_str(&text);
                    }
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
                if !plain_text_fallback.is_empty() {
                    plain_text_fallback.push('\n');
                }
                plain_text_fallback.push_str(protocol_line);
                event_tx_clone
                    .send(ProviderEvent::text_message(turn_id, "codex", protocol_line))
                    .await
                    .ok();
            }
        }
        (assistant_message, plain_text_fallback, has_protocol_json)
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
    let (assistant_message, plain_text_fallback, has_protocol_json) = match consumer.await {
        Ok(output) => output,
        Err(err) => {
            let message = format!("codex output consumer failed: {err}");
            event_tx
                .send(ProviderEvent::turn_failed(turn_id, "codex", &message))
                .await
                .ok();
            return Err(ProviderError::ExecutionFailed(message));
        }
    };

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

    let response_text = if !assistant_message.is_empty() {
        assistant_message
    } else if !plain_text_fallback.trim().is_empty() {
        plain_text_fallback.trim().to_string()
    } else if !has_protocol_json {
        output.stdout.trim().to_string()
    } else {
        let message = "codex protocol completed without an assistant message".to_string();
        event_tx
            .send(ProviderEvent::turn_failed(turn_id, "codex", &message))
            .await
            .ok();
        return Err(ProviderError::ExecutionFailed(message));
    };

    Ok(build_turn_result(response_text, &output, "codex"))
}

fn extract_protocol_assistant_text(msg_type: &str, json: &serde_json::Value) -> Option<String> {
    match msg_type {
        "item.delta" => json.get("delta").and_then(|delta| {
            let delta_type = delta.get("type").and_then(|value| value.as_str());
            let looks_like_message = matches!(
                delta_type,
                Some(
                    "agent_message_delta"
                        | "assistant_message_delta"
                        | "output_text_delta"
                        | "message_delta"
                )
            );
            looks_like_message.then(|| {
                delta
                    .get("text")
                    .or_else(|| delta.get("content"))
                    .and_then(json_text_content)
            })?
        }),
        "item.completed" => json.get("item").and_then(|item| {
            let item_type = item.get("type").and_then(|value| value.as_str());
            let looks_like_message = matches!(
                item_type,
                Some("agent_message" | "assistant_message" | "assistant" | "message")
            );
            looks_like_message.then(|| {
                item.get("text")
                    .or_else(|| item.get("content"))
                    .and_then(json_text_content)
            })?
        }),
        "response.output_text.delta" | "response/output_text/delta" => {
            json.get("delta").and_then(json_text_content)
        }
        _ => None,
    }
}

fn json_text_content(value: &serde_json::Value) -> Option<String> {
    if let Some(text) = value.as_str().filter(|text| !text.is_empty()) {
        return Some(text.to_string());
    }
    if let Some(blocks) = value.as_array() {
        let joined = blocks
            .iter()
            .filter_map(|block| {
                block
                    .as_str()
                    .map(ToString::to_string)
                    .or_else(|| block.get("text").and_then(json_text_content))
                    .or_else(|| block.get("content").and_then(json_text_content))
            })
            .collect::<String>();
        return (!joined.is_empty()).then_some(joined);
    }
    value
        .get("text")
        .and_then(json_text_content)
        .or_else(|| value.get("content").and_then(json_text_content))
}

fn codex_policy_args(policy: &ExecutionPolicy) -> Vec<String> {
    let mut args = Vec::new();
    let mode = match policy.effective_sandbox_mode() {
        EffectiveSandboxMode::ReadOnly => "read-only",
        EffectiveSandboxMode::WorkspaceWrite => "workspace-write",
        EffectiveSandboxMode::DangerFullAccess => "danger-full-access",
    };
    args.push("--sandbox".to_string());
    args.push(mode.to_string());

    for path in policy.additional_allowed_paths() {
        args.push("--add-dir".to_string());
        args.push(path.display().to_string());
    }

    args
}

pub(crate) fn codex_runtime_args(model: Option<&str>, thinking_level: Option<&str>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(model) = clean_option(model) {
        // Use Codex's config override surface instead of `--model` so the
        // same mapped args work for both `codex exec` and `codex app-server`.
        // `app-server` intentionally does not expose a `--model` flag.
        args.push("-c".to_string());
        args.push(format!("model={}", toml_string_literal(model)));
    }
    if let Some(level) = normalize_reasoning_effort(thinking_level) {
        // Codex exposes reasoning effort through its config layer. Keep this
        // before user-provided extra args so an advanced user can override it
        // explicitly in `providers.<name>.args` when needed.
        args.push("-c".to_string());
        args.push(format!("model_reasoning_effort={level}"));
    }
    args
}

fn toml_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                let codepoint = ch as u32;
                if codepoint <= 0xFFFF {
                    out.push_str(&format!("\\u{codepoint:04X}"));
                } else {
                    out.push_str(&format!("\\U{codepoint:08X}"));
                }
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn clean_option(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn normalize_reasoning_effort(value: Option<&str>) -> Option<&'static str> {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("minimal" | "min") => Some("minimal"),
        Some("low" | "light") => Some("low"),
        Some("medium" | "normal" | "standard") => Some("medium"),
        Some("high" | "deep") => Some("high"),
        Some("none" | "auto" | "default" | "") | None => None,
        // Unknown labels are not forwarded; otherwise a typo in settings can
        // make every future Codex spawn fail.
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn codex_policy_args_map_effective_modes_and_extra_dirs() {
        let read_only = ExecutionPolicy::read_only("/repo");
        assert_eq!(
            codex_policy_args(&read_only),
            vec!["--sandbox", "read-only"]
        );

        let workspace =
            ExecutionPolicy::workspace_write("/repo").add_allowed_paths([PathBuf::from("/shared")]);
        assert_eq!(
            codex_policy_args(&workspace),
            vec!["--sandbox", "workspace-write", "--add-dir", "/shared"]
        );

        let danger = ExecutionPolicy::danger_full_access("/repo");
        assert_eq!(
            codex_policy_args(&danger),
            vec!["--sandbox", "danger-full-access"]
        );
    }

    #[test]
    fn codex_runtime_args_map_model_and_reasoning_effort() {
        assert_eq!(
            codex_runtime_args(Some("gpt-5-codex"), Some("high")),
            vec![
                "-c",
                "model=\"gpt-5-codex\"",
                "-c",
                "model_reasoning_effort=high"
            ]
        );
        assert_eq!(
            codex_runtime_args(Some("  "), Some("unknown")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn extracts_protocol_assistant_text_from_delta_and_completed_shapes() {
        let delta = serde_json::json!({
            "type": "item.delta",
            "delta": { "type": "agent_message_delta", "text": "hel" }
        });
        assert_eq!(
            extract_protocol_assistant_text("item.delta", &delta).as_deref(),
            Some("hel")
        );

        let completed = serde_json::json!({
            "type": "item.completed",
            "item": {
                "type": "assistant_message",
                "content": [{ "text": "hello" }, { "text": " world" }]
            }
        });
        assert_eq!(
            extract_protocol_assistant_text("item.completed", &completed).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn ignores_non_assistant_protocol_text() {
        let tool_delta = serde_json::json!({
            "type": "item.delta",
            "delta": { "type": "terminal_output_delta", "text": "tool stdout" }
        });
        assert_eq!(
            extract_protocol_assistant_text("item.delta", &tool_delta),
            None
        );
    }
}
