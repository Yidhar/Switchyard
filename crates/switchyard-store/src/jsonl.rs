use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::{
    ArtifactStore, EventLog, SessionCatalog, SessionEventRepository, SessionRepository, StoreError,
    TurnRepository,
};
use switchyard_session::{Artifact, Event, Session, Turn};

/// File-based store using one directory per session.
///
/// Layout:
/// ```text
/// {base_dir}/{session_id}/
///     session.json
///     turns.jsonl
///     events.jsonl
///     artifacts.jsonl
/// ```
///
/// # Thread safety
///
/// `JsonlStore` is **not thread-safe**. It uses interior mutability (`RefCell`)
/// for lazy index loading, which is safe for single-threaded use only.
/// For shared access across async tasks or threads, wrap it in
/// `Arc<Mutex<JsonlStore>>` (sync) or `Arc<tokio::sync::Mutex<JsonlStore>>` (async).
pub struct JsonlStore {
    base_dir: PathBuf,
    /// In-memory index: turn_id -> session_id.
    /// Uses RefCell so both read and write paths can trigger lazy loading.
    index: RefCell<TurnIndex>,
}

struct TurnIndex {
    map: HashMap<Uuid, Uuid>,
    built: bool,
}

impl JsonlStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self {
            base_dir,
            index: RefCell::new(TurnIndex {
                map: HashMap::new(),
                built: false,
            }),
        }
    }

    fn session_dir(&self, session_id: Uuid) -> PathBuf {
        self.base_dir.join(session_id.to_string())
    }

    fn ensure_session_dir(&self, session_id: Uuid) -> Result<PathBuf, StoreError> {
        let dir = self.session_dir(session_id);
        fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// List all session IDs by scanning directory names.
    pub fn list_sessions(&self) -> Result<Vec<Uuid>, StoreError> {
        let read_dir = match fs::read_dir(&self.base_dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut sessions = Vec::new();
        for entry in read_dir {
            let entry = entry?;
            if entry.file_type()?.is_dir()
                && let Ok(id) = entry.file_name().to_string_lossy().parse::<Uuid>()
            {
                sessions.push(id);
            }
        }
        sessions.sort();
        Ok(sessions)
    }

    /// Resolve session_id for a given turn_id.
    /// Triggers lazy index rebuild on first call (works from both &self and &mut self).
    fn resolve_turn(&self, turn_id: Uuid) -> Result<Uuid, StoreError> {
        // Fast path: already in index
        if let Some(&sid) = self.index.borrow().map.get(&turn_id) {
            return Ok(sid);
        }

        // Lazy rebuild
        {
            let mut idx = self.index.borrow_mut();
            if !idx.built {
                Self::rebuild_index(&self.base_dir, &mut idx)?;
            }
        }

        self.index
            .borrow()
            .map
            .get(&turn_id)
            .copied()
            .ok_or(StoreError::NotFound(turn_id))
    }

    /// Insert a turn_id -> session_id mapping into the index.
    fn index_turn(&self, turn_id: Uuid, session_id: Uuid) {
        self.index.borrow_mut().map.insert(turn_id, session_id);
    }

    /// Rebuild index from disk.
    fn rebuild_index(base_dir: &Path, idx: &mut TurnIndex) -> Result<(), StoreError> {
        idx.built = true;
        let read_dir = match fs::read_dir(base_dir) {
            Ok(rd) => rd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e.into()),
        };
        for entry in read_dir {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let session_id = match entry.file_name().to_string_lossy().parse::<Uuid>() {
                Ok(id) => id,
                Err(_) => continue,
            };
            let turns_path = entry.path().join("turns.jsonl");
            let file = match fs::File::open(&turns_path) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            for line in BufReader::new(file).lines() {
                let line = line?;
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(turn) = serde_json::from_str::<Turn>(&line) {
                    idx.map.insert(turn.turn_id, session_id);
                }
            }
        }
        Ok(())
    }
}

impl SessionRepository for JsonlStore {
    fn save_session(&mut self, session: &Session) -> Result<(), StoreError> {
        let dir = self.ensure_session_dir(session.session_id)?;
        let path = dir.join("session.json");
        let json = serde_json::to_string_pretty(session)?;
        fs::write(path, json)?;
        Ok(())
    }

    fn load_session(&self, session_id: Uuid) -> Result<Option<Session>, StoreError> {
        let path = self.session_dir(session_id).join("session.json");
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let session: Session = serde_json::from_str(&content)?;
        Ok(Some(session))
    }
}

