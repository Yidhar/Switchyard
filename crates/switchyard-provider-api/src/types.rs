use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnInput {
    pub user_message: String,
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<InputAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputAttachment {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

impl TurnInput {
    pub fn text(user_message: impl Into<String>) -> Self {
        Self {
            user_message: user_message.into(),
            system_prompt: None,
            attachments: Vec::new(),
        }
    }

    pub fn with_attachments(mut self, attachments: Vec<InputAttachment>) -> Self {
        self.attachments = attachments;
        self
    }

    /// Return the user-visible task text with a stable, lightweight list of
    /// attached local files appended. Providers with native multimodal support
    /// still receive the actual attachment payload separately; this note keeps
    /// transcript history and non-native providers from silently dropping the
    /// user's image references.
    pub fn user_message_with_attachment_references(&self) -> String {
        if self.attachments.is_empty() || self.user_message.contains("[Switchyard Attachments]") {
            return self.user_message.clone();
        }

        let mut out = self.user_message.trim_end().to_string();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("[Switchyard Attachments]\n");
        out.push_str(
            "The user attached these local image files. Treat them as visual inputs when your provider supports images; otherwise use the file paths as references:\n",
        );
        for attachment in &self.attachments {
            match attachment.mime_type.as_deref() {
                Some(mime) => out.push_str(&format!("- {} ({mime})\n", attachment.path.display())),
                None => out.push_str(&format!("- {}\n", attachment.path.display())),
            }
        }
        out.trim_end().to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    pub timeout_secs: u64,
    pub write_access: bool,
    pub cwd: PathBuf,
    pub allowed_paths: Vec<PathBuf>,
}

/// Switchyard's effective sandbox posture after reducing the legacy
/// [`ExecutionPolicy`] fields.
///
/// The provider API intentionally keeps [`ExecutionPolicy`] wire-compatible:
/// `write_access = true` plus an empty `allowed_paths` remains the explicit
/// "danger / no path restriction" sentinel. New user-facing flows should use
/// [`ExecutionPolicy::workspace_write`] or [`ExecutionPolicy::read_only`]
/// rather than constructing that sentinel accidentally.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl Default for ExecutionPolicy {
    /// Conservative defaults: no write access, no allowed paths, current
    /// directory as cwd, 0 timeout (meaning "use the provider's default").
    /// Hook handlers and live-instance drain tasks should treat this as
    /// "deny everything risky" — operations that need write access or a
    /// specific allowed path must be opted in by the caller.
    fn default() -> Self {
        Self {
            timeout_secs: 0,
            write_access: false,
            cwd: PathBuf::from("."),
            allowed_paths: Vec::new(),
        }
    }
}

impl ExecutionPolicy {
    /// Deny all write approvals and request read-only CLI sandboxing.
    pub fn read_only(cwd: impl Into<PathBuf>) -> Self {
        Self {
            timeout_secs: 0,
            write_access: false,
            cwd: cwd.into(),
            allowed_paths: Vec::new(),
        }
    }

    /// Allow writes only inside `cwd` and any subsequently-added allowed
    /// paths. This is the safe default for user-facing Switchyard turns.
    pub fn workspace_write(cwd: impl Into<PathBuf>) -> Self {
        let cwd = cwd.into();
        Self {
            timeout_secs: 0,
            write_access: true,
            cwd: cwd.clone(),
            allowed_paths: vec![cwd],
        }
    }

    /// Explicit no-sandbox/no-path-restriction policy. This preserves the
    /// legacy `write_access=true + allowed_paths=[]` sentinel, but gives call
    /// sites a self-documenting constructor so permissive execution is never
    /// introduced by accident.
    pub fn danger_full_access(cwd: impl Into<PathBuf>) -> Self {
        Self {
            timeout_secs: 0,
            write_access: true,
            cwd: cwd.into(),
            allowed_paths: Vec::new(),
        }
    }

    /// Approve-everything sentinel for callers that want the pre-policy
    /// behavior (smoke tests, the no-arg `LiveInstance::send_message`
    /// entry point). `write_access = true` + an empty `allowed_paths`
    /// signals "no path restriction" rather than "deny everything", which
    /// the Codex drain task interprets as "approve every server-initiated
    /// request". Do NOT use in user-facing flows — it bypasses Switchyard
    /// gating entirely.
    pub fn permissive() -> Self {
        Self::danger_full_access(".")
    }

    pub fn with_timeout_secs(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    pub fn with_allowed_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.allowed_paths = dedup_paths(paths);
        self
    }

    pub fn add_allowed_paths<I>(mut self, paths: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        for path in paths {
            if !contains_equivalent_path(&self.allowed_paths, &path) {
                self.allowed_paths.push(path);
            }
        }
        self
    }

    pub fn effective_sandbox_mode(&self) -> EffectiveSandboxMode {
        if !self.write_access {
            EffectiveSandboxMode::ReadOnly
        } else if self.allowed_paths.is_empty() {
            EffectiveSandboxMode::DangerFullAccess
        } else {
            EffectiveSandboxMode::WorkspaceWrite
        }
    }

    /// Paths that should be exposed to provider CLIs in addition to their
    /// primary working directory. The workspace-write constructor includes
    /// `cwd` in `allowed_paths` for approval gating; provider-specific
    /// `--add-dir` flags should not repeat it.
    pub fn additional_allowed_paths(&self) -> Vec<PathBuf> {
        let cwd = lexical_normalize(&self.cwd);
        self.allowed_paths
            .iter()
            .filter(|path| lexical_normalize(path) != cwd)
            .cloned()
            .collect()
    }
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if !contains_equivalent_path(&out, &path) {
            out.push(path);
        }
    }
    out
}

fn contains_equivalent_path(paths: &[PathBuf], candidate: &Path) -> bool {
    let normalized = lexical_normalize(candidate);
    paths
        .iter()
        .any(|path| lexical_normalize(path) == normalized)
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop a normal segment. Preserve leading `..` and root /
                // prefix components so relative escape intent is not erased.
                let popped = out.pop();
                if !popped {
                    out.push(component.as_os_str());
                }
            }
            _ => out.push(component.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
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
