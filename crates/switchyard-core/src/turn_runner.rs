//! Minimal single-turn runner.

use std::path::PathBuf;
use std::time::Duration;

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_context::ContextComposer;
use switchyard_provider_api::{
    ContextBundle, ExecutionPolicy, InputAttachment, Provider, TurnInput,
};
use switchyard_provider_api::{
    extract_execution_telemetry, extract_hyard_job_observation, extract_terminal_output,
    is_empty_reasoning_payload,
};
use switchyard_session::{Session, Turn, TurnRole, TurnStatus};
use switchyard_store::CanonicalStore;
#[cfg(test)]
use switchyard_store::JsonlStore;

use crate::error::CoreError;
use crate::event_mapper::map_provider_event;

const ACTIVE_TURN_HEARTBEAT_SECS: u64 = 5;

/// Whether this turn is a normal core turn or a finalization turn (after delegate).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TurnPhase {
    Normal,
    Finalization,
}

/// Result of a completed turn.
pub struct TurnOutput {
    pub turn_id: Uuid,
    pub response: Option<String>,
}

/// Run a single turn against a provider.
pub async fn run_turn<S: CanonicalStore + ?Sized>(
    store: &mut S,
    session: &mut Session,
    provider: &dyn Provider,
    user_message: String,
    cwd: PathBuf,
) -> Result<TurnOutput, CoreError> {
    run_turn_full(store, session, provider, user_message, cwd, None, None).await
}

/// Run a single turn with raw output archiving.
pub async fn run_turn_with_archive<S: CanonicalStore + ?Sized>(
    store: &mut S,
    session: &mut Session,
    provider: &dyn Provider,
    user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
) -> Result<TurnOutput, CoreError> {
    run_turn_full(
        store,
        session,
        provider,
        user_message,
        cwd,
        artifact_dir,
        None,
    )
    .await
}

/// Full turn execution with optional archiving and runtime event sink.
pub async fn run_turn_full<S: CanonicalStore + ?Sized>(
    store: &mut S,
    session: &mut Session,
    provider: &dyn Provider,
    user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    runtime_tx: Option<&mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
) -> Result<TurnOutput, CoreError> {
    let policy = ExecutionPolicy::workspace_write(cwd.clone());
    run_turn_full_with_policy(
        store,
        session,
        provider,
        user_message,
        cwd,
        artifact_dir,
        runtime_tx,
        policy,
    )
    .await
}

/// Full turn execution with explicit sandbox / approval policy.
#[allow(clippy::too_many_arguments)]
pub async fn run_turn_full_with_policy<S: CanonicalStore + ?Sized>(
    store: &mut S,
    session: &mut Session,
    provider: &dyn Provider,
    user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    runtime_tx: Option<&mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
    policy: ExecutionPolicy,
) -> Result<TurnOutput, CoreError> {
    run_turn_phased_with_policy(
        store,
        session,
        provider,
        user_message,
        cwd,
        artifact_dir,
        runtime_tx,
        TurnPhase::Normal,
        switchyard_provider_api::CancellationToken::new(),
        policy,
    )
    .await
}

/// Full turn execution with explicit sandbox / approval policy.
#[allow(clippy::too_many_arguments)]
pub async fn run_turn_with_policy<S: CanonicalStore + ?Sized>(
    store: &mut S,
    session: &mut Session,
    provider: &dyn Provider,
    user_message: String,
    cwd: PathBuf,
    policy: ExecutionPolicy,
) -> Result<TurnOutput, CoreError> {
    run_turn_full_with_policy(
        store,
        session,
        provider,
        user_message,
        cwd,
        None,
        None,
        policy,
    )
    .await
}

/// Full turn execution with explicit phase, cancellation and policy support.
#[allow(clippy::too_many_arguments)]
pub async fn run_turn_phased_with_policy(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    provider: &dyn Provider,
    user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    runtime_tx: Option<&mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
    phase: TurnPhase,
    cancel: switchyard_provider_api::CancellationToken,
    policy: ExecutionPolicy,
) -> Result<TurnOutput, CoreError> {
    run_turn_phased_with_messages_and_policy(
        store,
        session,
        provider,
        user_message.clone(),
        user_message,
        cwd,
        artifact_dir,
        runtime_tx,
        phase,
        cancel,
        policy,
    )
    .await
}

/// Full turn execution with explicit phase and cancellation support.
#[allow(clippy::too_many_arguments)]
pub async fn run_turn_phased(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    provider: &dyn Provider,
    user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    runtime_tx: Option<&mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
    phase: TurnPhase,
    cancel: switchyard_provider_api::CancellationToken,
) -> Result<TurnOutput, CoreError> {
    let policy = ExecutionPolicy::workspace_write(cwd.clone());
    run_turn_phased_with_policy(
        store,
        session,
        provider,
        user_message,
        cwd,
        artifact_dir,
        runtime_tx,
        phase,
        cancel,
        policy,
    )
    .await
}

