//! Claude probe: detect installation, version, capabilities.

use std::path::PathBuf;

use switchyard_provider_api::{HostSurfaceKind, HostSurfaceProbe, ProbeResult, ProviderError};
use switchyard_provider_subprocess::{
    SubprocessConfig, check_auth_error, default_cli_capabilities, run_subprocess,
};

pub async fn run_probe(command: &str) -> Result<ProbeResult, ProviderError> {
    let args = vec!["--version".to_string()];
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

    let output = match run_subprocess(&config).await {
        Ok(o) => o,
        Err(switchyard_provider_subprocess::SubprocessError::NotFound(_)) => {
            return Err(ProviderError::NotInstalled(format!(
                "command '{command}' not found"
            )));
        }
        Err(e) => {
            return Err(ProviderError::ExecutionFailed(format!(
                "'{command} --version': {e}"
            )));
        }
    };

    if output.exit_code != Some(0) {
        if let Some(err) = check_auth_error(
            &output.stdout,
            output.stderr.as_deref().unwrap_or(""),
            "claude",
        ) {
            return Err(err);
        }
        return Err(ProviderError::ExecutionFailed(format!(
            "'{command} --version' exited {:?}",
            output.exit_code
        )));
    }

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
            kind: HostSurfaceKind::NativeSlash,
            installed: false,
            configured: false,
            discoverable: false,
            notes: vec!["could not resolve user home directory".to_string()],
        };
    };

    let skill = home.join(".claude").join("skills").join("hyard.md");
    let manifest = home.join(".claude").join("hyard-native-manifest.yaml");

    let skill_exists = skill.exists();
    let manifest_exists = manifest.exists();

    let mut notes = Vec::new();
    if !skill_exists {
        notes.push(format!("missing HYARD Claude skill: {}", skill.display()));
    }
    if !manifest_exists {
        notes.push(format!(
            "missing HYARD Claude manifest: {}",
            manifest.display()
        ));
    }

    HostSurfaceProbe {
        kind: HostSurfaceKind::NativeSlash,
        installed: skill_exists || manifest_exists,
        configured: skill_exists && manifest_exists,
        discoverable: skill_exists,
        notes,
    }
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}
