//! Orchestration E2E tests using FakeProvider.
//!
//! Tests the core -> peer -> core delegation loop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{Mutex, mpsc};
use uuid::Uuid;

use switchyard_core::{FakeProvider, run_routed_turn};
use switchyard_provider_api::{
    self, ArtifactBundle, ContextBundle, ExecutionPolicy, PeerCatalog, PeerDescriptor, ProbeResult,
    Provider, ProviderError, ProviderEvent, ProviderRole, TurnInput, TurnResult,
};
use switchyard_session::{self, EventType, Session, TurnOrigin};
use switchyard_store::{JsonlStore, SessionRepository, TurnRepository};

fn temp_store() -> (JsonlStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (JsonlStore::new(dir.path().to_path_buf()), dir)
}

/// Provider that emits a delegate request via sentinel block.
struct DelegatingProvider {
    results: Arc<Mutex<HashMap<Uuid, (TurnResult, ArtifactBundle)>>>,
    delegate_to: String,
    seen_inputs: Arc<Mutex<Vec<String>>>,
}

impl DelegatingProvider {
    fn new(delegate_to: &str) -> Self {
        Self {
            results: Arc::new(Mutex::new(HashMap::new())),
            delegate_to: delegate_to.to_string(),
            seen_inputs: Arc::new(Mutex::new(Vec::new())),
        }
    }

    async fn seen_inputs(&self) -> Vec<String> {
        self.seen_inputs.lock().await.clone()
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
        _cancel: switchyard_provider_api::CancellationToken,
    ) -> Result<(), ProviderError> {
        self.seen_inputs
            .lock()
            .await
            .push(input.user_message.clone());

        event_tx
            .send(ProviderEvent::turn_started(turn_id, "delegator"))
            .await
            .ok();

        // Check if the input contains a delegate_result — if so, produce final answer
        let response = if input.user_message.contains("delegate_result") {
            // Finalization turn: summarize the delegate result
            "Final answer incorporating delegate feedback.".to_string()
        } else {
            // First turn: emit a delegate request
            format!(
                "I need help reviewing this.\n\n\
                 <<<SWITCHYARD_JSON_BEGIN>>>\n\
                 {{\
                   \"type\": \"delegate\",\
                   \"requests\": [{{\
                     \"id\": \"task-1\",\
                     \"provider\": \"{}\",\
                     \"role\": \"reviewer\",\
                     \"task\": \"Review the code for issues\",\
                     \"write_access\": false,\
                     \"timeout_sec\": 60\
                   }}]\
                 }}\n\
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

#[tokio::test]
async fn no_delegate_plain_turn() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let provider = FakeProvider::success("simple answer");
    let catalog = PeerCatalog::new();

    let output = run_routed_turn(
        &mut store,
        &mut session,
        &provider,
        &catalog,
        &|_| None,
        None,
        "hello".to_string(),
        PathBuf::from("."),
    )
    .await
    .unwrap();

    assert!(!output.delegated);
    assert_eq!(output.response.as_deref(), Some("simple answer"));
}

#[tokio::test]
async fn delegate_success_core_peer_core() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("delegator".to_string());
    store.save_session(&session).unwrap();

    let core_provider = DelegatingProvider::new("reviewer");
    let mut catalog = PeerCatalog::new();
    catalog.add(PeerDescriptor {
        provider_id: "reviewer".to_string(),
        roles: vec![ProviderRole::Reviewer],
        available: true,
        capabilities: vec![],
        description: "reviewer CLI".to_string(),
        host_surface: None,
    });

    let output = run_routed_turn(
        &mut store,
        &mut session,
        &core_provider,
        &catalog,
        &|name| {
            if name == "reviewer" {
                Some(Box::new(FakeProvider::success(
                    "No issues found. Code looks good.",
                )))
            } else {
                None
            }
        },
        None,
        "review the auth module".to_string(),
        PathBuf::from("."),
    )
    .await
    .unwrap();

    assert!(output.delegated);
    // Final response should be the core's finalization answer
    assert!(output.response.as_deref().unwrap().contains("Final answer"));

    // Verify delegate events in store
    let events = store.list_session_events(session.session_id).unwrap();
    let delegate_requested = events
        .iter()
        .any(|e| e.event_type == EventType::DelegateRequested);
    let delegate_completed = events
        .iter()
        .any(|e| e.event_type == EventType::DelegateCompleted);
    assert!(delegate_requested, "should have delegate_requested event");
    assert!(delegate_completed, "should have delegate_completed event");

    // Verify peer turn exists with delegated_by
    let turns = store.list_turns(session.session_id).unwrap();
    let peer_turn = turns.iter().find(|t| t.origin == TurnOrigin::Delegate);
    assert!(peer_turn.is_some(), "should have a delegate turn");
    assert_eq!(
        peer_turn.unwrap().delegated_by.as_deref(),
        Some("delegator")
    );
}

#[tokio::test]
async fn delegate_peer_unavailable() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("delegator".to_string());
    store.save_session(&session).unwrap();

    let core_provider = DelegatingProvider::new("nonexistent");
    let catalog = PeerCatalog::new(); // empty — no peers available

    let output = run_routed_turn(
        &mut store,
        &mut session,
        &core_provider,
        &catalog,
        &|_| None,
        None,
        "review something".to_string(),
        PathBuf::from("."),
    )
    .await
    .unwrap();

    // Should still get a response (core handles the failure)
    assert!(output.response.is_some());
}

#[tokio::test]
async fn delegate_peer_failure() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("delegator".to_string());
    store.save_session(&session).unwrap();

