mod error;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use switchyard_store::StoreBackend;

pub use error::ConfigError;

const CONFIG_FILENAME: &str = "switchyard.toml";
const DOT_DIR: &str = ".switchyard";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwitchyardConfig {
    #[serde(default)]
    pub core: CoreConfig,
    #[serde(default)]
    pub providers: HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub sandbox: SandboxConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub store: StoreConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub orchestrator: OrchestratorConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    #[serde(default)]
    pub mode: SandboxMode,
    #[serde(default)]
    pub allowed_paths: Vec<PathBuf>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            mode: SandboxMode::WorkspaceWrite,
            allowed_paths: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxMode {
    ReadOnly,
    #[default]
    WorkspaceWrite,
    DangerFullAccess,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    #[serde(default)]
    pub worker_retry: WorkerRetryConfig,
}

/// Retry policy applied by the WorkerSupervisor when a worker dies mid-turn.
/// `max_attempts` includes the original attempt — `max_attempts=3` means up
/// to 2 retries after the initial try. `backoff_ms` lengths beyond
/// `max_attempts-1` are ignored; shorter lists pad with the last value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerRetryConfig {
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    #[serde(default = "default_backoff_ms")]
    pub backoff_ms: Vec<u64>,
}

fn default_max_attempts() -> u32 {
    3
}

fn default_backoff_ms() -> Vec<u64> {
    vec![2000, 5000, 10000]
}

impl Default for WorkerRetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: default_max_attempts(),
            backoff_ms: default_backoff_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreConfig {
    pub default_provider: String,
    #[serde(default)]
    pub default_peers: Vec<String>,
}

