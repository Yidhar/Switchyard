//! Tests for observable runtime event pipeline.
//!
//! Verifies that `run_routed_turn_observable` emits the correct sequence of
//! RuntimeEvent variants for plain turns, delegate turns, and peer events.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use switchyard_core::{FakeProvider, RuntimeEvent, run_routed_turn_observable};
use switchyard_provider_api::{
    ArtifactBundle, CancellationToken, ContextBundle, ExecutionPolicy, PeerCatalog, PeerDescriptor,
    ProbeResult, Provider, ProviderError, ProviderEvent, ProviderRole, TurnInput, TurnResult,
};
use switchyard_session::{InboxEntry, InboxStatus, Session};
use switchyard_store::{JsonlStore, SessionInboxRepository, SessionRepository, TurnRepository};

fn temp_store() -> (JsonlStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (JsonlStore::new(dir.path().to_path_buf()), dir)
}

/// Collect RuntimeEvents from a channel into a Vec.
///
/// Some runtime events are forwarded through short-lived async fan-out tasks.
/// Give those tasks a brief chance to flush after the caller drops its sender;
/// otherwise tests that immediately `try_recv` can race a correctly produced
/// event that has not reached the assertion channel yet.
async fn drain_events(rx: &mut mpsc::Receiver<RuntimeEvent>) -> Vec<RuntimeEvent> {
    let mut events = Vec::new();
    while let Ok(Some(evt)) =
        tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv()).await
    {
        events.push(evt);
    }
    events
}

struct RecordingProvider {
    provider_id: String,
    response_text: String,
    inputs: Arc<Mutex<Vec<String>>>,
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
}

impl RecordingProvider {
    fn new(provider_id: &str, response_text: &str) -> Self {
        Self {
            provider_id: provider_id.to_string(),
            response_text: response_text.to_string(),
            inputs: Arc::new(Mutex::new(Vec::new())),
            results: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait::async_trait]
impl Provider for RecordingProvider {
    async fn probe(&self) -> Result<ProbeResult, ProviderError> {
        Ok(ProbeResult {
            version: Some("1.0.0".to_string()),
            available: true,
            capabilities: Default::default(),
            issues: vec![],
            ..Default::default()
        })
    }

    async fn start_turn(
        &self,
        turn_id: Uuid,
        input: TurnInput,
        _policy: ExecutionPolicy,
        _context: ContextBundle,
        event_tx: mpsc::Sender<ProviderEvent>,
        _cancel: CancellationToken,
    ) -> Result<(), ProviderError> {
        self.inputs.lock().await.push(input.user_message.clone());

        event_tx
            .send(ProviderEvent::turn_started(turn_id, &self.provider_id))
            .await
            .ok();
        event_tx
            .send(ProviderEvent::text_message(
                turn_id,
                &self.provider_id,
                &self.response_text,
            ))
            .await
            .ok();
        event_tx
            .send(ProviderEvent::turn_completed(turn_id, &self.provider_id))
            .await
            .ok();

        self.results.lock().await.insert(
            turn_id,
            (
                TurnResult {
                    response_text: self.response_text.clone(),
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

// ── Plain turn (no delegation) ──

#[tokio::test]
async fn plain_turn_emits_core_started_and_completed() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let provider = FakeProvider::success("hello");
    let catalog = PeerCatalog::new();
    let (tx, mut rx) = mpsc::channel::<RuntimeEvent>(64);

    let output = run_routed_turn_observable(
        &mut store,
        &mut session,
        &provider,
        &catalog,
        &|_| None,
        None,
        "test".to_string(),
        PathBuf::from("."),
        None,
        Some(&tx),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    drop(tx); // close channel so drain works
    let events = drain_events(&mut rx).await;

    assert!(!output.delegated);
    assert_eq!(output.response.as_deref(), Some("hello"));

    // Should have: CoreTurnStarted, (CoreItemUpdated)*, TurnCompleted
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::CoreTurnStarted { .. })),
        "missing CoreTurnStarted"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::TurnCompleted { .. })),
        "missing TurnCompleted"
    );

    // CoreTurnStarted must come before TurnCompleted
    let start_idx = events
        .iter()
        .position(|e| matches!(e, RuntimeEvent::CoreTurnStarted { .. }))
        .unwrap();
    let end_idx = events
        .iter()
        .position(|e| matches!(e, RuntimeEvent::TurnCompleted { .. }))
        .unwrap();
    assert!(
        start_idx < end_idx,
        "CoreTurnStarted must precede TurnCompleted"
    );
}

#[tokio::test]
async fn plain_turn_failure_emits_turn_failed() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let provider = FakeProvider::failure("boom");
    let catalog = PeerCatalog::new();
    let (tx, mut rx) = mpsc::channel::<RuntimeEvent>(64);

    let err = match run_routed_turn_observable(
        &mut store,
        &mut session,
        &provider,
        &catalog,
        &|_| None,
        None,
        "test".to_string(),
        PathBuf::from("."),
        None,
        Some(&tx),
        CancellationToken::new(),
    )
    .await
    {
        Ok(_) => panic!("router should surface the failed turn as an error"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("boom"));

    drop(tx);
    let events = drain_events(&mut rx).await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, RuntimeEvent::TurnFailed { .. })),
        "missing TurnFailed"
    );
}

