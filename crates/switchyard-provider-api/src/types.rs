use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnInput {
    pub user_message: String,
    pub system_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    pub timeout_secs: u64,
    pub write_access: bool,
    pub cwd: PathBuf,
    pub allowed_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextBundle {
    pub summary: Option<String>,
    pub recent_turns: Vec<serde_json::Value>,
    pub peer_state: Vec<serde_json::Value>,
    pub artifacts: Vec<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnResult {
    pub response_text: String,
    pub exit_code: Option<i32>,
    pub stderr: Option<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ExecutionTelemetry {
    pub original_command: String,
    pub resolved_command: String,
    pub actual_command: String,
    pub actual_display: String,
    #[serde(default)]
    pub io_transport: Option<String>,
    #[serde(default)]
    pub used_npm_wrapper_rewrite: bool,
    #[serde(default)]
    pub js_entry: Option<String>,
    #[serde(default)]
    pub node_path: Option<String>,
    #[serde(default)]
    pub terminal_rows: Option<u16>,
    #[serde(default)]
    pub terminal_cols: Option<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactBundle {
    pub artifacts: Vec<ArtifactEntry>,
}

/// Well-known artifact type for raw provider output (stdout/stderr).
pub const ARTIFACT_TYPE_RAW_OUTPUT: &str = "raw_provider_output";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactEntry {
    pub artifact_type: String,
    pub title: String,
    pub summary: Option<String>,
    pub path: Option<PathBuf>,
    pub metadata: HashMap<String, serde_json::Value>,
}
