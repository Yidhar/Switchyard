#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use std::path::PathBuf;
use tauri::Emitter;

use switchyard_config::SwitchyardConfig;
use switchyard_core::{
    build_peer_catalog_probed, run_routed_turn_observable, ProviderRegistry,
};
use switchyard_provider_api::{CancellationToken, Provider};
use switchyard_provider_claude::ClaudeProvider;
use switchyard_provider_codex::CodexProvider;
use switchyard_provider_gemini::GeminiProvider;
use switchyard_session::{Session, Turn, Event};
use switchyard_store::{SessionCatalog, SessionRepository, StoreHandle, TurnRepository, SessionEventRepository};

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

#[tauri::command]
async fn load_config() -> Result<SwitchyardConfig, String> {
    let cwd = get_cwd();
    Ok(SwitchyardConfig::resolve(&cwd).unwrap_or_default())
}

#[tauri::command]
async fn save_config(config: SwitchyardConfig) -> Result<(), String> {
    let cwd = get_cwd();
    let config_path = cwd.join("switchyard.toml");
    config.write_to(&config_path).map_err(|e| format!("failed to save config: {}", e))?;
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
    store.save_session(&session).map_err(|e| format!("failed to save session: {}", e))?;
    Ok(session)
}

#[tauri::command]
async fn get_session_turns(session_id: String) -> Result<Vec<Turn>, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session_uuid = uuid::Uuid::parse_str(&session_id)
        .map_err(|e| format!("invalid session ID: {}", e))?;

    let turns = store.list_turns(session_uuid).map_err(|e| e.to_string())?;
    Ok(turns)
}

#[tauri::command]
async fn get_session_events(session_id: String) -> Result<Vec<Event>, String> {
    let cwd = get_cwd();
    let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
    let store = StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd))
        .map_err(|e| format!("failed to open store: {}", e))?;

    let session_uuid = uuid::Uuid::parse_str(&session_id)
        .map_err(|e| format!("invalid session ID: {}", e))?;

    let events = store.list_session_events(session_uuid).map_err(|e| e.to_string())?;
    Ok(events)
}

#[tauri::command]
async fn run_turn(
    app: tauri::AppHandle,
    session_id: String,
    message: String,
    provider: Option<String>,
) -> Result<String, String> {
    let (tx_result, rx_result) = tokio::sync::oneshot::channel();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let cwd = get_cwd();
            let config = SwitchyardConfig::resolve(&cwd).unwrap_or_default();
            let registry = build_registry(&config);

            let mut store = match StoreHandle::open(config.store_backend(&cwd), config.store_path(&cwd)) {
                Ok(s) => s,
                Err(e) => {
                    let _ = tx_result.send(Err(format!("failed to open store: {}", e)));
                    return;
                }
            };

            let session_uuid = match uuid::Uuid::parse_str(&session_id) {
                Ok(u) => u,
                Err(e) => {
                    let _ = tx_result.send(Err(format!("invalid session ID: {}", e)));
                    return;
                }
            };

            let mut session = match store.load_session(session_uuid) {
                Ok(Some(s)) => s,
                Ok(None) => {
                    let _ = tx_result.send(Err(format!("session {} not found", session_id)));
                    return;
                }
                Err(e) => {
                    let _ = tx_result.send(Err(format!("load session: {}", e)));
                    return;
                }
            };

            let provider = provider.unwrap_or_else(|| session.active_core.clone());

            let provider_impl = match registry.create(&provider, config.providers.get(&provider)) {
                Some(p) => p,
                None => {
                    let _ = tx_result.send(Err(format!("unsupported provider: {}", provider)));
                    return;
                }
            };

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

            let output = run_routed_turn_observable(
                &mut store,
                &mut session,
                provider_impl.as_ref(),
                &peer_catalog,
                &|name| registry.create(name, config.providers.get(name)),
                None,
                message,
                cwd,
                Some(&artifact_dir),
                Some(&tx),
                cancel,
            )
            .await;

            match output {
                Ok(out) => {
                    let _ = tx_result.send(Ok(out.response.unwrap_or_default()));
                }
                Err(e) => {
                    let _ = tx_result.send(Err(format!("turn failed: {}", e)));
                }
            }
        });
    });

    rx_result.await.map_err(|e| format!("thread join failed: {}", e))?
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            load_config,
            save_config,
            list_sessions,
            create_session,
            get_session_turns,
            get_session_events,
            run_turn
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
