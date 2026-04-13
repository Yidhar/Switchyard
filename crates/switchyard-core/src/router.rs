//! Routed turn: single user message that may trigger a core -> peer -> core loop.
//!
//! V1 constraints:
//! - Max 3 router iterations (core, delegate, core finalization)
//! - Only 1 delegate task per request
//! - Peers cannot delegate further

use std::path::{Path, PathBuf};

use switchyard_host_jobs::list_job_summaries;
use switchyard_text::{preview_chars, preview_collapsed};
use uuid::Uuid;

use switchyard_config::SwitchyardConfig;
use switchyard_provider_api::{
    DelegateRequest, PeerCatalog, Provider, extract_sentinel_blocks, render_delegate_result_block,
    strip_sentinel_blocks,
};
use switchyard_session::Session;
use switchyard_store::CanonicalStore;

use crate::error::CoreError;
use crate::turn_runner::TurnOutput;

const MAX_ROUTER_LOOPS: usize = 3;
const MAX_CONTINUATION_HINT_JOBS: usize = 3;

/// Result of a routed turn, which may include delegation.
pub struct RoutedTurnOutput {
    /// The final turn_id (the last core turn).
    pub turn_id: Uuid,
    /// The final response text to show the user.
    pub response: Option<String>,
    /// Whether delegation occurred during this routed turn.
    pub delegated: bool,
}

fn build_initial_router_message(
    user_message: &str,
    peer_catalog: &PeerCatalog,
    continuation_hint: Option<&str>,
) -> String {
    let mut sections = vec![user_message.to_string()];

    if !peer_catalog.peers.is_empty() {
        sections.push(peer_catalog.render_prompt_block());
    }

    if let Some(hint) = continuation_hint.filter(|hint| !hint.trim().is_empty()) {
        sections.push(hint.to_string());
    }

    sections.join("\n\n---\n")
}

fn build_finalization_router_message(
    user_message: &str,
    delegate_context: &str,
    continuation_hint: Option<&str>,
) -> String {
    let mut message = format!(
        "Original user task:\n{user_message}\n\n\
         A delegate task has been completed. Here is the result:\n\n\
         {delegate_context}"
    );

    if let Some(hint) = continuation_hint.filter(|hint| !hint.trim().is_empty()) {
        message.push_str("\n\n");
        message.push_str(hint);
    }

    message.push_str(
        "\n\nIncorporate the delegate's findings into your response. \
         Provide the final answer to the user's original task. \
         Do NOT emit another delegate request.",
    );
    message
}

fn build_hyard_continuation_hint(cwd: &Path) -> Option<String> {
    let config = SwitchyardConfig::resolve(cwd).unwrap_or_default();
    build_hyard_continuation_hint_from_dir(&config.job_dir(cwd))
}

fn build_hyard_continuation_hint_from_dir(job_dir: &Path) -> Option<String> {
    let mut jobs = list_job_summaries(job_dir, MAX_CONTINUATION_HINT_JOBS * 3)
        .into_iter()
        .filter(|job| is_active_hyard_job_status(&job.status))
        .collect::<Vec<_>>();

    if jobs.is_empty() {
        return None;
    }

    jobs.sort_by(|left, right| {
        right
            .wait_timeout_count
            .cmp(&left.wait_timeout_count)
            .then_with(|| {
                hyard_job_status_rank(&right.status).cmp(&hyard_job_status_rank(&left.status))
            })
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.provider.cmp(&right.provider))
    });
    jobs.truncate(MAX_CONTINUATION_HINT_JOBS);

    let mut lines = vec![
        "HYARD continuation hint:".to_string(),
        "You already have live HYARD job(s):".to_string(),
    ];
    for job in jobs {
        let mut detail = format!(
            "- provider={} job_id={} status={}",
            job.provider, job.job_id, job.status
        );
        if job.wait_timeout_count > 0 {
            detail.push_str(&format!(" wait_timeouts={}", job.wait_timeout_count));
        }
        if let Some(last_event) = job.last_event.as_deref().and_then(compact_hyard_detail) {
            detail.push_str(&format!(" last_event={last_event}"));
        } else if let Some(preview) = job
            .last_output_preview
            .as_deref()
            .and_then(compact_hyard_detail)
        {
            detail.push_str(&format!(" preview={preview}"));
        }
        lines.push(detail);
    }
    lines.push(
        "If a previous HYARD bridge call returned wait_timeout, that is not a failure; the same job_id is still running in background."
            .to_string(),
    );
    lines.push(
        "Prefer /hyard:status <job-id>, /hyard:result <job-id>, or /hyard:await <job-id> <timeout-sec> before starting a fresh delegate."
            .to_string(),
    );
    lines.push(
        "Do not restart the same delegate from scratch unless you intentionally want parallel duplicate work."
            .to_string(),
    );
    Some(lines.join("\n"))
}

