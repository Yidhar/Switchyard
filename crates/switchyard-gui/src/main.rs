#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::Emitter;
use tauri::Manager;

use switchyard_config::SwitchyardConfig;
use switchyard_core::{ProviderRegistry, build_peer_catalog_probed, run_routed_turn_observable};
use switchyard_provider_api::{CancellationToken, HostSurfaceProbe, Provider, LiveInstanceRegistry};
use switchyard_provider_claude::ClaudeProvider;
use switchyard_provider_codex::CodexProvider;
use switchyard_provider_gemini::GeminiProvider;
use switchyard_session::{Event, Session, Turn, Artifact, InboxEntry};
use switchyard_store::{
    SessionCatalog, SessionEventRepository, SessionRepository, StoreHandle, TurnRepository,
    ArtifactStore, SessionInboxRepository, EventLog,
};

fn get_cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
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
                            None => Box::new(CodexProvider::new(
                                "codex",
                                vec![],
                                std::collections::HashMap::new(),
                                900,
                            )),
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
                            None => Box::new(ClaudeProvider::new(
                                "claude",
                                vec![],
                                std::collections::HashMap::new(),
                                900,
                            )),
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
                            None => Box::new(GeminiProvider::new(
                                "gemini",
                                vec![],
                                std::collections::HashMap::new(),
                                900,
                            )),
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
                    None => Box::new(CodexProvider::new(
                        "codex",
                        vec![],
                        std::collections::HashMap::new(),
                        900,
                    )),
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
                    None => Box::new(ClaudeProvider::new(
                        "claude",
                        vec![],
                        std::collections::HashMap::new(),
                        900,
                    )),
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
                    None => Box::new(GeminiProvider::new(
                        "gemini",
                        vec![],
                        std::collections::HashMap::new(),
                        900,
                    )),
                };
                p
            }),
        );
    }

    registry
}

#[derive(Debug, serde::Serialize)]
struct ProviderStatus {
    provider_id: String,
    backend: Option<String>,
    command: Option<String>,
    args: Vec<String>,
    timeout_secs: Option<u64>,
    configured: bool,
    registered: bool,
    is_default_core: bool,
    is_default_peer: bool,
    roles: Vec<String>,
    available: bool,
    version: Option<String>,
    capabilities: Vec<String>,
    issues: Vec<String>,
    host_surface: Option<HostSurfaceProbe>,
    error: Option<String>,
    checked_at: String,
}

fn default_role_names(name: &str) -> Vec<String> {
    match name {
        "claude" => vec!["reviewer".to_string(), "analyst".to_string()],
        "gemini" => vec!["analyst".to_string(), "worker".to_string()],
        "codex" => vec!["worker".to_string(), "core".to_string()],
        _ => vec!["worker".to_string()],
    }
}

