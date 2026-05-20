mod host;
mod store_cmd;

use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};
use serde::Serialize;

use switchyard_config::SwitchyardConfig;
use switchyard_core::{ProviderRegistry, build_peer_catalog_probed, run_routed_turn_with_archive};
use switchyard_provider_api::{HostSurfaceProbe, Provider};
use switchyard_provider_claude::ClaudeProvider;
use switchyard_provider_codex::CodexProvider;
use switchyard_provider_gemini::GeminiProvider;
use switchyard_provider_subprocess::{find_on_path, probe_version};
use switchyard_session::Session;
use switchyard_store::{SessionRepository, StoreHandle};
use switchyard_tui::{
    app::App,
    launch::{resolve_resume_session, resolve_work_dir},
};

use crate::store_cmd::StoreAction;

#[derive(Parser)]
#[command(name = "switchyard", about = "CLI router for AI coding providers")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a single turn against a provider.
    Run {
        /// Provider to use (default: from config's core.default_provider).
        #[arg(long)]
        provider: Option<String>,

        /// Message to send to the provider.
        #[arg(long, short)]
        message: String,

        /// Working directory.
        #[arg(long, default_value = ".")]
        cwd: PathBuf,
    },
    /// Launch the interactive TUI.
    Tui {
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
    },
    /// Validate configuration and check provider availability.
    Check {
        /// Output machine-readable JSON report after probes complete.
        #[arg(long)]
        json: bool,
    },
    /// Machine-readable bridge for host packs (/hyard:* protocol).
    Host {
        #[command(subcommand)]
        action: HostAction,
    },
    /// Inspect and migrate persistent store backends.
    Store {
        #[command(subcommand)]
        action: StoreAction,
    },
}

#[derive(Subcommand)]
#[command(disable_help_subcommand = true)]
enum HostAction {
    /// List available providers with probe status.
    List,
    /// Delegate a task to a peer provider (leaf execution, no further delegation).
    /// This is a background tool; it may return wait_timeout while the same job continues.
    /// Continue other work, then follow up with status/result/await using the same job_id.
    Delegate {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        task: String,
        /// Route the eventual callback receipt into this session id or unique UUID prefix.
        #[arg(long)]
        session: Option<String>,
        /// Seconds to wait for completion before returning `wait_timeout`.
        #[arg(long, default_value_t = host::DEFAULT_WAIT_SECS)]
        wait_sec: u64,
    },
    /// Check the status of a job by turn ID.
    Status {
        #[arg(long)]
        job_id: String,
    },
    /// Wait again on an existing async job without restarting it.
    /// This may also return wait_timeout while the same job keeps running.
    Await {
        #[arg(long)]
        job_id: String,
        #[arg(long, default_value_t = 30)]
        timeout_sec: u64,
    },
    /// Get the full result of a completed job.
    Result {
        #[arg(long)]
        job_id: String,
    },
    /// Cancel a running delegate job (V1: best-effort).
    Cancel {
        #[arg(long)]
        job_id: String,
    },
    /// Read callback receipts from a session inbox (defaults to latest session).
    Inbox {
        /// Session id or unique UUID prefix. Defaults to the latest session when omitted.
        #[arg(long, conflicts_with = "resume_latest")]
        session: Option<String>,
        /// Explicitly target the most recently updated session.
        #[arg(long, conflicts_with = "session")]
        resume_latest: bool,
        /// Include read/consumed items instead of unread receipts only.
        #[arg(long)]
        all: bool,
        /// Mark returned unread items as read after listing them.
        #[arg(long, conflicts_with = "consume")]
        mark_read: bool,
        /// Mark returned items as consumed after listing them.
        #[arg(long, conflicts_with = "mark_read")]
        consume: bool,
    },
    /// Wait for callback receipts from the current/latest session and return when one arrives.
    Watch {
        /// Session id or unique UUID prefix. Defaults to the latest session when omitted.
        #[arg(long, conflicts_with = "resume_latest")]
        session: Option<String>,
        /// Explicitly target the most recently updated session.
        #[arg(long, conflicts_with = "session")]
        resume_latest: bool,
        /// Seconds to wait for a new/pending callback receipt before returning wait_timeout.
        #[arg(long, default_value_t = 180)]
        timeout_sec: u64,
        /// Mark returned unread items as read after they are delivered.
        #[arg(long, conflicts_with = "consume")]
        mark_read: bool,
        /// Mark returned items as consumed after they are delivered.
        #[arg(long, conflicts_with = "mark_read")]
        consume: bool,
    },
    /// Resume an existing session non-interactively, optionally driven by unread callback receipts.
    Resume {
        /// Session id or unique UUID prefix. Defaults to the latest session when omitted.
        #[arg(long, conflicts_with = "resume_latest")]
        session: Option<String>,
        /// Explicitly target the most recently updated session.
        #[arg(long, conflicts_with = "session")]
        resume_latest: bool,
        /// Resume with an explicit user message.
        #[arg(
            long,
            conflicts_with = "callbacks",
            required_unless_present = "callbacks"
        )]
        message: Option<String>,
        /// Resume only when unread non-quiet callback receipts are pending; otherwise return noop.
        #[arg(long, conflicts_with = "message", required_unless_present = "message")]
        callbacks: bool,
    },
    /// Wait for unread non-quiet callback receipts and automatically resume the same session.
    Follow {
        /// Session id or unique UUID prefix. Defaults to the latest session when omitted.
        #[arg(long, conflicts_with = "resume_latest")]
        session: Option<String>,
        /// Explicitly target the most recently updated session.
        #[arg(long, conflicts_with = "session")]
        resume_latest: bool,
        /// Seconds to wait for unread resumable callback receipts before returning wait_timeout.
        #[arg(long, default_value_t = 180)]
        timeout_sec: u64,
        /// Stay resident and keep re-arming watch->resume cycles until the process is stopped.
        #[arg(long)]
        forever: bool,
    },
    /// Internal async job worker. Not part of the public HYARD surface.
    #[command(hide = true)]
    Worker {
        #[arg(long)]
        job_id: String,
    },
    /// Print available /hyard commands and their usage.
    Help,
}