/// Full turn execution with distinct stored and provider-facing user messages.
///
/// This lets the router inject internal prompt context for the provider without
/// polluting the persisted user turn that appears in session history.
#[allow(clippy::too_many_arguments)]
#[allow(dead_code)]
pub(crate) async fn run_turn_phased_with_messages(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    provider: &dyn Provider,
    stored_user_message: String,
    provider_user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    runtime_tx: Option<&mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
    phase: TurnPhase,
    cancel: switchyard_provider_api::CancellationToken,
) -> Result<TurnOutput, CoreError> {
    let policy = ExecutionPolicy::workspace_write(cwd.clone());
    run_turn_phased_with_messages_and_policy(
        store,
        session,
        provider,
        stored_user_message,
        provider_user_message,
        cwd,
        artifact_dir,
        runtime_tx,
        phase,
        cancel,
        policy,
    )
    .await
}

/// Full turn execution with distinct stored and provider-facing user messages.
///
/// This lets the router inject internal prompt context for the provider without
/// polluting the persisted user turn that appears in session history.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_turn_phased_with_messages_and_policy(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    provider: &dyn Provider,
    stored_user_message: String,
    provider_user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    runtime_tx: Option<&mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
    phase: TurnPhase,
    cancel: switchyard_provider_api::CancellationToken,
    policy: ExecutionPolicy,
) -> Result<TurnOutput, CoreError> {
    run_turn_phased_with_messages_policy_and_attachments(
        store,
        session,
        provider,
        stored_user_message,
        provider_user_message,
        cwd,
        artifact_dir,
        runtime_tx,
        phase,
        cancel,
        Vec::new(),
        policy,
    )
    .await
}