impl Default for CoreConfig {
    fn default() -> Self {
        Self {
            default_provider: "codex".to_string(),
            default_peers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Provider turn hard timeout in seconds. `0` disables the hard timeout;
    /// callers should use cancellation/heartbeat supervision for long tasks.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
    #[serde(default)]
    pub backend: Option<String>,
}

fn default_timeout() -> u64 {
    // 0 means "no provider turn hard timeout". Long-running agent work is
    // supervised by cancellation/heartbeat paths instead of a fixed wall-clock
    // kill switch.
    0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub directory: Option<PathBuf>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            directory: Some(PathBuf::from(DOT_DIR).join("sessions")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StoreConfig {
    #[serde(default)]
    pub backend: StoreBackendConfig,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StoreBackendConfig {
    #[default]
    Auto,
    Jsonl,
    Sqlite,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    #[serde(default = "default_true")]
    pub show_diff: bool,
    #[serde(default = "default_true")]
    pub show_artifacts: bool,
}

fn default_true() -> bool {
    true
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            show_diff: true,
            show_artifacts: true,
        }
    }
}

impl SwitchyardConfig {
    /// Parse config from a TOML string.
    pub fn parse_toml(s: &str) -> Result<Self, ConfigError> {
        toml::from_str(s).map_err(ConfigError::Parse)
    }

    /// Load config from an explicit file path.
    pub fn from_file(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
        Self::parse_toml(&content)
    }

    /// Resolve and load config by searching upward from `start_dir` for `switchyard.toml`,
    /// then falling back to `~/.switchyard/switchyard.toml`.
    /// Returns default config if no file is found.
    pub fn resolve(start_dir: &Path) -> Result<Self, ConfigError> {
        if let Some(path) = Self::find_config_file(start_dir) {
            return Self::from_file(&path);
        }

        if let Some(home) = home_dir() {
            let home_config = home.join(DOT_DIR).join(CONFIG_FILENAME);
            if home_config.is_file() {
                return Self::from_file(&home_config);
            }
        }

        Ok(Self::default())
    }

    /// Write the current configuration back to the specified path in TOML format.
    pub fn write_to(&self, path: &Path) -> Result<(), ConfigError> {
        let toml_str = toml::to_string_pretty(self)?;
        std::fs::write(path, toml_str)?;
        Ok(())
    }

    /// Walk from `start_dir` up to filesystem root, looking for `switchyard.toml`.
    fn find_config_file(start_dir: &Path) -> Option<PathBuf> {
        let mut dir = start_dir.to_path_buf();
        loop {
            let candidate = dir.join(CONFIG_FILENAME);
            if candidate.is_file() {
                return Some(candidate);
            }
            if !dir.pop() {
                return None;
            }
        }
    }

    /// Resolved session directory, relative to `project_root`.
    pub fn session_dir(&self, project_root: &Path) -> PathBuf {
        match &self.session.directory {
            Some(dir) if dir.is_absolute() => dir.clone(),
            Some(dir) => project_root.join(dir),
            None => project_root.join(DOT_DIR).join("sessions"),
        }
    }

    /// Selected canonical store backend.
    pub fn store_backend(&self, project_root: &Path) -> StoreBackend {
        match self.store.backend {
            StoreBackendConfig::Auto => self.auto_store_backend(project_root),
            StoreBackendConfig::Jsonl => StoreBackend::Jsonl,
            StoreBackendConfig::Sqlite => StoreBackend::Sqlite,
        }
    }

    /// Resolved canonical store path, relative to `project_root` when configured
    /// as a relative path.
    pub fn store_path(&self, project_root: &Path) -> PathBuf {
        match &self.store.path {
            Some(path) if path.is_absolute() => path.clone(),
            Some(path) => project_root.join(path),
            None => match self.store_backend(project_root) {
                StoreBackend::Jsonl => self.session_dir(project_root),
                StoreBackend::Sqlite => project_root.join(DOT_DIR).join("store.sqlite3"),
            },
        }
    }

    /// Resolved artifact directory, relative to `project_root`.
    pub fn artifact_dir(&self, project_root: &Path) -> PathBuf {
        project_root.join(DOT_DIR).join("artifacts")
    }

    /// Resolved runtime-authority SQLite database path.
    ///
    /// Runtime state is intentionally stored next to the HYARD job directory
    /// rather than inside the session store.  This keeps the event-log authority
    /// independent from the user-facing conversation backend while still
    /// honoring absolute `session.directory = ".../sessions"` deployments that
    /// place Switchyard state outside the project tree.
    pub fn runtime_db_path(&self, project_root: &Path) -> PathBuf {
        self.job_dir(project_root)
            .parent()
            .map(|parent| parent.join("runtime.sqlite3"))
            .unwrap_or_else(|| project_root.join(DOT_DIR).join("runtime.sqlite3"))
    }

    /// Resolved runtime IPC endpoint for local committed-event broadcasts.
    ///
    /// The endpoint is derived from the durable runtime DB path so independent
    /// workspaces do not collide. On Windows this is a named pipe; on Unix it is
    /// a socket file next to the runtime DB.
    pub fn runtime_ipc_endpoint(&self, project_root: &Path) -> String {
        let runtime_db_path = self.runtime_db_path(project_root);
        let hash = stable_path_hash(&runtime_db_path);
        #[cfg(windows)]
        {
            format!(r"\\.\pipe\switchyard-runtime-{hash:016x}")
        }
        #[cfg(unix)]
        {
            runtime_db_path
                .with_file_name(format!("runtime-{hash:016x}.sock"))
                .to_string_lossy()
                .to_string()
        }
    }

    /// Additional sandbox allow-list paths resolved relative to
    /// `project_root`. The primary workspace directory is not included here;
    /// core policy builders add it automatically for `workspace-write`.
    pub fn sandbox_allowed_paths(&self, project_root: &Path) -> Vec<PathBuf> {
        self.sandbox
            .allowed_paths
            .iter()
            .map(|path| {
                if path.is_absolute() {
                    path.clone()
                } else {
                    project_root.join(path)
                }
            })
            .collect()
    }

    /// Resolved HYARD job directory.
    ///
    /// When the session directory looks like `.../sessions`, job manifests live
    /// next to it under `.../jobs`. Otherwise we fall back to the project-local
    /// `.switchyard/jobs` path.
    pub fn job_dir(&self, project_root: &Path) -> PathBuf {
        let session_dir = self.session_dir(project_root);
        let looks_like_sessions_dir =
            session_dir.file_name().and_then(|name| name.to_str()) == Some("sessions");

        if looks_like_sessions_dir {
            session_dir
                .parent()
                .map(|parent| parent.join("jobs"))
                .unwrap_or_else(|| project_root.join(DOT_DIR).join("jobs"))
        } else {
            project_root.join(DOT_DIR).join("jobs")
        }
    }

    /// Validate config for structural issues that TOML parsing alone cannot catch.
    /// Returns a list of human-readable warnings/errors.
    pub fn validate(&self) -> Vec<String> {
        let mut issues = Vec::new();

        if self.core.default_provider.is_empty() {
            issues.push("core.default_provider is empty".to_string());
        }

        // default_provider should have a matching entry in providers (if any are defined)
        if !self.providers.is_empty() && !self.providers.contains_key(&self.core.default_provider) {
            issues.push(format!(
                "core.default_provider '{}' has no matching [providers.{}] section",
                self.core.default_provider, self.core.default_provider,
            ));
        }

        // peers should also have matching provider entries
        for peer in &self.core.default_peers {
            if !self.providers.is_empty() && !self.providers.contains_key(peer) {
                issues.push(format!(
                    "core.default_peers contains '{}' but no [providers.{}] section exists",
                    peer, peer,
                ));
            }
        }

        for (name, provider) in &self.providers {
            if provider.command.is_empty() {
                issues.push(format!("providers.{name}.command is empty"));
            }
        }

        issues
    }

    fn auto_store_backend(&self, project_root: &Path) -> StoreBackend {
        if let Some(configured_path) = self.store.path.as_ref() {
            let resolved_path = if configured_path.is_absolute() {
                configured_path.clone()
            } else {
                project_root.join(configured_path)
            };
            return infer_backend_from_path_hint(&resolved_path);
        }

        let sqlite_default = project_root.join(DOT_DIR).join("store.sqlite3");
        let jsonl_default = self.session_dir(project_root);

        if sqlite_default.is_file() {
            StoreBackend::Sqlite
        } else if jsonl_default.exists() {
            StoreBackend::Jsonl
        } else {
            StoreBackend::Sqlite
        }
    }
}

fn infer_backend_from_path_hint(path: &Path) -> StoreBackend {
    if path.is_dir() {
        return StoreBackend::Jsonl;
    }

    let lower = path.to_string_lossy().to_ascii_lowercase();
    if lower.ends_with("/sessions") || lower.ends_with("\\sessions") {
        return StoreBackend::Jsonl;
    }

    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("sqlite") | Some("sqlite3") | Some("db") | Some("db3") => StoreBackend::Sqlite,
        _ => StoreBackend::Sqlite,
    }
}

fn stable_path_hash(path: &Path) -> u64 {
    // FNV-1a: tiny, deterministic, and sufficient for local endpoint naming.
    // Avoid DefaultHasher because its exact output is not a stable API.
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let text = path.to_string_lossy().to_ascii_lowercase();
    text.as_bytes().iter().fold(FNV_OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

impl FromStr for SwitchyardConfig {
    type Err = ConfigError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_toml(s)
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let cfg = SwitchyardConfig::default();
        assert_eq!(cfg.core.default_provider, "codex");
        assert!(cfg.core.default_peers.is_empty());
        assert_eq!(cfg.sandbox.mode, SandboxMode::WorkspaceWrite);
        assert!(cfg.sandbox.allowed_paths.is_empty());
        assert_eq!(cfg.store.backend, StoreBackendConfig::Auto);
        assert_eq!(
            cfg.store_backend(Path::new("/project")),
            StoreBackend::Sqlite
        );
        assert!(cfg.ui.show_diff);
        assert!(cfg.ui.show_artifacts);
    }

    #[test]
    fn parse_minimal_toml() {
        let cfg = SwitchyardConfig::parse_toml("").unwrap();
        assert_eq!(cfg.core.default_provider, "codex");
    }

    #[test]
    fn parse_full_toml() {
        let toml = r#"
[core]
default_provider = "claude"
default_peers = ["codex", "gemini"]

[providers.claude]
command = "claude"
args = ["-p", "--output-format", "stream-json"]
timeout_secs = 120

[providers.codex]
command = "codex"
args = ["--quiet"]

[sandbox]
mode = "read-only"
allowed_paths = ["../shared", "/tmp/cache"]

[session]
directory = ".my-sessions"

[store]
backend = "sqlite"
path = ".switchyard/store.sqlite3"

[ui]
show_diff = true
show_artifacts = false
"#;
        let cfg = SwitchyardConfig::parse_toml(toml).unwrap();
        assert_eq!(cfg.core.default_provider, "claude");
        assert_eq!(cfg.core.default_peers, vec!["codex", "gemini"]);
        assert_eq!(cfg.providers["claude"].command, "claude");
        assert_eq!(cfg.providers["claude"].timeout_secs, 120);
        assert_eq!(cfg.providers["codex"].timeout_secs, 0); // default: no hard timeout
        assert_eq!(cfg.sandbox.mode, SandboxMode::ReadOnly);
        assert_eq!(
            cfg.sandbox.allowed_paths,
            vec![PathBuf::from("../shared"), PathBuf::from("/tmp/cache")]
        );
        assert_eq!(cfg.store.backend, StoreBackendConfig::Sqlite);
        assert_eq!(
            cfg.store.path,
            Some(PathBuf::from(".switchyard/store.sqlite3"))
        );
        assert!(!cfg.ui.show_artifacts);
    }

    #[test]
    fn parse_invalid_toml_returns_error() {
        let result = SwitchyardConfig::parse_toml("not valid [[[toml");
        assert!(result.is_err());
    }

    #[test]
    fn session_dir_relative() {
        let cfg = SwitchyardConfig::default();
        let dir = cfg.session_dir(Path::new("/project"));
        assert_eq!(dir, PathBuf::from("/project/.switchyard/sessions"));
    }

    #[test]
    fn parse_sandbox_modes_and_allowed_paths() {
        let cfg = SwitchyardConfig::parse_toml(
            r#"
[sandbox]
mode = "danger-full-access"
allowed_paths = ["../shared", "scratch"]
"#,
        )
        .unwrap();
        assert_eq!(cfg.sandbox.mode, SandboxMode::DangerFullAccess);
        assert_eq!(
            cfg.sandbox.allowed_paths,
            vec![PathBuf::from("../shared"), PathBuf::from("scratch")]
        );
    }

    #[test]
    fn sandbox_allowed_paths_resolve_relative_to_project_root() {
        let mut cfg = SwitchyardConfig::default();
        cfg.sandbox.allowed_paths = vec![
            PathBuf::from("../shared"),
            PathBuf::from("/absolute/cache"),
            PathBuf::from("scratch"),
        ];
        assert_eq!(
            cfg.sandbox_allowed_paths(Path::new("/project/app")),
            vec![
                PathBuf::from("/project/app/../shared"),
                PathBuf::from("/absolute/cache"),
                PathBuf::from("/project/app/scratch"),
            ]
        );
    }

    #[test]
    fn session_dir_absolute_override() {
        let mut cfg = SwitchyardConfig::default();
        cfg.session.directory = Some(PathBuf::from("/custom/sessions"));
        let dir = cfg.session_dir(Path::new("/project"));
        assert_eq!(dir, PathBuf::from("/custom/sessions"));
    }

    #[test]
    fn store_path_defaults_to_session_dir_for_jsonl() {
        let mut cfg = SwitchyardConfig::default();
        cfg.store.backend = StoreBackendConfig::Jsonl;
        let path = cfg.store_path(Path::new("/project"));
        assert_eq!(path, PathBuf::from("/project/.switchyard/sessions"));
    }

    #[test]
    fn store_path_defaults_to_project_sqlite_file_for_sqlite() {
        let mut cfg = SwitchyardConfig::default();
        cfg.store.backend = StoreBackendConfig::Sqlite;
        let path = cfg.store_path(Path::new("/project"));
        assert_eq!(path, PathBuf::from("/project/.switchyard/store.sqlite3"));
    }

    #[test]
    fn store_path_relative_override_is_resolved_from_project_root() {
        let mut cfg = SwitchyardConfig::default();
        cfg.store.backend = StoreBackendConfig::Sqlite;
        cfg.store.path = Some(PathBuf::from("data/store.sqlite3"));
        let path = cfg.store_path(Path::new("/project"));
        assert_eq!(path, PathBuf::from("/project/data/store.sqlite3"));
    }

    #[test]
    fn store_path_absolute_override_is_preserved() {
        let mut cfg = SwitchyardConfig::default();
        cfg.store.path = Some(PathBuf::from("/custom/store.sqlite3"));
        let path = cfg.store_path(Path::new("/project"));
        assert_eq!(path, PathBuf::from("/custom/store.sqlite3"));
    }

    #[test]
    fn resolve_returns_default_when_no_file() {
        let cfg = SwitchyardConfig::resolve(Path::new("/nonexistent/path/unlikely")).unwrap();
        assert_eq!(cfg.core.default_provider, "codex");
    }

    #[test]
    fn auto_store_backend_prefers_legacy_jsonl_when_sessions_dir_exists() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = SwitchyardConfig::default();
        std::fs::create_dir_all(cfg.session_dir(temp.path())).unwrap();
        assert_eq!(cfg.store_backend(temp.path()), StoreBackend::Jsonl);
        assert_eq!(cfg.store_path(temp.path()), cfg.session_dir(temp.path()));
    }

    #[test]
    fn auto_store_backend_prefers_sqlite_when_sqlite_exists() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = SwitchyardConfig::default();
        let sqlite_path = cfg.store_path(temp.path());
        std::fs::create_dir_all(sqlite_path.parent().unwrap()).unwrap();
        std::fs::write(&sqlite_path, b"sqlite-placeholder").unwrap();
        std::fs::create_dir_all(cfg.session_dir(temp.path())).unwrap();
        assert_eq!(cfg.store_backend(temp.path()), StoreBackend::Sqlite);
        assert_eq!(cfg.store_path(temp.path()), sqlite_path);
    }

    #[test]
    fn job_dir_defaults_next_to_sessions() {
        let cfg = SwitchyardConfig::default();
        let dir = cfg.job_dir(Path::new("/project"));
        assert_eq!(dir, PathBuf::from("/project/.switchyard/jobs"));
    }

    #[test]
    fn runtime_db_path_defaults_next_to_jobs() {
        let cfg = SwitchyardConfig::default();
        let path = cfg.runtime_db_path(Path::new("/project"));
        assert_eq!(path, PathBuf::from("/project/.switchyard/runtime.sqlite3"));
    }

    #[test]
    fn job_dir_uses_absolute_session_parent_when_named_sessions() {
        let mut cfg = SwitchyardConfig::default();
        cfg.session.directory = Some(PathBuf::from("/custom/sessions"));
        let dir = cfg.job_dir(Path::new("/project"));
        assert_eq!(dir, PathBuf::from("/custom/jobs"));
    }

    #[test]
    fn runtime_db_path_uses_absolute_session_parent_when_named_sessions() {
        let mut cfg = SwitchyardConfig::default();
        cfg.session.directory = Some(PathBuf::from("/custom/sessions"));
        let path = cfg.runtime_db_path(Path::new("/project"));
        assert_eq!(path, PathBuf::from("/custom/runtime.sqlite3"));
    }

    #[test]
    fn job_dir_falls_back_for_custom_session_name() {
        let mut cfg = SwitchyardConfig::default();
        cfg.session.directory = Some(PathBuf::from(".custom-sessions"));
        let dir = cfg.job_dir(Path::new("/project"));
        assert_eq!(dir, PathBuf::from("/project/.switchyard/jobs"));
    }

    #[test]
    fn runtime_db_path_falls_back_for_custom_session_name() {
        let mut cfg = SwitchyardConfig::default();
        cfg.session.directory = Some(PathBuf::from(".custom-sessions"));
        let path = cfg.runtime_db_path(Path::new("/project"));
        assert_eq!(path, PathBuf::from("/project/.switchyard/runtime.sqlite3"));
    }

    #[test]
    fn runtime_ipc_endpoint_is_stable_and_workspace_scoped() {
        let cfg = SwitchyardConfig::default();
        let left = cfg.runtime_ipc_endpoint(Path::new("/project-a"));
        let left_again = cfg.runtime_ipc_endpoint(Path::new("/project-a"));
        let right = cfg.runtime_ipc_endpoint(Path::new("/project-b"));

        assert_eq!(left, left_again);
        assert_ne!(left, right);
        #[cfg(windows)]
        assert!(left.starts_with(r"\\.\pipe\switchyard-runtime-"));
        #[cfg(unix)]
        assert!(left.ends_with(".sock"));
    }

    #[test]
    fn from_file_missing_returns_error() {
        let result = SwitchyardConfig::from_file(Path::new("/nonexistent/switchyard.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn validate_default_config_no_issues() {
        // Default config with no providers defined is valid (providers are optional)
        let cfg = SwitchyardConfig::default();
        assert!(cfg.validate().is_empty());
    }

    #[test]
    fn validate_empty_command() {
        let toml = r#"
[providers.broken]
command = ""
"#;
        let cfg = SwitchyardConfig::parse_toml(toml).unwrap();
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| i.contains("command is empty")));
    }

    #[test]
    fn validate_zero_timeout_is_unlimited() {
        let toml = r#"
[core]
default_provider = "fast"

[providers.fast]
command = "codex"
timeout_secs = 0
"#;
        let cfg = SwitchyardConfig::parse_toml(toml).unwrap();
        let issues = cfg.validate();
        assert!(issues.is_empty());
    }

    #[test]
    fn validate_missing_default_provider_entry() {
        let toml = r#"
[core]
default_provider = "claude"

[providers.codex]
command = "codex"
"#;
        let cfg = SwitchyardConfig::parse_toml(toml).unwrap();
        let issues = cfg.validate();
        assert!(
            issues
                .iter()
                .any(|i| i.contains("claude") && i.contains("no matching"))
        );
    }

    #[test]
    fn validate_missing_peer_entry() {
        let toml = r#"
[core]
default_provider = "codex"
default_peers = ["claude", "gemini"]

[providers.codex]
command = "codex"
"#;
        let cfg = SwitchyardConfig::parse_toml(toml).unwrap();
        let issues = cfg.validate();
        assert!(issues.iter().any(|i| i.contains("claude")));
        assert!(issues.iter().any(|i| i.contains("gemini")));
    }

    #[test]
    fn validate_well_formed_config() {
        let toml = r#"
[core]
default_provider = "codex"
default_peers = ["claude"]

[providers.codex]
command = "codex"

[providers.claude]
command = "claude"
"#;
        let cfg = SwitchyardConfig::parse_toml(toml).unwrap();
        assert!(cfg.validate().is_empty());
    }
}
