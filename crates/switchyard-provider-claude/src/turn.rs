//! Claude headless turn execution via `claude -p --output-format stream-json
//! --include-partial-messages --verbose`.
//!
//! The `--include-partial-messages` flag is what makes the UI feel like a
//! real streaming chat: without it, Claude only emits the final consolidated
//! `assistant` message once the turn completes and the user sees
//! "Loading… → wall of text". With it, every `content_block_delta` arrives
//! incrementally, which we relay as [`ProviderEvent::text_message`] events
//! so the chat ticker can append tokens as they arrive — matching what the
//! persistent [`crate::live::ClaudeLiveInstance`] path already does.

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_provider_api::*;
use switchyard_provider_subprocess::{
    StreamingOutputLine, SubprocessConfig, build_subprocess_invocation_plan, build_turn_result,
    compose_prompt, emit_completion_event, handle_subprocess_error, run_subprocess_streaming,
};

use crate::stream_json::extract_delta_text;

#[allow(clippy::too_many_arguments)]
pub async fn run_claude_turn(
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
        .send(ProviderEvent::turn_started(turn_id, "claude"))
        .await
        .ok();

    // Claude requires --verbose for stream-json output format.
    // --include-partial-messages unlocks token-by-token deltas — without
    // it the CLI batches everything into a single trailing `assistant`
    // block and the chat ticker can't render progress.
    let mut args: Vec<String> = vec![
        "-p".to_string(),
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--include-partial-messages".to_string(),
        "--verbose".to_string(),
    ];
    args.extend(claude_policy_args(policy));
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
        // `assistant_message` is the buffered "best response we've seen so
        // far", surfaced as the final TurnResult.response_text. `streamed_any`
        // tracks whether we emitted at least one delta — used to decide
        // whether to honour later consolidated `assistant`/`result` payloads
        // (avoids double-counting their text).
        let mut assistant_message = String::new();
        let mut streamed_any = false;
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

                // 1. Streaming text deltas — emit a text_message so the chat
                //    ticker can render token-by-token. Handles both the
                //    wrapped `stream_event` shape (with --include-partial-messages)
                //    and the unwrapped form (without).
                if let Some(delta_text) = extract_delta_text(&json, msg_type)
                    && !delta_text.is_empty()
                {
                    assistant_message.push_str(delta_text);
                    streamed_any = true;
                    event_tx_clone
                        .send(ProviderEvent::text_message(turn_id, "claude", delta_text))
                        .await
                        .ok();
                }

                match msg_type {
                    // 2. Consolidated `assistant` block — Claude emits this
                    //    AFTER the deltas finish (with --include-partial-messages
                    //    on). If we already streamed the body, drop it on
                    //    the floor so we don't double-render. If deltas
                    //    didn't arrive (e.g. user disabled the flag via
                    //    extra_args), fall back to using the consolidated
                    //    text as the response body.
                    "assistant" if !streamed_any => {
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
                    // 3. `result` is the turn-boundary marker carrying usage
                    //    / cost / num_turns. Take its `result` field only
                    //    when nothing else gave us a response body (e.g.
                    //    a tool-only turn with no assistant text).
                    "result" => {
                        if let Some(text) = json.get("result").and_then(|r| r.as_str())
                            && assistant_message.is_empty()
                        {
                            assistant_message = text.to_string();
                        }
                    }
                    _ => {}
                }

                // 4. Pass the raw JSON through as ItemUpdated for the
                //    diagnostics drawer and any downstream observability.
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
                // Non-JSON line — surface as plain text so debugging is
                // possible when claude prints something unexpected.
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

fn claude_policy_args(policy: &ExecutionPolicy) -> Vec<String> {
    let mut args = Vec::new();
    match policy.effective_sandbox_mode() {
        EffectiveSandboxMode::ReadOnly => {
            args.push("--permission-mode".to_string());
            args.push("plan".to_string());
        }
        EffectiveSandboxMode::WorkspaceWrite => {
            args.push("--permission-mode".to_string());
            args.push("acceptEdits".to_string());
        }
        EffectiveSandboxMode::DangerFullAccess => {
            args.push("--dangerously-skip-permissions".to_string());
        }
    }

    for path in policy.additional_allowed_paths() {
        args.push("--add-dir".to_string());
        args.push(path.display().to_string());
    }

    args
}

pub(crate) fn claude_runtime_args(
    model: Option<&str>,
    thinking_level: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(model) = clean_option(model) {
        args.push("--model".to_string());
        args.push(model.to_string());
    }
    if let Some(effort) = normalize_claude_effort(thinking_level) {
        args.push("--effort".to_string());
        args.push(effort.to_string());
    }

    args
}

fn clean_option(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn normalize_claude_effort(value: Option<&str>) -> Option<&'static str> {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("minimal" | "min" | "low" | "light") => Some("low"),
        Some("medium" | "normal" | "standard") => Some("medium"),
        Some("high" | "deep") => Some("high"),
        Some("xhigh" | "x-high" | "extra-high" | "extra_high" | "very-high" | "very_high") => {
            Some("xhigh")
        }
        Some("max" | "maximum") => Some("max"),
        Some("none" | "auto" | "default" | "") | None => None,
        // Unknown labels are not forwarded; a typo should not break every
        // future Claude spawn. Users can still force exact release-specific
        // flags through providers.<name>.args.
        Some(_) => None,
    }
}

#[cfg(test)]
mod model_tests {
    use super::*;

    #[test]
    fn claude_runtime_args_map_model_and_effort() {
        assert_eq!(
            claude_runtime_args(Some("claude-sonnet-4-5"), Some("high")),
            vec!["--model", "claude-sonnet-4-5", "--effort", "high"]
        );
        assert_eq!(
            claude_runtime_args(Some(" "), Some("high")),
            vec!["--effort", "high"]
        );
        assert_eq!(
            claude_runtime_args(Some("sonnet"), Some("x-high")),
            vec!["--model", "sonnet", "--effort", "xhigh"]
        );
        assert_eq!(
            claude_runtime_args(None, Some("unknown")),
            Vec::<String>::new()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn claude_policy_args_map_modes_and_extra_dirs() {
        assert_eq!(
            claude_policy_args(&ExecutionPolicy::read_only("/repo")),
            vec!["--permission-mode", "plan"]
        );

        let workspace =
            ExecutionPolicy::workspace_write("/repo").add_allowed_paths([PathBuf::from("/shared")]);
        assert_eq!(
            claude_policy_args(&workspace),
            vec!["--permission-mode", "acceptEdits", "--add-dir", "/shared"]
        );

        assert_eq!(
            claude_policy_args(&ExecutionPolicy::danger_full_access("/repo")),
            vec!["--dangerously-skip-permissions"]
        );
    }
}