#[tokio::test]
async fn core_item_updated_emitted_for_streaming_text() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let provider = FakeProvider::success("streamed text");
    let catalog = PeerCatalog::new();
    let (tx, mut rx) = mpsc::channel::<RuntimeEvent>(64);

    run_routed_turn_observable(
        &mut store,
        &mut session,
        &provider,
        &catalog,
        &|_| None,
        None,
        "stream test".to_string(),
        PathBuf::from("."),
        None,
        Some(&tx),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    drop(tx);
    let events = drain_events(&mut rx).await;

    let item_updates: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, RuntimeEvent::CoreItemUpdated { .. }))
        .collect();
    assert!(
        !item_updates.is_empty(),
        "should have at least one CoreItemUpdated"
    );
}

#[tokio::test]
async fn unread_callback_receipts_are_injected_without_polluting_stored_user_turn() {
    let (mut store, dir) = temp_store();
    let mut session = Session::new("recorder".to_string());
    store.save_session(&session).unwrap();

    let job_id = Uuid::now_v7();
    let mut entry = InboxEntry::background_job_receipt(
        session.session_id,
        "claude",
        "Claude background job completed",
        "Claude finished a review while you were idle.",
    );
    entry.job_id = Some(job_id);
    entry.summary = Some("Found one follow-up item.".to_string());
    entry.payload = serde_json::json!({ "job_status": "completed" });
    store.save_inbox_entry(&entry).unwrap();

    let provider = RecordingProvider::new("recorder", "acknowledged");
    let captured_inputs = provider.inputs.clone();
    let catalog = PeerCatalog::new();
    let (tx, mut rx) = mpsc::channel::<RuntimeEvent>(64);

    let output = run_routed_turn_observable(
        &mut store,
        &mut session,
        &provider,
        &catalog,
        &|_| None,
        None,
        "please continue".to_string(),
        dir.path().to_path_buf(),
        None,
        Some(&tx),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    drop(tx);
    let events = drain_events(&mut rx).await;

    assert_eq!(output.response.as_deref(), Some("acknowledged"));

    let provider_inputs = captured_inputs.lock().await.clone();
    assert_eq!(provider_inputs.len(), 1);
    let provider_message = &provider_inputs[0];
    assert!(provider_message.contains("please continue"));
    assert!(provider_message.contains(&session.session_id.to_string()));
    assert!(provider_message.contains("/hyard:delegate"));
    assert!(provider_message.contains("--session"));
    assert!(provider_message.contains("/hyard:follow"));
    assert!(provider_message.contains("BACKGROUND COMPLETION NOTICES"));
    assert!(provider_message.contains("runtime callback receipts"));
    assert!(provider_message.contains(&job_id.to_string()));

    let callback_idx = events
        .iter()
        .position(|e| matches!(e, RuntimeEvent::CallbackReceiptsInjected { count: 1, .. }))
        .expect("callback injection event should be emitted");
    let core_start_idx = events
        .iter()
        .position(|e| matches!(e, RuntimeEvent::CoreTurnStarted { .. }))
        .expect("core turn should start");
    assert!(
        callback_idx < core_start_idx,
        "callback injection should be announced before the core turn starts"
    );

    let turns = store.list_turns(session.session_id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].user_message, "please continue");
    assert!(
        !turns[0]
            .user_message
            .contains("BACKGROUND COMPLETION NOTICES")
    );

    let inbox = store.list_inbox_entries(session.session_id).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, InboxStatus::Consumed);
}

