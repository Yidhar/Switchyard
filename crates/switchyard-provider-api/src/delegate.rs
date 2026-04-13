use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::role::ProviderRole;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateRequest {
    #[serde(rename = "type")]
    pub request_type: String,
    pub requests: Vec<DelegateTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateTask {
    pub id: String,
    pub provider: String,
    pub role: ProviderRole,
    pub task: String,
    #[serde(default)]
    pub write_access: bool,
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub allowed_paths: Vec<PathBuf>,
    #[serde(default = "default_timeout")]
    pub timeout_sec: u64,
}

fn default_timeout() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateResponse {
    #[serde(rename = "type")]
    pub response_type: String,
    pub results: Vec<DelegateTaskResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegateTaskResult {
    pub id: String,
    pub provider: String,
    pub status: DelegateStatus,
    pub summary: Option<String>,
    #[serde(default)]
    pub changed_files: Vec<PathBuf>,
    #[serde(default)]
    pub artifacts: Vec<HashMap<String, serde_json::Value>>,
    pub error: Option<String>,
    /// Subprocess exit code, if applicable.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Wall-clock duration of the delegate execution in milliseconds.
    #[serde(default)]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum DelegateStatus {
    Success,
    Failed,
    Timeout,
    Cancelled,
}

impl fmt::Display for DelegateStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Success => write!(f, "success"),
            Self::Failed => write!(f, "failed"),
            Self::Timeout => write!(f, "timeout"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl DelegateRequest {
    pub fn new(tasks: Vec<DelegateTask>) -> Self {
        Self {
            request_type: "delegate".to_string(),
            requests: tasks,
        }
    }
}

impl DelegateResponse {
    pub fn new(results: Vec<DelegateTaskResult>) -> Self {
        Self {
            response_type: "delegate_result".to_string(),
            results,
        }
    }
}