/// Full turn execution with distinct stored/provider-facing messages and
/// optional local input attachments. Attachments are passed only to the
/// provider-facing [`TurnInput`]; the caller is responsible for including any
/// desired attachment reference note in `stored_user_message`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_turn_phased_with_messages_policy_and_attachments(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    provider: &dyn Provider,
    stored_user_message: String,
    provider_user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    runtime_tx: Option<&mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
    phase: TurnPhase,
    cancel: switchyard_provider_api::CancellationToken,
    attachments: Vec<InputAttachment>,
    mut policy: ExecutionPolicy,
) -> Result<TurnOutput, CoreError> {
    policy.cwd = cwd;

    // 1. Create Turn
    let turn = match phase {
        TurnPhase::Normal => Turn::new(
            session.session_id,
            &session.active_core,
            TurnRole::Core,
            &stored_user_message,
        ),
        TurnPhase::Finalization => Turn::new_system(
            session.session_id,
            &session.active_core,
            &stored_user_message,
        ),
    };
    let turn_id = turn.turn_id;
    store.append_turn(&turn)?;
    session.mark_turn_active(turn_id, session.active_core.clone());
    store.save_session(session)?;

    if let Some(tx) = runtime_tx {
        let event = match phase {
            TurnPhase::Normal => crate::runtime_events::RuntimeEvent::CoreTurnStarted {
                turn_id,
                provider: session.active_core.clone(),
            },
            TurnPhase::Finalization => crate::runtime_events::RuntimeEvent::FinalizationStarted {
                turn_id,
                provider: session.active_core.clone(),
            },
        };
        tx.send(event).await.ok();
    }

    // 2. Compose context from store
    let composer = ContextComposer::default();
    let all_turns = store.list_turns(session.session_id)?;
    let all_events = store
        .list_session_events(session.session_id)
        .unwrap_or_default();
    let composed = composer.compose(
        session.summary.clone(),
        &all_turns,
        &all_events,
        vec![],
        &[],
    );

    let context = ContextBundle {
        summary: composed.summary.clone(),
        recent_turns: composed
            .recent_turns
            .iter()
            .map(|t| serde_json::to_value(t).unwrap_or_default())
            .collect(),
        peer_state: composed.peer_state,
        artifacts: composed
            .relevant_artifacts
            .iter()
            .map(|a| serde_json::to_value(a).unwrap_or_default())
            .collect(),
    };

    let rendered_context = switchyard_provider_subprocess::render_context_bundle(&context);
    let input = TurnInput {
        user_message: provider_user_message,
        system_prompt: if rendered_context.is_empty() {
            None
        } else {
            Some(rendered_context)
        },
        attachments,
    };

    // Bounded channel prevents backpressure deadlock when provider emits faster than we drain.
    let (event_tx, mut event_rx) = mpsc::channel(256);
    let provider_fut =
        provider.start_turn(turn_id, input, policy, context, event_tx, cancel.clone());

    let mut failed = false;
    let mut output_completed = false;
    let mut accumulated_response = String::new();
    let provider_result;

    async fn drain_event(
        pe: &switchyard_provider_api::ProviderEvent,
        failed: &mut bool,
        output_completed: &mut bool,
        runtime_tx: Option<&mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
        store: &mut (impl CanonicalStore + ?Sized),
    ) -> Result<(), CoreError> {
        if pe.event_type == switchyard_provider_api::EventType::TurnFailed {
            *failed = true;
        }
        let empty_reasoning_payload = is_empty_reasoning_payload(&pe.payload);
        if let Some(tx) = runtime_tx {
            if let Some(execution) = extract_execution_telemetry(&pe.payload) {
                let _ = tx
                    .send(
                        crate::runtime_events::RuntimeEvent::CoreExecutionTelemetry {
                            turn_id: pe.turn_id,
                            provider: pe.provider.clone(),
                            execution,
                        },
                    )
                    .await;
            }
            if let Some(job) = extract_hyard_job_observation(&pe.payload) {
                let _ = tx
                    .send(crate::runtime_events::RuntimeEvent::HyardJobObserved {
                        turn_id: pe.turn_id,
                        source_provider: pe.provider.clone(),
                        observed_at: pe.timestamp.to_rfc3339(),
                        job,
                    })
                    .await;
            }
            if let Some(terminal) = extract_terminal_output(&pe.payload) {
                let _ = tx
                    .send(crate::runtime_events::RuntimeEvent::CoreTerminalOutput {
                        turn_id: pe.turn_id,
                        provider: pe.provider.clone(),
                        text: terminal.line,
                        transport: terminal.transport,
                    })
                    .await;
            }
            if pe.event_type == switchyard_provider_api::EventType::TurnCompleted
                && !*output_completed
            {
                *output_completed = true;
                tx.send(crate::runtime_events::RuntimeEvent::CoreOutputCompleted {
                    turn_id: pe.turn_id,
                    provider: pe.provider.clone(),
                })
                .await
                .ok();
            }
            // Streaming items are user-visible progress. Send them through the
            // runtime channel reliably; otherwise a small GUI bridge buffer can
            // make the chat appear silent until the final DB refresh.
            //
            // Some provider protocols emit a lifecycle item whose useful data is
            // only in the JSON payload (for example a tool card with no summary
            // text yet). Forward those payload-only lifecycle events too so the
            // GUI can render tool/runtime cards immediately instead of waiting
            // for a later DB refresh. Terminal output is already mirrored via
            // CoreTerminalOutput, so do not duplicate terminal-only payloads
            // into the item stream.
            let item_text = pe.display_text_or_summary();
            let is_item_lifecycle = matches!(
                pe.event_type,
                switchyard_provider_api::EventType::ItemStarted
                    | switchyard_provider_api::EventType::ItemUpdated
                    | switchyard_provider_api::EventType::ItemCompleted
                    | switchyard_provider_api::EventType::ArtifactReady
            );
            let is_terminal_payload =
                switchyard_provider_api::extract_terminal_output(&pe.payload).is_some();
            if !empty_reasoning_payload
                && (item_text.is_some() || (is_item_lifecycle && !is_terminal_payload))
            {
                let _ = tx
                    .send(crate::runtime_events::RuntimeEvent::CoreItemUpdated {
                        turn_id: pe.turn_id,
                        provider: pe.provider.clone(),
                        event_type: pe.event_type.to_string(),
                        text: item_text.unwrap_or_default(),
                        payload: Some(pe.payload.clone()),
                    })
                    .await;
            }
        }
        if empty_reasoning_payload {
            // Empty reasoning heartbeats are high-frequency protocol noise. They
            // are not forwarded live and must not be persisted either, or a
            // later DB refresh will reintroduce the same empty cards into the GUI.
            return Ok(());
        }
        let canonical = map_provider_event(pe);
        store.append_event(&canonical)?;
        Ok(())
    }

    // Concurrent select: provider execution + event drain + cancellation
    tokio::pin!(provider_fut);
    let mut cancelled = false;
    let mut active_turn_heartbeat =
        tokio::time::interval(Duration::from_secs(ACTIVE_TURN_HEARTBEAT_SECS));
    active_turn_heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            res = &mut provider_fut => {
                provider_result = res;
                break;
            }
            _ = cancel.cancelled(), if !cancelled => {
                cancelled = true;
                failed = true;
            }
            Some(pe) = event_rx.recv() => {
                update_accumulated_response(&mut accumulated_response, &pe);
                drain_event(&pe, &mut failed, &mut output_completed, runtime_tx, store).await?;
            }
            _ = active_turn_heartbeat.tick() => {
                if session.active_turn_id == Some(turn_id) {
                    session.bump_active_turn_lease();
                    store.save_session(session)?;
                }
            }
        }
    }

    // Drain remaining events after provider completes
    while let Some(pe) = event_rx.recv().await {
        update_accumulated_response(&mut accumulated_response, &pe);
        drain_event(&pe, &mut failed, &mut output_completed, runtime_tx, store).await?;
    }

    if provider_result.is_err() || cancel.is_cancelled() {
        let err_msg = match &provider_result {
            Err(e) => e.to_string(),
            Ok(_) => "cancelled".to_string(),
        };
        let mut failed_turn = turn;
        failed_turn.status = TurnStatus::Failed;
        failed_turn.error_message = Some(err_msg.clone());
        failed_turn.completed_at = Some(chrono::Utc::now());
        let cleaned_response = clean_system_status_lines(&accumulated_response);
        if !cleaned_response.trim().is_empty() {
            failed_turn.provider_response = Some(cleaned_response);
        }
        store.append_turn(&failed_turn)?;
        session.clear_active_turn();
        store.save_session(session)?;
        if let Some(tx) = runtime_tx {
            tx.send(crate::runtime_events::RuntimeEvent::TurnFailed {
                turn_id,
                provider: session.active_core.clone(),
                error: err_msg,
            })
            .await
            .ok();
        }
        return Ok(TurnOutput {
            turn_id,
            response: failed_turn.provider_response,
        });
    }

    // Provider output is done — signal UI before slow finalize/archive work.
    if !output_completed && let Some(tx) = runtime_tx {
        tx.send(crate::runtime_events::RuntimeEvent::CoreOutputCompleted {
            turn_id,
            provider: session.active_core.clone(),
        })
        .await
        .ok();
    }

    // 4. Finalize turn
    let (result, artifact_bundle) = match provider.finalize_turn(turn_id).await {
        Ok(result) => result,
        Err(err) => {
            session.clear_active_turn();
            store.save_session(session)?;
            return Err(err.into());
        }
    };

    // Store provider artifacts
    for entry in &artifact_bundle.artifacts {
        let artifact_type = serde_json::from_value::<switchyard_session::ArtifactType>(
            serde_json::Value::String(entry.artifact_type.clone()),
        )
        .unwrap_or(switchyard_session::ArtifactType::RawProviderOutput);

        let mut artifact = switchyard_session::Artifact::new(turn_id, artifact_type, &entry.title);
        artifact.summary = entry.summary.clone();
        artifact.path = entry.path.clone();
        artifact.metadata = entry.metadata.clone();
        store.save_artifact(&artifact)?;
    }

    // Archive raw stdout/stderr to disk if artifact_dir is provided.
    // Use raw_stdout from metadata (unprocessed) rather than response_text (normalized).
    if let Some(dir) = artifact_dir {
        let raw_stdout = result
            .metadata
            .get("raw_stdout")
            .and_then(|v| v.as_str())
            .unwrap_or(&result.response_text);
        if let Ok(paths) = switchyard_artifacts::archive_raw_output(
            dir,
            &turn_id.to_string(),
            Some(raw_stdout),
            result.stderr.as_deref(),
        ) {
            for path in &paths {
                let title = path.file_name().unwrap_or_default().to_string_lossy();
                let mut artifact = switchyard_session::Artifact::new(
                    turn_id,
                    switchyard_session::ArtifactType::RawProviderOutput,
                    title.as_ref(),
                );
                artifact.path = Some(path.clone());
                store.save_artifact(&artifact)?;
            }
        }
    }

    // Archive raw stderr as summary artifact (always, even without artifact_dir)
    if let Some(stderr) = &result.stderr
        && !stderr.is_empty()
    {
        let mut artifact = switchyard_session::Artifact::new(
            turn_id,
            switchyard_session::ArtifactType::CommandOutput,
            "stderr",
        );
        artifact.summary = Some(stderr.clone());
        store.save_artifact(&artifact)?;
    }

    // 5. Update Turn in store
    let mut updated_turn = turn;
    let cleaned_response = clean_system_status_lines(&result.response_text);
    updated_turn.provider_response = Some(cleaned_response);
    if failed || result.exit_code.is_some_and(|c| c != 0) {
        updated_turn.status = TurnStatus::Failed;
        updated_turn.error_message = result
            .stderr
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                result
                    .exit_code
                    .filter(|code| *code != 0)
                    .map(|code| format!("non-zero exit ({code})"))
            })
            .or_else(|| failed.then(|| "provider reported failure".to_string()));
    } else {
        updated_turn.status = TurnStatus::Completed;
    }
    updated_turn.completed_at = Some(chrono::Utc::now());
    store.append_turn(&updated_turn)?;

    session.clear_active_turn();
    store.save_session(session)?;

    if let Some(tx) = runtime_tx {
        if updated_turn.status == TurnStatus::Completed {
            tx.send(crate::runtime_events::RuntimeEvent::TurnCompleted {
                turn_id,
                provider: session.active_core.clone(),
                response: updated_turn.provider_response.clone(),
            })
            .await
            .ok();
        } else {
            tx.send(crate::runtime_events::RuntimeEvent::TurnFailed {
                turn_id,
                provider: session.active_core.clone(),
                error: updated_turn.error_message.clone().unwrap_or_default(),
            })
            .await
            .ok();
        }
    }

    Ok(TurnOutput {
        turn_id,
        response: updated_turn.provider_response,
    })
}

