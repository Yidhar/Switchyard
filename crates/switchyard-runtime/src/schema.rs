pub const SQLITE_SCHEMA_VERSION: i64 = 1;
pub const INITIAL_MIGRATION_DESCRIPTION: &str = "runtime authority initial schema";

pub const SCHEMA: &str = r#"
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA busy_timeout = 5000;

CREATE TABLE IF NOT EXISTS host_jobs (
    job_id TEXT PRIMARY KEY,

    workspace_id TEXT,
    owner_session_id TEXT,
    callback_session_id TEXT,

    provider TEXT NOT NULL,
    task TEXT NOT NULL,
    cwd TEXT NOT NULL,

    status TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 0,

    worker_mode TEXT,
    pid INTEGER,
    job_token_hash TEXT,

    client_request_id TEXT,

    wait_timeout_count INTEGER NOT NULL DEFAULT 0,

    last_event TEXT,
    last_output_preview TEXT,
    stdout_bytes INTEGER NOT NULL DEFAULT 0,
    stderr_bytes INTEGER NOT NULL DEFAULT 0,

    result_ready INTEGER NOT NULL DEFAULT 0,
    artifact_count INTEGER NOT NULL DEFAULT 0,
    result_summary TEXT,
    error TEXT,

    worker_session_id TEXT,
    turn_id TEXT,
    callback_inbox_id TEXT,
    callback_emitted_at TEXT,

    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT,
    last_heartbeat_at TEXT
);

CREATE TABLE IF NOT EXISTS runtime_events (
    event_id INTEGER PRIMARY KEY AUTOINCREMENT,

    workspace_id TEXT,
    session_id TEXT,

    aggregate_type TEXT NOT NULL,
    aggregate_id TEXT NOT NULL,
    aggregate_version INTEGER NOT NULL,

    event_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,

    occurred_at TEXT NOT NULL,
    source TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS worker_instances (
    instance_id TEXT PRIMARY KEY,

    workspace_id TEXT,
    session_id TEXT,

    provider TEXT NOT NULL,
    kind TEXT NOT NULL,
    label TEXT,

    state TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 0,

    pid INTEGER,
    resume_token TEXT,
    in_flight_turn_id TEXT,

    spawned_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_heartbeat_at TEXT
);

CREATE TABLE IF NOT EXISTS runtime_turns (
    turn_id TEXT PRIMARY KEY,

    workspace_id TEXT,
    session_id TEXT NOT NULL,
    provider TEXT NOT NULL,

    role TEXT NOT NULL,
    status TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 0,

    user_message TEXT,
    response TEXT,
    error TEXT,

    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    completed_at TEXT
);

CREATE TABLE IF NOT EXISTS worker_spool (
    spool_id INTEGER PRIMARY KEY AUTOINCREMENT,
    workspace_id TEXT,
    job_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    report_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    token_hash TEXT,
    occurred_at TEXT NOT NULL,
    drained_at TEXT,
    UNIQUE(job_id, seq)
);

CREATE TABLE IF NOT EXISTS runtime_metadata (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    description TEXT NOT NULL,
    applied_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_host_jobs_owner_session
    ON host_jobs(owner_session_id, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_host_jobs_callback_session
    ON host_jobs(callback_session_id, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_host_jobs_status
    ON host_jobs(status, updated_at DESC);

CREATE UNIQUE INDEX IF NOT EXISTS idx_host_jobs_client_request
    ON host_jobs(client_request_id)
    WHERE client_request_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_runtime_events_session_event
    ON runtime_events(session_id, event_id);

CREATE INDEX IF NOT EXISTS idx_runtime_events_aggregate
    ON runtime_events(aggregate_type, aggregate_id, aggregate_version);

CREATE INDEX IF NOT EXISTS idx_runtime_events_workspace_event
    ON runtime_events(workspace_id, event_id);

CREATE INDEX IF NOT EXISTS idx_runtime_events_event_id
    ON runtime_events(event_id);

CREATE INDEX IF NOT EXISTS idx_worker_instances_session
    ON worker_instances(session_id, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_worker_instances_state
    ON worker_instances(state, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_runtime_turns_session
    ON runtime_turns(session_id, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_worker_spool_job_seq
    ON worker_spool(job_id, seq);
"#;
