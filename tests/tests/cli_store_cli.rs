use std::fs;
use std::path::Path;
use std::process::Command;

use switchyard_session::{
    Artifact, ArtifactType, Event, EventType, Session, Turn, TurnRole, TurnStatus,
};
use switchyard_store::{
    ArtifactStore, EventLog, JsonlStore, SessionCatalog, SessionRepository, StoreBackend,
    StoreHandle, TurnRepository,
};

fn write_local_config(dir: &Path, content: &str) {
    fs::write(dir.join("switchyard.toml"), content).expect("write switchyard.toml");
}

fn run_switchyard(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_switchyard"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run switchyard command")
}

fn seed_canonical_records(
    store: &mut (impl SessionRepository + TurnRepository + EventLog + ArtifactStore),
) -> (Session, Turn) {
    let session = Session::new("fake".to_string());
    seed_canonical_records_for_session(store, session)
}

fn seed_canonical_records_with_session_id(
    store: &mut (impl SessionRepository + TurnRepository + EventLog + ArtifactStore),
    session_id: uuid::Uuid,
) -> (Session, Turn) {
    let mut session = Session::new("fake".to_string());
    session.session_id = session_id;
    seed_canonical_records_for_session(store, session)
}

fn seed_canonical_records_for_session(
    store: &mut (impl SessionRepository + TurnRepository + EventLog + ArtifactStore),
    session: Session,
) -> (Session, Turn) {
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
fn store_list_sessions_limit_applies_to_json_report() {
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
    let (_session_a, _turn_a) = seed_canonical_records(&mut store);
    let (session_b, _turn_b) = seed_canonical_records(&mut store);

    let output = run_switchyard(
        dir.path(),
        &["store", "list-sessions", "--limit", "1", "--json"],
    );

    assert!(
        output.status.success(),
        "store list-sessions --limit should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["total_sessions"], 2);
    assert_eq!(json["displayed_sessions"], 1);
    assert_eq!(json["limit"], 1);
    let sessions = json["sessions"].as_array().cloned().unwrap_or_default();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["session_id"], session_b.session_id.to_string());
}

#[test]
fn store_list_sessions_short_uses_compact_table_output() {
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

    let output = run_switchyard(dir.path(), &["store", "list-sessions", "--short"]);

    assert!(
        output.status.success(),
        "store list-sessions --short should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("SESSION_ID"));
    assert!(stdout.contains("CORE"));
    assert!(stdout.contains("UPDATED_AT"));
    assert!(!stdout.contains("completed="));
    assert!(!stdout.contains("delegate_turns="));
}

#[test]
fn store_list_sessions_human_output_includes_store_show_hint() {
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

    let output = run_switchyard(dir.path(), &["store", "list-sessions"]);

    assert!(
        output.status.success(),
        "store list-sessions should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("switchyard store show <session-id-or-prefix>"));
    assert!(stdout.contains("omit the selector to open the latest session"));
    assert!(stdout.contains("switchyard tui --session <session-id-or-prefix>"));
    assert!(stdout.contains("switchyard tui --resume-latest"));
}

#[test]
fn store_check_reports_clean_sqlite_store() {
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

    let output = run_switchyard(dir.path(), &["store", "check", "--json"]);

    assert!(
        output.status.success(),
        "store check should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["backend"], "sqlite");
    assert_eq!(json["path_exists"], true);
    assert_eq!(json["ok"], true);
    assert_eq!(json["counts"]["sessions"], 1);
    assert_eq!(json["counts"]["turns"], 1);
    assert_eq!(json["counts"]["events"], 1);
    assert_eq!(json["counts"]["artifacts"], 1);
    assert_eq!(json["sqlite_schema_version"], 1);
    assert!(
        json["sqlite_integrity_errors"]
            .as_array()
            .is_some_and(|items| items.is_empty())
    );
    assert!(
        json["sqlite_foreign_key_issues"]
            .as_array()
            .is_some_and(|items| items.is_empty())
    );
}

#[test]
fn store_check_fails_for_missing_sqlite_store() {
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
    let output = run_switchyard(dir.path(), &["store", "check", "--json"]);

    assert!(
        !output.status.success(),
        "store check on missing sqlite should fail.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["path_exists"], false);
    assert_eq!(json["ok"], false);
    assert!(
        json["issues"]
            .as_array()
            .and_then(|items| items.first())
            .and_then(|value| value.as_str())
            .is_some_and(|issue| issue.contains("does not exist"))
    );
    assert!(!target_path.exists(), "check should not create sqlite file");
}

#[test]
fn store_show_reports_session_turn_event_and_artifact_detail() {
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
    let (session, turn) = seed_canonical_records(&mut store);

    let output = run_switchyard(
        dir.path(),
        &[
            "store",
            "show",
            "--session",
            &session.session_id.to_string(),
            "--json",
        ],
    );

    assert!(
        output.status.success(),
        "store show should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["backend"], "sqlite");
    assert_eq!(
        json["session"]["session_id"],
        session.session_id.to_string()
    );
    assert_eq!(json["session"]["active_core"], "fake");
    assert_eq!(json["session"]["counts"]["turns"], 1);
    assert_eq!(json["session"]["counts"]["events"], 1);
    assert_eq!(json["session"]["counts"]["artifacts"], 1);

    let turns = json["turns"].as_array().cloned().unwrap_or_default();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0]["turn_id"], turn.turn_id.to_string());
    assert_eq!(turns[0]["status"], "completed");
    assert_eq!(turns[0]["events"][0]["event_type"], "turn_completed");
    assert_eq!(turns[0]["artifacts"][0]["artifact_type"], "command_output");
    assert_eq!(turns[0]["artifacts"][0]["title"], "stdout");
}

#[test]
fn store_show_accepts_unique_uuid_prefix() {
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
    let prefix = &session.session_id.to_string()[..8];

    let output = run_switchyard(
        dir.path(),
        &["store", "show", "--session", prefix, "--json"],
    );

    assert!(
        output.status.success(),
        "store show should accept uuid prefix.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["session"]["session_id"],
        session.session_id.to_string()
    );
    assert_eq!(json["selection"]["mode"], "explicit");
}

#[test]
fn store_show_accepts_positional_session_selector() {
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
    let prefix = &session.session_id.to_string()[..8];

    let output = run_switchyard(dir.path(), &["store", "show", prefix, "--json"]);

    assert!(
        output.status.success(),
        "store show should accept positional session selector.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["session"]["session_id"],
        session.session_id.to_string()
    );
    assert_eq!(json["selection"]["mode"], "explicit");
}

#[test]
fn store_show_defaults_to_latest_session_when_selector_is_omitted() {
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
    let (session_a, _turn_a) = seed_canonical_records(&mut store);
    let (session_b, _turn_b) = seed_canonical_records(&mut store);

    let output = run_switchyard(dir.path(), &["store", "show", "--json"]);

    assert!(
        output.status.success(),
        "store show without selector should default to latest session.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_ne!(
        session_a.session_id.to_string(),
        session_b.session_id.to_string()
    );
    assert_eq!(
        json["session"]["session_id"],
        session_b.session_id.to_string()
    );
    assert_eq!(json["selection"]["mode"], "latest");
}

#[test]
fn store_show_human_output_hides_event_and_artifact_detail_by_default() {
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

    let output = run_switchyard(
        dir.path(),
        &[
            "store",
            "show",
            "--session",
            &session.session_id.to_string(),
        ],
    );

    assert!(
        output.status.success(),
        "store show human output should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("selected by:"));
    assert!(stdout.contains("session id:"));
    assert!(stdout.contains("turns:"));
    assert!(stdout.contains("events=1 artifacts=1"));
    assert!(stdout.contains("re-run with --verbose"));
    assert!(!stdout.contains("events (1):"));
    assert!(!stdout.contains("artifacts (1):"));
    assert!(!stdout.contains("turn_completed"));
    assert!(!stdout.contains("command_output"));
}

#[test]
fn store_show_human_output_verbose_includes_turn_event_and_artifact_sections() {
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

    let output = run_switchyard(
        dir.path(),
        &[
            "store",
            "show",
            "--session",
            &session.session_id.to_string(),
            "--verbose",
        ],
    );

    assert!(
        output.status.success(),
        "store show --verbose should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("events (1):"));
    assert!(stdout.contains("artifacts (1):"));
    assert!(stdout.contains("turn_completed"));
    assert!(stdout.contains("command_output"));
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
fn store_check_session_limits_counts_to_selected_session() {
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
    let (session_a, _turn_a) = seed_canonical_records(&mut store);
    let (_session_b, _turn_b) = seed_canonical_records(&mut store);

    let output = run_switchyard(
        dir.path(),
        &[
            "store",
            "check",
            "--session",
            &session_a.session_id.to_string(),
            "--json",
        ],
    );

    assert!(
        output.status.success(),
        "store check --session should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["counts"]["sessions"], 1);
    assert_eq!(json["counts"]["turns"], 1);
    assert_eq!(json["counts"]["events"], 1);
    assert_eq!(json["counts"]["artifacts"], 1);
}

#[test]
fn store_check_session_accepts_unique_uuid_prefix() {
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

    let prefix = &session.session_id.to_string()[..8];
    let output = run_switchyard(
        dir.path(),
        &["store", "check", "--session", prefix, "--json"],
    );

    assert!(
        output.status.success(),
        "store check should accept uuid prefix.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["ok"], true);
    assert_eq!(json["counts"]["sessions"], 1);
}

#[test]
fn store_check_session_rejects_ambiguous_uuid_prefix() {
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
    let _ = seed_canonical_records_with_session_id(
        &mut store,
        uuid::Uuid::parse_str("aaaaaaaa-0000-7000-8000-000000000001").unwrap(),
    );
    let _ = seed_canonical_records_with_session_id(
        &mut store,
        uuid::Uuid::parse_str("aaaaaaaa-0000-7000-8000-000000000002").unwrap(),
    );

    let output = run_switchyard(dir.path(), &["store", "check", "--session", "aaaaaaaa"]);

    assert!(
        !output.status.success(),
        "store check should reject ambiguous uuid prefix.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("ambiguous"));
    assert!(stderr.contains("aaaaaaaa-0000-7000-8000-000000000001"));
    assert!(stderr.contains("aaaaaaaa-0000-7000-8000-000000000002"));
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
    assert_eq!(json["verification"]["count_matches"], true);
    assert_eq!(json["verification"]["session_scope_matches"], true);
}

#[test]
fn store_migrate_session_moves_only_one_session() {
    let dir = tempfile::tempdir().unwrap();
    write_local_config(dir.path(), "");

    let source_dir = dir.path().join(".switchyard").join("sessions");
    let mut source = JsonlStore::new(source_dir);
    let (session_a, turn_a) = seed_canonical_records(&mut source);
    let (session_b, turn_b) = seed_canonical_records(&mut source);

    let output = run_switchyard(
        dir.path(),
        &[
            "store",
            "migrate",
            "--from-backend",
            "jsonl",
            "--session",
            &session_a.session_id.to_string(),
            "--to-backend",
            "sqlite",
            "--json",
        ],
    );

    assert!(
        output.status.success(),
        "store migrate --session should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["selected_session"], session_a.session_id.to_string());
    assert_eq!(json["source_counts"]["sessions"], 1);
    assert_eq!(json["migrated"]["sessions"], 1);
    assert_eq!(json["migrated"]["turns"], 1);
    assert_eq!(json["migrated"]["events"], 1);
    assert_eq!(json["migrated"]["artifacts"], 1);

    let target_path = dir.path().join(".switchyard").join("store.sqlite3");
    let target = StoreHandle::open(StoreBackend::Sqlite, target_path).unwrap();
    assert!(target.load_session(session_a.session_id).unwrap().is_some());
    assert!(target.load_session(session_b.session_id).unwrap().is_none());
    assert_eq!(target.list_turns(session_a.session_id).unwrap().len(), 1);
    assert_eq!(target.list_events(turn_a.turn_id).unwrap().len(), 1);
    assert_eq!(target.list_artifacts(turn_a.turn_id).unwrap().len(), 1);
    assert!(target.list_events(turn_b.turn_id).unwrap().is_empty());
    assert!(target.list_artifacts(turn_b.turn_id).unwrap().is_empty());
}

#[test]
fn store_migrate_session_accepts_unique_uuid_prefix() {
    let dir = tempfile::tempdir().unwrap();
    write_local_config(dir.path(), "");

    let source_dir = dir.path().join(".switchyard").join("sessions");
    let mut source = JsonlStore::new(source_dir);
    let (session, _turn) = seed_canonical_records(&mut source);
    let prefix = &session.session_id.to_string()[..8];

    let output = run_switchyard(
        dir.path(),
        &[
            "store",
            "migrate",
            "--from-backend",
            "jsonl",
            "--session",
            prefix,
            "--to-backend",
            "sqlite",
            "--json",
        ],
    );

    assert!(
        output.status.success(),
        "store migrate should accept uuid prefix.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["selected_session"], session.session_id.to_string());
    assert_eq!(json["migrated"]["sessions"], 1);
}

#[test]
fn store_migrate_verify_only_reports_success_after_prior_migration() {
    let dir = tempfile::tempdir().unwrap();
    write_local_config(dir.path(), "");

    let source_dir = dir.path().join(".switchyard").join("sessions");
    let mut source = JsonlStore::new(source_dir);
    let (_session, _turn) = seed_canonical_records(&mut source);

    let migrate_output = run_switchyard(
        dir.path(),
        &[
            "store",
            "migrate",
            "--from-backend",
            "jsonl",
            "--to-backend",
            "sqlite",
            "--json",
        ],
    );
    assert!(
        migrate_output.status.success(),
        "initial migration should succeed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&migrate_output.stdout),
        String::from_utf8_lossy(&migrate_output.stderr),
    );

    let verify_output = run_switchyard(
        dir.path(),
        &[
            "store",
            "migrate",
            "--from-backend",
            "jsonl",
            "--to-backend",
            "sqlite",
            "--verify-only",
            "--json",
        ],
    );

    assert!(
        verify_output.status.success(),
        "store migrate --verify-only should succeed after migration.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&verify_output.stdout),
        String::from_utf8_lossy(&verify_output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&verify_output.stdout).unwrap();
    assert_eq!(json["mode"], "verify_only");
    assert_eq!(json["verified"], true);
    assert_eq!(json["verification"]["matches"], true);
    assert_eq!(json["verification"]["count_matches"], true);
    assert_eq!(json["verification"]["session_scope_matches"], true);
    assert_eq!(json["source_counts"]["sessions"], 1);
    assert_eq!(json["target_counts_before"]["sessions"], 1);
}

#[test]
fn store_migrate_verify_only_detects_session_scope_mismatch_even_when_counts_match() {
    let dir = tempfile::tempdir().unwrap();
    write_local_config(dir.path(), "");

    let source_dir = dir.path().join(".switchyard").join("sessions");
    let mut source = JsonlStore::new(source_dir);
    let (source_session, _source_turn) = seed_canonical_records(&mut source);

    let target_path = dir.path().join(".switchyard").join("store.sqlite3");
    let mut target = StoreHandle::open(StoreBackend::Sqlite, target_path).unwrap();
    let (target_session, _target_turn) = seed_canonical_records(&mut target);

    let output = run_switchyard(
        dir.path(),
        &[
            "store",
            "migrate",
            "--from-backend",
            "jsonl",
            "--to-backend",
            "sqlite",
            "--verify-only",
            "--json",
        ],
    );

    assert!(
        !output.status.success(),
        "store migrate --verify-only should fail on session mismatch.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["mode"], "verify_only");
    assert_eq!(json["verification"]["matches"], false);
    assert_eq!(json["verification"]["count_matches"], true);
    assert_eq!(json["verification"]["session_scope_matches"], false);
    assert!(
        json["verification"]["missing_sessions"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|value| value == &source_session.session_id.to_string()))
    );
    assert!(
        json["verification"]["unexpected_sessions"]
            .as_array()
            .is_some_and(|items| items
                .iter()
                .any(|value| value == &target_session.session_id.to_string()))
    );
}

#[test]
fn store_migrate_session_allows_existing_other_sessions_in_target() {
    let dir = tempfile::tempdir().unwrap();
    write_local_config(dir.path(), "");

    let source_dir = dir.path().join(".switchyard").join("sessions");
    let mut source = JsonlStore::new(source_dir);
    let (session_a, _turn_a) = seed_canonical_records(&mut source);

    let target_path = dir.path().join(".switchyard").join("store.sqlite3");
    let mut target = StoreHandle::open(StoreBackend::Sqlite, target_path.clone()).unwrap();
    let (session_b, _turn_b) = seed_canonical_records(&mut target);

    let output = run_switchyard(
        dir.path(),
        &[
            "store",
            "migrate",
            "--from-backend",
            "jsonl",
            "--session",
            &session_a.session_id.to_string(),
            "--to-backend",
            "sqlite",
            "--json",
        ],
    );

    assert!(
        output.status.success(),
        "store migrate --session should allow existing unrelated sessions in target.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["selected_session"], session_a.session_id.to_string());
    assert_eq!(json["migrated"]["sessions"], 1);

    let target = StoreHandle::open(StoreBackend::Sqlite, target_path).unwrap();
    assert!(target.load_session(session_a.session_id).unwrap().is_some());
    assert!(target.load_session(session_b.session_id).unwrap().is_some());
    assert_eq!(target.list_sessions().unwrap().len(), 2);
}
