//! Gemini headless turn execution via `gemini -p "" -o stream-json`.
//!
//! On Windows, Gemini is typically an npm `.cmd` wrapper. Multiline prompts
//! in argv break batch file argument parsing. Instead, we pass `-p ""`
//! to enter headless mode and send the actual prompt via stdin.

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_provider_api::*;
use switchyard_provider_subprocess::{
    StreamingOutputLine, SubprocessConfig, build_subprocess_invocation_plan, build_turn_result,
    compose_prompt, emit_completion_event, handle_subprocess_error, run_subprocess_streaming,
};

/// Build the args and stdin_data for a Gemini invocation.
///
/// Returns `(args, prompt)` where prompt is sent via stdin.
pub(crate) fn build_gemini_invocation(
    extra_args: &[String],
    input: &TurnInput,
    policy: &ExecutionPolicy,
) -> (Vec<String>, String) {
    let prompt = compose_prompt(input);
    // `-p ""` triggers headless mode; actual prompt goes through stdin
    // to avoid Windows .cmd wrapper batch argument issues.
    let mut args: Vec<String> = vec![
        "-p".to_string(),
        String::new(), // empty string — prompt via stdin
        "-o".to_string(),
        "stream-json".to_string(),
    ];
    args.extend(gemini_policy_args(policy));
    args.extend_from_slice(extra_args);
    (args, prompt)
}

fn gemini_policy_args(policy: &ExecutionPolicy) -> Vec<String> {
    let mut args = Vec::new();
    match policy.effective_sandbox_mode() {
        EffectiveSandboxMode::ReadOnly => {
            args.push("--approval-mode".to_string());
            args.push("plan".to_string());
        }
        EffectiveSandboxMode::WorkspaceWrite => {
            args.push("--approval-mode".to_string());
            args.push("auto_edit".to_string());
        }
        EffectiveSandboxMode::DangerFullAccess => {
            args.push("--yolo".to_string());
        }
    }
    for path in policy.additional_allowed_paths() {
        args.push("--include-directories".to_string());
        args.push(path.display().to_string());
    }
    args
}

#[allow(clippy::too_many_arguments)]
pub async fn run_gemini_turn(
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
        .send(ProviderEvent::turn_started(turn_id, "gemini"))
        .await
        .ok();

    let (args, prompt) = build_gemini_invocation(extra_args, input, policy);
    let plan = build_subprocess_invocation_plan(original_command, command, &args);

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

    event_tx
        .send(ProviderEvent::execution_telemetry(
            turn_id,
            "gemini",
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
                    "gemini",
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
                if msg_type == "message"
                    && json.get("role").and_then(|r| r.as_str()) == Some("assistant")
                    && let Some(text) = json.get("content").and_then(|c| c.as_str())
                {
                    if json.get("delta").and_then(|d| d.as_bool()) == Some(true) {
                        assistant_message.push_str(text);
                    } else {
                        assistant_message = text.to_string();
                    }
                }
                event_tx_clone
                    .send(ProviderEvent::new(
                        turn_id,
                        EventType::ItemUpdated,
                        "gemini",
                        json,
                    ))
                    .await
                    .ok();
            } else if !protocol_line.trim().is_empty() {
                event_tx_clone
                    .send(ProviderEvent::text_message(
                        turn_id,
                        "gemini",
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
        Err(e) => return Err(handle_subprocess_error(e, turn_id, "gemini", event_tx).await),
    };

    emit_completion_event(&output, turn_id, "gemini", event_tx).await;

    let response_text = if assistant_message.is_empty() {
        output.stdout.trim().to_string()
    } else {
        assistant_message
    };

    Ok(build_turn_result(response_text, &output, "gemini"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_invocation_sends_prompt_via_stdin_not_argv() {
        let input = TurnInput {
            user_message: "Say hello".to_string(),
            system_prompt: Some("You are helpful".to_string()),
            attachments: Vec::new(),
        };
        let policy = ExecutionPolicy::workspace_write(".");
        let (args, prompt) = build_gemini_invocation(&[], &input, &policy);

        // Args must contain -p with empty string, NOT the prompt content
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], ""); // empty — prompt goes via stdin
        assert_eq!(args[2], "-o");
        assert_eq!(args[3], "stream-json");

        // Prompt must NOT appear in args
        for arg in &args {
            assert!(!arg.contains("Say hello"), "prompt leaked into argv: {arg}");
            assert!(
                !arg.contains("You are helpful"),
                "system_prompt leaked into argv: {arg}"
            );
        }

        // Prompt must be in stdin_data
        assert!(prompt.contains("Say hello"));
        assert!(prompt.contains("You are helpful"));
    }

    #[test]
    fn gemini_invocation_preserves_extra_args() {
        let input = TurnInput {
            user_message: "test".to_string(),
            system_prompt: None,
            attachments: Vec::new(),
        };
        let policy = ExecutionPolicy::workspace_write(".");
        let extra = vec!["--foo".to_string()];
        let (args, _) = build_gemini_invocation(&extra, &input, &policy);

        assert_eq!(
            args,
            vec![
                "-p",
                "",
                "-o",
                "stream-json",
                "--approval-mode",
                "auto_edit",
                "--foo"
            ]
        );
    }

    #[test]
    fn gemini_policy_args_map_modes_and_extra_dirs() {
        let read_only = ExecutionPolicy::read_only(".");
        assert_eq!(
            gemini_policy_args(&read_only),
            vec!["--approval-mode", "plan"]
        );

        let workspace = ExecutionPolicy::workspace_write("/repo")
            .add_allowed_paths(vec![std::path::PathBuf::from("/tmp/cache")]);
        assert_eq!(
            gemini_policy_args(&workspace),
            vec![
                "--approval-mode",
                "auto_edit",
                "--include-directories",
                "/tmp/cache"
            ]
        );

        let danger = ExecutionPolicy::danger_full_access(".");
        assert_eq!(gemini_policy_args(&danger), vec!["--yolo"]);
    }
}
