//! Shared provider registry construction for Switchyard app entrypoints.
//!
//! CLI, TUI, and GUI all need the same mapping from configured provider names
//! to concrete provider adapters. Keeping that wiring in one crate prevents a
//! fallback default from being fixed in one entrypoint and regressing in another.

use switchyard_config::{ProviderConfig, SwitchyardConfig};
use switchyard_core::ProviderRegistry;
use switchyard_provider_antigravity::AntigravityProvider;
use switchyard_provider_api::Provider;
use switchyard_provider_claude::ClaudeProvider;
use switchyard_provider_codex::CodexProvider;
use switchyard_provider_gemini::GeminiProvider;
use switchyard_provider_kohaku::KohakuProvider;

/// Built-in provider aliases exposed even when the local config omits them.
pub const BUILT_IN_PROVIDER_ALIASES: &[&str] =
    &["codex", "claude", "gemini", "antigravity", "kohaku"];

/// Default timeout for built-in provider fallbacks.
///
/// `0` means no hard wall-clock timeout. Long-running work is supervised via
/// cancellation, health checks, and heartbeat/no-output watchdog paths rather
/// than a fixed kill switch.
pub const FALLBACK_PROVIDER_TIMEOUT_SECS: u64 = 0;

/// Build the provider registry with all known adapters.
pub fn build_provider_registry(config: &SwitchyardConfig) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();

    for (name, prov_cfg) in &config.providers {
        let backend = prov_cfg
            .backend
            .as_deref()
            .or_else(|| inferred_provider_backend(name));
        if let Some(backend) = backend {
            register_provider_backend(&mut registry, name.clone(), backend);
        }
    }

    for alias in BUILT_IN_PROVIDER_ALIASES {
        if !registry.has(alias) {
            register_provider_backend(&mut registry, *alias, alias);
        }
    }

    registry
}

/// Infer the concrete adapter backend from a provider id/name.
pub fn inferred_provider_backend(name: &str) -> Option<&'static str> {
    let name = name.to_ascii_lowercase();
    if name.contains("codex") {
        Some("codex")
    } else if name.contains("claude") {
        Some("claude")
    } else if name.contains("antigravity") || name.contains("agy") {
        // Match Antigravity before Gemini: `agy` stores data under the Gemini
        // config tree, but it has a different CLI/protocol surface.
        Some("antigravity")
    } else if name.contains("gemini") {
        Some("gemini")
    } else if name.contains("kohaku") {
        Some("kohaku")
    } else {
        None
    }
}

/// Default command for a known backend.
pub fn default_provider_command(backend: &str) -> String {
    match backend {
        "codex" => "codex".to_string(),
        "claude" => "claude".to_string(),
        "gemini" => "gemini".to_string(),
        "antigravity" => "agy".to_string(),
        "kohaku" => "kt".to_string(),
        other => other.to_string(),
    }
}

/// Built-in fallback config for a provider id.
pub fn default_provider_config(provider_id: &str) -> ProviderConfig {
    let backend = inferred_provider_backend(provider_id).unwrap_or(provider_id);
    default_provider_config_for_backend(backend)
}

fn default_provider_config_for_backend(backend: &str) -> ProviderConfig {
    // KohakuTerrarium needs a creature as args[0]; default to the official
    // `general` creature from the kt-biome pack (`kt install @kt-biome`) so
    // kohaku works out of the box instead of erroring on an empty creature.
    let args = if backend == "kohaku" {
        vec!["@kt-biome/creatures/general".to_string()]
    } else {
        Vec::new()
    };
    ProviderConfig {
        command: default_provider_command(backend),
        args,
        env: std::collections::HashMap::new(),
        model: None,
        thinking_level: None,
        timeout_secs: FALLBACK_PROVIDER_TIMEOUT_SECS,
        backend: Some(backend.to_string()),
    }
}

