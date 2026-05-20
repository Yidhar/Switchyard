//! Tests for the `switchyard host` bridge functions.
//!
//! These tests verify the host bridge layer works correctly with
//! a FakeProvider, covering list, delegate, status, result, cancel, help.

use std::path::PathBuf;
use std::time::Duration;

use switchyard_cli::host;
use switchyard_config::{StoreBackendConfig, SwitchyardConfig};
use switchyard_core::{FakeProvider, ProviderRegistry};
use switchyard_provider_api::Provider;
use switchyard_session::{InboxStatus, Session, Turn, TurnRole, TurnStatus};
use switchyard_store::{
    JsonlStore, SessionCatalog, SessionInboxRepository, SessionRepository, StoreHandle,
    TurnRepository,
};
use uuid::Uuid;

fn build_test_registry() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    registry.register(
        "fake",
        Box::new(|_cfg| {
            let p: Box<dyn Provider> = Box::new(FakeProvider::success("test response"));
            p
        }),
    );
    registry.register(
        "failer",
        Box::new(|_cfg| {
            let p: Box<dyn Provider> = Box::new(FakeProvider::failure("provider error"));
            p
        }),
    );
    registry
}

fn temp_config(dir: &std::path::Path) -> SwitchyardConfig {
    let mut config = SwitchyardConfig::default();
    config.session.directory = Some(dir.to_path_buf());
    config
}

// ── host_list ──

#[tokio::test]
async fn host_list_runs_without_error() {
    let registry = build_test_registry();
    let config = SwitchyardConfig::default();
    // Just verify it doesn't panic; output goes to stdout
    host::host_list(&registry, &config).await;
}

// ── host_delegate ──

#[tokio::test]
async fn host_delegate_creates_session_and_turn() {
    let dir = tempfile::tempdir().unwrap();
    let registry = build_test_registry();
    let config = temp_config(dir.path());

    // host_delegate prints JSON to stdout and creates a session+turn in the store
    host::host_delegate(&registry, &config, "fake", "say hello", dir.path()).await;

    // Verify store has a session and a turn
    let store = JsonlStore::new(config.session_dir(dir.path()));
    let sessions = store.list_sessions().unwrap();
    assert!(!sessions.is_empty(), "should have at least one session");

    let turns = store.list_turns(sessions[0]).unwrap();
    assert!(!turns.is_empty(), "should have at least one turn");
    assert_eq!(turns[0].provider, "fake");
}

// ── host_status ──

#[tokio::test]
async fn host_status_finds_existing_turn() {
    let dir = tempfile::tempdir().unwrap();
    let session_dir = dir.path().join(".switchyard").join("sessions");
    std::fs::create_dir_all(&session_dir).unwrap();

    let mut store = JsonlStore::new(session_dir.clone());
    let session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    // Create a turn
    let turn = switchyard_session::Turn::new(
        session.session_id,
        "fake",
        switchyard_session::TurnRole::Core,
        "test message",
    );
    let turn_id = turn.turn_id;
    store.append_turn(&turn).unwrap();

    let mut config = SwitchyardConfig::default();
    config.session.directory = Some(session_dir);

    // host_status should find this turn (prints to stdout, doesn't exit(1))
    host::host_status(&config, &turn_id.to_string(), dir.path()).await;
}

#[tokio::test]
async fn host_status_finds_existing_turn_in_sqlite_store() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = SwitchyardConfig::default();
    config.store.backend = StoreBackendConfig::Sqlite;
    config.store.path = Some(PathBuf::from(".switchyard/store.sqlite3"));

    let mut store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .expect("sqlite store should open");
    let session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let turn = switchyard_session::Turn::new(
        session.session_id,
        "fake",
        switchyard_session::TurnRole::Core,
        "sqlite test message",
    );
    let turn_id = turn.turn_id;
    store.append_turn(&turn).unwrap();

    host::host_status(&config, &turn_id.to_string(), dir.path()).await;
}

// ── host_result ──

#[tokio::test]
async fn host_result_finds_existing_turn() {
    let dir = tempfile::tempdir().unwrap();
    let session_dir = dir.path().join(".switchyard").join("sessions");
    std::fs::create_dir_all(&session_dir).unwrap();

    let mut store = JsonlStore::new(session_dir.clone());
    let session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let mut turn = switchyard_session::Turn::new(
        session.session_id,
        "fake",
        switchyard_session::TurnRole::Core,
        "test",
    );
    turn.provider_response = Some("the result".to_string());
    turn.status = switchyard_session::TurnStatus::Completed;
    let turn_id = turn.turn_id;
    store.append_turn(&turn).unwrap();

    let mut config = SwitchyardConfig::default();
    config.session.directory = Some(session_dir);

    host::host_result(&config, &turn_id.to_string(), dir.path()).await;
}

