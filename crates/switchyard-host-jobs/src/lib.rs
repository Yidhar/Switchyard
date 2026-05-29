use std::fmt;
use std::fs;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use switchyard_provider_api::{ExecutionTelemetry, HyardJobObservation};
use switchyard_text::{prefix_chars, preview_trimmed};
use uuid::Uuid;

#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

const PREVIEW_MAX_CHARS: usize = 400;
const WORKER_LOG_TAIL_BYTES: usize = 8 * 1024;
const SUBPROCESS_BOOT_STALE_SECS: i64 = 15;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

fn suppress_windows_console(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostJobStatus {
    Queued,
    Running,
    CancelRequested,
    Completed,
    Failed,
    Cancelled,
}

impl HostJobStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::CancelRequested)
    }
}

impl fmt::Display for HostJobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::CancelRequested => "cancel_requested",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        };
        write!(f, "{value}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostJobWorkerMode {
    Inline,
    Subprocess,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostJobState {
    pub job_id: Uuid,
    pub provider: String,
    pub task: String,
    pub cwd: PathBuf,
    pub session_id: Option<Uuid>,
    #[serde(default)]
    pub callback_session_id: Option<Uuid>,
    pub turn_id: Option<Uuid>,
    pub status: HostJobStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub worker_mode: Option<HostJobWorkerMode>,
    pub pid: Option<u32>,
    pub last_event: Option<String>,
    pub last_output_preview: Option<String>,
    pub execution: Option<ExecutionTelemetry>,
    pub wait_timeout_count: u32,
    pub artifact_count: usize,
    pub result_ready: bool,
    pub result_summary: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub callback_inbox_id: Option<Uuid>,
    #[serde(default)]
    pub callback_emitted_at: Option<DateTime<Utc>>,
}

impl HostJobState {
    pub fn new(provider: impl Into<String>, task: impl Into<String>, cwd: PathBuf) -> Self {
        let now = Utc::now();
        Self {
            job_id: Uuid::now_v7(),
            provider: provider.into(),
            task: task.into(),
            cwd,
            session_id: None,
            callback_session_id: None,
            turn_id: None,
            status: HostJobStatus::Queued,
            created_at: now,
            updated_at: now,
            started_at: None,
            completed_at: None,
            worker_mode: None,
            pid: None,
            last_event: None,
            last_output_preview: None,
            execution: None,
            wait_timeout_count: 0,
            artifact_count: 0,
            result_ready: false,
            result_summary: None,
            error: None,
            callback_inbox_id: None,
            callback_emitted_at: None,
        }
    }

    pub fn set_last_preview(&mut self, preview: Option<&str>) {
        self.last_output_preview = preview.and_then(truncate_preview);
    }

    pub fn touch_wait_timeout(&mut self) {
        self.wait_timeout_count = self.wait_timeout_count.saturating_add(1);
        self.updated_at = Utc::now();
    }

    pub fn to_bridge_json(&self) -> serde_json::Value {
        let callback_session_id = self.callback_session_id.or(self.session_id);
        serde_json::json!({
            "job_id": self.job_id.to_string(),
            "status": self.status.to_string(),
            "provider": self.provider,
            "session_id": callback_session_id.map(|id| id.to_string()),
            "worker_session_id": self.session_id.map(|id| id.to_string()),
            "callback_session_id": self.callback_session_id.map(|id| id.to_string()),
            "turn_id": self.turn_id.map(|id| id.to_string()),
            "last_event": self.last_event,
            "last_output_preview": self.last_output_preview,
            "execution": self.execution,
            "summary": self.result_summary,
            "artifact_count": self.artifact_count,
            "result_ready": self.result_ready,
            "wait_timeout_count": self.wait_timeout_count,
            "error": self.error,
        })
    }

    pub fn to_wait_timeout_json(&self) -> serde_json::Value {
        let callback_session_id = self.callback_session_id.or(self.session_id);
        serde_json::json!({
            "job_id": self.job_id.to_string(),
            "status": "wait_timeout",
            "job_status": self.status.to_string(),
            "provider": self.provider,
            "session_id": callback_session_id.map(|id| id.to_string()),
            "worker_session_id": self.session_id.map(|id| id.to_string()),
            "callback_session_id": self.callback_session_id.map(|id| id.to_string()),
            "turn_id": self.turn_id.map(|id| id.to_string()),
            "last_event": self.last_event,
            "last_output_preview": self.last_output_preview,
            "execution": self.execution,
            "summary": self.result_summary,
            "artifact_count": self.artifact_count,
            "result_ready": self.result_ready,
            "wait_timeout_count": self.wait_timeout_count,
            "error": self.error,
            "message": "job is still running; call /hyard:status, /hyard:result, /hyard:await, or /hyard:cancel",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostJobSource {
    Live,
    Persisted,
    Recovered,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostJobSummary {
    pub job_id: String,
    pub provider: String,
    pub status: String,
    pub last_event: Option<String>,
    pub last_output_preview: Option<String>,
    pub execution: Option<ExecutionTelemetry>,
    pub wait_timeout_count: u32,
    pub artifact_count: usize,
    pub result_ready: bool,
    pub error: Option<String>,
    pub updated_at: String,
    pub source: HostJobSource,
}

impl HostJobSummary {
    pub fn is_active(&self) -> bool {
        matches!(
            self.status.as_str(),
            "queued" | "running" | "cancel_requested"
        )
    }

    pub fn short_job_id(&self) -> String {
        prefix_chars(&self.job_id, 8)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RawHostJobSummaryState {
    job_id: String,
    provider: String,
    status: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    completed_at: Option<String>,
    #[serde(default)]
    worker_mode: Option<HostJobWorkerMode>,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    last_event: Option<String>,
    #[serde(default)]
    last_output_preview: Option<String>,
    #[serde(default)]
    execution: Option<ExecutionTelemetry>,
    #[serde(default)]
    wait_timeout_count: u32,
    #[serde(default)]
    artifact_count: usize,
    #[serde(default)]
    result_ready: bool,
    #[serde(default)]
    error: Option<String>,
}

impl From<HostJobState> for HostJobSummary {
    fn from(job: HostJobState) -> Self {
        Self {
            job_id: job.job_id.to_string(),
            provider: job.provider,
            status: job.status.to_string(),
            last_event: job.last_event,
            last_output_preview: job.last_output_preview,
            execution: job.execution,
            wait_timeout_count: job.wait_timeout_count,
            artifact_count: job.artifact_count,
            result_ready: job.result_ready,
            error: job.error,
            updated_at: job.updated_at.to_rfc3339(),
            source: HostJobSource::Persisted,
        }
    }
}

impl HostJobSummary {
    pub fn from_observation(job: HyardJobObservation, observed_at: String) -> Self {
        Self {
            job_id: job.job_id,
            provider: job.provider,
            status: job.status,
            last_event: job.last_event,
            last_output_preview: job.last_output_preview,
            execution: job.execution,
            wait_timeout_count: job.wait_timeout_count,
            artifact_count: job.artifact_count,
            result_ready: job.result_ready,
            error: job.error,
            updated_at: observed_at,
            source: HostJobSource::Live,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HostJobStore {
    base_dir: PathBuf,
}

impl HostJobStore {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    pub fn ensure_dir(&self) -> io::Result<()> {
        fs::create_dir_all(&self.base_dir)
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    pub fn job_path(&self, job_id: Uuid) -> PathBuf {
        self.base_dir.join(format!("{job_id}.json"))
    }

    pub fn worker_stdout_path(&self, job_id: Uuid) -> PathBuf {
        self.base_dir.join(format!("{job_id}.worker.stdout.log"))
    }

    pub fn worker_stderr_path(&self, job_id: Uuid) -> PathBuf {
        self.base_dir.join(format!("{job_id}.worker.stderr.log"))
    }

    pub fn save(&self, job: &HostJobState) -> io::Result<()> {
        self.ensure_dir()?;
        let path = self.job_path(job.job_id);
        let data = serde_json::to_vec_pretty(job)
            .map_err(|e| io::Error::other(format!("serialize job: {e}")))?;
        write_file_replace(&path, &data)
    }

    pub fn load(&self, job_id: Uuid) -> io::Result<Option<HostJobState>> {
        let path = self.job_path(job_id);
        let data = match fs::read(&path) {
            Ok(data) => data,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err),
        };
        let job = serde_json::from_slice(&data).map_err(|e| {
            io::Error::other(format!("parse job manifest '{}': {e}", path.display()))
        })?;
        Ok(Some(job))
    }

    pub fn update<F>(&self, job_id: Uuid, mutate: F) -> io::Result<HostJobState>
    where
        F: FnOnce(&mut HostJobState),
    {
        let mut job = self.load(job_id)?.ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("job '{job_id}' not found"))
        })?;
        mutate(&mut job);
        job.updated_at = Utc::now();
        self.save(&job)?;
        Ok(job)
    }
}

fn write_file_replace(path: &Path, data: &[u8]) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("job.json");
    let tmp_path = parent.join(format!(".{file_name}.{}.tmp", Uuid::now_v7()));

    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)?;
        file.write_all(data)?;
        file.sync_all()?;
        Ok::<(), io::Error>(())
    })();

    if let Err(err) = write_result {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    if let Err(err) = replace_file(&tmp_path, path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(err);
    }

    sync_parent_dir(parent)?;
    Ok(())
}

#[cfg(windows)]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    let from_wide: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to_wide: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;
    let ok = unsafe { MoveFileExW(from_wide.as_ptr(), to_wide.as_ptr(), flags) };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

#[cfg(windows)]
fn sync_parent_dir(_parent: &Path) -> io::Result<()> {
    // MoveFileExW with MOVEFILE_WRITE_THROUGH flushes the replacement on Windows.
    Ok(())
}

#[cfg(not(windows))]
fn sync_parent_dir(parent: &Path) -> io::Result<()> {
    fs::File::open(parent)?.sync_all()
}

pub fn load_job_with_refresh(
    job_store: &HostJobStore,
    job_id: Uuid,
) -> Result<Option<HostJobState>, io::Error> {
    refresh_orphaned_job_state_with(job_store, job_id, is_process_alive)
}

pub fn refresh_orphaned_job_state_with<F>(
    job_store: &HostJobStore,
    job_id: Uuid,
    is_process_alive: F,
) -> Result<Option<HostJobState>, io::Error>
where
    F: Fn(u32) -> bool,
{
    let Some(mut job) = job_store.load(job_id)? else {
        return Ok(None);
    };

    if reconcile_host_job_state(job_store, &mut job, &is_process_alive) {
        job_store.save(&job)?;
    }

    Ok(Some(job))
}

fn reconcile_host_job_state<F>(
    job_store: &HostJobStore,
    job: &mut HostJobState,
    is_process_alive: &F,
) -> bool
where
    F: Fn(u32) -> bool,
{
    if job.status.is_terminal() {
        return false;
    }

    let Some(pid) = job.pid else {
        if !should_reconcile_pidless_subprocess_job(job.worker_mode, job.updated_at) {
            return false;
        }

        let now = Utc::now();
        let message = build_missing_worker_pid_message(
            &job_store.worker_stderr_path(job.job_id),
            &job_store.worker_stdout_path(job.job_id),
            job.status,
        );
        job.status = HostJobStatus::Failed;
        job.completed_at = Some(now);
        job.updated_at = now;
        job.error = Some(message);
        job.last_event = Some("worker_missing".to_string());
        return true;
    };

    if is_process_alive(pid) {
        return false;
    }

    let now = Utc::now();
    let message = build_dead_worker_pid_message(
        pid,
        job.status,
        &job_store.worker_stderr_path(job.job_id),
        &job_store.worker_stdout_path(job.job_id),
    );
    job.status = HostJobStatus::Failed;
    job.completed_at = Some(now);
    job.updated_at = now;
    job.pid = None;
    job.error = Some(message);
    job.last_event = Some("worker_missing".to_string());
    true
}

pub fn list_job_summaries(job_dir: &Path, max_jobs: usize) -> Vec<HostJobSummary> {
    list_job_summaries_with(job_dir, max_jobs, is_process_alive)
}

pub fn list_job_summaries_with<F>(
    job_dir: &Path,
    max_jobs: usize,
    is_process_alive: F,
) -> Vec<HostJobSummary>
where
    F: Fn(u32) -> bool,
{
    let entries = match fs::read_dir(job_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(_) => return Vec::new(),
    };

    let mut jobs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }

        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        let mut raw = match serde_json::from_str::<RawHostJobSummaryState>(&content) {
            Ok(raw) => raw,
            Err(_) => continue,
        };

        let recovered = reconcile_raw_orphaned_job(&path, &mut raw, &is_process_alive).is_some();
        if recovered {
            persist_reconciled_full_manifest(&path, &content, &is_process_alive);
        }
        let summary = HostJobSummary {
            job_id: raw.job_id,
            provider: raw.provider,
            status: raw.status,
            last_event: raw.last_event,
            last_output_preview: raw.last_output_preview,
            execution: raw.execution,
            wait_timeout_count: raw.wait_timeout_count,
            artifact_count: raw.artifact_count,
            result_ready: raw.result_ready,
            error: raw.error,
            updated_at: raw.updated_at,
            source: if recovered {
                HostJobSource::Recovered
            } else {
                HostJobSource::Persisted
            },
        };
        jobs.push(summary);
    }

    jobs.sort_by(|left, right| {
        right
            .is_active()
            .cmp(&left.is_active())
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.provider.cmp(&right.provider))
    });
    jobs.truncate(max_jobs);
    jobs
}

fn persist_reconciled_full_manifest<F>(path: &Path, content: &str, is_process_alive: &F)
where
    F: Fn(u32) -> bool,
{
    let Ok(mut job) = serde_json::from_str::<HostJobState>(content) else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    let store = HostJobStore::new(parent.to_path_buf());
    if reconcile_host_job_state(&store, &mut job, is_process_alive) {
        let _ = store.save(&job);
    }
}

fn reconcile_raw_orphaned_job<F>(
    path: &Path,
    raw: &mut RawHostJobSummaryState,
    is_process_alive: &F,
) -> Option<()>
where
    F: Fn(u32) -> bool,
{
    if !matches!(
        raw.status.as_str(),
        "queued" | "running" | "cancel_requested"
    ) {
        return None;
    }

    if let Some(pid) = raw.pid {
        if is_process_alive(pid) {
            return None;
        }

        let message = build_dead_worker_pid_message(
            pid,
            raw.status.as_str(),
            &sibling_log_path(path, &raw.job_id, "worker.stderr.log"),
            &sibling_log_path(path, &raw.job_id, "worker.stdout.log"),
        );
        raw.status = "failed".to_string();
        raw.completed_at = Some(Utc::now().to_rfc3339());
        raw.pid = None;
        raw.error = Some(message);
        raw.last_event = Some("worker_missing".to_string());
        raw.updated_at = Utc::now().to_rfc3339();

        return Some(());
    }

    let updated_at = parse_rfc3339_utc(&raw.updated_at)?;
    if !should_reconcile_pidless_subprocess_job(raw.worker_mode, updated_at) {
        return None;
    }

    let stderr_path = sibling_log_path(path, &raw.job_id, "worker.stderr.log");
    let stdout_path = sibling_log_path(path, &raw.job_id, "worker.stdout.log");
    let message = build_missing_worker_pid_message(&stderr_path, &stdout_path, raw.status.as_str());
    raw.status = "failed".to_string();
    raw.completed_at = Some(Utc::now().to_rfc3339());
    raw.error = Some(message);
    raw.last_event = Some("worker_missing".to_string());
    raw.updated_at = Utc::now().to_rfc3339();

    Some(())
}

fn should_reconcile_pidless_subprocess_job(
    worker_mode: Option<HostJobWorkerMode>,
    updated_at: DateTime<Utc>,
) -> bool {
    matches!(worker_mode, Some(HostJobWorkerMode::Subprocess))
        && updated_at <= Utc::now() - chrono::Duration::seconds(SUBPROCESS_BOOT_STALE_SECS)
}

fn parse_rfc3339_utc(value: &str) -> Option<DateTime<Utc>> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn build_dead_worker_pid_message(
    pid: u32,
    status: impl fmt::Display,
    stderr_path: &Path,
    stdout_path: &Path,
) -> String {
    let mut message = format!("host worker pid {pid} disappeared while job remained {status}");
    append_log_tail(&mut message, stderr_path, stdout_path);
    message
}

fn build_missing_worker_pid_message(
    stderr_path: &Path,
    stdout_path: &Path,
    status: impl fmt::Display,
) -> String {
    let mut message = format!(
        "host worker pid was never recorded for subprocess job while job remained {status}"
    );
    append_log_tail(&mut message, stderr_path, stdout_path);
    message
}

fn append_log_tail(message: &mut String, stderr_path: &Path, stdout_path: &Path) {
    if let Some(stderr_tail) = read_log_tail(stderr_path, WORKER_LOG_TAIL_BYTES) {
        let stderr_tail = collapse_whitespace(&stderr_tail);
        if !stderr_tail.is_empty() {
            message.push_str(&format!(
                "; stderr: {stderr_tail}; stderr_log={}",
                stderr_path.display()
            ));
            return;
        }
    }

    if let Some(stdout_tail) = read_log_tail(stdout_path, WORKER_LOG_TAIL_BYTES) {
        let stdout_tail = collapse_whitespace(&stdout_tail);
        if !stdout_tail.is_empty() {
            message.push_str(&format!(
                "; stdout: {stdout_tail}; stdout_log={}",
                stdout_path.display()
            ));
        }
    }
}

pub fn is_process_alive(pid: u32) -> bool {
    probe_process_alive(pid).unwrap_or(false)
}

pub fn probe_process_alive(pid: u32) -> io::Result<bool> {
    #[cfg(windows)]
    {
        let filter = format!("PID eq {pid}");
        let mut command = std::process::Command::new("tasklist");
        suppress_windows_console(&mut command);
        let output = command
            .args(["/FI", &filter, "/FO", "CSV", "/NH"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()?;

        if !output.status.success() {
            return Err(io::Error::other(format!(
                "tasklist exited with status {}",
                output.status
            )));
        }

        let text = String::from_utf8_lossy(&output.stdout);
        Ok(text
            .lines()
            .any(|line| line.contains(&format!("\"{pid}\""))))
    }

    #[cfg(not(windows))]
    {
        let status = std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?;

        Ok(status.success())
    }
}

fn truncate_preview(text: &str) -> Option<String> {
    preview_trimmed(text, PREVIEW_MAX_CHARS, "…")
}

fn read_log_tail(path: &Path, max_bytes: usize) -> Option<String> {
    let data = fs::read(path).ok()?;
    if data.is_empty() {
        return None;
    }

    let start = data.len().saturating_sub(max_bytes);
    let tail = String::from_utf8_lossy(&data[start..]).trim().to_string();
    (!tail.is_empty()).then_some(tail)
}

fn sibling_log_path(path: &Path, job_id: &str, suffix: &str) -> PathBuf {
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!("{job_id}.{suffix}"))
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_timeout_json_has_dual_status_fields() {
        let mut job = HostJobState::new("gemini", "task", PathBuf::from("."));
        job.status = HostJobStatus::Running;
        job.touch_wait_timeout();
        let json = job.to_wait_timeout_json();
        assert_eq!(json["status"], "wait_timeout");
        assert_eq!(json["job_status"], "running");
        assert_eq!(json["wait_timeout_count"], 1);
    }

    #[test]
    fn store_round_trips_job_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostJobStore::new(dir.path().to_path_buf());
        let mut job = HostJobState::new("codex", "hello", PathBuf::from("E:/repo"));
        job.status = HostJobStatus::Running;
        job.set_last_preview(Some("working"));
        store.save(&job).unwrap();

        let loaded = store.load(job.job_id).unwrap().unwrap();
        assert_eq!(loaded.job_id, job.job_id);
        assert_eq!(loaded.status, HostJobStatus::Running);
        assert_eq!(loaded.last_output_preview.as_deref(), Some("working"));
    }

    #[test]
    fn refresh_orphaned_job_marks_missing_worker_failed() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostJobStore::new(dir.path().to_path_buf());
        let mut job = HostJobState::new("claude", "task", PathBuf::from("."));
        job.status = HostJobStatus::Running;
        job.pid = Some(4242);
        job.last_event = Some("worker_started".to_string());
        store.save(&job).unwrap();
        fs::write(
            store.worker_stderr_path(job.job_id),
            "spawn failed: access denied",
        )
        .unwrap();

        let refreshed = refresh_orphaned_job_state_with(&store, job.job_id, |_| false)
            .unwrap()
            .unwrap();
        assert_eq!(refreshed.status, HostJobStatus::Failed);
        assert_eq!(refreshed.pid, None);
        assert_eq!(refreshed.last_event.as_deref(), Some("worker_missing"));
        assert!(
            refreshed
                .error
                .as_deref()
                .is_some_and(|msg| msg.contains("access denied"))
        );
    }

    #[test]
    fn list_job_summaries_recovers_stale_active_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostJobStore::new(dir.path().to_path_buf());

        let mut job = HostJobState::new("claude", "task", PathBuf::from("."));
        job.status = HostJobStatus::Running;
        job.pid = Some(35596);
        job.wait_timeout_count = 1;
        store.save(&job).unwrap();

        let summaries = list_job_summaries_with(dir.path(), 8, |_| false);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].status, "failed");
        assert_eq!(summaries[0].source, HostJobSource::Recovered);
        assert!(summaries[0].error.is_some());

        let persisted = store.load(job.job_id).unwrap().unwrap();
        assert_eq!(persisted.status, HostJobStatus::Failed);
        assert_eq!(persisted.pid, None);
        assert_eq!(persisted.last_event.as_deref(), Some("worker_missing"));
    }

    #[test]
    fn list_job_summaries_recovery_does_not_clobber_full_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostJobStore::new(dir.path().to_path_buf());

        let mut job = HostJobState::new("claude", "important task", PathBuf::from("E:/repo"));
        job.status = HostJobStatus::Running;
        job.pid = Some(4242);
        job.worker_mode = Some(HostJobWorkerMode::Subprocess);
        store.save(&job).unwrap();

        let summaries = list_job_summaries_with(dir.path(), 8, |_| false);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].status, "failed");
        assert_eq!(summaries[0].source, HostJobSource::Recovered);

        let loaded = store.load(job.job_id).unwrap().unwrap();
        assert_eq!(loaded.status, HostJobStatus::Failed);
        assert_eq!(loaded.last_event.as_deref(), Some("worker_missing"));
        assert_eq!(loaded.task, "important task");
        assert_eq!(loaded.cwd, PathBuf::from("E:/repo"));
        assert_eq!(loaded.provider, "claude");
    }

    #[test]
    fn refresh_orphaned_job_recovers_pidless_stale_subprocess_job() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostJobStore::new(dir.path().to_path_buf());

        let mut job = HostJobState::new("claude", "task", PathBuf::from("."));
        job.status = HostJobStatus::Queued;
        job.worker_mode = Some(HostJobWorkerMode::Subprocess);
        job.updated_at = Utc::now() - chrono::Duration::seconds(SUBPROCESS_BOOT_STALE_SECS + 1);
        fs::write(
            store.worker_stderr_path(job.job_id),
            "spawn failed before pid assignment",
        )
        .unwrap();
        store.save(&job).unwrap();

        let refreshed = refresh_orphaned_job_state_with(&store, job.job_id, |_| true)
            .unwrap()
            .unwrap();
        assert_eq!(refreshed.status, HostJobStatus::Failed);
        assert_eq!(refreshed.last_event.as_deref(), Some("worker_missing"));
        assert!(
            refreshed
                .error
                .as_deref()
                .is_some_and(|msg| msg.contains("never recorded"))
        );
    }

    #[test]
    fn list_job_summaries_recovers_pidless_stale_subprocess_job() {
        let dir = tempfile::tempdir().unwrap();
        let store = HostJobStore::new(dir.path().to_path_buf());

        let mut job = HostJobState::new("gemini", "task", PathBuf::from("."));
        job.status = HostJobStatus::Queued;
        job.worker_mode = Some(HostJobWorkerMode::Subprocess);
        job.updated_at = Utc::now() - chrono::Duration::seconds(SUBPROCESS_BOOT_STALE_SECS + 1);
        store.save(&job).unwrap();

        let summaries = list_job_summaries(dir.path(), 8);
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].status, "failed");
        assert_eq!(summaries[0].source, HostJobSource::Recovered);
        assert!(
            summaries[0]
                .error
                .as_deref()
                .is_some_and(|msg| msg.contains("never recorded"))
        );

        let persisted = store.load(job.job_id).unwrap().unwrap();
        assert_eq!(persisted.status, HostJobStatus::Failed);
        assert_eq!(persisted.last_event.as_deref(), Some("worker_missing"));
    }
}