#[tokio::test]
async fn callback_receipts_are_rolled_back_to_unread_when_finalization_fails() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("delegator".to_string());
    store.save_session(&session).unwrap();

    let mut entry = InboxEntry::background_job_receipt(
        session.session_id,
        "claude",
        "Claude background job completed",
        "Claude finished while you were idle.",
    );
    entry.payload = serde_json::json!({ "job_status": "completed" });
    store.save_inbox_entry(&entry).unwrap();

    let core = FinalizationFailProvider::new("reviewer");
    let catalog = make_catalog("reviewer");
    let (tx, _rx) = mpsc::channel::<RuntimeEvent>(64);

    let err = match run_routed_turn_observable(
        &mut store,
        &mut session,
        &core,
        &catalog,
        &|name| {
            if name == "reviewer" {
                Some(Box::new(FakeProvider::success("Looks good.")))
            } else {
                None
            }
        },
        None,
        "resume after callback".to_string(),
        PathBuf::from("."),
        None,
        Some(&tx),
        CancellationToken::new(),
    )
    .await
    {
        Ok(_) => panic!("router should surface the failed turn as an error"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("finalization failed"));

    let inbox = store.list_inbox_entries(session.session_id).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, InboxStatus::Unread);
}

// ── Delegation flow ──

/// Provider that emits a delegate request via sentinel block.
struct DelegatingProvider {
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
    delegate_to: String,
}