#[tauri::command]
async fn list_provider_status() -> Result<Vec<ProviderStatus>, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let registry = build_registry(&config);
    let checked_at = chrono::Utc::now().to_rfc3339();

    let mut provider_names = BTreeSet::new();
    provider_names.extend([
        "codex".to_string(),
        "claude".to_string(),
        "gemini".to_string(),
    ]);
    provider_names.extend(config.providers.keys().cloned());
    provider_names.extend(registry.names().into_iter().map(ToOwned::to_owned));

    let mut statuses = Vec::new();

    for name in provider_names {
        let provider_config = config.providers.get(&name);
        let configured = provider_config.is_some();
        let registered = registry.has(&name);

        let backend = provider_config.and_then(|cfg| cfg.backend.clone());
        let command = provider_config.map(|cfg| cfg.command.clone());
        let args = provider_config
            .map(|cfg| cfg.args.clone())
            .unwrap_or_default();
        let timeout_secs = provider_config.map(|cfg| cfg.timeout_secs);
        let mut issues = Vec::new();

        let mut status = ProviderStatus {
            provider_id: name.clone(),
            backend,
            command,
            args,
            timeout_secs,
            configured,
            registered,
            is_default_core: config.core.default_provider == name,
            is_default_peer: config.core.default_peers.iter().any(|peer| peer == &name),
            roles: default_role_names(&name),
            available: false,
            version: None,
            capabilities: Vec::new(),
            issues: Vec::new(),
            host_surface: None,
            error: None,
            checked_at: checked_at.clone(),
        };

        if !configured {
            issues.push(
                "not configured in switchyard.toml; using built-in provider fallback".to_string(),
            );
        }

        if !registered {
            issues.push(
                "provider backend is not registered; check providers.<name>.backend".to_string(),
            );
            status.issues = issues;
            status.error = Some("unsupported or unknown backend".to_string());
            statuses.push(status);
            continue;
        }

        let Some(provider) = registry.create(&name, provider_config) else {
            issues.push("provider factory returned no instance".to_string());
            status.issues = issues;
            status.error = Some("provider factory unavailable".to_string());
            statuses.push(status);
            continue;
        };

        match provider.probe().await {
            Ok(probe) => {
                status.available = probe.available;
                status.version = probe.version;
                status.capabilities = probe
                    .capabilities
                    .into_iter()
                    .map(|capability| capability.to_string())
                    .collect();
                status.capabilities.sort();
                issues.extend(probe.issues);
                status.host_surface = Some(probe.host_surface);
            }
            Err(err) => {
                issues.push("probe failed".to_string());
                status.error = Some(err.to_string());
            }
        }

        status.issues = issues;
        statuses.push(status);
    }

    Ok(statuses)
}

#[tauri::command]
async fn load_config() -> Result<SwitchyardConfig, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();

    let config_path = cwd.join("switchyard.toml");
    if !config_path.is_file() {
        let mut final_config = config.clone();

        // Set default core and peers
        if final_config.core.default_provider.is_empty() {
            final_config.core.default_provider = "codex".to_string();
        }
        if final_config.core.default_peers.is_empty() {
            final_config.core.default_peers = vec!["claude".to_string(), "gemini".to_string()];
        }

        // Populate three basic CLIs
        for name in &["codex", "claude", "gemini"] {
            if !final_config.providers.contains_key(*name) {
                let command = match *name {
                    "codex" => "codex-cli",
                    "claude" => "claude-cli",
                    "gemini" => "gemini-cli",
                    _ => *name,
                };

                final_config.providers.insert(
                    name.to_string(),
                    switchyard_config::ProviderConfig {
                        command: command.to_string(),
                        args: vec!["run".to_string()],
                        env: std::collections::HashMap::new(),
                        timeout_secs: 900,
                        backend: Some(name.to_string()),
                    },
                );
            }
        }

        if let Err(e) = final_config.write_to(&config_path) {
            println!(
                "Warning: failed to automatically write default switchyard.toml: {}",
                e
            );
        } else {
            println!("Automatically created default switchyard.toml configuration file.");
        }
        return Ok(final_config);
    }

    Ok(config)
}

#[tauri::command]
async fn save_config(config: SwitchyardConfig) -> Result<(), String> {
    let cwd = get_cwd();
    let config_path = cwd.join("switchyard.toml");
    config
        .write_to(&config_path)
        .map_err(|e| format!("failed to save config: {}", e))?;
    Ok(())
}

#[tauri::command]
async fn list_sessions() -> Result<Vec<Session>, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session_ids = store.list_sessions().map_err(|e| e.to_string())?;
    let mut sessions = Vec::new();
    for id in session_ids {
        if let Ok(Some(s)) = store.load_session(id) {
            sessions.push(s);
        }
    }
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(sessions)
}

#[tauri::command]
async fn create_session(provider: String) -> Result<Session, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let mut store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session = Session::new(provider);
    store
        .save_session(&session)
        .map_err(|e| format!("failed to save session: {}", e))?;
    Ok(session)
}

