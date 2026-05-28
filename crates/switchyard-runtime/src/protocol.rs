use std::{fmt, path::PathBuf, str::FromStr};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::RuntimeError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostJobStatus {
    Queued,
    WorkerBooting,
    Running,
    CancelRequested,
    Completed,
    Failed,
    Cancelled,
    Lost,
}

impl HostJobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::WorkerBooting => "worker_booting",
            Self::Running => "running",
            Self::CancelRequested => "cancel_requested",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Lost => "lost",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Lost
        )
    }

    pub fn is_active(self) -> bool {
        !self.is_terminal()
    }

    pub fn validate_transition(self, next: Self) -> Result<(), RuntimeError> {
        if self.is_terminal() && self != next {
            return Err(RuntimeError::InvalidHostJobTransition {
                from: self.to_string(),
                to: next.to_string(),
            });
        }
        Ok(())
    }
}

impl fmt::Display for HostJobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HostJobStatus {
    type Err = RuntimeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "queued" => Ok(Self::Queued),
            "worker_booting" => Ok(Self::WorkerBooting),
            "running" => Ok(Self::Running),
            "cancel_requested" => Ok(Self::CancelRequested),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            "lost" => Ok(Self::Lost),
            other => Err(RuntimeError::InvalidHostJobStatus(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerInstanceState {
    Booting,
    Idle,
    Running,
    Draining,
    Stopped,
    Lost,
    Failed,
}

impl WorkerInstanceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Booting => "booting",
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Draining => "draining",
            Self::Stopped => "stopped",
            Self::Lost => "lost",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for WorkerInstanceState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEventRecord {
    pub event_id: i64,
    pub workspace_id: Option<String>,
    pub session_id: Option<Uuid>,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub aggregate_version: i64,
    pub event_type: String,
    pub payload: Value,
    pub occurred_at: DateTime<Utc>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostJobRecord {
    pub job_id: Uuid,
    pub workspace_id: Option<String>,
    pub owner_session_id: Option<Uuid>,
    pub callback_session_id: Option<Uuid>,
    pub provider: String,
    pub task: String,
    pub cwd: PathBuf,
    pub status: HostJobStatus,
    pub version: i64,
    pub worker_mode: Option<String>,
    pub pid: Option<u32>,
    pub job_token_hash: Option<String>,
    pub client_request_id: Option<String>,
    pub wait_timeout_count: u32,
    pub last_event: Option<String>,
    pub last_output_preview: Option<String>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub result_ready: bool,
    pub artifact_count: usize,
    pub result_summary: Option<String>,
    pub error: Option<String>,
    pub worker_session_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub callback_inbox_id: Option<Uuid>,
    pub callback_emitted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct HostJobMutation {
    pub owner_session_id: Option<Uuid>,
    pub callback_session_id: Option<Uuid>,
    pub status: HostJobStatus,
    pub worker_mode: Option<String>,
    pub pid: Option<u32>,
    pub job_token_hash: Option<String>,
    pub wait_timeout_count: u32,
    pub last_event: Option<String>,
    pub last_output_preview: Option<String>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub result_ready: bool,
    pub artifact_count: usize,
    pub result_summary: Option<String>,
    pub error: Option<String>,
    pub worker_session_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub callback_inbox_id: Option<Uuid>,
    pub callback_emitted_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
}

impl HostJobMutation {
    pub fn from_record(record: &HostJobRecord) -> Self {
        Self {
            owner_session_id: record.owner_session_id,
            callback_session_id: record.callback_session_id,
            status: record.status,
            worker_mode: record.worker_mode.clone(),
            pid: record.pid,
            job_token_hash: record.job_token_hash.clone(),
            wait_timeout_count: record.wait_timeout_count,
            last_event: record.last_event.clone(),
            last_output_preview: record.last_output_preview.clone(),
            stdout_bytes: record.stdout_bytes,
            stderr_bytes: record.stderr_bytes,
            result_ready: record.result_ready,
            artifact_count: record.artifact_count,
            result_summary: record.result_summary.clone(),
            error: record.error.clone(),
            worker_session_id: record.worker_session_id,
            turn_id: record.turn_id,
            callback_inbox_id: record.callback_inbox_id,
            callback_emitted_at: record.callback_emitted_at,
            started_at: record.started_at,
            completed_at: record.completed_at,
            last_heartbeat_at: record.last_heartbeat_at,
        }
    }
}

#[derive(Debug, Clone)]
pub struct CreateHostJob {
    pub job_id: Uuid,
    pub workspace_id: Option<String>,
    pub owner_session_id: Option<Uuid>,
    pub callback_session_id: Option<Uuid>,
    pub provider: String,
    pub task: String,
    pub cwd: PathBuf,
    pub worker_mode: Option<String>,
    pub job_token_hash: Option<String>,
    pub client_request_id: Option<String>,
    pub source: String,
    pub payload: Value,
}

impl CreateHostJob {
    pub fn new(provider: impl Into<String>, task: impl Into<String>, cwd: PathBuf) -> Self {
        Self {
            job_id: Uuid::now_v7(),
            workspace_id: None,
            owner_session_id: None,
            callback_session_id: None,
            provider: provider.into(),
            task: task.into(),
            cwd,
            worker_mode: None,
            job_token_hash: None,
            client_request_id: None,
            source: "runtime".to_string(),
            payload: serde_json::json!({}),
        }
    }

    pub fn with_job_id(mut self, job_id: Uuid) -> Self {
        self.job_id = job_id;
        self
    }

    pub fn with_workspace_id(mut self, workspace_id: impl Into<String>) -> Self {
        self.workspace_id = Some(workspace_id.into());
        self
    }

    pub fn with_owner_session_id(mut self, session_id: Uuid) -> Self {
        self.owner_session_id = Some(session_id);
        self
    }

    pub fn with_callback_session_id(mut self, session_id: Uuid) -> Self {
        self.callback_session_id = Some(session_id);
        self
    }

    pub fn with_worker_mode(mut self, worker_mode: impl Into<String>) -> Self {
        self.worker_mode = Some(worker_mode.into());
        self
    }

    pub fn with_job_token_hash(mut self, token_hash: impl Into<String>) -> Self {
        self.job_token_hash = Some(token_hash.into());
        self
    }

    pub fn with_client_request_id(mut self, request_id: impl Into<String>) -> Self {
        self.client_request_id = Some(request_id.into());
        self
    }

    pub fn with_source(mut self, source: impl Into<String>) -> Self {
        self.source = source.into();
        self
    }

    pub fn with_payload(mut self, payload: Value) -> Self {
        self.payload = payload;
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeWrite<T> {
    pub record: T,
    pub event: Option<RuntimeEventRecord>,
    pub idempotent_replay: bool,
}

impl<T> RuntimeWrite<T> {
    pub fn committed(record: T, event: RuntimeEventRecord) -> Self {
        Self {
            record,
            event: Some(event),
            idempotent_replay: false,
        }
    }

    pub fn idempotent(record: T) -> Self {
        Self {
            record,
            event: None,
            idempotent_replay: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RuntimeSnapshot {
    pub max_event_id: i64,
    pub host_jobs: Vec<HostJobRecord>,
    pub events: Vec<RuntimeEventRecord>,
}
