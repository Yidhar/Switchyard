//! Tests for the `switchyard host` bridge functions.
//!
//! These tests verify the host bridge layer works correctly with
//! a FakeProvider, covering list, delegate, status, result, cancel, help.

use std::path::PathBuf;

use switchyard_cli::host;
use switchyard_config::{StoreBackendConfig, SwitchyardConfig};
use switchyard_core::{FakeProvider, ProviderRegistry};
use switchyard_provider_api::Provider;
use switchyard_session::Session;
use switchyard_store::{JsonlStore, SessionRepository, StoreHandle, TurnRepository};

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

    let mut store = StoreHandle::open(config.store_backend(), config.store_path(dir.path()))
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

    host::host_delegate_with_wait(&registry, &config, "slow", "long task", dir.path(), 0).await;

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