#[tauri::command]
async fn get_session_turns(session_id: String) -> Result<Vec<Turn>, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;

    let turns = store.list_turns(session_uuid).map_err(|e| e.to_string())?;
    Ok(turns)
}

#[tauri::command]
async fn get_session_events(session_id: String) -> Result<Vec<Event>, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;

    let events = store
        .list_session_events(session_uuid)
        .map_err(|e| e.to_string())?;
    Ok(events)
}

#[tauri::command]
async fn run_turn(
    app: tauri::AppHandle,
    pool: tauri::State<'_, Arc<switchyard_core::InstancePool>>,
    session_id: String,
    message: String,
    provider: Option<String>,
) -> Result<String, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let registry = build_registry(&config);

    let mut store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;

    let mut session = store
        .load_session(session_uuid)
        .map_err(|e| format!("load session: {}", e))?
        .ok_or_else(|| format!("session {} not found", session_id))?;

    let provider = provider.unwrap_or_else(|| session.active_core.clone());

    let provider_impl = registry
        .create(&provider, config.providers.get(&provider))
        .ok_or_else(|| format!("unsupported provider: {}", provider))?;

    // Pre-spawn/ensure core provider is persistent
    if let Some(persistent) = provider_impl.as_persistent() {
        if !pool.has_live_instance(&provider) {
            let env = config
                .providers
                .get(&provider)
                .map(|c| c.env.clone())
                .unwrap_or_default();
            if let Ok(inst) = persistent.start_persistent_instance(env).await {
                pool.register_instance(&provider, inst);
            }
        }
    }

    let registry_dyn: Arc<dyn switchyard_provider_api::LiveInstanceRegistry> = pool.inner().clone();
    let core_proxy = switchyard_core::PersistentProviderProxy::new(
        provider.clone(),
        provider_impl,
        Some(registry_dyn.clone()),
    );

    let peer_catalog = build_peer_catalog_probed(&provider, &registry, &config.providers).await;
    let artifact_dir = config.artifact_dir(&cwd);

    let (tx, mut rx) = tokio::sync::mpsc::channel(100);
    let app_clone = app.clone();

    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            let _ = app_clone.emit("runtime_event", event);
        }
    });

    let cancel = CancellationToken::new();
    {
        let state = app.state::<ActiveTurnState>();
        let mut guard = state.cancel.lock().unwrap();
        *guard = Some(cancel.clone());
    }

    let output = run_routed_turn_observable(
        &mut store,
        &mut session,
        &core_proxy,
        &peer_catalog,
        &|name| registry.create(name, config.providers.get(name)),
        Some(registry_dyn.as_ref()),
        message,
        cwd,
        Some(&artifact_dir),
        Some(&tx),
        cancel.clone(),
    )
    .await;

    {
        let state = app.state::<ActiveTurnState>();
        let mut guard = state.cancel.lock().unwrap();
        *guard = None;
    }

    match output {
        Ok(out) => Ok(out.response.unwrap_or_default()),
        Err(e) => Err(format!("turn failed: {}", e)),
    }
}

struct ActiveTurnState {
    cancel: std::sync::Mutex<Option<CancellationToken>>,
}

#[tauri::command]
fn cancel_turn(state: tauri::State<'_, ActiveTurnState>) -> Result<(), String> {
    let mut guard = state.cancel.lock().unwrap();
    if let Some(cancel) = guard.take() {
        cancel.cancel();
        Ok(())
    } else {
        Err("No active turn running".to_string())
    }
}