    let core_provider = DelegatingProvider::new("failer");
    let mut catalog = PeerCatalog::new();
    catalog.add(PeerDescriptor {
        provider_id: "failer".to_string(),
        roles: vec![ProviderRole::Worker],
        available: true,
        capabilities: vec![],
        description: "failer CLI".to_string(),
        host_surface: None,
    });

    let output = run_routed_turn(
        &mut store,
        &mut session,
        &core_provider,
        &catalog,
        &|name| {
            if name == "failer" {
                Some(Box::new(FakeProvider::failure("peer crashed")))
            } else {
                None
            }
        },
        None,
        "do something".to_string(),
        PathBuf::from("."),
    )
    .await
    .unwrap();

    // Delegation happened but peer failed
    assert!(output.delegated);
    // Core should still produce a final response
    assert!(output.response.is_some());
}

#[tokio::test]
async fn routed_turn_injects_hyard_continuation_hint_into_initial_prompt() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("delegator".to_string());
    store.save_session(&session).unwrap();

    let cwd = tempfile::tempdir().unwrap();
    let job_dir = cwd.path().join(".switchyard").join("jobs");
    std::fs::create_dir_all(&job_dir).unwrap();
    std::fs::write(
        job_dir.join("running.json"),
        r#"{
            "job_id": "11111111-1111-1111-1111-111111111111",
            "provider": "claude",
            "status": "running",
            "updated_at": "2026-04-04T11:00:00Z",
            "last_event": "item_updated:claude",
            "wait_timeout_count": 1
        }"#,
    )
    .unwrap();

    let core_provider = DelegatingProvider::new("reviewer");
    let catalog = PeerCatalog::new();

    let _output = run_routed_turn(
        &mut store,
        &mut session,
        &core_provider,
        &catalog,
        &|_| None,
        None,
        "review something".to_string(),
        cwd.path().to_path_buf(),
    )
    .await
    .unwrap();

    let seen_inputs = core_provider.seen_inputs().await;
    assert!(
        !seen_inputs.is_empty(),
        "core provider should receive at least one turn"
    );
    let first = &seen_inputs[0];
    assert!(first.contains("HYARD continuation hint"));
    assert!(first.contains("11111111-1111-1111-1111-111111111111"));
    assert!(first.contains("wait_timeout"));
    assert!(first.contains("/hyard:status"));
}

