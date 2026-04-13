//! Orchestration broker: validates delegate requests, executes peer turns,
//! and returns structured results.

mod error;

use std::collections::HashMap;
use std::time::Instant;

use tokio::sync::mpsc;
use uuid::Uuid;

use switchyard_context::ContextComposer;
use switchyard_provider_api::{
    ContextBundle, DelegateRequest, DelegateResponse, DelegateStatus, DelegateTask,
    DelegateTaskResult, ExecutionPolicy, PeerCatalog, Provider, ProviderEvent, TurnInput,
};
use switchyard_session::{Event, EventType, Session, Turn, TurnRole};
use switchyard_store::CanonicalStore;
use tokio_util::sync::CancellationToken;

pub use error::OrchestratorError;

/// Callback for observing peer provider events during delegation.
/// The router provides this to forward events to the TUI runtime channel.
pub type PeerEventObserver = dyn Fn(&ProviderEvent) + Send + Sync;

/// Execute a single delegate request: validate, run peer turn, return result.
///
/// This is the broker's main entry point. It:
/// 1. Validates the request against V1 constraints
/// 2. Resolves the peer provider
/// 3. Composes a bounded context for the peer
/// 4. Executes the peer turn
/// 5. Returns a DelegateResponse with the result
#[allow(clippy::too_many_arguments)]
pub async fn execute_delegate(
    request: &DelegateRequest,
    session: &mut Session,
    store: &mut (impl CanonicalStore + ?Sized),
    peer_catalog: &PeerCatalog,
    peer_provider: &dyn Provider,
    core_turn_id: Uuid,
    observer: Option<&PeerEventObserver>,
    cancel: CancellationToken,
) -> Result<DelegateResponse, OrchestratorError> {
    // V1: exactly one task
    if request.requests.len() != 1 {
        return Err(OrchestratorError::InvalidRequest(format!(
            "V1 only supports 1 delegate task, got {}",
            request.requests.len()
        )));
    }

    let task = &request.requests[0];

    // Validate peer exists and is available
    if !peer_catalog.is_available(&task.provider) {
        return Err(OrchestratorError::PeerUnavailable(task.provider.clone()));
    }

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

    // Execute the peer turn
    let result = execute_peer_turn(
        task,
        session,
        store,
        peer_provider,
        core_turn_id,
        observer,
        cancel,
    )
    .await;

    // Emit delegate_completed or delegate_failed
    match &result {
        Ok(task_result) => {
            let event = Event::new(
                core_turn_id,
                EventType::DelegateCompleted,
                &task.provider,
                serde_json::to_value(task_result).unwrap_or_default(),
            );
            store.append_event(&event)?;
        }
        Err(e) => {
            let fail_result = DelegateTaskResult {
                id: task.id.clone(),
                provider: task.provider.clone(),
                status: DelegateStatus::Failed,
                summary: None,
                changed_files: vec![],
                artifacts: vec![],
                error: Some(e.to_string()),
                exit_code: None,
                duration_ms: None,
            };
            let event = Event::new(
                core_turn_id,
                EventType::DelegateCompleted,
                &task.provider,
                serde_json::to_value(&fail_result).unwrap_or_default(),
            );
            store.append_event(&event)?;
            return Ok(DelegateResponse::new(vec![fail_result]));
        }
    }

    Ok(DelegateResponse::new(vec![result?]))
}