#[tauri::command]
async fn update_session_peers(session_id: String, enabled_peers: Vec<String>) -> Result<(), String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let mut store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;

    if let Some(mut session) = store.load_session(session_uuid).map_err(|e| e.to_string())? {
        session.enabled_peers = enabled_peers;
        store.save_session(&session).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct ArtifactItem {
    name: String,
    path: String,
    size: u64,
    is_dir: bool,
    modified: Option<String>,
}

#[tauri::command]
async fn list_artifacts() -> Result<Vec<ArtifactItem>, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let artifact_dir = config.artifact_dir(&cwd);

    if !artifact_dir.exists() {
        return Ok(Vec::new());
    }

    let mut items = Vec::new();
    let mut dir = tokio::fs::read_dir(&artifact_dir)
        .await
        .map_err(|e| format!("failed to read artifact dir: {}", e))?;

    while let Some(entry) = dir.next_entry().await.map_err(|e| e.to_string())? {
        let metadata = entry.metadata().await.map_err(|e| e.to_string())?;
        let modified = metadata
            .modified()
            .ok()
            .and_then(|t| {
                let datetime: chrono::DateTime<chrono::Local> = t.into();
                Some(datetime.to_rfc3339())
            });

        items.push(ArtifactItem {
            name: entry.file_name().to_string_lossy().to_string(),
            path: entry.path().to_string_lossy().to_string(),
            size: metadata.len(),
            is_dir: metadata.is_dir(),
            modified,
        });
    }

    // Sort by modified time descending (newest first)
    items.sort_by(|a, b| b.modified.cmp(&a.modified));

    Ok(items)
}

#[tauri::command]
async fn read_artifact(name: String) -> Result<String, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let artifact_dir = config.artifact_dir(&cwd);
    
    // Simple path traversal check
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err("invalid artifact name".to_string());
    }

    let file_path = artifact_dir.join(name);
    if !file_path.is_file() {
        return Err("artifact file not found".to_string());
    }

    tokio::fs::read_to_string(file_path)
        .await
        .map_err(|e| format!("failed to read artifact file: {}", e))
}

#[tauri::command]
async fn list_active_instances(
    pool: tauri::State<'_, Arc<switchyard_core::InstancePool>>,
) -> Result<Vec<String>, String> {
    Ok(pool.get_active_instances())
}

#[tauri::command]
async fn start_instance(
    pool: tauri::State<'_, Arc<switchyard_core::InstancePool>>,
    provider: String,
) -> Result<(), String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let registry = build_registry(&config);

    let provider_impl = registry
        .create(&provider, config.providers.get(&provider))
        .ok_or_else(|| format!("unsupported provider: {}", provider))?;

    if let Some(persistent) = provider_impl.as_persistent() {
        if pool.has_live_instance(&provider) {
            return Ok(()); // Already running
        }
        let env = config
            .providers
            .get(&provider)
            .map(|c| c.env.clone())
            .unwrap_or_default();
        let inst = persistent
            .start_persistent_instance(env)
            .await
            .map_err(|e| format!("failed to start persistent instance: {}", e))?;
        pool.register_instance(&provider, inst);
        Ok(())
    } else {
        Err(format!("provider {} does not support persistence", provider))
    }
}

