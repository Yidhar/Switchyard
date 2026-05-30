//! Peer catalog and descriptor types for orchestration.
//!
//! The core sees peers through these abstractions, never through raw CLI details.

use serde::{Deserialize, Serialize};

use crate::{capability::ProviderCapability, host_surface::HostSurfaceProbe, role::ProviderRole};

const MAX_INJECTED_CHANGED_FILES: usize = 8;

/// Prompt injection mode: determines how delegation instructions are rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMode {
    /// Switchyard internal (CLI/TUI): sentinel JSON blocks in text output.
    Sentinel,
    /// Host-native integration: /hyard:* slash commands.
    Hyard,
}

/// Describes a peer provider's capabilities as visible to the active core.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerDescriptor {
    /// Stable provider identifier (e.g. "claude", "gemini").
    pub provider_id: String,
    /// Recommended roles this peer can fulfill.
    pub roles: Vec<ProviderRole>,
    /// Whether probe() succeeded.
    pub available: bool,
    /// Capabilities detected during probe.
    pub capabilities: Vec<ProviderCapability>,
    /// Short human-readable description for prompt injection.
    pub description: String,
    /// Host surface readiness metadata, if available.
    pub host_surface: Option<HostSurfaceProbe>,
}

/// Catalog of available peers, injected into the core's context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerCatalog {
    pub peers: Vec<PeerDescriptor>,
}

impl PeerCatalog {
    pub fn new() -> Self {
        Self { peers: Vec::new() }
    }

    pub fn add(&mut self, peer: PeerDescriptor) {
        self.peers.push(peer);
    }

    /// Find a peer by provider id.
    pub fn find(&self, provider_id: &str) -> Option<&PeerDescriptor> {
        self.peers.iter().find(|p| p.provider_id == provider_id)
    }

    /// Check if a provider is available for delegation.
    pub fn is_available(&self, provider_id: &str) -> bool {
        self.find(provider_id).is_some_and(|p| p.available)
    }

    /// Render the catalog for sentinel mode (Switchyard internal CLI/TUI).
    pub fn render_prompt_block(&self) -> String {
        self.render_prompt(PromptMode::Sentinel)
    }

    /// Render the catalog for a specific prompt mode.
    pub fn render_prompt(&self, mode: PromptMode) -> String {
        if self.peers.is_empty() {
            return "No peer providers available for delegation.".to_string();
        }

        let mut lines = vec!["Available peer providers for delegation:".to_string()];
        for peer in &self.peers {
            let status = if peer.available {
                "available"
            } else {
                "unavailable"
            };
            let roles: Vec<_> = peer.roles.iter().map(|r| r.to_string()).collect();
            lines.push(format!(
                "- {}: {} (roles: {}) [{}]",
                peer.provider_id,
                peer.description,
                roles.join(", "),
                status,
            ));
            if let Some(surface) = &peer.host_surface {
                lines.push(format!(
                    "  Host surface: {} (installed: {}, configured: {}, discoverable: {})",
                    surface.kind, surface.installed, surface.configured, surface.discoverable
                ));
                for note in &surface.notes {
                    lines.push(format!("    note: {}", note));
                }
            }
        }
        lines.push(String::new());

        match mode {
            PromptMode::Sentinel => {
                lines.push("To delegate a task, emit a Switchyard delegate block:".to_string());
                lines.push("<<<SWITCHYARD_JSON_BEGIN>>>".to_string());
                lines.push(
                    r#"{"type":"delegate","requests":[{"id":"<unique>","provider":"<name>","role":"<role>","task":"<description>","write_access":false,"timeout_sec":0}]}"#.to_string(),
                );
                lines.push("<<<SWITCHYARD_JSON_END>>>".to_string());
                lines.push(
                    "Use timeout_sec=0 for no hard wall-clock timeout; set a positive timeout only when the caller intentionally wants a deadline."
                        .to_string(),
                );
            }
            PromptMode::Hyard => {
                lines.push("To delegate a task, use the async /hyard bridge:".to_string());
                lines.push(
                    r#"/hyard:delegate <provider> "<task description>" [--wait-sec <n>]"#
                        .to_string(),
                );
                lines.push(
                    "Treat HYARD as a background tool: launch peer work, then keep doing local work."
                        .to_string(),
                );
                lines.push(
                    "If status=\"wait_timeout\", that is NOT a failure; the same job_id is still running in background."
                        .to_string(),
                );
                lines.push(
                    "Use the compact bridge JSON verbatim and extract at least status, job_id, message, and next_actions."
                        .to_string(),
                );
                lines.push(
                    "Prefer the default short wait or --wait-sec 1-5 for true background launches; use longer waits only when foreground waiting is intentional."
                        .to_string(),
                );
                lines.push(
                    "After wait_timeout, reuse the same job_id with /hyard:status, /hyard:result, /hyard:await, or /hyard:cancel."
                        .to_string(),
                );
                lines.push(
                    "Do NOT call /hyard:await immediately after /hyard:delegate unless the very next step is blocked on that result."
                        .to_string(),
                );
                lines.push(
                    "At a natural checkpoint, you may inspect the same job_id; do not re-delegate the same task from scratch."
                        .to_string(),
                );
                lines.push(
                    "You may run multiple independent HYARD jobs in parallel when their tasks do not overlap."
                        .to_string(),
                );
            }
        }

        lines.push("Do NOT invoke provider CLIs directly.".to_string());
        lines.join("\n")
    }
}