fn register_provider_backend(
    registry: &mut ProviderRegistry,
    name: impl Into<String>,
    backend: &str,
) {
    match backend {
        "codex" => registry.register(
            name,
            Box::new(|cfg| {
                let p: Box<dyn Provider> = match cfg {
                    Some(c) => Box::new(CodexProvider::from_config(c)),
                    None => {
                        let c = default_provider_config_for_backend("codex");
                        Box::new(CodexProvider::from_config(&c))
                    }
                };
                p
            }),
        ),
        "claude" => registry.register(
            name,
            Box::new(|cfg| {
                let p: Box<dyn Provider> = match cfg {
                    Some(c) => Box::new(ClaudeProvider::from_config(c)),
                    None => {
                        let c = default_provider_config_for_backend("claude");
                        Box::new(ClaudeProvider::from_config(&c))
                    }
                };
                p
            }),
        ),
        "gemini" => registry.register(
            name,
            Box::new(|cfg| {
                let p: Box<dyn Provider> = match cfg {
                    Some(c) => Box::new(GeminiProvider::from_config(c)),
                    None => {
                        let c = default_provider_config_for_backend("gemini");
                        Box::new(GeminiProvider::from_config(&c))
                    }
                };
                p
            }),
        ),
        "antigravity" => registry.register(
            name,
            Box::new(|cfg| {
                let p: Box<dyn Provider> = match cfg {
                    Some(c) => Box::new(AntigravityProvider::from_config(c)),
                    None => {
                        let c = default_provider_config_for_backend("antigravity");
                        Box::new(AntigravityProvider::from_config(&c))
                    }
                };
                p
            }),
        ),
        "kohaku" => registry.register(
            name,
            Box::new(|cfg| {
                let p: Box<dyn Provider> = match cfg {
                    Some(c) => Box::new(KohakuProvider::from_config(c)),
                    None => {
                        let c = default_provider_config_for_backend("kohaku");
                        Box::new(KohakuProvider::from_config(&c))
                    }
                };
                p
            }),
        ),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchyard_provider_antigravity::AntigravityProvider;
    use switchyard_provider_claude::ClaudeProvider;
    use switchyard_provider_codex::CodexProvider;
    use switchyard_provider_gemini::GeminiProvider;
    use switchyard_provider_kohaku::KohakuProvider;

    #[test]
    fn built_in_fallback_configs_have_no_hard_timeout() {
        for alias in BUILT_IN_PROVIDER_ALIASES {
            let cfg = default_provider_config(alias);
            assert_eq!(cfg.timeout_secs, 0, "fallback timeout for {alias}");
        }
    }

    #[test]
    fn concrete_fallback_providers_receive_no_hard_timeout() {
        let codex = CodexProvider::from_config(&default_provider_config("codex"));
        let claude = ClaudeProvider::from_config(&default_provider_config("claude"));
        let gemini = GeminiProvider::from_config(&default_provider_config("gemini"));
        let antigravity = AntigravityProvider::from_config(&default_provider_config("antigravity"));
        let kohaku = KohakuProvider::from_config(&default_provider_config("kohaku"));

        assert_eq!(codex.timeout_secs, 0);
        assert_eq!(claude.timeout_secs, 0);
        assert_eq!(gemini.timeout_secs, 0);
        assert_eq!(antigravity.timeout_secs, 0);
        assert_eq!(kohaku.timeout_secs, 0);
    }

    #[test]
    fn registry_always_contains_built_in_aliases() {
        let registry = build_provider_registry(&SwitchyardConfig::default());

        for alias in BUILT_IN_PROVIDER_ALIASES {
            assert!(registry.has(alias), "missing built-in alias {alias}");
        }
    }

    #[test]
    fn backend_inference_handles_antigravity_before_gemini() {
        assert_eq!(inferred_provider_backend("codex-fast"), Some("codex"));
        assert_eq!(inferred_provider_backend("CLAUDE"), Some("claude"));
        assert_eq!(inferred_provider_backend("agy"), Some("antigravity"));
        assert_eq!(inferred_provider_backend("google-gemini"), Some("gemini"));
        assert_eq!(inferred_provider_backend("kohaku"), Some("kohaku"));
        assert_eq!(inferred_provider_backend("unknown"), None);
    }
}
