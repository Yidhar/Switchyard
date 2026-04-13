use std::fs;
use std::path::Path;
use std::process::Command;

use switchyard_session::{
    Artifact, ArtifactType, Event, EventType, Session, Turn, TurnRole, TurnStatus,
};
use switchyard_store::{
    ArtifactStore, EventLog, JsonlStore, SessionRepository, StoreBackend, StoreHandle,
    TurnRepository,
};

fn write_local_config(dir: &Path, content: &str) {
    fs::write(dir.join("switchyard.toml"), content).expect("write switchyard.toml");
}

fn seed_canonical_records(
    store: &mut (impl SessionRepository + TurnRepository + EventLog + ArtifactStore),
) -> (Session, Turn) {
    let session = Session::new("fake".to_string());
    store.save_session(&session).unwrap();

    let mut turn = Turn::new(
        session.session_id,
        "fake",
        TurnRole::Core,
        "inspect store state",
    );
    turn.status = TurnStatus::Completed;
    turn.provider_response = Some("done".to_string());
    store.append_turn(&turn).unwrap();

    let event = Event::new(
        turn.turn_id,
        EventType::TurnCompleted,
        "fake",
        serde_json::json!({"ok": true}),
    );
    store.append_event(&event).unwrap();

    let artifact = Artifact::new(turn.turn_id, ArtifactType::CommandOutput, "stdout");
    store.save_artifact(&artifact).unwrap();

    (session, turn)
}

