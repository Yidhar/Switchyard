use std::path::PathBuf;

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    ArtifactStore, EventLog, SessionCatalog, SessionEventRepository, SessionRepository, StoreError,
    TurnRepository,
};
use switchyard_session::{Artifact, Event, Session, Turn};

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
        let store = Self { conn };
        store.initialize_schema()?;
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
}
