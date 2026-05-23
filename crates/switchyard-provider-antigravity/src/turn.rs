//! Antigravity headless turn execution via `agy -p "<prompt>"`.
//!
//! Antigravity has **no** `--output-format`, **no** `stream-json`, **no**
//! `--acp`. Output is plain text to stdout, one shot, exits when the model
//! response is complete (or the agent decides to stop). Standard-output
//! line streaming is still useful for the UI's live ticker — each line is
//! surfaced as both a `terminal_output` event and a `text_message`
//! ProviderEvent so accumulators downstream see incremental progress.
//!
//! ## Why prompt-via-argv (not stdin)
//!
//! Unlike the Gemini wrapper, Antigravity is a native Go binary (`agy.exe`
//! on Windows), not a `.cmd` shim. There is no batch-file argument-parser
//! to confuse with multi-line prompts. We pass the prompt directly as the
//! `-p` value, letting Tokio's Command escape it for the target OS. This
//! keeps the invocation symmetrical with what a user types at the shell.

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_provider_api::*;
use switchyard_provider_subprocess::{
    StreamingOutputLine, SubprocessConfig, build_subprocess_invocation_plan, build_turn_result,
    compose_prompt, emit_completion_event, handle_subprocess_error, run_subprocess_streaming,
};

/// Build the args for an `agy -p "<prompt>"` invocation.
///
/// Returns `(args, prompt_in_argv_already)` — Antigravity gets the prompt
/// in argv directly, so no stdin payload is needed.
pub(crate) fn build_antigravity_invocation(
    extra_args: &[String],
    input: &TurnInput,
    policy: &ExecutionPolicy,
) -> Vec<String> {
    let prompt = compose_prompt(input);
    let mut args: Vec<String> = vec!["-p".to_string(), prompt];
    args.extend(antigravity_policy_args(policy));
    args.extend_from_slice(extra_args);
    args
}

fn antigravity_policy_args(policy: &ExecutionPolicy) -> Vec<String> {
    let mut args = Vec::new();
    match policy.effective_sandbox_mode() {
        EffectiveSandboxMode::ReadOnly | EffectiveSandboxMode::WorkspaceWrite => {
            args.push("--sandbox".to_string());
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

#[allow(clippy::too_many_arguments)]
pub async fn run_antigravity_turn(
    turn_id: Uuid,
    original_command: &str,
    command: &str,
    extra_args: &[String],
    input: &TurnInput,
    policy: &ExecutionPolicy,
    timeout_secs: u64,
    env: Option<&std::collections::HashMap<String, String>>,
    cwd: Option<&std::path::Path>,
    event_tx: &mpsc::Sender<ProviderEvent>,
    cancel: CancellationToken,
) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
    event_tx
        .send(ProviderEvent::turn_started(turn_id, "antigravity"))
        .await
        .ok();

    let args = build_antigravity_invocation(extra_args, input, policy);
    let plan = build_subprocess_invocation_plan(original_command, command, &args);

    let config = SubprocessConfig {
        command: &plan.command,
        args: &plan.args,
        stdin_data: None, // prompt is in argv
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
            "antigravity",
            &plan.execution,
        ))
        .await
        .ok();

    let event_tx_clone = event_tx.clone();
    let consumer = tokio::spawn(async move {
        let mut assistant_message = String::new();
        while let Some(output_line) = line_rx.recv().await {
            let line = output_line.text;
            let trimmed = line.trim_end_matches(['\r', '\n']);
            // Always emit the raw line as terminal output for the
            // diagnostics drawer.
            event_tx_clone
                .send(ProviderEvent::terminal_output(
                    turn_id,
                    "antigravity",
                    &line,
                    Some("merged"),
                    Some(output_line.transport.as_str()),
                ))
                .await
                .ok();
            if trimmed.is_empty() {
                continue;
            }
            // Plain text — surface each line as a text_message so the chat
            // bubble can accumulate progressively.
            event_tx_clone
                .send(ProviderEvent::text_message(
                    turn_id,
                    "antigravity",
                    &format!("{trimmed}\n"),
                ))
                .await
                .ok();
            assistant_message.push_str(trimmed);
            assistant_message.push('\n');
        }
        assistant_message
    });

    let result = run_subprocess_streaming(&config, &line_tx, cancel).await;
    drop(line_tx);
    let assistant_message = consumer.await.unwrap_or_default();

    let output = match result {
        Ok(o) => o,
        Err(e) => return Err(handle_subprocess_error(e, turn_id, "antigravity", event_tx).await),
    };

    emit_completion_event(&output, turn_id, "antigravity", event_tx).await;

    let response_text = if assistant_message.trim().is_empty() {
        output.stdout.trim().to_string()
    } else {
        assistant_message.trim_end().to_string()
    };

    Ok(build_turn_result(response_text, &output, "antigravity"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn antigravity_invocation_passes_prompt_in_argv() {
        let input = TurnInput {
            user_message: "Say hello".to_string(),
            system_prompt: None,
            attachments: Vec::new(),
        };
        let policy = ExecutionPolicy::workspace_write(".");
        let args = build_antigravity_invocation(&[], &input, &policy);

        assert_eq!(args[0], "-p");
        // The prompt lands directly in argv (no stdin redirection needed
        // since agy is a native exe, not a .cmd wrapper).
        assert!(args[1].contains("Say hello"));
    }

    #[test]
    fn antigravity_invocation_folds_system_prompt_into_prompt() {
        let input = TurnInput {
            user_message: "Refactor the parser".to_string(),
            system_prompt: Some("You are reviewing code".to_string()),
            attachments: Vec::new(),
        };
        let policy = ExecutionPolicy::workspace_write(".");
        let args = build_antigravity_invocation(&[], &input, &policy);
        assert_eq!(args[0], "-p");
        assert!(args[1].contains("You are reviewing code"));
        assert!(args[1].contains("Refactor the parser"));
    }

    #[test]
    fn antigravity_invocation_preserves_extra_args() {
        let input = TurnInput {
            user_message: "test".to_string(),
            system_prompt: None,
            attachments: Vec::new(),
        };
        let policy = ExecutionPolicy::workspace_write(".");
        let extra = vec!["--foo".to_string()];
        let args = build_antigravity_invocation(&extra, &input, &policy);
        assert_eq!(args.len(), 4); // -p, prompt, --sandbox, --foo
        assert_eq!(args[2], "--sandbox");
        assert_eq!(args[3], "--foo");
    }

    #[test]
    fn antigravity_policy_args_map_modes_and_extra_dirs() {
        let read_only = ExecutionPolicy::read_only(".");
        assert_eq!(antigravity_policy_args(&read_only), vec!["--sandbox"]);

        let workspace = ExecutionPolicy::workspace_write("/repo")
            .add_allowed_paths(vec![std::path::PathBuf::from("/tmp/cache")]);
        assert_eq!(
            antigravity_policy_args(&workspace),
            vec!["--sandbox", "--add-dir", "/tmp/cache"]
        );

        let danger = ExecutionPolicy::danger_full_access(".");
        assert_eq!(
            antigravity_policy_args(&danger),
            vec!["--dangerously-skip-permissions"]
        );
    }
}
