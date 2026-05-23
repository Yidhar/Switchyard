//! Tests for completion latency: CoreOutputCompleted arrives before TurnCompleted,
//! streaming events don't block milestone delivery, and TUI phase transitions are correct.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use switchyard_core::{RuntimeEvent, run_turn_full};
use switchyard_provider_api::*;
use switchyard_session::Session;
use switchyard_store::{JsonlStore, SessionRepository};

fn temp_store() -> (JsonlStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (JsonlStore::new(dir.path().to_path_buf()), dir)
}

async fn drain_events(rx: &mut mpsc::Receiver<RuntimeEvent>) -> Vec<RuntimeEvent> {
    let mut events = Vec::new();
    while let Ok(evt) = rx.try_recv() {
        events.push(evt);
    }
    events
}

fn event_name(e: &RuntimeEvent) -> &'static str {
    match e {
        RuntimeEvent::CallbackReceiptsInjected { .. } => "CallbackReceiptsInjected",
        RuntimeEvent::CoreTurnStarted { .. } => "CoreTurnStarted",
        RuntimeEvent::CoreExecutionTelemetry { .. } => "CoreExecutionTelemetry",
        RuntimeEvent::CoreItemUpdated { .. } => "CoreItemUpdated",
        RuntimeEvent::CoreTerminalOutput { .. } => "CoreTerminalOutput",
        RuntimeEvent::CoreOutputCompleted { .. } => "CoreOutputCompleted",
        RuntimeEvent::PeerTurnStarted { .. } => "PeerTurnStarted",
        RuntimeEvent::PeerExecutionTelemetry { .. } => "PeerExecutionTelemetry",
        RuntimeEvent::PeerItemUpdated { .. } => "PeerItemUpdated",
        RuntimeEvent::PeerTerminalOutput { .. } => "PeerTerminalOutput",
        RuntimeEvent::PeerOutputCompleted { .. } => "PeerOutputCompleted",
        RuntimeEvent::DelegateRequested { .. } => "DelegateRequested",
        RuntimeEvent::DelegateCompleted { .. } => "DelegateCompleted",
        RuntimeEvent::HyardJobObserved { .. } => "HyardJobObserved",
        RuntimeEvent::FinalizationStarted { .. } => "FinalizationStarted",
        RuntimeEvent::TurnCompleted { .. } => "TurnCompleted",
        RuntimeEvent::TurnFailed { .. } => "TurnFailed",
        RuntimeEvent::WorkerSpawned { .. } => "WorkerSpawned",
        RuntimeEvent::WorkerStateChanged { .. } => "WorkerStateChanged",
        RuntimeEvent::WorkerRetrying { .. } => "WorkerRetrying",
        RuntimeEvent::WorkerTerminated { .. } => "WorkerTerminated",
    }
}

// ── Test A: Output completes before turn completes (slow finalize) ──

/// Provider that finishes start_turn quickly but sleeps in finalize_turn.
struct SlowFinalizeProvider {
    finalize_delay: Duration,
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
}

impl SlowFinalizeProvider {
    fn new(delay: Duration) -> Self {
        Self {
            finalize_delay: delay,
            results: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

#[async_trait::async_trait]
impl Provider for SlowFinalizeProvider {
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
            .send(ProviderEvent::turn_started(turn_id, "slow"))
            .await
            .ok();
        event_tx
            .send(ProviderEvent::text_message(turn_id, "slow", "done"))
            .await
            .ok();
        event_tx
            .send(ProviderEvent::turn_completed(turn_id, "slow"))
            .await
            .ok();

        self.results.lock().await.insert(
            turn_id,
            (
                TurnResult {
                    response_text: "done".into(),
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
        // Simulate slow finalization (archive, save, etc.)
        tokio::time::sleep(self.finalize_delay).await;
        self.results
            .lock()
            .await
            .remove(&turn_id)
            .ok_or(ProviderError::ExecutionFailed("no result".into()))
    }
}

#[tokio::test]
async fn output_completed_arrives_before_turn_completed() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("slow".to_string());
    store.save_session(&session).unwrap();

    let provider = SlowFinalizeProvider::new(Duration::from_millis(200));
    let (tx, mut rx) = mpsc::channel::<RuntimeEvent>(64);

    run_turn_full(
        &mut store,
        &mut session,
        &provider,
        "test".to_string(),
        PathBuf::from("."),
        None,
        Some(&tx),
    )
    .await
    .unwrap();

    drop(tx);
    let events = drain_events(&mut rx).await;
    let names: Vec<&str> = events.iter().map(event_name).collect();

    // CoreOutputCompleted must exist and precede TurnCompleted
    let output_idx = names.iter().position(|n| *n == "CoreOutputCompleted");
    let complete_idx = names.iter().position(|n| *n == "TurnCompleted");

    assert!(
        output_idx.is_some(),
        "missing CoreOutputCompleted. events: {names:?}"
    );
    assert!(
        complete_idx.is_some(),
        "missing TurnCompleted. events: {names:?}"
    );
    assert!(
        output_idx.unwrap() < complete_idx.unwrap(),
        "CoreOutputCompleted must precede TurnCompleted. events: {names:?}"
    );
}

// ── Test B: Flood of streaming events doesn't delay completion ──

struct FloodProvider {
    event_count: usize,
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
}

impl FloodProvider {
    fn new(count: usize) -> Self {
        Self {
            event_count: count,
            results: Arc::new(Mutex::new(HashMap::new())),
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
        for i in 0..self.event_count {
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

#[tokio::test]
async fn flood_events_do_not_block_completion_milestones() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("flood".to_string());
    store.save_session(&session).unwrap();

    // Small runtime channel to verify milestones use send().await
    // while streaming items use try_send (droppable).
    // A background consumer drains rx so milestones don't deadlock.
    let provider = FloodProvider::new(500);
    let (tx, mut rx) = mpsc::channel::<RuntimeEvent>(16);

    let collector = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(evt) = rx.recv().await {
            events.push(evt);
        }
        events
    });

    let start = std::time::Instant::now();
    run_turn_full(
        &mut store,
        &mut session,
        &provider,
        "flood".to_string(),
        PathBuf::from("."),
        None,
        Some(&tx),
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();

    drop(tx);
    let events = collector.await.unwrap();
    let names: Vec<&str> = events.iter().map(event_name).collect();

    // Milestones must be present
    assert!(
        names.contains(&"CoreTurnStarted"),
        "missing CoreTurnStarted"
    );
    assert!(
        names.contains(&"CoreOutputCompleted"),
        "missing CoreOutputCompleted"
    );
    assert!(names.contains(&"TurnCompleted"), "missing TurnCompleted");

    // Some CoreItemUpdated may have been dropped — that's expected with try_send.
    let item_count = names.iter().filter(|n| **n == "CoreItemUpdated").count();
    assert!(
        item_count < 500,
        "expected some dropped items, got all {item_count}"
    );

    assert!(
        elapsed < Duration::from_secs(5),
        "turn took {elapsed:?} — streaming backpressure may be blocking milestones"
    );
}

// Test C (TUI phase transitions) lives in crates/switchyard-tui/src/state.rs tests
// since it depends on switchyard_tui types that can't be imported from core tests.
