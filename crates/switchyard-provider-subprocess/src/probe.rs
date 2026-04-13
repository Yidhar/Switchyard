//! Generic version probing for CLI-based providers.
//!
//! Tries `--version`, then `-v`, with timeout. On Windows, falls back to
//! invoking npm-installed CLIs via `node` directly when `.cmd` wrappers fail.

use crate::resolve::{find_on_path, resolve_command, resolve_npm_entry};
use crate::runner::SubprocessConfig;

/// Probe result from a generic CLI version check.
#[derive(Debug)]
pub struct VersionProbe {
    /// First line of version output (e.g. "0.116.0", "2.1.81 (Claude Code)").
    pub version: Option<String>,
    /// Resolved command path that was used.
    pub resolved_command: Option<String>,
}

/// Try to detect a CLI's version by running it with version flags.
///
/// Tries `--version` and `-v` with timeout. On Windows, falls back to
/// running via `node` for npm-installed scripts.
pub async fn probe_version(cmd: &str) -> VersionProbe {
    let found = find_on_path(cmd);
    let resolved = found.clone().unwrap_or_else(|| cmd.to_string());

    // Try direct execution with version flags
    for flag in &["--version", "-v"] {
        if let Some(version) = try_version_flag(&resolved, flag).await {
            return VersionProbe {
                version: Some(version),
                resolved_command: Some(resolved),
            };
        }
    }

    // Windows fallback: try via node for npm packages
    if cfg!(windows)
        && let Some(js_entry) = resolve_npm_entry(&resolved)
    {
        let js_path = js_entry.to_string_lossy().to_string();
        for flag in &["--version", "-v"] {
            if let Some(version) = try_node_version_flag(&js_path, flag).await {
                return VersionProbe {
                    version: Some(version),
                    resolved_command: Some(resolved),
                };
            }
        }
    }

    VersionProbe {
        version: None,
        resolved_command: if found.is_some() {
            Some(resolved)
        } else {
            None
        },
    }
}

async fn try_version_flag(cmd: &str, flag: &str) -> Option<String> {
    let args = vec![flag.to_string()];
    let config = SubprocessConfig {
        command: cmd,
        args: &args,
        stdin_data: None,
        timeout_secs: 5,
        cwd: None,
        pty_registry_key: None,
        prefer_pty: false,
    };
    match crate::runner::run_subprocess(&config).await {
        Ok(output) if output.exit_code == Some(0) => first_nonempty_line(&output.stdout),
        _ => None,
    }
}

async fn try_node_version_flag(js_path: &str, flag: &str) -> Option<String> {
    let node_command = resolve_command("node");
    let args = vec![js_path.to_string(), flag.to_string()];
    let config = SubprocessConfig {
        command: &node_command,
        args: &args,
        stdin_data: None,
        timeout_secs: 5,
        cwd: None,
        pty_registry_key: None,
        prefer_pty: false,
    };
    match crate::runner::run_subprocess(&config).await {
        Ok(output) if output.exit_code == Some(0) => first_nonempty_line(&output.stdout),
        _ => None,
    }
}

fn first_nonempty_line(s: &str) -> Option<String> {
    s.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
}

/// Check if a command is available on PATH (without executing it).
pub fn is_available(cmd: &str) -> bool {
    find_on_path(cmd).is_some()
}
