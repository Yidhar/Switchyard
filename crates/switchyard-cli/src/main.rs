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
    Delegate {
        #[arg(long)]
        provider: String,
        #[arg(long)]
        task: String,
        /// Time to wait for completion before returning `wait_timeout`.
        #[arg(long, default_value_t = host::DEFAULT_WAIT_SECS)]
        wait_sec: u64,
    },
    /// Check the status of a job by turn ID.
    Status {
        #[arg(long)]
        job_id: String,
    },
    /// Wait again on an existing async job without restarting it.
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
    let registry = build_registry();

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

            let mut store = StoreHandle::open(config.store_backend(), config.store_path(&cwd))
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
                wait_sec,
            } => {
                host::host_delegate_with_wait(&registry, &config, &provider, &task, &cwd, wait_sec)
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
