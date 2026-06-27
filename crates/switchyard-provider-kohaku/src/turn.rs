//! KohakuTerrarium headless turn execution via
//! `kt run <creature> --headless --json -p <prompt>`.
//!
//! `--json` makes `kt` emit one JSON object per line on stdout (logs go to
//! stderr), which we relay as [`ProviderEvent`]s so the chat ticker renders
//! token-by-token. The process exits 0 when the turn ends `ok`, non-zero
//! otherwise — which [`build_turn_result`] folds into the turn outcome.

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_provider_api::*;
use switchyard_provider_subprocess::{
    StreamingOutputLine, SubprocessConfig, build_subprocess_invocation_plan, build_turn_result,
    compose_prompt, emit_completion_event, handle_subprocess_error, run_subprocess_streaming,
};

use crate::jsonl::{KohakuEvent, classify};

#[allow(clippy::too_many_arguments)]
pub async fn run_kohaku_turn(
    turn_id: Uuid,
    original_command: &str,
    command: &str,
    configured_args: &[String],
    model: Option<&str>,
    thinking_level: Option<&str>,
    input: &TurnInput,
    timeout_secs: u64,
    env: Option<&std::collections::HashMap<String, String>>,
    policy: &ExecutionPolicy,
    cwd: Option<&std::path::Path>,
    event_tx: &mpsc::Sender<ProviderEvent>,
    cancel: CancellationToken,
) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
    event_tx
        .send(ProviderEvent::turn_started(turn_id, "kohaku"))
        .await
        .ok();

    // A real KohakuTerrarium turn needs a creature: `kt run <creature> ...`.
    // The first configured arg is the required `agent_path` positional. Without
    // it the spawn would either fail on argparse or (with no creature) never do
    // a real model call — fail loudly with actionable guidance instead.
    let creature = configured_args.first().map(|s| s.trim()).unwrap_or("");
    if creature.is_empty() || creature.starts_with('-') {
        let err = ProviderError::ExecutionFailed(
            "kohaku: no creature configured. Set the provider's first CLI argument (args[0]) to a \
             creature ref — a config-folder path or @pkg/creatures/<name>."
                .to_string(),
        );
        event_tx
            .send(ProviderEvent::turn_failed(
                turn_id,
                "kohaku",
                err.to_string(),
            ))
            .await
            .ok();
        return Err(err);
    }

    // argv: kt run <creature [+user extras]> --headless --json
    //          [--llm <sel>] --sandbox <preset> --cwd <dir> --no-subagents
    //          -p <prompt>
    // The creature ref must lead so it binds to the required `agent_path`
    // positional.
    let mut args: Vec<String> = vec!["run".to_string()];
    args.extend_from_slice(configured_args);
    args.push("--headless".to_string());
    args.push("--json".to_string());
    args.extend(kohaku_runtime_args(model, thinking_level));
    args.extend(kohaku_policy_args(policy));
    // Leaf execution: a routed/peer run must not spawn sub-agents.
    args.push("--no-subagents".to_string());
    args.push("-p".to_string());
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
            "kohaku",
            &plan.execution,
        ))
        .await
        .ok();

    let event_tx_clone = event_tx.clone();
    let consumer = tokio::spawn(async move {
        // `assistant_message` accumulates streamed `text` deltas (the primary
        // response body). `final_text` is the consolidated `turn_end.text`,
        // used only when no deltas arrived (e.g. a tool-only turn).
        let mut assistant_message = String::new();
        let mut final_text: Option<String> = None;
        while let Some(output_line) = line_rx.recv().await {
            let line = output_line.text;
            let protocol_line = line.trim_end_matches(['\r', '\n']);
            event_tx_clone
                .send(ProviderEvent::terminal_output(
                    turn_id,
                    "kohaku",
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
                match classify(&json) {
                    KohakuEvent::Text(text) if !text.is_empty() => {
                        assistant_message.push_str(&text);
                        event_tx_clone
                            .send(ProviderEvent::text_message(turn_id, "kohaku", &text))
                            .await
                            .ok();
                    }
                    KohakuEvent::TurnEnd { text, .. } => {
                        if final_text.is_none() && !text.is_empty() {
                            final_text = Some(text);
                        }
                    }
                    _ => {}
                }

                // Pass the raw JSON through for the diagnostics drawer and any
                // downstream observability (tool/subagent activity, usage).
                event_tx_clone
                    .send(ProviderEvent::new(
                        turn_id,
                        EventType::ItemUpdated,
                        "kohaku",
                        json,
                    ))
                    .await
                    .ok();
            } else if !protocol_line.trim().is_empty() {
                // Non-JSON line — surface as text so unexpected output is
                // visible when debugging.
                event_tx_clone
                    .send(ProviderEvent::text_message(
                        turn_id,
                        "kohaku",
                        protocol_line,
                    ))
                    .await
                    .ok();
            }
        }
        if assistant_message.is_empty() {
            final_text.unwrap_or_default()
        } else {
            assistant_message
        }
    });

    let result = run_subprocess_streaming(&config, &line_tx, cancel).await;
    drop(line_tx);
    let streamed_text = consumer.await.unwrap_or_default();

    let output = match result {
        Ok(o) => o,
        Err(e) => return Err(handle_subprocess_error(e, turn_id, "kohaku", event_tx).await),
    };

    emit_completion_event(&output, turn_id, "kohaku", event_tx).await;

    let response_text = if streamed_text.is_empty() {
        output.stdout.trim().to_string()
    } else {
        streamed_text
    };

    Ok(build_turn_result(response_text, &output, "kohaku"))
}

