pub mod error;
pub mod jsonl;
pub mod sqlite;
pub mod workspace_index;

use switchyard_session::{Artifact, Event, InboxEntry, Session, Turn};
use uuid::Uuid;

pub use error::StoreError;
pub use jsonl::JsonlStore;
pub use sqlite::{
    SQLITE_SCHEMA_VERSION, SqliteHealthInfo, SqliteMigrationRecord, SqliteSchemaInfo, SqliteStore,
};
pub use workspace_index::{
    WorkspaceIndex, WorkspaceIndexError, default_index_path, workspace_data_dir,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreBackend {
    Jsonl,
    Sqlite,
}

pub trait SessionRepository {
    fn save_session(&mut self, session: &Session) -> Result<(), StoreError>;
    fn load_session(&self, session_id: Uuid) -> Result<Option<Session>, StoreError>;
    fn delete_session(&mut self, session_id: Uuid) -> Result<(), StoreError>;
}

pub trait TurnRepository {
    fn append_turn(&mut self, turn: &Turn) -> Result<(), StoreError>;
    fn list_turns(&self, session_id: Uuid) -> Result<Vec<Turn>, StoreError>;

    /// Delete the turn identified by `turn_id` together with every later turn
    /// in the same session and all events / artifacts attached to those turns.
    /// Used by the edit/retry UI to rewind canonical history to the point
    /// immediately before `turn_id` so the next `run_turn` can re-dispatch a
    /// corrected message.
    ///
    /// No-op when `turn_id` is unknown. Implementations must be transactional
    /// — partial rewinds (turns gone but events left dangling) are not allowed.
    fn delete_turn_tail(&mut self, turn_id: Uuid) -> Result<(), StoreError>;
}

pub trait EventLog {
    fn append_event(&mut self, event: &Event) -> Result<(), StoreError>;
    fn list_events(&self, turn_id: Uuid) -> Result<Vec<Event>, StoreError>;
}

pub trait ArtifactStore {
    fn save_artifact(&mut self, artifact: &Artifact) -> Result<(), StoreError>;
    fn list_artifacts(&self, turn_id: Uuid) -> Result<Vec<Artifact>, StoreError>;
}

pub trait SessionCatalog {
    fn list_sessions(&self) -> Result<Vec<Uuid>, StoreError>;
}

pub trait SessionEventRepository {
    fn list_session_events(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError>;
}

pub trait SessionInboxRepository {
    fn save_inbox_entry(&mut self, entry: &InboxEntry) -> Result<(), StoreError>;
    fn list_inbox_entries(&self, session_id: Uuid) -> Result<Vec<InboxEntry>, StoreError>;
}

pub trait CanonicalStore:
    SessionRepository
    + TurnRepository
    + EventLog
    + ArtifactStore
    + SessionCatalog
    + SessionEventRepository
    + SessionInboxRepository
{
}

impl<T> CanonicalStore for T where
    T: SessionRepository
        + TurnRepository
        + EventLog
        + ArtifactStore
        + SessionCatalog
        + SessionEventRepository
        + SessionInboxRepository
{
}

pub enum StoreHandle {
    Jsonl(JsonlStore),
    Sqlite(SqliteStore),
}

impl StoreHandle {
    pub fn open(backend: StoreBackend, path: std::path::PathBuf) -> Result<Self, StoreError> {
        match backend {
            StoreBackend::Jsonl => Ok(Self::Jsonl(JsonlStore::new(path))),
            StoreBackend::Sqlite => Ok(Self::Sqlite(SqliteStore::new(path)?)),
        }
    }

    pub fn jsonl(path: std::path::PathBuf) -> Self {
        Self::Jsonl(JsonlStore::new(path))
    }

    pub fn sqlite(path: std::path::PathBuf) -> Result<Self, StoreError> {
        Ok(Self::Sqlite(SqliteStore::new(path)?))
    }

    pub fn backend(&self) -> StoreBackend {
        match self {
            Self::Jsonl(_) => StoreBackend::Jsonl,
            Self::Sqlite(_) => StoreBackend::Sqlite,
        }
    }

    pub fn sqlite_schema_info(&self) -> Result<Option<SqliteSchemaInfo>, StoreError> {
        match self {
            Self::Jsonl(_) => Ok(None),
            Self::Sqlite(store) => store.schema_info().map(Some),
        }
    }

    pub fn sqlite_health_info(&self) -> Result<Option<SqliteHealthInfo>, StoreError> {
        match self {
            Self::Jsonl(_) => Ok(None),
            Self::Sqlite(store) => store.health_info().map(Some),
        }
    }
}

impl SessionRepository for StoreHandle {
    fn save_session(&mut self, session: &Session) -> Result<(), StoreError> {
        match self {
            Self::Jsonl(store) => store.save_session(session),
            Self::Sqlite(store) => store.save_session(session),
        }
    }

    fn load_session(&self, session_id: Uuid) -> Result<Option<Session>, StoreError> {
        match self {
            Self::Jsonl(store) => store.load_session(session_id),
            Self::Sqlite(store) => store.load_session(session_id),
        }
    }

    fn delete_session(&mut self, session_id: Uuid) -> Result<(), StoreError> {
        match self {
            Self::Jsonl(store) => store.delete_session(session_id),
            Self::Sqlite(store) => store.delete_session(session_id),
        }
    }
}

impl TurnRepository for StoreHandle {
    fn append_turn(&mut self, turn: &Turn) -> Result<(), StoreError> {
        match self {
            Self::Jsonl(store) => store.append_turn(turn),
            Self::Sqlite(store) => store.append_turn(turn),
        }
    }

    fn list_turns(&self, session_id: Uuid) -> Result<Vec<Turn>, StoreError> {
        match self {
            Self::Jsonl(store) => store.list_turns(session_id),
            Self::Sqlite(store) => store.list_turns(session_id),
        }
    }

    fn delete_turn_tail(&mut self, turn_id: Uuid) -> Result<(), StoreError> {
        match self {
            Self::Jsonl(store) => store.delete_turn_tail(turn_id),
            Self::Sqlite(store) => store.delete_turn_tail(turn_id),
        }
    }
}

impl EventLog for StoreHandle {
    fn append_event(&mut self, event: &Event) -> Result<(), StoreError> {
        match self {
            Self::Jsonl(store) => store.append_event(event),
            Self::Sqlite(store) => store.append_event(event),
        }
    }

    fn list_events(&self, turn_id: Uuid) -> Result<Vec<Event>, StoreError> {
        match self {
            Self::Jsonl(store) => store.list_events(turn_id),
            Self::Sqlite(store) => store.list_events(turn_id),
        }
    }
}

impl ArtifactStore for StoreHandle {
    fn save_artifact(&mut self, artifact: &Artifact) -> Result<(), StoreError> {
        match self {
            Self::Jsonl(store) => store.save_artifact(artifact),
            Self::Sqlite(store) => store.save_artifact(artifact),
        }
    }

    fn list_artifacts(&self, turn_id: Uuid) -> Result<Vec<Artifact>, StoreError> {
        match self {
            Self::Jsonl(store) => store.list_artifacts(turn_id),
            Self::Sqlite(store) => store.list_artifacts(turn_id),
        }
    }
}

impl SessionCatalog for StoreHandle {
    fn list_sessions(&self) -> Result<Vec<Uuid>, StoreError> {
        match self {
            Self::Jsonl(store) => store.list_sessions(),
            Self::Sqlite(store) => store.list_sessions(),
        }
    }
}

impl SessionEventRepository for StoreHandle {
    fn list_session_events(&self, session_id: Uuid) -> Result<Vec<Event>, StoreError> {
        match self {
            Self::Jsonl(store) => store.list_session_events(session_id),
            Self::Sqlite(store) => store.list_session_events(session_id),
        }
    }
}

impl SessionInboxRepository for StoreHandle {
    fn save_inbox_entry(&mut self, entry: &InboxEntry) -> Result<(), StoreError> {
        match self {
            Self::Jsonl(store) => store.save_inbox_entry(entry),
            Self::Sqlite(store) => store.save_inbox_entry(entry),
        }
    }

    fn list_inbox_entries(&self, session_id: Uuid) -> Result<Vec<InboxEntry>, StoreError> {
        match self {
            Self::Jsonl(store) => store.list_inbox_entries(session_id),
            Self::Sqlite(store) => store.list_inbox_entries(session_id),
        }
    }
}
