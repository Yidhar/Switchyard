//! Orchestration broker: validates delegate requests, executes peer turns,
//! and returns structured results.

mod error;

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_context::ContextComposer;
use switchyard_provider_api::{
    ContextBundle, DelegateRequest, DelegateResponse, DelegateStatus, DelegateTask,
    DelegateTaskResult, ExecutionPolicy, PeerCatalog, Provider, ProviderEvent, TurnInput,
    LiveInstanceRegistry, LiveInstance, ProviderRole,
};
use switchyard_session::{Event, EventType, Session, Turn, TurnRole};
use switchyard_store::CanonicalStore;
use tokio_util::sync::CancellationToken;

pub use error::OrchestratorError;

/// Callback for observing peer provider events during delegation.
/// The router provides this to forward events to the TUI runtime channel.
pub type PeerEventObserver = dyn Fn(&ProviderEvent) + Send + Sync;

enum TaskOutcome {
    LiveInstance {
        response_text: String,
        failed: bool,
        duration_ms: u64,
    },
    CliProvider {
        turn_result: switchyard_provider_api::TurnResult,
        artifact_bundle: switchyard_provider_api::ArtifactBundle,
        failed: bool,
        duration_ms: u64,
    },
}

/// Execute a list of delegate tasks in parallel: validate, spawn peer turns concurrently,
/// stream progress events back, and return aggregated results.
#[allow(clippy::too_many_arguments)]
pub async fn execute_delegate(
    request: &DelegateRequest,
    session: &mut Session,
    store: &mut (impl CanonicalStore + ?Sized),
    peer_catalog: &PeerCatalog,
    resolve_peer: &(dyn Fn(&str) -> Option<Box<dyn Provider>> + Send + Sync),
    registry: Option<&dyn LiveInstanceRegistry>,
    core_turn_id: Uuid,
    observer: Option<&PeerEventObserver>,
    cancel: CancellationToken,
) -> Result<DelegateResponse, OrchestratorError> {
    struct RegistryReleaseGuard<'a> {
        registry: Option<&'a dyn LiveInstanceRegistry>,
        checked_out: Vec<(String, std::sync::Arc<tokio::sync::Mutex<dyn LiveInstance>>)>,
    }

    impl<'a> Drop for RegistryReleaseGuard<'a> {
        fn drop(&mut self) {
            if let Some(r) = self.registry {
                for (provider, inst) in self.checked_out.drain(..) {
                    r.release_instance(&provider, inst);
                }
            }
        }
    }

    let mut release_guard = RegistryReleaseGuard {
        registry,
        checked_out: Vec::new(),
    };

    if request.requests.is_empty() {
        return Ok(DelegateResponse::new(vec![]));
    }

    // Validate all peers exist and are available
    for task in &request.requests {
        if !peer_catalog.is_available(&task.provider) {
            return Err(OrchestratorError::PeerUnavailable(task.provider.clone()));
        }
    }

    let mut peer_turns = Vec::new();
    let mut tasks_to_spawn = Vec::new();

    // 1. Sequential Preparation: create turns in store and compose contexts
    for task in &request.requests {
        // Emit delegate_requested event
        let delegate_event = Event::new(
            core_turn_id,
            EventType::DelegateRequested,
            &session.active_core,
            serde_json::json!({
                "delegate_id": task.id,
                "peer": task.provider,
                "role": task.role.to_string(),
                "task": task.task,
            }),
        );
        store.append_event(&delegate_event)?;

        // Create a delegate Turn in the canonical session
        let peer_turn = Turn::new_delegate(
            session.session_id,
            &task.id,
            match task.role {
                ProviderRole::Core => TurnRole::Core,
                ProviderRole::Worker => TurnRole::Worker,
                ProviderRole::Reviewer => TurnRole::Reviewer,
                ProviderRole::Analyst => TurnRole::Analyst,
                _ => TurnRole::Worker,
            },
            &task.task,
            &session.active_core,
        );
        let peer_turn_id = peer_turn.turn_id;
        store.append_turn(&peer_turn)?;
        peer_turns.push(peer_turn);

        // Compose bounded context for peer
        let composer = ContextComposer::new(3); // Small window for peer
        let all_turns = store.list_turns(session.session_id).unwrap_or_default();
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
            summary: composed.summary,
            recent_turns: composed
                .recent_turns
                .iter()
                .map(|t| serde_json::to_value(t).unwrap_or_default())
                .collect(),
            peer_state: vec![],
            artifacts: composed
                .relevant_artifacts
                .iter()
                .map(|a| serde_json::to_value(a).unwrap_or_default())
                .collect(),
        };

        // Resolve peer execution instance
        let registry_instance = registry.and_then(|r| r.checkout_instance(&task.provider));
        if let Some(ref inst) = registry_instance {
            release_guard.checked_out.push((task.provider.clone(), std::sync::Arc::clone(inst)));
        }
        let peer_provider = if registry_instance.is_none() {
            resolve_peer(&task.provider)
        } else {
            None
        };

        if registry_instance.is_none() && peer_provider.is_none() {
            return Err(OrchestratorError::PeerUnavailable(task.provider.clone()));
        }

        tasks_to_spawn.push((task.clone(), peer_turn_id, context, peer_provider, registry_instance));
    }

    // 2. Concurrent Execution Setup
    let (event_tx, mut event_rx) = mpsc::channel(512);
    let mut join_handles = Vec::new();

    for (task, peer_turn_id, context, peer_p, reg_inst) in tasks_to_spawn {
        let tx_clone = event_tx.clone();
        let cancel_clone = cancel.clone();
        let handle = tokio::spawn(async move {
            run_single_task_io(task, peer_turn_id, context, peer_p, reg_inst, tx_clone, cancel_clone).await
        });
        join_handles.push(handle);
    }

    // Drop original sender so event_rx terminates when all spawned threads drop their cloned senders
    drop(event_tx);

    let mut peer_failed = false;

    // Process streamed events sequentially on the main thread
    while let Some(pe) = event_rx.recv().await {
        if pe.event_type == switchyard_provider_api::EventType::TurnFailed {
            peer_failed = true;
        }
        if let Some(obs) = observer {
            obs(&pe);
        }
        let canonical = map_provider_event_to_session(&pe);
        store.append_event(&canonical)?;
    }

    // 3. Harvest parallel outcomes
    let mut outcomes = Vec::new();
    for handle in join_handles {
        let outcome = handle.await
            .map_err(|e| OrchestratorError::PeerExecutionFailed(format!("Task task panic: {e}")))?;
        outcomes.push(outcome?);
    }

    // 4. Sequential Finalization: save artifacts & finalized turns back to store
    let mut task_results = Vec::new();
    for ((task, outcome), peer_turn) in std::iter::zip(std::iter::zip(request.requests.clone(), outcomes), peer_turns) {
        let mut completed_turn = peer_turn;
        let peer_turn_id = completed_turn.turn_id;

        match outcome {
            TaskOutcome::LiveInstance { response_text, failed, duration_ms } => {
                completed_turn.provider_response = Some(response_text.clone());
                completed_turn.status = if failed || peer_failed {
                    switchyard_session::TurnStatus::Failed
                } else {
                    switchyard_session::TurnStatus::Completed
                };
                completed_turn.completed_at = Some(chrono::Utc::now());
                store.append_turn(&completed_turn)?;

                let status = if completed_turn.status == switchyard_session::TurnStatus::Completed {
                    DelegateStatus::Success
                } else {
                    DelegateStatus::Failed
                };

                let res = DelegateTaskResult {
                    id: task.id.clone(),
                    provider: task.provider.clone(),
                    status,
                    summary: Some(response_text),
                    changed_files: vec![],
                    artifacts: vec![],
                    error: None,
                    exit_code: Some(0),
                    duration_ms: Some(duration_ms),
                };

                // Emit delegate completed event
                let event = Event::new(
                    core_turn_id,
                    EventType::DelegateCompleted,
                    &task.provider,
                    serde_json::to_value(&res).unwrap_or_default(),
                );
                store.append_event(&event)?;
                task_results.push(res);
            }
            TaskOutcome::CliProvider { turn_result, artifact_bundle, failed, duration_ms } => {
                // Store peer artifacts in canonical session
                for entry in &artifact_bundle.artifacts {
                    let artifact_type = serde_json::from_value::<switchyard_session::ArtifactType>(
                        serde_json::Value::String(entry.artifact_type.clone()),
                    )
                    .unwrap_or(switchyard_session::ArtifactType::RawProviderOutput);

                    let mut artifact =
                        switchyard_session::Artifact::new(peer_turn_id, artifact_type, &entry.title);
                    artifact.summary = entry.summary.clone();
                    artifact.path = entry.path.clone();
                    artifact.metadata = entry.metadata.clone();
                    store.save_artifact(&artifact)?;
                }

                completed_turn.provider_response = Some(turn_result.response_text.clone());
                completed_turn.status = if failed || peer_failed || turn_result.exit_code.is_some_and(|c| c != 0) {
                    switchyard_session::TurnStatus::Failed
                } else {
                    switchyard_session::TurnStatus::Completed
                };
                completed_turn.error_message = turn_result.stderr.clone();
                completed_turn.completed_at = Some(chrono::Utc::now());
                store.append_turn(&completed_turn)?;

                let status = if completed_turn.status == switchyard_session::TurnStatus::Completed {
                    DelegateStatus::Success
                } else {
                    DelegateStatus::Failed
                };

                let delegate_artifacts: Vec<HashMap<String, serde_json::Value>> = artifact_bundle
                    .artifacts
                    .iter()
                    .map(|e| {
                        let mut m = HashMap::new();
                        m.insert(
                            "artifact_type".into(),
                            serde_json::Value::String(e.artifact_type.clone()),
                        );
                        m.insert("title".into(), serde_json::Value::String(e.title.clone()));
                        if let Some(ref s) = e.summary {
                            m.insert("summary".into(), serde_json::Value::String(s.clone()));
                        }
                        if let Some(ref p) = e.path {
                            m.insert(
                                "path".into(),
                                serde_json::Value::String(p.to_string_lossy().to_string()),
                            );
                        }
                        m
                    })
                    .collect();

                let res = DelegateTaskResult {
                    id: task.id.clone(),
                    provider: task.provider.clone(),
                    status,
                    summary: Some(turn_result.response_text),
                    changed_files: vec![],
                    artifacts: delegate_artifacts,
                    error: turn_result.stderr,
                    exit_code: turn_result.exit_code,
                    duration_ms: Some(duration_ms),
                };

                let event = Event::new(
                    core_turn_id,
                    EventType::DelegateCompleted,
                    &task.provider,
                    serde_json::to_value(&res).unwrap_or_default(),
                );
                store.append_event(&event)?;
                task_results.push(res);
            }
        }
    }

    Ok(DelegateResponse::new(task_results))
}