impl Default for PeerCatalog {
    fn default() -> Self {
        Self::new()
    }
}

/// Render a DelegateResponse as a text block for injection into the core's next turn.
pub fn render_delegate_result_block(results: &[crate::delegate::DelegateTaskResult]) -> String {
    let compact_results = results
        .iter()
        .map(|result| {
            let changed_files: Vec<String> = result
                .changed_files
                .iter()
                .take(MAX_INJECTED_CHANGED_FILES)
                .map(|path| path.display().to_string())
                .collect();
            serde_json::json!({
                "id": result.id,
                "provider": result.provider,
                "status": result.status,
                "summary": result.summary,
                "changed_files": changed_files,
                "changed_file_count": result.changed_files.len(),
                "artifact_count": result.artifacts.len(),
                "artifacts_omitted": !result.artifacts.is_empty(),
                "error": result.error,
                "exit_code": result.exit_code,
                "duration_ms": result.duration_ms,
            })
        })
        .collect::<Vec<_>>();

    let response = serde_json::json!({
        "type": "delegate_result",
        "compact": true,
        "results": compact_results,
    });
    let json = serde_json::to_string(&response).unwrap_or_default();
    format!(
        "<<<SWITCHYARD_JSON_BEGIN>>>\n{}\n<<<SWITCHYARD_JSON_END>>>",
        json
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_catalog() -> PeerCatalog {
        let mut catalog = PeerCatalog::new();
        catalog.add(PeerDescriptor {
            provider_id: "claude".to_string(),
            roles: vec![ProviderRole::Reviewer],
            available: true,
            capabilities: vec![ProviderCapability::HeadlessTurn],
            description: "Claude CLI".to_string(),
            host_surface: Some(HostSurfaceProbe::ready(crate::HostSurfaceKind::NativeSlash)),
        });
        catalog
    }

    #[test]
    fn hyard_prompt_mentions_wait_timeout_and_await() {
        let prompt = sample_catalog().render_prompt(PromptMode::Hyard);
        assert!(prompt.contains("/hyard:delegate"));
        assert!(prompt.contains("wait_timeout"));
        assert!(prompt.contains("/hyard:await"));
        assert!(prompt.contains("background tool"));
        assert!(prompt.contains("keep doing local work"));
        assert!(prompt.contains("multiple independent HYARD jobs in parallel"));
        assert!(prompt.contains("do not re-delegate the same task"));
        assert!(prompt.contains("status, job_id, message, and next_actions"));
    }

    #[test]
    fn sentinel_prompt_keeps_timeout_guidance() {
        let prompt = sample_catalog().render_prompt(PromptMode::Sentinel);
        assert!(prompt.contains("\"timeout_sec\":0"));
        assert!(prompt.contains("no hard wall-clock timeout"));
        assert!(prompt.contains("intentionally wants a deadline"));
        assert!(prompt.contains("Do NOT invoke provider CLIs directly."));
    }

    #[test]
    fn delegate_result_block_is_compact_and_omits_full_artifacts() {
        let rendered = render_delegate_result_block(&[crate::delegate::DelegateTaskResult {
            id: "t1".to_string(),
            provider: "claude".to_string(),
            status: crate::delegate::DelegateStatus::Success,
            summary: Some("done".to_string()),
            changed_files: (0..10)
                .map(|idx| std::path::PathBuf::from(format!("src/file_{idx}.rs")))
                .collect(),
            artifacts: vec![std::collections::HashMap::from([(
                "kind".to_string(),
                serde_json::json!("full_payload_should_not_be_injected"),
            )])],
            error: None,
            exit_code: Some(0),
            duration_ms: Some(123),
        }]);

        assert!(rendered.contains("\"type\":\"delegate_result\""));
        assert!(rendered.contains("\"compact\":true"));
        assert!(rendered.contains("\"artifact_count\":1"));
        assert!(rendered.contains("\"artifacts_omitted\":true"));
        assert!(rendered.contains("\"changed_file_count\":10"));
        assert!(!rendered.contains("full_payload_should_not_be_injected"));
        assert!(!rendered.contains("\n  \"results\""));
    }
}