impl TurnRepository for JsonlStore {
    fn append_turn(&mut self, turn: &Turn) -> Result<(), StoreError> {
        let dir = self.ensure_session_dir(turn.session_id)?;
        append_jsonl(&dir.join("turns.jsonl"), turn)?;
        self.index_turn(turn.turn_id, turn.session_id);
        Ok(())
    }

    /// Returns turns collapsed by turn_id (latest entry wins).
    fn list_turns(&self, session_id: Uuid) -> Result<Vec<Turn>, StoreError> {
        let path = self.session_dir(session_id).join("turns.jsonl");
        let raw: Vec<Turn> = read_jsonl(&path)?;
        Ok(collapse_turns(raw))
    }
}

impl JsonlStore {
    /// Read all events for a session in one pass (avoids N+1 per-turn reads).
    pub fn list_session_events(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError> {
        read_jsonl(&self.session_dir(session_id).join("events.jsonl"))
    }
}

impl SessionCatalog for JsonlStore {
    fn list_sessions(&self) -> Result<Vec<Uuid>, StoreError> {
        JsonlStore::list_sessions(self)
    }
}

impl SessionEventRepository for JsonlStore {
    fn list_session_events(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError> {
        JsonlStore::list_session_events(self, session_id)
    }
}

impl EventLog for JsonlStore {
    fn append_event(&mut self, event: &Event) -> Result<(), StoreError> {
        let session_id = self.resolve_turn(event.turn_id)?;
        append_jsonl(&self.session_dir(session_id).join("events.jsonl"), event)
    }

    fn list_events(&self, turn_id: Uuid) -> Result<Vec<Event>, StoreError> {
        let session_id = self.resolve_turn(turn_id)?;
        let all: Vec<Event> = read_jsonl(&self.session_dir(session_id).join("events.jsonl"))?;
        Ok(all.into_iter().filter(|e| e.turn_id == turn_id).collect())
    }
}

impl ArtifactStore for JsonlStore {
    fn save_artifact(&mut self, artifact: &Artifact) -> Result<(), StoreError> {
        let session_id = self.resolve_turn(artifact.turn_id)?;
        append_jsonl(
            &self.session_dir(session_id).join("artifacts.jsonl"),
            artifact,
        )
    }

    fn list_artifacts(&self, turn_id: Uuid) -> Result<Vec<Artifact>, StoreError> {
        let session_id = self.resolve_turn(turn_id)?;
        let all: Vec<Artifact> = read_jsonl(&self.session_dir(session_id).join("artifacts.jsonl"))?;
        Ok(all.into_iter().filter(|a| a.turn_id == turn_id).collect())
    }
}

/// Collapse turn log entries: keep only the latest entry per turn_id,
/// preserving insertion order of first appearance.
fn collapse_turns(raw: Vec<Turn>) -> Vec<Turn> {
    let mut seen = HashMap::<Uuid, usize>::new();
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

fn append_jsonl<T: serde::Serialize>(path: &Path, item: &T) -> Result<(), StoreError> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    let line = serde_json::to_string(item)?;
    writeln!(file, "{line}")?;
    file.sync_all()?;
    Ok(())
}

fn read_jsonl<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>, StoreError> {
    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let reader = BufReader::new(file);
    let mut items = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let item: T = serde_json::from_str(&line)?;
        items.push(item);
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchyard_session::*;

    fn temp_store() -> (JsonlStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let store = JsonlStore::new(dir.path().to_path_buf());
        (store, dir)
    }

    #[test]
    fn session_save_and_load() {
        let (mut store, _dir) = temp_store();
        let session = Session::new("codex".to_string());
        let id = session.session_id;

        store.save_session(&session).unwrap();
        let loaded = store.load_session(id).unwrap().unwrap();
        assert_eq!(loaded.session_id, id);
        assert_eq!(loaded.active_core, "codex");
    }

    #[test]
    fn session_load_nonexistent_returns_none() {
        let (store, _dir) = temp_store();
        let result = store.load_session(Uuid::now_v7()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn turn_append_and_list() {
        let (mut store, _dir) = temp_store();
        let session = Session::new("codex".to_string());
        store.save_session(&session).unwrap();

        let t1 = Turn::new(session.session_id, "codex", TurnRole::Core, "msg 1");
        let t2 = Turn::new(session.session_id, "codex", TurnRole::Core, "msg 2");
        store.append_turn(&t1).unwrap();
        store.append_turn(&t2).unwrap();

        let turns = store.list_turns(session.session_id).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].user_message, "msg 1");
        assert_eq!(turns[1].user_message, "msg 2");
    }

    #[test]
    fn event_append_and_list() {
        let (mut store, _dir) = temp_store();
        let session = Session::new("codex".to_string());
        store.save_session(&session).unwrap();

        let turn = Turn::new(session.session_id, "codex", TurnRole::Core, "hello");
        store.append_turn(&turn).unwrap();

        let e1 = Event::new(
            turn.turn_id,
            EventType::TurnStarted,
            "codex",
            serde_json::json!({}),
        );
        let e2 = Event::new(
            turn.turn_id,
            EventType::ItemUpdated,
            "codex",
            serde_json::json!({"text": "hi"}),
        );
        let e3 = Event::new(
            turn.turn_id,
            EventType::TurnCompleted,
            "codex",
            serde_json::json!({}),
        );
        store.append_event(&e1).unwrap();
        store.append_event(&e2).unwrap();
        store.append_event(&e3).unwrap();

        let events = store.list_events(turn.turn_id).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, EventType::TurnStarted);
        assert_eq!(events[2].event_type, EventType::TurnCompleted);
    }