/// Execute an individual task's I/O asynchronously (live instance IPC or CLI subprocess).
async fn run_single_task_io(
    task: DelegateTask,
    peer_turn_id: Uuid,
    context: ContextBundle,
    peer_provider: Option<Box<dyn Provider>>,
    registry_instance: Option<std::sync::Arc<tokio::sync::Mutex<dyn LiveInstance>>>,
    event_tx: mpsc::Sender<ProviderEvent>,
    cancel: CancellationToken,
) -> Result<TaskOutcome, OrchestratorError> {
    let started_at = Instant::now();

    if let Some(inst_lock) = registry_instance {
        let mut inst = inst_lock.lock().await;
        if let Err(e) = inst.update_context(context).await {
            return Err(OrchestratorError::PeerExecutionFailed(format!(
                "Failed to sync context to persistent instance: {e}"
            )));
        }

        let mut event_rx = match inst.send_message(&task.task).await {
            Ok(rx) => rx,
            Err(e) => {
                return Err(OrchestratorError::PeerExecutionFailed(format!(
                    "Failed to delegate to persistent instance: {e}"
                )));
            }
        };
        drop(inst); // Unlock early

        let mut peer_failed = false;
        let mut response_text = String::new();

        while let Some(mut pe) = event_rx.recv().await {
            pe.provider = task.id.clone();
            pe.turn_id = peer_turn_id;
            if pe.event_type == switchyard_provider_api::EventType::TurnFailed {
                peer_failed = true;
            }
            if let Some(text) = pe.payload.get("text").and_then(|t| t.as_str()) {
                response_text.push_str(text);
            } else if let Some(result_text) = pe.payload.get("result").and_then(|r| r.as_str()) {
                response_text.push_str(result_text);
            }
            if event_tx.send(pe).await.is_err() {
                break;
            }
        }

        let duration_ms = started_at.elapsed().as_millis() as u64;
        Ok(TaskOutcome::LiveInstance {
            response_text,
            failed: peer_failed,
            duration_ms,
        })
    } else if let Some(peer_p) = peer_provider {
        let cwd = task.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
        let policy = ExecutionPolicy {
            timeout_secs: task.timeout_sec,
            write_access: task.write_access,
            cwd,
            allowed_paths: task.allowed_paths.clone(),
        };

        let rendered_context = switchyard_provider_subprocess::render_context_bundle(&context);
        let input = TurnInput {
            user_message: task.task.clone(),
            system_prompt: if rendered_context.is_empty() {
                None
            } else {
                Some(rendered_context)
            },
        };

        let (task_event_tx, mut task_event_rx) = mpsc::channel(256);
        let provider_fut = peer_p.start_turn(peer_turn_id, input, policy, context, task_event_tx, cancel);

        let mut peer_failed = false;
        tokio::pin!(provider_fut);
        let provider_result = loop {
            tokio::select! {
                res = &mut provider_fut => {
                    break res;
                }
                Some(mut pe) = task_event_rx.recv() => {
                    pe.provider = task.id.clone();
                    if pe.event_type == switchyard_provider_api::EventType::TurnFailed {
                        peer_failed = true;
                    }
                    let _ = event_tx.send(pe).await;
                }
            }
        };

        while let Some(mut pe) = task_event_rx.recv().await {
            pe.provider = task.id.clone();
            if pe.event_type == switchyard_provider_api::EventType::TurnFailed {
                peer_failed = true;
            }
            if event_tx.send(pe).await.is_err() {
                break;
            }
        }

        let duration_ms = started_at.elapsed().as_millis() as u64;

        provider_result.map_err(|e| OrchestratorError::PeerExecutionFailed(e.to_string()))?;

        let (turn_result, artifact_bundle) = peer_p
            .finalize_turn(peer_turn_id)
            .await
            .map_err(|e| OrchestratorError::PeerExecutionFailed(e.to_string()))?;

        Ok(TaskOutcome::CliProvider {
            turn_result,
            artifact_bundle,
            failed: peer_failed,
            duration_ms,
        })
    } else {
        Err(OrchestratorError::PeerExecutionFailed(format!(
            "No live instance and no provider resolved for '{}'",
            task.provider
        )))
    }
}

/// Map a ProviderEvent to a canonical session Event.
/// Uses serde round-trip since both EventType enums have identical serialization.
fn map_provider_event_to_session(pe: &ProviderEvent) -> Event {
    let event_type: EventType =
        serde_json::from_value(serde_json::to_value(&pe.event_type).unwrap_or_default())
            .unwrap_or(EventType::ItemUpdated);
    Event::new(pe.turn_id, event_type, &pe.provider, pe.payload.clone())
}

