//! KohakuTerrarium headless turn execution via
//! `kt run <creature> --headless --json -p <prompt>`.
//!
//! `--json` makes `kt` emit one JSON object per line on stdout (logs go to
//! stderr), which we relay as [`ProviderEvent`]s so the chat ticker renders
//! token-by-token. The process exits 0 when the turn ends `ok`, non-zero
//! otherwise — which [`build_turn_result`] folds into the turn outcome.

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_provider_api::sentinel::{SENTINEL_BEGIN, SENTINEL_END};
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

    // argv: kt run <creature [+user extras]> --headless --json --log-level error
    //          [--llm <sel>] --sandbox <preset> --cwd <dir> --no-subagents
    //          -p <prompt>
    // The creature ref must lead so it binds to the required `agent_path`
    // positional.
    let mut args: Vec<String> = vec!["run".to_string()];
    args.extend_from_slice(configured_args);
    args.push("--headless".to_string());
    args.push("--json".to_string());
    // Quiet kt's stderr to errors only. Plugins (e.g. kt-biome's PEV verifier
    // and OpenTelemetry exporter) log benign WARNING-level "plugin disabled /
    // no-op" noise on every build; Switchyard captures kt stderr and surfaces it
    // as a failed turn's reason + a diagnostics artifact, so that noise would
    // otherwise dominate the view. Real turn outcomes arrive on the stdout JSONL
    // (`turn_end`), independent of this log level, so nothing actionable is lost.
    args.push("--log-level".to_string());
    args.push("ERROR".to_string());
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

    // NOTE: kohaku deliberately does NOT emit execution_telemetry. `kt.exe` is
    // Switchyard's own driver, not a model action — surfacing it would make the
    // live-execution card headline read "正在运行 kt.exe" and count the driver
    // as a command. The real per-tool activity (below) drives the headline and
    // the command count instead; the resolved invocation is still recorded in
    // the archived raw-output artifact for replay/diagnostics.

    let event_tx_clone = event_tx.clone();
    let consumer = tokio::spawn(async move {
        // `assistant_message` accumulates the FULL streamed body — including any
        // routing sentinel block, plus a consolidated `turn_end.text` on the
        // no-delta path — and is what the router parses for delegation (it
        // strips the block for the final display). `display` gates what reaches
        // the chat bubble so a sentinel never flickers into view while
        // streaming. `turn_error` captures a structured failure so we surface
        // the reason instead of dumping the raw JSONL as the response body.
        let mut assistant_message = String::new();
        let mut turn_error: Option<String> = None;
        let mut display = SentinelDisplayFilter::new();
        while let Some(output_line) = line_rx.recv().await {
            let line = output_line.text;
            let protocol_line = line.trim_end_matches(['\r', '\n']);
            if protocol_line.is_empty() {
                continue;
            }

            // `kt --json` stdout is pure machine protocol, one JSON object per
            // line. Mirroring it to the terminal channel would replay the whole
            // protocol into the live-execution card (runtime-detail flood), so
            // only genuine NON-JSON stdout is surfaced as terminal output.
            let Ok(json) = serde_json::from_str::<serde_json::Value>(protocol_line) else {
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
                continue;
            };

            match classify(&json) {
                KohakuEvent::Text(text) if !text.is_empty() => {
                    assistant_message.push_str(&text);
                    let revealed = display.push(&text);
                    if !revealed.is_empty() {
                        event_tx_clone
                            .send(ProviderEvent::text_message(turn_id, "kohaku", &revealed))
                            .await
                            .ok();
                    }
                }
                KohakuEvent::TurnEnd {
                    status,
                    text,
                    error,
                } => {
                    if status == "error" {
                        if turn_error.is_none() {
                            turn_error = error
                                .filter(|e| !e.trim().is_empty())
                                .or_else(|| (!text.trim().is_empty()).then(|| text.clone()))
                                .or_else(|| Some("kohaku: turn ended with error".to_string()));
                        }
                    } else if assistant_message.is_empty() && !text.is_empty() {
                        // No `text` deltas arrived — the body was delivered
                        // consolidated in `turn_end.text`. Stream it now (gated)
                        // so the bubble fills live instead of only appearing
                        // after finalize + DB refresh.
                        assistant_message.push_str(&text);
                        let revealed = display.push(&text);
                        if !revealed.is_empty() {
                            event_tx_clone
                                .send(ProviderEvent::text_message(turn_id, "kohaku", &revealed))
                                .await
                                .ok();
                        }
                    }
                }
                KohakuEvent::Error(message) => {
                    if turn_error.is_none() && !message.is_empty() {
                        turn_error = Some(message);
                    }
                }
                KohakuEvent::Activity {
                    activity_type,
                    value,
                } => {
                    // Translate genuine tool/subagent calls into normalized
                    // command_execution items so they're counted and surfaced in
                    // the live-execution card (matching codex/claude). Pure KT
                    // runtime telemetry (processing_*, token_usage, session_info,
                    // compact_*, …) is backend noise and is dropped so the chat
                    // shows only the model's message.
                    if let Some(item) = normalize_tool_activity(&activity_type, &value) {
                        event_tx_clone
                            .send(ProviderEvent::new(
                                turn_id,
                                EventType::ItemUpdated,
                                "kohaku",
                                item,
                            ))
                            .await
                            .ok();
                    }
                }
                // Empty text, turn_start, and unrecognized lines carry no
                // user-facing signal — drop them from the chat stream.
                KohakuEvent::Text(_) | KohakuEvent::TurnStart | KohakuEvent::Other => {}
            }
        }
        ConsumerResult {
            assistant_message,
            turn_error,
        }
    });

    let result = run_subprocess_streaming(&config, &line_tx, cancel).await;
    drop(line_tx);
    let consumed = consumer.await.unwrap_or_default();

    let output = match result {
        Ok(o) => o,
        Err(e) => return Err(handle_subprocess_error(e, turn_id, "kohaku", event_tx).await),
    };

    // `emit_completion_event` derives the lifecycle from the exit code — kt
    // exits non-zero on a turn error, so a failure already surfaces a
    // `turn_failed` event (don't double-emit one here).
    emit_completion_event(&output, turn_id, "kohaku", event_tx).await;

    // Use the full streamed assistant body (the router strips any sentinel for
    // display). On failure, surface the structured error message so the reason
    // isn't lost — appended after any partial prose, never replacing the raw
    // JSONL protocol dump.
    let response_text = match consumed.turn_error {
        Some(error) if consumed.assistant_message.trim().is_empty() => error,
        Some(error) => format!(
            "{}\n\n[KohakuTerrarium 错误] {error}",
            consumed.assistant_message.trim_end()
        ),
        None => consumed.assistant_message,
    };

    Ok(build_turn_result(response_text, &output, "kohaku"))
}

