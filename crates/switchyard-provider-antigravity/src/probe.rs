//! Antigravity probe: detect installation, capture host-surface state.
//!
//! `agy` does not honour `--version` or `-v` as of 2026-05 (issue
//! [agy#7](https://github.com/google-antigravity/antigravity-cli/issues/7)).
//! We try the standard flags via the shared `probe_version` helper to
//! future-proof, then fall back to `agy --help` to confirm the binary
//! responds. As a last resort, PATH membership alone marks the provider
//! installed (with a `version=None` note).

use std::path::PathBuf;

use switchyard_provider_api::{HostSurfaceKind, HostSurfaceProbe, ProbeResult, ProviderError};
use switchyard_provider_subprocess::{SubprocessConfig, default_cli_capabilities, run_subprocess};

pub async fn run_probe(command: &str) -> Result<ProbeResult, ProviderError> {
    // 1. Try the standard version flags first — agy ignores them today but
    //    will hopefully accept `--version` soon.
    let shared = switchyard_provider_subprocess::probe_version(command).await;
    if let Some(version) = shared.version {
        return Ok(ProbeResult {
            version: Some(version),
            available: true,
            capabilities: default_cli_capabilities(),
            issues: vec![],
            host_surface: detect_host_surface(),
        });
    }

    // 2. `agy --help` is the cheapest "binary is alive" check until
    //    `--version` lands. Exit code 0 + non-empty stdout = available.
    let args = vec!["--help".to_string()];
    let config = SubprocessConfig {
        command,
        args: &args,
        stdin_data: None,
        timeout_secs: 10,
        cwd: None,
        pty_registry_key: None,
        prefer_pty: false,
        env: None,
    };

    match run_subprocess(&config).await {
        Ok(out) if out.exit_code == Some(0) && !out.stdout.trim().is_empty() => Ok(ProbeResult {
            // No reliable version surface yet. Surface `None` so callers
            // don't claim a fake number.
            version: None,
            available: true,
            capabilities: default_cli_capabilities(),
            issues: vec![
                "agy does not yet expose a version flag (#7); version unavailable".to_string(),
            ],
            host_surface: detect_host_surface(),
        }),
        Ok(out) => Err(ProviderError::NotInstalled(format!(
            "'{command} --help' exited {:?} with empty stdout — not Antigravity?",
            out.exit_code
        ))),
        Err(_) => {
            // Last resort: PATH lookup alone. Useful in CI where the binary
            // exists but won't run (e.g. requires keychain access).
            if switchyard_provider_subprocess::is_available(command) {
                Ok(ProbeResult {
                    version: None,
                    available: false,
                    capabilities: default_cli_capabilities(),
                    issues: vec![format!(
                        "'{command}' found on PATH but did not respond to --help"
                    )],
                    host_surface: detect_host_surface(),
                })
            } else {
                Err(ProviderError::NotInstalled(format!(
                    "command '{command}' not found on PATH"
                )))
            }
        }
    }
}

fn detect_host_surface() -> HostSurfaceProbe {
    // Antigravity shares Gemini's config tree under `~/.gemini/`. The
    // antigravity-cli specific subtree is `~/.gemini/antigravity-cli/`.
    let Some(home) = user_home_dir() else {
        return HostSurfaceProbe {
            kind: HostSurfaceKind::NativeCustomCommand,
            installed: false,
            configured: false,
            discoverable: false,
            notes: vec!["could not resolve user home directory".to_string()],
        };
    };

    let agy_root = home.join(".gemini").join("antigravity-cli");
    let conversations_dir = agy_root.join("conversations");
    let settings_file = agy_root.join("settings.json");
    let cache_index = agy_root.join("cache").join("last_conversations.json");

    let any_state = conversations_dir.exists() || settings_file.exists() || cache_index.exists();
    let fully_configured = settings_file.exists() && conversations_dir.exists();

    let mut notes = Vec::new();
    if !settings_file.exists() {
        notes.push(format!(
            "antigravity settings file missing: {}",
            settings_file.display()
        ));
    }
    if !conversations_dir.exists() {
        notes.push(format!(
            "no conversation history yet: {}",
            conversations_dir.display()
        ));
    }
    if !cache_index.exists() {
        notes.push(format!(
            "cache index missing (needed for future continuation warmpath): {}",
            cache_index.display()
        ));
    }

    HostSurfaceProbe {
        kind: HostSurfaceKind::NativeCustomCommand,
        installed: any_state,
        configured: fully_configured,
        discoverable: any_state,
        notes,
    }
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}