/// Build the provider registry with all known adapters.
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

/// Known CLI command names for auto-discovery.
const KNOWN_CLIS: &[&str] = &["codex", "claude", "gemini"];

#[derive(Clone, Serialize)]
struct CheckEntry {
    provider: String,
    adapter: bool,
    status: String,
    version: Option<String>,
    host_surface: HostSurfaceProbe,
    issues: Vec<String>,
    error: Option<String>,
}

#[derive(Clone, Serialize)]
struct CheckReport {
    configuration_ok: bool,
    configuration_issues: Vec<String>,
    general_issues: Vec<String>,
    providers: Vec<CheckEntry>,
    total: usize,
    ready_count: usize,
    failed: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let registry = build_registry(&config);

    match cli.command {
        Commands::Run {
            provider,
            message,
            cwd: work_dir,
        } => {
            let provider = provider.unwrap_or_else(|| config.core.default_provider.clone());

            let issues = config.validate();
            for issue in &issues {
                eprintln!("config warning: {issue}");
            }

            let mut store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
                .unwrap_or_else(|e| {
                    eprintln!("failed to open store: {e}");
                    process::exit(1);
                });
            let mut session = Session::new(provider.clone());
            store.save_session(&session).unwrap_or_else(|e| {
                eprintln!("failed to create session: {e}");
                process::exit(1);
            });

            let provider_impl = match registry.create(&provider, config.providers.get(&provider)) {
                Some(p) => p,
                None => {
                    eprintln!("unsupported provider: {provider}");
                    eprintln!("available: {}", registry.names().join(", "));
                    process::exit(1);
                }
            };

            let peer_catalog =
                build_peer_catalog_probed(&provider, &registry, &config.providers).await;
            let artifact_dir = config.artifact_dir(&cwd);

            match run_routed_turn_with_archive(
                &mut store,
                &mut session,
                provider_impl.as_ref(),
                &peer_catalog,
                &|name| registry.create(name, config.providers.get(name)),
                None,
                message,
                work_dir,
                Some(&artifact_dir),
            )
            .await
            {
                Ok(output) => {
                    if let Some(response) = &output.response {
                        println!("{response}");
                    }
                }
                Err(e) => {
                    eprintln!("turn failed: {e}");
                    process::exit(1);
                }
            }
        }
        Commands::Tui {
            provider,
            session,
            resume_latest,
            cwd: work_dir,
        } => {
            let tui_cwd = resolve_work_dir(&cwd, &work_dir);
            let config = SwitchyardConfig::resolve(&tui_cwd).unwrap_or_default();
            let store_backend = config.store_backend(&tui_cwd);
            let store_path = config.store_path(&tui_cwd);
            let job_dir = config.job_dir(&tui_cwd);
            let mut resume_session = None;
            let provider_override = provider.clone();
            let mut provider_name =
                provider.unwrap_or_else(|| config.core.default_provider.clone());

            if session.is_some() || resume_latest {
                let store =
                    StoreHandle::open(store_backend, store_path.clone()).unwrap_or_else(|e| {
                        eprintln!("failed to open store: {e}");
                        process::exit(1);
                    });
                let selected = resolve_resume_session(&store, session.as_deref(), resume_latest)
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
            if let Err(err) = app.run(&registry, &config).await {
                eprintln!("fatal: {err}");
                process::exit(1);
            }
        }
        Commands::Check { json } => {
            let config_issues = config.validate();

            // Merge: explicit config entries + auto-discovered CLIs
            let mut to_probe: Vec<(String, Option<&switchyard_config::ProviderConfig>)> = config
                .providers
                .iter()
                .map(|(k, v)| (k.clone(), Some(v)))
                .collect();
            for name in KNOWN_CLIS {
                if !to_probe.iter().any(|(n, _)| n == name) && find_on_path(name).is_some() {
                    to_probe.push((name.to_string(), None));
                }
            }

            use std::io::Write;
            let mut any_failed = !config_issues.is_empty();
            let mut ready_count = 0;
            let mut entries: Vec<CheckEntry> = Vec::new();
            let mut general_issues: Vec<String> = Vec::new();

            if to_probe.is_empty() {
                any_failed = true;
                general_issues.push("no provider CLIs found on PATH".to_string());
            }

            if !json {
                if config_issues.is_empty() {
                    println!("configuration OK");
                } else {
                    println!("configuration has {} issue(s):", config_issues.len());
                    for issue in &config_issues {
                        println!("  - {issue}");
                    }
                }
            }

            for (name, cfg) in &to_probe {
                if !json {
                    print!("probing {name}... ");
                    std::io::stdout().flush().ok();
                }

                if let Some(provider) = registry.create(name, *cfg) {
                    match provider.probe().await {
                        Ok(result) => {
                            let surface = result.host_surface.clone();
                            let status = if result.issues.is_empty()
                                && result.available
                                && surface.is_ready()
                            {
                                ready_count += 1;
                                "ready"
                            } else {
                                "installed (with warnings)"
                            };
                            if !json {
                                println!(
                                    "{status} (version: {})",
                                    result.version.as_deref().unwrap_or("unknown")
                                );
                                println!(
                                    "  host surface: {} (installed: {}, configured: {}, discoverable: {})",
                                    surface.kind,
                                    surface.installed,
                                    surface.configured,
                                    surface.discoverable
                                );
                                for note in &surface.notes {
                                    println!("    note: {note}");
                                }
                                for issue in &result.issues {
                                    println!("  warning: {issue}");
                                }
                            }
                            entries.push(CheckEntry {
                                provider: name.clone(),
                                adapter: true,
                                status: status.to_string(),
                                version: result.version.clone(),
                                host_surface: surface,
                                issues: result.issues.clone(),
                                error: None,
                            });
                        }
                        Err(e) => {
                            if !json {
                                println!("FAILED: {e}");
                            }
                            entries.push(CheckEntry {
                                provider: name.clone(),
                                adapter: true,
                                status: "failed".to_string(),
                                version: None,
                                host_surface: HostSurfaceProbe::default(),
                                issues: vec![],
                                error: Some(e.to_string()),
                            });
                            any_failed = true;
                        }
                    }
                } else {
                    let cmd = cfg.map(|c| c.command.as_str()).unwrap_or(name);
                    let probe = probe_version(cmd).await;
                    let host_surface =
                        HostSurfaceProbe::unavailable(vec!["adapter not implemented".to_string()]);
                    match probe.version {
                        Some(version) => {
                            if !json {
                                println!(
                                    "installed (version: {version}) [adapter not implemented]"
                                );
                            }
                            entries.push(CheckEntry {
                                provider: name.clone(),
                                adapter: false,
                                status: "adapter_missing".to_string(),
                                version: Some(version),
                                host_surface,
                                issues: vec![],
                                error: None,
                            });
                        }
                        None if probe.resolved_command.is_some() => {
                            if !json {
                                println!("found on PATH [adapter not implemented]");
                            }
                            entries.push(CheckEntry {
                                provider: name.clone(),
                                adapter: false,
                                status: "adapter_missing".to_string(),
                                version: None,
                                host_surface,
                                issues: vec![],
                                error: None,
                            });
                        }
                        None => {
                            if !json {
                                println!("FAILED: not found");
                            }
                            entries.push(CheckEntry {
                                provider: name.clone(),
                                adapter: false,
                                status: "not_found".to_string(),
                                version: None,
                                host_surface: HostSurfaceProbe::unavailable(vec![
                                    "command not found".to_string(),
                                ]),
                                issues: vec![],
                                error: Some("not found".to_string()),
                            });
                            any_failed = true;
                        }
                    }
                }
            }

            let total = entries.len();
            let report = CheckReport {
                configuration_ok: config_issues.is_empty(),
                configuration_issues: config_issues,
                general_issues,
                providers: entries,
                total,
                ready_count,
                failed: any_failed,
            };

            if json {
                match serde_json::to_string_pretty(&report) {
                    Ok(payload) => println!("{payload}"),
                    Err(err) => {
                        eprintln!("failed to serialize check report: {err}");
                        process::exit(1);
                    }
                }
            } else {
                println!("\n{total} provider(s) checked.");
                if !report.general_issues.is_empty() {
                    for issue in &report.general_issues {
                        println!("  - {issue}");
                    }
                }
            }

            if any_failed {
                process::exit(1);
            }
        }
        Commands::Host { action } => match action {
            HostAction::List => {
                host::host_list(&registry, &config).await;
            }
            HostAction::Delegate {
                provider,
                task,
                session,
                wait_sec,
            } => {
                host::host_delegate_with_wait(
                    &registry,
                    &config,
                    &provider,
                    &task,
                    &cwd,
                    wait_sec,
                    session.as_deref(),
                )
                .await;
            }
            HostAction::Status { job_id } => {
                host::host_status(&config, &job_id, &cwd).await;
            }
            HostAction::Await {
                job_id,
                timeout_sec,
            } => {
                host::host_await(&config, &job_id, &cwd, timeout_sec).await;
            }
            HostAction::Result { job_id } => {
                host::host_result(&config, &job_id, &cwd).await;
            }
            HostAction::Cancel { job_id } => {
                host::host_cancel(&config, &job_id, &cwd).await;
            }
            HostAction::Inbox {
                session,
                resume_latest,
                all,
                mark_read,
                consume,
            } => {
                host::host_inbox(
                    &config,
                    session.as_deref(),
                    resume_latest,
                    all,
                    mark_read,
                    consume,
                    &cwd,
                )
                .await;
            }
            HostAction::Watch {
                session,
                resume_latest,
                timeout_sec,
                mark_read,
                consume,
            } => {
                host::host_watch(
                    &config,
                    session.as_deref(),
                    resume_latest,
                    timeout_sec,
                    mark_read,
                    consume,
                    &cwd,
                )
                .await;
            }
            HostAction::Resume {
                session,
                resume_latest,
                message,
                callbacks,
            } => {
                host::host_resume(
                    &registry,
                    &config,
                    session.as_deref(),
                    resume_latest,
                    message.as_deref(),
                    callbacks,
                    &cwd,
                )
                .await;
            }
            HostAction::Follow {
                session,
                resume_latest,
                timeout_sec,
                forever,
            } => {
                host::host_follow(
                    &registry,
                    &config,
                    session.as_deref(),
                    resume_latest,
                    timeout_sec,
                    forever,
                    &cwd,
                )
                .await;
            }
            HostAction::Worker { job_id } => {
                host::host_worker(&registry, &config, &job_id, &cwd).await;
            }
            HostAction::Help => {
                host::host_help();
            }
        },
        Commands::Store { action } => {
            if let Err(err) = store_cmd::run(action, &config, &cwd) {
                eprintln!("{err}");
                process::exit(1);
            }
        }
    }
}
