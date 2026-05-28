use std::{path::Path, str::FromStr, time::Duration};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    RuntimeError,
    protocol::{
        CreateHostJob, HostJobMutation, HostJobRecord, HostJobStatus, RuntimeEventRecord,
        RuntimeSnapshot, RuntimeWrite,
    },
    schema::{INITIAL_MIGRATION_DESCRIPTION, SCHEMA, SQLITE_SCHEMA_VERSION},
};

const HOST_JOB_COLUMNS: &str = r#"
    job_id,
    workspace_id,
    owner_session_id,
    callback_session_id,
    provider,
    task,
    cwd,
    status,
    version,
    worker_mode,
    pid,
    job_token_hash,
    client_request_id,
    wait_timeout_count,
    last_event,
    last_output_preview,
    stdout_bytes,
    stderr_bytes,
    result_ready,
    artifact_count,
    result_summary,
    error,
    worker_session_id,
    turn_id,
    callback_inbox_id,
    callback_emitted_at,
    created_at,
    updated_at,
    started_at,
    completed_at,
    last_heartbeat_at
"#;

const RUNTIME_EVENT_COLUMNS: &str = r#"
    event_id,
    workspace_id,
    session_id,
    aggregate_type,
    aggregate_id,
    aggregate_version,
    event_type,
    payload_json,
    occurred_at,
    source
"#;

pub struct RuntimeDb {
    conn: Connection,
}