/// Execute a single peer turn.
async fn execute_peer_turn(
    task: &DelegateTask,
    session: &mut Session,
    store: &mut (impl CanonicalStore + ?Sized),
    peer_provider: &dyn Provider,
    _core_turn_id: Uuid,
    observer: Option<&PeerEventObserver>,
    cancel: CancellationToken,
) -> Result<DelegateTaskResult, OrchestratorError> {
    let started_at = Instant::now();

    // Create a delegate Turn in the canonical session
    let peer_turn = Turn::new_delegate(
        session.session_id,
        &task.provider,
        match task.role {
            switchyard_provider_api::ProviderRole::Core => TurnRole::Core,
            switchyard_provider_api::ProviderRole::Worker => TurnRole::Worker,
            switchyard_provider_api::ProviderRole::Reviewer => TurnRole::Reviewer,
            switchyard_provider_api::ProviderRole::Analyst => TurnRole::Analyst,
            _ => TurnRole::Worker,
        },
        &task.task,
        &session.active_core,
    );
    let peer_turn_id = peer_turn.turn_id;
    store.append_turn(&peer_turn)?;

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

    let cwd = task
        .cwd
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let policy = ExecutionPolicy {
        timeout_secs: task.timeout_sec,
        write_access: task.write_access,
        cwd,
        allowed_paths: task.allowed_paths.clone(),
    };

    // Render bounded context so peer has awareness of the session state
    let rendered_context = switchyard_provider_subprocess::render_context_bundle(&context);
    let input = TurnInput {
        user_message: task.task.clone(),
        system_prompt: if rendered_context.is_empty() {
            None
        } else {
            Some(rendered_context)
        },
    };

    // Execute peer turn with event streaming
    let (event_tx, mut event_rx) = mpsc::channel(256);
    let provider_fut =
        peer_provider.start_turn(peer_turn_id, input, policy, context, event_tx, cancel);

    let mut peer_failed = false;
    let provider_result;

    let mut process_peer_event = |pe: &ProviderEvent| -> Result<(), OrchestratorError> {
        if pe.event_type == switchyard_provider_api::EventType::TurnFailed {
            peer_failed = true;
        }
        if let Some(obs) = observer {
            obs(pe);
        }
        let canonical = map_provider_event_to_session(pe);
        store.append_event(&canonical)?;
        Ok(())
    };

    tokio::pin!(provider_fut);
    loop {
        tokio::select! {
            res = &mut provider_fut => {
                provider_result = res;
                break;
            }
            Some(pe) = event_rx.recv() => {
                process_peer_event(&pe)?;
            }
        }
    }

    while let Some(pe) = event_rx.recv().await {
        process_peer_event(&pe)?;
    }

    let duration_ms = started_at.elapsed().as_millis() as u64;

    if let Err(e) = provider_result {
        // Update peer turn as failed
        let mut failed_turn = peer_turn;
        failed_turn.status = switchyard_session::TurnStatus::Failed;
        failed_turn.error_message = Some(e.to_string());
        failed_turn.completed_at = Some(chrono::Utc::now());
        store.append_turn(&failed_turn)?;

        return Ok(DelegateTaskResult {
            id: task.id.clone(),
            provider: task.provider.clone(),
            status: DelegateStatus::Failed,
            summary: None,
            changed_files: vec![],
            artifacts: vec![],
            error: Some(e.to_string()),
            exit_code: None,
            duration_ms: Some(duration_ms),
        });
    }

    // Finalize peer turn
    let (turn_result, artifact_bundle) = peer_provider
        .finalize_turn(peer_turn_id)
        .await
        .map_err(|e| OrchestratorError::PeerExecutionFailed(e.to_string()))?;

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

    // Update peer turn in store
    let mut completed_turn = peer_turn;
    completed_turn.provider_response = Some(turn_result.response_text.clone());
    completed_turn.status = if peer_failed || turn_result.exit_code.is_some_and(|c| c != 0) {
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

    Ok(DelegateTaskResult {
        id: task.id.clone(),
        provider: task.provider.clone(),
        status,
        summary: Some(turn_result.response_text),
        changed_files: vec![],
        artifacts: delegate_artifacts,
        error: turn_result.stderr,
        exit_code: turn_result.exit_code,
        duration_ms: Some(duration_ms),
    })
}

/// Map a ProviderEvent to a canonical session Event.
/// Uses serde round-trip since both EventType enums have identical serialization.
fn map_provider_event_to_session(pe: &ProviderEvent) -> Event {
    let event_type: EventType =
        serde_json::from_value(serde_json::to_value(&pe.event_type).unwrap_or_default())
            .unwrap_or(EventType::ItemUpdated);
    Event::new(pe.turn_id, event_type, &pe.provider, pe.payload.clone())
}