fn is_active_hyard_job_status(status: &str) -> bool {
    matches!(status, "queued" | "running" | "cancel_requested")
}

fn hyard_job_status_rank(status: &str) -> u8 {
    match status {
        "running" => 3,
        "cancel_requested" => 2,
        "queued" => 1,
        _ => 0,
    }
}

fn compact_hyard_detail(text: &str) -> Option<String> {
    let preview = preview_collapsed(text, 80, "…");
    (!preview.is_empty()).then_some(preview)
}

/// Run a routed turn: execute core, check for delegate, execute peer, finalize core.
///
/// This is the top-level entry point for CLI and TUI. It replaces direct
/// calls to `run_turn()` when orchestration is enabled.
pub async fn run_routed_turn<S: CanonicalStore + ?Sized>(
    store: &mut S,
    session: &mut Session,
    core_provider: &dyn Provider,
    peer_catalog: &PeerCatalog,
    resolve_peer: &dyn Fn(&str) -> Option<Box<dyn Provider>>,
    user_message: String,
    cwd: PathBuf,
) -> Result<RoutedTurnOutput, CoreError> {
    run_routed_turn_with_archive(
        store,
        session,
        core_provider,
        peer_catalog,
        resolve_peer,
        user_message,
        cwd,
        None,
    )
    .await
}

/// Routed turn with optional raw output archiving.
#[allow(clippy::too_many_arguments)]
pub async fn run_routed_turn_with_archive(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    core_provider: &dyn Provider,
    peer_catalog: &PeerCatalog,
    resolve_peer: &dyn Fn(&str) -> Option<Box<dyn Provider>>,
    user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
) -> Result<RoutedTurnOutput, CoreError> {
    run_routed_turn_observable(
        store,
        session,
        core_provider,
        peer_catalog,
        resolve_peer,
        user_message,
        cwd,
        artifact_dir,
        None,
        switchyard_provider_api::CancellationToken::new(),
    )
    .await
}