/// Accumulated outcome of the JSONL consumer task.
#[derive(Default)]
struct ConsumerResult {
    /// Full streamed assistant body (including any routing sentinel block, and
    /// any consolidated `turn_end.text` for the no-delta path).
    assistant_message: String,
    /// Structured failure message (`turn_end` error or a top-level `error`).
    turn_error: Option<String>,
}

/// Gates the chat display channel so a routing sentinel block never streams into
/// the bubble. Deltas are accumulated; each `push` returns only the
/// sentinel-safe text that has not yet been shown. The caller keeps the full
/// (ungated) text separately for routing.
#[derive(Default)]
struct SentinelDisplayFilter {
    buf: String,
    shown: usize,
}

impl SentinelDisplayFilter {
    fn new() -> Self {
        Self::default()
    }

    /// Append `delta`; return the newly-revealed sentinel-safe text, if any.
    fn push(&mut self, delta: &str) -> String {
        self.buf.push_str(delta);
        let safe = sentinel_safe_display(&self.buf);
        if safe.len() > self.shown {
            let revealed = safe[self.shown..].to_string();
            self.shown = safe.len();
            revealed
        } else {
            String::new()
        }
    }
}

/// The portion of `text` that is safe to display now: prose outside any
/// complete sentinel block, withholding a pending (unclosed) block and any
/// trailing fragment that could be the start of a `BEGIN` marker. The returned
/// prefix grows monotonically as more deltas arrive.
fn sentinel_safe_display(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    loop {
        match text[cursor..].find(SENTINEL_BEGIN) {
            Some(rel) => {
                let begin = cursor + rel;
                out.push_str(&text[cursor..begin]);
                let after = begin + SENTINEL_BEGIN.len();
                match text[after..].find(SENTINEL_END) {
                    Some(rel_end) => cursor = after + rel_end + SENTINEL_END.len(),
                    // Pending block — withhold everything from the marker on.
                    None => return out,
                }
            }
            None => {
                let tail = &text[cursor..];
                let keep = tail.len() - partial_begin_suffix_len(tail);
                out.push_str(&tail[..keep]);
                return out;
            }
        }
    }
}

/// Length of the longest suffix of `tail` that is a proper prefix of the
/// `BEGIN` marker, so a marker split across deltas is withheld until complete.
fn partial_begin_suffix_len(tail: &str) -> usize {
    let begin = SENTINEL_BEGIN.as_bytes();
    let bytes = tail.as_bytes();
    let max = (begin.len() - 1).min(bytes.len());
    (1..=max)
        .rev()
        .find(|&k| bytes[bytes.len() - k..] == begin[..k])
        .unwrap_or(0)
}