fn is_system_status_line(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.starts_with('[') {
        let prefixes = [
            "[会话]",
            "[回合]",
            "[系统]",
            "[系统反馈]",
            "[助手]",
            "[结果]",
            "[限额]",
            "[思考]",
            "[工具]",
            "[文件]",
            "[Diff]",
            "[待办]",
            "[委托]",
            "[错误]",
            "[执行]",
            "[exec]",
            "[HTTP]",
            "[STDIO]",
            "[hyard]",
            "[error]",
            "[命令]",
        ];
        if prefixes.iter().any(|prefix| trimmed.starts_with(prefix)) {
            return true;
        }
        if let Some(end_idx) = trimmed.find(']') {
            let inside = &trimmed[1..end_idx];
            if !inside.is_empty()
                && inside
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == ':' || c == '/')
            {
                return true;
            }
        }
    }
    false
}

fn clean_system_status_lines(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|line| !is_system_status_line(line))
        .collect();
    lines.join("\n")
}

fn payload_item_type(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get("item_type")
        .and_then(|value| value.as_str())
        .or_else(|| {
            payload
                .get("item")
                .and_then(|item| item.get("type"))
                .and_then(|value| value.as_str())
        })
        .or_else(|| {
            payload
                .get("params")
                .and_then(|params| params.get("item_type"))
                .and_then(|value| value.as_str())
        })
        .or_else(|| {
            payload
                .get("params")
                .and_then(|params| params.get("item"))
                .and_then(|item| item.get("type"))
                .and_then(|value| value.as_str())
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

fn delta_text(value: &serde_json::Value) -> Option<String> {
    non_empty_payload_str(value)
        .or_else(|| value.get("text").and_then(non_empty_payload_str))
        .or_else(|| value.get("content").and_then(content_text))
        .or_else(|| value.get("delta").and_then(delta_text))
        .or_else(|| {
            value
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(content_text)
        })
}

fn update_accumulated_response(
    accumulated: &mut String,
    pe: &switchyard_provider_api::ProviderEvent,
) {
    let payload = &pe.payload;

    if let Some(item_type) = payload_item_type(payload)
        && is_non_assistant_activity_item(item_type)
    {
        return;
    }

    // 1. Check if it's a text message (plain text)
    if let Some(t) = payload.get("text").and_then(|v| v.as_str()) {
        if !is_system_status_line(t) {
            accumulated.push_str(t);
        }
        return;
    }

    // 2. Check for delta updates (Codex/Claude delta)
    if let Some(delta) = payload.get("delta") {
        if let Some(t) = delta_text(delta) {
            accumulated.push_str(&t);
            return;
        }
    }

    if let Some(params) = payload.get("params") {
        let before = accumulated.clone();
        let nested = switchyard_provider_api::ProviderEvent::new(
            pe.turn_id,
            pe.event_type.clone(),
            pe.provider.clone(),
            params.clone(),
        );
        update_accumulated_response(accumulated, &nested);
        if *accumulated != before {
            return;
        }
    }

    // 3. Check for Gemini content
    if let Some(t) = payload.get("content").and_then(content_text) {
        let is_delta = payload
            .get("delta")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if is_delta {
            accumulated.push_str(&t);
        } else {
            *accumulated = t.to_string();
        }
        return;
    }

    // 4. Check for Codex item.completed
    if let Some(t) = payload
        .get("item")
        .and_then(|i| i.get("text"))
        .and_then(|v| v.as_str())
    {
        *accumulated = t.to_string();
        return;
    }
    if let Some(t) = payload
        .get("item")
        .and_then(|i| i.get("content"))
        .and_then(content_text)
    {
        *accumulated = t;
        return;
    }
    if let Some(t) = payload
        .get("item")
        .and_then(|i| i.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(content_text)
    {
        *accumulated = t;
        return;
    }

    // 5. Check for Claude result
    if let Some(t) = payload.get("result").and_then(|v| v.as_str()) {
        *accumulated = t.to_string();
        return;
    }

    // 6. Check for Claude assistant content
    if let Some(t) = payload
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(content_text)
    {
        *accumulated = t;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_provider::FakeProvider;
    use std::collections::HashMap;
    use std::sync::Arc;
    use switchyard_session::EventType;
    use switchyard_store::{ArtifactStore, EventLog, SessionRepository, TurnRepository};

    fn temp_store() -> (JsonlStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (JsonlStore::new(dir.path().to_path_buf()), dir)
    }

    #[test]
    fn update_accumulated_response_handles_codex_delta_shapes() {
        let turn_id = Uuid::now_v7();
        let mut text = String::new();

        let string_delta = switchyard_provider_api::ProviderEvent::new(
            turn_id,
            switchyard_provider_api::EventType::ItemUpdated,
            "codex",
            serde_json::json!({
                "method": "item/agentMessage/delta",
                "params": { "delta": "hel" }
            }),
        );
        update_accumulated_response(&mut text, &string_delta);

        let object_delta = switchyard_provider_api::ProviderEvent::new(
            turn_id,
            switchyard_provider_api::EventType::ItemUpdated,
            "codex",
            serde_json::json!({
                "type": "item.delta",
                "delta": { "type": "agent_message_delta", "text": "lo" }
            }),
        );
        update_accumulated_response(&mut text, &object_delta);

        assert_eq!(text, "hello");
    }

    #[test]
    fn update_accumulated_response_ignores_tool_result_content() {
        let turn_id = Uuid::now_v7();
        let mut text = String::from("assistant body");
        let tool_result = switchyard_provider_api::ProviderEvent::new(
            turn_id,
            switchyard_provider_api::EventType::ItemCompleted,
            "codex",
            serde_json::json!({
                "type": "item.completed",
                "item": {
                    "type": "tool_result",
                    "content": "tool stdout"
                }
            }),
        );

        update_accumulated_response(&mut text, &tool_result);

        assert_eq!(text, "assistant body");
    }

    struct PolicyCaptureProvider {
        seen: Arc<tokio::sync::Mutex<Option<ExecutionPolicy>>>,
        results: Arc<
            tokio::sync::Mutex<
                HashMap<
                    Uuid,
                    (
                        switchyard_provider_api::TurnResult,
                        switchyard_provider_api::ArtifactBundle,
                    ),
                >,
            >,
        >,
    }

    impl PolicyCaptureProvider {
        fn new() -> Self {
            Self {
                seen: Arc::new(tokio::sync::Mutex::new(None)),
                results: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for PolicyCaptureProvider {
        async fn probe(
            &self,
        ) -> Result<switchyard_provider_api::ProbeResult, switchyard_provider_api::ProviderError>
        {
            Ok(switchyard_provider_api::ProbeResult {
                version: None,
                available: true,
                capabilities: Default::default(),
                issues: vec![],
                ..Default::default()
            })
        }

        async fn start_turn(
            &self,
            turn_id: Uuid,
            _input: TurnInput,
            policy: ExecutionPolicy,
            _context: ContextBundle,
            event_tx: mpsc::Sender<switchyard_provider_api::ProviderEvent>,
            _cancel: switchyard_provider_api::CancellationToken,
        ) -> Result<(), switchyard_provider_api::ProviderError> {
            *self.seen.lock().await = Some(policy);
            event_tx
                .send(switchyard_provider_api::ProviderEvent::turn_started(
                    turn_id,
                    "policy-capture",
                ))
                .await
                .ok();
            event_tx
                .send(switchyard_provider_api::ProviderEvent::turn_completed(
                    turn_id,
                    "policy-capture",
                ))
                .await
                .ok();
            self.results.lock().await.insert(
                turn_id,
                (
                    switchyard_provider_api::TurnResult {
                        response_text: "ok".into(),
                        exit_code: Some(0),
                        stderr: None,
                        metadata: HashMap::new(),
                    },
                    switchyard_provider_api::ArtifactBundle { artifacts: vec![] },
                ),
            );
            Ok(())
        }

        async fn finalize_turn(
            &self,
            turn_id: Uuid,
        ) -> Result<
            (
                switchyard_provider_api::TurnResult,
                switchyard_provider_api::ArtifactBundle,
            ),
            switchyard_provider_api::ProviderError,
        > {
            self.results.lock().await.remove(&turn_id).ok_or(
                switchyard_provider_api::ProviderError::ExecutionFailed("no result".into()),
            )
        }
    }

    #[tokio::test]
    async fn run_turn_default_policy_is_workspace_write_not_permissive() {
        let (mut store, _dir) = temp_store();
        let mut session = Session::new("policy-capture".to_string());
        store.save_session(&session).unwrap();
        let provider = PolicyCaptureProvider::new();
        let cwd = PathBuf::from(".");

        run_turn(
            &mut store,
            &mut session,
            &provider,
            "capture policy".to_string(),
            cwd.clone(),
        )
        .await
        .unwrap();

        let seen = provider.seen.lock().await.clone().expect("policy captured");
        assert!(seen.write_access);
        assert_eq!(seen.cwd, cwd);
        assert_eq!(seen.allowed_paths, vec![cwd]);
        assert_eq!(
            seen.effective_sandbox_mode(),
            switchyard_provider_api::EffectiveSandboxMode::WorkspaceWrite
        );
    }

    #[tokio::test]
    async fn run_turn_with_policy_passes_read_only_policy_through() {
        let (mut store, _dir) = temp_store();
        let mut session = Session::new("policy-capture".to_string());
        store.save_session(&session).unwrap();
        let provider = PolicyCaptureProvider::new();
        let cwd = PathBuf::from(".");

        run_turn_with_policy(
            &mut store,
            &mut session,
            &provider,
            "capture policy".to_string(),
            cwd.clone(),
            ExecutionPolicy::read_only(cwd.clone()),
        )
        .await
        .unwrap();

        let seen = provider.seen.lock().await.clone().expect("policy captured");
        assert!(!seen.write_access);
        assert_eq!(seen.cwd, cwd);
        assert!(seen.allowed_paths.is_empty());
        assert_eq!(
            seen.effective_sandbox_mode(),
            switchyard_provider_api::EffectiveSandboxMode::ReadOnly
        );
    }

    #[tokio::test]
    async fn run_turn_success_writes_events_and_result() {
        let (mut store, _dir) = temp_store();
        let mut session = Session::new("fake".to_string());
        store.save_session(&session).unwrap();

        let provider = FakeProvider::success("I fixed the bug");
        let output = run_turn(
            &mut store,
            &mut session,
            &provider,
            "fix the bug".to_string(),
            PathBuf::from("."),
        )
        .await
        .unwrap();
        let turn_id = output.turn_id;
        assert_eq!(output.response.as_deref(), Some("I fixed the bug"));

        let events = store.list_events(turn_id).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, EventType::TurnStarted);

        let turns = store.list_turns(session.session_id).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Completed);
        let persisted_session = store.load_session(session.session_id).unwrap().unwrap();
        assert!(persisted_session.active_turn_id.is_none());
        assert!(persisted_session.active_turn_lease_expires_at.is_none());

        let artifacts = store.list_artifacts(turn_id).unwrap();
        assert_eq!(artifacts.len(), 1);
    }

    #[tokio::test]
    async fn run_turn_forwards_payload_only_lifecycle_items_to_runtime() {
        use std::collections::HashMap;
        use std::sync::Arc;
        use switchyard_provider_api::*;

        struct PayloadOnlyProvider {
            results: Arc<tokio::sync::Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
        }

        #[async_trait::async_trait]
        impl Provider for PayloadOnlyProvider {
            async fn probe(&self) -> Result<ProbeResult, ProviderError> {
                Ok(ProbeResult {
                    version: None,
                    available: true,
                    capabilities: Default::default(),
                    issues: vec![],
                    ..Default::default()
                })
            }

            async fn start_turn(
                &self,
                turn_id: Uuid,
                _input: TurnInput,
                _policy: ExecutionPolicy,
                _context: ContextBundle,
                event_tx: mpsc::Sender<ProviderEvent>,
                _cancel: CancellationToken,
            ) -> Result<(), ProviderError> {
                event_tx
                    .send(ProviderEvent::turn_started(turn_id, "payload-only"))
                    .await
                    .ok();
                event_tx
                    .send(ProviderEvent::new(
                        turn_id,
                        switchyard_provider_api::EventType::ItemStarted,
                        "payload-only",
                        serde_json::json!({ "opaque": { "phase": "tool-starting" } }),
                    ))
                    .await
                    .ok();
                event_tx
                    .send(ProviderEvent::turn_completed(turn_id, "payload-only"))
                    .await
                    .ok();

                self.results.lock().await.insert(
                    turn_id,
                    (
                        TurnResult {
                            response_text: "ok".into(),
                            exit_code: Some(0),
                            stderr: None,
                            metadata: HashMap::new(),
                        },
                        ArtifactBundle { artifacts: vec![] },
                    ),
                );
                Ok(())
            }

            async fn finalize_turn(
                &self,
                turn_id: Uuid,
            ) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
                self.results
                    .lock()
                    .await
                    .remove(&turn_id)
                    .ok_or(ProviderError::ExecutionFailed("no result".into()))
            }
        }

        let (mut store, _dir) = temp_store();
        let mut session = Session::new("payload-only".to_string());
        store.save_session(&session).unwrap();

        let provider = PayloadOnlyProvider {
            results: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        };
        let (tx, mut rx) = mpsc::channel::<crate::runtime_events::RuntimeEvent>(16);

        run_turn_full(
            &mut store,
            &mut session,
            &provider,
            "trigger tool".to_string(),
            PathBuf::from("."),
            None,
            Some(&tx),
        )
        .await
        .unwrap();

        drop(tx);
        let mut runtime_events = Vec::new();
        while let Some(event) = rx.recv().await {
            runtime_events.push(event);
        }

        let payload_only_item = runtime_events.iter().find_map(|event| match event {
            crate::runtime_events::RuntimeEvent::CoreItemUpdated {
                event_type,
                text,
                payload,
                ..
            } if event_type == "item_started"
                && text.is_empty()
                && payload
                    .as_ref()
                    .and_then(|payload| payload.get("opaque"))
                    .is_some() =>
            {
                Some(())
            }
            _ => None,
        });

        assert!(
            payload_only_item.is_some(),
            "payload-only item_started should be forwarded to runtime; events: {runtime_events:?}"
        );
    }

    #[tokio::test]
    async fn run_turn_failure_marks_turn_failed() {
        let (mut store, _dir) = temp_store();
        let mut session = Session::new("fake".to_string());
        store.save_session(&session).unwrap();

        let provider = FakeProvider::failure("crash");
        let output = run_turn(
            &mut store,
            &mut session,
            &provider,
            "do something".to_string(),
            PathBuf::from("."),
        )
        .await
        .unwrap();

        let events = store.list_events(output.turn_id).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].event_type, EventType::TurnFailed);

        let turns = store.list_turns(session.session_id).unwrap();
        assert_eq!(turns.last().unwrap().status, TurnStatus::Failed);
        let persisted_session = store.load_session(session.session_id).unwrap().unwrap();
        assert!(persisted_session.active_turn_id.is_none());
    }

    #[tokio::test]
    async fn run_turn_with_prior_history_composes_context() {
        let (mut store, _dir) = temp_store();
        let mut session = Session::new("fake".to_string());
        session.summary = Some("working on auth".to_string());
        store.save_session(&session).unwrap();

        let provider = FakeProvider::success("done");
        run_turn(
            &mut store,
            &mut session,
            &provider,
            "first".to_string(),
            PathBuf::from("."),
        )
        .await
        .unwrap();

        let output = run_turn(
            &mut store,
            &mut session,
            &provider,
            "second".to_string(),
            PathBuf::from("."),
        )
        .await
        .unwrap();

        let events = store.list_events(output.turn_id).unwrap();
        assert_eq!(events.len(), 3);
    }

    /// Verify >256 events don't deadlock (channel capacity is 256).
    #[tokio::test]
    async fn run_turn_handles_many_events_without_deadlock() {
        use std::collections::HashMap;
        use std::sync::Arc;
        use switchyard_provider_api::*;

        struct FloodProvider {
            results: Arc<tokio::sync::Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
        }
        impl FloodProvider {
            fn new() -> Self {
                Self {
                    results: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
                }
            }
        }

        #[async_trait::async_trait]
        impl Provider for FloodProvider {
            async fn probe(&self) -> Result<ProbeResult, ProviderError> {
                Ok(ProbeResult {
                    version: None,
                    available: true,
                    capabilities: Default::default(),
                    issues: vec![],
                    ..Default::default()
                })
            }
            async fn start_turn(
                &self,
                turn_id: Uuid,
                _input: TurnInput,
                _policy: ExecutionPolicy,
                _context: ContextBundle,
                event_tx: mpsc::Sender<ProviderEvent>,
                _cancel: CancellationToken,
            ) -> Result<(), ProviderError> {
                event_tx
                    .send(ProviderEvent::turn_started(turn_id, "flood"))
                    .await
                    .ok();
                for i in 0..500 {
                    event_tx
                        .send(ProviderEvent::text_message(
                            turn_id,
                            "flood",
                            format!("line {i}"),
                        ))
                        .await
                        .ok();
                }
                event_tx
                    .send(ProviderEvent::turn_completed(turn_id, "flood"))
                    .await
                    .ok();
                self.results.lock().await.insert(
                    turn_id,
                    (
                        TurnResult {
                            response_text: "ok".into(),
                            exit_code: Some(0),
                            stderr: None,
                            metadata: HashMap::new(),
                        },
                        ArtifactBundle { artifacts: vec![] },
                    ),
                );
                Ok(())
            }
            async fn finalize_turn(
                &self,
                turn_id: Uuid,
            ) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
                self.results
                    .lock()
                    .await
                    .remove(&turn_id)
                    .ok_or(ProviderError::ExecutionFailed("no result".into()))
            }
        }

        let (mut store, _dir) = temp_store();
        let mut session = Session::new("flood".to_string());
        store.save_session(&session).unwrap();

        let provider = FloodProvider::new();
        let output = run_turn(
            &mut store,
            &mut session,
            &provider,
            "flood".to_string(),
            PathBuf::from("."),
        )
        .await
        .unwrap();

        let events = store.list_events(output.turn_id).unwrap();
        assert_eq!(events.len(), 502); // 1 started + 500 text + 1 completed
    }

    #[tokio::test]
    async fn non_zero_exit_without_stderr_keeps_fallback_error_message() {
        use std::collections::HashMap;
        use std::sync::Arc;
        use switchyard_provider_api::*;

        struct ExitCodeOnlyProvider {
            results: Arc<tokio::sync::Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
        }

        #[async_trait::async_trait]
        impl Provider for ExitCodeOnlyProvider {
            async fn probe(&self) -> Result<ProbeResult, ProviderError> {
                Ok(ProbeResult {
                    version: None,
                    available: true,
                    capabilities: Default::default(),
                    issues: vec![],
                    ..Default::default()
                })
            }

            async fn start_turn(
                &self,
                turn_id: Uuid,
                _input: TurnInput,
                _policy: ExecutionPolicy,
                _context: ContextBundle,
                event_tx: mpsc::Sender<ProviderEvent>,
                _cancel: CancellationToken,
            ) -> Result<(), ProviderError> {
                event_tx
                    .send(ProviderEvent::turn_started(turn_id, "exitonly"))
                    .await
                    .ok();
                event_tx
                    .send(ProviderEvent::turn_completed(turn_id, "exitonly"))
                    .await
                    .ok();
                self.results.lock().await.insert(
                    turn_id,
                    (
                        TurnResult {
                            response_text: String::new(),
                            exit_code: Some(1),
                            stderr: None,
                            metadata: HashMap::new(),
                        },
                        ArtifactBundle { artifacts: vec![] },
                    ),
                );
                Ok(())
            }

            async fn finalize_turn(
                &self,
                turn_id: Uuid,
            ) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
                self.results
                    .lock()
                    .await
                    .remove(&turn_id)
                    .ok_or(ProviderError::ExecutionFailed("no result".into()))
            }
        }

        let (mut store, _dir) = temp_store();
        let mut session = Session::new("exitonly".to_string());
        store.save_session(&session).unwrap();

        let provider = ExitCodeOnlyProvider {
            results: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        };

        let output = run_turn(
            &mut store,
            &mut session,
            &provider,
            "test".to_string(),
            PathBuf::from("."),
        )
        .await
        .unwrap();

        let turns = store.list_turns(session.session_id).unwrap();
        let last = turns.last().unwrap();
        assert_eq!(last.status, TurnStatus::Failed);
        assert_eq!(last.error_message.as_deref(), Some("non-zero exit (1)"));

        let events = store.list_events(output.turn_id).unwrap();
        assert_eq!(
            events[1].event_type,
            switchyard_session::EventType::TurnCompleted
        );
    }

    #[test]
    fn test_system_status_line_filtering() {
        assert!(is_system_status_line(
            "[命令] 开始执行: \"C:\\Program Files\\PowerShell\\7\\pwsh.exe\" -Command ..."
        ));
        assert!(is_system_status_line("[exec] running task"));
        assert!(is_system_status_line("[HTTP] GET /status"));
        assert!(is_system_status_line("[system:info] memory level high"));
        assert!(!is_system_status_line("Normal assistant message text"));
        assert!(!is_system_status_line("This has [命令] in the middle"));

        let mixed_text = "Hello\n[命令] Executing shell\nWorld\n[系统反馈] Done";
        let cleaned = clean_system_status_lines(mixed_text);
        assert_eq!(cleaned, "Hello\nWorld");
    }
}