// ── host_cancel ──

#[tokio::test]
async fn host_cancel_reports_state_for_existing_turn() {
    let dir = tempfile::tempdir().unwrap();
    let session_dir = dir.path().join(".switchyard").join("sessions");
    std::fs::create_dir_all(&session_dir).unwrap();

    let mut store = JsonlStore::new(session_dir.clone());
    let session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let mut turn = switchyard_session::Turn::new(
        session.session_id,
        "fake",
        switchyard_session::TurnRole::Core,
        "work",
    );
    turn.status = switchyard_session::TurnStatus::Completed;
    let turn_id = turn.turn_id;
    store.append_turn(&turn).unwrap();

    let mut config = SwitchyardConfig::default();
    config.session.directory = Some(session_dir);

    // V1: cancel on a completed job just returns its state
    host::host_cancel(&config, &turn_id.to_string(), dir.path()).await;
}

// ── host_help ──

#[tokio::test]
async fn host_help_prints_all_commands() {
    // Just verify it doesn't panic
    host::host_help();
}

#[tokio::test]
async fn host_delegate_timeout_leaves_job_running_for_later_inspection() {
    let dir = tempfile::tempdir().unwrap();
    let mut registry = ProviderRegistry::new();
    registry.register(
        "slow",
        Box::new(|_cfg| {
            let p: Box<dyn Provider> =
                Box::new(FakeProvider::timeout(std::time::Duration::from_millis(250)));
            p
        }),
    );
    let config = temp_config(dir.path());

    host::host_delegate_with_wait(&registry, &config, "slow", "long task", dir.path(), 0, None)
        .await;

    let job_dir = config.job_dir(dir.path());
    let jobs: Vec<_> = std::fs::read_dir(job_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(jobs.len(), 1, "should create one job manifest");

    let payload = std::fs::read_to_string(&jobs[0]).unwrap();
    assert!(
        payload.contains("\"status\": \"queued\"") || payload.contains("\"status\": \"running\""),
        "job should still be active after immediate wait timeout. payload:\n{payload}"
    );
}

// ── host_delegate is leaf (no further delegation) ──

#[tokio::test]
async fn host_delegate_is_leaf_turn_not_routed() {
    // Use a fake provider. Since host_delegate uses run_turn_with_archive
    // (not run_routed_turn), it should NOT trigger delegation even if
    // the response contains sentinel blocks. We verify by checking that
    // only one core turn exists (no delegate turn).
    let dir = tempfile::tempdir().unwrap();
    let registry = build_test_registry();
    let config = temp_config(dir.path());

    host::host_delegate(&registry, &config, "fake", "do something", dir.path()).await;

    let store = JsonlStore::new(config.session_dir(dir.path()));
    let sessions = store.list_sessions().unwrap();
    let turns = store.list_turns(sessions[0]).unwrap();

    // Leaf turn: exactly one turn, no delegate turns
    assert_eq!(
        turns.len(),
        1,
        "leaf execution should produce exactly one turn"
    );
    assert_eq!(turns[0].origin, switchyard_session::TurnOrigin::User);
}

#[tokio::test]
async fn host_delegate_writes_callback_receipt_to_session_inbox() {
    let dir = tempfile::tempdir().unwrap();
    let registry = build_test_registry();
    let config = temp_config(dir.path());

    host::host_delegate(&registry, &config, "fake", "summarize state", dir.path()).await;

    let store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let sessions = store.list_sessions().unwrap();
    assert_eq!(sessions.len(), 1);

    let inbox = store.list_inbox_entries(sessions[0]).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(
        inbox[0].kind,
        switchyard_session::InboxItemKind::BackgroundJobReceipt
    );
    assert_eq!(inbox[0].status, switchyard_session::InboxStatus::Unread);
    assert_eq!(inbox[0].provider.as_deref(), Some("fake"));
    assert!(inbox[0].message.contains("background"));
}

#[tokio::test]
async fn host_resume_callbacks_continues_existing_session_and_consumes_inbox() {
    let dir = tempfile::tempdir().unwrap();
    let registry = build_test_registry();
    let config = temp_config(dir.path());

    let mut store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let mut prior_turn = Turn::new(
        session.session_id,
        "fake",
        TurnRole::Core,
        "previous work item",
    );
    prior_turn.provider_response = Some("existing progress".to_string());
    prior_turn.status = TurnStatus::Completed;
    store.append_turn(&prior_turn).unwrap();

    let mut receipt = switchyard_session::InboxEntry::background_job_receipt(
        session.session_id,
        "claude",
        "claude background job completed",
        "Claude finished a background task while you were idle.",
    );
    receipt.job_id = Some(Uuid::now_v7());
    receipt.summary = Some("Auth review complete".to_string());
    receipt.payload = serde_json::json!({
        "job_status": "completed",
        "callback_delivery": "checkpoint",
        "summary": "Auth review complete",
    });
    store.save_inbox_entry(&receipt).unwrap();

    let turns_before = store.list_turns(session.session_id).unwrap().len();
    drop(store);

    host::host_resume(
        &registry,
        &config,
        Some(&session.session_id.to_string()),
        false,
        None,
        true,
        dir.path(),
    )
    .await;

    let store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let turns = store.list_turns(session.session_id).unwrap();
    assert_eq!(turns.len(), turns_before + 1);
    let resumed_turn = turns.last().unwrap();
    assert_eq!(resumed_turn.status, TurnStatus::Completed);
    assert!(
        resumed_turn
            .user_message
            .contains("Background callback receipts are ready."),
        "resume should store the clean callback continuation message"
    );

    let inbox = store.list_inbox_entries(session.session_id).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, InboxStatus::Consumed);
}