    #[test]
    fn events_filtered_by_turn() {
        let (mut store, _dir) = temp_store();
        let session = Session::new("codex".to_string());
        store.save_session(&session).unwrap();

        let t1 = Turn::new(session.session_id, "codex", TurnRole::Core, "msg 1");
        let t2 = Turn::new(session.session_id, "codex", TurnRole::Core, "msg 2");
        store.append_turn(&t1).unwrap();
        store.append_turn(&t2).unwrap();

        store
            .append_event(&Event::new(
                t1.turn_id,
                EventType::TurnStarted,
                "codex",
                serde_json::json!({}),
            ))
            .unwrap();
        store
            .append_event(&Event::new(
                t2.turn_id,
                EventType::TurnStarted,
                "codex",
                serde_json::json!({}),
            ))
            .unwrap();
        store
            .append_event(&Event::new(
                t1.turn_id,
                EventType::TurnCompleted,
                "codex",
                serde_json::json!({}),
            ))
            .unwrap();

        let t1_events = store.list_events(t1.turn_id).unwrap();
        assert_eq!(t1_events.len(), 2);

        let t2_events = store.list_events(t2.turn_id).unwrap();
        assert_eq!(t2_events.len(), 1);
    }

    #[test]
    fn artifact_save_and_list() {
        let (mut store, _dir) = temp_store();
        let session = Session::new("codex".to_string());
        store.save_session(&session).unwrap();

        let turn = Turn::new(session.session_id, "codex", TurnRole::Core, "fix bug");
        store.append_turn(&turn).unwrap();

        let mut a = Artifact::new(turn.turn_id, ArtifactType::FileChange, "modified main.rs");
        a.path = Some(PathBuf::from("src/main.rs"));
        store.save_artifact(&a).unwrap();

        let artifacts = store.list_artifacts(turn.turn_id).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].title, "modified main.rs");
        assert_eq!(artifacts[0].artifact_type, ArtifactType::FileChange);
    }

    #[test]
    fn list_sessions() {
        let (mut store, _dir) = temp_store();
        let s1 = Session::new("codex".to_string());
        let s2 = Session::new("claude".to_string());
        store.save_session(&s1).unwrap();
        store.save_session(&s2).unwrap();

        let sessions = store.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn list_sessions_empty_dir() {
        let (store, _dir) = temp_store();
        let sessions = store.list_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn session_update_overwrites() {
        let (mut store, _dir) = temp_store();
        let mut session = Session::new("codex".to_string());
        store.save_session(&session).unwrap();

        session.active_core = "claude".to_string();
        session.summary = Some("switched to claude".to_string());
        store.save_session(&session).unwrap();

        let loaded = store.load_session(session.session_id).unwrap().unwrap();
        assert_eq!(loaded.active_core, "claude");
        assert_eq!(loaded.summary.as_deref(), Some("switched to claude"));
    }

    #[test]
    fn delegate_turn_stored_correctly() {
        let (mut store, _dir) = temp_store();
        let session = Session::new("codex".to_string());
        store.save_session(&session).unwrap();

        let turn = Turn::new_delegate(
            session.session_id,
            "claude",
            TurnRole::Reviewer,
            "review auth module",
            "codex",
        );
        store.append_turn(&turn).unwrap();

        let turns = store.list_turns(session.session_id).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].origin, TurnOrigin::Delegate);
        assert_eq!(turns[0].role, TurnRole::Reviewer);
        assert_eq!(turns[0].delegated_by.as_deref(), Some("codex"));
    }

    #[test]
    fn index_survives_cold_start_write_path() {
        let dir = tempfile::tempdir().unwrap();

        // Phase 1: write data with one store instance
        let turn_id;
        {
            let mut store = JsonlStore::new(dir.path().to_path_buf());
            let session = Session::new("codex".to_string());
            store.save_session(&session).unwrap();
            let turn = Turn::new(session.session_id, "codex", TurnRole::Core, "hello");
            turn_id = turn.turn_id;
            store.append_turn(&turn).unwrap();
        }

        // Phase 2: new store, write triggers rebuild
        let mut store2 = JsonlStore::new(dir.path().to_path_buf());
        let event = Event::new(
            turn_id,
            EventType::TurnStarted,
            "codex",
            serde_json::json!({}),
        );
        store2.append_event(&event).unwrap();

        let events = store2.list_events(turn_id).unwrap();
        assert_eq!(events.len(), 1);
    }

    /// Read-only access on a cold store must trigger lazy index rebuild.
    #[test]
    fn cold_start_read_only_triggers_index_rebuild() {
        let dir = tempfile::tempdir().unwrap();

        // Phase 1: populate data
        let turn_id;
        {
            let mut store = JsonlStore::new(dir.path().to_path_buf());
            let session = Session::new("codex".to_string());
            store.save_session(&session).unwrap();
            let turn = Turn::new(session.session_id, "codex", TurnRole::Core, "hello");
            turn_id = turn.turn_id;
            store.append_turn(&turn).unwrap();
            store
                .append_event(&Event::new(
                    turn_id,
                    EventType::TurnStarted,
                    "codex",
                    serde_json::json!({}),
                ))
                .unwrap();
            store
                .append_event(&Event::new(
                    turn_id,
                    EventType::TurnCompleted,
                    "codex",
                    serde_json::json!({}),
                ))
                .unwrap();

            let mut a = Artifact::new(turn_id, ArtifactType::FileChange, "main.rs");
            a.path = Some(PathBuf::from("src/main.rs"));
            store.save_artifact(&a).unwrap();
        }

        // Phase 2: brand new store, NO writes — read-only queries must work
        let store2 = JsonlStore::new(dir.path().to_path_buf());
        let events = store2.list_events(turn_id).unwrap();
        assert_eq!(events.len(), 2);

        let artifacts = store2.list_artifacts(turn_id).unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].title, "main.rs");
    }

    #[test]
    fn delegate_events_stored_in_canonical_log() {
        let (mut store, _dir) = temp_store();
        let session = Session::new("codex".to_string());
        store.save_session(&session).unwrap();

        let turn = Turn::new(session.session_id, "codex", TurnRole::Core, "do work");
        store.append_turn(&turn).unwrap();

        let e1 = Event::new(
            turn.turn_id,
            EventType::DelegateRequested,
            "codex",
            serde_json::json!({"peer": "claude"}),
        );
        let e2 = Event::new(
            turn.turn_id,
            EventType::DelegateCompleted,
            "claude",
            serde_json::json!({"status": "success"}),
        );
        let e3 = Event::new(
            turn.turn_id,
            EventType::ArtifactReady,
            "codex",
            serde_json::json!({"path": "src/main.rs"}),
        );
        store.append_event(&e1).unwrap();
        store.append_event(&e2).unwrap();
        store.append_event(&e3).unwrap();

        let events = store.list_events(turn.turn_id).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, EventType::DelegateRequested);
        assert_eq!(events[1].event_type, EventType::DelegateCompleted);
        assert_eq!(events[2].event_type, EventType::ArtifactReady);
    }

    #[test]
    fn list_turns_collapses_by_turn_id() {
        let (mut store, _dir) = temp_store();
        let session = Session::new("codex".to_string());
        store.save_session(&session).unwrap();

        let mut turn = Turn::new(session.session_id, "codex", TurnRole::Core, "fix bug");
        store.append_turn(&turn).unwrap();

        // Update same turn (simulates runner re-appending with new status)
        turn.status = TurnStatus::Completed;
        turn.provider_response = Some("done".to_string());
        store.append_turn(&turn).unwrap();

        let turns = store.list_turns(session.session_id).unwrap();
        assert_eq!(turns.len(), 1);
        assert_eq!(turns[0].status, TurnStatus::Completed);
        assert_eq!(turns[0].provider_response.as_deref(), Some("done"));
    }
}
