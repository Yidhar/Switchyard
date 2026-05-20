use std::{path::PathBuf, process};

use clap::Parser;
use switchyard_config::SwitchyardConfig;
use switchyard_core::ProviderRegistry;
use switchyard_provider_api::Provider;
use switchyard_provider_claude::ClaudeProvider;
use switchyard_provider_codex::CodexProvider;
use switchyard_provider_gemini::GeminiProvider;
use switchyard_store::{SessionRepository, StoreHandle};
use switchyard_tui::app::App;
use switchyard_tui::launch::{resolve_resume_session, resolve_work_dir};

#[derive(Parser)]
#[command(name = "switchyard-tui", about = "Interactive TUI for Switchyard")]
struct Cli {
    /// Provider to preselect (default: from config's core.default_provider or resumed session).
    #[arg(long)]
    provider: Option<String>,

    /// Resume one existing session by full id or unique UUID prefix.
    #[arg(long, conflicts_with = "resume_latest")]
    session: Option<String>,

    /// Resume the most recently updated session.
    #[arg(long, conflicts_with = "session")]
    resume_latest: bool,

    /// Working directory used for config/store resolution and routed turns.
    #[arg(long, default_value = ".")]
    cwd: PathBuf,
}

fn build_registry(config: &SwitchyardConfig) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();

    // Register all configured providers dynamically!
    for (name, prov_cfg) in &config.providers {
        let backend = prov_cfg.backend.as_deref().unwrap_or_else(|| {
            if name.contains("codex") {
                "codex"
            } else if name.contains("claude") {
                "claude"
            } else if name.contains("gemini") {
                "gemini"
            } else {
                ""
            }
        });
        match backend {
            "codex" => {
                registry.register(
                    name.clone(),
                    Box::new(|cfg| {
                        let p: Box<dyn Provider> = match cfg {
                            Some(c) => Box::new(CodexProvider::from_config(c)),
                            None => Box::new(CodexProvider::new("codex", vec![], std::collections::HashMap::new(), 900)),
                        };
                        p
                    }),
                );
            }
            "claude" => {
                registry.register(
                    name.clone(),
                    Box::new(|cfg| {
                        let p: Box<dyn Provider> = match cfg {
                            Some(c) => Box::new(ClaudeProvider::from_config(c)),
                            None => Box::new(ClaudeProvider::new("claude", vec![], std::collections::HashMap::new(), 900)),
                        };
                        p
                    }),
                );
            }
            "gemini" => {
                registry.register(
                    name.clone(),
                    Box::new(|cfg| {
                        let p: Box<dyn Provider> = match cfg {
                            Some(c) => Box::new(GeminiProvider::from_config(c)),
                            None => Box::new(GeminiProvider::new("gemini", vec![], std::collections::HashMap::new(), 900)),
                        };
                        p
                    }),
                );
            }
            _ => {}
        }
    }

    // Always ensure the default three are registered even if not in config
    if !registry.has("codex") {
        registry.register(
            "codex",
            Box::new(|cfg| {
                let p: Box<dyn Provider> = match cfg {
                    Some(c) => Box::new(CodexProvider::from_config(c)),
                    None => Box::new(CodexProvider::new("codex", vec![], std::collections::HashMap::new(), 900)),
                };
                p
            }),
        );
    }
    if !registry.has("claude") {
        registry.register(
            "claude",
            Box::new(|cfg| {
                let p: Box<dyn Provider> = match cfg {
                    Some(c) => Box::new(ClaudeProvider::from_config(c)),
                    None => Box::new(ClaudeProvider::new("claude", vec![], std::collections::HashMap::new(), 900)),
                };
                p
            }),
        );
    }
    if !registry.has("gemini") {
        registry.register(
            "gemini",
            Box::new(|cfg| {
                let p: Box<dyn Provider> = match cfg {
                    Some(c) => Box::new(GeminiProvider::from_config(c)),
                    None => Box::new(GeminiProvider::new("gemini", vec![], std::collections::HashMap::new(), 900)),
                };
                p
            }),
        );
    }

    registry
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let tui_cwd = resolve_work_dir(&cwd, &cli.cwd);
    let config = SwitchyardConfig::resolve(&tui_cwd).unwrap_or_default();
    let registry = build_registry(&config);
    let store_backend = config.store_backend(&tui_cwd);
    let store_path = config.store_path(&tui_cwd);
    let job_dir = config.job_dir(&tui_cwd);
    let mut resume_session = None;
    let provider_override = cli.provider.clone();
    let mut provider_name = cli
        .provider
        .unwrap_or_else(|| config.core.default_provider.clone());

    if cli.session.is_some() || cli.resume_latest {
        let store = StoreHandle::open(store_backend, store_path.clone()).unwrap_or_else(|err| {
            eprintln!("failed to open store: {err}");
            process::exit(1);
        });
        let selected = resolve_resume_session(&store, cli.session.as_deref(), cli.resume_latest)
            .unwrap_or_else(|err| {
                eprintln!("{err}");
                process::exit(1);
            });
        resume_session = selected;

        if provider_override.is_none()
            && let Some(session_id) = selected
            && let Ok(Some(existing)) = store.load_session(session_id)
        {
            provider_name = existing.active_core;
        }
    }

    if let Err(err) = std::env::set_current_dir(&tui_cwd) {
        eprintln!(
            "failed to switch working directory to '{}': {err}",
            tui_cwd.display()
        );
        process::exit(1);
    }

    let mut app = App::with_store(provider_name, store_backend, store_path, job_dir);
    if let Some(session_id) = resume_session {
        app.set_resume_session(session_id);
    }
    if let Err(e) = app.run(&registry, &config).await {
        eprintln!("fatal: {e}");
        process::exit(1);
    }
}