/// Map a KohakuTerrarium `activity` event onto Switchyard's normalized item
/// vocabulary, or `None` if it is backend runtime *telemetry* that should not
/// reach the chat. Genuine tool/subagent calls become `command_execution`
/// items so the live-execution card counts them ("已运行 N 条命令") and shows
/// the running one in its headline ("正在运行 <tool>") — the same surfaces
/// codex/claude drive via their command items. A `*_start` becomes a running
/// item that the frontend merges (by command) with its later `*_done`/`*_error`
/// into one transitioning card. Pure telemetry (processing_*, token_usage,
/// *_token_update, session_info, compact_*, tool_promoted, job_cancelled,
/// interrupt, …) and unknown types are dropped so a vocabulary drift in the KT
/// fork never reintroduces flooding.
fn normalize_tool_activity(
    activity_type: &str,
    value: &serde_json::Value,
) -> Option<serde_json::Value> {
    let status = match activity_type {
        "tool_start" | "subagent_tool_start" | "subagent_start" => "running",
        "tool_done" | "subagent_tool_done" | "subagent_done" => "completed",
        "tool_error" | "subagent_tool_error" | "subagent_error" => "failed",
        _ => return None,
    };
    let name = value
        .get("detail")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            value
                .get("metadata")
                .and_then(|m| m.get("name"))
                .and_then(|v| v.as_str())
        })
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("tool");
    Some(serde_json::json!({
        "item_type": "command_execution",
        "command": name,
        "status": status,
    }))
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
    fn sentinel_filter_withholds_block_split_across_deltas() {
        let mut filter = SentinelDisplayFilter::new();
        assert_eq!(filter.push("Hello "), "Hello ");
        // The BEGIN marker is split across two deltas — nothing must leak.
        assert_eq!(filter.push("<<<SWITCHYARD_JSON"), "");
        assert_eq!(filter.push("_BEGIN>>>{\"type\":\"deleg"), "");
        assert_eq!(filter.push("ate\"}<<<SWITCHYARD_JSON_END>>>"), "");
        // Prose after the closed block resumes streaming.
        assert_eq!(filter.push(" world"), " world");
    }

    #[test]
    fn sentinel_filter_passes_plain_text_incrementally() {
        let mut filter = SentinelDisplayFilter::new();
        assert_eq!(filter.push("a"), "a");
        assert_eq!(filter.push("bc"), "bc");
        assert_eq!(filter.push(""), "");
    }

    #[test]
    fn sentinel_safe_display_strips_a_complete_block() {
        let text = "before <<<SWITCHYARD_JSON_BEGIN>>>X<<<SWITCHYARD_JSON_END>>> after";
        assert_eq!(sentinel_safe_display(text), "before  after");
    }

    #[test]
    fn sentinel_safe_display_withholds_pending_block() {
        let text = "keep <<<SWITCHYARD_JSON_BEGIN>>>{\"partial\":true";
        assert_eq!(sentinel_safe_display(text), "keep ");
    }

    #[test]
    fn normalize_tool_activity_surfaces_tools_and_drops_telemetry() {
        // Pure telemetry and unknown types are dropped.
        assert!(normalize_tool_activity("token_usage", &serde_json::json!({})).is_none());
        assert!(normalize_tool_activity("processing_start", &serde_json::json!({})).is_none());
        assert!(normalize_tool_activity("session_info", &serde_json::json!({})).is_none());
        assert!(normalize_tool_activity("brand_new_event", &serde_json::json!({})).is_none());

        // A tool start becomes a running command_execution (the frontend merges
        // it with the later done/error by command into one transitioning card),
        // so the live card counts it and shows it in the headline.
        let start =
            normalize_tool_activity("tool_start", &serde_json::json!({"detail": "read"})).unwrap();
        assert_eq!(
            start.get("item_type").and_then(|v| v.as_str()),
            Some("command_execution")
        );
        assert_eq!(start.get("command").and_then(|v| v.as_str()), Some("read"));
        assert_eq!(
            start.get("status").and_then(|v| v.as_str()),
            Some("running")
        );

        let done =
            normalize_tool_activity("tool_done", &serde_json::json!({"detail": "read"})).unwrap();
        assert_eq!(
            done.get("status").and_then(|v| v.as_str()),
            Some("completed")
        );

        let errored = normalize_tool_activity(
            "tool_error",
            &serde_json::json!({"metadata": {"name": "write"}}),
        )
        .unwrap();
        assert_eq!(
            errored.get("command").and_then(|v| v.as_str()),
            Some("write")
        );
        assert_eq!(
            errored.get("status").and_then(|v| v.as_str()),
            Some("failed")
        );
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