/// Full observable routed turn. Emits RuntimeEvent through `runtime_tx`
/// so the TUI can display live state without polling the store.
#[allow(clippy::too_many_arguments)]
pub async fn run_routed_turn_observable(
    store: &mut (impl CanonicalStore + ?Sized),
    session: &mut Session,
    core_provider: &dyn Provider,
    peer_catalog: &PeerCatalog,
    resolve_peer: &dyn Fn(&str) -> Option<Box<dyn Provider>>,
    user_message: String,
    cwd: PathBuf,
    artifact_dir: Option<&std::path::Path>,
    runtime_tx: Option<&tokio::sync::mpsc::Sender<crate::runtime_events::RuntimeEvent>>,
    cancel: switchyard_provider_api::CancellationToken,
) -> Result<RoutedTurnOutput, CoreError> {
    let mut last_output: Option<TurnOutput> = None;
    let mut delegated = false;
    let mut inject_context: Option<String> = None;
    let continuation_hint = build_hyard_continuation_hint(&cwd);

    for iteration in 0..MAX_ROUTER_LOOPS {
        let message = if iteration == 0 {
            // First iteration: user message + peer catalog + optional HYARD continuity state.
            build_initial_router_message(&user_message, peer_catalog, continuation_hint.as_deref())
        } else if let Some(ref ctx) = inject_context {
            // Finalization: original task + delegate result + explicit instruction
            build_finalization_router_message(&user_message, ctx, continuation_hint.as_deref())
        } else {
            break;
        };

        let phase = if iteration > 0 {
            crate::turn_runner::TurnPhase::Finalization
        } else {
            crate::turn_runner::TurnPhase::Normal
        };

        // Check cancellation before starting each iteration
        if cancel.is_cancelled() {
            return Err(CoreError::Runner("cancelled by user".to_string()));
        }

        let output = crate::turn_runner::run_turn_phased(
            store,
            session,
            core_provider,
            message,
            cwd.clone(),
            artifact_dir,
            runtime_tx,
            phase,
            cancel.clone(),
        )
        .await?;
        let response_text = output.response.clone().unwrap_or_default();
        let core_turn_id = output.turn_id;
        last_output = Some(output);

        // Check for sentinel delegate block in response
        let sentinel_blocks = extract_sentinel_blocks(&response_text);
        let delegate_request: Option<DelegateRequest> = sentinel_blocks
            .first()
            .and_then(|block| serde_json::from_str::<DelegateRequest>(block).ok())
            .filter(|req| req.request_type == "delegate");

        if let Some(request) = delegate_request {
            if iteration >= MAX_ROUTER_LOOPS - 1 {
                break;
            }

            if request.requests.is_empty() {
                break;
            }

            // V1: take the first task only (ignore extras)
            let task = &request.requests[0];

            if let Some(tx) = runtime_tx {
                tx.send(crate::runtime_events::RuntimeEvent::DelegateRequested {
                    core_turn_id,
                    peer: task.provider.clone(),
                    role: task.role.to_string(),
                    task_summary: preview_chars(&task.task, 80, "…"),
                })
                .await
                .ok();
            }

            let peer = match resolve_peer(&task.provider) {
                Some(p) => p,
                None => {
                    // Peer not available — inject error and let core handle it
                    inject_context = Some(format!(
                        "Delegate to '{}' failed: provider not available.",
                        task.provider
                    ));
                    continue;
                }
            };

            // Build observer to forward peer events as RuntimeEvents
            let peer_name = task.provider.clone();
            let observer: Option<Box<switchyard_orchestrator::PeerEventObserver>> =
                runtime_tx.map(|tx| {
                    let tx = tx.clone();
                    let peer = peer_name.clone();
                    Box::new(move |pe: &switchyard_provider_api::ProviderEvent| {
                        let event = if let Some(execution) =
                            switchyard_provider_api::extract_execution_telemetry(&pe.payload)
                        {
                            Some(
                                crate::runtime_events::RuntimeEvent::PeerExecutionTelemetry {
                                    turn_id: pe.turn_id,
                                    provider: peer.clone(),
                                    execution,
                                },
                            )
                        } else if let Some(job) =
                            switchyard_provider_api::extract_hyard_job_observation(&pe.payload)
                        {
                            Some(crate::runtime_events::RuntimeEvent::HyardJobObserved {
                                source_provider: pe.provider.clone(),
                                observed_at: pe.timestamp.to_rfc3339(),
                                job,
                            })
                        } else if let Some(terminal) =
                            switchyard_provider_api::extract_terminal_output(&pe.payload)
                        {
                            Some(crate::runtime_events::RuntimeEvent::PeerTerminalOutput {
                                turn_id: pe.turn_id,
                                provider: peer.clone(),
                                text: terminal.line,
                                transport: terminal.transport,
                            })
                        } else {
                            match pe.event_type {
                                switchyard_provider_api::EventType::TurnStarted => {
                                    Some(crate::runtime_events::RuntimeEvent::PeerTurnStarted {
                                        turn_id: pe.turn_id,
                                        provider: peer.clone(),
                                    })
                                }
                                switchyard_provider_api::EventType::ItemUpdated => {
                                    pe.display_text_or_summary().map(|text| {
                                        crate::runtime_events::RuntimeEvent::PeerItemUpdated {
                                            turn_id: pe.turn_id,
                                            provider: peer.clone(),
                                            text,
                                        }
                                    })
                                }
                                switchyard_provider_api::EventType::TurnCompleted => {
                                    Some(crate::runtime_events::RuntimeEvent::PeerOutputCompleted {
                                        turn_id: pe.turn_id,
                                        provider: peer.clone(),
                                    })
                                }
                                _ => None,
                            }
                        };
                        if let Some(evt) = event {
                            tx.try_send(evt).ok();
                        }
                    }) as Box<switchyard_orchestrator::PeerEventObserver>
                });
            let obs_ref = observer.as_deref();

            // Execute delegate through orchestrator
            match switchyard_orchestrator::execute_delegate(
                &request,
                session,
                store,
                peer_catalog,
                peer.as_ref(),
                core_turn_id,
                obs_ref,
                cancel.clone(),
            )
            .await
            {
                Ok(response) => {
                    delegated = true;
                    let summary = response.results.first().and_then(|r| r.summary.clone());
                    let status = response
                        .results
                        .first()
                        .map(|r| r.status.to_string())
                        .unwrap_or_default();
                    if let Some(tx) = runtime_tx {
                        tx.send(crate::runtime_events::RuntimeEvent::DelegateCompleted {
                            core_turn_id,
                            peer: task.provider.clone(),
                            status,
                            summary: summary.clone(),
                        })
                        .await
                        .ok();
                    }
                    let result_block = render_delegate_result_block(&response.results);
                    inject_context = Some(result_block);
                }
                Err(e) => {
                    if let Some(tx) = runtime_tx {
                        tx.send(crate::runtime_events::RuntimeEvent::DelegateCompleted {
                            core_turn_id,
                            peer: task.provider.clone(),
                            status: "failed".to_string(),
                            summary: Some(e.to_string()),
                        })
                        .await
                        .ok();
                    }
                    inject_context = Some(format!("Delegate to '{}' failed: {e}", task.provider));
                }
            }
        } else {
            // No delegate — done
            break;
        }
    }

    let output = last_output.ok_or_else(|| CoreError::Runner("no turn executed".to_string()))?;

    // Strip sentinel blocks from the final response so users see clean prose.
    let clean_response = output.response.map(|r| {
        let stripped = strip_sentinel_blocks(&r);
        if stripped.is_empty() { r } else { stripped }
    });

    Ok(RoutedTurnOutput {
        turn_id: output.turn_id,
        response: clean_response,
        delegated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn continuation_hint_reads_active_jobs_and_prioritizes_wait_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let job_dir = dir.path();
        fs::write(
            job_dir.join("completed.json"),
            r#"{
                "job_id": "00000000-0000-0000-0000-000000000001",
                "provider": "gemini",
                "status": "completed",
                "updated_at": "2026-04-04T12:00:00Z"
            }"#,
        )
        .unwrap();
        fs::write(
            job_dir.join("queued.json"),
            r#"{
                "job_id": "00000000-0000-0000-0000-000000000002",
                "provider": "codex",
                "status": "queued",
                "updated_at": "2026-04-04T09:00:00Z"
            }"#,
        )
        .unwrap();
        fs::write(
            job_dir.join("running.json"),
            r#"{
                "job_id": "00000000-0000-0000-0000-000000000003",
                "provider": "claude",
                "status": "running",
                "updated_at": "2026-04-04T11:00:00Z",
                "last_event": "item_updated:claude",
                "wait_timeout_count": 2
            }"#,
        )
        .unwrap();

        let hint = build_hyard_continuation_hint_from_dir(job_dir).unwrap();
        assert!(hint.contains("HYARD continuation hint"));
        assert!(hint.contains("wait_timeout"));
        assert!(hint.contains("00000000-0000-0000-0000-000000000003"));
        assert!(!hint.contains("00000000-0000-0000-0000-000000000001"));

        let claude_pos = hint
            .find("provider=claude job_id=00000000-0000-0000-0000-000000000003")
            .unwrap();
        let codex_pos = hint
            .find("provider=codex job_id=00000000-0000-0000-0000-000000000002")
            .unwrap();
        assert!(
            claude_pos < codex_pos,
            "running wait-timeout job should sort first"
        );
    }

    #[test]
    fn finalization_message_includes_continuation_hint() {
        let message = build_finalization_router_message(
            "original task",
            "delegate_result payload",
            Some("HYARD continuation hint:\n- provider=claude job_id=abc status=running"),
        );

        assert!(message.contains("Original user task:\noriginal task"));
        assert!(message.contains("delegate_result payload"));
        assert!(message.contains("HYARD continuation hint"));
        assert!(message.contains("Do NOT emit another delegate request."));
    }

    #[test]
    fn preview_chars_is_utf8_safe_for_multibyte_delegate_tasks() {
        let text = "这是一次通过 hyard/Switchyard 发起的最小连通性测试。请简短回复以下三项：1）已成功收到任务；2）你的 provider 与 role；3）输出字符串 pong。";
        let truncated = preview_chars(text, 80, "…");

        assert!(truncated.starts_with("这是一次通过"));
        assert!(truncated.ends_with('…'));
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }
}
