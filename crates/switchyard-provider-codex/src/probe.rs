//! Codex probe: detect installation, version, auth, capabilities.

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
            "codex",
        ) {
            return Err(err);
        }
        return Err(ProviderError::ExecutionFailed(format!(
            "'{command} --version' exited {:?}",
            output.exit_code
        )));
    }

    let version = output.stdout.lines().next().map(|line| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("codex-cli ")
            .or_else(|| trimmed.strip_prefix("codex "))
            .unwrap_or(trimmed)
            .to_string()
    });

    let mut issues = Vec::new();

    // Check if non-interactive mode is available
    let help_args = vec!["--help".to_string()];
    let help_config = SubprocessConfig {
        command,
        args: &help_args,
        stdin_data: None,
        timeout_secs: 5,
        cwd: None,
        pty_registry_key: None,
        prefer_pty: false,
    };
    if let Ok(help_output) = run_subprocess(&help_config).await {
        let help = &help_output.stdout;
        let has_exec = help.contains("exec") && help.contains("non-interactively");
        let has_quiet = help.contains("--quiet") || help.contains("-q");
        if !has_exec && !has_quiet {
            issues.push(
                "no non-interactive mode found (exec subcommand or --quiet flag)".to_string(),
            );
        }
    }

    let mut surface = detect_host_surface();
    surface.notes.extend(detect_feature_notes(command).await);
    Ok(ProbeResult {
        version,
        available: true,
        capabilities: default_cli_capabilities(),
        issues,
        host_surface: surface,
    })
}

fn detect_host_surface() -> HostSurfaceProbe {
    let Some(home) = user_home_dir() else {
        return HostSurfaceProbe {
            kind: HostSurfaceKind::Skill,
            installed: false,
            configured: false,
            discoverable: false,
            notes: vec!["could not resolve user home directory".to_string()],
        };
    };

    let agents = home.join(".codex").join("AGENTS.md");
    let legacy_flat_skill = home.join(".codex").join("skills").join("hyard.md");
    let skill = home
        .join(".codex")
        .join("skills")
        .join("hyard")
        .join("SKILL.md");

    let agents_exists = agents.exists();
    let skill_exists = skill.exists() || legacy_flat_skill.exists();

    let mut notes = Vec::new();
    if !agents_exists {
        notes.push(format!("missing Codex AGENTS file: {}", agents.display()));
    }
    if !skill_exists {
        notes.push(format!("missing Codex HYARD skill: {}", skill.display()));
    } else if legacy_flat_skill.exists() && !skill.exists() {
        notes.push(format!(
            "using legacy flat Codex skill path: {}",
            legacy_flat_skill.display()
        ));
    }

    HostSurfaceProbe {
        kind: HostSurfaceKind::Skill,
        installed: agents_exists || skill_exists,
        configured: agents_exists && skill_exists,
        discoverable: skill_exists,
        notes,
    }
}

async fn detect_feature_notes(command: &str) -> Vec<String> {
    let args = vec!["features".to_string(), "list".to_string()];
    let config = SubprocessConfig {
        command,
        args: &args,
        stdin_data: None,
        timeout_secs: 5,
        cwd: None,
        pty_registry_key: None,
        prefer_pty: false,
    };

    let Ok(output) = run_subprocess(&config).await else {
        return vec!["failed to probe codex feature flags".to_string()];
    };

    if output.exit_code != Some(0) {
        return vec!["codex feature probe exited non-zero".to_string()];
    }

    let mut notes = Vec::new();
    let plugins_enabled = feature_enabled(&output.stdout, "plugins");
    let hooks_enabled = feature_enabled(&output.stdout, "codex_hooks");

    match (plugins_enabled, hooks_enabled) {
        (Some(true), Some(true)) => notes.push(
            "codex features indicate plugins and hooks are enabled; native plugin path may be available."
                .to_string(),
        ),
        (Some(true), Some(false)) => notes.push(
            "codex plugins are enabled but codex_hooks is disabled; keep HYARD on skill fallback."
                .to_string(),
        ),
        (Some(false), _) => notes.push(
            "codex plugins feature is disabled in this install; keep HYARD on skill/shell fallback."
                .to_string(),
        ),
        _ => notes.push("could not determine codex plugin/hook feature readiness".to_string()),
    }

    notes
}

fn feature_enabled(stdout: &str, feature_name: &str) -> Option<bool> {
    stdout
        .lines()
        .find(|line| line.trim_start().starts_with(feature_name))
        .and_then(|line| line.split_whitespace().last())
        .and_then(|value| match value {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        })
}

fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}
