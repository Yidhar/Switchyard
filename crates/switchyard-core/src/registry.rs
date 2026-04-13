//! Provider registry: maps provider names to factory functions.
//!
//! The registry is populated at startup (by the CLI or TUI) with concrete
//! provider constructors. This decouples core from specific provider crates.

use std::collections::HashMap;

use switchyard_config::ProviderConfig;
use switchyard_provider_api::{HostSurfaceProbe, Provider};

/// Factory function that constructs a boxed Provider from optional config.
pub type ProviderFactory = Box<dyn Fn(Option<&ProviderConfig>) -> Box<dyn Provider> + Send + Sync>;

/// Registry of available provider adapters.
pub struct ProviderRegistry {
    factories: HashMap<String, ProviderFactory>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            factories: HashMap::new(),
        }
    }

    /// Register a provider factory under a name (e.g. "codex", "claude").
    pub fn register(&mut self, name: impl Into<String>, factory: ProviderFactory) {
        self.factories.insert(name.into(), factory);
    }

    /// Create a provider instance by name, with optional config.
    pub fn create(&self, name: &str, config: Option<&ProviderConfig>) -> Option<Box<dyn Provider>> {
        self.factories.get(name).map(|f| f(config))
    }

    /// List all registered provider names.
    pub fn names(&self) -> Vec<&str> {
        self.factories.keys().map(|s| s.as_str()).collect()
    }

    /// Check if a provider name is registered.
    pub fn has(&self, name: &str) -> bool {
        self.factories.contains_key(name)
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a PeerCatalog from the registry, excluding the active core provider.
///
/// Uses default role assignments. For probe-based availability, use
/// `build_peer_catalog_probed()` instead.
pub fn build_peer_catalog(
    active_core: &str,
    registry: &ProviderRegistry,
) -> switchyard_provider_api::PeerCatalog {
    use switchyard_provider_api::{PeerCatalog, PeerDescriptor};

    let mut catalog = PeerCatalog::new();
    for name in registry.names() {
        if name == active_core {
            continue;
        }
        let roles = default_roles(name);
        catalog.add(PeerDescriptor {
            provider_id: name.to_string(),
            roles,
            available: true,
            capabilities: vec![],
            description: format!("{name} CLI"),
            host_surface: None,
        });
    }
    catalog
}

/// Build a PeerCatalog with actual probe results for availability/capabilities.
pub async fn build_peer_catalog_probed(
    active_core: &str,
    registry: &ProviderRegistry,
    config_providers: &std::collections::HashMap<String, switchyard_config::ProviderConfig>,
) -> switchyard_provider_api::PeerCatalog {
    use switchyard_provider_api::{PeerCatalog, PeerDescriptor};

    // Probe all peers in parallel via JoinSet
    let mut join_set = tokio::task::JoinSet::new();
    for name in registry.names() {
        if name == active_core {
            continue;
        }
        if let Some(provider) = registry.create(name, config_providers.get(name)) {
            let name = name.to_string();
            join_set.spawn(async move {
                let (available, caps, host_surface) = match provider.probe().await {
                    Ok(r) => (
                        r.available,
                        r.capabilities.into_iter().collect(),
                        r.host_surface,
                    ),
                    Err(_) => (false, vec![], HostSurfaceProbe::default()),
                };
                (name, available, caps, host_surface)
            });
        }
    }

    let mut results: Vec<(
        String,
        bool,
        Vec<switchyard_provider_api::ProviderCapability>,
        HostSurfaceProbe,
    )> = Vec::new();
    while let Some(Ok(result)) = join_set.join_next().await {
        results.push(result);
    }

    let mut catalog = PeerCatalog::new();
    for (name, available, capabilities, host_surface) in results {
        catalog.add(PeerDescriptor {
            provider_id: name.to_string(),
            roles: default_roles(&name),
            available,
            capabilities,
            description: format!("{name} CLI"),
            host_surface: Some(host_surface),
        });
    }
    catalog
}

fn default_roles(name: &str) -> Vec<switchyard_provider_api::ProviderRole> {
    use switchyard_provider_api::ProviderRole;
    match name {
        "claude" => vec![ProviderRole::Reviewer, ProviderRole::Analyst],
        "gemini" => vec![ProviderRole::Analyst, ProviderRole::Worker],
        "codex" => vec![ProviderRole::Worker, ProviderRole::Core],
        _ => vec![ProviderRole::Worker],
    }
}
