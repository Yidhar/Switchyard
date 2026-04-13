use std::path::PathBuf;

use switchyard_config::SwitchyardConfig;
use switchyard_core::ProviderRegistry;
use switchyard_provider_api::Provider;
use switchyard_provider_claude::ClaudeProvider;
use switchyard_provider_codex::CodexProvider;
use switchyard_provider_gemini::GeminiProvider;
use switchyard_tui::app::App;

fn build_registry() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    registry.register(
        "codex",
        Box::new(|cfg| {
            let p: Box<dyn Provider> = match cfg {
                Some(c) => Box::new(CodexProvider::from_config(c)),
                None => Box::new(CodexProvider::new("codex", vec![], 900)),
            };
            p
        }),
    );
    registry.register(
        "claude",
        Box::new(|cfg| {
            let p: Box<dyn Provider> = match cfg {
                Some(c) => Box::new(ClaudeProvider::from_config(c)),
                None => Box::new(ClaudeProvider::new("claude", vec![], 900)),
            };
            p
        }),
    );
    registry.register(
        "gemini",
        Box::new(|cfg| {
            let p: Box<dyn Provider> = match cfg {
                Some(c) => Box::new(GeminiProvider::from_config(c)),
                None => Box::new(GeminiProvider::new("gemini", vec![], 900)),
            };
            p
        }),
    );
    registry
}

#[tokio::main]
async fn main() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let registry = build_registry();
    let store_backend = config.store_backend();
    let store_path = config.store_path(&cwd);
    let job_dir = config.job_dir(&cwd);
    let provider = config.core.default_provider.clone();

    let mut app = App::with_store(provider, store_backend, store_path, job_dir);
    if let Err(e) = app.run(&registry, &config).await {
        eprintln!("fatal: {e}");
        std::process::exit(1);
    }
}
