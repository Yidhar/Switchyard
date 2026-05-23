use std::path::PathBuf;
use std::time::Duration;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    ArtifactStore, EventLog, SessionCatalog, SessionEventRepository, SessionInboxRepository,
    SessionRepository, StoreError, TurnRepository,
};
use switchyard_session::{Artifact, Event, InboxEntry, Session, Turn};

pub const SQLITE_SCHEMA_VERSION: i64 = 1;
const INITIAL_MIGRATION_DESCRIPTION: &str = "initial schema";

const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS sessions (
    session_id TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    active_core TEXT NOT NULL,
    data TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS turn_index (
    turn_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS turns_log (
    sequence_id INTEGER PRIMARY KEY AUTOINCREMENT,
    turn_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    started_at TEXT NOT NULL,
    data TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_turns_log_session_sequence
    ON turns_log(session_id, sequence_id);

CREATE TABLE IF NOT EXISTS events (
    sequence_id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    timestamp TEXT NOT NULL,
    data TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_events_session_sequence
    ON events(session_id, sequence_id);
CREATE INDEX IF NOT EXISTS idx_events_turn_sequence
    ON events(turn_id, sequence_id);

CREATE TABLE IF NOT EXISTS artifacts (
    sequence_id INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id TEXT NOT NULL,
    turn_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    title TEXT NOT NULL,
    artifact_type TEXT NOT NULL,
    data TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_artifacts_turn_sequence
    ON artifacts(turn_id, sequence_id);

CREATE TABLE IF NOT EXISTS session_inbox (
    entry_id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    status TEXT NOT NULL,
    data TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_session_inbox_session_updated
    ON session_inbox(session_id, updated_at DESC);

CREATE TABLE IF NOT EXISTS store_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    description TEXT NOT NULL,
    applied_at TEXT NOT NULL
);
"#;

#[derive(Debug, Clone, Serialize)]
pub struct SqliteMigrationRecord {
    pub version: i64,
    pub description: String,
    pub applied_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SqliteSchemaInfo {
    pub schema_version: i64,
    pub store_id: Option<String>,
    pub created_at: Option<String>,
    pub migrations: Vec<SqliteMigrationRecord>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SqliteHealthInfo {
    pub integrity_errors: Vec<String>,
    pub foreign_key_issues: Vec<String>,
}

pub struct SqliteStore {
    conn: Connection,
}

impl SqliteStore {
    pub fn new(path: PathBuf) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;

        let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;

        let store = Self { conn };
        if user_version < SQLITE_SCHEMA_VERSION {
            store.initialize_schema()?;
        }
        Ok(store)
    }

    fn initialize_schema(&self) -> Result<(), StoreError> {
        self.conn.execute_batch(SCHEMA)?;
        self.conn
            .pragma_update(None, "user_version", SQLITE_SCHEMA_VERSION)?;
        self.bootstrap_metadata()?;
        Ok(())
    }

    fn bootstrap_metadata(&self) -> Result<(), StoreError> {
        let now = Utc::now().to_rfc3339();
        let store_id = Uuid::now_v7().to_string();
        let schema_version = SQLITE_SCHEMA_VERSION.to_string();

        self.conn.execute(
            "INSERT OR IGNORE INTO store_metadata (key, value) VALUES ('format', 'switchyard_sqlite')",
            [],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO store_metadata (key, value) VALUES ('store_id', ?1)",
            params![store_id],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO store_metadata (key, value) VALUES ('created_at', ?1)",
            params![now.clone()],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO store_metadata (key, value) VALUES ('schema_version', ?1)",
            params![schema_version.clone()],
        )?;
        self.conn.execute(
            "UPDATE store_metadata SET value = ?1 WHERE key = 'schema_version'",
            params![schema_version],
        )?;
        self.conn.execute(
            r#"
            INSERT OR IGNORE INTO schema_migrations (version, description, applied_at)
            VALUES (?1, ?2, ?3)
            "#,
            params![SQLITE_SCHEMA_VERSION, INITIAL_MIGRATION_DESCRIPTION, now],
        )?;
        Ok(())
    }

    fn metadata_value(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.conn
            .query_row(
                "SELECT value FROM store_metadata WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(StoreError::from)
    }

    pub fn schema_info(&self) -> Result<SqliteSchemaInfo, StoreError> {
        let schema_version = self
            .metadata_value("schema_version")?
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or(SQLITE_SCHEMA_VERSION);
        let store_id = self.metadata_value("store_id")?;
        let created_at = self.metadata_value("created_at")?;

        let mut stmt = self.conn.prepare(
            "SELECT version, description, applied_at FROM schema_migrations ORDER BY version ASC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(SqliteMigrationRecord {
                version: row.get(0)?,
                description: row.get(1)?,
                applied_at: row.get(2)?,
            })
        })?;

        let mut migrations = Vec::new();
        for row in rows {
            migrations.push(row?);
        }

        Ok(SqliteSchemaInfo {
            schema_version,
            store_id,
            created_at,
            migrations,
        })
    }

    pub fn health_info(&self) -> Result<SqliteHealthInfo, StoreError> {
        Ok(SqliteHealthInfo {
            integrity_errors: self.integrity_errors()?,
            foreign_key_issues: self.foreign_key_issues()?,
        })
    }

    fn integrity_errors(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare("PRAGMA integrity_check")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        if results.len() == 1 && results[0] == "ok" {
            Ok(Vec::new())
        } else {
            Ok(results)
        }
    }

    fn foreign_key_issues(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare("PRAGMA foreign_key_check")?;
        let rows = stmt.query_map([], |row| {
            let table = row.get::<_, String>(0)?;
            let rowid = row.get::<_, i64>(1)?;
            let parent = row.get::<_, String>(2)?;
            let fkid = row.get::<_, i64>(3)?;
            Ok(format!(
                "table={table} rowid={rowid} parent={parent} fkid={fkid}"
            ))
        })?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    fn resolve_turn(&self, turn_id: Uuid) -> Result<Uuid, StoreError> {
        let session_id = self
            .conn
            .query_row(
                "SELECT session_id FROM turn_index WHERE turn_id = ?1",
                params![turn_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        match session_id {
            Some(session_id) => session_id
                .parse::<Uuid>()
                .map_err(|_| StoreError::NotFound(turn_id)),
            None => Err(StoreError::NotFound(turn_id)),
        }
    }
}

impl SessionRepository for SqliteStore {
    fn save_session(&mut self, session: &Session) -> Result<(), StoreError> {
        let json = serde_json::to_string(session)?;
        self.conn.execute(
            r#"
            INSERT INTO sessions (session_id, created_at, updated_at, active_core, data)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(session_id) DO UPDATE SET
                created_at = excluded.created_at,
                updated_at = excluded.updated_at,
                active_core = excluded.active_core,
                data = excluded.data
            "#,
            params![
                session.session_id.to_string(),
                session.created_at.to_rfc3339(),
                session.updated_at.to_rfc3339(),
                session.active_core,
                json,
            ],
        )?;
        Ok(())
    }

    fn load_session(&self, session_id: Uuid) -> Result<Option<Session>, StoreError> {
        let json = self
            .conn
            .query_row(
                "SELECT data FROM sessions WHERE session_id = ?1",
                params![session_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        json.map(|value| serde_json::from_str(&value))
            .transpose()
            .map_err(StoreError::from)
    }

    fn delete_session(&mut self, session_id: Uuid) -> Result<(), StoreError> {
        let sid = session_id.to_string();
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM sessions WHERE session_id = ?1", params![sid])?;
        tx.execute("DELETE FROM turn_index WHERE session_id = ?1", params![sid])?;
        tx.execute("DELETE FROM turns_log WHERE session_id = ?1", params![sid])?;
        tx.execute("DELETE FROM events WHERE session_id = ?1", params![sid])?;
        tx.execute("DELETE FROM artifacts WHERE session_id = ?1", params![sid])?;
        tx.execute(
            "DELETE FROM session_inbox WHERE session_id = ?1",
            params![sid],
        )?;
        tx.commit()?;
        Ok(())
    }
}

impl TurnRepository for SqliteStore {
    fn append_turn(&mut self, turn: &Turn) -> Result<(), StoreError> {
        let json = serde_json::to_string(turn)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            r#"
            INSERT INTO turns_log (turn_id, session_id, started_at, data)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            params![
                turn.turn_id.to_string(),
                turn.session_id.to_string(),
                turn.started_at.to_rfc3339(),
                json,
            ],
        )?;
        tx.execute(
            r#"
            INSERT INTO turn_index (turn_id, session_id)
            VALUES (?1, ?2)
            ON CONFLICT(turn_id) DO UPDATE SET
                session_id = excluded.session_id
            "#,
            params![turn.turn_id.to_string(), turn.session_id.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn list_turns(&self, session_id: Uuid) -> Result<Vec<Turn>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT data FROM turns_log WHERE session_id = ?1 ORDER BY sequence_id ASC")?;
        let rows = stmt.query_map(params![session_id.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut turns = Vec::new();
        for row in rows {
            let json = row?;
            turns.push(serde_json::from_str::<Turn>(&json)?);
        }
        Ok(collapse_turns(turns))
    }

    fn delete_turn_tail(&mut self, turn_id: Uuid) -> Result<(), StoreError> {
        // Look up the smallest sequence_id for this turn (in case the row was
        // appended more than once across edits) plus its session. The
        // aggregate query always returns one row, even when the turn doesn't
        // exist — both columns come back NULL, hence the `Option` types.
        let row: (Option<i64>, Option<String>) = self.conn.query_row(
            "SELECT MIN(sequence_id), session_id \
             FROM turns_log WHERE turn_id = ?1",
            params![turn_id.to_string()],
            |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?)),
        )?;
        let (Some(min_seq), Some(session_id_str)) = row else {
            return Ok(()); // unknown turn — no-op
        };

        let tx = self.conn.transaction()?;

        // Capture the set of turn_ids in the tail before deleting anything
        // from turns_log — we need them to scrub events/artifacts/index.
        let tail_turn_ids: Vec<String> = {
            let mut stmt = tx.prepare(
                "SELECT DISTINCT turn_id FROM turns_log \
                 WHERE session_id = ?1 AND sequence_id >= ?2",
            )?;
            let rows =
                stmt.query_map(params![session_id_str, min_seq], |r| r.get::<_, String>(0))?;
            let mut ids = Vec::new();
            for r in rows {
                ids.push(r?);
            }
            ids
        };

        for tid in &tail_turn_ids {
            tx.execute("DELETE FROM events WHERE turn_id = ?1", params![tid])?;
            tx.execute("DELETE FROM artifacts WHERE turn_id = ?1", params![tid])?;
            tx.execute("DELETE FROM turn_index WHERE turn_id = ?1", params![tid])?;
        }

        tx.execute(
            "DELETE FROM turns_log WHERE session_id = ?1 AND sequence_id >= ?2",
            params![session_id_str, min_seq],
        )?;

        tx.commit()?;
        Ok(())
    }
}

impl EventLog for SqliteStore {
    fn append_event(&mut self, event: &Event) -> Result<(), StoreError> {
        let session_id = self.resolve_turn(event.turn_id)?;
        let json = serde_json::to_string(event)?;
        self.conn.execute(
            r#"
            INSERT INTO events (event_id, turn_id, session_id, timestamp, data)
            VALUES (?1, ?2, ?3, ?4, ?5)
            "#,
            params![
                event.event_id.to_string(),
                event.turn_id.to_string(),
                session_id.to_string(),
                event.timestamp.to_rfc3339(),
                json,
            ],
        )?;
        Ok(())
    }

    fn list_events(&self, turn_id: Uuid) -> Result<Vec<Event>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT data FROM events WHERE turn_id = ?1 ORDER BY sequence_id ASC")?;
        let rows = stmt.query_map(params![turn_id.to_string()], |row| row.get::<_, String>(0))?;
        let mut events = Vec::new();
        for row in rows {
            let json = row?;
            events.push(serde_json::from_str::<Event>(&json)?);
        }
        Ok(events)
    }
}

impl ArtifactStore for SqliteStore {
    fn save_artifact(&mut self, artifact: &Artifact) -> Result<(), StoreError> {
        let session_id = self.resolve_turn(artifact.turn_id)?;
        let json = serde_json::to_string(artifact)?;
        self.conn.execute(
            r#"
            INSERT INTO artifacts (artifact_id, turn_id, session_id, title, artifact_type, data)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            "#,
            params![
                artifact.artifact_id.to_string(),
                artifact.turn_id.to_string(),
                session_id.to_string(),
                artifact.title,
                artifact.artifact_type.to_string(),
                json,
            ],
        )?;
        Ok(())
    }

    fn list_artifacts(&self, turn_id: Uuid) -> Result<Vec<Artifact>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT data FROM artifacts WHERE turn_id = ?1 ORDER BY sequence_id ASC")?;
        let rows = stmt.query_map(params![turn_id.to_string()], |row| row.get::<_, String>(0))?;
        let mut artifacts = Vec::new();
        for row in rows {
            let json = row?;
            artifacts.push(serde_json::from_str::<Artifact>(&json)?);
        }
        Ok(artifacts)
    }
}

impl SessionCatalog for SqliteStore {
    fn list_sessions(&self) -> Result<Vec<Uuid>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT session_id FROM sessions ORDER BY session_id ASC")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut sessions = Vec::new();
        for row in rows {
            let session_id = row?;
            if let Ok(id) = session_id.parse::<Uuid>() {
                sessions.push(id);
            }
        }
        Ok(sessions)
    }
}

impl SessionEventRepository for SqliteStore {
    fn list_session_events(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT data FROM events WHERE session_id = ?1 ORDER BY sequence_id ASC")?;
        let rows = stmt.query_map(params![session_id.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut events = Vec::new();
        for row in rows {
            let json = row?;
            events.push(serde_json::from_str::<Event>(&json)?);
        }
        Ok(events)
    }
}

impl SessionInboxRepository for SqliteStore {
    fn save_inbox_entry(&mut self, entry: &InboxEntry) -> Result<(), StoreError> {
        let json = serde_json::to_string(entry)?;
        self.conn.execute(
            r#"
            INSERT INTO session_inbox (entry_id, session_id, created_at, updated_at, status, data)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(entry_id) DO UPDATE SET
                session_id = excluded.session_id,
                created_at = excluded.created_at,
                updated_at = excluded.updated_at,
                status = excluded.status,
                data = excluded.data
            "#,
            params![
                entry.entry_id.to_string(),
                entry.session_id.to_string(),
                entry.created_at.to_rfc3339(),
                entry.updated_at.to_rfc3339(),
                entry.status.to_string(),
                json,
            ],
        )?;
        Ok(())
    }

    fn list_inbox_entries(&self, session_id: Uuid) -> Result<Vec<InboxEntry>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT data FROM session_inbox WHERE session_id = ?1 ORDER BY updated_at DESC, entry_id ASC",
        )?;
        let rows = stmt.query_map(params![session_id.to_string()], |row| {
            row.get::<_, String>(0)
        })?;
        let mut entries = Vec::new();
        for row in rows {
            let json = row?;
            entries.push(serde_json::from_str::<InboxEntry>(&json)?);
        }
        Ok(entries)
    }
}

fn collapse_turns(raw: Vec<Turn>) -> Vec<Turn> {
    let mut seen = std::collections::HashMap::<Uuid, usize>::new();
    let mut result: Vec<Turn> = Vec::new();
    for turn in raw {
        if let Some(&idx) = seen.get(&turn.turn_id) {
            result[idx] = turn;
        } else {
            seen.insert(turn.turn_id, result.len());
            result.push(turn);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchyard_session::{ArtifactType, EventType, Session, TurnRole, TurnStatus};

    fn temp_db_path() -> (PathBuf, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("switchyard.sqlite3");
        (path, dir)
    }

    #[test]
    fn session_save_and_load() {
        let (path, _dir) = temp_db_path();
        let mut store = SqliteStore::new(path).unwrap();
        let session = Session::new("codex".to_string());
        let session_id = session.session_id;

        store.save_session(&session).unwrap();
        let loaded = store.load_session(session_id).unwrap().unwrap();
        assert_eq!(loaded.session_id, session_id);
        assert_eq!(loaded.active_core, "codex");
    }

    #[test]
    fn turn_event_artifact_roundtrip() {
        let (path, _dir) = temp_db_path();
        let mut store = SqliteStore::new(path).unwrap();
        let session = Session::new("codex".to_string());
        store.save_session(&session).unwrap();

        let turn = Turn::new(session.session_id, "codex", TurnRole::Core, "hello");
        store.append_turn(&turn).unwrap();

        let event = Event::new(
            turn.turn_id,
            EventType::TurnStarted,
            "codex",
            serde_json::json!({"step": 1}),
        );
        store.append_event(&event).unwrap();

        let mut artifact = Artifact::new(turn.turn_id, ArtifactType::CommandOutput, "stdout");
        artifact.summary = Some("done".to_string());
        store.save_artifact(&artifact).unwrap();

        assert_eq!(store.list_turns(session.session_id).unwrap().len(), 1);
        assert_eq!(store.list_events(turn.turn_id).unwrap().len(), 1);
        assert_eq!(
            store.list_session_events(session.session_id).unwrap().len(),
            1
        );
        assert_eq!(store.list_artifacts(turn.turn_id).unwrap().len(), 1);
    }

    #[test]
    fn list_sessions_survives_reopen() {
        let (path, _dir) = temp_db_path();
        let session = Session::new("claude".to_string());

        {
            let mut store = SqliteStore::new(path.clone()).unwrap();
            store.save_session(&session).unwrap();
        }

        let reopened = SqliteStore::new(path).unwrap();
        let sessions = reopened.list_sessions().unwrap();
        assert_eq!(sessions, vec![session.session_id]);
        assert!(reopened.load_session(session.session_id).unwrap().is_some());
    }

    #[test]
    fn list_turns_collapses_by_turn_id() {
        let (path, _dir) = temp_db_path();
        let mut store = SqliteStore::new(path).unwrap();
        let session = Session::new("gemini".to_string());
        store.save_session(&session).unwrap();

        let mut turn = Turn::new(session.session_id, "gemini", TurnRole::Core, "msg");
        let turn_id = turn.turn_id;
        store.append_turn(&turn).unwrap();

        turn.status = TurnStatus::Completed;
        turn.provider_response = Some("ok".to_string());
        store.append_turn(&turn).unwrap();

        let turns = store.list_turns(session.session_id).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].turn_id, turn_id);
        assert_eq!(turns[0].status, TurnStatus::Completed);
        assert_eq!(turns[0].provider_response.as_deref(), Some("ok"));
    }

    #[test]
    fn delete_turn_tail_drops_target_and_descendants() {
        let (path, _dir) = temp_db_path();
        let mut store = SqliteStore::new(path).unwrap();
        let session = Session::new("codex".to_string());
        store.save_session(&session).unwrap();

        let t1 = Turn::new(session.session_id, "codex", TurnRole::Core, "first");
        let t2 = Turn::new(session.session_id, "codex", TurnRole::Core, "second");
        let t3 = Turn::new(session.session_id, "codex", TurnRole::Core, "third");
        store.append_turn(&t1).unwrap();
        store.append_turn(&t2).unwrap();
        store.append_turn(&t3).unwrap();

        for t in [&t1, &t2, &t3] {
            store
                .append_event(&Event::new(
                    t.turn_id,
                    EventType::TurnStarted,
                    "codex",
                    serde_json::json!({}),
                ))
                .unwrap();
        }
        store
            .save_artifact(&Artifact::new(
                t2.turn_id,
                ArtifactType::RawProviderOutput,
                "raw",
            ))
            .unwrap();

        store.delete_turn_tail(t2.turn_id).unwrap();

        let surviving = store.list_turns(session.session_id).unwrap();
        assert_eq!(surviving.len(), 1, "only t1 should remain");
        assert_eq!(surviving[0].turn_id, t1.turn_id);
        assert_eq!(store.list_events(t1.turn_id).unwrap().len(), 1);
        assert!(store.list_events(t2.turn_id).unwrap().is_empty());
        assert!(store.list_events(t3.turn_id).unwrap().is_empty());
        assert!(store.list_artifacts(t2.turn_id).unwrap().is_empty());
    }

    #[test]
    fn delete_turn_tail_unknown_id_is_noop() {
        let (path, _dir) = temp_db_path();
        let mut store = SqliteStore::new(path).unwrap();
        let session = Session::new("codex".to_string());
        store.save_session(&session).unwrap();
        let t1 = Turn::new(session.session_id, "codex", TurnRole::Core, "only");
        store.append_turn(&t1).unwrap();

        store.delete_turn_tail(Uuid::now_v7()).unwrap();
        assert_eq!(store.list_turns(session.session_id).unwrap().len(), 1);
    }

    #[test]
    fn schema_info_is_bootstrapped_on_open() {
        let (path, _dir) = temp_db_path();
        let store = SqliteStore::new(path).unwrap();

        let schema = store.schema_info().unwrap();
        assert_eq!(schema.schema_version, SQLITE_SCHEMA_VERSION);
        assert!(schema.store_id.is_some());
        assert!(schema.created_at.is_some());
        assert_eq!(schema.migrations.len(), 1);
        assert_eq!(schema.migrations[0].version, SQLITE_SCHEMA_VERSION);
    }

    #[test]
    fn health_info_reports_clean_store() {
        let (path, _dir) = temp_db_path();
        let store = SqliteStore::new(path).unwrap();

        let health = store.health_info().unwrap();
        assert!(health.integrity_errors.is_empty());
        assert!(health.foreign_key_issues.is_empty());
    }

    #[test]
    fn inbox_entries_append_and_update() {
        let (path, _dir) = temp_db_path();
        let mut store = SqliteStore::new(path).unwrap();
        let session = Session::new("codex".to_string());
        store.save_session(&session).unwrap();

        let mut entry = InboxEntry::background_job_receipt(
            session.session_id,
            "gemini",
            "Gemini background job completed",
            "Gemini finished a UI draft while you were idle.",
        );
        let entry_id = entry.entry_id;
        store.save_inbox_entry(&entry).unwrap();

        let inbox = store.list_inbox_entries(session.session_id).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].entry_id, entry_id);
        assert!(inbox[0].is_unread());

        entry.mark_read();
        store.save_inbox_entry(&entry).unwrap();

        let inbox = store.list_inbox_entries(session.session_id).unwrap();
        assert_eq!(inbox.len(), 1);
        assert_eq!(inbox[0].status, switchyard_session::InboxStatus::Read);
        assert_eq!(inbox[0].provider.as_deref(), Some("gemini"));
    }

    #[test]
    fn session_delete() {
        let (path, _dir) = temp_db_path();
        let mut store = SqliteStore::new(path).unwrap();
        let session = Session::new("codex".to_string());
        let id = session.session_id;

        store.save_session(&session).unwrap();
        assert!(store.load_session(id).unwrap().is_some());

        store.delete_session(id).unwrap();
        assert!(store.load_session(id).unwrap().is_none());
    }
}
