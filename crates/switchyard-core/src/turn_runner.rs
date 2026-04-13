//! Minimal single-turn runner.

use std::path::PathBuf;

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_context::ContextComposer;
use switchyard_provider_api::{ContextBundle, ExecutionPolicy, Provider, TurnInput};
use switchyard_provider_api::{
    extract_execution_telemetry, extract_hyard_job_observation, extract_terminal_output,
};
use switchyard_session::{Session, Turn, TurnRole, TurnStatus};
use switchyard_store::CanonicalStore;
#[cfg(test)]
use switchyard_store::JsonlStore;

use crate::error::CoreError;
use crate::event_mapper::map_provider_event;

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
    run_turn_phased(
        store,
        session,
        provider,
        user_message,
        cwd,
        artifact_dir,
        runtime_tx,
        TurnPhase::Normal,
        switchyard_provider_api::CancellationToken::new(),
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
    // 1. Create Turn
    let turn = Turn::new(
        session.session_id,
        &session.active_core,
        TurnRole::Core,
        &user_message,
    );
    let turn_id = turn.turn_id;
    store.append_turn(&turn)?;

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

    let policy = ExecutionPolicy {
        // 0 = use the provider's configured default timeout.
        timeout_secs: 0,
        write_access: true,
        cwd,
        allowed_paths: vec![],
    };

    let rendered_context = switchyard_provider_subprocess::render_context_bundle(&context);
    let input = TurnInput {
        user_message,
        system_prompt: if rendered_context.is_empty() {
            None
        } else {
            Some(rendered_context)
        },
    };

    // Bounded channel prevents backpressure deadlock when provider emits faster than we drain.
    let (event_tx, mut event_rx) = mpsc::channel(256);
    let provider_fut =
        provider.start_turn(turn_id, input, policy, context, event_tx, cancel.clone());

    let mut failed = false;
    let mut output_completed = false;
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
        if let Some(tx) = runtime_tx {
            if let Some(execution) = extract_execution_telemetry(&pe.payload) {
                let _ = tx.try_send(
                    crate::runtime_events::RuntimeEvent::CoreExecutionTelemetry {
                        turn_id: pe.turn_id,
                        provider: pe.provider.clone(),
                        execution,
                    },
                );
            }
            if let Some(job) = extract_hyard_job_observation(&pe.payload) {
                let _ = tx.try_send(crate::runtime_events::RuntimeEvent::HyardJobObserved {
                    source_provider: pe.provider.clone(),
                    observed_at: pe.timestamp.to_rfc3339(),
                    job,
                });
            }
            if let Some(terminal) = extract_terminal_output(&pe.payload) {
                let _ = tx.try_send(crate::runtime_events::RuntimeEvent::CoreTerminalOutput {
                    turn_id: pe.turn_id,
                    provider: pe.provider.clone(),
                    text: terminal.line,
                    transport: terminal.transport,
                });
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
            // Streaming items: best-effort (droppable). Never block execution.
            if let Some(text) = pe.display_text_or_summary() {
                let _ = tx.try_send(crate::runtime_events::RuntimeEvent::CoreItemUpdated {
                    turn_id: pe.turn_id,
                    provider: pe.provider.clone(),
                    text,
                });
            }
        }
        let canonical = map_provider_event(pe);
        store.append_event(&canonical)?;
        Ok(())
    }

    // Concurrent select: provider execution + event drain + cancellation
    tokio::pin!(provider_fut);
    let mut cancelled = false;
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
                drain_event(&pe, &mut failed, &mut output_completed, runtime_tx, store).await?;
            }
        }
    }

    // Drain remaining events after provider completes
    while let Some(pe) = event_rx.recv().await {
        drain_event(&pe, &mut failed, &mut output_completed, runtime_tx, store).await?;
    }

    provider_result?;

    // If cancelled, skip expensive finalize/archive — mark as failed and return.
    if cancel.is_cancelled() {
        let mut cancelled_turn = turn;
        cancelled_turn.status = TurnStatus::Failed;
        cancelled_turn.error_message = Some("cancelled".to_string());
        cancelled_turn.completed_at = Some(chrono::Utc::now());
        store.append_turn(&cancelled_turn)?;
        if let Some(tx) = runtime_tx {
            tx.send(crate::runtime_events::RuntimeEvent::TurnFailed {
                turn_id,
                provider: session.active_core.clone(),
                error: "cancelled".to_string(),
            })
            .await
            .ok();
        }
        return Ok(TurnOutput {
            turn_id,
            response: None,
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
    let (result, artifact_bundle) = provider.finalize_turn(turn_id).await?;

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
    updated_turn.provider_response = Some(result.response_text.clone());
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

    session.updated_at = chrono::Utc::now();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_provider::FakeProvider;
    use switchyard_session::EventType;
    use switchyard_store::{ArtifactStore, EventLog, SessionRepository, TurnRepository};

    fn temp_store() -> (JsonlStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (JsonlStore::new(dir.path().to_path_buf()), dir)
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

        let artifacts = store.list_artifacts(turn_id).unwrap();
        assert_eq!(artifacts.len(), 1);
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
}