#[tokio::test]
async fn host_resume_callbacks_noop_when_no_unread_receipts() {
    let dir = tempfile::tempdir().unwrap();
    let registry = build_test_registry();
    let config = temp_config(dir.path());

    let mut store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let mut quiet_receipt = switchyard_session::InboxEntry::background_job_receipt(
        session.session_id,
        "gemini",
        "gemini background job running",
        "Gemini background task is still running.",
    );
    quiet_receipt.job_id = Some(Uuid::now_v7());
    quiet_receipt.payload = serde_json::json!({
        "job_status": "running",
        "callback_delivery": "quiet",
    });
    store.save_inbox_entry(&quiet_receipt).unwrap();

    drop(store);

    host::host_resume(
        &registry,
        &config,
        Some(&session.session_id.to_string()),
        false,
        None,
        true,
        dir.path(),
    )
    .await;

    let store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let turns = store.list_turns(session.session_id).unwrap();
    assert!(
        turns.is_empty(),
        "no turn should be created when there are no unread resumable callback receipts"
    );

    let inbox = store.list_inbox_entries(session.session_id).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, InboxStatus::Unread);
}

#[tokio::test]
async fn host_resume_callbacks_returns_busy_when_session_has_active_turn_lease() {
    let dir = tempfile::tempdir().unwrap();
    let registry = build_test_registry();
    let config = temp_config(dir.path());

    let mut store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let mut session = Session::new("fake".to_string());
    let active_turn_id = Uuid::now_v7();
    session.mark_turn_active(active_turn_id, "fake");
    store.save_session(&session).unwrap();

    let mut receipt = switchyard_session::InboxEntry::background_job_receipt(
        session.session_id,
        "claude",
        "claude background job completed",
        "Claude finished a background task while you were idle.",
    );
    receipt.job_id = Some(Uuid::now_v7());
    receipt.payload = serde_json::json!({
        "job_status": "completed",
        "callback_delivery": "checkpoint",
    });
    store.save_inbox_entry(&receipt).unwrap();
    drop(store);

    host::host_resume(
        &registry,
        &config,
        Some(&session.session_id.to_string()),
        false,
        None,
        true,
        dir.path(),
    )
    .await;

    let store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let turns = store.list_turns(session.session_id).unwrap();
    assert!(
        turns.is_empty(),
        "busy sessions should not start a concurrent callback-driven resume"
    );

    let persisted_session = store.load_session(session.session_id).unwrap().unwrap();
    assert_eq!(persisted_session.active_turn_id, Some(active_turn_id));
    let inbox = store.list_inbox_entries(session.session_id).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, InboxStatus::Unread);
}