#[tokio::test]
async fn routed_turn_injects_hyard_continuation_hint_into_finalization_prompt() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("delegator".to_string());
    store.save_session(&session).unwrap();

    let cwd = tempfile::tempdir().unwrap();
    let job_dir = cwd.path().join(".switchyard").join("jobs");
    std::fs::create_dir_all(&job_dir).unwrap();
    std::fs::write(
        job_dir.join("running.json"),
        r#"{
            "job_id": "22222222-2222-2222-2222-222222222222",
            "provider": "gemini",
            "status": "queued",
            "updated_at": "2026-04-04T10:00:00Z",
            "last_output_preview": "research team still working",
            "wait_timeout_count": 2
        }"#,
    )
    .unwrap();

    let core_provider = DelegatingProvider::new("reviewer");
    let mut catalog = PeerCatalog::new();
    catalog.add(PeerDescriptor {
        provider_id: "reviewer".to_string(),
        roles: vec![ProviderRole::Reviewer],
        available: true,
        capabilities: vec![],
        description: "reviewer CLI".to_string(),
        host_surface: None,
    });

    let _output = run_routed_turn(
        &mut store,
        &mut session,
        &core_provider,
        &catalog,
        &|name| {
            if name == "reviewer" {
                Some(Box::new(FakeProvider::success(
                    "No issues found. Code looks good.",
                )))
            } else {
                None
            }
        },
        None,
        "review the auth module".to_string(),
        cwd.path().to_path_buf(),
    )
    .await
    .unwrap();

    let seen_inputs = core_provider.seen_inputs().await;
    assert!(
        seen_inputs.len() >= 2,
        "core provider should receive initial and finalization turns"
    );
    let finalization = &seen_inputs[1];
    assert!(finalization.contains("delegate_result"));
    assert!(finalization.contains("HYARD continuation hint"));
    assert!(finalization.contains("22222222-2222-2222-2222-222222222222"));
    assert!(finalization.contains("Do NOT emit another delegate request."));
}

