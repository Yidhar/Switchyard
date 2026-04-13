//! Gemini probe: detect installation, version, capabilities.
//!
//! Gemini uses `-v` not `--version`, and on Windows the `.cmd` wrapper
//! may fail so we fall back to the shared probe_version logic.

use std::path::PathBuf;

use switchyard_provider_api::{HostSurfaceKind, HostSurfaceProbe, ProbeResult, ProviderError};
use switchyard_provider_subprocess::{SubprocessConfig, default_cli_capabilities, run_subprocess};

pub async fn run_probe(command: &str) -> Result<ProbeResult, ProviderError> {
    let args = vec!["-v".to_string()];
    let config = SubprocessConfig {
        command,
        args: &args,
        stdin_data: None,
        timeout_secs: 10,
        cwd: None,
        pty_registry_key: None,
        prefer_pty: false,
    };

    let output = match run_subprocess(&config).await {
        Ok(o) if o.exit_code == Some(0) => o,
        _ => {
            // Direct invocation failed; try shared probe (handles npm node fallback)
            let probe = switchyard_provider_subprocess::probe_version(command).await;
            if let Some(version) = probe.version {
                return Ok(ProbeResult {
                    version: Some(version),
                    available: true,
                    capabilities: default_cli_capabilities(),
                    issues: vec![],
                    host_surface: detect_host_surface(),
                });
            }
            return Err(ProviderError::NotInstalled(format!(
                "command '{command}' not found or not responding"
            )));
        }
    };

    let version = output.stdout.lines().next().map(|l| l.trim().to_string());

    Ok(ProbeResult {
        version,
        available: true,
        capabilities: default_cli_capabilities(),
        issues: vec![],
        host_surface: detect_host_surface(),
    })
}

fn detect_host_surface() -> HostSurfaceProbe {
    let Some(home) = user_home_dir() else {
        return HostSurfaceProbe {
            kind: HostSurfaceKind::NativeCustomCommand,
            installed: false,
            configured: false,
            discoverable: false,
            notes: vec!["could not resolve user home directory".to_string()],
        };
    };

    let skill = home.join(".gemini").join("skills").join("hyard.md");
    let manifest = home.join(".gemini").join("hyard-extension.yaml");

    let skill_exists = skill.exists();
    let manifest_exists = manifest.exists();

    let mut notes = Vec::new();
    if !skill_exists {
        notes.push(format!("missing HYARD Gemini skill: {}", skill.display()));
    }
    if !manifest_exists {
        notes.push(format!(
            "missing HYARD Gemini extension manifest: {}",
            manifest.display()
        ));
    }

    HostSurfaceProbe {
        kind: HostSurfaceKind::NativeCustomCommand,
        installed: skill_exists || manifest_exists,
        configured: skill_exists && manifest_exists,
        discoverable: skill_exists || manifest_exists,
        notes,
    }
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}