#[tokio::test]
async fn host_delegate_session_routes_callback_to_target_session_inbox() {
    let dir = tempfile::tempdir().unwrap();
    let registry = build_test_registry();
    let config = temp_config(dir.path());

    let mut store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let host_session = Session::new("codex".to_string());
    store.save_session(&host_session).unwrap();
    drop(store);

    let host_session_selector = host_session.session_id.to_string();
    host::host_delegate_with_wait(
        &registry,
        &config,
        "fake",
        "review this module",
        dir.path(),
        1,
        Some(host_session_selector.as_str()),
    )
    .await;

    let store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let sessions = store.list_sessions().unwrap();
    assert!(
        sessions.len() >= 2,
        "delegate should create a worker session in addition to the host session"
    );

    let host_inbox = store.list_inbox_entries(host_session.session_id).unwrap();
    assert_eq!(host_inbox.len(), 1);
    assert_eq!(host_inbox[0].status, InboxStatus::Unread);
    assert_eq!(host_inbox[0].provider.as_deref(), Some("fake"));
    let expected_callback_session_id = host_session.session_id.to_string();
    assert_eq!(
        host_inbox[0].payload["callback_session_id"].as_str(),
        Some(expected_callback_session_id.as_str())
    );

    let other_inbox_entries: usize = sessions
        .into_iter()
        .filter(|session_id| *session_id != host_session.session_id)
        .map(|session_id| store.list_inbox_entries(session_id).unwrap().len())
        .sum();
    assert_eq!(
        other_inbox_entries, 0,
        "callback receipt should be routed to the target host session, not the worker session"
    );
}

#[tokio::test]
async fn host_follow_waits_for_resumable_callbacks_and_resumes_session() {
    let dir = tempfile::tempdir().unwrap();
    let registry = build_test_registry();
    let config = temp_config(dir.path());

    let mut store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();
    drop(store);

    let session_id = session.session_id;
    let session_selector = session_id.to_string();
    let config_for_task = config.clone();
    let dir_for_task = dir.path().to_path_buf();
    let injector = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut store = StoreHandle::open(
            config_for_task.store_backend(&dir_for_task),
            config_for_task.store_path(&dir_for_task),
        )
        .unwrap();
        let mut receipt = switchyard_session::InboxEntry::background_job_receipt(
            session_id,
            "claude",
            "claude background job completed",
            "Claude finished a background task while you were idle.",
        );
        receipt.job_id = Some(Uuid::now_v7());
        receipt.summary = Some("Design review complete".to_string());
        receipt.payload = serde_json::json!({
            "job_status": "completed",
            "callback_delivery": "checkpoint",
            "summary": "Design review complete",
        });
        store.save_inbox_entry(&receipt).unwrap();
    });

    host::host_follow(
        &registry,
        &config,
        Some(session_selector.as_str()),
        false,
        1,
        false,
        dir.path(),
    )
    .await;

    injector.await.unwrap();

    let store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let turns = store.list_turns(session_id).unwrap();
    assert_eq!(turns.len(), 1, "follow should create one resumed turn");
    assert_eq!(turns[0].status, TurnStatus::Completed);
    assert!(
        turns[0]
            .user_message
            .contains("Background callback receipts are ready."),
        "follow should resume with the callback continuation message"
    );

    let inbox = store.list_inbox_entries(session_id).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, InboxStatus::Consumed);
}

#[tokio::test]
async fn host_follow_ignores_quiet_receipts_and_times_out_without_resume() {
    let dir = tempfile::tempdir().unwrap();
    let registry = build_test_registry();
    let config = temp_config(dir.path());

    let mut store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let mut quiet_receipt = switchyard_session::InboxEntry::background_job_receipt(
        session.session_id,
        "gemini",
        "gemini background job running",
        "Gemini background task is still running.",
    );
    quiet_receipt.job_id = Some(Uuid::now_v7());
    quiet_receipt.payload = serde_json::json!({
        "job_status": "running",
        "callback_delivery": "quiet",
    });
    store.save_inbox_entry(&quiet_receipt).unwrap();
    drop(store);

    host::host_follow(
        &registry,
        &config,
        Some(&session.session_id.to_string()),
        false,
        0,
        false,
        dir.path(),
    )
    .await;

    let store = StoreHandle::open(
        config.store_backend(dir.path()),
        config.store_path(dir.path()),
    )
    .unwrap();
    let turns = store.list_turns(session.session_id).unwrap();
    assert!(
        turns.is_empty(),
        "quiet receipts should not wake follow into creating a new turn"
    );

    let inbox = store.list_inbox_entries(session.session_id).unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(inbox[0].status, InboxStatus::Unread);
}