/// Map Switchyard's sandbox mode + workspace onto `kt` headless flags.
fn kohaku_policy_args(policy: &ExecutionPolicy) -> Vec<String> {
    let preset = match policy.effective_sandbox_mode() {
        // KT's sandbox plugin is a process-internal intent gate (not an OS
        // boundary); Switchyard still enforces its own OS-level sandbox.
        EffectiveSandboxMode::ReadOnly => "READ_ONLY",
        EffectiveSandboxMode::WorkspaceWrite => "WORKSPACE",
        // "danger" = unrestricted; do not load KT's sandbox plugin.
        EffectiveSandboxMode::DangerFullAccess => "off",
    };
    let mut args = vec!["--sandbox".to_string(), preset.to_string()];
    // KT's workspace root + tool cwd derive from the Agent pwd.
    args.push("--cwd".to_string());
    args.push(policy.cwd.display().to_string());
    // NOTE: KT has no multi-root writable allowlist, so
    // `policy.additional_allowed_paths()` is not mapped in v1.
    args
}

/// Map the configured model onto the `kt --llm <selector>` profile.
///
/// KT has no standalone effort flag — reasoning is encoded in the selector's
/// `@reasoning=` variation — so `thinking_level` is not appended in v1 (a
/// user wanting it can include it directly in `model`, e.g.
/// `enzi/gpt-5.5-custom@reasoning=low`).
pub(crate) fn kohaku_runtime_args(
    model: Option<&str>,
    _thinking_level: Option<&str>,
) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
        args.push("--llm".to_string());
        args.push(model.to_string());
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn runtime_args_map_model_only() {
        assert_eq!(
            kohaku_runtime_args(Some("enzi/gpt-5.5-custom"), Some("high")),
            vec!["--llm", "enzi/gpt-5.5-custom"]
        );
        assert_eq!(kohaku_runtime_args(Some("  "), None), Vec::<String>::new());
        assert_eq!(kohaku_runtime_args(None, Some("low")), Vec::<String>::new());
    }

    #[test]
    fn policy_args_map_sandbox_modes() {
        let ro = kohaku_policy_args(&ExecutionPolicy::read_only("/repo"));
        assert_eq!(ro[0..2], ["--sandbox", "READ_ONLY"]);
        assert_eq!(ro[2], "--cwd");
        assert_eq!(ro[3], PathBuf::from("/repo").display().to_string());

        assert_eq!(
            kohaku_policy_args(&ExecutionPolicy::workspace_write("/repo"))[0..2],
            ["--sandbox", "WORKSPACE"]
        );
        assert_eq!(
            kohaku_policy_args(&ExecutionPolicy::danger_full_access("/repo"))[0..2],
            ["--sandbox", "off"]
        );
    }
}
