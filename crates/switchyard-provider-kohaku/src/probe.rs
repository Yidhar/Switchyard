//! KohakuTerrarium probe: detect installation, version, and whether this `kt`
//! supports the headless JSONL mode (the `switchyard-headless` fork).

use switchyard_provider_api::{HostSurfaceProbe, ProbeResult, ProviderError};
use switchyard_provider_subprocess::{
    SubprocessConfig, check_auth_error, default_cli_capabilities, run_subprocess,
};

/// Marker the fork prints in `kt --version` (see `cli/version.py`).
const HEADLESS_FORK_HINT: &str = "install the switchyard-headless fork: pip install 'git+https://github.com/Yidhar/KohakuTerrarium@headless'";

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
                "command '{command}' not found (install KohakuTerrarium; {HEADLESS_FORK_HINT})"
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
            "kohaku",
        ) {
            return Err(err);
        }
        return Err(ProviderError::ExecutionFailed(format!(
            "'{command} --version' exited {:?}",
            output.exit_code
        )));
    }

    let version = output.stdout.lines().next().map(|l| l.trim().to_string());

    // The fork advertises headless support in `kt --version`. On stock `kt`
    // the line is absent — keep the provider visible but flag the gap so the
    // user knows why a turn would fail.
    let lower = output.stdout.to_ascii_lowercase();
    let headless_capable = lower.contains("headless:") || output.stdout.contains("--headless");
    let mut issues = Vec::new();
    if !headless_capable {
        issues.push(format!(
            "this `kt` lacks headless mode (`kt run --headless --json`); {HEADLESS_FORK_HINT}"
        ));
    }

    Ok(ProbeResult {
        version,
        available: true,
        capabilities: default_cli_capabilities(),
        issues,
        // KohakuTerrarium is integrated as a leaf provider; it does not expose
        // a HYARD host surface to delegate to other providers.
        host_surface: HostSurfaceProbe::unavailable(vec![
            "KohakuTerrarium runs as a leaf provider; no HYARD host surface".to_string(),
        ]),
    })
}
