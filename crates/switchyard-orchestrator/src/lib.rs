//! Orchestration broker: validates delegate requests, executes peer turns,
//! and returns structured results.

mod error;
pub mod supervisor;

pub use supervisor::{RetryPolicy, SpawnRecipe, SupervisedOutcome, WorkerSupervisor};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_context::ContextComposer;
use switchyard_provider_api::{
    ContextBundle, DelegateRequest, DelegateResponse, DelegateStatus, DelegateTask,
    DelegateTaskResult, ExecutionPolicy, LiveInstance, LiveInstanceRegistry, PeerCatalog, Provider,
    ProviderEvent, ProviderRole, TurnInput, is_empty_reasoning_payload,
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
    Supervised {
        response_text: String,
        failed: bool,
        retries_attempted: u32,
        last_error: Option<String>,
        duration_ms: u64,
    },
    CliProvider {
        turn_result: switchyard_provider_api::TurnResult,
        artifact_bundle: switchyard_provider_api::ArtifactBundle,
        failed: bool,
        duration_ms: u64,
    },
}

/// Per-task routing decision made by `execute_delegate` during the sequential
/// preparation phase. The `Supervised` branch is the default for any peer
/// whose Provider implements `PersistentProvider` and a registry is present;
/// `Legacy` covers pre-registered live instances when no real Provider can be
/// resolved (used by tests that register MockLiveInstance without a CLI
/// behind it); `Cli` is the per-turn subprocess fallback.
enum TaskRoute {
    Supervised {
        provider: Box<dyn Provider>,
        recipe: SpawnRecipe,
    },
    Legacy {
        instance: Arc<tokio::sync::Mutex<dyn LiveInstance>>,
    },
    Cli {
        provider: Box<dyn Provider>,
    },
}