#[tauri::command]
async fn stop_instance(
    pool: tauri::State<'_, Arc<switchyard_core::InstancePool>>,
    provider: String,
) -> Result<(), String> {
    if let Some(inst_lock) = pool.remove_instance(&provider) {
        let mut inst = inst_lock.lock().await;
        inst.terminate()
            .await
            .map_err(|e| format!("failed to terminate instance: {}", e))?;
        Ok(())
    } else {
        Err(format!("no active persistent instance for provider {}", provider))
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SessionTrace {
    version: u32,
    session: Session,
    turns: Vec<Turn>,
    events: Vec<Event>,
    artifacts: Vec<Artifact>,
    inbox: Vec<InboxEntry>,
}

#[tauri::command]
async fn delete_session(session_id: String) -> Result<(), String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let mut store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;

    store.delete_session(session_uuid).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn rename_session(session_id: String, name: String) -> Result<(), String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let mut store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;

    if let Some(mut session) = store.load_session(session_uuid).map_err(|e| e.to_string())? {
        session.name = Some(name);
        store.save_session(&session).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn update_session_summary(session_id: String, summary: Option<String>) -> Result<(), String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let mut store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;

    if let Some(mut session) = store.load_session(session_uuid).map_err(|e| e.to_string())? {
        session.summary = summary;
        store.save_session(&session).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn update_session_checklist(session_id: String, checklist_json: String) -> Result<(), String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let mut store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;

    if let Some(mut session) = store.load_session(session_uuid).map_err(|e| e.to_string())? {
        session.native_bindings.insert("checklist".to_string(), checklist_json);
        store.save_session(&session).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn export_session_trace(session_id: String) -> Result<String, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session_uuid =
        uuid::Uuid::parse_str(&session_id).map_err(|e| format!("invalid session ID: {}", e))?;

    let session = store
        .load_session(session_uuid)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("session {} not found", session_id))?;

    let turns = store.list_turns(session_uuid).map_err(|e| e.to_string())?;
    let events = store.list_session_events(session_uuid).map_err(|e| e.to_string())?;

    let mut artifacts = Vec::new();
    for turn in &turns {
        if let Ok(arts) = store.list_artifacts(turn.turn_id) {
            artifacts.extend(arts);
        }
    }

    let inbox = store.list_inbox_entries(session_uuid).map_err(|e| e.to_string())?;

    let trace = SessionTrace {
        version: 1,
        session,
        turns,
        events,
        artifacts,
        inbox,
    };

    serde_json::to_string_pretty(&trace).map_err(|e| format!("failed to serialize trace: {}", e))
}

#[tauri::command]
async fn import_session_trace(trace_json: String) -> Result<Session, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let mut store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let trace: SessionTrace = serde_json::from_str(&trace_json)
        .map_err(|e| format!("failed to parse trace JSON: {}", e))?;

    let new_session_id = uuid::Uuid::now_v7();
    let mut turn_id_map = std::collections::HashMap::new();

    let mut session = trace.session;
    session.session_id = new_session_id;
    session.name = Some(format!(
        "{} (Imported)",
        session.name.as_deref().unwrap_or("Session")
    ));

    store
        .save_session(&session)
        .map_err(|e| format!("failed to save session: {}", e))?;

    for mut turn in trace.turns {
        let old_turn_id = turn.turn_id;
        let new_turn_id = uuid::Uuid::now_v7();
        turn_id_map.insert(old_turn_id, new_turn_id);

        turn.session_id = new_session_id;
        turn.turn_id = new_turn_id;
        store
            .append_turn(&turn)
            .map_err(|e| format!("failed to append turn: {}", e))?;
    }

    for mut event in trace.events {
        if let Some(&new_tid) = turn_id_map.get(&event.turn_id) {
            event.turn_id = new_tid;
        }
        store
            .append_event(&event)
            .map_err(|e| format!("failed to append event: {}", e))?;
    }

    for mut artifact in trace.artifacts {
        if let Some(&new_tid) = turn_id_map.get(&artifact.turn_id) {
            artifact.turn_id = new_tid;
        }
        store
            .save_artifact(&artifact)
            .map_err(|e| format!("failed to save artifact: {}", e))?;
    }

    for mut entry in trace.inbox {
        entry.session_id = new_session_id;
        store
            .save_inbox_entry(&entry)
            .map_err(|e| format!("failed to save inbox entry: {}", e))?;
    }

    Ok(session)
}

fn main() {
    tauri::Builder::default()
        .manage(ActiveTurnState {
            cancel: std::sync::Mutex::new(None),
        })
        .manage(Arc::new(switchyard_core::InstancePool::new()))
        .invoke_handler(tauri::generate_handler![
            load_config,
            save_config,
            list_provider_status,
            list_sessions,
            create_session,
            get_session_turns,
            get_session_events,
            run_turn,
            cancel_turn,
            update_session_peers,
            list_artifacts,
            read_artifact,
            list_active_instances,
            start_instance,
            stop_instance,
            delete_session,
            rename_session,
            update_session_summary,
            update_session_checklist,
            export_session_trace,
            import_session_trace
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