impl RuntimeDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RuntimeError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA busy_timeout = 5000;
            "#,
        )?;

        let user_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let db = Self { conn };
        if user_version < SQLITE_SCHEMA_VERSION {
            db.initialize_schema()?;
        }
        Ok(db)
    }

    fn initialize_schema(&self) -> Result<(), RuntimeError> {
        self.conn.execute_batch(SCHEMA)?;
        self.conn
            .pragma_update(None, "user_version", SQLITE_SCHEMA_VERSION)?;
        self.bootstrap_metadata()?;
        Ok(())
    }

    fn bootstrap_metadata(&self) -> Result<(), RuntimeError> {
        let now = Utc::now().to_rfc3339();
        let runtime_id = Uuid::now_v7().to_string();
        let schema_version = SQLITE_SCHEMA_VERSION.to_string();

        self.conn.execute(
            "INSERT OR IGNORE INTO runtime_metadata (key, value) VALUES ('format', 'switchyard_runtime_sqlite')",
            [],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO runtime_metadata (key, value) VALUES ('runtime_id', ?1)",
            params![runtime_id],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO runtime_metadata (key, value) VALUES ('created_at', ?1)",
            params![now.clone()],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO runtime_metadata (key, value) VALUES ('schema_version', ?1)",
            params![schema_version.clone()],
        )?;
        self.conn.execute(
            "UPDATE runtime_metadata SET value = ?1 WHERE key = 'schema_version'",
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

    pub fn schema_version(&self) -> Result<i64, RuntimeError> {
        self.conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(RuntimeError::from)
    }

    pub fn create_host_job(
        &mut self,
        input: CreateHostJob,
    ) -> Result<RuntimeWrite<HostJobRecord>, RuntimeError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        if let Some(request_id) = input.client_request_id.as_deref() {
            if let Some(existing) = get_host_job_by_client_request_tx(&tx, request_id)? {
                tx.commit()?;
                return Ok(RuntimeWrite::idempotent(existing));
            }
        }

        let now = Utc::now();
        tx.execute(
            r#"
            INSERT INTO host_jobs (
                job_id,
                workspace_id,
                owner_session_id,
                callback_session_id,
                provider,
                task,
                cwd,
                status,
                version,
                worker_mode,
                pid,
                job_token_hash,
                client_request_id,
                wait_timeout_count,
                last_event,
                last_output_preview,
                stdout_bytes,
                stderr_bytes,
                result_ready,
                artifact_count,
                result_summary,
                error,
                worker_session_id,
                turn_id,
                callback_inbox_id,
                callback_emitted_at,
                created_at,
                updated_at,
                started_at,
                completed_at,
                last_heartbeat_at
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20,
                ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31
            )
            "#,
            params![
                input.job_id.to_string(),
                input.workspace_id,
                uuid_to_text(input.owner_session_id),
                uuid_to_text(input.callback_session_id),
                input.provider,
                input.task,
                input.cwd.to_string_lossy().to_string(),
                HostJobStatus::Queued.to_string(),
                0_i64,
                input.worker_mode,
                Option::<i64>::None,
                input.job_token_hash,
                input.client_request_id,
                0_i64,
                Option::<String>::None,
                Option::<String>::None,
                0_i64,
                0_i64,
                0_i64,
                0_i64,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                now.to_rfc3339(),
                now.to_rfc3339(),
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
            ],
        )?;

        let record = get_host_job_tx(&tx, input.job_id)?
            .ok_or(RuntimeError::HostJobNotFound(input.job_id))?;
        let event = append_runtime_event_tx(
            &tx,
            record.workspace_id.clone(),
            record.owner_session_id.or(record.callback_session_id),
            "host_job",
            &record.job_id.to_string(),
            record.version,
            "host_job.created",
            input.payload,
            input.source,
        )?;
        tx.commit()?;
        Ok(RuntimeWrite::committed(record, event))
    }

    pub fn transition_host_job<F>(
        &mut self,
        job_id: Uuid,
        event_type: impl Into<String>,
        source: impl Into<String>,
        payload: Value,
        mutate: F,
    ) -> Result<RuntimeWrite<HostJobRecord>, RuntimeError>
    where
        F: FnOnce(&mut HostJobMutation),
    {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = get_host_job_tx(&tx, job_id)?.ok_or(RuntimeError::HostJobNotFound(job_id))?;
        let mut next = HostJobMutation::from_record(&current);
        mutate(&mut next);
        current.status.validate_transition(next.status)?;

        let now = Utc::now();
        let next_version = current.version + 1;
        tx.execute(
            r#"
            UPDATE host_jobs SET
                owner_session_id = ?1,
                callback_session_id = ?2,
                status = ?3,
                version = ?4,
                worker_mode = ?5,
                pid = ?6,
                job_token_hash = ?7,
                wait_timeout_count = ?8,
                last_event = ?9,
                last_output_preview = ?10,
                stdout_bytes = ?11,
                stderr_bytes = ?12,
                result_ready = ?13,
                artifact_count = ?14,
                result_summary = ?15,
                error = ?16,
                worker_session_id = ?17,
                turn_id = ?18,
                callback_inbox_id = ?19,
                callback_emitted_at = ?20,
                updated_at = ?21,
                started_at = ?22,
                completed_at = ?23,
                last_heartbeat_at = ?24
            WHERE job_id = ?25
            "#,
            params![
                uuid_to_text(next.owner_session_id),
                uuid_to_text(next.callback_session_id),
                next.status.to_string(),
                next_version,
                next.worker_mode,
                next.pid.map(i64::from),
                next.job_token_hash,
                i64::from(next.wait_timeout_count),
                next.last_event,
                next.last_output_preview,
                to_i64(next.stdout_bytes, "stdout_bytes")?,
                to_i64(next.stderr_bytes, "stderr_bytes")?,
                bool_to_i64(next.result_ready),
                to_i64(next.artifact_count as u64, "artifact_count")?,
                next.result_summary,
                next.error,
                uuid_to_text(next.worker_session_id),
                uuid_to_text(next.turn_id),
                uuid_to_text(next.callback_inbox_id),
                datetime_to_text(next.callback_emitted_at),
                now.to_rfc3339(),
                datetime_to_text(next.started_at),
                datetime_to_text(next.completed_at),
                datetime_to_text(next.last_heartbeat_at),
                job_id.to_string(),
            ],
        )?;

        let record = get_host_job_tx(&tx, job_id)?.ok_or(RuntimeError::HostJobNotFound(job_id))?;
        let event = append_runtime_event_tx(
            &tx,
            record.workspace_id.clone(),
            record.owner_session_id.or(record.callback_session_id),
            "host_job",
            &record.job_id.to_string(),
            record.version,
            event_type,
            payload,
            source,
        )?;
        tx.commit()?;
        Ok(RuntimeWrite::committed(record, event))
    }

    pub fn get_host_job(&self, job_id: Uuid) -> Result<Option<HostJobRecord>, RuntimeError> {
        get_host_job_conn(&self.conn, job_id)
    }

    pub fn list_host_jobs_for_session(
        &self,
        session_id: Uuid,
        limit: usize,
    ) -> Result<Vec<HostJobRecord>, RuntimeError> {
        let sql = format!(
            "SELECT {HOST_JOB_COLUMNS} FROM host_jobs \
             WHERE owner_session_id = ?1 OR callback_session_id = ?1 \
             ORDER BY updated_at DESC, job_id ASC LIMIT ?2"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(
            params![session_id.to_string(), limit as i64],
            map_host_job_row,
        )?;
        collect_rows(rows)
    }

    pub fn runtime_events_after(
        &self,
        session_id: Option<Uuid>,
        after_event_id: i64,
        limit: usize,
    ) -> Result<Vec<RuntimeEventRecord>, RuntimeError> {
        let limit = limit.max(1) as i64;
        let sql = if session_id.is_some() {
            format!(
                "SELECT {RUNTIME_EVENT_COLUMNS} FROM runtime_events \
                 WHERE session_id = ?1 AND event_id > ?2 \
                 ORDER BY event_id ASC LIMIT ?3"
            )
        } else {
            format!(
                "SELECT {RUNTIME_EVENT_COLUMNS} FROM runtime_events \
                 WHERE event_id > ?1 ORDER BY event_id ASC LIMIT ?2"
            )
        };

        let mut stmt = self.conn.prepare(&sql)?;
        if let Some(session_id) = session_id {
            let rows = stmt.query_map(
                params![session_id.to_string(), after_event_id, limit],
                map_runtime_event_row,
            )?;
            collect_rows(rows)
        } else {
            let rows = stmt.query_map(params![after_event_id, limit], map_runtime_event_row)?;
            collect_rows(rows)
        }
    }

    pub fn snapshot_for_session(
        &self,
        session_id: Uuid,
        after_event_id: i64,
        event_limit: usize,
        job_limit: usize,
    ) -> Result<RuntimeSnapshot, RuntimeError> {
        Ok(RuntimeSnapshot {
            max_event_id: self.max_event_id()?,
            host_jobs: self.list_host_jobs_for_session(session_id, job_limit)?,
            events: self.runtime_events_after(Some(session_id), after_event_id, event_limit)?,
        })
    }

    pub fn max_event_id(&self) -> Result<i64, RuntimeError> {
        self.conn
            .query_row(
                "SELECT COALESCE(MAX(event_id), 0) FROM runtime_events",
                [],
                |row| row.get(0),
            )
            .map_err(RuntimeError::from)
    }
}

fn get_host_job_conn(
    conn: &Connection,
    job_id: Uuid,
) -> Result<Option<HostJobRecord>, RuntimeError> {
    let sql = format!("SELECT {HOST_JOB_COLUMNS} FROM host_jobs WHERE job_id = ?1");
    conn.query_row(&sql, params![job_id.to_string()], map_host_job_row)
        .optional()
        .map_err(RuntimeError::from)
        .and_then(|row| row.transpose())
}

fn get_host_job_tx(
    tx: &Transaction<'_>,
    job_id: Uuid,
) -> Result<Option<HostJobRecord>, RuntimeError> {
    let sql = format!("SELECT {HOST_JOB_COLUMNS} FROM host_jobs WHERE job_id = ?1");
    tx.query_row(&sql, params![job_id.to_string()], map_host_job_row)
        .optional()
        .map_err(RuntimeError::from)
        .and_then(|row| row.transpose())
}

fn get_host_job_by_client_request_tx(
    tx: &Transaction<'_>,
    request_id: &str,
) -> Result<Option<HostJobRecord>, RuntimeError> {
    let sql = format!("SELECT {HOST_JOB_COLUMNS} FROM host_jobs WHERE client_request_id = ?1");
    tx.query_row(&sql, params![request_id], map_host_job_row)
        .optional()
        .map_err(RuntimeError::from)
        .and_then(|row| row.transpose())
}

fn append_runtime_event_tx(
    tx: &Transaction<'_>,
    workspace_id: Option<String>,
    session_id: Option<Uuid>,
    aggregate_type: impl Into<String>,
    aggregate_id: &str,
    aggregate_version: i64,
    event_type: impl Into<String>,
    payload: Value,
    source: impl Into<String>,
) -> Result<RuntimeEventRecord, RuntimeError> {
    let aggregate_type = aggregate_type.into();
    let event_type = event_type.into();
    let source = source.into();
    let payload_json = serde_json::to_string(&payload)?;
    let occurred_at = Utc::now();

    tx.execute(
        r#"
        INSERT INTO runtime_events (
            workspace_id,
            session_id,
            aggregate_type,
            aggregate_id,
            aggregate_version,
            event_type,
            payload_json,
            occurred_at,
            source
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
        "#,
        params![
            workspace_id,
            uuid_to_text(session_id),
            aggregate_type,
            aggregate_id,
            aggregate_version,
            event_type,
            payload_json,
            occurred_at.to_rfc3339(),
            source,
        ],
    )?;

    let event_id = tx.last_insert_rowid();
    let sql = format!("SELECT {RUNTIME_EVENT_COLUMNS} FROM runtime_events WHERE event_id = ?1");
    tx.query_row(&sql, params![event_id], map_runtime_event_row)?
}

fn map_host_job_row(row: &Row<'_>) -> rusqlite::Result<Result<HostJobRecord, RuntimeError>> {
    let job_id_text: String = row.get(0)?;
    let owner_session_id_text: Option<String> = row.get(2)?;
    let callback_session_id_text: Option<String> = row.get(3)?;
    let status_text: String = row.get(7)?;
    let pid_i64: Option<i64> = row.get(10)?;
    let wait_timeout_count_i64: i64 = row.get(13)?;
    let stdout_bytes_i64: i64 = row.get(16)?;
    let stderr_bytes_i64: i64 = row.get(17)?;
    let result_ready_i64: i64 = row.get(18)?;
    let artifact_count_i64: i64 = row.get(19)?;
    let worker_session_id_text: Option<String> = row.get(22)?;
    let turn_id_text: Option<String> = row.get(23)?;
    let callback_inbox_id_text: Option<String> = row.get(24)?;
    let callback_emitted_at_text: Option<String> = row.get(25)?;
    let created_at_text: String = row.get(26)?;
    let updated_at_text: String = row.get(27)?;
    let started_at_text: Option<String> = row.get(28)?;
    let completed_at_text: Option<String> = row.get(29)?;
    let last_heartbeat_at_text: Option<String> = row.get(30)?;

    Ok((|| {
        Ok(HostJobRecord {
            job_id: parse_uuid("job_id", &job_id_text)?,
            workspace_id: row.get(1)?,
            owner_session_id: parse_optional_uuid("owner_session_id", owner_session_id_text)?,
            callback_session_id: parse_optional_uuid(
                "callback_session_id",
                callback_session_id_text,
            )?,
            provider: row.get(4)?,
            task: row.get(5)?,
            cwd: std::path::PathBuf::from(row.get::<_, String>(6)?),
            status: HostJobStatus::from_str(&status_text)?,
            version: row.get(8)?,
            worker_mode: row.get(9)?,
            pid: optional_i64_to_u32(pid_i64, "pid")?,
            job_token_hash: row.get(11)?,
            client_request_id: row.get(12)?,
            wait_timeout_count: i64_to_u32(wait_timeout_count_i64, "wait_timeout_count")?,
            last_event: row.get(14)?,
            last_output_preview: row.get(15)?,
            stdout_bytes: i64_to_u64(stdout_bytes_i64, "stdout_bytes")?,
            stderr_bytes: i64_to_u64(stderr_bytes_i64, "stderr_bytes")?,
            result_ready: result_ready_i64 != 0,
            artifact_count: i64_to_usize(artifact_count_i64, "artifact_count")?,
            result_summary: row.get(20)?,
            error: row.get(21)?,
            worker_session_id: parse_optional_uuid("worker_session_id", worker_session_id_text)?,
            turn_id: parse_optional_uuid("turn_id", turn_id_text)?,
            callback_inbox_id: parse_optional_uuid("callback_inbox_id", callback_inbox_id_text)?,
            callback_emitted_at: parse_optional_datetime(
                "callback_emitted_at",
                callback_emitted_at_text,
            )?,
            created_at: parse_datetime("created_at", &created_at_text)?,
            updated_at: parse_datetime("updated_at", &updated_at_text)?,
            started_at: parse_optional_datetime("started_at", started_at_text)?,
            completed_at: parse_optional_datetime("completed_at", completed_at_text)?,
            last_heartbeat_at: parse_optional_datetime(
                "last_heartbeat_at",
                last_heartbeat_at_text,
            )?,
        })
    })())
}

fn map_runtime_event_row(
    row: &Row<'_>,
) -> rusqlite::Result<Result<RuntimeEventRecord, RuntimeError>> {
    let session_id_text: Option<String> = row.get(2)?;
    let payload_json: String = row.get(7)?;
    let occurred_at_text: String = row.get(8)?;

    Ok((|| {
        Ok(RuntimeEventRecord {
            event_id: row.get(0)?,
            workspace_id: row.get(1)?,
            session_id: parse_optional_uuid("session_id", session_id_text)?,
            aggregate_type: row.get(3)?,
            aggregate_id: row.get(4)?,
            aggregate_version: row.get(5)?,
            event_type: row.get(6)?,
            payload: serde_json::from_str(&payload_json)?,
            occurred_at: parse_datetime("occurred_at", &occurred_at_text)?,
            source: row.get(9)?,
        })
    })())
}

fn collect_rows<T, F>(rows: rusqlite::MappedRows<'_, F>) -> Result<Vec<T>, RuntimeError>
where
    F: FnMut(&Row<'_>) -> rusqlite::Result<Result<T, RuntimeError>>,
{
    let mut records = Vec::new();
    for row in rows {
        records.push(row??);
    }
    Ok(records)
}

fn parse_uuid(column: &'static str, value: &str) -> Result<Uuid, RuntimeError> {
    value
        .parse::<Uuid>()
        .map_err(|_| RuntimeError::InvalidUuid {
            column,
            value: value.to_string(),
        })
}

fn parse_optional_uuid(
    column: &'static str,
    value: Option<String>,
) -> Result<Option<Uuid>, RuntimeError> {
    value.map(|value| parse_uuid(column, &value)).transpose()
}

fn parse_datetime(column: &'static str, value: &str) -> Result<DateTime<Utc>, RuntimeError> {
    DateTime::parse_from_rfc3339(value)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| RuntimeError::InvalidTimestamp {
            column,
            value: value.to_string(),
        })
}

fn parse_optional_datetime(
    column: &'static str,
    value: Option<String>,
) -> Result<Option<DateTime<Utc>>, RuntimeError> {
    value
        .map(|value| parse_datetime(column, &value))
        .transpose()
}

fn uuid_to_text(value: Option<Uuid>) -> Option<String> {
    value.map(|id| id.to_string())
}

fn datetime_to_text(value: Option<DateTime<Utc>>) -> Option<String> {
    value.map(|dt| dt.to_rfc3339())
}

fn bool_to_i64(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

fn to_i64(value: u64, column: &'static str) -> Result<i64, RuntimeError> {
    i64::try_from(value).map_err(|_| RuntimeError::InvalidTimestamp {
        column,
        value: value.to_string(),
    })
}

fn i64_to_u64(value: i64, column: &'static str) -> Result<u64, RuntimeError> {
    u64::try_from(value).map_err(|_| RuntimeError::InvalidTimestamp {
        column,
        value: value.to_string(),
    })
}

fn i64_to_u32(value: i64, column: &'static str) -> Result<u32, RuntimeError> {
    u32::try_from(value).map_err(|_| RuntimeError::InvalidTimestamp {
        column,
        value: value.to_string(),
    })
}

fn i64_to_usize(value: i64, column: &'static str) -> Result<usize, RuntimeError> {
    usize::try_from(value).map_err(|_| RuntimeError::InvalidTimestamp {
        column,
        value: value.to_string(),
    })
}

fn optional_i64_to_u32(
    value: Option<i64>,
    column: &'static str,
) -> Result<Option<u32>, RuntimeError> {
    value.map(|value| i64_to_u32(value, column)).transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_runtime_db() -> (RuntimeDb, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = RuntimeDb::open(dir.path().join("runtime.sqlite")).unwrap();
        (db, dir)
    }

    #[test]
    fn migrations_create_runtime_tables() {
        let (db, _dir) = temp_runtime_db();
        assert_eq!(db.schema_version().unwrap(), SQLITE_SCHEMA_VERSION);

        for table in [
            "host_jobs",
            "runtime_events",
            "worker_instances",
            "runtime_turns",
            "worker_spool",
            "runtime_metadata",
        ] {
            let exists: i64 = db
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(exists, 1, "missing table {table}");
        }
    }

    #[test]
    fn create_host_job_appends_event_transactionally() {
        let (mut db, _dir) = temp_runtime_db();
        let owner = Uuid::now_v7();
        let write = db
            .create_host_job(
                CreateHostJob::new("claude", "review auth", PathBuf::from("E:/repo"))
                    .with_workspace_id("workspace-a")
                    .with_owner_session_id(owner)
                    .with_client_request_id("req-1")
                    .with_source("test")
                    .with_payload(serde_json::json!({"reason": "unit"})),
            )
            .unwrap();

        assert!(!write.idempotent_replay);
        assert_eq!(write.record.status, HostJobStatus::Queued);
        assert_eq!(write.record.version, 0);
        let event = write.event.expect("create event");
        assert_eq!(event.event_id, 1);
        assert_eq!(event.event_type, "host_job.created");
        assert_eq!(event.aggregate_version, 0);
        assert_eq!(event.session_id, Some(owner));

        let rows = db.runtime_events_after(None, 0, 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].payload["reason"], "unit");
    }

    #[test]
    fn transition_bumps_version_and_appends_event() {
        let (mut db, _dir) = temp_runtime_db();
        let job_id = Uuid::now_v7();
        db.create_host_job(
            CreateHostJob::new("gemini", "analyze perf", PathBuf::from(".")).with_job_id(job_id),
        )
        .unwrap();

        let write = db
            .transition_host_job(
                job_id,
                "host_job.running",
                "worker",
                serde_json::json!({"pid": 4242}),
                |job| {
                    job.status = HostJobStatus::Running;
                    job.pid = Some(4242);
                    job.last_event = Some("worker_started".to_string());
                    job.started_at = Some(Utc::now());
                    job.last_heartbeat_at = Some(Utc::now());
                },
            )
            .unwrap();

        assert_eq!(write.record.status, HostJobStatus::Running);
        assert_eq!(write.record.version, 1);
        assert_eq!(write.record.pid, Some(4242));
        assert_eq!(write.record.last_event.as_deref(), Some("worker_started"));
        let event = write.event.expect("transition event");
        assert_eq!(event.event_type, "host_job.running");
        assert_eq!(event.aggregate_version, 1);

        let events = db.runtime_events_after(None, 0, 10).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].event_id, 2);
    }

    #[test]
    fn terminal_transition_rejected() {
        let (mut db, _dir) = temp_runtime_db();
        let job_id = Uuid::now_v7();
        db.create_host_job(
            CreateHostJob::new("codex", "task", PathBuf::from(".")).with_job_id(job_id),
        )
        .unwrap();
        db.transition_host_job(
            job_id,
            "host_job.completed",
            "worker",
            serde_json::json!({}),
            |job| {
                job.status = HostJobStatus::Completed;
                job.result_ready = true;
                job.completed_at = Some(Utc::now());
            },
        )
        .unwrap();

        let err = db
            .transition_host_job(
                job_id,
                "host_job.running",
                "worker",
                serde_json::json!({}),
                |job| {
                    job.status = HostJobStatus::Running;
                },
            )
            .expect_err("terminal job cannot become running");
        assert!(matches!(err, RuntimeError::InvalidHostJobTransition { .. }));
        assert_eq!(
            db.get_host_job(job_id).unwrap().unwrap().status,
            HostJobStatus::Completed
        );
    }

    #[test]
    fn duplicate_client_request_id_returns_existing() {
        let (mut db, _dir) = temp_runtime_db();
        let first = db
            .create_host_job(
                CreateHostJob::new("claude", "first", PathBuf::from("."))
                    .with_client_request_id("client-1"),
            )
            .unwrap();
        let second = db
            .create_host_job(
                CreateHostJob::new("claude", "second", PathBuf::from("."))
                    .with_client_request_id("client-1"),
            )
            .unwrap();

        assert!(second.idempotent_replay);
        assert_eq!(second.record.job_id, first.record.job_id);
        assert!(second.event.is_none());
        assert_eq!(db.runtime_events_after(None, 0, 10).unwrap().len(), 1);
    }

    #[test]
    fn event_append_failure_rolls_back_state_update() {
        let (mut db, _dir) = temp_runtime_db();
        let job_id = Uuid::now_v7();
        db.create_host_job(
            CreateHostJob::new("claude", "task", PathBuf::from(".")).with_job_id(job_id),
        )
        .unwrap();
        db.conn
            .execute_batch(
                r#"
                CREATE TRIGGER fail_runtime_events_insert
                BEFORE INSERT ON runtime_events
                BEGIN
                  SELECT RAISE(FAIL, 'injected runtime event failure');
                END;
                "#,
            )
            .unwrap();

        let err = db
            .transition_host_job(
                job_id,
                "host_job.running",
                "test",
                serde_json::json!({}),
                |job| {
                    job.status = HostJobStatus::Running;
                    job.last_event = Some("should_rollback".to_string());
                },
            )
            .expect_err("trigger should fail event append");
        assert!(format!("{err}").contains("injected runtime event failure"));

        db.conn
            .execute_batch("DROP TRIGGER fail_runtime_events_insert")
            .unwrap();
        let job = db.get_host_job(job_id).unwrap().unwrap();
        assert_eq!(job.status, HostJobStatus::Queued);
        assert_eq!(job.version, 0);
        assert_eq!(job.last_event, None);
        assert_eq!(db.runtime_events_after(None, 0, 10).unwrap().len(), 1);
    }

    #[test]
    fn list_and_snapshot_are_cursor_based() {
        let (mut db, _dir) = temp_runtime_db();
        let session = Uuid::now_v7();
        let other = Uuid::now_v7();
        db.create_host_job(
            CreateHostJob::new("claude", "task-a", PathBuf::from("."))
                .with_owner_session_id(session),
        )
        .unwrap();
        db.create_host_job(
            CreateHostJob::new("gemini", "task-b", PathBuf::from(".")).with_owner_session_id(other),
        )
        .unwrap();

        let snapshot = db.snapshot_for_session(session, 0, 100, 100).unwrap();
        assert_eq!(snapshot.host_jobs.len(), 1);
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.events[0].session_id, Some(session));
        assert_eq!(snapshot.max_event_id, 2);
    }
}
