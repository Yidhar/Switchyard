//! Peer catalog and descriptor types for orchestration.
//!
//! The core sees peers through these abstractions, never through raw CLI details.

use serde::{Deserialize, Serialize};

use crate::{capability::ProviderCapability, host_surface::HostSurfaceProbe, role::ProviderRole};

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
                    r#"{"type":"delegate","requests":[{"id":"<unique>","provider":"<name>","role":"<role>","task":"<description>","write_access":false,"timeout_sec":900}]}"#.to_string(),
                );
                lines.push("<<<SWITCHYARD_JSON_END>>>".to_string());
                lines.push(
                    "Set timeout_sec to match scope; deep research or multi-step team tasks often need 600-1800 seconds."
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
                    "If the bridge returns status=\"wait_timeout\", that is NOT a failure: the peer job continues running in background."
                        .to_string(),
                );
                lines.push(
                    "The bridge writes one compact JSON object to stdout; read it verbatim and extract at least status, job_id, message, and next_actions."
                        .to_string(),
                );
                lines.push(
                    "After wait_timeout, inspect or continue the same job with /hyard:status <job-id>, /hyard:result <job-id>, /hyard:await <job-id> <timeout-sec>, or /hyard:cancel <job-id>."
                        .to_string(),
                );
                lines.push(
                    "Do not re-delegate the same task from scratch when you already have a job_id."
                        .to_string(),
                );
                lines.push(
                    "If status/result/await says the job is still active, keep using the same job_id until you get a terminal state or enough progress to report."
                        .to_string(),
                );
                lines.push(
                    "Use shorter wait windows (10-30s) for quick tasks and larger ones (30-120s+) for research or multi-step work."
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
    let response = crate::delegate::DelegateResponse::new(results.to_vec());
    let json = serde_json::to_string_pretty(&response).unwrap_or_default();
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
        assert!(prompt.contains("Do not re-delegate the same task"));
        assert!(prompt.contains("status, job_id, message, and next_actions"));
    }

    #[test]
    fn sentinel_prompt_keeps_timeout_guidance() {
        let prompt = sample_catalog().render_prompt(PromptMode::Sentinel);
        assert!(prompt.contains("\"timeout_sec\":900"));
        assert!(prompt.contains("600-1800 seconds"));
        assert!(prompt.contains("Do NOT invoke provider CLIs directly."));
    }
}