impl DelegatingProvider {
    fn new(delegate_to: &str) -> Self {
        Self {
            results: Arc::new(Mutex::new(HashMap::new())),
            delegate_to: delegate_to.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl Provider for DelegatingProvider {
    async fn probe(&self) -> Result<ProbeResult, ProviderError> {
        Ok(ProbeResult {
            version: Some("1.0.0".to_string()),
            available: true,
            capabilities: Default::default(),
            issues: vec![],
            ..Default::default()
        })
    }

    async fn start_turn(
        &self,
        turn_id: Uuid,
        input: TurnInput,
        _policy: ExecutionPolicy,
        _context: ContextBundle,
        event_tx: mpsc::Sender<ProviderEvent>,
        _cancel: CancellationToken,
    ) -> Result<(), ProviderError> {
        event_tx
            .send(ProviderEvent::turn_started(turn_id, "delegator"))
            .await
            .ok();

        let response = if input.user_message.contains("delegate_result") {
            "Final answer after delegation.".to_string()
        } else {
            format!(
                "Delegating.\n\n\
                 <<<SWITCHYARD_JSON_BEGIN>>>\n\
                 {{\"type\":\"delegate\",\"requests\":[{{\
                   \"id\":\"t1\",\"provider\":\"{}\",\"role\":\"reviewer\",\
                   \"task\":\"Review code\",\"write_access\":false,\"timeout_sec\":60\
                 }}]}}\n\
                 <<<SWITCHYARD_JSON_END>>>",
                self.delegate_to
            )
        };

        event_tx
            .send(ProviderEvent::text_message(turn_id, "delegator", &response))
            .await
            .ok();
        event_tx
            .send(ProviderEvent::turn_completed(turn_id, "delegator"))
            .await
            .ok();

        self.results.lock().await.insert(
            turn_id,
            (
                TurnResult {
                    response_text: response,
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

/// Provider that delegates on the first turn and fails during finalization.
struct FinalizationFailProvider {
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
    delegate_to: String,
}

impl FinalizationFailProvider {
    fn new(delegate_to: &str) -> Self {
        Self {
            results: Arc::new(Mutex::new(HashMap::new())),
            delegate_to: delegate_to.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl Provider for FinalizationFailProvider {
    async fn probe(&self) -> Result<ProbeResult, ProviderError> {
        Ok(ProbeResult {
            version: Some("1.0.0".to_string()),
            available: true,
            capabilities: Default::default(),
            issues: vec![],
            ..Default::default()
        })
    }

    async fn start_turn(
        &self,
        turn_id: Uuid,
        input: TurnInput,
        _policy: ExecutionPolicy,
        _context: ContextBundle,
        event_tx: mpsc::Sender<ProviderEvent>,
        _cancel: CancellationToken,
    ) -> Result<(), ProviderError> {
        event_tx
            .send(ProviderEvent::turn_started(turn_id, "delegator"))
            .await
            .ok();

        if input.user_message.contains("delegate_result") {
            let error = "finalization failed";
            event_tx
                .send(ProviderEvent::turn_failed(turn_id, "delegator", error))
                .await
                .ok();

            self.results.lock().await.insert(
                turn_id,
                (
                    TurnResult {
                        response_text: String::new(),
                        exit_code: Some(1),
                        stderr: Some(error.to_string()),
                        metadata: HashMap::new(),
                    },
                    ArtifactBundle { artifacts: vec![] },
                ),
            );
            return Ok(());
        }

        let response = format!(
            "Delegating.\n\n\
             <<<SWITCHYARD_JSON_BEGIN>>>\n\
             {{\"type\":\"delegate\",\"requests\":[{{\
               \"id\":\"t1\",\"provider\":\"{}\",\"role\":\"reviewer\",\
               \"task\":\"Review code\",\"write_access\":false,\"timeout_sec\":60\
             }}]}}\n\
             <<<SWITCHYARD_JSON_END>>>",
            self.delegate_to
        );

        event_tx
            .send(ProviderEvent::text_message(turn_id, "delegator", &response))
            .await
            .ok();
        event_tx
            .send(ProviderEvent::turn_completed(turn_id, "delegator"))
            .await
            .ok();

        self.results.lock().await.insert(
            turn_id,
            (
                TurnResult {
                    response_text: response,
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

fn make_catalog(peer_name: &str) -> PeerCatalog {
    let mut catalog = PeerCatalog::new();
    catalog.add(PeerDescriptor {
        provider_id: peer_name.to_string(),
        roles: vec![ProviderRole::Reviewer],
        available: true,
        capabilities: vec![],
        description: "test peer".to_string(),
        host_surface: None,
    });
    catalog
}

#[tokio::test]
async fn delegate_emits_delegate_requested_and_completed() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("delegator".to_string());
    store.save_session(&session).unwrap();

    let core = DelegatingProvider::new("reviewer");
    let catalog = make_catalog("reviewer");
    let (tx, mut rx) = mpsc::channel::<RuntimeEvent>(64);

    let output = run_routed_turn_observable(
        &mut store,
        &mut session,
        &core,
        &catalog,
        &|name| {
            if name == "reviewer" {
                Some(Box::new(FakeProvider::success("Looks good.")))
            } else {
                None
            }
        },
        None,
        "review auth".to_string(),
        PathBuf::from("."),
        None,
        Some(&tx),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    drop(tx);
    let events = drain_events(&mut rx).await;

    assert!(output.delegated);

    // Must have DelegateRequested
    let delegate_req = events
        .iter()
        .find(|e| matches!(e, RuntimeEvent::DelegateRequested { .. }));
    assert!(delegate_req.is_some(), "missing DelegateRequested");
    if let Some(RuntimeEvent::DelegateRequested { peer, .. }) = delegate_req {
        assert_eq!(peer, "t1");
    }

    // Must have DelegateCompleted
    let delegate_done = events
        .iter()
        .find(|e| matches!(e, RuntimeEvent::DelegateCompleted { .. }));
    assert!(delegate_done.is_some(), "missing DelegateCompleted");

    // DelegateRequested must precede DelegateCompleted
    let req_idx = events
        .iter()
        .position(|e| matches!(e, RuntimeEvent::DelegateRequested { .. }))
        .unwrap();
    let done_idx = events
        .iter()
        .position(|e| matches!(e, RuntimeEvent::DelegateCompleted { .. }))
        .unwrap();
    assert!(
        req_idx < done_idx,
        "DelegateRequested must precede DelegateCompleted"
    );
}

#[tokio::test]
async fn delegate_emits_peer_turn_started_and_item_updated() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("delegator".to_string());
    store.save_session(&session).unwrap();

    let core = DelegatingProvider::new("worker");
    let catalog = make_catalog("worker");
    let (tx, mut rx) = mpsc::channel::<RuntimeEvent>(64);

    run_routed_turn_observable(
        &mut store,
        &mut session,
        &core,
        &catalog,
        &|name| {
            if name == "worker" {
                Some(Box::new(FakeProvider::success("Done with review.")))
            } else {
                None
            }
        },
        None,
        "do work".to_string(),
        PathBuf::from("."),
        None,
        Some(&tx),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    drop(tx);
    let events = drain_events(&mut rx).await;

    // Must have PeerTurnStarted from the orchestrator observer
    let peer_started = events
        .iter()
        .find(|e| matches!(e, RuntimeEvent::PeerTurnStarted { .. }));
    assert!(
        peer_started.is_some(),
        "missing PeerTurnStarted — orchestrator observer should emit it"
    );

    // Must have PeerItemUpdated from the orchestrator observer
    let peer_updated = events
        .iter()
        .find(|e| matches!(e, RuntimeEvent::PeerItemUpdated { .. }));
    assert!(
        peer_updated.is_some(),
        "missing PeerItemUpdated — orchestrator observer should emit it"
    );
}

#[tokio::test]
async fn delegate_emits_finalization_started_with_real_turn_id() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("delegator".to_string());
    store.save_session(&session).unwrap();

    let core = DelegatingProvider::new("reviewer");
    let catalog = make_catalog("reviewer");
    let (tx, mut rx) = mpsc::channel::<RuntimeEvent>(64);

    run_routed_turn_observable(
        &mut store,
        &mut session,
        &core,
        &catalog,
        &|name| {
            if name == "reviewer" {
                Some(Box::new(FakeProvider::success("OK")))
            } else {
                None
            }
        },
        None,
        "review".to_string(),
        PathBuf::from("."),
        None,
        Some(&tx),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    drop(tx);
    let events = drain_events(&mut rx).await;

    // Must have FinalizationStarted (iteration > 0)
    let finalization = events
        .iter()
        .find(|e| matches!(e, RuntimeEvent::FinalizationStarted { .. }));
    assert!(finalization.is_some(), "missing FinalizationStarted");

    // turn_id must not be nil (P0-4 fix)
    if let Some(RuntimeEvent::FinalizationStarted { turn_id, .. }) = finalization {
        assert_ne!(
            *turn_id,
            Uuid::nil(),
            "FinalizationStarted must have a real turn_id"
        );
    }
}

#[tokio::test]
async fn full_delegation_event_order() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("delegator".to_string());
    store.save_session(&session).unwrap();

    let core = DelegatingProvider::new("peer");
    let catalog = make_catalog("peer");
    let (tx, mut rx) = mpsc::channel::<RuntimeEvent>(64);

    run_routed_turn_observable(
        &mut store,
        &mut session,
        &core,
        &catalog,
        &|name| {
            if name == "peer" {
                Some(Box::new(FakeProvider::success("peer result")))
            } else {
                None
            }
        },
        None,
        "task".to_string(),
        PathBuf::from("."),
        None,
        Some(&tx),
        CancellationToken::new(),
    )
    .await
    .unwrap();

    drop(tx);
    let events = drain_events(&mut rx).await;

    // Expected order: CoreTurnStarted, [CoreItemUpdated...], TurnCompleted (core turn 1),
    //                 DelegateRequested, PeerTurnStarted, [PeerItemUpdated...], DelegateCompleted,
    //                 FinalizationStarted, CoreTurnStarted (core turn 2), [CoreItemUpdated...], TurnCompleted
    //
    // We check the relative ordering of the key milestones.

    let names: Vec<&str> = events
        .iter()
        .map(|e| match e {
            RuntimeEvent::CallbackReceiptsInjected { .. } => "CallbackReceiptsInjected",
            RuntimeEvent::TurnPreparing { .. } => "TurnPreparing",
            RuntimeEvent::CoreTurnStarted { .. } => "CoreTurnStarted",
            RuntimeEvent::CoreExecutionTelemetry { .. } => "CoreExecutionTelemetry",
            RuntimeEvent::CoreItemUpdated { .. } => "CoreItemUpdated",
            RuntimeEvent::CoreTerminalOutput { .. } => "CoreTerminalOutput",
            RuntimeEvent::CoreOutputCompleted { .. } => "CoreOutputCompleted",
            RuntimeEvent::DelegateRequested { .. } => "DelegateRequested",
            RuntimeEvent::PeerTurnStarted { .. } => "PeerTurnStarted",
            RuntimeEvent::PeerExecutionTelemetry { .. } => "PeerExecutionTelemetry",
            RuntimeEvent::PeerItemUpdated { .. } => "PeerItemUpdated",
            RuntimeEvent::PeerTerminalOutput { .. } => "PeerTerminalOutput",
            RuntimeEvent::PeerOutputCompleted { .. } => "PeerOutputCompleted",
            RuntimeEvent::DelegateCompleted { .. } => "DelegateCompleted",
            RuntimeEvent::HyardJobObserved { .. } => "HyardJobObserved",
            RuntimeEvent::FinalizationStarted { .. } => "FinalizationStarted",
            RuntimeEvent::TurnCompleted { .. } => "TurnCompleted",
            RuntimeEvent::TurnFailed { .. } => "TurnFailed",
            RuntimeEvent::WorkerSpawned { .. } => "WorkerSpawned",
            RuntimeEvent::WorkerStateChanged { .. } => "WorkerStateChanged",
            RuntimeEvent::WorkerRetrying { .. } => "WorkerRetrying",
            RuntimeEvent::WorkerTerminated { .. } => "WorkerTerminated",
        })
        .collect();

    // Core turn 1 starts first
    let first_core_start = names.iter().position(|n| *n == "CoreTurnStarted").unwrap();
    // Then first TurnCompleted (core turn 1)
    let first_complete = names.iter().position(|n| *n == "TurnCompleted").unwrap();
    // DelegateRequested after first core turn completes
    let delegate_req = names
        .iter()
        .position(|n| *n == "DelegateRequested")
        .unwrap();
    // DelegateCompleted after DelegateRequested
    let delegate_done = names
        .iter()
        .position(|n| *n == "DelegateCompleted")
        .unwrap();
    // FinalizationStarted after DelegateCompleted
    let finalization = names
        .iter()
        .position(|n| *n == "FinalizationStarted")
        .unwrap();
    // Last TurnCompleted (finalization turn)
    let last_complete = names.iter().rposition(|n| *n == "TurnCompleted").unwrap();

    assert!(
        first_core_start < first_complete,
        "CoreTurnStarted < TurnCompleted(1)"
    );
    assert!(
        first_complete < delegate_req,
        "TurnCompleted(1) < DelegateRequested"
    );
    assert!(
        delegate_req < delegate_done,
        "DelegateRequested < DelegateCompleted"
    );
    assert!(
        delegate_done < finalization,
        "DelegateCompleted < FinalizationStarted"
    );
    assert!(
        finalization < last_complete,
        "FinalizationStarted < TurnCompleted(final)"
    );
}