#[tokio::test]
async fn delegate_using_live_instance_registry() {
    use switchyard_provider_api::{LiveInstance, ContextBundle, ProviderEvent};
    use switchyard_core::instance::InstancePool;

    struct MockLiveInstance {
        events: Vec<ProviderEvent>,
        context_calls: Arc<Mutex<Vec<ContextBundle>>>,
    }

    #[async_trait::async_trait]
    impl LiveInstance for MockLiveInstance {
        async fn send_message(&mut self, _text: &str) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
            let (tx, rx) = mpsc::channel(10);
            for evt in self.events.drain(..) {
                tx.send(evt).await.ok();
            }
            Ok(rx)
        }
        async fn update_context(&mut self, context: ContextBundle) -> Result<(), ProviderError> {
            self.context_calls.lock().await.push(context);
            Ok(())
        }
        async fn terminate(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    let (mut store, _dir) = temp_store();
    let mut session = Session::new("delegator".to_string());
    store.save_session(&session).unwrap();

    let core_provider = DelegatingProvider::new("reviewer");
    let mut catalog = PeerCatalog::new();
    catalog.add(PeerDescriptor {
        provider_id: "reviewer".to_string(),
        roles: vec![ProviderRole::Reviewer],
        available: true,
        capabilities: vec![],
        description: "reviewer CLI".to_string(),
        host_surface: None,
    });

    // Create registry and register a live instance
    let registry = InstancePool::new();
    let context_calls = Arc::new(Mutex::new(Vec::new()));
    let peer_turn_id = Uuid::now_v7();
    let live_instance = MockLiveInstance {
        events: vec![
            ProviderEvent::turn_started(peer_turn_id, "reviewer"),
            ProviderEvent::text_message(peer_turn_id, "reviewer", "Code looks beautiful and correct!"),
            ProviderEvent::turn_completed(peer_turn_id, "reviewer"),
        ],
        context_calls: context_calls.clone(),
    };
    registry.register("reviewer", Box::new(live_instance));

    let output = run_routed_turn(
        &mut store,
        &mut session,
        &core_provider,
        &catalog,
        &|_| None, // Resolve peer returns None because we want to use the live registry!
        Some(&registry),
        "review the auth module".to_string(),
        PathBuf::from("."),
    )
    .await
    .unwrap();

    assert!(output.delegated);
    assert!(output.response.as_deref().unwrap().contains("Final answer"));

    // Verify context was updated in the live instance
    let calls = context_calls.lock().await;
    assert!(!calls.is_empty(), "Live instance context should have been updated");
}

#[tokio::test]
async fn delegate_concurrent_execution_using_pooled_instances() {
    use switchyard_provider_api::{LiveInstance, ContextBundle, ProviderEvent};
    use switchyard_core::instance::InstancePool;

    struct SlowMockLiveInstance {
        events: Vec<ProviderEvent>,
        sleep_ms: u64,
    }

    #[async_trait::async_trait]
    impl LiveInstance for SlowMockLiveInstance {
        async fn send_message(&mut self, text: &str) -> Result<mpsc::Receiver<ProviderEvent>, ProviderError> {
            eprintln!("SlowMockLiveInstance send_message: {}", text);
            let (tx, rx) = mpsc::channel(10);
            let evts = self.events.drain(..).collect::<Vec<_>>();
            let sleep_ms = self.sleep_ms;
            tokio::spawn(async move {
                eprintln!("SlowMockLiveInstance tokio spawn sleeping: {}", sleep_ms);
                tokio::time::sleep(tokio::time::Duration::from_millis(sleep_ms)).await;
                eprintln!("SlowMockLiveInstance tokio spawn finished sleeping");
                for evt in evts {
                    tx.send(evt).await.ok();
                }
            });
            Ok(rx)
        }
        async fn update_context(&mut self, _context: ContextBundle) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn terminate(&mut self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    let (mut store, _dir) = temp_store();
    let mut session = Session::new("delegator".to_string());
    store.save_session(&session).unwrap();

    struct MultiDelegatingProvider {
        results: Arc<Mutex<HashMap<Uuid, String>>>,
    }
    #[async_trait::async_trait]
    impl Provider for MultiDelegatingProvider {
        async fn probe(&self) -> Result<ProbeResult, ProviderError> {
            Ok(ProbeResult { available: true, ..Default::default() })
        }
        async fn start_turn(
            &self,
            turn_id: Uuid,
            input: TurnInput,
            _policy: ExecutionPolicy,
            _context: ContextBundle,
            event_tx: mpsc::Sender<ProviderEvent>,
            _cancel: switchyard_provider_api::CancellationToken,
        ) -> Result<(), ProviderError> {
            eprintln!("MultiDelegatingProvider user_message: {}", input.user_message);
            event_tx.send(ProviderEvent::turn_started(turn_id, "delegator")).await.ok();
            let response = if input.user_message.contains("delegate_result") {
                "Final answer."
            } else {
                "<<<SWITCHYARD_JSON_BEGIN>>>\n\
                 {\
                   \"type\": \"delegate\",\
                   \"requests\": [\
                     { \"id\": \"task-1\", \"provider\": \"worker\", \"role\": \"worker\", \"task\": \"work 1\" },\
                     { \"id\": \"task-2\", \"provider\": \"worker\", \"role\": \"worker\", \"task\": \"work 2\" }\
                   ]\
                 }\n\
                 <<<SWITCHYARD_JSON_END>>>"
            };
            self.results.lock().await.insert(turn_id, response.to_string());
            event_tx.send(ProviderEvent::text_message(turn_id, "delegator", response)).await.ok();
            event_tx.send(ProviderEvent::turn_completed(turn_id, "delegator")).await.ok();
            Ok(())
        }
        async fn finalize_turn(&self, turn_id: Uuid) -> Result<(TurnResult, ArtifactBundle), ProviderError> {
            let resp = self.results.lock().await.remove(&turn_id).unwrap_or_else(|| "Final answer.".to_string());
            Ok((TurnResult { response_text: resp, exit_code: Some(0), stderr: None, metadata: HashMap::new() }, ArtifactBundle { artifacts: vec![] }))
        }
    }

    let mut catalog = PeerCatalog::new();
    catalog.add(PeerDescriptor {
        provider_id: "worker".to_string(),
        roles: vec![ProviderRole::Worker],
        available: true,
        capabilities: vec![],
        description: "worker CLI".to_string(),
        host_surface: None,
    });

    let registry = InstancePool::new();
    let turn1 = Uuid::now_v7();
    let turn2 = Uuid::now_v7();

    registry.register("worker", Box::new(SlowMockLiveInstance {
        events: vec![
            ProviderEvent::turn_started(turn1, "worker"),
            ProviderEvent::text_message(turn1, "worker", "result 1"),
            ProviderEvent::turn_completed(turn1, "worker"),
        ],
        sleep_ms: 400,
    }));
    registry.register("worker", Box::new(SlowMockLiveInstance {
        events: vec![
            ProviderEvent::turn_started(turn2, "worker"),
            ProviderEvent::text_message(turn2, "worker", "result 2"),
            ProviderEvent::turn_completed(turn2, "worker"),
        ],
        sleep_ms: 400,
    }));

    let start = std::time::Instant::now();
    let multi_provider = MultiDelegatingProvider {
        results: Arc::new(Mutex::new(HashMap::new())),
    };
    let output = run_routed_turn(
        &mut store,
        &mut session,
        &multi_provider,
        &catalog,
        &|_| None,
        Some(&registry),
        "start delegation".to_string(),
        PathBuf::from("."),
    )
    .await
    .unwrap();

    let duration = start.elapsed();
    assert!(output.delegated);
    assert!(duration.as_millis() < 700, "Expected parallel execution under 700ms, took {}ms", duration.as_millis());
}