#[test]
fn store_inspect_reports_counts_for_configured_sqlite_store() {
    let dir = tempfile::tempdir().unwrap();
    write_local_config(
        dir.path(),
        r#"
[store]
backend = "sqlite"
path = ".switchyard/store.sqlite3"
"#,
    );

    let mut store = StoreHandle::open(
        StoreBackend::Sqlite,
        dir.path().join(".switchyard").join("store.sqlite3"),
    )
    .unwrap();
    let (_session, _turn) = seed_canonical_records(&mut store);

    let output = Command::new(env!("CARGO_BIN_EXE_switchyard"))
        .args(["store", "inspect", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run switchyard store inspect");

    assert!(
        output.status.success(),
        "store inspect should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["backend"], "sqlite");
    assert_eq!(json["sessions"], 1);
    assert_eq!(json["turns"], 1);
    assert_eq!(json["events"], 1);
    assert_eq!(json["artifacts"], 1);
    assert_eq!(json["path_exists"], true);
    assert_eq!(json["sqlite_schema_version"], 1);
    assert!(json["sqlite_store_id"].as_str().is_some());
}

#[test]
fn store_inspect_does_not_create_missing_sqlite_file() {
    let dir = tempfile::tempdir().unwrap();
    write_local_config(
        dir.path(),
        r#"
[store]
backend = "sqlite"
path = ".switchyard/missing.sqlite3"
"#,
    );

    let target_path = dir.path().join(".switchyard").join("missing.sqlite3");
    assert!(
        !target_path.exists(),
        "test should start with no sqlite file"
    );

    let output = Command::new(env!("CARGO_BIN_EXE_switchyard"))
        .args(["store", "inspect", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run switchyard store inspect on missing sqlite path");

    assert!(
        output.status.success(),
        "store inspect on missing sqlite should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["path_exists"], false);
    assert_eq!(json["sessions"], 0);
    assert!(
        !target_path.exists(),
        "inspect should not create sqlite file"
    );
}

#[test]
fn store_list_sessions_reports_per_session_counts() {
    let dir = tempfile::tempdir().unwrap();
    write_local_config(
        dir.path(),
        r#"
[store]
backend = "sqlite"
path = ".switchyard/store.sqlite3"
"#,
    );

    let mut store = StoreHandle::open(
        StoreBackend::Sqlite,
        dir.path().join(".switchyard").join("store.sqlite3"),
    )
    .unwrap();
    let (session, _turn) = seed_canonical_records(&mut store);

    let output = Command::new(env!("CARGO_BIN_EXE_switchyard"))
        .args(["store", "list-sessions", "--json"])
        .current_dir(dir.path())
        .output()
        .expect("run switchyard store list-sessions");

    assert!(
        output.status.success(),
        "store list-sessions should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let sessions = json["sessions"].as_array().cloned().unwrap_or_default();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["session_id"], session.session_id.to_string());
    assert_eq!(sessions[0]["active_core"], "fake");
    assert_eq!(sessions[0]["turns"], 1);
    assert_eq!(sessions[0]["events"], 1);
    assert_eq!(sessions[0]["artifacts"], 1);
    assert_eq!(sessions[0]["completed_turns"], 1);
    assert_eq!(sessions[0]["failed_turns"], 0);
}

#[test]
fn store_migrate_moves_jsonl_data_into_sqlite_store() {
    let dir = tempfile::tempdir().unwrap();
    write_local_config(dir.path(), "");

    let source_dir = dir.path().join(".switchyard").join("sessions");
    let mut source = JsonlStore::new(source_dir);
    let (session, turn) = seed_canonical_records(&mut source);

    let output = Command::new(env!("CARGO_BIN_EXE_switchyard"))
        .args([
            "store",
            "migrate",
            "--from-backend",
            "jsonl",
            "--to-backend",
            "sqlite",
            "--json",
        ])
        .current_dir(dir.path())
        .output()
        .expect("run switchyard store migrate");

    assert!(
        output.status.success(),
        "store migrate should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["from_backend"], "jsonl");
    assert_eq!(json["to_backend"], "sqlite");
    assert_eq!(json["migrated"]["sessions"], 1);
    assert_eq!(json["migrated"]["turns"], 1);
    assert_eq!(json["migrated"]["events"], 1);
    assert_eq!(json["migrated"]["artifacts"], 1);

    let target_path = dir.path().join(".switchyard").join("store.sqlite3");
    let target = StoreHandle::open(StoreBackend::Sqlite, target_path).unwrap();
    assert!(target.load_session(session.session_id).unwrap().is_some());
    assert_eq!(target.list_turns(session.session_id).unwrap().len(), 1);
    assert_eq!(target.list_events(turn.turn_id).unwrap().len(), 1);
    assert_eq!(target.list_artifacts(turn.turn_id).unwrap().len(), 1);
}

#[test]
fn store_migrate_dry_run_reports_counts_without_creating_target() {
    let dir = tempfile::tempdir().unwrap();
    write_local_config(dir.path(), "");

    let source_dir = dir.path().join(".switchyard").join("sessions");
    let mut source = JsonlStore::new(source_dir);
    let (_session, _turn) = seed_canonical_records(&mut source);
    let target_path = dir.path().join(".switchyard").join("store.sqlite3");

    let output = Command::new(env!("CARGO_BIN_EXE_switchyard"))
        .args([
            "store",
            "migrate",
            "--from-backend",
            "jsonl",
            "--to-backend",
            "sqlite",
            "--dry-run",
            "--json",
        ])
        .current_dir(dir.path())
        .output()
        .expect("run switchyard store migrate --dry-run");

    assert!(
        output.status.success(),
        "store migrate --dry-run should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["mode"], "dry_run");
    assert_eq!(json["source_counts"]["sessions"], 1);
    assert_eq!(json["source_counts"]["turns"], 1);
    assert_eq!(json["source_counts"]["events"], 1);
    assert_eq!(json["source_counts"]["artifacts"], 1);
    assert_eq!(json["can_apply"], true);
    assert_eq!(json["migrated"]["sessions"], 0);
    assert!(
        !target_path.exists(),
        "dry-run should not create target sqlite file"
    );
}

#[test]
fn store_migrate_verify_reports_success() {
    let dir = tempfile::tempdir().unwrap();
    write_local_config(dir.path(), "");

    let source_dir = dir.path().join(".switchyard").join("sessions");
    let mut source = JsonlStore::new(source_dir);
    let (_session, _turn) = seed_canonical_records(&mut source);

    let output = Command::new(env!("CARGO_BIN_EXE_switchyard"))
        .args([
            "store",
            "migrate",
            "--from-backend",
            "jsonl",
            "--to-backend",
            "sqlite",
            "--verify",
            "--json",
        ])
        .current_dir(dir.path())
        .output()
        .expect("run switchyard store migrate --verify");

    assert!(
        output.status.success(),
        "store migrate --verify should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["mode"], "apply");
    assert_eq!(json["verified"], true);
    assert_eq!(json["verification"]["matches"], true);
}