/// Execute a list of delegate tasks in parallel: validate, route per task
/// (supervised / legacy / CLI), spawn peer turns concurrently, stream progress
/// events back, and return aggregated results.
///
/// `retry_policy` is consumed by the WorkerSupervisor for any task routed to
/// the supervised path. `None` falls back to [`RetryPolicy::default`] (3
/// attempts, [2s, 5s, 10s] backoff).
#[allow(clippy::too_many_arguments)]
pub async fn execute_delegate(
    request: &DelegateRequest,
    session: &mut Session,
    store: &mut (impl CanonicalStore + ?Sized),
    peer_catalog: &PeerCatalog,
    resolve_peer: &(dyn Fn(&str) -> Option<Box<dyn Provider>> + Send + Sync),
    registry: Option<Arc<dyn LiveInstanceRegistry>>,
    retry_policy: Option<RetryPolicy>,
    // Per-provider env map for supervised spawns. Looked up by `task.provider`
    // when building the `SpawnRecipe.env`; missing entries default to empty.
    provider_envs: HashMap<String, HashMap<String, String>>,
    supervisor_observer: Option<Arc<crate::supervisor::SupervisorObserver>>,
    core_turn_id: Uuid,
    observer: Option<&PeerEventObserver>,
    cancel: CancellationToken,
) -> Result<DelegateResponse, OrchestratorError> {
    struct RegistryReleaseGuard {
        registry: Option<Arc<dyn LiveInstanceRegistry>>,
        // instance_ids to release; the registry retains the Arc, so we just
        // hand back the id and let the pool transition state to Idle.
        // Only the Legacy route uses this — Supervised manages its own
        // checkout/release inside `WorkerSupervisor::execute`.
        checked_out: Vec<Uuid>,
    }

    impl Drop for RegistryReleaseGuard {
        fn drop(&mut self) {
            if let Some(r) = &self.registry {
                for id in self.checked_out.drain(..) {
                    r.release(id);
                }
            }
        }
    }

    let mut release_guard = RegistryReleaseGuard {
        registry: registry.clone(),
        checked_out: Vec::new(),
    };

    let retry_policy = retry_policy.unwrap_or_default();

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

        // Route this task. Three possible paths:
        //   1. Supervised — peer Provider is persistent AND a registry exists.
        //      WorkerSupervisor handles spawn/checkout/retry/death-notify.
        //   2. Legacy — no Provider can be resolved (tests pre-register a
        //      MockLiveInstance without a real CLI behind it); use whatever's
        //      idle in the pool.
        //   3. Cli — Provider resolves but isn't persistent: per-turn subprocess.
        let peer_provider = resolve_peer(&task.provider);
        let persistent_capable = peer_provider
            .as_ref()
            .and_then(|p| p.as_persistent())
            .is_some();

        let route = if let Some(p) = peer_provider {
            if persistent_capable && registry.is_some() {
                let mut env = provider_envs
                    .get(&task.provider)
                    .cloned()
                    .unwrap_or_default();
                // Stamp Switchyard identity so any hooks the spawned worker
                // CLI fires can route back to this session. Mirrors the
                // injection in switchyard-gui's run_turn pre-spawn block.
                env.insert(
                    "SWITCHYARD_SESSION_ID".to_string(),
                    session.session_id.to_string(),
                );
                env.insert("SWITCHYARD_PROVIDER".to_string(), task.provider.clone());
                // Workers inherit any prior resume_token persisted in
                // the session's native_bindings (same key the GUI uses
                // for Core: `{provider}_resume_token`). Lets the
                // supervisor's auto-retry continue the worker's
                // conversation on respawn instead of starting fresh.
                let resume_key = format!("{}_worker_resume_token", task.provider);
                let resume_token = session.native_bindings.get(&resume_key).cloned();
                let recipe = SpawnRecipe {
                    provider: task.provider.clone(),
                    session_id: session.session_id,
                    label: Some(task.id.clone()),
                    cwd: task.cwd.clone().unwrap_or_else(|| PathBuf::from(".")),
                    env,
                    reuse_after: true,
                    resume_token,
                };
                TaskRoute::Supervised {
                    provider: p,
                    recipe,
                }
            } else {
                TaskRoute::Cli { provider: p }
            }
        } else if let Some(reg) = registry.as_ref()
            && let Some((id, inst)) = reg.checkout_any_idle(&task.provider, session.session_id)
        {
            release_guard.checked_out.push(id);
            TaskRoute::Legacy { instance: inst }
        } else {
            return Err(OrchestratorError::PeerUnavailable(task.provider.clone()));
        };

        tasks_to_spawn.push((task.clone(), peer_turn_id, context, route));
    }

    // 2. Concurrent Execution Setup
    let (event_tx, mut event_rx) = mpsc::channel(512);
    let mut join_handles = Vec::new();

    for (task, peer_turn_id, context, route) in tasks_to_spawn {
        let tx_clone = event_tx.clone();
        let cancel_clone = cancel.clone();
        let registry_clone = registry.clone();
        let retry_policy_clone = retry_policy.clone();
        let supervisor_observer_clone = supervisor_observer.clone();
        let handle = tokio::spawn(async move {
            run_single_task_io(
                task,
                peer_turn_id,
                context,
                route,
                registry_clone,
                retry_policy_clone,
                supervisor_observer_clone,
                tx_clone,
                cancel_clone,
            )
            .await
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
        if is_empty_reasoning_payload(&pe.payload) {
            // Same policy as core turns: these are high-volume reasoning
            // heartbeats with no visible content. Do not send them to observers
            // or persist them, otherwise GUI DB refreshes revive empty cards.
            continue;
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
        let outcome = handle
            .await
            .map_err(|e| OrchestratorError::PeerExecutionFailed(format!("Task task panic: {e}")))?;
        outcomes.push(outcome?);
    }

    // 4. Sequential Finalization: save artifacts & finalized turns back to store
    let mut task_results = Vec::new();
    for ((task, outcome), peer_turn) in std::iter::zip(
        std::iter::zip(request.requests.clone(), outcomes),
        peer_turns,
    ) {
        let mut completed_turn = peer_turn;
        let peer_turn_id = completed_turn.turn_id;

        match outcome {
            TaskOutcome::LiveInstance {
                response_text,
                failed,
                duration_ms,
            } => {
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
            TaskOutcome::Supervised {
                response_text,
                failed,
                retries_attempted,
                last_error,
                duration_ms,
            } => {
                completed_turn.provider_response = Some(response_text.clone());
                completed_turn.status = if failed || peer_failed {
                    switchyard_session::TurnStatus::Failed
                } else {
                    switchyard_session::TurnStatus::Completed
                };
                completed_turn.error_message = last_error.clone();
                completed_turn.completed_at = Some(chrono::Utc::now());
                store.append_turn(&completed_turn)?;

                let status = if completed_turn.status == switchyard_session::TurnStatus::Completed {
                    DelegateStatus::Success
                } else {
                    DelegateStatus::Failed
                };

                // Encode retry telemetry into the delegate_completed payload so
                // the Core (which never sees `delegate_retrying`) can inspect
                // how many recoveries were needed when it inspects the result.
                let mut artifacts_meta = Vec::new();
                if retries_attempted > 0 {
                    let mut entry = HashMap::new();
                    entry.insert(
                        "kind".into(),
                        serde_json::Value::String("supervisor_retry_summary".into()),
                    );
                    entry.insert(
                        "retries_attempted".into(),
                        serde_json::Value::Number(retries_attempted.into()),
                    );
                    if let Some(err) = &last_error {
                        entry.insert("last_error".into(), serde_json::Value::String(err.clone()));
                    }
                    artifacts_meta.push(entry);
                }

                let res = DelegateTaskResult {
                    id: task.id.clone(),
                    provider: task.provider.clone(),
                    status,
                    summary: Some(response_text),
                    changed_files: vec![],
                    artifacts: artifacts_meta,
                    error: last_error,
                    exit_code: if failed { Some(1) } else { Some(0) },
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
            TaskOutcome::CliProvider {
                turn_result,
                artifact_bundle,
                failed,
                duration_ms,
            } => {
                // Store peer artifacts in canonical session
                for entry in &artifact_bundle.artifacts {
                    let artifact_type = serde_json::from_value::<switchyard_session::ArtifactType>(
                        serde_json::Value::String(entry.artifact_type.clone()),
                    )
                    .unwrap_or(switchyard_session::ArtifactType::RawProviderOutput);

                    let mut artifact = switchyard_session::Artifact::new(
                        peer_turn_id,
                        artifact_type,
                        &entry.title,
                    );
                    artifact.summary = entry.summary.clone();
                    artifact.path = entry.path.clone();
                    artifact.metadata = entry.metadata.clone();
                    store.save_artifact(&artifact)?;
                }

                completed_turn.provider_response = Some(turn_result.response_text.clone());
                completed_turn.status =
                    if failed || peer_failed || turn_result.exit_code.is_some_and(|c| c != 0) {
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

/// Execute an individual task's I/O asynchronously. Dispatches on `route`:
/// supervisor-driven for persistent peers, direct LiveInstance for pre-
/// registered test pool entries, CLI subprocess otherwise.
#[allow(clippy::too_many_arguments)]
async fn run_single_task_io(
    task: DelegateTask,
    peer_turn_id: Uuid,
    context: ContextBundle,
    route: TaskRoute,
    registry: Option<Arc<dyn LiveInstanceRegistry>>,
    retry_policy: RetryPolicy,
    supervisor_observer: Option<Arc<crate::supervisor::SupervisorObserver>>,
    event_tx: mpsc::Sender<ProviderEvent>,
    cancel: CancellationToken,
) -> Result<TaskOutcome, OrchestratorError> {
    let started_at = Instant::now();
    let policy = execution_policy_from_delegate_task(&task);

    if let TaskRoute::Supervised { provider, recipe } = route {
        // Supervised path: WorkerSupervisor owns the spawn / retry / death-
        // notify lifecycle. We bridge supervisor events through a stamping
        // forwarder so canonical-session events carry `provider = task.id`
        // (matches the convention used by Legacy and CLI branches).
        let registry = registry.expect("Supervised route implies registry is Some");
        // The supervisor's lifecycle observer is threaded through
        // `execute_delegate` and forwarded into this task; if no observer was
        // provided, the supervisor silently drops its lifecycle events.
        let supervisor = WorkerSupervisor::new(registry, retry_policy, supervisor_observer);

        let (super_tx, mut super_rx) = mpsc::channel::<ProviderEvent>(256);
        let task_id = task.id.clone();
        let event_tx_for_forward = event_tx.clone();

        let supervisor_future = async move {
            supervisor
                .execute(
                    &*provider,
                    recipe,
                    peer_turn_id,
                    task.task.clone(),
                    context,
                    super_tx,
                    policy.clone(),
                    cancel,
                )
                .await
        };

        let forward_future = async move {
            while let Some(mut pe) = super_rx.recv().await {
                pe.provider = task_id.clone();
                if event_tx_for_forward.send(pe).await.is_err() {
                    break;
                }
            }
        };

        let (outcome, _) = tokio::join!(supervisor_future, forward_future);

        let duration_ms = started_at.elapsed().as_millis() as u64;
        return Ok(TaskOutcome::Supervised {
            response_text: outcome.response_text,
            failed: outcome.failed,
            retries_attempted: outcome.retries_attempted,
            last_error: outcome.last_error,
            duration_ms,
        });
    }

    if let TaskRoute::Legacy {
        instance: inst_lock,
    } = route
    {
        let mut inst = inst_lock.lock().await;
        if let Err(e) = inst.update_context(context).await {
            return Err(OrchestratorError::PeerExecutionFailed(format!(
                "Failed to sync context to persistent instance: {e}"
            )));
        }

        let mut event_rx = match inst.send_message_with_policy(&task.task, &policy).await {
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
        return Ok(TaskOutcome::LiveInstance {
            response_text,
            failed: peer_failed,
            duration_ms,
        });
    }

    if let TaskRoute::Cli { provider: peer_p } = route {
        let rendered_context = switchyard_provider_subprocess::render_context_bundle(&context);
        let input = TurnInput {
            user_message: task.task.clone(),
            system_prompt: if rendered_context.is_empty() {
                None
            } else {
                Some(rendered_context)
            },
            attachments: Vec::new(),
        };

        let (task_event_tx, mut task_event_rx) = mpsc::channel(256);
        let provider_fut =
            peer_p.start_turn(peer_turn_id, input, policy, context, task_event_tx, cancel);

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

fn execution_policy_from_delegate_task(task: &DelegateTask) -> ExecutionPolicy {
    let cwd = task.cwd.clone().unwrap_or_else(|| PathBuf::from("."));
    let mut policy = if task.write_access {
        ExecutionPolicy::workspace_write(cwd.clone()).add_allowed_paths(
            task.allowed_paths
                .iter()
                .map(|path| resolve_policy_path(&cwd, path))
                .collect::<Vec<_>>(),
        )
    } else {
        ExecutionPolicy::read_only(cwd)
    };
    policy.timeout_secs = task.timeout_sec;
    policy
}

fn resolve_policy_path(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// Map a ProviderEvent to a canonical session Event.
/// Uses serde round-trip since both EventType enums have identical serialization.
/// The worker identity stamps (`instance_id`, `label`) live on the ProviderEvent
/// struct but the canonical session Event has only `payload` for transport —
/// fold the stamps into payload so they reach the on-disk session log and
/// downstream readers (including a future-restored Core) can attribute events
/// back to the worker that produced them.
fn map_provider_event_to_session(pe: &ProviderEvent) -> Event {
    let event_type: EventType =
        serde_json::from_value(serde_json::to_value(&pe.event_type).unwrap_or_default())
            .unwrap_or(EventType::ItemUpdated);
    let mut payload = pe.payload.clone();
    if pe.instance_id.is_some() || pe.label.is_some() {
        let obj = match payload.as_object_mut() {
            Some(o) => o,
            None => {
                payload = serde_json::Value::Object(serde_json::Map::new());
                payload.as_object_mut().unwrap()
            }
        };
        if let Some(id) = pe.instance_id {
            obj.insert(
                "instance_id".to_string(),
                serde_json::Value::String(id.to_string()),
            );
        }
        if let Some(ref label) = pe.label {
            obj.insert(
                "label".to_string(),
                serde_json::Value::String(label.clone()),
            );
        }
    }
    Event::new(pe.turn_id, event_type, &pe.provider, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchyard_provider_api::{EffectiveSandboxMode, ProviderRole};

    fn delegate_task(write_access: bool) -> DelegateTask {
        delegate_task_with_timeout(write_access, 120)
    }

    fn delegate_task_with_timeout(write_access: bool, timeout_sec: u64) -> DelegateTask {
        DelegateTask {
            id: "task-1".to_string(),
            provider: "codex".to_string(),
            role: ProviderRole::Worker,
            task: "do work".to_string(),
            write_access,
            cwd: Some(PathBuf::from("/project/app")),
            allowed_paths: Vec::new(),
            timeout_sec,
        }
    }

    #[test]
    fn delegate_policy_defaults_to_read_only_when_write_access_false() {
        let task = delegate_task(false);
        let policy = execution_policy_from_delegate_task(&task);

        assert_eq!(policy.timeout_secs, 120);
        assert_eq!(policy.cwd, PathBuf::from("/project/app"));
        assert!(!policy.write_access);
        assert!(policy.allowed_paths.is_empty());
        assert_eq!(
            policy.effective_sandbox_mode(),
            EffectiveSandboxMode::ReadOnly
        );
    }

    #[test]
    fn delegate_policy_write_access_is_workspace_scoped_not_danger() {
        let task = delegate_task(true);
        let policy = execution_policy_from_delegate_task(&task);

        assert_eq!(policy.timeout_secs, 120);
        assert!(policy.write_access);
        assert_eq!(policy.allowed_paths, vec![PathBuf::from("/project/app")]);
        assert_eq!(
            policy.effective_sandbox_mode(),
            EffectiveSandboxMode::WorkspaceWrite
        );
    }

    #[test]
    fn delegate_policy_preserves_zero_timeout_as_no_hard_deadline() {
        let task = delegate_task_with_timeout(false, 0);
        let policy = execution_policy_from_delegate_task(&task);

        assert_eq!(policy.timeout_secs, 0);
    }

    #[test]
    fn delegate_policy_resolves_extra_paths_relative_to_task_cwd() {
        let mut task = delegate_task(true);
        task.allowed_paths = vec![PathBuf::from("../shared"), PathBuf::from("/tmp/cache")];

        let policy = execution_policy_from_delegate_task(&task);

        assert!(
            policy
                .allowed_paths
                .contains(&PathBuf::from("/project/app"))
        );
        assert!(
            policy
                .allowed_paths
                .contains(&PathBuf::from("/project/app/../shared"))
        );
        assert!(policy.allowed_paths.contains(&PathBuf::from("/tmp/cache")));
    }
}
