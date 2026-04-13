//! End-to-end integration test using FakeProvider.
//!
//! Validates: Turn creation → event emission → store writes → cold-start reads.

use std::path::PathBuf;

use switchyard_core::{FakeProvider, run_turn};
use switchyard_session::*;
use switchyard_store::*;

fn temp_store() -> (JsonlStore, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (JsonlStore::new(dir.path().to_path_buf()), dir)
}

#[tokio::test]
async fn full_turn_lifecycle_success() {
    let (mut store, dir) = temp_store();
    let mut session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let provider = FakeProvider::success("done");
    let output = run_turn(
        &mut store,
        &mut session,
        &provider,
        "hello".to_string(),
        PathBuf::from("."),
    )
    .await
    .unwrap();
    let turn_id = output.turn_id;
    assert_eq!(output.response.as_deref(), Some("done"));

    // Verify Turn (collapsed to latest per turn_id)
    let turns = store.list_turns(session.session_id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, TurnStatus::Completed);

    // Verify Events
    let events = store.list_events(turn_id).unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].event_type, EventType::TurnStarted);
    assert_eq!(events[1].event_type, EventType::ItemUpdated);
    assert_eq!(events[2].event_type, EventType::TurnCompleted);

    // Verify Artifacts
    let artifacts = store.list_artifacts(turn_id).unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].artifact_type, ArtifactType::RawProviderOutput);

    // Verify cold-start read
    let store2 = JsonlStore::new(dir.path().to_path_buf());
    let loaded_session = store2.load_session(session.session_id).unwrap().unwrap();
    assert_eq!(loaded_session.active_core, "fake");

    let cold_events = store2.list_events(turn_id).unwrap();
    assert_eq!(cold_events.len(), 3);

    let cold_artifacts = store2.list_artifacts(turn_id).unwrap();
    assert_eq!(cold_artifacts.len(), 1);
}

#[tokio::test]
async fn full_turn_lifecycle_failure() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let provider = FakeProvider::failure("internal error");
    let output = run_turn(
        &mut store,
        &mut session,
        &provider,
        "crash me".to_string(),
        PathBuf::from("."),
    )
    .await
    .unwrap();

    let turns = store.list_turns(session.session_id).unwrap();
    let final_turn = turns.last().unwrap();
    assert_eq!(final_turn.status, TurnStatus::Failed);
    assert_eq!(final_turn.error_message.as_deref(), Some("internal error"));

    let events = store.list_events(output.turn_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].event_type, EventType::TurnFailed);
}

#[tokio::test]
async fn multi_turn_session() {
    let (mut store, _dir) = temp_store();
    let mut session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let provider = FakeProvider::success("response");

    let out1 = run_turn(
        &mut store,
        &mut session,
        &provider,
        "first".to_string(),
        PathBuf::from("."),
    )
    .await
    .unwrap();

    let out2 = run_turn(
        &mut store,
        &mut session,
        &provider,
        "second".to_string(),
        PathBuf::from("."),
    )
    .await
    .unwrap();

    assert_ne!(out1.turn_id, out2.turn_id);

    let events1 = store.list_events(out1.turn_id).unwrap();
    let events2 = store.list_events(out2.turn_id).unwrap();
    assert_eq!(events1.len(), 3);
    assert_eq!(events2.len(), 3);

    // Session should list all turns (collapsed: 1 per turn_id)
    let all_turns = store.list_turns(session.session_id).unwrap();
    assert_eq!(all_turns.len(), 2);
}

#[tokio::test]
async fn full_turn_lifecycle_success_with_sqlite_store_handle() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join(".switchyard").join("store.sqlite3");
    let mut store = StoreHandle::open(StoreBackend::Sqlite, db_path.clone()).unwrap();
    let mut session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let provider = FakeProvider::success("done");
    let output = run_turn(
        &mut store,
        &mut session,
        &provider,
        "hello".to_string(),
        PathBuf::from("."),
    )
    .await
    .unwrap();

    assert_eq!(output.response.as_deref(), Some("done"));

    let turns = store.list_turns(session.session_id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, TurnStatus::Completed);

    let reopened = StoreHandle::open(StoreBackend::Sqlite, db_path).unwrap();
    let loaded_session = reopened.load_session(session.session_id).unwrap().unwrap();
    assert_eq!(loaded_session.active_core, "fake");
    assert_eq!(reopened.list_events(output.turn_id).unwrap().len(), 3);
    assert_eq!(reopened.list_artifacts(output.turn_id).unwrap().len(), 1);
}
