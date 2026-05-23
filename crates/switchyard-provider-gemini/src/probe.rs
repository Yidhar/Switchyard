//! Gemini probe: detect installation, version, capabilities.
//!
//! Gemini uses `-v` not `--version`, and on Windows the `.cmd` wrapper
//! may fail so we fall back to the shared probe_version logic.
//!
//! ## Deprecation
//!
//! Google announced 2026-06-18 as the EOL for the consumer / Code Assist
//! Standard tiers of Gemini CLI (Enterprise Code Assist continues). The
//! Switchyard team has frozen the Gemini adapter at its current feature set;
//! new providers (Antigravity) inherit Google's investment going forward.
//!
//! We surface this in `ProbeResult.issues` so the GUI status surface flags
//! it to the user without breaking existing config — anyone running
//! Switchyard against Gemini today still gets working turns, just with a
//! visible "please migrate" annotation. Removal of the adapter itself is
//! deferred until well past the EOL date so projects with frozen toolchains
//! don't break overnight.

use std::path::PathBuf;

use switchyard_provider_api::{HostSurfaceKind, HostSurfaceProbe, ProbeResult, ProviderError};
use switchyard_provider_subprocess::{SubprocessConfig, default_cli_capabilities, run_subprocess};

/// Hard-coded EOL date for the Gemini CLI consumer tiers. Used by the probe
/// to construct a deprecation note that mentions both the date and the
/// suggested replacement.
pub const GEMINI_EOL: &str = "2026-06-18";

/// One-line deprecation note appended to `ProbeResult.issues`. Centralised
/// here so the GUI / CLI status surfaces stay in sync.
pub fn deprecation_note() -> String {
    format!(
        "DEPRECATED: Google has set {GEMINI_EOL} as the EOL for the Gemini CLI \
         consumer / Code Assist Standard tiers. Migrate to the Antigravity \
         provider (backend `antigravity`, command `agy`); Enterprise Code \
         Assist users can keep Gemini indefinitely."
    )
}

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
        env: None,
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
                    issues: vec![deprecation_note()],
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
        issues: vec![deprecation_note()],
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deprecation_note_mentions_eol_date_and_replacement() {
        let note = deprecation_note();
        // Lock the date so a future careless edit doesn't ship a vague
        // "soon" message.
        assert!(
            note.contains(GEMINI_EOL),
            "note must cite {GEMINI_EOL}: {note}"
        );
        assert!(
            note.contains("Antigravity"),
            "note must point to the replacement"
        );
        assert!(
            note.to_lowercase().contains("enterprise"),
            "note must call out the enterprise carve-out",
        );
    }

    #[test]
    fn eol_constant_matches_google_announcement() {
        // 2026-06-18 from the Google Developers blog. Lock here so it
        // doesn't drift when someone else copies probe.rs.
        assert_eq!(GEMINI_EOL, "2026-06-18");
    }
}
